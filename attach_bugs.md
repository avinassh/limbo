# ATTACH Bugs Found

*45 bugs found, merged into 31 distinct issues below. Original bug numbers preserved in parentheses for traceability. Bugs 32-36 found in Round 10.*

---

## Bug 1: CREATE INDEX on attached DB stores schema prefix in sqlite_master SQL, producing files unreadable by sqlite3 *(was Bugs 1, 32)*

**Repro:**
```sql
ATTACH '/tmp/test.db' AS aux;
CREATE TABLE aux.t(id INTEGER PRIMARY KEY, val TEXT);
CREATE INDEX aux.idx ON t(val);
INSERT INTO aux.t VALUES(1,'a'),(2,'b');
SELECT sql FROM aux.sqlite_master WHERE type='index';
-- Then try: sqlite3 /tmp/test.db "SELECT * FROM t;"
```

**Expected (sqlite3 behavior):**
Stored SQL: `CREATE INDEX idx ON t(val)` — no schema prefix. File is readable by sqlite3.

**Actual (tursodb):**
Stored SQL: `CREATE INDEX aux.idx ON t (val)` — includes schema prefix `aux.`. When sqlite3 opens this file as a standalone database, it fails:
```
Error: malformed database schema (idx) - corrupt database (11)
```

**Root cause:** The CREATE INDEX code path includes the schema qualifier in the SQL string stored in sqlite_master. Tables, triggers, and views do NOT have this problem — only CREATE INDEX. Any tursodb-created attached database containing indexes is permanently unreadable by sqlite3.

**Severity:** Critical — produces corrupt/incompatible database files.

---

## Bug 2: View body name resolution in attached schemas is completely broken *(was Bugs 2, 11, 37)*

Three manifestations of the same root cause — view body SQL is always resolved against main instead of the view's own schema:

**Manifestation A — CREATE VIEW fails when table only exists in attached schema:**
```sql
-- aux has table t2, main does NOT
ATTACH '/tmp/test.db' AS aux;
CREATE VIEW aux.v AS SELECT * FROM t2;
-- Error: no such table: t2
```

**Manifestation B — Pre-existing views on attached databases can't be queried:**
```sql
-- test_view.db was created by sqlite3 with: CREATE VIEW v AS SELECT * FROM t;
ATTACH '/tmp/test_view.db' AS aux;
SELECT * FROM aux.v;
-- Error: no such table: t
```

**Manifestation C — Views in attached schemas silently return data from the WRONG schema:**
```sql
CREATE TABLE items(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO items VALUES(1, 'main_item');
ATTACH ':memory:' AS aux;
CREATE TABLE aux.items(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO aux.items VALUES(2, 'aux_item');
CREATE VIEW aux.v_items AS SELECT * FROM items;
SELECT * FROM aux.v_items;
-- Returns: 1|main_item  (WRONG! Should be 2|aux_item)
```

**Root cause:** When re-parsing a view's stored SQL, unqualified table names are always resolved against the main schema, never against the view's own schema. In sqlite3, view body resolution starts from the view's schema.

**Severity:** Critical — makes ALL views in attached databases unusable. Manifestation C is the worst: silently returns wrong data (cross-schema leakage).

---

## Bug 3: DETACH succeeds during active transaction (should fail)

**Repro:**
```sql
ATTACH '/tmp/test.db' AS aux;
BEGIN;
INSERT INTO aux.t VALUES(30, 'in_txn');
DETACH aux;
-- No error! Data is silently lost.
```

**Expected:** `Error: database aux is locked`

**Severity:** High — silent data loss.

---

## Bug 4: PANIC — INSERT OR REPLACE on attached DB with UNIQUE constraint crashes

**Repro:**
```sql
ATTACH ':memory:' AS aux;
CREATE TABLE aux.t(id INTEGER PRIMARY KEY, name TEXT UNIQUE);
INSERT INTO aux.t VALUES(1, 'alice');
INSERT OR REPLACE INTO aux.t VALUES(2, 'alice');
-- thread panicked at core/translate/insert.rs:3691: index to exist
```

