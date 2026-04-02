# Things Tried - ATTACH Bug Investigation

## Initial Setup
- Explored ATTACH implementation code across translate/attach.rs, vdbe/execute.rs, connection.rs, emitter/mod.rs
- Built tursodb CLI

## Test Plan
1. Basic ATTACH/DETACH functionality
2. Schema operations on attached DBs (CREATE TABLE, DROP TABLE, ALTER TABLE)
3. Writes to attached DBs (INSERT, UPDATE, DELETE)
4. Transactions across attached DBs
5. Triggers on attached DBs (including cross-db references)
6. REPLACE INTO on attached DBs
7. ON CONFLICT handling on attached DBs
8. Views on attached DBs
9. Integrity check on attached DBs
10. JSON functions with attached DBs
11. CHECK constraints on attached DBs
12. STRICT tables on attached DBs
13. Foreign keys on attached DBs
14. Schema reads (sqlite_master) on attached DBs
15. Detach during active operations

## Tests Run

### Basic Functionality (all passed)
- ATTACH and read from attached DB
- Cross-database JOIN
- Write (INSERT) to attached DB
- PRAGMA table_info and integrity_check on attached DB
- sqlite_master / sqlite_schema on attached DB
- UPDATE and DELETE on attached DB
- INSERT INTO ... SELECT across databases
- ATTACH with expression path (string concat)
- PRAGMA database_list
- ATTACH empty string (in-memory DB)
- DETACH non-existent schema (proper error)
- ATTACH with duplicate alias (proper error)
- ATTACH with reserved name 'main' (proper error)
- Multiple attached DBs with cross-joins (3-way join)
- Aggregate functions on attached DB (COUNT, SUM, AVG, MIN, MAX, GROUP_CONCAT)
- typeof() on attached DB
- LIKE/GLOB on attached DB
- COALESCE/IFNULL/NULLIF on attached DB
- Window functions (ROW_NUMBER) on attached DB
- CASE expression on attached DB
- CTE (WITH clause) on attached DB
- Correlated subquery cross-database
- UNION ALL cross-database
- ORDER BY + LIMIT on cross-db UNION
- INSERT with DEFAULT VALUES on attached DB
- Multi-column UPDATE on attached DB
- ALTER TABLE ADD COLUMN on attached DB
- CREATE INDEX on attached DB
- DROP INDEX on attached DB
- DROP TABLE on attached DB
- CREATE TABLE with CHECK constraint on attached DB (enforced correctly)
- STRICT table on attached DB
- Foreign key enforcement on attached DB
- Trigger on attached DB (same-schema)
- AUTOINCREMENT on attached DB
- EXPLAIN on attached DB query
- INSERT OR IGNORE on attached DB with UNIQUE (worked)
- INSERT OR REPLACE on attached DB WITHOUT unique constraint (worked)
- UPSERT (ON CONFLICT DO UPDATE) on attached DB
- RETURNING clause on INSERT/UPDATE/DELETE on attached DB
- Dual-DB write in single transaction (worked)
- SAVEPOINT release on attached DB (worked)
- COUNT(*) on empty attached table
- ATTACH same file twice with different names (worked)

### Bugs Found
1. CREATE INDEX stores schema prefix in sqlite_master SQL
2. CREATE VIEW on attached DB fails to resolve unqualified table names
3. DETACH succeeds during active transaction (should fail)
4. INSERT OR REPLACE on attached DB with UNIQUE constraint causes PANIC
5. ROLLBACK TO SAVEPOINT does not undo writes on attached databases

### Round 2 Tests (finding bugs 6-10)

#### Working correctly:
- FK CASCADE (ON DELETE CASCADE) on attached DB
- Trigger firing on attached DB (AFTER INSERT trigger)
- Schema name resolution with same table name in main and attached
- ALTER TABLE RENAME on attached DB
- INSERT INTO attached SELECT from main (cross-DB INSERT...SELECT)
- CREATE TABLE IF NOT EXISTS on existing attached table
- Multiple ATTACH (3 DBs) and PRAGMA database_list
- sqlite_schema query on attached DB
- ALTER TABLE RENAME COLUMN on attached DB
- PRAGMA aux.table_info - works correctly
- PRAGMA aux.table_xinfo - works correctly
- PRAGMA aux.page_count / page_size / freelist_count - correct
- UPDATE with subquery across databases
- EXPLAIN QUERY PLAN on cross-DB join
- WAL compatibility (turso writes, sqlite3 reads back)
- CHECK constraint enforcement on attached DB
- STRICT table type enforcement on attached DB
- ATTACH with expression (concatenation) as filename
- ATTACH :memory: (special string)
- ATTACH '' (empty string, in-memory)
- Re-attach same file after DETACH
- ROLLBACK on multi-DB transaction (correctly rolls back both)
- BEGIN IMMEDIATE with attached DB writes
- PRAGMA aux.journal_mode - correct
- CREATE TABLE on attached DB, verify persistence
- Multiple DDL operations on attached DB in one session
- DROP TABLE on attached DB
- INSERT OR IGNORE with multiple unique constraints
- UPSERT (INSERT ... ON CONFLICT DO UPDATE) on attached DB
- JSON functions on attached DB
- CREATE INDEX then query on attached DB
- ATTACH non-existent file (creates new DB)
- Schema-qualified name in expression context (subquery)

#### Bugs Found (Round 2):
6. PRAGMA aux.integrity_check checks main instead of attached
7. PRAGMA aux.index_list / index_info / index_xinfo return empty on attached DBs
8. PRAGMA aux.table_list shows main DB tables with hardcoded "main" schema name
9. No limit on number of attached databases (sqlite3 enforces max 10)
10. ALTER TABLE ADD COLUMN type validation on attached DB reads wrong pager (I/O error)

### Not Tested (features not yet supported)
- VACUUM on attached DB ("not supported yet" error)
- REINDEX ("not supported yet" error)
- CREATE TABLE AS SELECT ("not supported" error)
- TEMP tables (not supported yet)
- WITHOUT ROWID INSERT (not supported yet)
- Stored generated columns (not supported)
- compile_options pragma (not a valid pragma name in turso)
- Recursive CTEs (not supported yet)
- UPDATE ... FROM clause (not supported)

### Round 3 Tests (finding bugs 11-15)

