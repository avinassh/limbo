# VACUUM Bugs Found

## Bug 1: VACUUM INTO doesn't unescape doubled single quotes in path

**Location**: `core/translate/vacuum.rs:67-85` - `extract_path_from_expr`

The parser stores string literals with their outer quotes. The extraction code
uses `s.trim_matches('\'').trim_matches('"')` which strips arbitrary numbers
of surrounding quotes but does NOT unescape doubled single quotes (`''` → `'`).

**Reproduction**:
```
VACUUM INTO '/tmp/foo''.db';
```
SQLite: creates file `/tmp/foo'.db` (one quote)
Turso: creates file `/tmp/foo''.db` (literal two quotes)

**Also affected**: Several pathological cases like `VACUUM INTO ''foo''` where
the `trim_matches` call strips two quotes from each side rather than just one
(the parser's literal includes exactly one pair of surrounding quotes).

**Impact**: Paths that contain single quotes (legal via SQL escape) are written
to the wrong file names, silently creating files in unexpected locations.

## Bug 2: VACUUM fails on databases with MATERIALIZED VIEWs

**Location**: `core/vdbe/vacuum.rs` — `classify_schema_entries` and the target
build state machine; interaction with materialized views' DBSP backing tables.

When a materialized view `mv` exists, sqlite_master has 4 entries:
- `t` (the base table)
- `mv` (a view)
- `__turso_internal_dbsp_state_v1_mv` (a table, storage-backed)
- `sqlite_autoindex___turso_internal_dbsp_state_v1_mv_1` (its primary-key index)

The VACUUM replay creates `__turso_internal_dbsp_state_v1_mv` as a regular
storage-backed table during phase 1 (tables_to_create). Then phase 4
(post_data_entries) re-executes the `CREATE MATERIALIZED VIEW mv ...`
statement, which internally also creates its DBSP state table — colliding
with the one created in phase 1.

**Reproduction**:
```
CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT);
INSERT INTO t VALUES (1, 'hello');
CREATE MATERIALIZED VIEW mv AS SELECT * FROM t WHERE a > 0;
VACUUM;
-- Error: Parse error: table "__turso_internal_dbsp_state_v1_mv" already exists
```

Same failure on `VACUUM INTO '...'`. The state after a failed VACUUM in-place
appears to be usable, but the compacted image was never produced, so the user
sees a hard error with no recovery path — they cannot VACUUM a database that
contains any materialized view at all.

**Impact**: VACUUM (and VACUUM INTO) is completely broken for any database that
uses materialized views, one of the experimental features that VACUUM is
supposed to be compatible with (per `vacuum_target_opts_from_source`).

## Bug 3: In-place VACUUM panics on an empty (never-initialized) database

**Location**: `core/vdbe/vacuum.rs` — `VacuumInPlacePhase::BeginSourceTx`
calling `source_pager.begin_exclusive_tx()`.

If a fresh connection executes `VACUUM` without having done anything else that
allocates page 1, the call fails with a user-visible InternalError:

```
$ tursodb empty.db "VACUUM;"
Error: Internal error: begin_exclusive_tx can be done on an initialized database
(page 1 must already be allocated)
```

`VACUUM INTO '...';` on the same empty connection succeeds because it uses the
regular `BEGIN` path. SQLite3 on an equivalent empty database returns success
and VACUUM is a no-op.

**Reproduction**:
```bash
rm -f /tmp/empty.db*
tursodb /tmp/empty.db "VACUUM;"   # hard InternalError
```

**Impact**: Running `VACUUM` on a completely fresh database as the first
statement produces an InternalError. Users who wrap VACUUM in maintenance
scripts on possibly-empty databases will see a scary failure. The check should
either be skipped (VACUUM of an uninitialized db is a no-op) or the preflight
should initialize page 1.

## Bug 4: VACUUM schema name comparison is case-sensitive

**Location**: `core/translate/vacuum.rs:47-53`

```rust
if schema_name != "main" {
    bail_parse_error!(
        "VACUUM is only supported for the main database; schema '{}' is not supported yet",
        schema_name
    );
}
```

SQL identifiers (including schema names) are case-insensitive in SQLite.
Turso's comparison uses `!=` directly on strings, so only lowercase `main`
is accepted.

**Reproduction**:
```
$ tursodb x.db "CREATE TABLE t(a); VACUUM MAIN;"
Error: Parse error: VACUUM is only supported for the main database; schema
'MAIN' is not supported yet

$ tursodb x.db "CREATE TABLE t(a); VACUUM Main;"
Error: Parse error: VACUUM is only supported for the main database; schema
'Main' is not supported yet
```

SQLite3 accepts both `VACUUM MAIN` and `VACUUM 'main'` without issue.

**Impact**: Users who write `VACUUM MAIN;` (a valid SQL incantation accepted
by SQLite) get a misleading "schema is not supported yet" error from Turso,
suggesting that the problem is feature support when it's actually a
case-sensitivity bug. Same failure mode on any case variant other than the
exact lowercase `main`.

## Bug 5: VACUUM INTO silently writes plaintext output from an encrypted source

**Location**: `core/vdbe/execute.rs` in `op_vacuum_into_inner`
(VacuumIntoOpPhase::Init).

The opcode opens the output database without forwarding the source connection's
encryption settings:

```rust
let io: Arc<dyn crate::IO> = Arc::new(crate::io::PlatformIO::new()?);
let output_db = crate::Database::open_file_with_flags(
    io,
    dest_path,
    OpenFlags::Create,
    output_opts,
    None,  // <-- encryption opts are always None
)?;
let output_conn = output_db.connect()?;  // <-- no encryption key either
```

`vacuum_target_opts_from_source` carries the `with_encryption(...)` *feature
flag* through, but the `EncryptionOpts` (cipher + key) and
`connect_with_encryption` path are only used for the in-place VACUUM temp
database (see `open_vacuum_temp_db`/`vacuum_temp_db_encryption`). The VACUUM
INTO path has no equivalent wiring.

**Reproduction**:
```
$ tursodb --experimental-encryption encrypted.db "
  PRAGMA cipher='aes256gcm';
  PRAGMA hexkey='000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f';
  CREATE TABLE secrets(u, pw);
  INSERT INTO secrets VALUES('user1','sensitive_password_abc');
  VACUUM INTO 'out.db';
"
$ head -c 16 encrypted.db           # source header
Turso...
$ head -c 16 out.db                 # VACUUM INTO output
SQLite format 3.
$ strings out.db | grep -E 'user1|sensitive'
sensitive_password_abc
user1
$ tursodb out.db "SELECT * FROM secrets;"
user1|sensitive_password_abc
```

The output file has the standard `SQLite format 3` magic and is readable
without the encryption key — sensitive data appears in plaintext inside the
file image and via grep / strings / `tursodb` without any pragma.

**Impact**: Serious confidentiality violation. Users who run `VACUUM INTO`
as an encrypted-to-encrypted backup expecting the destination to carry the
same protection instead silently produce a plaintext copy of the full
database. In-place `VACUUM` on the same source remains encrypted (that path
correctly forwards the key through `open_vacuum_temp_db`), so the bug is
specific to `VACUUM INTO` and easy to miss in casual testing.

## Bug 6: VACUUM clobbers sqlite_sequence.seq when the manual value is lower than max(rowid)

**Location**: `core/vdbe/vacuum.rs` — `build_copy_sql` + `classify_schema_entries`
(ordering of copies) combined with the AUTOINCREMENT counter machinery that
fires on every INSERT the copy loop issues.

The sqlite_sequence row has `rowid=1`, before the AUTOINCREMENT table `t`
(whose rowid is 2). VACUUM copies tables in rowid-ordered sequence, so:

1. sqlite_sequence is copied first. `INSERT OR REPLACE INTO sqlite_sequence
   VALUES ('t', 50)` writes the source value (50) into the target.
2. Table `t` data is copied. Each INSERT with an explicit rowid (e.g., id=100)
   fires the target's AUTOINCREMENT counter machinery, which bumps
   sqlite_sequence.seq to `max(existing, inserted_id) = max(50, 100) = 100`.

The intended invariant is that the target's sqlite_sequence reflects the
source's seq exactly. SQLite's `vacuum.c` ships data through the page layer
directly and does not fire AUTOINCREMENT tracking during the rebuild, so it
preserves the source value unchanged. Turso's `todo: sqlite disables
AUTOINCREMENT during vacuum, but we don't have such a way yet` comment at
`core/vdbe/vacuum.rs:725` acknowledges the gap.

**Reproduction**:
```
CREATE TABLE t(id INTEGER PRIMARY KEY AUTOINCREMENT, val);
INSERT INTO t(val) VALUES('a');
INSERT INTO t(id, val) VALUES(100, 'b');
UPDATE sqlite_sequence SET seq = 50 WHERE name = 't';
SELECT seq FROM sqlite_sequence;  -- source: 50
VACUUM;
SELECT seq FROM sqlite_sequence;  -- SQLite: 50, Turso: 100
```

Same divergence on `VACUUM INTO '...'`: the destination also reports seq=100
instead of the source's 50.

**Impact**: Apps that manually manage AUTOINCREMENT counters (to reset them,
reserve a range, or skip values) will silently lose their manual seq after
VACUUM — the next INSERT will produce a rowid based on the clobbered value.
Because SQLite documents and preserves this manual override, portable code
relying on it will break on Turso only after a VACUUM.

## Bug 7: `PRAGMA page_size=N` followed by `VACUUM` does not change page_size

**Location**: `core/vdbe/vacuum.rs` `VacuumInPlacePhase::ReadSourceMetadata`
and `core/vdbe/execute.rs` `VacuumIntoOpPhase::Init` — both read the pager's
current page_size and use it verbatim.

SQLite documents that `PRAGMA page_size=N;` prior to `VACUUM` is the
supported way to change an existing database's page size (see
<https://sqlite.org/pragma.html#pragma_page_size>: "The page_size pragma is
intended for use when initially creating a database file *or else prior to
a VACUUM or ALTER operation*"). Turso accepts the pragma silently (no error)
but the subsequent VACUUM reads the old page_size from the source pager:

```rust
// vdbe/vacuum.rs — in-place
let page_size = source_pager.get_page_size().map(|ps| ps.get()).unwrap_or(4096);
// ...
let temp_db = open_vacuum_temp_db(connection, &source_db, page_size, reserved_space)?;

// vdbe/execute.rs — VACUUM INTO
let page_size: u32 = extract_pragma_int(
    &program.connection.pragma_query(&format!("\"{escaped_schema_name}\".page_size"))?,
    "page_size",
)?;
```

Both read the *current* page_size rather than the pending override, so the
target is built at the same size as the source.

**Reproduction** (in-place):
```
$ tursodb x.db "CREATE TABLE t(a); INSERT INTO t VALUES(1);"
$ tursodb x.db "PRAGMA page_size=8192; VACUUM; PRAGMA page_size;"
4096
# SQLite on equivalent input: 8192
```

**Reproduction** (VACUUM INTO):
```
$ tursodb x.db "PRAGMA page_size=8192; VACUUM INTO 'y.db';"
$ tursodb y.db "PRAGMA page_size;"
4096
# SQLite: 8192
```

**Impact**: Users cannot migrate an existing database to a different page
size via VACUUM. The pragma-plus-VACUUM workflow is the SQLite-documented
way to do this; portable code that relies on it silently keeps the old page
size on Turso. The bug is silent (no error) so there is no indication the
intended change did not happen.

## Bug 8: `VACUUM INTO` does not accept expressions or parameter binding for the path

**Location**: `core/translate/vacuum.rs:67-85` `extract_path_from_expr`.

Turso's parser dispatches `VACUUM INTO <expr>` to a helper that only
matches `Expr::Literal(Literal::String(_))` or `Expr::Id(_)` and rejects
every other AST node with "VACUUM INTO requires a string literal path".
SQLite, in contrast, treats the destination as an ordinary expression: it
permits `VACUUM INTO ?`, `VACUUM INTO ('/tmp/' || 'foo.db')`, and any other
scalar expression that evaluates to a string at run time.

**Reproduction**:
```sql
-- All three accepted by SQLite, rejected by Turso:
VACUUM INTO ?;                        -- parameter binding
VACUUM INTO '/tmp/' || 'foo.db';      -- expression
VACUUM INTO :dest;                    -- named parameter
```

Turso output:
```
Error: Parse error: VACUUM INTO requires a string literal path
```

SQLite, via the Python binding, correctly executes `VACUUM INTO ?` with a
supplied parameter value and writes the output to the bound path.

**Impact**: Library callers cannot parameterize the destination path —
every backup site must interpolate the path into the SQL text instead of
using the driver's binding API. That forces string concatenation at the
application level, opens a SQL-injection surface if user-supplied paths are
ever passed, and breaks portable code that prepares a statement once and
reuses it with different paths.

## Bug 9: VACUUM adds a spurious sqlite_autoindex_* row to sqlite_master for `__turso_internal_types`

**Location**: `core/vdbe/vacuum.rs::vacuum_target_build_step` —
`VacuumTargetBuildPhase::PrepareCreateTable` replays the source's CREATE
TABLE SQL under `start_nested()`/`end_nested()` (for system tables). The
in-memory implicit PK index on `__turso_internal_types(name)` lands in
sqlite_master during the target build, but the source's sqlite_master never
contained that row (the bootstrap path that originally created the table
evidently skips registering the autoindex).

**Reproduction**:
```
$ tursodb --experimental-custom-types x.db \
    "CREATE TYPE pos_int BASE INTEGER; CREATE TABLE t(a pos_int);"
$ tursodb --experimental-custom-types x.db \
    "SELECT type, name FROM sqlite_master;"
table|__turso_internal_types
table|t

$ tursodb --experimental-custom-types x.db "VACUUM;"
$ tursodb --experimental-custom-types x.db \
    "SELECT type, name FROM sqlite_master;"
table|__turso_internal_types
index|sqlite_autoindex___turso_internal_types_1
table|t
```

**Impact**: The user-visible sqlite_master row count grows by one every
time a user runs VACUUM on a database that has ever used `CREATE TYPE`.
Apps that enumerate sqlite_master (introspection tools, schema diffs,
dbhash-style checksums) will see an unexpected new row. Because this is
a one-way mutation — the spurious row persists after VACUUM and is itself
copied by any future VACUUM — there is no way to get back to the original
sqlite_master shape without manual intervention.

## Bug 10: VACUUM does not renumber rowids for tables without INTEGER PRIMARY KEY (SQLite compat)

**Location**: `core/vdbe/vacuum.rs::build_copy_sql` — when `btree.has_rowid`
is true the function always prepends a rowid-alias pseudo-column to both
the SELECT and INSERT column lists, so the source rowids are copied
verbatim into the target.

SQLite's documentation notes that "The VACUUM command may change the
ROWIDs of entries in any table that does not have an explicit INTEGER
PRIMARY KEY." In practice SQLite's `vacuum.c` issues `INSERT INTO NEW.t
SELECT * FROM OLD.t` (no rowid column), which causes a contiguous
renumbering (1, 2, 3, …) in the target. Turso carries the original rowid
across.

**Reproduction**:
```
CREATE TABLE t(a TEXT);  -- has_rowid, no INTEGER PRIMARY KEY
INSERT INTO t VALUES('a'), ('b'), ('c'), ('d');
DELETE FROM t WHERE a IN ('b','c');
SELECT rowid, a FROM t;   -- 1|a, 4|d
VACUUM;
SELECT rowid, a FROM t;
-- SQLite: 1|a, 2|d     (renumbered)
-- Turso:  1|a, 4|d     (preserved)
```

**Impact**: SQLite-compat divergence. Applications that:
- serialize rowids as external identifiers and *expect* them to stay stable
  across VACUUM (portable SQLite apps have historically had to treat this
  as unsafe because SQLite says so),
- use `max(rowid)` after VACUUM to estimate density or available slots, or
- rely on sparse-rowid maintenance via VACUUM for space reuse,

will observe a different post-VACUUM shape on Turso vs SQLite. The same
divergence appears on `VACUUM INTO`: the destination carries forward the
source rowids instead of tight-packing them.

## Bug 11: VACUUM fails on CHECK-constraint-violating rows (SQLite preserves)

**Location**: `core/vdbe/vacuum.rs::build_copy_sql` + target `INSERT INTO ... VALUES`
path. The VACUUM copies via SQL `INSERT`, which the INSERT opcode compiles with
a full CHECK-constraint enforcement epilogue. There is no "disable CHECK" flag
mirrored to the target build connection.

SQLite's `vacuum.c` copies data via the page/B-tree layer (xfer optimization
and raw INSERT with constraints disabled), so pre-existing rows that violate
CHECK constraints survive unchanged. Turso's VACUUM re-evaluates CHECK on every
copied row and aborts on the first violation.

**Reproduction**:
```
CREATE TABLE t(a INTEGER CHECK(a > 0));
PRAGMA ignore_check_constraints=ON;
INSERT INTO t VALUES(-5);    -- row persisted because constraint is bypassed
VACUUM;
-- SQLite:  succeeds, row preserved
-- Turso:   Error: Runtime error: CHECK constraint failed: a > 0 (19)
```

Same behavior on `VACUUM INTO '/tmp/out.db'`. Also triggers whenever a CHECK
constraint was added via `ALTER TABLE ADD COLUMN ... CHECK(...) DEFAULT ...`
against pre-existing rows whose default would violate the new constraint —
Turso accepts the ALTER and stores the violating data, but a subsequent
VACUUM then fails.

**Impact**: VACUUM becomes a footgun on any database where historical data
predates a CHECK constraint, or where `ignore_check_constraints=ON` was ever
used. The source remains usable after the failure, but the user cannot compact
the database until every offending row is deleted by hand. Portable code that
works unchanged on SQLite silently breaks on Turso's VACUUM.

## Bug 12: VACUUM INTO rejects pre-existing zero-length destination files

**Location**: `core/vdbe/execute.rs:14394` —
`if std::path::Path::new(dest_path).exists()` returns an unconditional
"output file already exists" error.

SQLite's documented behavior for `VACUUM INTO` accepts an existing zero-length
destination file: `SQLITE_OPEN_CREATE | SQLITE_OPEN_READWRITE` is happy to
fill an empty placeholder. Many deployments pre-create the destination file
(e.g., via `touch` from a shell driver or from the kernel open-with-O_CREAT
path) and expect VACUUM INTO to write into it.

**Reproduction**:
```
$ touch /tmp/out.db                 # empty file, size 0
$ sqlite3 src.db "VACUUM INTO '/tmp/out.db';"   # SQLite: succeeds, out.db filled
$ tursodb src.db   "VACUUM INTO '/tmp/out.db';" # Turso: Error: Parse error: output file already exists: /tmp/out.db
```

**Impact**: Portable backup/replication scripts that rely on pre-creating
the destination (common for mode/ownership control) fail on Turso. The
workaround is to unlink the file first, which is racy and breaks "touch"-based
pre-allocation. The check is too strict: it should be `metadata(dest).len() != 0`
before rejecting.

## Bug 13: VACUUM fails on user tables with `SQLITE_MAX_COLUMN` columns

**Location**: `core/vdbe/vacuum.rs::build_copy_sql` adds a leading rowid alias
pseudo-column to the copy `SELECT` whenever `has_rowid` is true. The statement
then exceeds `core/translate/select.rs::SQLITE_MAX_COLUMN` (2000) by one
column and bails with "too many columns in result set". SQLite's VACUUM
copies via the page/xfer layer rather than a SELECT, so it does not hit this
limit.

**Reproduction**:
```
CREATE TABLE t(c1 INTEGER DEFAULT 0, c2 INTEGER DEFAULT 0, ..., c2000 INTEGER DEFAULT 0);
-- (exactly 2000 columns, no INTEGER PRIMARY KEY — table has an implicit rowid)
INSERT INTO t DEFAULT VALUES;
VACUUM;
-- SQLite: succeeds
-- Turso:  Error: Parse error: too many columns in result set
```

1999 non-PK columns + the synthetic `rowid` alias = 2000 and works. 2000
non-PK columns + synthetic rowid = 2001 and exceeds the limit.

Declaring an `INTEGER PRIMARY KEY` alias column changes the behavior — the
alias column IS the rowid, so no extra pseudo-column is prepended and the
select still sits at 2000. The bug therefore surfaces only when the source
has hit the full SQLITE_MAX_COLUMN budget and does not declare an explicit
INTEGER PRIMARY KEY.

**Impact**: A user who imports a wide schema (analytics tables near the
SQLite column limit) can successfully create and query the table, but VACUUM
fails permanently with no clean workaround short of a schema migration to
introduce an INTEGER PRIMARY KEY.

## Bug 14: VACUUM INTO leaks the destination file on mid-vacuum failure

**Location**: `core/vdbe/execute.rs::cleanup_op_vacuum_into` —
the cleanup routine calls `target_build_context.cleanup_after_error()` and
drops `_output_db`, but never unlinks `dest_path`.

Any mid-vacuum failure after the destination handle is opened (e.g., a CHECK
constraint violation during the copy loop per Bug 11, a unique violation on
a secondary index, etc.) leaves the freshly-written `dest.db` and `dest.db-wal`
on disk. On retry, the preflight existence check (Bug 12's same line) rejects
the operation with "output file already exists: ...", so the user cannot retry
without manually removing the leftovers. The partial file is a valid SQLite
header but an incomplete image.

**Reproduction** (re-using Bug 11 as the failure trigger):
```
CREATE TABLE t(a INTEGER CHECK(a > 0));
PRAGMA ignore_check_constraints=ON;
INSERT INTO t VALUES(-5);
VACUUM INTO '/tmp/out.db';    -- fails mid-copy on CHECK
-- Error: Runtime error: CHECK constraint failed: a > 0 (19)
$ ls -la /tmp/out.db*
-rw-rw-r-- 4096 /tmp/out.db       <-- leaked
-rw-rw-r--    0 /tmp/out.db-wal   <-- leaked

VACUUM INTO '/tmp/out.db';    -- retry
-- Error: Parse error: output file already exists: /tmp/out.db
```

**Impact**: Failed VACUUM INTO transitions the backup driver into a "needs
manual cleanup" state that prevents retries. In unattended maintenance
scripts (cron/scheduled jobs), this cascades into repeated failures until
a human intervenes. SQLite's vacuum.c removes the output file on error paths
via its `pDestDb->onError` cleanup; Turso's corresponding cleanup is missing
the unlink.

## Bug 15: In-place VACUUM on an MVCC database silently demotes the source to WAL journal mode

**Location**: `core/vdbe/vacuum.rs::VacuumInPlacePhase` — after the copy-back
commits, the source file still contains `__turso_internal_mvcc_meta`, but the
journal-mode detection that fresh connections run at open time returns `wal`
instead of `mvcc`. The VACUUM's TRUNCATE checkpoint and subsequent schema
reload leave the source looking like a plain WAL database.

**Reproduction**:
```
PRAGMA journal_mode='mvcc';
CREATE TABLE t(a);
INSERT INTO t VALUES(1);
PRAGMA wal_checkpoint(TRUNCATE);    -- required preflight for MVCC VACUUM
-- New connection reports: journal_mode = mvcc  ✓
VACUUM;
-- New connection after VACUUM reports: journal_mode = wal  ✗
SELECT type, name FROM sqlite_master;
-- table|__turso_internal_mvcc_meta   (still physically present)
-- table|t
```

After VACUUM, a user has to manually `PRAGMA journal_mode='mvcc'` to re-enter
MVCC mode. In the default-connection path (which opens, reads, uses), code
that depended on MVCC semantics (snapshot isolation etc.) silently switches to
plain WAL semantics with no error.

`VACUUM INTO` on the same MVCC source is unaffected — the destination image
is correctly tagged as MVCC and fresh connections to it report `journal_mode = mvcc`.

**Impact**: Applications that use MVCC for snapshot-isolated reads will, after
any maintenance VACUUM, revert to plain WAL isolation without any user-visible
signal. Correctness-sensitive workloads relying on MVCC could read data under
incompatible isolation semantics until someone runs `PRAGMA journal_mode='mvcc'`
again. This is a silent feature downgrade, not a loss of data, but it bypasses
the consent-required nature of journal_mode changes.

## Bug 16: VACUUM INTO adds a spurious CDC commit-marker record on the source when CDC is enabled

**Location**: `core/vdbe/execute.rs:14404` and `14526` — `op_vacuum_into_inner` wraps the
operation in a `BEGIN` / `COMMIT` pair on the *source* connection to pin source
state during the copy. The final `COMMIT` goes through the regular
transaction-commit machinery, which emits a CDC `change_type=2` (commit-marker)
row for the source connection's CDC stream. The in-place `VACUUM` path does not
go through regular `BEGIN`/`COMMIT` on the source (it drives the source
directly at the WAL layer), so no spurious CDC record is emitted there.

**Reproduction**:
```
CREATE TABLE t(a);
PRAGMA unstable_capture_data_changes_conn='full';
INSERT INTO t VALUES(1);
SELECT 'before:', change_id, change_type FROM turso_cdc;
-- before: (1|1|t), (2|2|)  — INSERT + its commit marker
VACUUM INTO '/tmp/out.db';
SELECT 'after:', change_id, change_type FROM turso_cdc;
-- after:  (1|1|t), (2|2|), (3|2|)  ← new commit marker from VACUUM INTO
```

The source's `turso_cdc` table has gained a row after a read-only
maintenance operation that should not have modified any user data. For an
in-place `VACUUM` in the same scenario the CDC log is unchanged. Each
subsequent `VACUUM INTO` adds another commit-marker: running it twice in a
row goes 2 → 3 → 4 CDC rows even though user data never changed.

**Impact**: CDC consumers observing the source's CDC stream see a spurious
commit marker for every successful `VACUUM INTO`. Tooling that derives
transaction boundaries from commit markers (replication, logical decoding,
audit logs) will record phantom transactions tied to backup/maintenance
operations. The two VACUUM opcodes also diverge in observable side-effects,
so portability of the "CDC of a read-only operation should be empty"
invariant depends on which opcode the operator happened to choose.
Reproduces with every CDC mode (`'id'`, `'before'`, `'after'`, `'full'`).

## Bug 17: VACUUM rewrites header change_counter to 1, losing the source's counter value

**Location**: `core/vdbe/vacuum.rs::VacuumDbHeaderMeta::from_source_header` +
`apply_to` — the header fields copied by VACUUM do not include
`change_counter` (offset 24-27) or `version_valid_for` (offset 92-95), and
the target DB is built with a fresh pager header where `change_counter=1`.
In-place VACUUM commits the rebuilt image, which overwrites the source's
page 1 with the target's page 1.

SQLite's VACUUM bumps `change_counter` by 1 (relative to the prior value),
keeping the invariant that `change_counter` is monotonically non-decreasing
across every write. This is how SQLite's read transactions detect that the
database has changed between reads — a cached `change_counter` that is
less than the current one means "refresh the cache." Turso's VACUUM
replaces the counter with 1, which can be *lower* than what a process
observed before the VACUUM.

**Reproduction**:
```
$ sqlite3 /tmp/x.db "CREATE TABLE t(a); INSERT INTO t VALUES(1);
                     INSERT INTO t VALUES(2); INSERT INTO t VALUES(3);
                     /* ... more inserts ... */"
$ xxd -s 24 -l 4 /tmp/x.db     # change_counter = 0x0b (11)
00000018: 0000 000b                                ....
$ tursodb /tmp/x.db "VACUUM;"
$ xxd -s 24 -l 4 /tmp/x.db     # change_counter = 1 (!)
00000018: 0000 0001                                ....
```

The `version_valid_for` field (offset 92) similarly moves from SQLite's
`change_counter`-tracking value to Turso's hardcoded `0x002e7e58`
(3047000, the SQLite-3.47.0 library version). Both fields lose their
SQLite-compatibility semantics.

**Impact**: A concurrent SQLite reader that cached `change_counter=11`
before Turso's VACUUM will, on its next access, see `change_counter=1`.
SQLite's read-path logic uses "if cached != current, refresh" — a lower
current value after a write still triggers a refresh, but tooling that
reasons about monotonic counters (backup tools, replication cursors,
monitoring) will misinterpret the reset as a database rewind. Same
behaviour on both `VACUUM` in-place and `VACUUM INTO` (the destination
always starts with `change_counter=1`).

## Bug 18: VACUUM fails on SQLite-created databases containing WITHOUT ROWID tables (incl. FTS5/RTREE backing tables)

**Location**: `core/vdbe/vacuum.rs::vacuum_target_build_step` —
`VacuumTargetBuildPhase::PrepareCreateTable` calls
`state.target_conn.prepare(sql_str)` on each source-table CREATE
statement. Turso's parser rejects `WITHOUT ROWID` outright
(`parser/src/parser.rs`), so the replay of any CREATE TABLE that contains
that suffix errors out with `"WITHOUT ROWID tables are not supported"`.

This blocks VACUUM on any SQLite-created database that uses WITHOUT ROWID
tables directly, and also on any database that uses FTS5 or RTREE — both
of which create WITHOUT ROWID backing tables (`fts_idx`, `fts_config`,
etc.) as part of their virtual-table bootstrap. Turso can open and even
modify such a database partway (SELECT from unrelated tables, create new
user tables), because the schema-load path skips unparseable tables
rather than failing the open; but VACUUM tries to re-create every
storage-backed table via `prepare()`, which hits the WITHOUT ROWID
rejection and aborts.

**Reproduction**:
```
$ sqlite3 /tmp/fts.db "CREATE VIRTUAL TABLE fts USING fts5(c);
                       INSERT INTO fts VALUES('hello');"
$ tursodb /tmp/fts.db "SELECT type, name FROM sqlite_master;"
# Turso prints: fts, fts_data, fts_idx, fts_content, fts_docsize, fts_config
# (fts_idx and fts_config are WITHOUT ROWID backing tables)
$ tursodb /tmp/fts.db "VACUUM;"
Error: Parse error: WITHOUT ROWID tables are not supported

$ sqlite3 /tmp/user_wor.db "CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT) WITHOUT ROWID;
                            INSERT INTO t VALUES(1, 'hello');"
$ tursodb /tmp/user_wor.db "VACUUM;"
Error: Parse error: WITHOUT ROWID tables are not supported
```

Same failure on `VACUUM INTO` — the destination file is left behind at
its partial-build size (extension of Bug 14).

**Impact**: Users migrating from or interoperating with SQLite cannot run
VACUUM/VACUUM INTO on any database that contains WITHOUT ROWID tables —
including the FTS5/RTREE backing tables they never explicitly created.
Because Turso otherwise presents these databases as partially-usable
(sqlite_master reads return the rows, unrelated tables are writable),
users can accumulate work into such a database and only discover the
VACUUM gap at maintenance time, with no clean workaround short of
migrating the data to a fresh database.

## Bug 19: VACUUM INTO writes an extra empty `.db-wal` sidecar file to the destination path

**Location**: `core/vdbe/execute.rs::op_vacuum_into_inner`
(`VacuumIntoOpPhase::Init`) — the output database is opened with Turso's
default WAL mode, which allocates and persists a WAL sidecar file even
when zero frames are committed to it. `finalize_vacuum_into_output`
runs a TRUNCATE checkpoint, but the WAL file itself is retained as a
zero-byte sidecar after the handle is closed.

SQLite's `VACUUM INTO` produces only the `.db` file — the destination
opens in the default rollback-journal mode and the journal file is
deleted after each statement. Turso's destination opens in WAL mode
regardless of whether the source was in WAL or rollback mode, so the
destination directory ends up with both `dest.db` and `dest.db-wal`.

**Reproduction**:
```
$ tursodb /tmp/src.db "CREATE TABLE t(a); INSERT INTO t VALUES(1);"
$ tursodb /tmp/src.db "VACUUM INTO '/tmp/dst.db';"
$ ls -la /tmp/dst.db*
-rw-rw-r-- 1 ubuntu ubuntu 8192 /tmp/dst.db
-rw-rw-r-- 1 ubuntu ubuntu    0 /tmp/dst.db-wal

$ sqlite3 /tmp/src.db "VACUUM INTO '/tmp/sq_dst.db';"
$ ls -la /tmp/sq_dst.db*
-rw-rw-r-- 1 ubuntu ubuntu 8192 /tmp/sq_dst.db
# (no -wal file)
```

**Impact**: Portable backup scripts that stream the `.db` file to cloud
storage or compress it as a single artifact miss the `.db-wal` sidecar.
The sidecar is empty and technically safe to drop, but its mere
existence surprises users who expect VACUUM INTO to produce a single
self-contained file (as SQLite does). Tooling that uses the dest path to
detect "did the VACUUM finish cleanly?" (via mtime or size checks) will
see the `.db-wal` and misdiagnose incomplete-backup. Because Turso also
defaults to WAL mode, the journal_mode bytes (offset 18-19) in the dest
header are `02 02` regardless of the source's journal mode — a related
divergence from SQLite's `VACUUM INTO`, which always produces a rollback
-journal destination (`01 01`).

## Bug 20: VACUUM INTO mid-copy failure from parser rejection leaks the destination file (extension of Bug 14)

**Location**: `core/vdbe/execute.rs::cleanup_op_vacuum_into` — the same
cleanup gap as Bug 14, but triggered by the target build's parser
(Bug 18) rather than by a runtime constraint violation (Bug 11).

**Reproduction** (re-using Bug 18's failure trigger):
```
$ sqlite3 /tmp/fts.db "CREATE VIRTUAL TABLE fts USING fts5(c);
                       INSERT INTO fts VALUES('hello');"
$ tursodb /tmp/fts.db "VACUUM INTO '/tmp/fts_dst.db';"
Error: Parse error: WITHOUT ROWID tables are not supported
$ ls -la /tmp/fts_dst.db*
-rw-rw-r-- 1 ubuntu ubuntu 4096 /tmp/fts_dst.db       # leaked
-rw-rw-r-- 1 ubuntu ubuntu    0 /tmp/fts_dst.db-wal   # leaked

$ tursodb /tmp/fts.db "VACUUM INTO '/tmp/fts_dst.db';"
Error: Parse error: output file already exists: /tmp/fts_dst.db
```

**Impact**: Same retry-unfriendly state as Bug 14: a failed VACUUM INTO
leaves the destination half-built and blocks subsequent attempts with
the "already exists" preflight. The cleanup function's scope needs to
cover target-build parse errors, not just runtime failures. Together
with Bug 18, this means users with FTS5/RTREE-using databases hit a
double footgun: VACUUM INTO can't finish, AND retry fails until the
operator manually deletes the leaked output.

## Bug 21: VACUUM INTO destination file is opened in WAL journal mode, so bytes 18/19 of the output header differ from any SQLite-created source

**Location**: `core/vdbe/execute.rs::op_vacuum_into_inner`
(`VacuumIntoOpPhase::Init`) — when the output is opened via
`Database::open_file_with_flags`, Turso's default pager-opens-WAL policy
applies; there's no "honour the source's journal mode" code path. The
file-format-write-version / file-format-read-version bytes (offsets 18
and 19 of the SQLite header, per <https://www.sqlite.org/fileformat.html>)
are always written as `02 02` on the destination.

SQLite's `VACUUM INTO` writes `01 01` to the destination (rollback mode)
regardless of the source. This field is a *self-describing* hint about
the journal mode used at file creation; SQLite will continue to upgrade
the bytes when WAL mode is enabled on the resulting file.

**Reproduction**:
```
$ sqlite3 /tmp/src.db "CREATE TABLE t(a); INSERT INTO t VALUES(1);"
$ xxd -s 16 -l 4 /tmp/src.db     # src: 1000 0101 (page_size=4096, rollback)

$ tursodb /tmp/src.db "VACUUM INTO '/tmp/turso_dst.db';"
$ xxd -s 16 -l 4 /tmp/turso_dst.db
# 1000 0202 (WAL)

$ sqlite3 /tmp/src.db "VACUUM INTO '/tmp/sqlite_dst.db';"
$ xxd -s 16 -l 4 /tmp/sqlite_dst.db
# 1000 0101 (rollback, preserving the source)
```

**Impact**: Users who run `VACUUM INTO` expecting a bit-identical copy of
the source header (for cross-platform backup, or for tooling that reads
raw header bytes) will see a file-format-version mismatch. The dest
still parses correctly in SQLite — WAL-tagged headers are valid there —
so the divergence is silent: `diff` on the two headers reveals it only
to someone who already suspects it. Combined with Bug 19's sidecar
file and Bug 17's reset change_counter, the Turso VACUUM INTO output
diverges from SQLite's in three independent ways, all pointing to the
destination being "a fresh Turso DB that happens to contain the source's
user data" rather than "a self-similar copy of the source."

## Bug 22: `PRAGMA auto_vacuum=MODE; VACUUM` does not apply the pending mode change

**Location**: `core/vdbe/vacuum.rs::VacuumInPlacePhase::ReadSourceMetadata` and
`core/vdbe/execute.rs::VacuumIntoOpPhase::Init`. Both paths compute
`target_auto_vacuum_mode = source_pager.get_auto_vacuum_mode()` which
ignores any `PRAGMA auto_vacuum=...` pending from the same connection.

The SQLite-documented way to change auto_vacuum mode on an existing
database is `PRAGMA auto_vacuum = <mode>; VACUUM;`. Turso silently
accepts the pragma (its getter returns the new value) but the subsequent
VACUUM reads the *source pager's current mode* rather than the pending
override, so the mode never actually changes on disk. This is distinct
from U2 (which is about the pragma by itself being ineffective on fresh
DBs) because here the pragma + VACUUM pair is explicitly the vector that
SQLite designates for an on-disk mode change.

**Reproduction** (enable auto_vacuum):
```
$ tursodb --experimental-autovacuum x.db \
    "CREATE TABLE t(a); INSERT INTO t VALUES(1),(2);
     PRAGMA auto_vacuum=FULL; VACUUM; PRAGMA auto_vacuum;"
0
$ xxd -s 52 -l 4 x.db     # largest_root_btree_page header field
00000034: 0000 0000     ← auto_vacuum mode byte still 0 (NONE)
# SQLite on equivalent input: 00000034: 0000 0003 (auto_vacuum=FULL)
```

**Reproduction** (disable auto_vacuum — source started with FULL via
sqlite3):
```
$ sqlite3 av.db "PRAGMA auto_vacuum=FULL; CREATE TABLE t(a);
                 INSERT INTO t VALUES(1);"
$ xxd -s 52 -l 4 av.db     # 00000003 (FULL)
$ tursodb --experimental-autovacuum av.db \
    "PRAGMA auto_vacuum=NONE; VACUUM; PRAGMA auto_vacuum;"
1
$ xxd -s 52 -l 4 av.db     # 00000003 (still FULL)
# SQLite on same input correctly switches to 0 (NONE)
```

**Impact**: The pragma-plus-VACUUM workflow to change auto_vacuum mode
is silently ineffective. Portable code that moves a database between
modes via this documented recipe will keep whatever mode the database
was in, with no error or warning. Combined with `reject_unsupported_vacuum_auto_vacuum_mode`
(which blocks VACUUM on incremental-vacuum source DBs), users who want
to downgrade an incremental-vacuum DB to FULL or NONE have no working
path on Turso.

## Bug 23: VACUUM INTO's spurious CDC commit marker is a connection-visible phantom row that never lands on disk

**Location**: Extension of Bug 16 (`core/vdbe/execute.rs::op_vacuum_into_inner`
`VacuumIntoOpPhase::Init` `BEGIN` and `::Done` `COMMIT`). The source
connection's CDC machinery writes a `change_type=2` commit marker row
when the source's wrapping `COMMIT` fires, but that row never makes it
to the persistent `turso_cdc` btree — only the connection's in-memory
view of `turso_cdc` shows it.

Bug 16 observed that VACUUM INTO adds a CDC commit marker. This bug
goes further: that marker is **not durable**. A `PRAGMA wal_checkpoint(FULL)`,
a connection close / reopen, or a second Turso process all reveal that
the row is absent. Other CDC rows emitted by real INSERTs in the same
session are durable; only VACUUM INTO's marker is phantom.

**Reproduction**:
```
$ tursodb /tmp/c.db "
  CREATE TABLE t(a);
  PRAGMA unstable_capture_data_changes_conn='full';
  INSERT INTO t VALUES(1);
  VACUUM INTO '/tmp/out.db';
  SELECT count(*) FROM turso_cdc;        -- 3  (INSERT + its commit + VACUUM-INTO commit)
  PRAGMA wal_checkpoint(FULL);
  SELECT count(*) FROM turso_cdc;"       -- 2  (checkpoint reveals the phantom)

$ tursodb /tmp/c.db 'SELECT count(*) FROM turso_cdc;'
2                                        -- fresh connection: only 2 rows on disk

$ sqlite3 /tmp/c.db 'SELECT change_id FROM turso_cdc;'
1
2                                        -- SQLite confirms: row 3 never persisted
```

The connection that ran VACUUM INTO observes a CDC stream with three
rows before the checkpoint and two rows afterwards. If a subsequent
user-level write commits in the same session, the commit marker row
*does* become durable (via the next write's own commit path), so the
bug is invisible under workloads that don't idle immediately after
VACUUM INTO.

**Impact**: CDC consumers see inconsistent row counts depending on
whether they read through the connection that ran VACUUM INTO or
through a separate connection / process. Replication and audit tools
observe a phantom commit marker that disappears on re-read, breaking
the "CDC log entries are append-only and durable once observed"
invariant. The inconsistency compounds across connections: an operator
running `VACUUM INTO` and immediately querying `turso_cdc` sees a row
that an external CDC tailer will never see.

## Bug 24: VACUUM fails after `ALTER TABLE RENAME COLUMN` on a column referenced by an expression index or a `COLLATE` clause

**Location**: `ALTER TABLE RENAME COLUMN` implementation (pre-existing
bug) does not update the stored CREATE INDEX SQL for two specific index
shapes: expression indexes (`CREATE INDEX ix ON t(col * 2)`) and
column-with-COLLATE indexes (`CREATE INDEX ix ON t(col COLLATE BINARY)`).
When VACUUM later tries to replay the stale CREATE INDEX against the
renamed table, the target connection fails to parse the stale column
reference and aborts the whole VACUUM.

ALTER TABLE RENAME COLUMN correctly updates:
- Direct column references in index column lists (`(col)`)
- Partial index WHERE clauses (including complex ones with parens)
- CHECK constraints (including compound expressions)
- Trigger bodies
- Generated column expressions
- FOREIGN KEY references

But not:
- Index expression list entries like `col * 2`, `printf('%d', col)`, `(a + b)`
- Index column list entries with `col COLLATE ...` suffixes

**Reproduction**:
```
$ tursodb x.db "
  CREATE TABLE t(old_col INTEGER, b TEXT);
  CREATE INDEX ix_expr    ON t(old_col * 2);
  CREATE INDEX ix_collate ON t(old_col COLLATE BINARY);
  INSERT INTO t VALUES (1, 'x');
  ALTER TABLE t RENAME COLUMN old_col TO new_col;
  SELECT sql FROM sqlite_master;"
CREATE TABLE t (new_col INTEGER, b TEXT)
CREATE INDEX ix_expr ON t (old_col * 2)             -- stale, still old_col
CREATE INDEX ix_collate ON t (old_col COLLATE BINARY) -- stale, still old_col

$ tursodb x.db "VACUUM;"
Error: Parse error: Error: invalid expression in CREATE INDEX: old_col * 2
```

The database is still queryable — the indexes work for reads because
Turso's runtime resolves columns against the current table schema —
but VACUUM is broken for the lifetime of the database. VACUUM INTO has
the same failure and leaves a 4-KB partial destination file behind
(Bug 14/20 family cleanup gap).

Verified with SQLite as oracle:
```
$ sqlite3 y.db "CREATE TABLE t(old_col INTEGER, b TEXT);
                CREATE INDEX ix_expr    ON t(old_col * 2);
                CREATE INDEX ix_collate ON t(old_col COLLATE BINARY);
                ALTER TABLE t RENAME COLUMN old_col TO new_col;
                SELECT sql FROM sqlite_master;"
CREATE TABLE t(new_col INTEGER, b TEXT)
CREATE INDEX ix_expr ON t(new_col * 2)                -- correctly updated
CREATE INDEX ix_collate ON t(new_col COLLATE BINARY)  -- correctly updated
```

**Impact**: A user whose schema uses expression or COLLATE indexes on
any column that ever gets renamed hits a permanent VACUUM block. The
database remains serviceable for normal queries, but regular
maintenance (VACUUM, VACUUM INTO for backup) cannot run without a
manual schema-repair (drop and recreate each offending index). The bug
composes nastily with Bug 20: VACUUM INTO against such a database
leaks the destination file AND blocks retries until the operator
unlinks the leaked output.

## Bug 25: VACUUM fails on SQLite-created databases containing INSTEAD OF triggers on views

**Location**: `core/vdbe/vacuum.rs::vacuum_target_build_step`
`VacuumTargetBuildPhase::PreparePostData` replays `CREATE TRIGGER` against
the target connection via `state.target_conn.prepare(&entry.sql)?;`. For an
`INSTEAD OF INSERT ON v` trigger, the target parser cannot resolve `v` as
a table (`core/translate/trigger.rs:161-163` requires the target to be a
btree table, and also rejects `INSTEAD OF` at line 172 even when it IS a
view). The prepare error propagates out of the target build and the
whole VACUUM aborts.

The source DB is fully queryable: SELECT from the table and the view both
work, and INSERT INTO v correctly fires the trigger. Only VACUUM and
VACUUM INTO are affected.

**Reproduction**:
```
$ sqlite3 /tmp/iotr.db "
  CREATE TABLE t(a);
  CREATE VIEW v AS SELECT a FROM t;
  CREATE TRIGGER trg INSTEAD OF INSERT ON v
    BEGIN INSERT INTO t VALUES (NEW.a); END;
  INSERT INTO v VALUES (1);
  INSERT INTO v VALUES (2);
  SELECT * FROM t;"
1
2
$ tursodb --experimental-views /tmp/iotr.db "SELECT * FROM t; SELECT * FROM v;"
1
2
1
2
$ tursodb --experimental-views /tmp/iotr.db "VACUUM;"
Error: Parse error: no such table: v

$ tursodb --experimental-views /tmp/iotr.db "VACUUM INTO '/tmp/iotr_out.db';"
Error: Parse error: no such table: v
$ ls -la /tmp/iotr_out.db*
-rw-rw-r-- 4096 /tmp/iotr_out.db       <-- leaked (Bug 14/20 family)
-rw-rw-r--    0 /tmp/iotr_out.db-wal
```

The `--experimental-views` flag is not required — VACUUM and VACUUM INTO
fail the same way without it, because `vacuum_target_opts_from_source`
carries the source's feature flag through to the target connection.

**Impact**: Users migrating SQLite databases that use INSTEAD OF triggers
(a common pattern for making views updatable, or for logging/mirroring
writes) cannot run VACUUM on Turso. The database remains usable for reads
and INSTEAD OF-mediated writes, so users may not notice the VACUUM gap
until maintenance time. Combined with Bug 14/20, `VACUUM INTO` also leaks
a 4-KB partial destination file and blocks retries with "output file
already exists." Root cause is twofold and deep in Turso's trigger
implementation (`core/translate/trigger.rs:161-172`): (a) `get_table()`
resolves only btree tables, not views, and (b) even if it did, the
subsequent INSTEAD OF guard rejects the feature. Until both are
addressed, replay of a stored INSTEAD OF CREATE TRIGGER will always
fail.

## Bug 26: VACUUM returns "Corrupt database: no schema metadata" for SQLite FTS4/RTREE backing tables

**Location**: `core/vdbe/vacuum.rs::build_copy_sql` (around line 977):
```rust
let Some(btree) = source_btree_table else {
    return Err(LimboError::Corrupt(format!(
        "no schema metadata for storage-backed table \"{escaped_table_name}\""
    )));
};
```

FTS4 creates backing tables `ft_content`, `ft_segments`, `ft_segdir`,
`ft_docsize`, `ft_stat` (all rowid-based regular tables). RTREE creates
`r_rowid`, `r_node`, `r_parent`. When Turso opens such a DB, the
virtual-table module ("fts4", "rtree") is not registered, so the virtual
table's CREATE VIRTUAL TABLE fails to resolve the module and the
backing tables do not get fully wired into Turso's in-memory schema
(specifically, `get_btree_table(name)` returns None even though the
sqlite_master row has `rootpage != 0`). At VACUUM time,
`classify_schema_entries` pushes these backing tables into
`tables_to_copy` (since rootpage != 0), but the lookup via
`source_conn.with_schema(...).get_btree_table(name)` returns None, and
`build_copy_sql` bails with "Corrupt database: no schema metadata...".

This is distinct from Bug 18 (WITHOUT ROWID): FTS5 backing tables
contain `WITHOUT ROWID` and hit the parser rejection earlier. FTS4 and
RTREE backing tables are *rowid* tables and do not use WITHOUT ROWID,
so they pass the parser but fail on the schema-metadata lookup.

**Reproduction (FTS4)**:
```
$ sqlite3 /tmp/fts4.db "CREATE VIRTUAL TABLE ft USING fts4(c);
                        INSERT INTO ft VALUES ('hello');"
$ tursodb /tmp/fts4.db "VACUUM;"
Error: Corrupt database: no schema metadata for storage-backed table "ft_content"
```

**Reproduction (RTREE)**:
```
$ sqlite3 /tmp/rtree.db "CREATE VIRTUAL TABLE r USING rtree(id, minX, maxX, minY, maxY);
                         INSERT INTO r VALUES (1, 0.0, 1.0, 0.0, 1.0);"
$ tursodb /tmp/rtree.db "VACUUM;"
Error: Corrupt database: no schema metadata for storage-backed table "r_rowid"
```

Same error on `VACUUM INTO` (but dest file is not leaked because the
error happens before any writes land). The error message
("Corrupt database") is misleading — the source DB is not corrupt;
Turso just cannot handle backing tables of unknown virtual-table
modules. Tooling that treats "Corrupt database" as a signal to rebuild
will misdiagnose these databases.

**Impact**: Any SQLite-origin database that uses FTS4 or RTREE (common
for search and spatial indexes) cannot be VACUUMed on Turso. The user
sees a scary "Corrupt database" error that incorrectly suggests data
integrity issues. Combined with Bug 18 (FTS5/WITHOUT ROWID), essentially
all SQLite virtual-table extensions lock out VACUUM on Turso, even
though the rest of the database is readable. Unlike Bug 18, the error
pathway here doesn't leak the destination file during VACUUM INTO.

## Bug 27: VACUUM corrupts schema for CREATE VIEW with reserved-keyword column names (data integrity)

**Location**: `core/vdbe/vacuum.rs::vacuum_target_build_step`
`VacuumTargetBuildPhase::PreparePostData` replays `CREATE VIEW` on the
target via `state.target_conn.prepare(&entry.sql)?`. Turso's parser
re-stringifier for CREATE VIEW **strips the quotes** around column list
entries regardless of whether the identifier is a reserved keyword. The
target sqlite_master ends up with `CREATE VIEW v (order) AS ...` instead
of `CREATE VIEW v ("order") AS ...`. That stored SQL is **malformed**:
`order` is a reserved keyword and cannot be used unquoted as an
identifier. Both Turso and SQLite fail to parse the schema after the
VACUUM — the view becomes unusable in both engines, and SQLite reports
a "malformed database schema" error for the entire database.

Reserved-keyword column names WORK when wrapped in the VIEW column list
of a SQLite-created DB because SQLite preserves the quotes in its
sqlite_master.sql. Turso's parser silently re-stringifies these to
unquoted form on replay, producing invalid SQL.

**Reproduction**:
```
$ sqlite3 /tmp/x.db "
  CREATE TABLE t(a);
  INSERT INTO t VALUES (42);
  CREATE VIEW v(\"order\") AS SELECT a FROM t;"
$ sqlite3 /tmp/x.db 'SELECT * FROM v;'
42
$ tursodb --experimental-views /tmp/x.db 'SELECT * FROM v;'
42
$ tursodb --experimental-views /tmp/x.db 'VACUUM;'
$ tursodb --experimental-views /tmp/x.db 'SELECT sql FROM sqlite_master;'
CREATE TABLE t (a)
CREATE VIEW v (order) AS SELECT a FROM t         <-- QUOTES STRIPPED
$ tursodb --experimental-views /tmp/x.db 'SELECT * FROM v;'
Error: Parse error: no such table: v
$ sqlite3 /tmp/x.db 'SELECT * FROM v;'
Error: in prepare, malformed database schema (v) - near "order": syntax error (11)
```

Affects every reserved keyword we tested as a view column name:
`order`, `where`, `from`, `select`, `having`, `limit`, `union`. (For
`union` Turso's own parser happens to still resolve the view, but
SQLite consistently rejects the post-VACUUM schema with "malformed
database schema".) The bug applies identically to `VACUUM INTO`: the
destination DB is created but has the same corrupted view schema.

The bug is localized to **CREATE VIEW (column_list)** entries. CREATE
TABLE, CREATE INDEX, CREATE TRIGGER `OF` column lists, and SELECT AS
aliases all preserve quotes correctly. Turso's own CREATE VIEW
has the same quote-stripping on initial parse, so the bug exists
independent of VACUUM (see also U-family parser normalization) — but
**VACUUM amplifies it**: a SQLite-origin DB that was functional before
VACUUM becomes corrupt and cross-engine-unreadable after a VACUUM pass
on Turso.

**Impact**: Data integrity bug. Post-VACUUM, the database has
malformed schema rows that SQLite cannot parse. Users migrating from
or interoperating with SQLite can have a functional database become
unreadable by SQLite after a routine VACUUM on Turso. Even Turso fails
to query the view for most reserved keywords. There is no recovery
short of `PRAGMA writable_schema` manual edits (which Turso doesn't
support) or dropping and re-creating the view. The "corruption" is
text-level malformed schema rather than page-level damage, so tools
that rely on header/page integrity (like dbhash or file-integrity
monitors) do not flag it, yet the schema is genuinely broken.

## Bug 28: VACUUM fails on SQLite-created DB with partial index whose WHERE uses non-deterministic functions

**Location**: `core/vdbe/vacuum.rs::vacuum_target_build_step`
`VacuumTargetBuildPhase::PrepareCreateIndex` calls
`state.target_conn.prepare(&entry.sql)?`. Turso's CREATE INDEX parser
rejects any WHERE clause that references non-deterministic functions
(e.g., `datetime('now')`, `random()`, `strftime(...)`) with the error
"cannot use aggregate, window functions or reference other tables in
WHERE clause of CREATE INDEX". SQLite is more permissive: it allows the
WHERE clause to contain non-deterministic expressions (as long as the
table is empty at CREATE INDEX time — SQLite only rejects when trying
to index existing non-deterministic rows, but the CREATE itself
succeeds).

This means: a SQLite-created DB where a partial index's WHERE uses
`datetime('now')` or similar, and where the table was empty at index
creation, stores that index in sqlite_master. Turso can OPEN such a
DB fine and read/write it. But VACUUM (and VACUUM INTO) fails because
the target build replays the CREATE INDEX statement through Turso's
parser, which applies the stricter check.

**Reproduction**:
```
$ sqlite3 /tmp/x.db "CREATE TABLE t(a INTEGER);
                     CREATE INDEX ix ON t(a) WHERE datetime('now') > '2024-01-01';
                     INSERT INTO t VALUES (1);"
$ tursodb /tmp/x.db "SELECT * FROM t;"
1
$ tursodb /tmp/x.db "VACUUM;"
Error: Parse error: Error: cannot use aggregate, window functions or reference other tables in WHERE clause of CREATE INDEX:
 datetime ('now') > '2024-01-01'
$ tursodb /tmp/x.db "VACUUM INTO '/tmp/out.db';"
Error: Parse error: Error: cannot use aggregate, window functions or reference other tables in WHERE clause of CREATE INDEX:
 datetime ('now') > '2024-01-01'
$ ls /tmp/out.db*
-rw-rw-r-- 4096 /tmp/out.db       <-- leaked (Bug 14/20 family)
-rw-rw-r--    0 /tmp/out.db-wal
```

**Impact**: Any SQLite-origin database that uses partial indexes with
non-deterministic WHERE clauses (e.g., `WHERE created_at > datetime('now')`
for time-windowed indexes) can be queried by Turso but cannot be
VACUUMed. Combined with Bug 14/20, VACUUM INTO also leaks a 4-KB
partial destination file. The root cause is a parser strictness
mismatch; it compounds with the SQLite divergence in that the exact
WHERE form that SQLite-based tooling produces cannot be roundtripped
through Turso's VACUUM.

## Bug 29: In-place VACUUM panics with "must not already hold a read lock" after `SELECT * FROM pragma_*` virtual tables

**Location**: `core/storage/wal.rs:3789` — assertion in
`begin_exclusive_tx`. The in-place VACUUM preflight (`VacuumInPlacePhase::BeginSourceTx`)
calls `source_pager.begin_exclusive_tx()`, which has an internal
assertion that the connection must not already hold a read lock. The
PRAGMA virtual-table form (`SELECT * FROM pragma_<name>(...)`) opens a
read transaction internally and does not release it before the next
statement, so a subsequent `VACUUM` trips the assertion.

**Reproduction**:
```
$ tursodb /tmp/x.db "
  CREATE TABLE t(a);
  SELECT * FROM pragma_foreign_keys;
  VACUUM;
"
0
thread 'main' panicked at core/storage/wal.rs:3789:9:
begin_exclusive_tx: must not already hold a read lock
```

The panic reproduces with every PRAGMA virtual-table form tested:
`pragma_foreign_keys`, `pragma_journal_mode`, `pragma_table_info`,
`pragma_table_list`, `pragma_index_list`, `pragma_index_xinfo`,
`pragma_function_list`, `pragma_database_list`, `pragma_module_list`.
The corresponding `PRAGMA <name>` (function form) does NOT trigger the
panic — only the `SELECT * FROM pragma_<name>` virtual-table form does.

`VACUUM INTO '...'` on the same source is unaffected because that
opcode goes through the regular `BEGIN`/`COMMIT` path on the source
rather than `begin_exclusive_tx`.

Without a user table (so the DB is empty), the same panic surfaces as
Bug 3's "begin_exclusive_tx can be done on an initialized database"
InternalError instead — but with any user table present, it's a hard
panic (process abort with backtrace) rather than a clean error.

**Impact**: Any maintenance script that probes schema metadata via the
PRAGMA virtual tables before running an in-place `VACUUM` will crash
the entire process with a panic instead of receiving a normal SQL
error. This is the most severe failure mode tested: the assertion
trips inside Turso's internal state machine, leaving the connection
state effectively unrecoverable. The panic-then-process-abort path
also bypasses any cleanup, leaving any partial WAL state behind.

## Bug 30: VACUUM creates duplicate sqlite_sequence rows when source has sqlite_sequence rowid != 1

**Location**: `core/vdbe/vacuum.rs::VacuumTargetBuildPhase::CopyRows`
for the `sqlite_sequence` table (combined with the AUTOINCREMENT
counter machinery firing on every INSERT). When the source's
`sqlite_sequence` row has a non-1 rowid (e.g., user manually rebuilt
the table or the row was originally allocated at a higher rowid), the
VACUUM copy-back leaves both the auto-generated rowid=1 row AND the
source row with its original rowid in the target. SQLite's vacuum.c
preserves only one row at the new compacted rowid.

**Reproduction**:
```
$ sqlite3 /tmp/x.db "
  CREATE TABLE t(id INTEGER PRIMARY KEY AUTOINCREMENT);
  INSERT INTO t (id) VALUES (10);
  DELETE FROM sqlite_sequence;
  INSERT INTO sqlite_sequence (rowid, name, seq) VALUES (5, 't', 100);
"
$ sqlite3 /tmp/x.db "SELECT rowid, name, seq FROM sqlite_sequence;"
5|t|100

$ tursodb /tmp/x.db "VACUUM;"
$ tursodb /tmp/x.db "SELECT rowid, name, seq FROM sqlite_sequence;"
1|t|1               <-- spurious row created by AUTOINCREMENT machinery during VACUUM
5|t|100             <-- source row preserved at original rowid

$ sqlite3 /tmp/x.db "SELECT rowid, name, seq FROM sqlite_sequence;"
1|t|100             <-- SQLite: single row, rowid renumbered, seq preserved
```

The bug also reproduces with rowid=0, negative rowids, and orphan
sqlite_sequence rows for tables that don't exist or aren't AUTOINCREMENT.
For each non-1 source rowid, Turso ends up with two rows after VACUUM:
the auto-created `(1, 't', 1)` row from the AUTOINCREMENT counter and
the source's original `(rowid, name, seq)` row preserved verbatim.

**Impact**: After VACUUM, the user-visible `sqlite_sequence` table has
rows that did not exist before — both the source's row AND a new row
for any AUTOINCREMENT table whose counter the AUTOINCREMENT machinery
populated during the copy. Downstream tools that count `sqlite_sequence`
rows or assume one row per AUTOINCREMENT table will see inconsistent
state. The duplicate row also breaks the implicit 1-to-1 mapping
between AUTOINCREMENT tables and sqlite_sequence rows, breaking
tools that derive table identity from sqlite_sequence.

## Bug 31: VACUUM clobbers same-session UPDATE to sqlite_sequence with the AUTOINCREMENT cache value

**Location**: Same connection that performed an INSERT into an
AUTOINCREMENT table caches the table's AUTOINCREMENT counter
in connection state (per `core/vdbe/vacuum.rs:725` `todo: sqlite
disables AUTOINCREMENT during vacuum, but we don't have such a way
yet`). When that connection then runs `UPDATE sqlite_sequence SET
name=NULL WHERE name='t';` and follows with `VACUUM;`, the cached
counter writes back during the VACUUM copy and overwrites the user's
NULL/renamed value with the original `name='t', seq=...`.

The bug is **same-session-specific**: if the UPDATE happens in a
different connection (or different process), the value is preserved
through VACUUM. Only when an INSERT into the AUTOINCREMENT table
already populated the connection's internal cache does VACUUM trip
the writeback.

**Reproduction**:
```
$ tursodb /tmp/x.db "
  CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT);
  INSERT INTO t DEFAULT VALUES;             -- populates conn's AUTOINCREMENT cache
  UPDATE sqlite_sequence SET name=NULL WHERE name='t';
  SELECT 'before:', rowid, typeof(name), name, seq FROM sqlite_sequence;
  VACUUM;
  SELECT 'after:', rowid, typeof(name), name, seq FROM sqlite_sequence;
"
before:|1|null||1
after:|1|text|t|1                  <-- name reverted from NULL to 't'

# In a fresh connection AFTER the bug fired, the change is durable:
$ tursodb /tmp/x.db "SELECT rowid, typeof(name), name, seq FROM sqlite_sequence;"
1|text|t|1                          <-- bug-installed value persists on disk

# Compare same-session in SQLite:
$ sqlite3 /tmp/y.db "
  CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT);
  INSERT INTO t DEFAULT VALUES;
  UPDATE sqlite_sequence SET name=NULL WHERE name='t';
  VACUUM;
  SELECT 'after:', rowid, typeof(name), name, seq FROM sqlite_sequence;