**Root cause:** Index lookup in `emit_replace_delete_conflicting_row` uses the wrong database ID (main instead of attached).

**Severity:** Critical — process crash.

---

## Bug 5: ROLLBACK TO SAVEPOINT does not undo writes on attached databases

**Repro:**
```sql
ATTACH ':memory:' AS aux;
CREATE TABLE aux.t(id INTEGER PRIMARY KEY, val TEXT);
BEGIN;
INSERT INTO aux.t VALUES(1, 'before_sp');
SAVEPOINT sp1;
INSERT INTO aux.t VALUES(2, 'in_sp');
ROLLBACK TO sp1;
COMMIT;
SELECT * FROM aux.t;
-- Returns BOTH rows (row 2 should have been rolled back)
```

**Severity:** Critical — silent data corruption. Affects nested savepoints too.

---

## Bug 6: Multiple PRAGMAs ignore schema qualifier, operating on main instead of specified DB *(was Bugs 6, 7, 8, 22, 23, 27)*

All of these PRAGMAs ignore the schema qualifier and read/write the main database instead:

| PRAGMA | Symptom |
|--------|---------|
| `integrity_check` / `quick_check` | Checks main DB, reports "in database main" |
| `index_list(tbl)` / `index_info(idx)` / `index_xinfo(idx)` | Returns empty results for attached DB indexes |
| `table_list` | Shows main DB tables with hardcoded "main" schema name |
| `cache_size` | Set/get always operates on main's cache |
| `freelist_count` | Returns main's freelist count |
| `synchronous` | Set/get operates on connection-wide setting, not per-DB |

**Repro (synchronous — most dangerous):**
```sql
ATTACH ':memory:' AS mem;
PRAGMA main.synchronous;   -- 2
PRAGMA mem.synchronous = 0;
PRAGMA main.synchronous;   -- 0 (WRONG! Should still be 2)
```

**Root cause:** In `core/translate/pragma.rs`, the `schema` variable is always bound to `resolver.schema()` (main DB schema). The resolved `database_id` is correct but the schema/pager used for lookups is wrong.

**Severity:** High — `synchronous` can cause data loss; `integrity_check` gives false confidence; `index_list/info` prevents schema introspection on attached DBs.

---

## Bug 7: ALTER TABLE ADD COLUMN type validation on attached DB reads wrong pager *(was Bugs 10, 30)*

**Repro:**
```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.strict_t(id INTEGER PRIMARY KEY, name TEXT NOT NULL) STRICT;
INSERT INTO mem.strict_t VALUES(1, 'alice');
ALTER TABLE mem.strict_t ADD COLUMN num INTEGER NOT NULL DEFAULT 'not_int';
-- Expected: "type mismatch on DEFAULT"
-- Actual: I/O error: short read on page 2: expected 4096 bytes, got 0
```

**Root cause:** `core/translate/alter.rs:306` hardcodes `db: crate::MAIN_DB_ID` in `emit_add_column_default_type_validation()`. Reads main's pager at the attached table's root page number.

**Severity:** High — prevents ALTER TABLE ADD COLUMN with type-checked defaults on STRICT tables in any attached DB.

---

## Bug 8: No limit on number of attached databases *(was Bug 9)*

**Repro:**
```sql
ATTACH ':memory:' AS d1;
-- ... repeat ...
ATTACH ':memory:' AS d12;
-- All succeed. sqlite3 rejects at 11 with "too many attached databases - max 10"
```

**Severity:** Medium — potential resource exhaustion.

---

## Bug 9: Unqualified names don't fall back to attached databases *(was Bugs 12, 15)*

**Repro:**
```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.only_here(id INTEGER PRIMARY KEY);
INSERT INTO only_here VALUES(1);        -- Error: no such table
SELECT * FROM only_here;                -- Error: no such table
DROP INDEX only_here_idx;               -- Error: no such index (if index exists)
DROP TRIGGER only_here_trg;             -- Error: no such trigger (if trigger exists)
```

**Expected:** sqlite3 searches main → temp → attached DBs (in attach order) for unqualified names.

