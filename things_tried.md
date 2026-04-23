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




## Session 3

### 14. CHECK constraint re-enforcement during VACUUM
Used `PRAGMA ignore_check_constraints=ON` to seed a row that violates a
column CHECK constraint. Turso's VACUUM fails with the CHECK error on the
copy INSERT; SQLite's page-level xfer preserves the row. Verified on
both `VACUUM` and `VACUUM INTO`, and for `ALTER TABLE ADD COLUMN CHECK(...)`
which can introduce violations that pre-existing rows retain. Setting
`ignore_check_constraints=ON` in the source connection before VACUUM is
irrelevant because the VACUUM target connection has its own evaluation
state. Logged as Bug 11.

### 15. VACUUM INTO destination existence check
Tested `touch /tmp/out.db` (zero-byte file) then VACUUM INTO into it.
SQLite fills the zero-byte placeholder; Turso rejects it as "output file
already exists". Also tested symlinks, FIFOs, directories, chardev-style
paths (/dev/null) — all rejected by the existence check. Only dangling
symlinks currently slip through. Logged as Bug 12.

### 16. VACUUM on 2000-column table without INTEGER PRIMARY KEY
Binary-searched the boundary: 1999 columns works, 2000 fails. Root cause
is that `build_copy_sql` prepends a rowid pseudo-column so `SELECT` has
`N+1` columns, exceeding `SQLITE_MAX_COLUMN = 2000`. With an
INTEGER PRIMARY KEY alias column the rowid is reused instead of
prepended, so wide tables with an IPK are fine. Logged as Bug 13.

### 17. VACUUM INTO leaves partial destination on mid-copy failure
Verified via Bug 11's CHECK-failure path. After the error, `/tmp/out.db`
and `/tmp/out.db-wal` remain on disk (first at 4096 bytes, WAL at 0). The
next VACUUM INTO call then fails with "output file already exists",
trapping unattended maintenance scripts. The cleanup function
`cleanup_op_vacuum_into` drops the db handle but never unlinks. Logged
as Bug 14.

### 18. In-place VACUUM demotes MVCC source to WAL
Created a DB with `PRAGMA journal_mode='mvcc'`, ran a truncate checkpoint,
then `VACUUM`. Before VACUUM: fresh connection reports `mvcc`. After
VACUUM: fresh connection reports `wal`, even though `__turso_internal_mvcc_meta`
is still in sqlite_master. `VACUUM INTO` on the same source is unaffected
(destination reports `mvcc`). Logged as Bug 15.

### 19. Other things tried, no new bug
- Schema_version preservation — both sqlite/turso bump by 1 on VACUUM (OK)
- Schema_version preservation on VACUUM INTO — both bump by 1 (OK)
- Encoding stays UTF-8 (UTF-16 source rejected at open before VACUUM)
- Reserved bytes / cipher_plaintext_header_size PRAGMAs unsupported
- Journal mode preserved (wal → wal, mvcc VACUUM INTO → mvcc)
- Foreign keys disabled during VACUUM so FK violations (with PRAGMA foreign_keys=OFF)
  are preserved; FK state is per-connection so it doesn't affect the rebuild
- VACUUM inside `BEGIN` / `SAVEPOINT` correctly rejected
- VACUUM INTO to same path as source — rejected as already-exists
- Symlink source and dangling-symlink dest both succeed
- Path with `/./` or `//` or trailing spaces or leading dot — all OK
- Comments in CREATE TABLE stripped at parse time (U3 territory, not VACUUM)
- `IF NOT EXISTS` stripped from CREATE TABLE but preserved on CREATE INDEX
  (parser inconsistency, not VACUUM). sqlite strips on both.
- UNIQUE indexes with many NULLs preserved (NULL != NULL per SQL)
- `sqlite_sequence.name='non-existent'` preserved through VACUUM
- `sqlite_sequence.seq='not an int'` — Turso overwrites with 1 (extension of Bug 6)
- `sqlite_sequence` empty rows / `name=''` / `name=NULL` — all preserved
- Very long path (>200 chars) — OK
- Path with colon, shell metacharacters — OK
- VACUUM INTO URI (`file:...`) rejected with cryptic error (U4)
- Partial indexes with WHERE clause — OK
- Multi-column DESC/ASC indexes — OK
- Expression indexes with `json_extract` / `lower` / `length` — OK
- NULLS FIRST / NULLS LAST in CREATE INDEX preserved
- Trigger OF column-list preserved; column rename updates OF list
- Trigger BEFORE UPDATE OF cols — doesn't fire during VACUUM copy (OK)
- TEMP tables unaffected by VACUUM (expected)
- Triggers on sqlite_master-referencing views survive — OK
- CREATE TABLE AS SELECT (CTAS) schema text reformatted after VACUUM (U3)
- Custom types (`CREATE TYPE ... BASE ...`) — see Bug 9 from session 2
- ATTACHed DBs unaffected by VACUUM on main; VACUUM <attached> INTO works
- Large blobs (1MB single blob) preserved via overflow pages
- 100+ secondary indexes — VACUUM succeeds
- ALTER RENAME COLUMN updates references in triggers and sqlite_master
- `CHECK(...)` with CAST expressions preserved
- DEFAULT expressions (including random()) — initial values persisted, not re-evaluated
- JSONB blobs preserved with correct byte-layout
- Boolean columns preserved as integer 0/1 via typeof
- Numeric affinity (INT2, MEDIUMINT, BIGINT, TINYINT) — all preserved
- zeroblob(N) preserved
- DATETIME column — text/real/integer storage preserved per value
- Unicode (emoji) in TEXT column preserved
- Stack overflow on INSERT with a CHECK containing ~80 `AND`-combined sub-expressions
  — unrelated to VACUUM but VACUUM would hit it on any DB that contains such schema
