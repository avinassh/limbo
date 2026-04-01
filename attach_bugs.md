# ATTACH Bugs Found

## Bug 1: CREATE INDEX on attached DB stores schema prefix in sqlite_master SQL

**Repro:**
```sql
ATTACH '/tmp/claude/test_attach.db' AS aux;
CREATE INDEX aux.idx_t2_data ON t2(data);
SELECT sql FROM aux.sqlite_master WHERE type='index';
```

**Expected (sqlite3 behavior):**
```
CREATE INDEX idx_t2_data ON t2(data)
```

**Actual (tursodb):**
```
CREATE INDEX aux.idx_t2_data ON t2 (data)
```

The stored SQL in `sqlite_master` should NOT include the schema prefix (`aux.`). This breaks compatibility because re-parsing the stored SQL would fail or behave differently. In SQLite, the schema qualifier is never stored in `sqlite_master.sql`.

## Bug 2: CREATE VIEW on attached DB fails to resolve unqualified table names in the target schema

**Repro:**
```sql
-- Setup: test_attach.db has table t2
-- main db does NOT have table t2
ATTACH '/tmp/claude/test_attach.db' AS aux;
CREATE VIEW aux.v_t2 AS SELECT id, data FROM t2 WHERE num > 100;
```

**Expected (sqlite3 behavior):**
View is created successfully. When creating a view in a schema (`aux`), unqualified table references should resolve against that schema first.

**Actual (tursodb):**
```
Parse error: no such table: t2
```

In SQLite, `CREATE VIEW aux.v_t2 AS SELECT id, data FROM t2 ...` resolves `t2` within the `aux` schema. Tursodb fails because it only looks up `t2` in the main schema.

## Bug 3: DETACH succeeds during active transaction (should fail with "database is locked")

**Repro:**
```sql
ATTACH '/tmp/claude/test_attach.db' AS aux;
BEGIN;
INSERT INTO aux.t2 VALUES(30, 'in_txn', 777, 'test');
DETACH aux;
-- No error! Data is silently lost.
```

**Expected (sqlite3 behavior):**
```
Error: database aux is locked
```

SQLite prevents DETACH while a transaction is active on the database. Tursodb silently allows the DETACH, which effectively rolls back the transaction and loses the inserted data without any error to the user.

## Bug 4: PANIC - INSERT OR REPLACE on attached DB with UNIQUE constraint causes crash

**Repro:**
```sql
-- Setup: uq_test.db has: CREATE TABLE uq(id INTEGER PRIMARY KEY, name TEXT UNIQUE);
-- with rows (1, 'alice'), (2, 'bob')
ATTACH '/tmp/claude/uq_test.db' AS aux;
INSERT OR REPLACE INTO aux.uq VALUES(3, 'bob');
```

**Expected (sqlite3 behavior):**
The existing row (2, 'bob') is deleted and replaced with (3, 'bob'). Table contains: (1, 'alice'), (3, 'bob').

**Actual (tursodb):**
```
thread 'main' panicked at core/translate/insert.rs:3691:14:
index to exist
```