**Root cause:** Name resolution for tables, indexes, and triggers only searches the main schema. Never falls back to attached databases.

**Severity:** High — forces always-qualified names, breaking SQLite compatibility.

---

## Bug 10: Schema.table.column three-part references broken in cross-DB queries *(was Bugs 13, 26)*

**Manifestation A — Silently resolves to wrong table:**
```sql
CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO t VALUES(1,'MAIN');
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO mem.t VALUES(1,'MEM');
SELECT main.t.val, mem.t.val FROM main.t, mem.t WHERE main.t.id = mem.t.id;
-- Returns: MAIN|MAIN (should be MAIN|MEM)
```

**Manifestation B — False "ambiguous column" error:**
```sql
SELECT * FROM main.m, mem.m WHERE main.m.id = mem.m.id;
-- Error: ambiguous column name: m.id
```

**Root cause:** Three-part `schema.table.column` is parsed as `table.column` with `schema` ignored.

**Severity:** High — silent wrong results (A) or prevents valid queries (B).

---

## Bug 11: Query optimizer does not use indexes on attached databases *(was Bug 14)*

**Repro:**
```sql
ATTACH '/tmp/test.db' AS aux;  -- aux has CREATE INDEX idx ON t(category)
EXPLAIN QUERY PLAN SELECT * FROM aux.t WHERE category = 'fruit';
-- Shows: SCAN t  (should be: SEARCH t USING INDEX idx)
```

**Root cause:** Optimizer's index selection logic doesn't consider indexes from attached database schemas.

**Severity:** High — O(n) scans instead of O(log n) lookups on all attached DB queries.

---

## Bug 12: ATTACH NULL fails instead of creating in-memory database *(was Bug 16)*

```sql
ATTACH NULL AS aux;
-- Error: attach: filename argument must be text
-- sqlite3 creates in-memory DB
```

---

## Bug 13: Parser can't handle non-identifier schema qualifiers *(was Bugs 17, 39, 40)*

The parser fails on schema-qualified names when the schema name is a keyword, empty string, or numeric:

```sql
-- Keyword:
ATTACH ':memory:' AS "select";
CREATE TABLE "select".t(id INTEGER PRIMARY KEY);  -- Error: unexpected token 'select'

-- Empty string:
ATTACH ':memory:' AS "";
CREATE TABLE "".t1(id INTEGER PRIMARY KEY);  -- Error: unexpected token '.'

-- Numeric:
ATTACH ':memory:' AS "123";
CREATE TABLE "123".t1(id INTEGER PRIMARY KEY);  -- Error: unexpected token '123.'
```

All three work in sqlite3. The ATTACH itself succeeds in all cases — only the subsequent `schema.table` reference fails.

**Root cause:** The parser's schema-qualified name resolution doesn't correctly handle quoted identifiers that aren't standard identifiers in the schema position.

**Severity:** Medium — prevents using reserved words, numbers, or empty strings as attached DB names.

---

## Bug 14: ATTACH of 0-byte (empty) file causes hang/infinite loop *(was Bug 18)*

```bash
> /tmp/empty.db
echo "ATTACH '/tmp/empty.db' AS aux;" | tursodb --experimental-attach -q
# Process hangs indefinitely — must be killed
```

**Severity:** Critical — process freeze.

---

## Bug 15: BEGIN IMMEDIATE/EXCLUSIVE doesn't acquire locks on attached databases *(was Bug 19)*

**Repro:**
```sql
ATTACH ':memory:' AS aux;
CREATE TABLE aux.t(id INTEGER PRIMARY KEY);
EXPLAIN BEGIN IMMEDIATE;
-- Only shows: Transaction 0 2 1 (main only)
-- Missing: Transaction for attached DB
```

**Root cause:** `core/translate/transaction.rs:30-43` only emits Transaction for `MAIN_DB_ID`.

**Severity:** High — breaks `BEGIN IMMEDIATE` semantics, allows concurrent writers on attached DBs.

---

## Bug 16: DML on attached DB unnecessarily opens WRITE transaction on main *(was Bug 20)*