- VACUUM INTO under readonly source — rejected with appropriate error


## Session 4

### 20. VACUUM INTO adds spurious CDC commit marker on source
Running VACUUM INTO from a connection with CDC enabled
(`PRAGMA unstable_capture_data_changes_conn='full'`, same for `'before'`,
`'after'`, `'id'`) adds a new `change_type=2` (commit marker) row to the
source's `turso_cdc` table. Reproduces even when no data was inserted since
enabling CDC — the bare `BEGIN`/`COMMIT` wrapped around the VACUUM INTO copy
trips the source's CDC commit-record emission path. In-place `VACUUM` on the
same source does **not** add a CDC record (it uses a lower-level WAL commit
that bypasses CDC). Repeated `VACUUM INTO` calls append one commit marker
each, so 2 calls in a row take the CDC count from N → N+2. Logged as Bug 16.

### 21. DROP TABLE / DROP INDEX leave stale sqlite_stat1 rows
Not a VACUUM bug but surfaced while comparing sqlite_stat1 across VACUUM.
SQLite cleans stat rows for the dropped object; Turso doesn't, and VACUUM
copies the stale rows forward. Logged in unrelated_bugs.md as U12.

### 22. Other things tried, no new bug
- VACUUM with WITHOUT ROWID table — rejected (feature unsupported), unchanged
- VACUUM with VIRTUAL TABLE USING fts5/rtree — modules not registered
- VACUUM with CREATE TABLE sqlite_foo — rejected by reserved-prefix check (OK)
- VACUUM with CREATE TABLE __turso_internal_foo — same, OK
- VACUUM on :memory: — rejected explicitly; VACUUM INTO from :memory: works
- VACUUM on readonly connection — rejected; VACUUM INTO works
- VACUUM inside BEGIN / within txn — rejected (both forms)
- VACUUM INTO pre-existing .db-wal leftover but fresh .db — succeeds
- VACUUM INTO to path with URL encoding `%20` — written verbatim, OK
- VACUUM INTO to path containing single quotes or `~` — Bug 1 / not expanded
- VACUUM INTO to path via `..` path traversal — resolved, OK
- VACUUM INTO to symlink/hard-link pointing back to source — rejected as exists
- VACUUM INTO with PRAGMA query_only=1 — rejected with clean error
- VACUUM INTO restrictive umask — honors 0077 on the output file
- Repeated VACUUM on the same DB — schema_version bumps by 1 each time
- Repeated failed VACUUM (CHECK violation) — source stays usable; no state
  leaks through to the next attempt (unlike Bug 14's VACUUM INTO path)
- VACUUM with 500-column table — OK
- VACUUM with large trigger body (100 statements), 50 triggers on one table,
  window-function views, CTE-referring views, UNION views — all preserved
- VACUUM with INSTEAD OF trigger on a view — parser rejects INSTEAD OF
- VACUUM with FK cycle (A→B, B→A) and FK off — data preserved
- VACUUM with complex CHECK (CASE / CAST / multi-column) — preserved
- VACUUM with ALTER ADD COLUMN NOT NULL DEFAULT — preserved
- VACUUM with ALTER RENAME then DROP then ADD COLUMN chain — OK
- VACUUM with sqlite_stat1 manually stuffed with non-numeric stat — preserved
- VACUUM with orphan sqlite_stat1 rows (for missing tables/indexes) — preserved
- VACUUM with duplicate sqlite_sequence rows (same name) — preserved (matches
  SQLite behaviour)
- VACUUM with AUTOINCREMENT sequence set to negative — clobbered to max(rowid)
  (extension of Bug 6)
- VACUUM with DEFAULT CURRENT_TIMESTAMP / DEFAULT random() — values not
  re-evaluated on copy (correct)
- VACUUM with indexes on generated virtual columns / json_extract / abs/upper
  expressions — all preserved
- VACUUM with BLOB primary key, TEXT PK, INTEGER PK ALIAS not first-column
- VACUUM with very large BLOB (1MB) / many large rows / 100-1000 row tables
- VACUUM with JSONB values, Inf, NaN, INT64 max/min — preserved
- VACUUM with rowid aliased by all three (`rowid`,`_rowid_`,`oid`) — preserved
- VACUUM with PRAGMA cache_size=negative — per-connection, preserved as-is
- VACUUM with PRAGMA synchronous=EXTRA — preserved
- VACUUM with PRAGMA locking_mode=EXCLUSIVE — preserved
- VACUUM with PRAGMA journal_size_limit — pragma unsupported  
- VACUUM with PRAGMA recursive_triggers — pragma unsupported (U1)
- VACUUM of DB with CDC-enabled schema — CDC tables + contents preserved
- VACUUM INTO of DB with CDC — preserves CDC records in target; source gets
  extra commit-marker row (Bug 16)
- VACUUM of MVCC DB — in-place demotes to WAL (Bug 15); VACUUM INTO keeps MVCC
- MVCC VACUUM INTO target's __turso_internal_mvcc_meta starts with
  persistent_tx_ts_max=2 (own value, not source's). Behaviour OK since target
  is a fresh file; not flagged as a bug
- MVCC in-place VACUUM preserves __turso_internal_mvcc_meta row (value=4)
- User-defined collations/functions aren't directly creatable from SQL, so the
  `mirror_symbols` path is exercised mainly through built-ins, which work
- VACUUM INTO output file permissions honour current umask correctly
- VACUUM INTO of a read-only source connection — succeeds (writes only target)
- Integrity-check after VACUUM with many rows, deletes, freelist pages — OK
- VACUUM on DB where only DROP TABLE remains (empty sqlite_sequence) — matches
  SQLite: empty sqlite_sequence disappears post-VACUUM
- Rootpage reassignment after VACUUM mirrors SQLite's behaviour (t=2,
  sqlite_stat1=3, ix=4 when source has indexes+stat1)
- UNIQUE with many NULLs — preserved (NULL!=NULL per SQL)


## Session 5

### 23. change_counter (header offset 24-27) reset to 1 by Turso VACUUM
SQLite's VACUUM bumps change_counter by 1; Turso's VACUUM writes
`change_counter=1` to the new page 1 regardless of the source's value.
Verified with a SQLite-created DB whose change_counter was `0x0b` (11);
after Turso VACUUM the bytes are `0x01`. `version_valid_for` (offset 92)
also shifts from SQLite's counter-tracking value to Turso's hardcoded
`0x002e7e58` (3047000, SQLite 3.47.0 version). Logged as Bug 17. Note
Turso doesn't increment change_counter on regular writes either, so the
general "counter frozen at 1" is a broader Turso issue — but VACUUM
*overwrites* a higher counter with 1, so the specific rewind behaviour is
VACUUM-only.

### 24. VACUUM on DB with WITHOUT ROWID tables (SQLite, FTS5/RTREE backing)
Replay-time CREATE TABLE parser rejects `WITHOUT ROWID` (Turso doesn't
support that clause). A SQLite-created DB with FTS5 or RTREE creates
backing tables with `WITHOUT ROWID` suffix in their CREATE SQL; Turso
reads these via sqlite_master fine but VACUUM fails with
"WITHOUT ROWID tables are not supported". Logged as Bug 18. Verified also
for plain user-created `WITHOUT ROWID` tables. Same failure on VACUUM INTO,
which also leaves the partial dest file (Bug 14 cleanup gap, formalized
separately as Bug 20).

### 25. VACUUM INTO leaves `.db-wal` sidecar in output dir
Turso's destination opens in WAL mode by default, producing a
`dest.db-wal` file alongside `dest.db`. SQLite's VACUUM INTO produces
only `dest.db` (rollback journal mode). Logged as Bug 19. Related:
the destination's journal_mode header bytes (offset 18-19) are always
`02 02` in Turso vs `01 01` in SQLite — logged separately as Bug 21
because the header divergence is observable even after unlinking the
sidecar.

### 26. Partial dest leak from VACUUM INTO + Bug 18 trigger
VACUUM INTO on a SQLite DB with FTS5 backing (WITHOUT ROWID) fails at
the target build's CREATE TABLE step; the destination `.db` (4096 bytes,
magic header + page-1 stub) and empty `.db-wal` remain on disk. Retry
fails with "output file already exists". This is Bug 14's cleanup gap
triggered via a different failure trigger; logged separately as Bug 20
because it demonstrates the gap is not specific to CHECK constraints.

