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