```sql
ATTACH ':memory:' AS aux;
CREATE TABLE aux.t(id INTEGER PRIMARY KEY);
EXPLAIN INSERT INTO aux.t VALUES(1);
-- Shows WRITE transaction on both main (iDb=0) AND aux (iDb=2)
-- Should only open write on aux
```

**Severity:** Medium — unnecessary lock contention on main DB.

---

## Bug 17: DROP TABLE on attached DB doesn't clean up indexes or sqlite_sequence *(was Bugs 21, 25)*

**Index B-tree pages leaked:**
```sql
-- Attached DB has table with 2 indexes. After DROP TABLE:
-- Expected: freelist_count = 3 (table + 2 indexes)
-- Actual: freelist_count = 1 (only table freed, index pages leaked)
```

**sqlite_sequence not cleaned up:**
```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY AUTOINCREMENT, val TEXT);
INSERT INTO mem.t(val) VALUES('a');
DROP TABLE mem.t;
SELECT * FROM mem.sqlite_sequence;
-- Returns: t|1 (should be empty)
```

**Root cause:** `resolver.schema().get_indices()` and sequence table lookup return main DB's data. For attached DBs, returns empty/wrong results, so cleanup is skipped.

**Severity:** High — permanent storage leak for index pages.

---

## Bug 18: EXPLAIN QUERY PLAN doesn't show schema name prefix for attached DB tables *(was Bug 24)*

```sql
EXPLAIN QUERY PLAN SELECT * FROM aux.t;
-- Shows: SCAN t  (should be: SCAN aux.t)
```

**Severity:** Low — diagnostic/informational.

---

## Bug 19: INDEXED BY fails on attached DB *(was Bug 28)*

```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY, val TEXT);
CREATE INDEX mem.idx ON t(val);
SELECT * FROM mem.t INDEXED BY idx WHERE val = 'x';
-- Error: no such index: idx
```

**Severity:** Medium — prevents INDEXED BY hints on attached tables.

---

## Bug 20: Unqualified ANALYZE only analyzes main, not attached databases *(was Bug 29)*

```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY, val TEXT);
CREATE INDEX mem.idx ON t(val);
ANALYZE;  -- Only analyzes main
SELECT * FROM mem.sqlite_stat1;  -- Error: no such table
-- Note: ANALYZE mem; (qualified) works correctly
```

**Severity:** Medium — combined with Bug 11, attached DB performance is doubly penalized.

---

## Bug 21: WAL not checkpointed on DETACH *(was Bug 31)*

```sql
ATTACH '/tmp/test.db' AS aux;
CREATE TABLE aux.t(id INTEGER PRIMARY KEY);
INSERT INTO aux.t VALUES(1);
DETACH aux;
-- WAL file (20KB) remains on disk. sqlite3 checkpoints on DETACH.
```

**Severity:** Medium — wastes disk space.

---

## Bug 22: `.schema` adds extra schema prefix to table name in CREATE INDEX *(was Bug 33)*

```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY, val TEXT);
CREATE INDEX mem.idx ON t(val);
.schema
-- Shows: CREATE INDEX mem.idx ON mem.t (val)
-- Should be: CREATE INDEX mem.idx ON t(val)
```

The stored SQL in sqlite_master has `ON t` but `.schema` display adds `ON mem.t`.

**Severity:** Low — cosmetic.

---

## Bug 23: count(*) on empty attached DB sqlite_master causes I/O error *(was Bug 34)*

```sql
ATTACH ':memory:' AS mem;
SELECT count(*) FROM mem.sqlite_master;
-- Error: I/O error: short read on page 1: expected 4096 bytes, got 0
-- Note: SELECT * FROM mem.sqlite_master works fine (returns empty)
```

**Severity:** Medium — prevents basic schema introspection on freshly attached DBs.

---

## Bug 24: `file:` URI `mode=memory` parameter ignored for ATTACH *(was Bug 35)*

```sql
ATTACH 'file:mydb?mode=memory' AS mdb;
-- Creates file-based DB on disk instead of in-memory DB
```

