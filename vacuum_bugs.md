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