### 27. Other things tried, no new bug
- VACUUM INTO across filesystems (tmpfs/dev/shm as dest) — works
- VACUUM INTO through dangling symlinks — writes to target
- VACUUM INTO into a path that's a regular file at depth-1 parent — rejected
- VACUUM INTO into a read-only dir — rejected with "permission denied"
- `VACUUM INTO ?` parameter binding — rejected at parse (Bug 8)
- VACUUM with `PRAGMA query_only=1` — rejected cleanly
- VACUUM with temp_store=MEMORY setting — no effect (still WAL)
- VACUUM with cache_spill OFF — unsupported pragma
- VACUUM INTO where source is in-memory (`:memory:`) — output created on disk
- VACUUM INTO with `--vfs=memory` source — still writes dest to disk
- VACUUM INTO with `--vfs=syscall` — works
- VACUUM INTO concurrent attempts on same source — one wins, others Locking error
- VACUUM INTO during concurrent readonly SELECT — works (VACUUM uses exclusive)
- VACUUM of a DB with SIGKILL mid-operation — source preserved; dest artifact
  leaks (Bug 14 family)
- VACUUM of BLOBs with embedded NULs — bit-preserving
- VACUUM of huge BLOBs (200000 bytes via randomblob) — preserved
- VACUUM of sparse rowids (INT64_MAX, 10^12 gaps) — preserved
- VACUUM of text-affinity columns with mixed types — type preservation works
- VACUUM with `PRAGMA auto_vacuum=INCREMENTAL` — rejected with InternalError
- VACUUM of DB with composite PRIMARY KEY — rowid preserved
- VACUUM of DB with REAL/BLOB PRIMARY KEY — rowid preserved
- VACUUM of DB with BLOB primary key sort — preserved
- VACUUM + DEFAULT CURRENT_TIMESTAMP — values preserved (not re-evaluated)
- VACUUM + DEFAULT subquery expression — rejected at INSERT time (not VACUUM)
- VACUUM + TEMP TABLE in same session — unaffected (per-connection temp schema)
- VACUUM + TEMP TRIGGER — survives in sqlite_temp_master
- VACUUM of MVCC DB without checkpoint — VACUUM INTO works; in-place rejected
  by preflight check