"
after:|1|null||1                    <-- SQLite: NULL preserved
```

A more dramatic variant: same connection updates the *name* (not just
to NULL) — the source loses one row to the user's UPDATE and gains
one row from the AUTOINCREMENT cache, so the post-VACUUM table has
TWO rows where the source had one:
```
$ tursodb /tmp/x.db "
  CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT);
  INSERT INTO t DEFAULT VALUES;
  UPDATE sqlite_sequence SET name='renamed' WHERE name='t';
  SELECT 'before:', * FROM sqlite_sequence;
  VACUUM;
  SELECT 'after:', * FROM sqlite_sequence;
"
before:|renamed|1
after:|renamed|1                    <-- source row preserved
after:|t|1                          <-- spurious row from AUTOINCREMENT cache
```

**Impact**: Same-session VACUUM after any UPDATE on `sqlite_sequence`
silently clobbers the user's intent with whatever the AUTOINCREMENT
machinery cached. Apps that perform "INSERT into AI table; tweak the
sequence; VACUUM; close" workflows lose the tweak. Combined with
Bug 30, you can end up with both a clobbered name AND a duplicated
row. The bug surfaces as either:
1. Reverted UPDATE (NULL → 't', or other value → original name)
2. Phantom duplicate row alongside the user's edited row

## Bug 33: VACUUM with CDC enabled creates a new sqlite_sequence row tracking the internal turso_cdc table's AUTOINCREMENT counter

**Location**: Same root cause as Bug 31's AUTOINCREMENT machinery firing
during VACUUM copy, applied to Turso's *internal* `turso_cdc` table
(which is defined with `change_id INTEGER PRIMARY KEY AUTOINCREMENT`
when CDC is enabled). CDC's normal write path inserts into `turso_cdc`
without going through the connection's AUTOINCREMENT counter cache, so
pre-VACUUM `sqlite_sequence` is empty (or has no `turso_cdc` row).
VACUUM's copy loop then re-INSERTs every `turso_cdc` row through the
regular INSERT machinery, which DOES update `sqlite_sequence` —
producing a `(turso_cdc, max(change_id))` row that wasn't there before.

**Reproduction**:
```
$ tursodb /tmp/x.db "
  CREATE TABLE t (a);
  PRAGMA unstable_capture_data_changes_conn='full';
  INSERT INTO t VALUES (1);
