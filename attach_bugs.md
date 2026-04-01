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

