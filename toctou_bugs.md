# MVCC Exclusive Acquisition TOCTOU Bugs

This document tracks two related MVCC bugs around `CREATE INDEX` / exclusive
transaction acquisition. Both are time-of-check/time-of-use races where an
exclusive transaction decides its snapshot is still valid, but a concurrent
writer commits in a window not covered by that check.

## 1. Writer commits after conflict checks but before `Preparing(ts)`

### What is the bug?

An exclusive transaction can acquire `exclusive_tx` from an old snapshot while a
normal writer is already in the commit path, after it has passed commit-conflict
checks, but before it has published `TransactionState::Preparing(ts)`.

For `CREATE INDEX`, this can let the DDL transaction build an index from a
snapshot that does not include a concurrently committed row. If the writer later
commits, the table contains the row but the newly built index can miss it.

### Why does it happen?

`acquire_exclusive_tx` checks:

- whether another transaction is `Preparing`
- whether `last_committed_tx_ts` is newer than the exclusive transaction's
  `begin_ts`
- then it acquires `exclusive_tx`
- then it repeats the same checks

The problem is that a writer in `CommitState::Initial` can already have passed
the exclusive/schema conflict checks, but still be `Active` because it has not
yet stored `Preparing(ts)`.

Timeline:

```text
W = normal writer
E = exclusive/DDL tx

t0: W enters CommitState::Initial
t1: W checks has_exclusive_tx() == false
t2: W has not yet stored Preparing(ts)

t3: E runs acquire_exclusive_tx
t4: E checks has_preparing_tx_other_than == false
t5: E checks last_committed_tx_ts; still old
t6: E wins exclusive_tx CAS
t7: E repeats the checks; W is still Active, so they still pass

t8: W resumes, stores Preparing(ts)
t9: W commits and advances last_committed_tx_ts while E holds exclusive_tx
```

At `t9`, `E` holds `exclusive_tx`, but its snapshot predates `W`'s committed
write. That violates the isolation rule that exclusive DDL should not proceed
from a stale snapshot.

### How to reproduce it

Use yield injection to stop `W` after commit-conflict checks and before
`Preparing(ts)`:

- Add / use `CommitYieldPoint::AfterCommitConflictChecksBeforePreparing`.
- Create table `t(id INTEGER PRIMARY KEY, v INTEGER)`.
- `W`: `BEGIN CONCURRENT; INSERT INTO t VALUES (2, 200);`
- `E`: `BEGIN DEFERRED; SELECT COUNT(*) FROM t;` to pin an old snapshot.
- Install a yield injector on `W` for
  `AfterCommitConflictChecksBeforePreparing`.
- Step `W`'s `COMMIT` until the yield point.
- While `W` is yielded, run `E`: `CREATE INDEX idx_v ON t(v)`.

Buggy behavior:

```text
CREATE INDEX succeeds while W is between conflict checks and Preparing(ts).
```

Expected behavior:

```text
CREATE INDEX returns Busy and must not acquire exclusive_tx.
```

Regression test:

```text
test_create_index_exclusive_acquire_waits_for_commit_before_preparing
```

## 2. Writer is `Committed(ts)` before `last_committed_tx_ts` advances

### What is the bug?

An exclusive transaction can acquire `exclusive_tx` from an old snapshot while a
normal writer has already moved from `Preparing(ts)` to `Committed(ts)`, but has
not yet advanced `last_committed_tx_ts`.

This is a later commit-stage variant of the same stale-snapshot problem. The
writer is no longer `Preparing`, so a check that only looks for preparing
transactions does not block. The committed timestamp is also not yet visible via
`last_committed_tx_ts`, so the watermark check can also pass.

### Why does it happen?

The commit state machine marks the writer as committed before it updates the
global committed timestamp watermark:

```text
CommitEnd:
  W stores TransactionState::Committed(end_ts)

FinalizeCommit:
  W later updates last_committed_tx_ts
  W later removes itself from txs
```

That leaves a window where:

- `W` is not `Preparing`
- `W` is already logically committed at `end_ts`
- `last_committed_tx_ts` is still old

Timeline:

```text
W = normal writer
E = exclusive/DDL tx

t0: E begins and pins an old snapshot
t1: W enters CommitEnd
t2: W stores Committed(commit_ts)
t3: W has not yet advanced last_committed_tx_ts

t4: E runs acquire_exclusive_tx
t5: E checks has_preparing_tx_other_than == false because W is Committed
t6: E checks last_committed_tx_ts; still old
t7: E can acquire exclusive_tx from the old snapshot
```

There is an additional TOCTOU edge if the exclusive check tries to combine a
watermark load with a separate scan of `txs`:

```text
t0: E checks last_committed_tx_ts; still old
t1: W advances last_committed_tx_ts
t2: W removes itself from txs
t3: E scans txs and does not see W
```

So a correct fix must make the watermark observation and in-flight committed
transaction observation atomic with respect to the writer's watermark
update/removal, or use a different single source of truth that cannot miss both
states.

### How to reproduce it

Use the existing `CommitYieldPoint::BeforeGlobalHeaderUpdate`, which fires after
the writer is `Committed(ts)` but before `last_committed_tx_ts` is updated:

- Create table `t(id INTEGER PRIMARY KEY, v INTEGER)`.
- `W`: `BEGIN CONCURRENT; INSERT INTO t VALUES (2, 200);`
- `E`: `BEGIN DEFERRED; SELECT COUNT(*) FROM t;` to pin an old snapshot.
- Install a yield injector on `W` for
  `CommitYieldPoint::BeforeGlobalHeaderUpdate`.
- Step `W`'s `COMMIT` until the yield point.
- Assert `W` is `TransactionState::Committed(commit_ts)`.
- Assert `last_committed_tx_ts < commit_ts`.
- While `W` is yielded, run `E`: `CREATE INDEX idx_v ON t(v)`.

Buggy behavior:

```text
CREATE INDEX succeeds while W is Committed(ts) but not reflected in
last_committed_tx_ts.
```

Expected behavior:

```text
CREATE INDEX returns Busy and must not acquire exclusive_tx.
```

Regression test:

```text
test_create_index_exclusive_acquire_blocks_committed_before_watermark
```