- VACUUM preserves column type text with precision (DECIMAL(10,2), etc.)
- VACUUM preserves STRICT keyword in CREATE TABLE
- VACUUM preserves bracket-quoted column names (normalized to unquoted form)
- VACUUM preserves DEFERRABLE/INITIALLY DEFERRED FK clauses
- VACUUM preserves ON UPDATE CASCADE / ON DELETE CASCADE clauses
- VACUUM preserves compound PRIMARY KEY + autoindex
- VACUUM preserves UNIQUE indexes with COLLATE per column
- VACUUM preserves NULLs in UNIQUE indexes (multiple NULLs allowed)
- VACUUM preserves JSONB blobs byte-exactly
- VACUUM preserves -0.0 (though SQLite's display may show as 0.0)
- VACUUM preserves Inf/NaN/INT64 boundaries
- VACUUM preserves Unicode table/column names (emoji, CJK)
- VACUUM preserves ALTER TABLE RENAME/DROP/ADD column chain
- VACUUM preserves triggers with multi-statement body
- VACUUM preserves DEFERRED/IMMEDIATE/EXCLUSIVE BEGIN rejection
- `VACUUM MAIN INTO '...'` works despite `VACUUM MAIN` being case-rejected
  (Bug 4 discrepancy still present)
- VACUUM of attached DB via `VACUUM att` rejected; `VACUUM att INTO` works
- `PRAGMA synchronous` per-conn values not preserved across VACUUM
- `PRAGMA locking_mode=NORMAL` rejected ("locking_mode must be EXCLUSIVE")
- VACUUM of tables with very many columns (SQLITE_MAX_COLUMN boundary) — Bug 13
- VACUUM with multiprocess-WAL source — works; dest has empty -wal but no -tshm
- Multiple VACUUMs in a row — file stays same size, schema_cookie bumps each time
- VACUUM with PRAGMA page_size=65536 — 2 pages (128KB file) preserved
- VACUUM of wide INDEX (26 columns) — preserved
- VACUUM + expression index on json_extract — preserved
- VACUUM + partial index with WHERE literal 1=1 — preserved
- VACUUM + orphan sqlite_stat1 rows for dropped tables — preserved (U12)
- VACUUM + orphan view (table dropped but view still in sqlite_master) —
  preserved (matches SQLite)
- VACUUM + orphan trigger (table dropped from trigger body) — preserved
- VACUUM + DROP COLUMN referenced in trigger OF clause — allowed; trigger keeps
  stale column reference (matches SQLite)
- VACUUM of sqlite_sequence with NULL name / NULL seq / orphan entries — preserved
- VACUUM preserves INSERT OR REPLACE ON CONFLICT clause in CREATE TABLE
- CTAS (CREATE TABLE AS SELECT) schema text reformatted after VACUUM (U3 family)
- VACUUM + PRAGMA encoding='UTF-16le' source — rejected at open (Turso UTF-8 only)

Bugs 17-21 all logged as new VACUUM bugs. Some are divergences from SQLite
rather than outright breakages, but each has a concrete way to surface as a
user-visible problem: change_counter rewinds confuse tooling (17), WITHOUT ROWID
source DBs can't be VACUUMed at all (18), `.db-wal` sidecar breaks single-file
backup assumptions (19), parse-time failure leaks dest files (20), journal_mode
header bytes silently differ from SQLite's output (21).


## Session 6

### 28. `PRAGMA auto_vacuum=MODE; VACUUM;` does not apply pending mode
Tested both enabling (source auto_vacuum=NONE, pragma=FULL, VACUUM) and
disabling (source auto_vacuum=FULL via SQLite-created DB, pragma=NONE,
VACUUM). In both directions Turso preserves the source's on-disk auto_vacuum
mode, ignoring the pending pragma override. Header byte 52
(`largest_root_btree_page`) is unchanged after VACUUM. SQLite treats this
pragma-plus-VACUUM pair as the documented way to change modes on an
existing DB. Turso's silent no-op leaves portable code broken. Logged as
Bug 22. The underlying getter `source_pager.get_auto_vacuum_mode()` used
in `target_auto_vacuum_mode` reads the pager's current mode rather than
the pending override.

### 29. VACUUM INTO emits phantom CDC commit marker that never lands on disk
Extension of Bug 16. The source connection's CDC machinery writes a
`change_type=2` commit marker when VACUUM INTO's implicit `COMMIT` fires,
but the row is never durable. `PRAGMA wal_checkpoint(FULL)` in the same
session already reveals its absence (row count goes 3→2). Reopening the
DB in a fresh Turso process, or reading via `sqlite3`, confirms only the
genuine INSERT and its commit marker are on disk. CDC consumers observing
through a separate tailer see 2 rows; the connection that ran VACUUM INTO
observes 3 until it issues any real write or a checkpoint. Subsequent
real writes in the same session make the phantom row durable
retroactively — so the bug only surfaces when VACUUM INTO is the last
write on the connection or when cache invalidation happens first. Logged
as Bug 23.

### 30. VACUUM fails after ALTER TABLE RENAME COLUMN on expression indexes and COLLATE indexes
Tested:
- `CREATE INDEX ix ON t(old_col * 2)` → after RENAME COLUMN → stored SQL stays `(old_col * 2)` (stale)
- `CREATE INDEX ix ON t(old_col COLLATE BINARY)` → stays `(old_col COLLATE BINARY)` (stale)
- `CREATE INDEX ix ON t(old_col)` → correctly updated to `(new_col)` ✓
- `CREATE INDEX ix ON t(b) WHERE old_col > 0` → correctly updated ✓
- `CHECK(old_col > 0)` in table → correctly updated ✓
- `GENERATED ALWAYS AS (old_col * 2)` → correctly updated ✓
- FK `REFERENCES p(old_col)` → correctly updated ✓
- Trigger body `NEW.old_col` → correctly updated ✓
- View `SELECT old_col FROM t` → correctly updated ✓

So ALTER TABLE RENAME COLUMN only misses two specific shapes of CREATE
INDEX — expression column list entries and column-COLLATE suffixes.
Once the stale SQL is in sqlite_master, VACUUM's target build replays
the CREATE INDEX via `prepare()` on the target, which rejects the stale
column name with "invalid expression in CREATE INDEX". VACUUM and
VACUUM INTO both fail on the same parse; VACUUM INTO additionally
leaks a 4-KB partial destination (Bug 14/20 family cleanup gap).
Logged as Bug 24. ALTER RENAME COLUMN side of the issue logged as
unrelated bug U17.

### 31. Other things tried, no new bug
- VACUUM of MVCC source with uncheckpointed writes → works (VACUUM INTO
  captures the logical state correctly)
- VACUUM + sqlite_sequence with NULL seq → overwritten to 1 (extension of
  Bug 6, already noted in session 3)
- VACUUM with source mixed-case table name → sqlite_sequence.name also
  lowercased (extension of U9)
- VACUUM with source containing `turso_cdc` table — CDC table preserved
  across VACUUM INTO; `turso_cdc_version` preserved intact
- `CREATE TEMP VIEW` persists as a permanent view in main sqlite_master
  (unrelated to VACUUM — logged as U15)
- `ALTER TABLE ADD COLUMN ... GENERATED AS (...) VIRTUAL` drops the
  VIRTUAL keyword from stored SQL; CREATE TABLE at schema-creation time
  preserves it. Not a VACUUM bug but surfaces in post-VACUUM schema
  (logged as U16)
- VACUUM with many ALTER TABLE RENAME + ADD + DROP sequence → schema
  consistent post-VACUUM
- VACUUM + INSTEAD OF triggers on views → feature not supported in Turso
- VACUUM + RECURSIVE CTE stored view → CREATE VIEW accepts, query
  rejects; VACUUM preserves the view definition unchanged
- VACUUM with source having CREATE VIRTUAL TABLE for fts5 module →
  module not registered, source can't be created
- VACUUM + FTS index method (`USING fts`) → preserved end-to-end in
  both in-place and INTO; post-VACUUM fts_match() still works on dest
- VACUUM + FTS + DELETE from base table → index correctly updated
- VACUUM with concurrent VACUUM INTO from two processes → one wins,
  other fails with "output file already exists"
- VACUUM INTO with concurrent INSERT on another connection → both
  succeed (VACUUM uses source read-only snapshot)
- VACUUM INTO via `/dev/shm` cross-mount → works
- VACUUM INTO with path containing single space `' '` → created as
  file named ' ' (relative to CWD)
- VACUUM INTO FIFO / existing char device / directory → rejected by
  existence check (Bug 12 family)
- VACUUM with AUTOINCREMENT sequence=INT64_MAX → VACUUM preserves
  the max-seq; next AUTOINCREMENT insert fails with "database is full"
  (same behavior as SQLite)
- VACUUM + negative rowids with explicit IPK → preserved
- VACUUM + compound PK with REAL + TEXT columns → preserved
- VACUUM + CREATE INDEX with NULLS LAST / CASE + COLLATE → preserved
- VACUUM + very long column name (1000 chars) → preserved
- VACUUM + reserved keyword column names ("order", "select", "table") → preserved
- VACUUM + numeric DEFAULT (hex, unary-minus, negative) → preserved
- VACUUM + CHECK constraint with julianday / CAST / IIF / IN list → preserved
- VACUUM + CTAS from view → schema reformatted at table-definition level
  (U3 family)
- VACUUM + orphan sqlite_stat1 rows for dropped indexes / tables → preserved (U12)
- VACUUM + wal_checkpoint(PASSIVE) right before VACUUM → works
- VACUUM + PRAGMA temp_store=MEMORY per-conn → per-conn, preserved
- VACUUM of DB where source has views + partial indexes + expression
  indexes + CHECK + FK simultaneously → no additional bugs observed
- VACUUM of SQLite-created DB with PRAGMA wal_autocheckpoint=0 →
  opens and VACUUMs fine on Turso
- VACUUM + incremental_vacuum mode source → correctly rejected by
  `reject_unsupported_vacuum_auto_vacuum_mode`
- VACUUM of attached DB via `VACUUM aux INTO '...'` → captures only
  aux contents (not main), as expected
- VACUUM with sqlite_sequence rows with NULL seq, NULL name, empty name,
  duplicate names → preserved through VACUUM (only integer seq gets
  clobbered per Bug 6)
- VACUUM with `CREATE INDEX ix ON t USING fts (...)` → FTS backing
  btree table preserved; searches work post-VACUUM
- Trigger execution ordering: SQLite and Turso both fire triggers in
  REVERSE creation order (highest rowid first). VACUUM preserves the
  rowid order, so firing order is stable across VACUUM
- VACUUM + WINDOW function view / DISTINCT view / UNION view →
  preserved
- VACUUM + BEFORE UPDATE OF multi-col trigger → preserved
- VACUUM + RAISE(FAIL), RAISE(ABORT) in trigger → preserved
- VACUUM + ALTER TABLE ADD COLUMN REFERENCES + FK ON DELETE CASCADE → preserved
- VACUUM + table with 20-column composite index → preserved
- VACUUM + table with 500 triggers on same event → preserved
- VACUUM + 200 table schema (many tables) → works
- VACUUM with encrypted source → in-place stays encrypted; VACUUM INTO
  produces plaintext with source's reserved_space preserved (dest has
  wasted reserved space per page) — reconfirms Bug 5 with new detail