"

$ tursodb /tmp/x.db "SELECT count(*) FROM sqlite_sequence;"
0                                   <-- pre-VACUUM: empty sqlite_sequence

$ tursodb /tmp/x.db "SELECT count(*) FROM turso_cdc;"
2                                   <-- 2 CDC rows (insert + commit marker)

$ tursodb /tmp/x.db "VACUUM;"
$ tursodb /tmp/x.db "SELECT * FROM sqlite_sequence;"
turso_cdc|2                         <-- post-VACUUM: new row appears
```

The new row is durable and visible to fresh connections. Source's
`turso_cdc` data is unchanged, but its `sqlite_sequence` table now
tracks an internal Turso table that the user did not opt into
tracking. With both a user AUTOINCREMENT table AND CDC enabled,
sqlite_sequence ends up with two entries:
```
$ tursodb /tmp/x.db "
  CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT);
  CREATE TABLE t2 (id INTEGER PRIMARY KEY AUTOINCREMENT);
  PRAGMA unstable_capture_data_changes_conn='full';
  INSERT INTO t1 DEFAULT VALUES;
  INSERT INTO t2 DEFAULT VALUES;
  SELECT 'before:', name, seq FROM sqlite_sequence ORDER BY name;
  VACUUM;
  SELECT 'after:', name, seq FROM sqlite_sequence ORDER BY name;
"
before:|t1|1
before:|t2|1
after:|t1|1
after:|t2|1
after:|turso_cdc|4                  <-- new row tracking internal table
```

**Impact**: The mere presence of CDC on the source connection causes
VACUUM to *modify user-visible source data* (sqlite_sequence) with a
row tracking an internal implementation detail (`turso_cdc`). Tools
that snapshot sqlite_sequence pre/post VACUUM see new rows tied to
implementation internals. The same row also leaks to VACUUM INTO
output, polluting the destination's sqlite_sequence with a turso_cdc
row even when CDC was an opt-in choice on the source side. Rolls
combined with Bug 6 (clobbered AI counters) and Bug 30 (duplicate
rows) into a broader pattern: VACUUM's AUTOINCREMENT-cache leak is
the root cause, and every internal Turso table that uses
AUTOINCREMENT (turso_cdc plus any future internal feature) becomes
a vector for unwanted sqlite_sequence pollution.

