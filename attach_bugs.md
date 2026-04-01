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