**Severity:** Medium — breaks URI-based in-memory database usage.

---

## Bug 25: CREATE TRIGGER cross-schema silently targets wrong table *(was Bug 36)*

**Repro:**
```sql
CREATE TABLE t1(id INTEGER PRIMARY KEY, val TEXT);
CREATE TABLE log(msg TEXT);
ATTACH ':memory:' AS aux;
CREATE TABLE aux.t1(id INTEGER PRIMARY KEY, val TEXT);

CREATE TRIGGER main.tr AFTER INSERT ON aux.t1
BEGIN
  INSERT INTO log VALUES(NEW.val);
END;
-- sqlite3: Error: trigger tr cannot reference objects in database aux
-- tursodb: Silently creates trigger on main.t1 instead of aux.t1
```

The trigger fires on `main.t1` inserts, NOT `aux.t1` inserts. The `aux.` qualifier is silently ignored.

**Severity:** High — silent wrong behavior, trigger fires on wrong table.

---

## Bug 26: ATTACH with subquery expression not supported *(was Bug 38)*

```sql
ATTACH (SELECT ':memory:') AS dynamic_db;
-- Error: Subquery is not supported in this position
-- sqlite3 supports this. Other expressions (concat, CAST, CASE, functions) work.
```

**Severity:** Low — uncommon usage, easy workaround.

---

## Bug 27: `.import` CLI command cannot import into attached DB tables

**Repro:**
```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.data(id INTEGER, name TEXT, score INTEGER);
.import --csv /path/to/data.csv mem.data
-- Error: "Error creating table: LexerError(...)"
-- tursodb does not find the existing table and tries to auto-create one with literal name "mem.data"
```

**Expected (sqlite3):** sqlite3 also doesn't support schema-qualified names in `.import` (treats `mem.data` as a literal table name), but it at least succeeds by creating a table with that literal name. tursodb fails entirely.

**Root cause:** The `.import` command doesn't support schema-qualified table names for lookup. When the table isn't found in main schema, it attempts auto-creation using the qualified name as a literal, which fails during parsing.

**Severity:** Medium — prevents CSV import into attached database tables.

---

## Bug 28: INSERT OR REPLACE panic extends to composite PRIMARY KEY tables on attached DB

**Repro:**
```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(a INTEGER, b INTEGER, val TEXT, PRIMARY KEY(a, b));
INSERT INTO mem.t VALUES(1, 1, 'first');
INSERT INTO mem.t VALUES(1, 2, 'second');
REPLACE INTO mem.t VALUES(1, 1, 'replaced');
-- thread panicked at core/translate/insert.rs:3691: index to exist
```

**Expected:** Should replace the conflicting row without error.

**Root cause:** Same as Bug 4 — `emit_replace_delete_conflicting_row` uses `resolver.schema()` (main) to look up the index backing the composite PK. Since the index exists only in the attached DB's schema, the lookup fails with a panic. Composite PKs create an internal unique index that is just as affected as explicit UNIQUE constraints.

**Severity:** Critical — process crash. Affects ANY `REPLACE`/`INSERT OR REPLACE` on attached DB tables with composite primary keys.

---

## Bug 29: INSERT OR REPLACE panic extends to expression indexes on attached DB

**Repro:**
```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY, email TEXT);
CREATE UNIQUE INDEX mem.idx_lower_email ON t(LOWER(email));
INSERT INTO mem.t VALUES(1, 'Alice@test.com');
INSERT OR REPLACE INTO mem.t VALUES(2, 'alice@test.com');
-- thread panicked at core/translate/insert.rs:3691: index to exist
```

**Expected:** Should replace the row that conflicts on the expression index.

**Root cause:** Same as Bugs 4 and 28 — `resolver.schema()` returns main schema which doesn't have the expression index.

**Severity:** Critical — process crash on any REPLACE with expression-based UNIQUE indexes on attached DBs.

---

## Bug 30: DDL and SELECT on attached DB open unnecessary transactions on main database

