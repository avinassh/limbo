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