Panic in `emit_replace_delete_conflicting_row` at `core/translate/insert.rs:3691`. The code expects to find the index for the UNIQUE constraint but fails when the table is in an attached database. The index lookup likely uses the wrong database ID (main instead of the attached DB's).

**Severity:** Critical - causes complete process crash.

## Bug 5: ROLLBACK TO SAVEPOINT does not undo writes on attached databases

**Repro:**
```sql
-- Setup: test_attach.db has table t2
ATTACH '/tmp/claude/test_attach.db' AS aux;
BEGIN;
INSERT INTO aux.t2 VALUES(301, 'before_sp', 0, 'x');
SAVEPOINT sp1;
INSERT INTO aux.t2 VALUES(302, 'in_sp', 0, 'x');
ROLLBACK TO sp1;
COMMIT;
SELECT * FROM aux.t2 WHERE id IN (301, 302);
```

**Expected (sqlite3 behavior):**
```
301|before_sp
```
Only row 301 should remain. Row 302 was inserted after the savepoint and should be rolled back.

**Actual (tursodb):**
```
301|before_sp|0|x
302|in_sp|0|x
```

Both rows are present. The `ROLLBACK TO sp1` did not undo the insert of row 302 in the attached database. This means savepoint-based rollbacks are completely broken for attached databases, which can lead to data corruption in any code relying on savepoints for error recovery on attached DBs.

**Severity:** Critical - silent data corruption (writes that should be rolled back are persisted).

## Bug 6: PRAGMA aux.integrity_check runs on main database instead of attached

**Repro:**
```sql
CREATE TABLE main_t(id INTEGER PRIMARY KEY, name TEXT);
INSERT INTO main_t VALUES(1,'a'),(2,'b');
ATTACH '/tmp/claude/test_multi_idx.db' AS aux;
PRAGMA aux.integrity_check;
```

**Expected (sqlite3 behavior):**
```
ok
```
Returns integrity check results for the `aux` database.

**Actual (tursodb):**
```
*** in database main ***
Page 3: never used
Page 4: never used
Page 5: never used
Page 6: never used
```

The pragma ignores the schema qualifier and checks `main` instead of `aux`. The output explicitly says "in database main". The same bug affects `PRAGMA aux.quick_check`.

**Root cause:** In `core/translate/pragma.rs`, the `schema` variable (line ~574) is always bound to `resolver.schema()`, which returns the main database schema. This `schema` is then passed to `translate_integrity_check()` / `translate_quick_check()`, so even though `database_id` is correctly resolved to the attached DB, the schema object still points to main.

## Bug 7: PRAGMA aux.index_list / aux.index_info / aux.index_xinfo return empty on attached DBs

**Repro:**
```sql
-- test_multi_idx.db has: CREATE TABLE multi(id, a TEXT, b INTEGER, c REAL)
-- with indexes: idx_a ON multi(a), idx_b ON multi(b), UNIQUE idx_ab ON multi(a,b)
ATTACH '/tmp/claude/test_multi_idx.db' AS aux;
PRAGMA aux.index_list(multi);
PRAGMA aux.index_info(idx_ab);
PRAGMA aux.index_xinfo(idx_ab);
```

**Expected (sqlite3 behavior):**
```
-- index_list:
0|idx_ab|1|c|0
1|idx_b|0|c|0
2|idx_a|0|c|0
-- index_info:
0|1|a
1|2|b
-- index_xinfo:
0|1|a|0|BINARY|1
1|2|b|0|BINARY|1
2|-1||0|BINARY|0
```

**Actual (tursodb):**
All three pragmas return empty results (no rows).

**Root cause:** Same as Bug 6 — `core/translate/pragma.rs` uses `schema` (main DB) for index lookups at lines ~780, ~813, ~867. The index/table lookup searches the wrong schema.

## Bug 8: PRAGMA aux.table_list shows main DB tables with hardcoded "main" schema name

**Repro:**
```sql
ATTACH '/tmp/claude/test_multi_idx.db' AS aux;
PRAGMA aux.table_list;
```

**Expected (sqlite3 behavior):**
```
aux|multi|table|4|0|0
aux|sqlite_schema|table|5|0|0
```

**Actual (tursodb):**
```
main|sqlite_schema|table|5|0|0
```

Two problems:
1. The schema name column is hardcoded to "main" (line 928 in `core/translate/pragma.rs`)
2. The table list reads from the main schema, so it only shows main DB's `sqlite_schema` and misses the `multi` table in the attached DB

**Root cause:** Line 928 in `core/translate/pragma.rs` hardcodes `"main"` as the schema name, and lines 939-986 iterate over `schema` (main DB) instead of the attached DB's schema.

## Bug 9: No limit on number of attached databases (should enforce max 10)

**Repro:**
```sql
-- Create 11 databases and attach them all
ATTACH '/tmp/claude/many_1.db' AS db1;
ATTACH '/tmp/claude/many_2.db' AS db2;
...
ATTACH '/tmp/claude/many_11.db' AS db11;
-- All succeed, no error
SELECT * FROM db11.t;
```

**Expected (sqlite3 behavior):**
```
Error: too many attached databases - max 10
```
SQLite enforces SQLITE_LIMIT_ATTACHED (default 10) and rejects the 11th ATTACH.

**Actual (tursodb):**
All 11 (and even 20+) ATTACH operations succeed without any error. There is no limit enforcement.

**Severity:** Medium - potential resource exhaustion. Without a limit, a malicious or buggy application could exhaust memory by attaching an unbounded number of databases.

## Bug 10: ALTER TABLE ADD COLUMN type validation on attached DB reads wrong pager (I/O error)

**Repro:**
```sql
-- Setup: test_strict_alt.db has:
-- CREATE TABLE strict_alt(id INTEGER PRIMARY KEY, name TEXT NOT NULL) STRICT;
-- with rows (1,'alice'), (2,'bob')
ATTACH '/tmp/claude/test_strict_alt.db' AS aux;
ALTER TABLE aux.strict_alt ADD COLUMN num INTEGER NOT NULL DEFAULT 'not_int';
```

**Expected (sqlite3 behavior):**
```
Error: type mismatch on DEFAULT
```
The ALTER TABLE correctly detects that the default value 'not_int' is not compatible with INTEGER type on a STRICT table.

**Actual (tursodb):**
```
ERROR turso_core::storage::sqlite3_ondisk: short read on page 2: expected 4096 bytes, got 0
Error: I/O error: short read on page 2: expected 4096 bytes, got 0
```

Instead of a clean type-mismatch error, tursodb crashes with an I/O error because it reads from the wrong pager. The same ALTER TABLE works correctly on main DB tables.

**Root cause:** In `core/translate/alter.rs` line 306, `emit_add_column_default_type_validation()` hardcodes `db: crate::MAIN_DB_ID` in the `OpenRead` instruction. When the target table is in an attached database, this opens the wrong pager (main's) with the attached table's root page number, causing a page read from the wrong file.

**Severity:** High - prevents valid DDL operations on attached databases and produces misleading error messages.

## Bug 11: Querying pre-existing views on attached databases fails

**Repro:**
```sql
-- Setup with sqlite3:
-- CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT);
-- INSERT INTO t VALUES(1,'a'),(2,'b');
-- CREATE VIEW v AS SELECT * FROM t;
ATTACH '/tmp/claude/test_view2.db' AS vdb;
SELECT * FROM vdb.v;
```

**Expected (sqlite3 behavior):**
```
1|a
2|b
```
The view's SQL references `t` unqualified. When the view is in an attached schema, `t` should resolve within that schema.

**Actual (tursodb):**
```
Parse error: no such table: t
```

When tursodb re-parses the view's stored SQL (`SELECT * FROM t`), it resolves `t` only in the main schema. Since `t` doesn't exist in main, the query fails. This means ALL views in attached databases with unqualified table references are completely broken. This is related to Bug 2 (CREATE VIEW fails) but distinct: here the view already exists in the attached file and still can't be queried.

**Severity:** Critical - makes any attached database containing views unusable.

## Bug 12: Unqualified table names do not fall back to attached databases

**Repro:**
```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.only_in_mem(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO only_in_mem VALUES(1, 'test');
-- Also affects SELECT:
SELECT * FROM only_in_mem;
```

**Expected (sqlite3 behavior):**
SQLite searches main first, then attached databases in ATTACH order. Since `only_in_mem` exists only in `mem`, it should be found there.
```
1|test
```

**Actual (tursodb):**
```
Parse error: no such table: only_in_mem
```

Tursodb only searches the main schema for unqualified table names. It never falls back to attached databases. In SQLite, the resolution order is: main → temp → attached DBs (in attach order).

**Severity:** High - forces users to always use schema-qualified names with attached databases, breaking SQLite compatibility and making many existing applications unusable.

## Bug 13: Schema-qualified column references (schema.table.column) resolve to wrong table in cross-DB JOINs

**Repro:**
```sql
CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO t VALUES(1,'MAIN');
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO mem.t VALUES(1,'MEM');
SELECT main.t.val, mem.t.val FROM main.t, mem.t WHERE main.t.id = mem.t.id;
```

**Expected (sqlite3 behavior):**
```
MAIN|MEM
```

**Actual (tursodb):**
```
MAIN|MAIN
```

When using `schema.table.column` syntax (e.g., `mem.t.val`), the column reference incorrectly resolves to the main database's table instead of the attached database's table. `mem.t.val` returns `MAIN` instead of `MEM`. This only happens when both databases have a table with the same name. Using aliases (`FROM main.t a, mem.t b`) works correctly as a workaround.

**Root cause:** The three-part column reference `mem.t.val` is likely being parsed as `table.column` with `mem` interpreted as a table alias, ignoring the schema qualifier. The resolver then matches the first `t` it finds (main's).

**Severity:** High - produces silently wrong query results, which is the worst category of bug.

## Bug 14: Query optimizer does not use indexes on attached databases

**Repro:**
```sql
-- Setup: test_base.db has table items with CREATE INDEX idx_items_cat ON items(category)
ATTACH '/tmp/claude/test_base.db' AS aux;
EXPLAIN QUERY PLAN SELECT * FROM aux.items WHERE category = 'fruit';
```

**Expected (sqlite3 behavior):**
```
QUERY PLAN
`--SEARCH aux.items USING INDEX idx_items_cat (category=?)
```

**Actual (tursodb):**
```
QUERY PLAN
`--SCAN items
```

The optimizer performs a full table scan instead of using the available index on `category`. The same query on a main-database table with the same index correctly uses `SEARCH ... USING INDEX`. Verified via `EXPLAIN` bytecode: no `OpenRead` for the index page is emitted.

The indexes are visible in `aux.sqlite_master` and `aux.sqlite_stat1` is readable, so the schema is loaded - but the optimizer's index selection logic doesn't consider indexes from attached databases.

**Severity:** High - severe performance degradation for any query on indexed attached tables. Queries that should be O(log n) become O(n).

## Bug 15: Unqualified DROP INDEX / DROP TRIGGER fails to search attached databases

**Repro:**
```sql
-- DROP INDEX:
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY, val TEXT);
CREATE INDEX mem.idx_t_val ON t(val);
DROP INDEX idx_t_val;  -- ERROR: No such index: idx_t_val

-- DROP TRIGGER:
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY, val TEXT);
CREATE TABLE mem.log(id INTEGER PRIMARY KEY, msg TEXT);
CREATE TRIGGER mem.trg AFTER INSERT ON t BEGIN INSERT INTO log(msg) VALUES('x'); END;
DROP TRIGGER trg;  -- ERROR: no such trigger: trg
```

**Expected (sqlite3 behavior):**
Both succeed. SQLite searches main, then temp, then attached databases when resolving unqualified index/trigger names.

**Actual (tursodb):**
```
-- DROP INDEX:
Invalid argument supplied: No such index: idx_t_val

-- DROP TRIGGER:
Parse error: no such trigger: trg
```

Tursodb only searches the main schema for unqualified DROP INDEX and DROP TRIGGER. This is the same class of bug as Bug 12 (unqualified table names) but affects DDL operations. Using schema-qualified names (`DROP INDEX mem.idx_t_val`) works as a workaround.

**Severity:** Medium - broken for unqualified names, but schema-qualified workaround exists.

## Bug 16: ATTACH NULL fails instead of creating an in-memory database

**Repro:**
```sql
ATTACH NULL AS aux;
CREATE TABLE aux.t(id INTEGER PRIMARY KEY);
INSERT INTO aux.t VALUES(1);
SELECT * FROM aux.t;
```

**Expected (sqlite3 behavior):**
```
1
```
In SQLite, `ATTACH NULL AS aux` creates a temporary in-memory database (equivalent to `ATTACH '' AS aux`).

**Actual (tursodb):**
```
Error: Invalid argument supplied: attach: filename argument must be text
```

Tursodb rejects the NULL argument because it checks for TEXT type explicitly. SQLite treats NULL, empty string, and ':memory:' all as in-memory database specifications.

**Severity:** Medium - breaks compatibility with applications that use `ATTACH NULL`.

## Bug 17: Schema-qualified names with SQL keywords as schema name fail to parse

**Repro:**
```sql
ATTACH ':memory:' AS "select";
CREATE TABLE "select".t(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO "select".t VALUES(1, 'test');
SELECT * FROM "select".t;
```

**Expected (sqlite3 behavior):**
```
1|test
```
SQLite correctly handles double-quoted SQL keywords as schema names in schema-qualified references.

**Actual (tursodb):**
```
Error: unexpected token 'select' at offset 14
Parse error: no such table: t
Parse error: no such table: t
```

The ATTACH itself succeeds (the schema name is registered), but subsequent use of the double-quoted keyword as a schema qualifier in `"select".t` fails. The parser doesn't correctly handle quoted SQL keywords in the `schema.table` position. Backtick and bracket quoting also fail for keyword schema names in this position.

**Root cause:** The parser's schema-qualified name resolution treats quoted keywords specially in the schema position. It correctly handles `"select"` as a table name (`CREATE TABLE "select"(...)` works) but fails when used as a schema qualifier before the dot.

**Severity:** Medium - prevents using reserved words as attached database names, which is a valid use case in SQLite.

## Bug 18: ATTACH of a 0-byte (empty) file causes hang (infinite loop)

**Repro:**
```bash
# Create an empty file
> /tmp/empty.db
# Try to attach it
echo "ATTACH '/tmp/empty.db' AS aux;" | tursodb --experimental-attach -q
# Process hangs indefinitely
```

**Expected (sqlite3 behavior):**
SQLite correctly handles attaching a 0-byte file. It treats it as an empty database and allows creating tables and inserting data.

**Actual (tursodb):**
The process enters an infinite loop (or deadlock) and never returns. It must be killed externally.

**Root cause:** When opening a 0-byte file, the database header read gets 0 bytes. The code likely enters a retry loop or fails to handle the case where the database file exists but has no content (unlike a non-existent file, which creates a new database).

**Severity:** Critical - process hang/freeze that requires external kill. Any application that attaches a file that was truncated or incompletely written will hang.

## Bug 19: BEGIN IMMEDIATE/EXCLUSIVE does not acquire locks on attached databases

**Repro:**
```sql
CREATE TABLE m(id INTEGER PRIMARY KEY);
ATTACH ':memory:' AS aux;
CREATE TABLE aux.a(id INTEGER PRIMARY KEY);
EXPLAIN BEGIN IMMEDIATE;
```

**Expected (sqlite3 behavior):**
```
Transaction    0     1     0    -- main, write lock
Transaction    1     1     0    -- temp, write lock
Transaction    2     1     0    -- aux, write lock
AutoCommit     0     0     0
```
SQLite emits Transaction instructions for ALL databases (main, temp, all attached) during `BEGIN IMMEDIATE` and `BEGIN EXCLUSIVE`.

**Actual (tursodb):**
```
Transaction    0     2     1    -- main only, write lock
AutoCommit     0     0     0
```
Tursodb only emits a Transaction instruction for the main database. Attached databases are not locked.

**Root cause:** In `core/translate/transaction.rs` lines 30-31 and 42-43, the `translate_begin` function only emits a Transaction instruction with `MAIN_DB_ID`. It does not iterate over attached databases to emit Transaction instructions for them.

**Severity:** High - breaks the semantics of `BEGIN IMMEDIATE` which guarantees that after it succeeds, no other connection can write to any of the connection's databases. Without locking attached databases, concurrent writers could cause conflicts. Also affects `BEGIN EXCLUSIVE`.

## Bug 20: DML on attached database unnecessarily opens WRITE transaction on main database

**Repro:**
```sql
ATTACH ':memory:' AS aux;
CREATE TABLE aux.t(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO aux.t VALUES(1,'a'),(2,'b');
EXPLAIN DELETE FROM aux.t WHERE id = 1;
```

**Expected (sqlite3 behavior):**
```
Transaction    2     1     1    -- ONLY aux needs write transaction
```
SQLite only emits a Transaction instruction for the database being modified.

**Actual (tursodb):**
```
Transaction    0     2     0    -- main gets WRITE transaction (unnecessary!)
Transaction    2     2     1    -- aux gets write transaction (correct)
```
Tursodb emits a WRITE transaction on the main database even when only the attached database is being modified.

**Root cause:** The write transaction emission logic always includes a write Transaction for `MAIN_DB_ID` regardless of which database is actually being modified.

**Severity:** Medium - causes unnecessary lock contention on the main database. Operations on attached databases that should be independent of main will hold a write lock on main, preventing concurrent readers/writers on main. Could cause `SQLITE_BUSY` errors in multi-connection scenarios that wouldn't occur in sqlite3.

## Bug 21: DROP TABLE on attached DB leaks index B-tree pages

**Repro:**
```sql
-- Setup with sqlite3:
-- CREATE TABLE t(id INTEGER PRIMARY KEY, a TEXT, b INTEGER);
-- CREATE INDEX idx_a ON t(a);
-- CREATE INDEX idx_b ON t(b);
-- INSERT INTO t VALUES(1,'x',10),(2,'y',20),(3,'z',30);
-- Before: page_count=4, freelist_count=0
ATTACH '/tmp/test_drop_idx.db' AS aux;
DROP TABLE aux.t;
-- After: check with sqlite3
```

**Expected (sqlite3 behavior):**
```
page_count: 4
freelist_count: 3
integrity_check: ok
```
All 3 data pages (1 table + 2 indexes) are freed and added to the freelist.

**Actual (tursodb):**
```
page_count: 4
freelist_count: 1
*** in database main ***
Page 3: never used
Page 4: never used
```
Only 1 page is freed (the table's B-tree root). The 2 index B-tree pages are leaked — they're marked as "never used" by integrity_check but are not on the freelist. Verified via `EXPLAIN DROP TABLE`: tursodb emits only ONE `Destroy` instruction (for the table), while sqlite3 emits TWO `Destroy` instructions (one for the table, one for the index).

**Root cause:** In `core/translate/schema.rs:1607`, `resolver.schema().get_indices(tbl_name)` returns the main DB's schema indices. For an attached DB table, this returns an empty list, so no index B-trees are destroyed.

**Severity:** High - causes silent storage leak. Each DROP TABLE on an attached DB with indexes permanently leaks pages. Over time, the database file grows without bound. These pages can never be reclaimed without VACUUM (which isn't supported for attached DBs).

## Bug 22: PRAGMA cache_size ignores schema qualifier (operates on main DB)

**Repro:**
```sql
ATTACH ':memory:' AS mem;
PRAGMA main.cache_size = 1000;
PRAGMA mem.cache_size = 2000;
PRAGMA main.cache_size;
PRAGMA mem.cache_size;
```

**Expected (sqlite3 behavior):**
```
1000
2000
```
Each database has its own independent cache_size setting.

**Actual (tursodb):**
```
2000
2000
```
Setting `mem.cache_size = 2000` overwrites `main.cache_size` from 1000 to 2000. The schema qualifier is completely ignored — both read and write operations on `PRAGMA mem.cache_size` actually operate on the main database's cache_size.

**Root cause:** The PRAGMA implementation for `cache_size` does not resolve the schema qualifier to the correct database. It always reads/writes the main database's cache configuration.

**Severity:** Medium - prevents per-database cache tuning. Applications that set different cache sizes for main vs attached databases will silently misconfigure the cache.

## Bug 23: PRAGMA freelist_count ignores schema qualifier (returns main's value)

**Repro:**
```sql
-- Setup: test.db has 1 free page (created table, inserted data, dropped table)
-- Main DB has 0 free pages
ATTACH '/tmp/test_fl.db' AS aux;
PRAGMA main.freelist_count;
PRAGMA aux.freelist_count;
```

**Expected (sqlite3 behavior):**
```
0
1
```
Each database reports its own freelist count independently.

**Actual (tursodb):**
```
0
0
```
`PRAGMA aux.freelist_count` returns 0 (main's value) instead of 1 (aux's actual freelist count). The schema qualifier is ignored.

**Root cause:** Same pattern as Bug 22 — the PRAGMA implementation reads from the main database's pager regardless of the schema qualifier.

**Severity:** Medium - prevents accurate space accounting for attached databases. Applications monitoring database size/fragmentation will get wrong information for attached DBs.

## Bug 24: EXPLAIN QUERY PLAN does not show schema name prefix for attached DB tables

**Repro:**
```sql
ATTACH '/tmp/test.db' AS aux;
EXPLAIN QUERY PLAN SELECT * FROM aux.t;
-- In a cross-DB join:
EXPLAIN QUERY PLAN SELECT * FROM m JOIN aux.t ON m.id = aux.t.id;
```

**Expected (sqlite3 behavior):**
```
QUERY PLAN
`--SCAN aux.t

QUERY PLAN
|--SCAN m
`--SEARCH aux.t USING INTEGER PRIMARY KEY (rowid=?)
```
sqlite3 prefixes attached table names with the schema name in EQP output.

**Actual (tursodb):**
```
QUERY PLAN
`--SCAN t

QUERY PLAN
|--SCAN m
`--SEARCH t USING INTEGER PRIMARY KEY (rowid=?)
```
The schema name prefix is missing. Table names are shown without qualification.

**Severity:** Low - diagnostic/informational issue. Makes it impossible to determine which database a table scan refers to when same-name tables exist in multiple schemas. Breaks compatibility with tools that parse EQP output.

## Bug 25: DROP TABLE on attached DB does not clean up sqlite_sequence entry (AUTOINCREMENT)

**Repro:**
```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY AUTOINCREMENT, val TEXT);
INSERT INTO mem.t(val) VALUES('a');
SELECT * FROM mem.sqlite_sequence;
-- Shows: t|1
DROP TABLE mem.t;
SELECT * FROM mem.sqlite_sequence;
```

**Expected (sqlite3 behavior):**
```
-- After DROP TABLE:
(empty - no rows)
```
sqlite3 removes the corresponding entry from `sqlite_sequence` when dropping an AUTOINCREMENT table.

**Actual (tursodb):**
```
-- After DROP TABLE:
t|1
```
The `t|1` entry persists in `sqlite_sequence` after the table is dropped. On the main database, tursodb correctly removes the entry — this bug is specific to attached databases.

**Root cause:** Related to Bug 21's root cause — `core/translate/schema.rs` uses `resolver.schema()` (main DB schema) to look up the sqlite_sequence table. Since the sequence table is in the attached DB's schema, it isn't found, and the cleanup code is skipped.

**Severity:** Medium - stale entries in `sqlite_sequence` accumulate over repeated CREATE/DROP TABLE cycles on attached databases. While this doesn't cause incorrect behavior for new tables (AUTOINCREMENT checks MAX(rowid) as well), it wastes space and could cause confusion when inspecting `sqlite_sequence`.

## Bug 26: Schema-qualified column references in WHERE clause with cartesian product give "ambiguous column name" error

**Repro:**
```sql
CREATE TABLE m(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO m VALUES(1, 'MAIN');
ATTACH ':memory:' AS mem;
CREATE TABLE mem.m(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO mem.m VALUES(1, 'MEM');
SELECT * FROM main.m, mem.m WHERE main.m.id = mem.m.id;
```

**Expected (sqlite3 behavior):**
```
1|MAIN|1|MEM
```
sqlite3 correctly uses the schema qualifier (`main.m.id`, `mem.m.id`) to disambiguate same-named tables in a cartesian product.

**Actual (tursodb):**
```
Parse error: ambiguous column name: m.id
```

The parser/resolver treats `main.m.id` and `mem.m.id` as just `m.id`, ignoring the schema qualifier in the WHERE clause. This only happens with comma-join (cartesian product) syntax. Using explicit `JOIN ... ON` doesn't give the ambiguous error, but instead silently resolves to the wrong table (Bug 13). Using aliases (`FROM main.m a, mem.m b WHERE a.id = b.id`) works correctly.

**Root cause:** The three-part name `schema.table.column` in WHERE clause is not properly parsed or resolved when used with comma-join syntax and same-named tables across databases.

**Severity:** High - prevents valid cross-database queries that work correctly in sqlite3.

## Bug 27: PRAGMA synchronous ignores schema qualifier (reads/writes main DB instead of specified DB)

**Repro:**
```sql
ATTACH ':memory:' AS mem;
PRAGMA main.synchronous;   -- Returns 2
PRAGMA mem.synchronous;    -- Returns 2
PRAGMA mem.synchronous = 0;
PRAGMA mem.synchronous;    -- Returns 0 (correct)
PRAGMA main.synchronous;   -- Returns 0 (WRONG - should still be 2)
```

**Expected (sqlite3 behavior):**
```
2
2
-- after set mem to 0:
0
2
```
In sqlite3, `PRAGMA mem.synchronous = 0` only affects the `mem` database. `main.synchronous` remains at 2.

**Actual (tursodb):**
```
2
2
-- after set mem to 0:
0
0
```
Setting `mem.synchronous = 0` also changes `main.synchronous` to 0. The schema qualifier is completely ignored for both reading and writing.

**Root cause:** In `core/translate/pragma.rs`, the synchronous PRAGMA uses `connection.set_sync_mode()` and `connection.get_sync_mode()`, which operate on a connection-wide setting rather than per-pager/per-database setting. SQLite stores synchronous mode per-database (in the pager), not per-connection.

**Severity:** High - can cause data loss. If an application sets `PRAGMA mem.synchronous = OFF` for a temporary attached DB (for performance), this silently disables synchronous writes on the main database too, risking data corruption on crash.

## Bug 28: INDEXED BY clause fails on attached DB with "no such index"

**Repro:**
```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY, val TEXT, num INTEGER);
INSERT INTO mem.t VALUES(1,'a',10),(2,'b',20),(3,'c',30);
CREATE INDEX mem.idx_val ON t(val);
SELECT * FROM mem.t INDEXED BY idx_val WHERE val = 'b';
```

**Expected (sqlite3 behavior):**
```
2|b|20
```
sqlite3 correctly finds the index `idx_val` in the attached database and uses it.

**Actual (tursodb):**
```
Parse error: no such index: idx_val
```

The `INDEXED BY` clause cannot find indexes that exist in attached databases. It only searches the main database's schema. The index clearly exists (visible in `mem.sqlite_master`), but the INDEXED BY name resolution doesn't search attached schemas.

**Root cause:** The INDEXED BY resolution logic in the query planner only searches the main database schema for the named index. It does not consider the database context of the table being queried.

**Severity:** Medium - prevents using `INDEXED BY` hints on attached database tables. Applications that rely on `INDEXED BY` for query plan stability will fail when using attached databases. Also affects `NOT INDEXED` (which works correctly, interestingly).

## Bug 29: Unqualified ANALYZE only analyzes main database, not attached databases

**Repro:**
```sql
CREATE TABLE main.mt(id INTEGER PRIMARY KEY, val TEXT);
CREATE INDEX main.idx_mt ON mt(val);
INSERT INTO main.mt VALUES(1,'a'),(2,'b'),(3,'a');

ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY, val TEXT);
CREATE INDEX mem.idx_t ON t(val);
INSERT INTO mem.t VALUES(1,'x'),(2,'y'),(3,'x');

ANALYZE;

SELECT * FROM main.sqlite_stat1;   -- Returns data
SELECT * FROM mem.sqlite_stat1;    -- ERROR: no such table
```

**Expected (sqlite3 behavior):**
```
-- main.sqlite_stat1:
mt|idx_mt|3 2
-- mem.sqlite_stat1:
t|idx_t|3 2
```
In sqlite3, unqualified `ANALYZE` analyzes ALL databases (main + all attached databases).

**Actual (tursodb):**
```
-- main.sqlite_stat1:
mt||3
mt|idx_mt|3 2
-- mem.sqlite_stat1:
Parse error: no such table: sqlite_stat1
```

The `sqlite_stat1` table is never created in the attached database. Only main is analyzed.

Note: `ANALYZE mem;` (qualified) works correctly and creates `mem.sqlite_stat1`. The bug is specifically with the unqualified `ANALYZE` command.

**Root cause:** The `translate_analyze` function in `core/translate/analyze.rs` likely only iterates over the main database's tables when no schema qualifier is provided, instead of iterating over all attached databases as well.

**Severity:** Medium - applications that run `ANALYZE;` to update statistics will only update the main database. Attached database queries will have suboptimal query plans due to missing statistics. Combined with Bug 14 (optimizer doesn't use indexes on attached DBs), this means attached database performance is doubly penalized.

## Bug 30: ALTER TABLE ADD COLUMN default type validation reads wrong pager on ALL attached DBs (not just file-based)

**Repro:**
```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.strict_t(id INTEGER PRIMARY KEY, name TEXT NOT NULL) STRICT;
INSERT INTO mem.strict_t VALUES(1, 'alice');
ALTER TABLE mem.strict_t ADD COLUMN num INTEGER NOT NULL DEFAULT 'not_int';
```

**Expected (sqlite3 behavior):**
```
Error: type mismatch on DEFAULT
```
Clean type-mismatch error because the DEFAULT value 'not_int' is incompatible with INTEGER on a STRICT table.

**Actual (tursodb):**
```
ERROR turso_core::storage::sqlite3_ondisk: short read on page 2: expected 4096 bytes, got 0
Error: I/O error: short read on page 2: expected 4096 bytes, got 0
```

Instead of a clean type-mismatch error, tursodb crashes with an I/O error. This was documented as Bug 10 for file-based attached databases, but this repro shows it also affects **in-memory** attached databases. The root cause is the same: `core/translate/alter.rs` line 306 hardcodes `db: crate::MAIN_DB_ID` in the OpenRead instruction for default type validation, so it reads from the main database's pager at the attached table's root page number.

For in-memory attached DBs, the main DB often has fewer pages, so reading the attached table's root page number from main's pager returns 0 bytes, causing the "short read" error. For file-based attached DBs, it might read garbage data from a different page.

**Root cause:** Same as Bug 10 — `core/translate/alter.rs:306` hardcodes `MAIN_DB_ID`. This confirms Bug 10 affects ALL attached databases, not just file-based ones.

**Severity:** High - prevents ALTER TABLE ADD COLUMN with type-checked defaults on STRICT tables in any attached database. The I/O error is particularly confusing for in-memory databases where there are no actual I/O operations.

## Bug 31: WAL not checkpointed on DETACH for attached databases

**Repro:**
```sql
-- Setup: create an attached file-based DB, write data, then DETACH
ATTACH '/tmp/test_wal.db' AS aux;
CREATE TABLE aux.t(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO aux.t VALUES(1,'a'),(2,'b');
DETACH aux;
-- Check files:
-- ls -la /tmp/test_wal.db*
```

**Expected (sqlite3 behavior):**
After DETACH, the WAL file is checkpointed and removed (or truncated to 0 bytes). Only the main .db file remains:
```
-rw-r--r--  8192  /tmp/test_wal.db
```

**Actual (tursodb):**
```
-rw-rw-r--  4096  /tmp/test_wal.db
-rw-rw-r-- 20632  /tmp/test_wal.db-wal
```
The WAL file remains with significant content (20KB) after DETACH. The same issue occurs on process exit without explicit DETACH. In contrast, the main database's WAL IS properly checkpointed on exit (0 bytes).

Even explicit `PRAGMA aux.wal_checkpoint(FULL)` before DETACH doesn't remove the WAL file - the checkpoint succeeds (pages are moved to the main file) but the WAL file is not truncated or deleted.

**Root cause:** The `detach_database()` function in `core/connection.rs` does not perform a WAL checkpoint before closing the attached database's pager. sqlite3 performs a passive checkpoint on DETACH.

**Severity:** Medium - While the WAL file can be replayed by the next connection, it wastes disk space and can confuse backup tools. In production, large WAL files from many attach/detach cycles could consume significant storage.

## Bug 32: CREATE INDEX on attached DB produces database files unreadable by sqlite3

**Repro:**
```sql
ATTACH '/tmp/test_corrupt.db' AS aux;
CREATE TABLE aux.t(id INTEGER PRIMARY KEY, val TEXT);
CREATE INDEX aux.idx ON t(val);
INSERT INTO aux.t VALUES(1,'a'),(2,'b');
DETACH aux;
-- Then open with sqlite3:
-- sqlite3 /tmp/test_corrupt.db "SELECT * FROM t;"
```

**Expected (sqlite3 behavior):**
The database file should be readable by sqlite3 with no issues.

**Actual (tursodb):**
```
Error: malformed database schema (idx) - corrupt database (11)
```

sqlite3 cannot open the database at all. The dump shows:
```
CORRUPTION ERROR
malformed database schema (idx) - corrupt database
ERROR: near "ORDER": syntax error
```

**Root cause:** This is a critical escalation of Bug 1. The CREATE INDEX stores `CREATE INDEX aux.idx ON t (val)` in sqlite_master's SQL column (with the schema prefix `aux.`). When sqlite3 reads this back as the main database, it tries to parse `CREATE INDEX aux.idx ON t(val)` which is invalid SQL in the main schema context (there is no `aux` schema). This causes sqlite3 to reject the entire database as corrupt.

Tables, triggers, and views do NOT have this problem - only CREATE INDEX stores the schema prefix. This means any tursodb-created attached database that contains indexes is permanently unreadable by sqlite3 until the schema is manually repaired.

**Severity:** Critical - produces corrupt/incompatible database files that cannot be read by sqlite3 or any standard SQLite tooling. This is a data compatibility issue that could affect all users of attached databases.

## Bug 33: `.schema` command adds extra schema prefix to table name in CREATE INDEX for attached databases

**Repro:**
```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY, val TEXT);
CREATE INDEX mem.idx ON t(val);
-- Check stored SQL:
SELECT sql FROM mem.sqlite_master WHERE type='index';
-- Shows: CREATE INDEX mem.idx ON t (val)
-- Check .schema display:
.schema
```

**Expected (sqlite3 behavior):**
```
CREATE TABLE mem.t(id INTEGER PRIMARY KEY, val TEXT);
CREATE INDEX mem.idx ON t(val);
```
sqlite3's `.schema` prefixes the index name with the schema (`mem.idx`) for display but does NOT prefix the table name.

**Actual (tursodb):**
```
CREATE TABLE mem.t (id INTEGER PRIMARY KEY, val TEXT);
CREATE INDEX mem.idx ON mem.t (val);
```
The `.schema` output shows `ON mem.t` instead of `ON t`. This is a separate bug from Bug 1 (which is about the stored SQL in sqlite_master). The stored SQL has `CREATE INDEX mem.idx ON t (val)` (schema prefix only on index name), but `.schema` adds an additional `mem.` prefix to the table name in the ON clause.

**Root cause:** The `.schema` rendering logic adds the schema prefix to the table name in the CREATE INDEX statement's ON clause, even though the stored SQL doesn't have it.

**Severity:** Low - cosmetic issue in `.schema` output, but could confuse users who copy-paste the output to recreate indexes.

## Bug 34: Reading sqlite_master from empty (freshly attached) database causes I/O error

**Repro:**
```sql
ATTACH ':memory:' AS mem;
-- Don't create any tables, just query sqlite_master
SELECT count(*) FROM mem.sqlite_master;
```

**Expected (sqlite3 behavior):**
```
0
```
sqlite3 correctly handles reading sqlite_master from an empty database and returns 0 rows.

**Actual (tursodb):**
```
ERROR turso_core::storage::sqlite3_ondisk: short read on page 1: expected 4096 bytes, got 0
Error: I/O error: short read on page 1: expected 4096 bytes, got 0
```

The error occurs because an empty database (whether in-memory or file-based) doesn't have page 1 allocated yet. The `count(*)` query requires reading the sqlite_master B-tree root page (page 1), which doesn't exist.

Note: `SELECT * FROM mem.sqlite_master` and `SELECT name FROM mem.sqlite_master WHERE type='table'` return empty results without error, suggesting they use a different code path that handles the empty database case. Only `count(*)` triggers the I/O error because it uses the `Count` opcode which tries to directly access the page.

**Root cause:** The `Count` opcode in the VDBE executor doesn't handle the case where the table's root page doesn't exist in an empty database.

**Severity:** Medium - prevents basic schema introspection on freshly attached databases. Applications that check `count(*) FROM schema_table` before performing operations will crash.

## Bug 35: `file:` URI `mode=memory` parameter ignored for ATTACH (creates file-based DB instead of in-memory)

**Repro:**
```sql
ATTACH 'file:mydb?mode=memory' AS mdb;
CREATE TABLE mdb.t(id INTEGER PRIMARY KEY);
INSERT INTO mdb.t VALUES(1);
PRAGMA database_list;
```

**Expected (sqlite3 behavior):**
```
0|main|
2|mdb|
```
The `file` column is empty because `mode=memory` creates an in-memory database. No files are created on disk.

**Actual (tursodb):**
```
0|main|
2|mdb|/path/to/cwd/mydb
```
The `file` column shows a file path, and actual files are created on disk:
```
-rw-rw-r--  4096  mydb
-rw-rw-r-- 12392  mydb-wal
```

The `mode=memory` URI parameter is ignored. Instead of creating an in-memory database, tursodb creates a file-based database using the URI name ("mydb") as the filename in the current working directory.

**Root cause:** The URI parameter parsing in `core/connection.rs` likely doesn't handle the `mode=memory` query parameter for the `file:` URI scheme. The filename extraction strips the `file:` prefix and query parameters but doesn't check for `mode=memory` to redirect to in-memory storage.

**Severity:** High - applications that use `file:` URIs with `mode=memory` for temporary in-memory databases will unexpectedly create files on disk, potentially causing:
1. Unexpected disk writes in production environments
2. Data persistence when temporary data was expected
3. Disk space usage where none was intended
4. Files left behind in the working directory

