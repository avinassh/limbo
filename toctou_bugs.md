# MVCC CREATE INDEX TOCTOU Bugs

This note documents two related but distinct TOCTOU bugs around MVCC exclusive
transaction acquisition and `CREATE INDEX` backfill.

Both bugs can produce the same external symptom:

1. A DDL transaction begins before a concurrent writer commits.
2. The writer inserts a row and commits.
3. `CREATE INDEX` incorrectly acquires the exclusive slot from the old
   transaction snapshot.
4. The index backfill sees the old snapshot and misses the newly committed row.
5. A later indexed operation, for example `DELETE FROM t WHERE id = 2`, can fail
   with corruption such as an `IdxDelete` error because the table row exists but
   the secondary index entry does not.

The correct behavior is for the stale DDL transaction to fail exclusive
acquisition with `Busy`. Retrying `CREATE INDEX` in a fresh transaction should
see the committed row and build a complete index.

## Shared Invariant

`acquire_exclusive_tx` uses `last_committed_tx_ts` as the stale-snapshot guard:

```rust
tx.begin_ts < last_committed_tx_ts => Busy
```

That guard is the authority for "a write committed after this transaction
began." Correctness depends on two properties:

1. A committed writer must publish `last_committed_tx_ts` before another writer
   can safely pass the commit-lock boundary and try to acquire exclusive.
2. Exclusive acquisition must re-check the committed timestamp after it wins the
   exclusive CAS, because the timestamp can change between the pre-check and
   `compare_exchange`.

The two bugs below violate those properties in different windows.

## Bug 1: Commit Finalization Publishes The Watermark Too Late

### Window

In `CommitState::FinalizeCommit`, the writer had already:

- written and synced the logical commit record,
- marked the transaction `Committed(end_ts)`,
- rewritten live row versions from `TxID` references to `Timestamp(end_ts)`,
- notified commit dependents.

But the old order released the commit lock before publishing
`last_committed_tx_ts` and `global_header`.

That created a gap:

```text
writer state is Committed(end_ts)
row versions are rewritten
commit lock is released
last_committed_tx_ts is still old
```

During that gap, an old DDL transaction could try `CREATE INDEX`.

### Timeline

1. Connection 1 creates table `t` and commits row `(1, 100)`.
2. Connection 2 begins a concurrent writer transaction and inserts `(2, 200)`.
3. Connection 3 begins `BEGIN DEFERRED` and performs a read, establishing
   `ddl_begin_ts`.
4. Connection 2 commits and reaches the FinalizeCommit gap after becoming
   `Committed(writer_end_ts)` but before publishing `last_committed_tx_ts`.
5. Connection 3 runs `CREATE INDEX idx_v ON t(v)`.
6. `acquire_exclusive_tx` sees:
   - no other transaction in `Preparing`,
   - `last_committed_tx_ts < writer_end_ts`,
   - `ddl_begin_ts < writer_end_ts`, but the stale watermark hides that.
7. Connection 3 acquires exclusive and backfills the index from its old snapshot,
   so row `(2, 200)` is not indexed.
8. Connection 2 resumes and publishes the watermark.
9. A later `DELETE FROM t WHERE id = 2` sees the table row but cannot delete the
   missing index entry.

### Root Cause

The commit path made the writer complete enough for other transactions to make
progress, but had not yet published the timestamp used by
`acquire_exclusive_tx` to reject old snapshots.

This is a publication-order bug. The timestamp guard itself is fine, but it was
not updated before releasing the serialization point.

### Fix Direction

Publish `last_committed_tx_ts` and `global_header` before releasing the commit
lock in `FinalizeCommit`.

The yield point named `BeforeGlobalHeaderUpdate` should keep its enum position
for stable yield ordinals, but after this fix its historical name is misleading:
it should fire after watermark/header publication and after releasing the commit
lock, before final transaction cleanup.

### Test Shape

The clean reproducer should use commit yield injection:

1. Start writer and DDL transactions.
2. DDL transaction reads first, pinning `ddl_begin_ts`.
3. Writer commits with a fixed yield at `CommitYieldPoint::BeforeGlobalHeaderUpdate`.
4. Assert writer is `Committed(writer_end_ts)`.
5. Assert the fixed ordering has published
   `last_committed_tx_ts >= writer_end_ts`.
