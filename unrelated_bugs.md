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