#### Working correctly:
- Triggers on attached DB fire correctly (AFTER INSERT, AFTER UPDATE, AFTER DELETE)
- BEFORE trigger with RAISE(ABORT) on attached DB
- Complex trigger (AFTER DELETE updates another table in same attached DB)
- Cross-database JOIN with different table names
- Cross-database JOIN with aliases (workaround for Bug 13)
- Cross-database LEFT OUTER JOIN
- Cross-database COALESCE + LEFT JOIN
- Cross-database EXISTS / NOT EXISTS
- Cross-database correlated subquery
- Cross-database UNION / INTERSECT / EXCEPT
- 3-way cross-database JOIN
- GROUP BY / HAVING on attached DB
- DISTINCT on attached DB
- Window functions (ROW_NUMBER, SUM OVER) on attached DB
- ORDER BY with schema-qualified columns
- CTE (non-recursive) with attached DB
- BETWEEN / IN operators on attached DB
- JSON functions (json_extract, json_type) on attached DB
- BLOB operations (hex, zeroblob) on attached DB
- CAST on attached DB
- RETURNING clause on attached DB operations
- last_insert_rowid() on attached DB
- changes() / total_changes() on attached DB
- UPSERT (ON CONFLICT DO UPDATE) on attached DB
- INSERT OR IGNORE on attached DB
- INSERT OR ABORT on attached DB
- Multi-row INSERT on attached DB
- Multi-row INSERT with conflict (correct statement rollback)
- Statement-level rollback on attached DB (NOT NULL violation)
- DDL inside transaction + ROLLBACK on attached DB
- ATTACH :memory: read/write
- ATTACH with KEY parameter
- ATTACH non-existent file creates new DB
- ATTACH with file: URI and mode=ro (read-only enforcement)
- DETACH then reattach same file (data persists)
- Turso-created DB reattach
- Multiple writes + COMMIT on attached DB (verified with sqlite3)
- Autocommit on attached DB (verified with sqlite3)
- ROLLBACK on multi-DB transaction (rolls back both)
- Multiple DDL + DML interleaved on attached DB
- DETACH/re-ATTACH with index reuse
- PRAGMA user_version read/write on attached DB
- PRAGMA schema_version on attached DB
- PRAGMA page_size / journal_mode / page_count / freelist_count / cache_size / auto_vacuum / encoding on attached DB
- PRAGMA wal_checkpoint on attached DB
- ALTER TABLE RENAME on attached DB
- ALTER TABLE RENAME COLUMN on attached DB (updates index SQL too)
- ALTER TABLE DROP COLUMN on attached DB
- DROP TABLE with indexes on attached DB (both cleaned up)
- DROP INDEX on attached DB (schema-qualified)
- DROP TRIGGER on attached DB (schema-qualified)
- CREATE TABLE IF NOT EXISTS cross-DB
- CREATE INDEX IF NOT EXISTS on attached DB
- CREATE TABLE same name in 3 schemas (main + 2 attached)
- FK enforcement within attached DB
- ON DELETE CASCADE on attached DB
- STRICT table type enforcement on attached DB
- CHECK constraint enforcement on attached DB
- EXPLAIN QUERY PLAN on cross-DB query
- Large text values (overflow pages) on attached DB
- Access after DETACH (correct error)
- Error for wrong schema name (correct error)
- CREATE VIEW on main referencing attached table (correctly rejected)
- CREATE TABLE / CREATE VIEW / CREATE TRIGGER: schema prefix NOT stored in sqlite_master SQL (correct)
  - Exception: CREATE INDEX DOES store schema prefix (Bug 1)

#### Bugs Found (Round 3):
11. Views on attached DBs cannot be queried (unqualified table in view SQL fails)
12. Unqualified table names don't fall back to attached databases
13. Schema.table.column references resolve to wrong table in cross-DB JOINs (same table name)
14. Query optimizer doesn't use indexes on attached databases (full scan instead)
15. DROP INDEX / DROP TRIGGER with unqualified name fails to search attached databases

### Round 4 Tests (finding bugs 16-20)

#### Working correctly:
- ANALYZE aux (reads and writes sqlite_stat1 to correct DB)
- Cross-DB trigger fires correctly (trigger in attached DB)
- ATTACH WAL-mode database read/write
- Same table name in 3+ attached DBs with schema qualifiers
- Cross-DB INSERT INTO ... SELECT
- COLLATE NOCASE on attached DB (SELECT, DISTINCT, UNIQUE)
- NATURAL JOIN cross-DB
- JOIN USING cross-DB
- Self-join on attached table
- Complex cross-DB correlated subquery with COALESCE
- Cross-DB UPDATE with subquery SET
- Cross-DB DELETE with WHERE EXISTS
- Cross-DB EXCEPT / INTERSECT
- ATTACH during active transaction (works like sqlite3)
- Multiple attached DBs write in single transaction (correctly committed)
- Data integrity verification: turso-created attached DB passes sqlite3 integrity_check
- Trigger on attached DB persists correctly, fires in sqlite3
- FK SET NULL on attached DB
- FK SET DEFAULT on attached DB
- ON UPDATE CASCADE on attached DB
- ON DELETE CASCADE on attached DB
- BEFORE UPDATE trigger with RAISE(ABORT) on attached DB
- Multiple triggers on same attached table
- AUTOINCREMENT on attached DB (sqlite_sequence management)
- Schema version correctly incremented after DDL on attached DB
- REPLACE (without UNIQUE beyond PK) on attached DB
- DETACH DATABASE syntax works
- INSERT OR IGNORE with multiple UNIQUE constraints on attached DB
- UPSERT (ON CONFLICT DO UPDATE) on attached DB
- Group functions (GROUP_CONCAT, json_group_array) on attached DB
- Complex CHECK constraints (LIKE, BETWEEN) on attached DB
- STRICT table type enforcement on attached DB
- DEFAULT expression (datetime('now')) on attached DB
- large text values (overflow pages) on attached DB
- Complex CASE WHEN with cross-DB subqueries
- LIMIT/OFFSET on cross-DB UNION
- Cross-DB JOIN with aggregation (COUNT, SUM)
- 3-way cross-DB JOIN with different table names
- Scalar functions (abs, min, max, printf, substr, instr, replace) on attached DB
- Date/time functions on attached DB
- IIF function on attached DB
- TOTAL() vs SUM() on attached DB
- ROWID access on attached DB
- BETWEEN cross-DB comparison
- last_insert_rowid() across databases (connection-wide)
- Schema name case insensitivity (MyDb == mydb == MYDB)
- Multi-column PK on attached DB
- ALTER TABLE RENAME on attached DB (updates index/trigger SQL)
- ALTER TABLE RENAME COLUMN on attached DB (updates index SQL)
- ALTER TABLE ADD COLUMN with DEFAULT on attached DB
- ALTER TABLE DROP COLUMN on attached DB
- DDL+DML in transaction on attached DB
- CREATE TABLE + ALTER TABLE in same transaction on attached DB
- Schema migration pattern (CREATE+INSERT SELECT+DROP+RENAME) on attached DB
- DETACH main correctly rejected
- ATTACH with directory path correctly rejected
- ATTACH same file twice (correct behavior, changes visible across aliases)
- DROP TABLE IF EXISTS on non-existent attached table (correctly silent)
- Ambiguous table/schema name (table named 'aux' in main) resolved correctly
- Unicode table/column names on attached DB
- ALTER TABLE RENAME to name existing in another schema (correctly allowed)
- Cross-DB UPDATE with EXISTS
- NOT NULL constraint violation error on attached DB
- PK violation error on attached DB (correct error message)
- Read-only attached DB (file: URI ?mode=ro) rejects writes
- ATTACH different page size correctly rejected
- ATTACH non-SQLite file gives I/O error
- ATTACH empty string creates in-memory DB
- Externally modified attached DB correctly read in new session
- INSERT from CTE into attached DB (non-recursive)
- ATTACH with quoted non-keyword identifier works ("mydb", [mydb], `mydb`)
- PRAGMA aux.schema_version correctly reflects DDL changes
- PRAGMA aux.table_info works correctly
- PRAGMA aux.table_xinfo works correctly
- PRAGMA aux.encoding correctly returns UTF-8
- PRAGMA aux.auto_vacuum returns correct value
- PRAGMA database_list shows correct IDs
- SELECT on aux.sqlite_schema alias works
- COUNT on aux.sqlite_master with GROUP BY works
- GLOB operator on attached DB works
- Table/index name collision detection on attached DB works
- EXPLAIN bytecode shows correct iDb for attached DB operations