- VACUUM + ATTACH → main VACUUM unaffected; aux VACUUM rejected; aux
  INTO works
- VACUUM of huge blob (1MB randomblob) → overflow pages preserved
- VACUUM of binary BLOB with embedded NUL / magic-bytes-like content → preserved

Bugs 22-24 all logged as new VACUUM bugs this session. Also logged
unrelated bugs U15, U16, U17.


## Session 7

### 32. VACUUM fails on SQLite-created DB with INSTEAD OF trigger on view
Turso can open and query (SELECT and INSTEAD-OF-mediated INSERT) a
SQLite DB that contains `CREATE TRIGGER ... INSTEAD OF INSERT ON v`.
VACUUM's post-data replay of the CREATE TRIGGER hits
`core/translate/trigger.rs:161-163` which requires the target table to
resolve as a btree table (not a view) — error is "no such table: v".
Even if the resolver accepted views, `core/translate/trigger.rs:172`
would reject INSTEAD OF with "INSTEAD OF triggers are not supported
yet". VACUUM INTO fails the same way and leaks a 4-KB partial dest.
Logged as Bug 25.

### 33. VACUUM gives "Corrupt database: no schema metadata" on FTS4/RTREE backing tables
FTS4 creates backing tables `ft_content`, `ft_segments`, `ft_segdir`,
`ft_docsize`, `ft_stat` (rowid-based, not WITHOUT ROWID). RTREE
similarly creates `r_rowid`, `r_node`, `r_parent`. When Turso opens
such a DB, the virtual-table module isn't loaded; the backing tables
have rootpage != 0 in sqlite_master but `get_btree_table(name)`
returns None in Turso's schema. VACUUM's `build_copy_sql` bails with
"Corrupt database: no schema metadata for storage-backed table
<name>". The error message incorrectly suggests data corruption;
the source DB is otherwise healthy. Distinct from Bug 18 (FTS5
WITHOUT ROWID parser rejection). Logged as Bug 26.

