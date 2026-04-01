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