**Repro:**
```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO mem.t VALUES(1, 'test');

-- DDL on attached DB opens WRITE transaction on main:
EXPLAIN CREATE TABLE mem.t2(id INTEGER PRIMARY KEY);
-- Shows: Transaction iDb=0 tx_mode=Write AND Transaction iDb=2 tx_mode=Write
-- sqlite3 only shows: Transaction iDb=2

-- Even SELECT on attached DB opens READ transaction on main:
EXPLAIN SELECT * FROM mem.t;
-- Shows: Transaction iDb=0 tx_mode=Read AND Transaction iDb=2 tx_mode=Read
-- sqlite3 only shows: Transaction iDb=2
```

**Expected (sqlite3):** Only open transactions on the databases actually involved in the operation.

**Root cause:** Transaction emission always includes main database (iDb=0) regardless of which databases are actually accessed. This extends Bug 16 (which documented the issue for DML) to DDL and read-only operations as well.

**Severity:** Medium — unnecessary lock contention on main DB for all attached DB operations, including read-only queries.

---

## Bug 31: SAVEPOINT ROLLBACK doesn't undo DDL (CREATE TABLE) on attached databases

**Repro:**
```sql
ATTACH ':memory:' AS mem;
BEGIN;
SAVEPOINT sp1;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY);
INSERT INTO mem.t VALUES(1);
ROLLBACK TO sp1;
-- Table still exists, data still present!
SELECT * FROM mem.t;
-- Returns: 1
```

**Expected (sqlite3):**
```
Error: no such table: mem.t
```
In sqlite3, `ROLLBACK TO sp1` undoes both the CREATE TABLE and the INSERT.

