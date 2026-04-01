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

### Not Tested (features not yet supported)
- VACUUM on attached DB ("not supported yet" error)
- REINDEX ("not supported yet" error)
- CREATE TABLE AS SELECT ("not supported" error)