6. Run stale `CREATE INDEX`; it should return `Busy`.
7. Resume writer.
8. Fresh `CREATE INDEX` and `DELETE FROM t WHERE id = 2` should succeed, and
   `PRAGMA integrity_check` should be `ok`.

## Bug 2: Exclusive Acquisition Does Not Recheck Timestamp After CAS

### Window

`acquire_exclusive_tx` checks the stale-snapshot guard before acquiring the
exclusive slot:

```text
check has_preparing_tx_other_than
check tx.begin_ts < last_committed_tx_ts
compare_exchange(NO_EXCLUSIVE_TX, tx_id)
post-CAS check has_preparing_tx_other_than only
```

The missing post-CAS timestamp recheck creates a second TOCTOU window.

### Timeline

1. Connection 1 starts a concurrent writer and inserts `(2, 200)`.
2. Connection 2 starts a DDL transaction, reads, and pins an old `begin_ts`.
3. Connection 2 enters `acquire_exclusive_tx`.
4. The pre-CAS timestamp check passes because the writer has not committed yet.
5. Connection 1 commits and publishes `last_committed_tx_ts`.
6. Connection 2 wins the exclusive CAS.
7. The post-CAS path only rechecks `has_preparing_tx_other_than`.
8. Connection 2 keeps the exclusive slot even though its snapshot is now stale.
9. `CREATE INDEX` backfills from the old snapshot and misses row `(2, 200)`.

### Root Cause

The stale-snapshot decision is made before acquiring the exclusive slot, but it
is not validated again after the slot is acquired.

The existing post-CAS preparing recheck protects against a transaction entering
or remaining in `Preparing` during the CAS window. It does not protect against a
transaction that fully commits and advances `last_committed_tx_ts` during that
same window.

### Fix Direction

After `compare_exchange` succeeds, re-run the same stale-snapshot predicate:

```rust
tx.begin_ts < last_committed_tx_ts
```

If it is true, release `exclusive_tx` and return `Busy`.

This should use `last_committed_tx_ts`, not a scan for committed transactions in
`txs`. The watermark is the intended authority. Scanning `txs` adds a second
source of truth and risks false positives around read-only commits, cleanup
timing, and committed transactions that are still resident only because final
cleanup has not removed them yet.

### Existing Branch Reproducer

The local branch `bug3-reproducer` contains:

```text
test_bug3_create_index_exclusive_acquisition_race
```

That test is not a yield-injection test. It uses an ad hoc `#[cfg(test)]`
sleep/yield inside `acquire_exclusive_tx` between the timestamp pre-check and
the CAS, then commits the writer on another thread.

The test currently passes by reproducing corruption. That means a passing result
there is not evidence of a fix unless the test is changed to expect `Busy` and
post-fix integrity.

### Better Test Shape

Convert the ad hoc sleep to a real deterministic yield point in exclusive
acquisition:

```text
after committed-timestamp pre-check
before exclusive_tx compare_exchange
```

Then the test can be single-threaded and deterministic:

1. DDL transaction enters exclusive acquisition and yields after the pre-check.
2. Writer commits and publishes `last_committed_tx_ts`.
3. Resume DDL acquisition.
4. Post-CAS timestamp recheck should release exclusive and return `Busy`.
5. A fresh `CREATE INDEX` should build a complete index.

## Relationship Between The Bugs

The two bugs are independent.

Publishing the watermark before releasing the commit lock fixes Bug 1, but it
does not fix Bug 2. A writer can still commit after the pre-CAS timestamp check
and before the exclusive CAS.

Rechecking the timestamp after CAS fixes Bug 2, but it does not fix Bug 1 if
the committing writer releases the commit lock before publishing the watermark.
In that case both the pre-CAS and post-CAS checks can still see the old
watermark.

The complete fix needs both:

1. Commit finalization publishes `last_committed_tx_ts` before releasing the
   commit lock.
2. Exclusive acquisition rechecks `tx.begin_ts < last_committed_tx_ts` after a
   successful CAS and releases the exclusive slot on failure.