### 34. VACUUM corrupts schema for CREATE VIEW with reserved-keyword column names
When source has `CREATE VIEW v("order") AS ...`, Turso's VIEW parser
strips quotes from the column list when re-stringifying. Post-VACUUM
sqlite_master contains `CREATE VIEW v (order) AS ...` — malformed
because `order` is a reserved keyword. Both Turso and SQLite fail to
parse the schema; SQLite reports "malformed database schema (v) - near
'order': syntax error". Affects every reserved keyword tested
(`order`, `where`, `select`, `from`, `having`, `limit`, `union`).
Scope: CREATE VIEW column_list only — CREATE TABLE, INDEX, TRIGGER OF
lists all preserve quotes. Data integrity bug. Logged as Bug 27.

### 35. VACUUM fails on SQLite-created DB with partial index using non-deterministic WHERE functions
SQLite allows `CREATE INDEX ix ON t(a) WHERE datetime('now') > '...'`
at CREATE time on an empty table, storing the index in sqlite_master.
Turso's CREATE INDEX parser is stricter — it rejects WHERE clauses that
reference non-deterministic functions with "cannot use aggregate,
window functions or reference other tables in WHERE clause of CREATE
INDEX". So Turso can OPEN such a DB but cannot VACUUM/VACUUM INTO it.
VACUUM INTO also leaks a 4-KB partial dest (Bug 14/20 family). Logged
as Bug 28.

### 36. Other things tried, no new bug
- Various STRICT table shapes with ANY, REAL, BLOB, INT — preserved
- Custom type chains (CREATE TYPE t2 BASE t1) — rejected, base type
  must be built-in
- VACUUM INTO via symlinked paths — WAL sidecar lands next to symlink,
  not resolved target (SQLite behaves the same — not new)
- VACUUM after many ALTER ADD COLUMN / RENAME / DROP chains — OK
- VACUUM + triggers with UPDATE/INSERT/DELETE bodies of various shapes
  — all preserved. Including UPSERT (INSERT ON CONFLICT DO UPDATE) in
  trigger body, WITH/CTE in trigger body (some variants rejected by
  both Turso and SQLite at parse time), NEW/OLD compound references
- VACUUM + DEFAULT expressions with sqlite_version(), last_insert_rowid(),
  strftime, julianday, abs, MIN/MAX, hex(zeroblob(N)), printf — preserved
- VACUUM + CHECK constraints using IIF, CAST, CASE-WHEN, json_valid,
  compound AND/OR, CURRENT_TIME, IS NULL — preserved
- VACUUM + indexes with expression indexes (abs, lower, json_extract,
  iif, CASE WHEN), partial indexes with IS NULL / BETWEEN / GLOB /
  typeof() / LIKE '%[abc]%' — preserved
- VACUUM + view with recursive CTE (CREATE accepts, query fails) —
  schema preserved intact
- VACUUM + CDC 'id'/'before'/'after'/'full' modes with INSERT INTO
  reveals phantom commit marker (Bug 16/23 family, already logged)
- VACUUM + PRAGMA page_size = 512 / 65536 — preserved
- VACUUM + PRAGMA default_cache_size — Turso's default is -2000 (2 MB
  in KB units); SQLite defaults to 0. Not a divergence from VACUUM's
  perspective since from_source_header copies source value
- VACUUM + PRAGMA user_version = INT32_MAX / -1 — preserved
- VACUUM + MVCC + views / triggers / custom types — each preserved
  individually; MVCC demotion to WAL persists (Bug 15)
