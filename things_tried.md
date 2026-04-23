# VACUUM Bug Hunting Log

## Approach
Investigating VACUUM implementation in tursodb, looking for bugs, particularly in:
- VACUUM interactions with transactions, triggers, mvcc, integrity check, check constraints, strict tables, json, checksums, encryption
- Edge cases in VACUUM INTO
- Edge cases in in-place VACUUM

## Log

### 1. Path parsing test — VACUUM INTO
Created `ptest.db` with a simple table. Tried:
- `VACUUM INTO '/tmp/file.db'` → works
- `VACUUM INTO "path.db"` → also worked (probably because trim_matches also strips `"`)
- `VACUUM INTO '/tmp/foo''.db'` → CREATED THE LITERAL FILE `foo''.db` (bug — sqlite creates `foo'.db`)

Root cause: `extract_path_from_expr` in `core/translate/vacuum.rs` uses
`s.trim_matches('\'').trim_matches('"')` which does not unescape doubled quotes.
Logged as Bug 1.

### 2. Basic VACUUM with triggers, views, constraints
- Regular triggers preserved, don't fire during copy phase — OK
- CHECK constraints preserved — OK
- FK disabled during VACUUM — OK
- Indexes preserved and usable after VACUUM — OK
- UNIQUE indexes preserved — OK
- Partial / expression / descending / collation indexes — all OK
- Generated virtual columns — OK
- STRICT tables — OK
- sqlite_stat1 preserved through VACUUM — OK
- ALTER TABLE RENAME / ADD / DROP COLUMN + VACUUM — OK
- Custom types (BASE TEXT) — OK on in-place VACUUM
- Unicode table/column names — OK
- Reserved keyword table/column names — OK
- WAL content (unchecked data) captured — OK
- AUTOINCREMENT sqlite_sequence correctly preserved — OK
- TEMP schema VACUUM INTO is a no-op — OK
- user_version/application_id preserved — OK
- page_size preserved — OK

### 3. Materialized views VACUUM
`CREATE MATERIALIZED VIEW mv AS …` + VACUUM (or VACUUM INTO) fails with
"table __turso_internal_dbsp_state_v1_mv already exists". Root cause: the
DBSP backing table appears as a storage-backed table in sqlite_schema AND is
re-created implicitly when the `CREATE MATERIALIZED VIEW` is replayed in
the post-data phase. Logged as Bug 2.

### 4. Empty-database in-place VACUUM
`VACUUM` as the first statement on a fresh (never-initialized) database returns
`Error: Internal error: begin_exclusive_tx can be done on an initialized
database (page 1 must already be allocated)`. SQLite handles this as a no-op.
Logged as Bug 3.

### 5. Case-sensitive schema name comparison
`VACUUM MAIN;` fails with a misleading "schema 'MAIN' is not supported yet"
message because `translate_vacuum` compares the raw schema name to
`"main"` directly. Tested `MAIN`, `Main`, `main` (works), `'main'` (works).
Logged as Bug 4.

### 6. VACUUM INTO on encrypted source
Opening an encrypted source via `PRAGMA cipher` + `PRAGMA hexkey` and then
running `VACUUM INTO 'out.db'` produces an UNENCRYPTED output — the dest
starts with `SQLite format 3`, strings/grep reveal sensitive data. Source
remains encrypted (`Turso` magic). In-place `VACUUM` on the same source
does encrypt the temp DB correctly (via `open_vacuum_temp_db`). Logged as
Bug 5 — serious confidentiality violation.

### 7. Other things tried, no bug found
- Large data (>64 pages, cross batch boundary) — OK
- Many tables (50+) — OK
- Many schema changes — OK
- Multi-VACUUM in a row — OK
- Stress test (2000 rows, delete half, VACUUM) — compacts correctly
- Big blobs (overflow pages) — preserved
- Negative rowids / INT64 boundaries — preserved
- Path with spaces, long paths — OK
- Shadowed rowid alias names (rowid,_rowid_,oid as columns) — OK
- Unique keys with collation, DESC indexes — OK
- Unicode / reserved keyword / quoted table & column names — OK
- Trigger+view ordering, even when trigger created before referenced view — OK
- Multiple AUTOINCREMENT tables — sqlite_sequence preserved
- MVCC in-place VACUUM after checkpoint — OK
- MVCC VACUUM INTO — output is MVCC-flagged with MVCC meta table re-created;
  usable and queryable
- CDC (unstable_capture_data_changes_conn) — existing CDC table preserved
- Unencrypted VACUUM INTO — produces plaintext DB (as expected)
- In-place VACUUM of encrypted DB — temp and output remain encrypted — OK
- VACUUM preserved column order, defaults, check constraints, FK definitions
- VACUUM after REINDEX — OK
- VACUUM MAIN INTO '...' works (uses `get_database_id_by_name` which
  normalizes) — discrepancy noted in Bug 4
- VACUUM INTO concurrent path — rejects dest-already-exists via Path::exists
  (dangling symlinks still succeed, but that's normal filesystem behavior)
- VACUUM INTO self-path — rejected as "output file already exists"
- Source connection still usable after VACUUM INTO failure
- Source connection still usable after dest-already-exists VACUUM INTO failure
- auto_vacuum FULL mode — not actually being set by PRAGMA (unrelated to
  VACUUM bugs; did not dig further)
- PRAGMA cipher/hexkey before CREATE TABLE work for in-place — OK
- Multiple triggers on the same table preserve order — OK
- INTEGER PRIMARY KEY DESC — OK
- Shadowed columns don't break the rowid alias logic in build_copy_sql — OK

