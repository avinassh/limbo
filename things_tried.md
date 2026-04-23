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

## Session 2

### 8. AUTOINCREMENT `sqlite_sequence.seq` clobbering
Set up an AUTOINCREMENT table, inserted id=100 (seq=100), then manually
`UPDATE sqlite_sequence SET seq = 50`. After VACUUM the target shows seq=100,
not 50. Reproduced on both plain `VACUUM` and `VACUUM INTO`. Root cause
traced to copy order: sqlite_sequence row (rowid=1) is copied first, then the
INSERTs for the base table fire target-side AUTOINCREMENT tracking which
overwrites seq back to max(rowid). The code's own `todo: sqlite disables
AUTOINCREMENT during vacuum, but we don't have such a way yet` at
`core/vdbe/vacuum.rs:725` acknowledges the mechanism gap. Logged as Bug 6.

### 9. `PRAGMA page_size` + `VACUUM` does not resize
`PRAGMA page_size=8192; VACUUM;` on a page_size=4096 DB produces no change —
header byte 16 stays at 0x1000 (4096). Equivalent SQLite invocation changes
the header to 0x2000 (8192). `VACUUM INTO` has the same bug: the destination
also uses the source's current page_size. Both code paths read page_size
from the source pager rather than honouring the pending pragma override.
Logged as Bug 7.

### 10. `VACUUM INTO` only accepts string literals / identifiers
`VACUUM INTO ?`, `VACUUM INTO 'a'||'b'`, `VACUUM INTO :name` all fail at
parse time with "VACUUM INTO requires a string literal path". SQLite accepts
all three (verified via Python bindings for `?`). Root cause: Turso's
`extract_path_from_expr` in `core/translate/vacuum.rs:67-85` explicitly
matches only `Expr::Literal(Literal::String)` and `Expr::Id`. Logged as Bug 8.

### 11. `__turso_internal_types` gains an autoindex row via VACUUM
Created a custom type with `CREATE TYPE pos_int BASE INTEGER; CREATE TABLE
t(a pos_int);` and then ran VACUUM. Source sqlite_master had 2 rows
(the types table and `t`); target has 3 — a fresh
`sqlite_autoindex___turso_internal_types_1` appeared. Re-VACUUMing keeps
the extra row; there's no way back to the original shape. This affects any
database that has ever used `CREATE TYPE`. Logged as Bug 9.

### 12. Rowid preservation vs SQLite renumbering
Created a plain rowid table (no INTEGER PRIMARY KEY), inserted 4 rows,
deleted 2, ran VACUUM. Turso keeps rowids as (1, 4); SQLite renumbers to
(1, 2). Caused by `build_copy_sql` always prepending a rowid pseudo-column
to the copy when `has_rowid`. Documented SQLite compat divergence.
Logged as Bug 10.

### 13. Other things tried, no new bug
- `VACUUM INTO` to a path that's an existing dir → correctly rejected
- `VACUUM INTO` to `/dev/null`, FIFO, socket → rejected as "already exists"
- Dangling-symlink destination → accepted (writes through to the link target)
- Relative path `'../foo.db'` → fails with a statfs error (cryptic, noted in U4)
- Path containing a newline — preserved exactly in the filename (correct)
- Path with trailing spaces — preserved exactly (no trim, correct)
- `VACUUM INTO '/tmp/existing_dir'` → rejected as already exists (correct)
- Autoindex preservation for user tables with `TEXT PRIMARY KEY` — OK
- View→view references, UNION view, DISTINCT view, window-function view — OK
- `INSTEAD OF` trigger on view — feature not supported ("no such table: v")
- CHECK constraint with CASE/WHEN/function — preserved, re-enforces post-VACUUM
- FK `ON DELETE SET DEFAULT` — preserved
- Self-referential FK with `ON DELETE CASCADE` — caused a stack overflow
  crash unrelated to VACUUM (noted in unrelated_bugs.md as U5)
- `REAL` with `Inf`/`-Inf`/`NaN` values — preserved
- `REAL` integer-valued `1.0` — preserves `typeof='real'` correctly
- AUTOINCREMENT with seq > max(id) (higher than any used id) — preserved
  correctly in both SQLite and Turso (only the lower-than-max case is broken)
- Multiple AUTOINCREMENT tables with custom seqs — all get clobbered (same bug)
- AUTOINCREMENT with no inserts → sqlite_sequence empty before and after
- `DROP TABLE` of the only AUTOINCREMENT user → sqlite_sequence disappears
  (matches SQLite)
- Tables with all three rowid aliases (`rowid`,`_rowid_`,`oid`) shadowed — OK
- Triggers with `FOR EACH ROW` / `OF col` / WHEN subqueries — preserved
- `INSERT OR REPLACE` on sqlite_sequence doesn't fire unique index because
  sqlite_sequence has none; the reference to OR REPLACE is semantically just
  to use rowid as the conflict target
- INT64 min/max IPK values — preserved
- Large blobs (overflow pages) — preserved
- `PRAGMA locking_mode=EXCLUSIVE` — preserved (per-connection state, not DB)
- `user_version`, `application_id`, schema_cookie bumped — correct
- cache_size pragma across VACUUM — preserved (per-connection state)
- JSON/JSONB values — preserved with correct types
- Bracketed column names `[a]` — accepted, normalized to unquoted form
- Quoted table & column names with embedded quotes (`"a""b"`) — OK
- Expression indexes using built-in functions (`lower`, `length`) — OK
- Partial index with `WHERE id IS NOT NULL` — preserved
- DESC index — preserved
- Multiple ALTER (ADD/RENAME/DROP) then VACUUM — schema remains consistent
- ANALYZE then VACUUM — sqlite_stat1 preserved
- ATTACH: `VACUUM at INTO '...'` works (case-insensitive match happens by
  virtue of schema lookup) even though plain `VACUUM AT` rejects the name
- TEMP table is ignored by VACUUM (expected)
- VACUUM on an explicitly-readonly connection is rejected; VACUUM INTO works
- VACUUM INTO `:memory:` creates a regular file named `:memory:` on disk
  (SQLite silently discards the output); minor UX divergence, not obviously
  broken
- VACUUM INTO with `file:` URI path fails with "statfs shared WAL coordination
  path" error (same U4 family)
- Trigger inside trigger (`VACUUM` invoked from BEGIN..END) rejected by parser
- CDC (`PRAGMA unstable_capture_data_changes_conn='full'`) — VACUUM doesn't
  corrupt the CDC table; existing records preserved; post-VACUUM inserts
  produce new CDC rows as expected
- MVCC journal_mode VACUUM (in-place and INTO) — preserves MVCC meta table
  in the output image where expected
- Encrypted DB opened via `PRAGMA cipher`/`hexkey` then re-opened in a fresh
  CLI invocation — fails to read the header (eager read before pragma). This
  is unrelated to VACUUM; affects any second invocation of the CLI
- `VACUUM INTO` path with `/tmp/abc''def'''` — parser's
  `s.trim_matches('\'')` strips ALL trailing quotes rather than one pair,
  producing an unexpected filename. Extension of Bug 1