- VACUUM + reserved column keywords via `"UNION"`, `"HAVING"`, etc as
  TABLE / INDEX / TRIGGER OF elements — all preserved with quotes
- VACUUM + column with weird escape shapes (`"a""b"`, `"c'd"`) — OK
- VACUUM + encrypted source + MVCC + custom types combo — VACUUM INTO
  produces plaintext with custom type meta visible (Bug 5 + Bug 9 combo)
- VACUUM + source that has NUMERIC/DECIMAL precision — preserved
- VACUUM + source with compound PK, BLOB PK, REAL PK — preserved
- VACUUM + source where stored SQL has multi-line/newline DEFAULT —
  preserved
- Stack overflow on VACUUM when source has CHECK constraint with 100+
  AND clauses (already-documented U8 extended: VACUUM itself aborts
  on schema parse even with 0 data rows)
- VACUUM of DB that uses CREATE TABLE `IF NOT EXISTS` — SQLite strips
  IF NOT EXISTS from stored SQL (same as Turso), so no divergence
- VACUUM when sqlite_master has no storage rows but sqlite_sequence
  exists (dropped all AUTOINCREMENT tables) — VACUUM removes
  sqlite_sequence post-VACUUM (matches SQLite)
- VACUUM of DB with empty MVCC (no user tables, only
  __turso_internal_mvcc_meta) — succeeds
- VACUUM INTO always produces `.db-log` sidecar for MVCC sources
  (Bug 19 extension: BOTH `.db-wal` AND `.db-log` as zero-byte files)
- VACUUM preservation of sqlite_sequence with NULL/BLOB/REAL/orphan
  entries — preserved through VACUUM
- Turso's UPDATE on turso_cdc_version panics with "cdc_rowid_before_reg
  must be set" (unrelated bug not tied to VACUUM, logged as U18)
- VACUUM preservation of sqlite_master rowid ordering: both SQLite and
  Turso reorder post-VACUUM to tables-first-then-indexes (matches)

Bugs 25-28 all logged as new VACUUM bugs this session (4 concrete
Turso-on-SQLite divergences that block or corrupt VACUUM on otherwise
valid databases). Unrelated bug U18 (panic on UPDATE
turso_cdc_version) also added.


## Session 8

### 36. In-place VACUUM panics when prior statement used PRAGMA virtual table
`SELECT * FROM pragma_<name>(...)` opens an internal read transaction
that does NOT release before the next statement. A subsequent
`VACUUM;` calls `source_pager.begin_exclusive_tx()`, which has an
internal assertion forbidding a held read lock — process panics with
"begin_exclusive_tx: must not already hold a read lock" at
`core/storage/wal.rs:3789`. Reproduces with at least:
`pragma_foreign_keys`, `pragma_journal_mode`, `pragma_table_info`,
`pragma_table_list`, `pragma_index_list`, `pragma_index_xinfo`,
`pragma_function_list`, `pragma_database_list`, `pragma_module_list`.
The function-form `PRAGMA <name>` (without SELECT FROM) does NOT
trigger the panic. `VACUUM INTO '...'` is unaffected (uses regular
BEGIN path). Logged as Bug 29.

### 37. VACUUM creates duplicate sqlite_sequence rows when source has rowid != 1
Source: `INSERT INTO sqlite_sequence (rowid, name, seq) VALUES (5, 't', 100)`
plus an AUTOINCREMENT table `t`. After Turso VACUUM, sqlite_sequence
has TWO rows: the source's row preserved at rowid=5 plus a fresh
`(1, 't', 1)` row from the AUTOINCREMENT machinery firing during the
copy. SQLite preserves a single row at the new compacted rowid.
Reproduces with rowid=0, negative rowids, and orphan rows for
non-existent tables. Logged as Bug 30.

### 38. VACUUM clobbers same-session UPDATE to sqlite_sequence
Same connection that did INSERT into AUTOINCREMENT table caches the
counter. UPDATE sqlite_sequence to NULL/different name in the same
session, then VACUUM — Turso's AUTOINCREMENT cache writes back during
VACUUM copy, overwriting the user's update. Bug doesn't reproduce
across separate connections (where the cache is empty). Two failure
modes:
1. UPDATE name=NULL → VACUUM restores name='t' (NULL → text)
2. UPDATE name='renamed' → VACUUM creates duplicate row, both
   'renamed' AND 't' present
Logged as Bug 31.

### 39. VACUUM with CDC adds turso_cdc row to sqlite_sequence
The internal `turso_cdc` table is defined with `INTEGER PRIMARY KEY
AUTOINCREMENT`. CDC's normal write path doesn't fire the AUTOINCREMENT
counter, so pre-VACUUM sqlite_sequence is empty. VACUUM's copy loop
re-inserts every turso_cdc row through the regular INSERT machinery,
which DOES update sqlite_sequence — producing a `turso_cdc|N` row that
wasn't there before. Affects any source using CDC, including the
copy-to-destination via VACUUM INTO. Logged as Bug 33. Same root
cause as Bug 31 / Bug 6 (AUTOINCREMENT-cache leak during VACUUM
copy).

### 40. Other things tried, no new bug
- VACUUM with very many indexes (50+ on same table) — preserved
- VACUUM with INSERT OR REPLACE on PK conflict — preserved
- VACUUM with UPSERT (INSERT ON CONFLICT DO UPDATE) — preserved
- VACUUM with RETURNING clause — works
- VACUUM with sqlite_sequence having NULL/BLOB/REAL/text seq — preserved
  (only NULL→max(rowid) clobbering per Bug 6 family)
