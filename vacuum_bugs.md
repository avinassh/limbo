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