#### Not tested (features not supported yet):
- REINDEX ("not supported yet")
- VACUUM aux ("not supported with schema name yet")
- Recursive CTEs ("circular reference" error)
- UPDATE ... FROM clause ("not supported")
- RANK() window function ("no such function")
- PRAGMA foreign_key_check ("Not a valid pragma name")
- PRAGMA foreign_key_list ("Not a valid pragma name")
- PRAGMA collation_list ("Not a valid pragma name")
- PRAGMA data_version ("Not a valid pragma name")
- PRAGMA locking_mode ("Not a valid pragma name")
- Generated columns (STORED / VIRTUAL) on attached DB ("not supported")
- CREATE TABLE AS SELECT ("not supported")
- MVCC mode (--experimental-mvcc flag doesn't exist)

#### Bugs Found (Round 4):
16. ATTACH NULL fails (should create in-memory DB like sqlite3)
17. Schema-qualified names with SQL keywords as schema name fail to parse ("select".t)
18. ATTACH of 0-byte (empty) file causes hang/infinite loop
19. BEGIN IMMEDIATE/EXCLUSIVE doesn't emit Transaction for attached databases
20. DML on attached DB unnecessarily opens WRITE transaction on main database

### Round 5 Tests (finding bugs 21-25)

#### Working correctly:
- Cross-DB trigger within same attached schema (trigger references same-schema table)
- INSERT OR ROLLBACK on attached DB (correctly rolls back transaction)
- Cross-DB IN subquery (SELECT from main WHERE col IN (SELECT from aux))
- Cross-DB COALESCE with subquery
- Complex UPSERT (ON CONFLICT DO UPDATE) on attached DB
- CREATE INDEX IF NOT EXISTS on attached DB
- UPDATE with correlated subquery from main to attached
- DROP TABLE IF EXISTS on non-existent attached table
- DETACH then re-ATTACH (data persists correctly)
- COMMIT with only attached DB changes
- CASE WHEN expression on attached DB
- RETURNING clause on INSERT/UPDATE/DELETE on attached DB
- FK ON UPDATE CASCADE on attached DB
- FK ON DELETE CASCADE (multi-level) on attached DB
- Empty table operations (SELECT/COUNT/INSERT/DELETE) on attached DB
- COLLATE NOCASE on attached DB (WHERE, DISTINCT, ORDER BY)
- Expression index on attached DB (SELECT works, index not used - related to Bug 14)
- LIKE with ESCAPE on attached DB
- Multiple DDL in single transaction on attached DB
- GROUP BY HAVING on attached DB
- NOT NULL constraint violation in transaction on attached DB
- Cross-DB DELETE with NOT IN subquery
- Window function PARTITION BY on attached DB
- INSERT OR REPLACE with PK-only (no UNIQUE index) on attached DB
- INSERT OR REPLACE with RETURNING on attached DB
- COUNT(DISTINCT) on attached DB
- Cross-DB DELETE with IN subquery
- BEFORE trigger RAISE(IGNORE) on attached DB
- BEFORE trigger RAISE(FAIL) on attached DB
- ATTACH WAL-mode database (read/write)
- UNION ALL across same-name tables in different schemas
- json_group_array on attached DB
- ALTER TABLE RENAME on attached DB (updates trigger/index SQL)
- INSERT with self-referencing subquery on attached DB
- PRAGMA user_version SET on attached DB (correctly modifies aux file)
- PRAGMA application_id SET on attached DB (correctly modifies aux file)
- PRAGMA max_page_count per-DB independence (works correctly)
- PRAGMA page_count per-DB (works correctly)
- PRAGMA auto_vacuum per-DB (works correctly)
- AUTOINCREMENT sequence management on attached DB
- INSERT SELECT from indexed attached DB to main
- Complex constraints (NOT NULL + UNIQUE + CHECK) on attached DB
- ATTACH with KEY parameter (ignored, like sqlite3)
- DROP + recreate table in transaction on attached DB
- Large BLOB values (10KB, 50KB) on attached DB with overflow pages
- STRICT table type enforcement on attached DB
- Multi-DB ROLLBACK (correctly undoes changes to both DBs)
- Cross-DB BETWEEN query
- DEFAULT CURRENT_TIMESTAMP on attached DB
- Complex nested cross-DB subqueries (multi-level)
- INSERT OR IGNORE + AUTOINCREMENT on attached DB
- CTE with attached DB
- ATTACH during savepoint
- Cross-DB comparison in WHERE clause
- Complex correlated DELETE on attached DB
- ROWID access on attached DB
- DDL isolation between attached DBs
- Schema-qualified columns in GROUP BY/HAVING/ORDER BY
- Table alias conflicting with schema name (correctly resolved)
- Cross-DB move operation (INSERT SELECT + DELETE in transaction)
- INSERT OR FAIL on attached DB (transaction continues after error)
- GROUP_CONCAT on attached DB
- ALTER TABLE RENAME COLUMN on attached DB (updates index SQL)
- last_insert_rowid() across databases
- ATTACH with unicode schema name
- Multi-session ATTACH with WAL persistence
- EXPLAIN cross-DB JOIN (correct iDb values in bytecode)
- INSERT with cross-DB subquery in VALUES clause
- DDL persistence on attached DB file (verified across sessions)
- Schema version correctly bumps after DDL on attached DB
- INSERT OR IGNORE with PK conflict on attached DB
- changes()/total_changes() across databases
- Encoding consistency (UTF-8) on attached DB
- UPDATE PK on attached DB
- DETACH + re-ATTACH cursor state cleanup
- Trigger with comment in body on attached DB
- ATTACH fresh (non-existent) file creates new DB correctly
- Operations after failed ATTACH (non-existent path, corrupt file)
- Database ID assignment after DETACH/re-ATTACH cycle
- ANALYZE on attached DB writes to correct sqlite_stat1
- Cross-DB view creation correctly rejected (sqlite3 compatibility)
- Schema-qualified trigger body correctly rejected (sqlite3 compatibility)
- PRAGMA wal_checkpoint per-DB (different return values due to WAL mode differences)
- sqlite_master UNION across schemas

#### Not tested (features not supported):
- REINDEX ("not supported yet")
- PRAGMA journal_size_limit ("Not a valid pragma name")
- PRAGMA data_version ("Not a valid pragma name")
- PRAGMA checksum_verification ("Not a valid pragma name")
- PRAGMA optimize ("Not a valid pragma name")
- sqlite_offset() ("no such function")
- CREATE TABLE AS SELECT ("not supported")
- Recursive CTEs ("not supported")

#### Code analysis findings (confirmed via source review):
- core/translate/schema.rs:1607 - DROP TABLE gets indices from main schema (Bug 21)
- core/translate/schema.rs:1409,1427 - CREATE VIRTUAL TABLE writes to main DB
- core/translate/schema.rs:1435 - ParseSchema uses cursor_id as DB ID (virtual tables)
- core/translate/index.rs:1163 - IndexMethodOptimize hardcoded to main DB
- core/translate/main_loop/open.rs:517 - IndexMethodQuery hardcoded to main DB
- core/translate/transaction.rs:30-47 - BEGIN IMMEDIATE/CONCURRENT only emits for main (Bug 19)
- core/vdbe/execute.rs:3321 - n_active_writes not counted for attached DBs
- core/vdbe/execute.rs:2745 - MVCC FK rollback hardcoded to MAIN_DB_ID
- Multiple TODO comments: database: None in select.rs:457, expr.rs:5695,5811, planner.rs:2007

#### Bugs Found (Round 5):
21. DROP TABLE on attached DB leaks index B-tree pages (freelist_count=1 vs 3)
22. PRAGMA cache_size ignores schema qualifier (operates on main DB)
23. PRAGMA freelist_count ignores schema qualifier (returns main's value)
24. EXPLAIN QUERY PLAN doesn't show schema name prefix for attached DB tables
25. DROP TABLE on attached DB doesn't clean up sqlite_sequence entry (AUTOINCREMENT)

### Round 6: Systematic Bug Hunting (2026-04-01)

#### Tests performed:
- UPSERT (INSERT ... ON CONFLICT DO UPDATE) on attached DB - works
- INSERT OR IGNORE on attached DB - works
- INSERT OR ABORT on attached DB - works
- INSERT OR ROLLBACK on attached DB - works correctly (rolls back txn)
- INSERT OR FAIL on attached DB - works correctly (keeps txn open)
- CHECK constraints on pre-existing attached DB - works
- FK enforcement (ON DELETE CASCADE, ON UPDATE CASCADE, ON DELETE SET NULL) - works
- RETURNING clause on attached DB (INSERT/UPDATE/DELETE) - works
- Trigger with WHEN clause on attached DB - works
- BEFORE trigger with RAISE on attached DB - works
- Cross-DB INSERT ... SELECT (both directions) - works
- Cross-DB INSERT ... SELECT with JOIN - works
- UPDATE OR REPLACE on attached DB - works
- REPLACE INTO with PK-only on attached DB - works
- REPLACE INTO with UNIQUE constraint - PANICS (Bug 4, already known)
- ALTER TABLE RENAME on attached DB - works
- ALTER TABLE RENAME COLUMN on attached DB - works
- ALTER TABLE DROP COLUMN on attached DB (memory and file) - works
- ALTER TABLE RENAME COLUMN with index - works (index SQL updated)
- ALTER TABLE RENAME COLUMN with trigger - works (trigger SQL updated)
- COMPOUND queries (UNION, INTERSECT, EXCEPT) across DBs - works
- Window functions on attached DB - works
- CTE with attached DB - works
- Correlated subquery on attached DB - works
- EXISTS subquery across DBs - works
- NOT IN subquery across DBs - works
- Cartesian product with schema.table.column in WHERE - BUG 26
- PRAGMA database_list with multiple attached DBs - works
- last_insert_rowid across attached DBs - works
- changes() and total_changes() across attached DBs - works
- ROLLBACK across multiple attached DBs - works
- SAVEPOINT across multiple attached DBs - BROKEN (Bug 5, confirmed)
- PRAGMA schema_version on attached DB - works (read correctly per-DB)
- PRAGMA page_count on attached DB - works (correct per-DB values)
- PRAGMA page_size on attached DB - works
- PRAGMA user_version on attached DB - works
- PRAGMA application_id on attached DB - works
- PRAGMA encoding on attached DB - works
- PRAGMA synchronous on attached DB - BUG 27 (ignores schema qualifier)
- PRAGMA max_page_count on attached DB - works
- PRAGMA auto_vacuum on attached DB - not enabled (experimental)
- PRAGMA journal_mode on attached DB - consistent
- PRAGMA cache_spill on attached DB - consistent (connection-wide in both)
- PRAGMA query_only on attached DB - consistent (connection-wide in both)
- PRAGMA temp_store on attached DB - consistent (connection-wide in both)
- PRAGMA foreign_keys on attached DB - consistent (connection-wide in both)
- PRAGMA full_column_names on attached DB - consistent (connection-wide in both)
- PRAGMA ignore_check_constraints on attached DB - consistent (connection-wide)
- PRAGMA require_where on attached DB - not in sqlite3 (tursodb-specific)
- COLLATE NOCASE on attached DB - works
- DEFAULT expressions on attached DB - works
- File persistence (write, detach, reattach) - works
- ATTACH same file twice with different aliases - works (both allow)
- ATTACH inside a transaction - works (both allow)
- ATTACH with expression as filename (concatenation) - works
- ATTACH with empty string filename - works (creates temp DB)
- ATTACH with reserved names (main, temp) - correctly rejected
- ATTACH with duplicate alias - correctly rejected
- ATTACH with non-existent directory - correctly errors
- ATTACH with non-SQLite file - correctly errors
- ATTACH with different page size - correctly rejected
- DETACH non-existent database - correctly errors
- DETACH twice (double detach) - correctly errors
- DETACH during transaction - succeeds (Bug 3, known), rolls back correctly
- ATTACH/DETACH/REATTACH ID reuse - works (reuses freed IDs)
- DROP TABLE on attached DB with triggers - correctly removes triggers
- DROP VIEW on attached DB - works
- DROP VIEW IF EXISTS on attached DB - works
- DROP TRIGGER on attached DB - works
- DROP TABLE with FK constraints on attached DB - correctly prevented
- CREATE TABLE IF NOT EXISTS on attached DB - works
- CREATE INDEX IF NOT EXISTS on attached DB - works
- CREATE VIEW IF NOT EXISTS on attached DB - BROKEN (not ATTACH-specific, logged as unrelated)
- CREATE TRIGGER IF NOT EXISTS on attached DB - works
- STRICT table operations on attached DB - works
- Multi-row INSERT VALUES on attached DB - works
- INSERT DEFAULT VALUES on attached DB - works
- Multi-table schema on attached DB - works
- Composite PRIMARY KEY on attached DB - works
- Complex DELETE with subquery on attached DB - works
- Cross-DB UPDATE with subquery - works
- Nested triggers on attached DB - works
- Recursive triggers - not implemented (not ATTACH-specific)
- Trigger + SAVEPOINT on attached DB - BROKEN (Bug 5)
- Large blobs (overflow pages) on attached DB - works
- typeof/quote/hex on attached DB - works
- LIKE/GLOB/BETWEEN on attached DB - works
- IN subquery cross-DB - works
- GROUP BY/HAVING on attached DB - works
- ORDER BY/LIMIT/OFFSET on attached DB - works
- CASE expressions on attached DB - works
- IFNULL/COALESCE on attached DB - works
- AGGREGATE functions (COUNT/SUM/AVG/MIN/MAX/GROUP_CONCAT/TOTAL) on attached DB - works
- NATURAL JOIN across DBs - works
- LEFT JOIN across DBs - works
- JOIN USING across DBs - works
- INDEXED BY on attached DB - BUG 28
- NOT INDEXED on attached DB - works
- Expression index with JSON on attached DB - creates correctly, not used by optimizer (Bug 14)
- ANALYZE on attached DB (qualified) - works
- ANALYZE (unqualified) - BUG 29 (only analyzes main)
- Pre-existing indexes on attached DB (UNIQUE enforcement) - works
- Pre-existing triggers on attached DB - works
- Pre-existing CHECK constraints on attached DB - works
- Cross-DB trigger reference - correctly rejected (same as sqlite3)
- DDL + DML in transaction on attached DB - works
- Multi-DB transaction commit and rollback - works
- Error handling in transaction on attached DB - works
- Auto-commit on attached DB - works
- Schema cookie tracking on attached DB - works
- sqlite_master operations (read, UNION across DBs) - works
- sqlite_schema alias on attached DB - works
- .tables with multiple attached DBs - works
- .schema with attached DBs - works (with Bug 1 in index SQL)
- .dump - only dumps main (same as sqlite3)
- ROWID/_rowid_/oid aliases on attached DB - works
- NULLS FIRST/NULLS LAST on attached DB - works
- EXPLAIN on ATTACH statement - works
- WAL checkpoint on attached DB - works
- INSERT ... SELECT ... ON CONFLICT across DBs - works
- INSERT ... RETURNING with cross-DB expression - works
- STRICT table + ALTER TABLE ADD COLUMN on memory-attached DB - BUG 30

#### Unrelated bugs found:
- CREATE VIEW IF NOT EXISTS broken on BOTH main and attached DB (if_not_exists field ignored in translate_create_view)

#### Bugs Found (Round 6):
26. Schema.table.column in WHERE with cartesian product gives "ambiguous column name"
27. PRAGMA synchronous ignores schema qualifier (writes/reads connection-wide)
28. INDEXED BY fails on attached DB with "no such index"
29. Unqualified ANALYZE only analyzes main DB, not attached databases
30. ALTER TABLE ADD COLUMN default type validation reads wrong pager on ALL attached DBs (extends Bug 10)

### Round 7: Deep Bug Hunting (2026-04-01)

#### Tests performed:
- Generated columns on attached DB (not supported, error as expected)
- Cross-DB foreign keys (correctly rejected by both tursodb and sqlite3)
- Partial index on attached DB (read works, INSERT works)
- RENAME TABLE to name existing in another schema (works correctly)
- WAL checkpoint behavior on DETACH vs sqlite3 (BUG 31)
- VACUUM on attached DB (not supported, error as expected)
- Database_list after ATTACH/DETACH cycles (correct ID reuse)
- WAL persistence after process exit for attached vs main (confirms BUG 31)
- AUTOCOMMIT mode on attached DB (works correctly)
- Complex triggers (BEFORE+AFTER) on attached DB (works correctly)
- COLLATE NOCASE index on pre-existing attached DB (works)
- Expression index on attached DB (creates correctly, not used by optimizer)
- UNIQUE INDEX enforcement on attached DB (works correctly)
- INSERT OR REPLACE with UNIQUE on attached DB (still panics - Bug 4)
- Sequential writes on attached DB (correct)
- DROP TABLE on attached DB cleans up triggers (correct)
- Multi-column PK on attached DB (works)
- Self-join on attached DB (works)
- CREATE TABLE IF NOT EXISTS across schemas (works)
- Complex CHECK constraints on attached DB (works correctly)
- PRAGMA index_list/info/xinfo on attached DB (confirms Bug 7)
- ALTER TABLE DROP COLUMN with index on attached DB (correctly rejects)
- ALTER TABLE ADD COLUMN NOT NULL DEFAULT on attached DB (works correctly)
- Multi-DB ROLLBACK (correctly rolls back both)
- DROP TABLE with FK references on attached DB (correctly prevented)
- Deferred FK on attached DB (works correctly)
- Deferred FK violation at COMMIT on attached DB (correctly detected)
- Nested SAVEPOINTs on attached DB (broken - confirms Bug 5)
- Schema persistence after DETACH (BUG 32 - indexes corrupt file)
- Schema persistence WITHOUT indexes (works correctly)
- .schema display for attached DB indexes (BUG 33 - extra prefix)
- .schema display for triggers (general bug - triggers not shown even on main)
- Trigger round-trip across tursodb sessions (works correctly)
- Pre-existing attached DB with complex schema (works correctly)
- ALTER TABLE RENAME + CHECK constraint on attached DB (works)
- ALTER TABLE RENAME + index SQL update on attached DB (works, with Bug 1 prefix)
- Complex cross-DB JOIN with GROUP BY + HAVING (works correctly)
- Window functions on cross-DB JOIN (works correctly)
- Cross-DB UPDATE with scalar subquery (works correctly)
- Cross-DB DELETE with IN subquery (works correctly)
- Cross-DB BETWEEN comparison (works correctly)
- Cross-DB COALESCE + LEFT JOIN (works correctly)
- INSERT SELECT with CASE expression across DBs (works correctly)
- Cross-DB INSERT ... SELECT with ORDER BY (works correctly)
- DISTINCT on cross-DB UNION (works correctly)
- RETURNING with expressions on attached DB (works correctly)
- Multiple writes to same row in transaction on attached DB (works)
- Schema_version tracking across DDL on attached DB (correct)
- Multi-session writes to attached DB (data persists correctly)
- Re-attach after external modification (sees new data correctly)
- Read-only ATTACH with file: URI (correctly rejects writes)
- ATTACH with symlink (works correctly)
- Self-ATTACH (same file as main) (works like sqlite3)
- ATTACH with empty string schema name (works)
- ATTACH with unicode schema name (works)
- ATTACH with very long schema name (works)
- ATTACH with runtime expression filename (works)
- EXPLAIN for various attached DB operations (correct iDb values)
- BEGIN IMMEDIATE with multiple attached DBs (confirms Bug 19)
- count(*) on empty attached DB sqlite_master (BUG 34)
- count(*) on empty file-based attached DB (also BUG 34)
- file: URI with mode=memory parameter (BUG 35 - creates file instead of memory)
- COLLATE in ORDER BY on attached DB (works)
- GROUP_CONCAT on attached DB (works)
- TYPE affinity on attached DB (correct)
- MIN/MAX with COLLATE NOCASE on attached DB (correct)
- BLOB operations on attached DB (correct)
- AUTOINCREMENT on pre-existing attached DB (correct)
- total_changes() across databases (correct)
- last_insert_rowid() across databases (correct)
- changes() across databases (correct)
- PRAGMA page_count tracking on attached DB (correct)
- UPSERT DO NOTHING on attached DB (works)
- INSERT OR IGNORE with multiple UNIQUE constraints on attached DB (works)
- CREATE INDEX IF NOT EXISTS on attached DB (works)
- Large text (overflow pages) on attached DB (works)
- Complex table with many constraints on attached DB (works)
- ERROR messages for constraint violations on attached DB (correct format)
- DETACH during active transaction (confirms Bug 3)
- DROP TABLE IF EXISTS unqualified on attached table (silently does nothing - consequence of Bug 12)
- VACUUM on attached DB (not supported yet, error as expected)
- VACUUM INTO from attached DB (not supported yet)
- ATTACH with KEY parameter (works, key ignored like sqlite3)
- ATTACH with 'temp' name (correctly rejected)

#### Unrelated bugs found:
- DROP VIEW IF EXISTS on a TABLE silently does nothing (should error like sqlite3: "use DROP TABLE")
- .schema command doesn't show triggers even on main DB

#### Bugs Found (Round 7):
31. WAL not checkpointed on DETACH for attached databases
32. CREATE INDEX on attached DB produces files unreadable by sqlite3 (schema prefix in stored SQL)
33. .schema display adds extra schema prefix to table name in CREATE INDEX for attached DBs
34. Reading sqlite_master from empty attached DB with count(*) causes I/O error
35. file: URI mode=memory parameter ignored for ATTACH (creates file instead of memory)

### Round 8: Additional Bug Hunting (2026-04-02)

#### Tests performed:
- INSTEAD OF triggers on views (not supported even on main - unrelated)
- PRAGMA aux.quick_check (checks main instead of aux - same as Bug 6)
- Cross-DB UPDATE with correlated subquery from 2 attached DBs (works)
- Cross-DB trigger body references (trigger created, fails at runtime - same as sqlite3)
- Trigger with OLD/NEW references on attached DB (works)
- Trigger chain (cascading triggers) on attached DB (works)
- BEFORE INSERT trigger with RAISE on attached DB (works)
- DELETE RETURNING with trigger on attached DB (works)
- Trigger cleanup on DROP TABLE in attached DB (works)
- ALTER TABLE DROP COLUMN referenced in trigger on attached DB (correctly rejected)
- ALTER TABLE RENAME COLUMN in CHECK constraint on attached DB (works)
- schema.table.* syntax (not valid SQL in either sqlite3 or tursodb)
- Nested savepoints across multiple attached DBs (confirms Bug 5)
- ATTACH sqlite3-created WAL DB (works correctly)
- ATTACH with spaces in path (works)
- ALTER TABLE RENAME with multiple indexes on attached DB (works)
- DDL+DML rollback in transaction on attached DB (works)
- UPSERT with triggers on attached DB (works)
- 3-way cross-DB operations (works)
- Complex ON CONFLICT expression on attached DB (works)
- Same-name triggers across schemas (works correctly)
- Complex cross-DB UPDATE with correlated subquery (works)
- ATTACH during BEGIN IMMEDIATE (works)
- Trigger running total on attached DB (works)
- DDL rollback on attached DB (works correctly)
- GROUP BY with schema-qualified columns (works)
- INSERT SELECT with GROUP BY on attached DB (works)
- Complex aggregation (SUM + CASE + HAVING) on attached DB (works)
- INSERT SELECT with ORDER BY LIMIT cross-DB (works)
- PRAGMA table_info after ALTER on attached DB (works)
- Multi-row INSERT ON CONFLICT on attached DB (works)
- Max rowid auto-assign on attached DB (works)
- NULL handling in cross-DB JOINs (matches sqlite3)
- AUTOINCREMENT after DELETE on attached DB (works correctly)
- ALTER TABLE RENAME + view SQL on attached (NOT updated - same bug exists on main, unrelated)
- REPLACE INTO with TEXT PRIMARY KEY on attached DB (panics - same as Bug 4)
- Trigger error propagation on attached DB (correctly rolls back)
- Cross-schema trigger body validation (tursodb and sqlite3 both defer to runtime)
- Multi-DB transaction failure rollback (works correctly)
- EXPLAIN bytecode for INSERT/UPDATE/DELETE on attached DB (correct iDb values, but Bug 20 confirmed)
- DROP TRIGGER by unqualified name with same-name trigger in multiple schemas (works correctly)
- Complex cross-DB correlated subquery (works, matches sqlite3)
- ALTER TABLE DROP COLUMN with trigger not referencing dropped column (works)
- Multi-trigger on attached DB (works)
- ATTACH non-WAL (vacuumed) DB (works)
- Many tables on attached DB (works)
- Keyword as schema name (Bug 17 confirmed)
- ATTACH same file as main (self-attach) (works correctly, write gives "database is busy")
- EXPLAIN for ATTACH (correct bytecode)
- Subquery in INSERT VALUES from attached DB (works)
- ATTACH with function call expression (works)
- ATTACH with CASE expression (works)
- ATTACH with CAST expression (works)
- VACUUM INTO with attached DB (only vacuums main, correct)
- Schema version isolation between main and attached (correct)
- EXCEPT/INTERSECT across attached DBs (works)
- Expression index correctness on attached DB (correct results)
- STRICT table type coercion on attached DB (matches sqlite3)
- Complex UPDATE with cross-DB aggregate subquery (works)
- CASE WHEN with cross-DB subquery in SELECT (works)
- ORDER BY with schema-qualified columns cross-DB (works)
- HAVING with cross-DB subquery (works)
- UNION across sqlite_master from multiple schemas (works)
- Trigger body subquery with same-name table in both schemas (correctly resolves to trigger's schema)
- COLLATION in DISTINCT on attached DB (matches sqlite3)
- JSON operations on attached DB (matches sqlite3)
- COALESCE cross-DB (matches sqlite3)
- CREATE TRIGGER in aux on main-only table (correctly rejected)
- DROP TABLE IF EXISTS on view (silently succeeds, should error like sqlite3 - unrelated)
- DROP VIEW IF EXISTS on table (silently succeeds, should error like sqlite3 - unrelated)

#### New bugs found (Round 8):
36. CREATE TRIGGER in one schema ON another schema's table is silently accepted and targets wrong table
37. Views in attached schemas resolve unqualified table names from main instead of own schema
38. ATTACH with subquery expression not supported (sqlite3 supports it)
39. Schema-qualified names with empty string schema fail to parse
40. Schema-qualified names with numeric schema name fail to parse

### Post-investigation: Merged duplicates/related bugs

40 bugs merged into 26 distinct issues in attach_bugs.md:
- Bugs 1+32 → Bug 1 (schema prefix in CREATE INDEX + corrupt files)
- Bugs 2+11+37 → Bug 2 (view name resolution in attached schemas)
- Bugs 6+7+8+22+23+27 → Bug 6 (PRAGMAs ignore schema qualifier)
- Bugs 10+30 → Bug 7 (ALTER ADD COLUMN type validation wrong pager)
- Bugs 12+15 → Bug 9 (unqualified names don't fall back to attached DBs)
- Bugs 13+26 → Bug 10 (schema.table.column three-part references broken)
- Bugs 17+39+40 → Bug 13 (parser can't handle non-identifier schema qualifiers)
- Bugs 21+25 → Bug 17 (DROP TABLE cleanup misses indexes/sequence)

### Round 9: Deep Edge Case Investigation (2026-04-02)

#### Tests performed (working correctly):
- json_each table-valued function on attached DB
- Complex UPSERT with excluded references on attached DB
- ALTER TABLE ADD COLUMN with FK on attached DB
- DELETE with RETURNING + trigger on attached DB
- UPSERT + triggers (AFTER INSERT/UPDATE) on attached DB
- DELETE from attached DB table with index (correct index maintenance)
- DELETE from attached DB table with same-name table in main (correct schema resolution)
- UPDATE UNIQUE constraint violation on attached DB (correctly enforced)
- Complex subquery in FROM clause from attached DB
- Correlated subquery with attached DB in complex expression
- Multiple CTEs from different attached DBs
- COLLATE NOCASE + UNIQUE on attached DB
- FK ON UPDATE CASCADE on attached DB
- Chained triggers (INSERT→UPDATE→DELETE→INSERT) on attached DB
- Multi-row INSERT statement rollback on attached DB (UNIQUE violation)
- Cross-DB INSERT ON CONFLICT with triggers
- AUTOINCREMENT persistence across DETACH/reattach cycles
- Cross-DB JOIN + ORDER BY + LIMIT
- Cross-DB UPDATE from two attached DBs
- ALTER TABLE RENAME + sqlite_master updates on attached DB
- Error recovery after constraint violation on attached DB
- DETACH doesn't affect operations on remaining attached DBs
- Cross-DB DELETE with IN subquery from different attached DB
- Multiple ALTER TABLE operations in sequence on attached DB
- DEFAULT expressions (datetime, random, concatenation) on attached DB
- DDL rollback (BEGIN + CREATE TABLE + ROLLBACK) on attached DB
- Complex DDL+DML rollback on attached DB
- COMMIT + ROLLBACK sequence on attached DB
- 3-way UNION from attached DBs
- CASE WHEN cross-DB comparison
- NATURAL JOIN cross-DB with type affinity mismatch
- RAISE(ABORT) trigger statement rollback on attached DB
- RAISE(FAIL) trigger on attached DB
- Complex UPSERT with running stats on attached DB
- Cross-DB UPDATE RETURNING
- ATTACH/DETACH 10 cycles (resource management)
- 3-DB aggregate INSERT SELECT
- Drop + recreate table with different schema on attached DB
- Large blob operations (100KB, 200KB zeroblob) on attached DB
- Cross-DB INSERT SELECT with type coercion
- VACUUM INTO with attached DBs present (correctly only copies main)
- PRAGMA table_xinfo on complex attached DB table
- DDL on read-only attached DB (correctly rejected)
- Schema version tracking across DDL on attached DB
- Multi-DB transaction error recovery
- RETURNING + trigger interaction on attached DB
- Group functions (GROUP_CONCAT) on attached DB
- Multiple indexes UPDATE on attached DB (all maintained correctly)
- ANALYZE on specific attached DB indexes (correct sqlite_stat1)
- CREATE INDEX IF NOT EXISTS on attached DB
- HAVING with cross-DB EXISTS subquery
- Bulk operations (50 rows + index + delete/update) on attached DB
- sqlite_master queries (GROUP BY, LIKE) on attached DB
- Large INSERT SELECT between attached DBs with complex WHERE
- Zero-length strings and empty blobs on attached DB
- Special characters in column names on attached DB
- AUTOINCREMENT sequence management on attached DB
- COALESCE with NULLs cross-DB
- Cross-DB row move in single transaction
- Various PRAGMAs (schema_version, user_version, application_id, etc.) on attached DB
- Per-DB PRAGMA settings (user_version isolation)
- REPLACE INTO on main when attached has same-name table (correct isolation)
- INSERT SELECT from main view into attached DB
- RELEASE SAVEPOINT on attached DB (correct)
- Round-trip: tursodb create → reattach (complex schema with COLLATE, CHECK, UNIQUE)
- File-based attached DB with triggers (round-trip to sqlite3 works)
- External modification (sqlite3 modifies, tursodb reattaches and sees changes)

#### Confirmed existing bugs:
- Bug 1: CREATE INDEX schema prefix in stored SQL (confirmed in multiple new tests)
- Bug 2: View resolution on attached DB (confirmed)
- Bug 4: INSERT OR REPLACE panic (confirmed with composite PK, expression indexes)
- Bug 5: SAVEPOINT ROLLBACK (confirmed for DDL too)
- Bug 6: PRAGMAs ignore schema qualifier (confirmed for freelist_count, integrity_check)
- Bug 11: Optimizer doesn't use indexes on attached DB (confirmed even after ANALYZE)
- Bug 16: Unnecessary write transaction on main (confirmed for DDL and SELECT too)
- Bug 23: count(*) on empty attached DB sqlite_master (confirmed)

#### New bugs found (Round 9):
27. `.import` CLI command cannot import into attached DB tables
28. INSERT OR REPLACE panic extends to composite PRIMARY KEY on attached DB
29. INSERT OR REPLACE panic extends to expression indexes on attached DB
30. DDL and SELECT on attached DB open unnecessary transactions on main
31. SAVEPOINT ROLLBACK doesn't undo DDL (CREATE TABLE) on attached databases

#### Unrelated bugs found:
- ALTER TABLE ADD COLUMN with expression-based DEFAULT (e.g., datetime('now')) rejected by tursodb but allowed by sqlite3 (not ATTACH-specific)
- UPDATE t SET a = b, b = a appears to hang (infinite loop) on both main and attached - actually was piped input issue, not a real bug
- DELETE FROM sqlite_sequence rejected with "table may not be modified" on both main and attached (sqlite3 allows it) - not ATTACH-specific

### Round 10: Feature Flag & URI Parameter Bug Hunting (2026-04-02)

#### Tests performed:
- Self-referencing UPDATE on attached DB (works)
- UPDATE with LIMIT on attached DB (works)
- DELETE with LIMIT on attached DB (works, ORDER BY not supported)
- Cross-attached-DB INSERT SELECT (works)
- Trigger body name resolution on attached DB (works)
- File persistence after exit without DETACH (works)
- INSERT SELECT from json_each cross-DB (works)
- Deferred FK at COMMIT on attached DB (works correctly)
- HAVING with cross-DB subquery (works)
- REPLACE with COLLATE NOCASE UNIQUE on attached DB (panics - same as Bug 4)
- Schema migration pattern on attached DB (CREATE/INSERT/DROP/RENAME in txn - works)
- Schema cookie tracking after DDL on attached DB (works)
- Cross-schema FK reference (correctly rejected by both tursodb and sqlite3)
- Cross-DB INSERT into STRICT table (correctly rejects type mismatch)
- Generated columns on MAIN DB (works correctly with --experimental-generated-columns)
- Generated columns on ATTACHED DB (BUG 32 - feature flag not propagated)
- Generated columns VIRTUAL from sqlite3-created attached DB (BUG 32)
- Generated columns STORED from sqlite3-created attached DB (separate error - "Stored generated columns not supported")
- WITHOUT ROWID table on attached DB (works correctly)
- FTS5 virtual table on attached DB (virtual table not found, regular table accessible)
- Mixed supported/unsupported schema on attached DB (regular tables accessible)
- Experimental autovacuum on attached DB (auto_vacuum stays 0)
- Experimental custom types on attached DB (works - flag propagated correctly)
- Experimental encryption on attached DB via URI (works with correct cipher name)
- Experimental index method on attached DB (works)
- Experimental views on attached DB (Bug 2 - view body resolution still broken)
- ROWID with aliases in cross-DB query (works)
- Simple ROWID on attached DB (works)
- Sequential ALTER TABLE on attached DB (rename, rename column, add column - all work)
- Nested savepoints on attached DB (confirms Bug 5 - ROLLBACK TO doesn't undo)
- Derived table from attached DB subquery (works)
- DELETE with NOT IN subquery from attached DB (works)
- Cross-DB COALESCE NULL handling (works)
- Cross-DB UPDATE with correlated subquery (works)
- EXPLAIN CREATE INDEX on attached DB (confirms Bug 1 - schema prefix in SQL)
- BEFORE RAISE(ABORT) on attached DB (works)
- Complex CASE WHEN cross-DB (works)
- NATURAL JOIN cross-DB (works)
- Complex GROUP BY cross-DB JOIN with HAVING (works correctly, matches sqlite3)
- Partial index on attached DB (works, not used by optimizer - Bug 11)
- Functions in WHERE on attached DB (LOWER, LENGTH, SUBSTR - all work)
- UNION ALL cross-DB with ORDER BY (works)
- Complex table with all column types on attached DB (works)
- EXPLAIN QUERY PLAN comparing main vs attached index use (confirms Bug 11)
- Aggregate functions on empty attached table (all correct)
- Cross-DB INSERT ON CONFLICT with excluded.* (works)
- 3-way cross-DB JOIN (works, matches sqlite3)
- Mixed INSERT modes in transaction on attached DB (works)
- Trigger cascade on attached DB (works)
- INSERT OR IGNORE all-conflict on attached (works, changes()=0)
- 3-way cross-DB INSERT with complex expression (works)
- Same table name in 4+ schemas (works)
- ATTACH with cache=shared URI (works)
- Multiple triggers on same attached table (works)
- Multi-DB ROLLBACK on file-based attached (works correctly)
- DELETE RETURNING with trigger on attached DB (works)
- UPDATE RETURNING on attached DB (works)
- Complex UPSERT with excluded on attached (works)
- AUTOINCREMENT after delete on attached (continues correctly)
- AUTOINCREMENT max rowid on attached (correctly errors)
- Multi-DB transaction where one fails (correct isolation)
- Complex ALTER RENAME COLUMN with trigger on attached (trigger SQL updated correctly)
- LEFT JOIN cross-DB with NULL handling (works)
- Multi-column UPDATE from cross-DB subquery (works)
- Kitchen sink constraints on attached DB (all enforced correctly)
- FK CASCADE 3 levels deep on attached DB (works)
- DDL after COMMIT on attached DB (works)
- .databases command with attached DBs (works)
- .indexes with attached DBs (works, shows both schemas)
- .schema with attached DBs (shows tables, Bug 22 confirmed for index prefix)
- .schema with table name argument (doesn't search attached DBs - consequence of Bug 9)
- Same-name index in main and attached (works, both accessible)
- Cross-DB UPDATE with same table name (works with 2-part names, Bug 10 with 3-part names)
- Complex cross-DB BETWEEN (works, matches sqlite3)
- DISTINCT on cross-DB UNION ALL (works)
- ORDER BY on subquery from attached DB (works)
- COLLATE NOCASE PK on attached DB (works)
- STRICT table violations on attached DB (all enforced correctly)
- Multi-row STRICT violation statement rollback (works correctly)
- Deferred FK + multi-DB writes + ROLLBACK (works correctly)
- query_only pragma on attached DB (correctly prevents writes)
- ignore_check_constraints on attached DB (works, matches sqlite3)
- INSERT ON CONFLICT DO NOTHING on attached (works)
- NULL handling in all column positions on attached (correct)
- ATTACH during explicit transaction + writes (works)
- ATTACH during transaction + ROLLBACK (ATTACH persists, DDL rolled back - correct)
- Bitmap allocation stress test (10 attach/detach cycles - correct)
- ATTACH with spaces in path (works)
- Complex INSERT SELECT with transformation cross-DB (works)
- BEFORE trigger with RAISE on attached DB (works)
- Large data (overflow pages) on attached DB (works)
- .mode list with attached DB data (works)
- Many rows with AUTOINCREMENT on attached (works)
- RETURNING with complex expressions on attached DB (works)
- ATTACH with relative file: URI (works)
- PRAGMA database_list with file paths (correct)
- ATTACH same file twice with writes (works, changes visible across aliases)

#### URI parameter tests:
- `file::memory:?cache=shared` (works)
- `file:...?mode=ro` (correctly prevents writes)
- `file:...?immutable=1` (BUG 33 - allows writes!)
- `file:...?mode=rw` to non-existent file (BUG 34 - creates file instead of failing)
- `file:` with empty path (BUG 35 - fails instead of creating in-memory DB)
- `file:?mode=memory` with empty path (BUG 35 - same issue)
- `file:...?nolock=1` (accepted, no visible effect)
- `file:...?vfs=memdb` (correctly errors - no such VFS)
- `file:...?unknown_param=value` (silently ignored)
- `file:...?psow=1` (accepted, no visible effect)
- `file:...?cipher=aes256gcm&hexkey=...` (works correctly)
- `file:...?modeof=...` (accepted)
- Read-only file (chmod 444) ATTACH (BUG 36 - fails instead of opening read-only)

#### Bugs Found (Round 10):
32. Generated columns feature flag not propagated to attached DB schema
33. `immutable=1` URI parameter ignored for ATTACH (allows writes)
34. `mode=rw` URI parameter creates non-existent files for ATTACH
35. `file:` URI with empty path fails in ATTACH (should create in-memory DB)
36. ATTACH on read-only file (chmod 444) fails instead of opening read-only