- VACUUM with sqlite_sequence having BLOB/Unicode/multi-line name — preserved
- VACUUM with multi-byte UTF-8 emoji/Japanese/Hebrew — preserved
- VACUUM with very long path (200+ chars) for VACUUM INTO — works
- VACUUM with Unicode path (üñîçødé.db) — works
- VACUUM with cross-mount destination (tmpfs/dev/shm) — works
- VACUUM INTO with relative path / absolute path / Windows-style — works
- VACUUM INTO with empty literal path — Turso rejects with clean error;
  SQLite silently accepts (creates a temp file)
- VACUUM after PRAGMA cipher in same session — preserves encryption
- VACUUM INTO on encrypted source — produces plaintext (Bug 5 reconfirmed)
- VACUUM after multiple connections do schema changes — final state preserved
- VACUUM with CHECK using printf/IIF/CASE/CAST/julianday/datetime/
  json_valid/json_extract — preserved
- VACUUM with index using lower/upper/abs/hex/length/cast — preserved
- VACUUM with TRIGGER body containing semicolons in string literals — preserved
- VACUUM with TRIGGER body containing reserved-keyword column refs — preserved
- VACUUM with TRIGGER WHEN clause referencing OLD/NEW.rowid — preserved
- VACUUM with TRIGGER firing during INSERT, BEFORE/AFTER, RAISE — preserved;
  none of these triggers fire during VACUUM copy phase (correct)
- VACUUM with CHECK constraints on STRICT tables — preserved
- VACUUM with FK self-referencing chain (10 levels deep) — preserved
- VACUUM with FK ON UPDATE/DELETE NO ACTION clauses — preserved
- VACUUM with sqlite_master entries having weird names (rowid, oid,
  sqlite_master, sqlite_sequence as user data) — preserved
- VACUUM with PRAGMA short_column_names / busy_timeout / max_page_count
  / locking_mode / synchronous / cache_spill / temp_store — per-conn,
  preserved
- VACUUM with EXPLAIN VACUUM — does NOT execute the actual VACUUM (good)
- VACUUM with EXPLAIN VACUUM INTO 'x.db' — does NOT create the file
- VACUUM with INSERT INTO with json_each as source — works
- VACUUM with Indexes ASC/DESC mixed, NULLS FIRST/LAST (turso rejects
  the latter at parse time) — works for ASC/DESC
- VACUUM with PRAGMA journal_mode=DELETE source from sqlite — turso
  silently converts to WAL on open (general turso behavior, not VACUUM-specific)
- VACUUM with sqlite-created DB with CHARACTER(20) / NUMERIC(10,2)
  / DECIMAL(8) / VARCHAR / etc. — preserved (with parser whitespace
  reformatting per U-family)
- VACUUM after pragma_function_list (function form, not VT) — works
- VACUUM with sqlite_sequence having seq value > max(rowid) — preserved
- VACUUM with sqlite_sequence having multiple rows for same name
  (SQL allows duplicates) — both preserved
- VACUUM with `[t]` SQLite-bracket-quoted identifiers — Turso strips
  the brackets and re-stringifies as `t (a INTEGER, b TEXT)` (U-family
  parser normalization, amplified by VACUUM)
- VACUUM with backtick-quoted identifiers — preserved through VACUUM
- VACUUM with multi-line schema text containing -- and /* */ comments —
  comments stripped, formatting flattened (U3 family)
- VACUUM with PRAGMA cache_size=N (positive page count) — Turso has a
  minimum of 200 pages, silently caps lower values (not VACUUM-specific)
- VACUUM with INSERT INTO trigger that has RAISE(ROLLBACK/ABORT/IGNORE)
  conditions — VACUUM doesn't fire these triggers, source rows preserved
- VACUUM with sqlite_sequence containing a row with rowid=0 — Bug 30 family
- VACUUM with INSERT OR IGNORE / OR REPLACE / OR ABORT — preserved
- VACUUM with UNIQUE composite indexes + multiple NULLs — preserved
- VACUUM with FK self-FK + ON DELETE CASCADE — preserved
- VACUUM with INTEGER PRIMARY KEY DESC — preserved (autoindex retained)
- VACUUM with column name "rowid" / "ROWID" / "PRIMARY" (reserved word) — preserved
- VACUUM with column having tab/newline/special chars in name — preserved
- VACUUM with TEXT containing only NUL bytes — preserved
- VACUUM with BLOB containing magic-bytes-like content — preserved
- VACUUM with very large blob (1MB) in single row — preserved (overflow OK)
- VACUUM with CTAS-derived view containing window functions — preserved
- VACUUM with view referencing dropped table — preserved (SQLite-compatible)
- VACUUM with CDC mode 'before'/'after'/'id'/'full' — Bug 33 reproduces in all modes
- VACUUM with sqlite_sequence row where seq is JSON-shaped TEXT — preserved
- VACUUM in `:memory:` rejected; `VACUUM INTO ':memory:'` writes a
  literal file named `:memory:` (existing behavior)
- VACUUM with various PRAGMA function forms (table_info, index_list,
  database_list, function_list) followed by VACUUM — works

Bugs 29-31 and Bug 33 logged as new VACUUM bugs this session
(panic + 3 sqlite_sequence-related divergences, all surfacing as
post-VACUUM data shape differences from SQLite). All 4 are
distinct from existing Bug 6/9/14/15/20/24-28 family, sharing the
common root cause that VACUUM's INSERT path through the regular
target connection fires AUTOINCREMENT/cache machinery that the
source-side bypass never does.