**Root cause:** Extends Bug 5 (SAVEPOINT ROLLBACK doesn't undo writes on attached databases) to DDL operations. Neither schema changes (CREATE TABLE, CREATE INDEX) nor DML changes (INSERT, UPDATE, DELETE) are rolled back by SAVEPOINT on attached databases.

**Severity:** Critical — silent data/schema corruption. Code expecting transactional DDL protection gets neither on attached databases.

---

## Bug 32: Experimental feature flags not propagated to attached DB schema (generated columns)

**Repro:**
```sql
-- Start tursodb with: --experimental-attach --experimental-generated-columns
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY, first TEXT, last TEXT, full TEXT GENERATED ALWAYS AS (first || ' ' || last) VIRTUAL);
-- CREATE TABLE succeeds, but schema re-parse fails:
INSERT INTO mem.t(id, first, last) VALUES(1, 'John', 'Doe');
-- Error: table 't' uses generated columns but the generated_columns feature is not enabled
-- Error: no such table: t
```

Also affects attaching a sqlite3-created DB with generated columns:
```sql
-- sqlite3 creates: CREATE TABLE t(... full TEXT GENERATED ALWAYS AS (...) VIRTUAL)
ATTACH '/path/to/gen_col.db' AS gv;
SELECT * FROM gv.t;
-- Same error: generated_columns feature is not enabled
```

The same feature works correctly on the main database.

**Expected:** Generated columns should work on attached databases when the `--experimental-generated-columns` flag is enabled.

**Root cause:** In `core/connection.rs:1642-1644`, `attach_database()` creates `DatabaseOpts` with only `.with_views()` and `.with_custom_types()`. It's missing `.with_generated_columns(self.db.experimental_generated_columns_enabled())`. When the attached DB's schema is re-parsed via `ParseSchema`, the Schema object doesn't have `generated_columns_enabled = true`, so any table with generated columns is rejected.

**Severity:** High — makes generated columns completely unusable on any attached database.

---

## Bug 33: `immutable=1` URI parameter ignored for ATTACH — allows writes to immutable database

**Repro:**
```sql
-- Create a DB with sqlite3 first:
-- sqlite3 /tmp/immut.db "CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT); INSERT INTO t VALUES(1, 'original');"

ATTACH 'file:/tmp/immut.db?immutable=1' AS imm;
SELECT * FROM imm.t;
-- Returns: 1|original (read works)

INSERT INTO imm.t VALUES(2, 'new');
-- No error! Write succeeds!

SELECT * FROM imm.t;
-- Returns: 1|original AND 2|new
```

The data is actually written to disk — verified with sqlite3 after detach.

**Expected (sqlite3 behavior):** `INSERT INTO imm.t VALUES(2, 'new');` fails with "attempt to write a readonly database".

**Root cause:** The `immutable=1` URI parameter is parsed but not enforced. The database is opened in read-write mode regardless. The `mode=ro` parameter works correctly, but `immutable=1` does not apply read-only enforcement.

**Severity:** High — silent data corruption. `immutable=1` is used when a DB is known not to change (e.g., read-only media, shared reference data). Allowing writes violates this contract.

---

## Bug 34: `mode=rw` URI parameter ignored for ATTACH — creates files that don't exist

**Repro:**
```sql
-- /tmp/nonexistent.db does NOT exist
ATTACH 'file:/tmp/nonexistent.db?mode=rw' AS rw;
-- No error! File is created on disk.

CREATE TABLE rw.t(id INTEGER PRIMARY KEY);
INSERT INTO rw.t VALUES(1);
SELECT * FROM rw.t;
-- Returns: 1
```

**Expected (sqlite3 behavior):** `ATTACH 'file:/tmp/nonexistent.db?mode=rw' AS rw;` fails with "unable to open database".

In SQLite's URI scheme:
- `mode=rw` means "read-write, do NOT create"
- `mode=rwc` means "read-write, create if needed" (default)

**Root cause:** The `mode=rw` parameter's "don't create" semantics are not enforced. The file is opened with `O_CREAT` regardless of the mode parameter.

**Severity:** Medium — creates unexpected files on disk. Could cause issues in deployment scripts that rely on `mode=rw` to verify a database exists before using it.

---

## Bug 35: `file:` URI with empty path fails in ATTACH (should create in-memory DB)

**Repro:**
```sql
ATTACH 'file:' AS empty_uri;
-- Error: I/O error (open): entity not found

ATTACH 'file:?mode=memory' AS mem;
-- Error: I/O error (open): entity not found
```

**Expected (sqlite3 behavior):**
```sql
ATTACH 'file:' AS empty_uri;
-- Succeeds, creates in-memory DB
CREATE TABLE empty_uri.t(id INTEGER);
INSERT INTO empty_uri.t VALUES(1);
SELECT * FROM empty_uri.t;
-- Returns: 1
```

In sqlite3, `file:` with an empty path creates a temporary/in-memory database. Similarly, `file:?mode=memory` creates an in-memory DB. tursodb fails on both because it tries to open a file with an empty path.

**Root cause:** The URI parser in `from_uri_attached` doesn't handle the empty-path case. When the path component of the file: URI is empty, it should be treated as `:memory:` (or a temporary DB).

**Severity:** Medium — prevents valid URI patterns for in-memory attached databases.

---

## Bug 36: ATTACH on read-only file (chmod 444) fails instead of opening in read-only mode

**Repro:**
```bash
# Create a DB and make it read-only
sqlite3 /tmp/readonly.db "CREATE TABLE t(id PRIMARY KEY, val TEXT); INSERT INTO t VALUES(1, 'test');"
chmod 444 /tmp/readonly.db
```

```sql
ATTACH '/tmp/readonly.db' AS ro;
-- Error: I/O error (open): permission denied
```

**Expected (sqlite3 behavior):**
```sql
ATTACH '/tmp/readonly.db' AS ro;
-- Succeeds, opens in read-only mode
SELECT * FROM ro.t;
-- Returns: 1|test
INSERT INTO ro.t VALUES(2, 'new');
-- Error: attempt to write a readonly database
```

sqlite3 detects the file permissions and automatically opens the database in read-only mode. tursodb fails to open the file at all because it always tries to open in read-write mode first.

**Root cause:** `attach_database()` opens the file with read-write flags. When the file is read-only (permissions 444), the open fails. There's no fallback to open the file in read-only mode, unlike sqlite3 which automatically downgrades to read-only.

**Severity:** Medium — prevents attaching any database file that doesn't have write permissions, even for read-only access.
