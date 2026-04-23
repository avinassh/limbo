# Unrelated Bugs Found (not ATTACH-specific)

## 1. PRAGMA table_info on VIEW returns wrong column types

**Repro:**
```sql
CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, age INTEGER);
CREATE VIEW v AS SELECT id, name FROM t;
PRAGMA table_info(v);
```

**Expected (sqlite3):**
```
0|id|INTEGER|0||0
1|name|TEXT|0||0
```

**Actual (tursodb):**
```
0|id|TEXT|0||0
1|name|TEXT|0||0
```

All columns in views are reported as `TEXT` regardless of their actual type in the underlying table. This affects both main and attached database views.

## 2. ANALYZE produces extra table-level statistics row

**Repro:**
```sql
CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT);
CREATE INDEX idx ON t(val);
INSERT INTO t VALUES(1,'a'),(2,'b'),(3,'c'),(4,'d'),(5,'e');
ANALYZE;
SELECT * FROM sqlite_stat1 ORDER BY tbl, idx;
```

**Expected (sqlite3):**
```
t|idx|5 1
```

**Actual (tursodb):**
```
t||5
t|idx|5 1
```

Tursodb produces an extra row `t||5` with empty index name containing just the table row count. While this is valid sqlite_stat1 format, sqlite3's ANALYZE does not produce this row, causing a compatibility difference. The extra row is accepted by sqlite3 when reading back and doesn't cause issues.

## 3. CREATE VIEW IF NOT EXISTS errors when view already exists

**Repro:**
```sql
CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT);
CREATE VIEW v AS SELECT * FROM t;
CREATE VIEW IF NOT EXISTS v AS SELECT id FROM t;
```

**Expected (sqlite3 behavior):**
No error - `IF NOT EXISTS` makes it a no-op when the view already exists.

**Actual (tursodb):**
```
Parse error: view v already exists
```

The `if_not_exists` flag in the `CreateView` AST variant is ignored. In `core/translate/mod.rs` line 239-244, the `CreateView` is destructured with `..` which silently discards the `if_not_exists: bool` field. The `translate_create_view` function in `core/translate/view.rs` never checks this flag before checking if the view already exists (lines 295-306).

This bug affects BOTH main and attached databases - it is not ATTACH-specific.

## 4. DROP VIEW IF EXISTS on a TABLE silently does nothing (should error)

**Repro:**
```sql
CREATE TABLE t(id INTEGER PRIMARY KEY);
DROP VIEW IF EXISTS t;
```

**Expected (sqlite3 behavior):**
```
Parse error: use DROP TABLE to delete table t
```
sqlite3 errors because `t` exists as a TABLE, not a VIEW. The `IF EXISTS` only suppresses the error when the object doesn't exist at all, not when it exists as a different type.

**Actual (tursodb):**
No error, silently does nothing. This affects both main and attached databases.

## 5. `.schema` command doesn't display triggers

**Repro:**
```sql
CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT);
CREATE TABLE log(id INTEGER PRIMARY KEY, msg TEXT);
CREATE TRIGGER trg AFTER INSERT ON t BEGIN INSERT INTO log(msg) VALUES(NEW.val); END;
.schema
```

**Expected (sqlite3 behavior):**
```
CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT);
CREATE TABLE log(id INTEGER PRIMARY KEY, msg TEXT);
CREATE TRIGGER trg AFTER INSERT ON t BEGIN INSERT INTO log(msg) VALUES(NEW.val); END;
```

**Actual (tursodb):**
```
CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT);
CREATE TABLE log (id INTEGER PRIMARY KEY, msg TEXT);
```

Triggers are completely missing from the `.schema` output. This affects both main and attached databases.

## 6. Floating-point display precision differs from sqlite3

**Repro:**
```sql
SELECT 1.7976931348623157E+308;
SELECT 0.1 + 0.2;
```

**Expected (sqlite3):**
```
1.7976931348623157e+308
0.30000000000000004
```

**Actual (tursodb):**
```
1.79769313486232e+308
0.3
```

tursodb displays fewer significant digits and rounds `0.1 + 0.2` to `0.3` instead of showing the IEEE 754 exact result. This is a display formatting difference, not a computation error.

## 7. ALTER TABLE ADD COLUMN rejects expression-based DEFAULT values

**Repro:**
```sql
CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT);
INSERT INTO t VALUES(1, 'alice');
ALTER TABLE t ADD COLUMN created TEXT DEFAULT (datetime('now'));
-- Error: Cannot add a column with non-constant default
```

**Expected (sqlite3):**
Successfully adds the column. sqlite3 supports expression defaults in ALTER TABLE ADD COLUMN since version 3.35.0.

**Actual (tursodb):**
Rejects with "Cannot add a column with non-constant default". This prevents using common patterns like `DEFAULT (datetime('now'))` or `DEFAULT (random())` when adding columns to existing tables. Not ATTACH-specific.

## 8. INSERT INTO view gives "no such table" instead of "cannot modify view"

**Repro:**
```sql
CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT);
CREATE VIEW v AS SELECT * FROM t;
INSERT INTO v VALUES(2, 'b');
```

**Expected (sqlite3):**
```
Parse error: cannot modify v because it is a view
```

**Actual (tursodb):**
```
Parse error: no such table: v
```

Views exist as schema objects but the INSERT/UPDATE/DELETE code paths don't recognize them. Instead of giving the proper "cannot modify view" error, tursodb doesn't find the view at all when used as a DML target. Affects both main and attached databases.

## 9. COLLATE clause in ORDER BY of compound SELECT not supported

**Repro:**
```sql
CREATE TABLE t1(id INTEGER PRIMARY KEY, name TEXT);
INSERT INTO t1 VALUES(1,'Bob'),(2,'alice');
CREATE TABLE t2(id INTEGER PRIMARY KEY, name TEXT);
INSERT INTO t2 VALUES(3,'Charlie'),(4,'david');
SELECT * FROM t1 UNION ALL SELECT * FROM t2 ORDER BY name COLLATE NOCASE;
```

**Expected (sqlite3):**
```
2|alice
1|Bob
3|Charlie
4|david
```

**Actual (tursodb):**
```
Parse error: ORDER BY expression in compound SELECT must be a column number or name
```

sqlite3 allows COLLATE in ORDER BY of compound SELECT. tursodb rejects it. Not ATTACH-specific.

## 10. SAVEPOINT ROLLBACK after DDL produces spurious "page is dirty" error

**Repro:**
```sql
BEGIN;
SAVEPOINT sp1;
CREATE TABLE mt(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO mt VALUES(1, 'before');
ROLLBACK TO sp1;
-- Error: page 2 is dirty
-- Then: Parse error: no such table: mt (correct - table was rolled back)
```

**Expected (sqlite3):**
No error. ROLLBACK TO SAVEPOINT cleanly undoes the CREATE TABLE and INSERT without any pager errors.

**Actual (tursodb):**
The DDL rollback works correctly (table doesn't exist after rollback), but the pager produces a spurious `Error: page 2 is dirty` message before the expected "no such table" error. This error may confuse applications that check for errors during transaction management.

Not ATTACH-specific — occurs on main database. But it compounds with ATTACH transactions (Bug 42 in attach_bugs.md).
