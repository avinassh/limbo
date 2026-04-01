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
