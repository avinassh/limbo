# ATTACH Bugs Found

*56 raw bugs found across 12 rounds, merged into 41 distinct issues below. Original bug numbers preserved in parentheses for traceability.*

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

## Bug 4: PANIC — INSERT OR REPLACE on attached DB crashes for any UNIQUE/PK index type *(was Bugs 4, 28, 29)*

Any `INSERT OR REPLACE` / `REPLACE INTO` on an attached DB table that has a UNIQUE constraint, composite PK, or expression-based UNIQUE index causes a panic:

**Manifestation A — Explicit UNIQUE constraint:**
```sql
ATTACH ':memory:' AS aux;
CREATE TABLE aux.t(id INTEGER PRIMARY KEY, name TEXT UNIQUE);
INSERT INTO aux.t VALUES(1, 'alice');
INSERT OR REPLACE INTO aux.t VALUES(2, 'alice');
-- thread panicked at core/translate/insert.rs:3691: index to exist
```

**Manifestation B — Composite PRIMARY KEY:**
```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(a INTEGER, b INTEGER, val TEXT, PRIMARY KEY(a, b));
INSERT INTO mem.t VALUES(1, 1, 'first');
REPLACE INTO mem.t VALUES(1, 1, 'replaced');
-- thread panicked at core/translate/insert.rs:3691: index to exist
```

**Manifestation C — Expression-based UNIQUE index:**
```sql
ATTACH ':memory:' AS mem;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY, email TEXT);
CREATE UNIQUE INDEX mem.idx_lower_email ON t(LOWER(email));
INSERT INTO mem.t VALUES(1, 'Alice@test.com');
INSERT OR REPLACE INTO mem.t VALUES(2, 'alice@test.com');
-- thread panicked at core/translate/insert.rs:3691: index to exist
```

**Root cause:** `emit_replace_delete_conflicting_row` uses `resolver.schema()` (main) to look up the index backing the constraint. Since the index only exists in the attached DB's schema, the lookup fails with a panic.

**Severity:** Critical — process crash. Affects ANY `REPLACE`/`INSERT OR REPLACE` on attached DB tables with UNIQUE constraints, composite primary keys, or expression indexes.

---

## Bug 5: SAVEPOINT ROLLBACK doesn't undo changes (DML or DDL) on attached databases *(was Bugs 5, 31)*

Neither data changes nor schema changes are rolled back by SAVEPOINT on attached databases:

**DML not rolled back:**
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

**DDL not rolled back:**
```sql
ATTACH ':memory:' AS mem;
BEGIN;
SAVEPOINT sp1;
CREATE TABLE mem.t(id INTEGER PRIMARY KEY);
INSERT INTO mem.t VALUES(1);
ROLLBACK TO sp1;
SELECT * FROM mem.t;
-- Returns: 1 (both CREATE TABLE and INSERT should have been undone)
-- sqlite3: Error: no such table: mem.t
```

**Severity:** Critical — silent data/schema corruption. Affects nested savepoints too. Code expecting transactional protection gets none on attached databases.

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

## Bug 16: Operations on attached DB unnecessarily open transactions on main database *(was Bugs 20, 30)*

All operations targeting attached databases — DML, DDL, and even read-only queries — unnecessarily open a transaction on the main database:

```sql
ATTACH ':memory:' AS aux;
CREATE TABLE aux.t(id INTEGER PRIMARY KEY);

-- DML opens WRITE on main:
EXPLAIN INSERT INTO aux.t VALUES(1);
-- Shows WRITE transaction on both main (iDb=0) AND aux (iDb=2)

-- DDL opens WRITE on main:
EXPLAIN CREATE TABLE aux.t2(id INTEGER PRIMARY KEY);
-- Shows: Transaction iDb=0 tx_mode=Write AND Transaction iDb=2 tx_mode=Write

-- Even SELECT opens READ on main:
EXPLAIN SELECT * FROM aux.t;
-- Shows: Transaction iDb=0 tx_mode=Read AND Transaction iDb=2 tx_mode=Read
```

**Expected (sqlite3):** Only open transactions on the databases actually involved in the operation.

**Root cause:** Transaction emission always includes main database (iDb=0) regardless of which databases are actually accessed.

**Severity:** Medium — unnecessary lock contention on main DB for all attached DB operations.

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

## Bug 24: URI `mode=memory` and empty `file:` path not handled for ATTACH *(was Bugs 24-orig, 35)*

Two related failures in URI-based in-memory database creation for ATTACH:

**`mode=memory` parameter ignored (creates file on disk instead):**
```sql
ATTACH 'file:mydb?mode=memory' AS mdb;
-- Creates file-based DB on disk instead of in-memory DB
```

**Empty `file:` path fails (should create in-memory/temp DB):**
```sql
ATTACH 'file:' AS empty_uri;
-- Error: I/O error (open): entity not found

ATTACH 'file:?mode=memory' AS mem;
-- Error: I/O error (open): entity not found
-- sqlite3: both succeed and create in-memory DBs
```

**Root cause:** The URI parser in `from_uri_attached` doesn't handle: (a) the `mode=memory` parameter to override file creation, or (b) the empty-path case where `file:` should be treated as `:memory:`.

**Severity:** Medium — breaks URI-based in-memory database patterns.

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

## Bug 28: Experimental feature flags not propagated to attached DB schema *(was Bug 32)*

**Repro (generated columns):**
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
ATTACH '/path/to/gen_col.db' AS gv;
SELECT * FROM gv.t;
-- Same error: generated_columns feature is not enabled
```

The same feature works correctly on the main database.

**Root cause:** In `core/connection.rs:1642-1644`, `attach_database()` creates `DatabaseOpts` with only `.with_views()` and `.with_custom_types()`. It's missing `.with_generated_columns(self.db.experimental_generated_columns_enabled())`. When the attached DB's schema is re-parsed via `ParseSchema`, the Schema object doesn't have `generated_columns_enabled = true`, so any table with generated columns is rejected.

**Severity:** High — makes generated columns completely unusable on any attached database.

---

## Bug 29: `immutable=1` URI parameter ignored for ATTACH — allows writes to immutable database *(was Bug 33)*

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

## Bug 30: `mode=rw` URI parameter ignored for ATTACH — creates files that don't exist *(was Bug 34)*

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

**Severity:** Medium — creates unexpected files on disk.

---

## Bug 31: ATTACH on read-only file (chmod 444) fails instead of opening in read-only mode *(was Bug 36)*

**Repro:**
```bash
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

---

## Bug 32: Function-style pragmas (table-valued) don't search attached databases

**Repro:**
```sql
ATTACH ':memory:' AS m1;
CREATE TABLE m1.t(id INTEGER PRIMARY KEY, val TEXT, num INTEGER);
-- Function-style pragma (FROM clause) - returns empty:
SELECT * FROM pragma_table_info('t');
-- Statement-style pragma - works correctly:
PRAGMA m1.table_info(t);
```

Also affects `pragma_index_list()` and `pragma_table_xinfo()`:
```sql
ATTACH ':memory:' AS m1;
CREATE TABLE m1.t(id INTEGER PRIMARY KEY, val TEXT);
CREATE INDEX m1.idx ON t(val);
SELECT * FROM pragma_index_list('t');      -- Empty!
SELECT * FROM pragma_table_xinfo('t');     -- Empty!
PRAGMA m1.index_list(t);                   -- Works correctly
PRAGMA m1.table_xinfo(t);                  -- Doesn't exist as statement pragma, but table_info works
```

**Expected (sqlite3 behavior):** `SELECT * FROM pragma_table_info('t')` searches all schemas (main → temp → attached) and returns column info for `t` found in `m1`.

**Root cause:** The function-style pragma implementation only searches the main schema when resolving the table name argument. It doesn't fall back to attached database schemas like the `PRAGMA schema.function(arg)` statement syntax does.

**Severity:** Medium — prevents using pragmas as table-valued functions for schema introspection on attached databases. Blocks patterns like `SELECT ... FROM sqlite_master m, pragma_table_info(m.name)` for attached DB schemas.

---

## Bug 33: PRAGMA integrity_check on attached DB produces false corruption reports *(extends Bug 6)*

**Repro:**
```sql
ATTACH ':memory:' AS m1;
CREATE TABLE m1.t(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO m1.t VALUES(1, 'a'),(2, 'b');
PRAGMA m1.integrity_check;
-- Returns: *** in database main ***\nPage 2: never used
```

**Expected:** `ok` (the database is perfectly valid).

**Bytecode analysis:**
```
-- Main DB: IntegrityCk roots=[2, 1]  -- includes table (page 2) and sqlite_master (page 1)
-- Attached: IntegrityCk roots=[1]     -- ONLY sqlite_master, MISSING table (page 2)
```

The IntegrityCk instruction correctly targets `db=2` (attached DB), so it reads the right pager. But the roots list only includes root page 1 (sqlite_master), omitting all user table and index root pages. This causes every user table page to be reported as "never used" — a false corruption report.

Additionally, the error prefix is hardcoded as `"*** in database main ***"` instead of using the actual database name. `PRAGMA m1.quick_check` exhibits the same two issues.

**Root cause:** When emitting the IntegrityCk instruction for attached DBs, the code doesn't enumerate user table root pages from the attached schema. It only includes sqlite_master's root page (1). The error message prefix string is also hardcoded to "main".

**Severity:** High — PRAGMA integrity_check is completely broken on attached databases: always reports false corruption, never says "ok", and attributes errors to the wrong database. Gives false negative confidence when actually checking main.

---

## Bug 34: `.schema TABLE_NAME` doesn't search attached databases

**Repro:**
```sql
ATTACH ':memory:' AS m1;
CREATE TABLE m1.t(id INTEGER PRIMARY KEY, val TEXT);
CREATE INDEX m1.idx ON t(val);
.schema t
-- Returns: (empty)
```

**Expected (sqlite3):**
```sql
.schema t
-- Returns:
-- CREATE TABLE m1.t(id INTEGER PRIMARY KEY, val TEXT);
-- CREATE INDEX m1.idx ON t(val);
```

Note: `.schema` without arguments correctly shows schemas from all databases. Only the filtered form `.schema TABLE_NAME` fails to search attached DBs.

**Root cause:** The `.schema TABLE_NAME` command only searches the main schema for matching table names. It doesn't fall back to attached databases like sqlite3 does.

**Severity:** Low — workaround is to use `.schema m1.t` (qualified name) or `.schema` (all schemas).

---

## Bug 35: `.schema` shows wrong view column info for attached DB views

**Repro:**
```sql
ATTACH ':memory:' AS m1;
CREATE TABLE m1.t(id INTEGER PRIMARY KEY, val TEXT);
CREATE VIEW m1.v AS SELECT * FROM t;
.schema
```

**tursodb output:**
```
CREATE VIEW v AS SELECT * FROM t;
/* v(x) */
```

**sqlite3 output:**
```
CREATE VIEW m1.v AS SELECT * FROM t
/* m1.v(id,val) */;
```

Three differences:
1. Column info says `x` instead of `id,val` — wrong column names
2. View name missing schema prefix (`v` instead of `m1.v`)
3. Missing schema prefix in column info comment

Note: `PRAGMA m1.table_info(v)` correctly returns `id` and `val` columns. The issue is in the `.schema` display code.

**Root cause:** When `.schema` generates the view column comment for attached DB views, it fails to resolve the view's SELECT body against the attached schema (same root cause as Bug 2 — view body resolution). The fallback produces an incorrect default column name `x`.

**Severity:** Low — cosmetic display issue in `.schema` output. `PRAGMA m1.table_info(v)` provides correct results.

---

## Bug 36: PRAGMA integrity_check error prefix always says "main" for all databases

**Repro:**
```sql
ATTACH ':memory:' AS m1;
CREATE TABLE m1.t(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO m1.t VALUES(1, 'a');
PRAGMA m1.integrity_check;
-- Output includes: *** in database main ***
-- Should say: *** in database m1 ***
```

**Bytecode evidence:**
```
5     String8  0  5  0  '*** in database main ***'
```

The error prefix string is a compile-time constant "main" regardless of which database is being checked. Combined with Bug 33 (false corruption reports), this means integrity check errors on attached DBs are both incorrect and misattributed to main.

**Root cause:** In `core/translate/pragma.rs`, the integrity check error prefix is hardcoded as `"*** in database main ***"` instead of interpolating the actual schema name.

**Severity:** Medium — misidentifies which database has integrity issues when checking attached databases.

---

## Bug 37: Same-name tables from different schemas in JOIN: optimizer degrades PK search to full SCAN

**Repro:**
```sql
CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO t VALUES(1,'m1'),(2,'m2');
ATTACH ':memory:' AS a1;
CREATE TABLE a1.t(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO a1.t VALUES(1,'a1'),(2,'a2');
ATTACH ':memory:' AS a2;
CREATE TABLE a2.t(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO a2.t VALUES(1,'a2_1'),(2,'a2_2');
-- 3-way join with same table name 't' in all schemas
EXPLAIN QUERY PLAN SELECT main.t.val, a1.t.val, a2.t.val FROM main.t JOIN a1.t ON main.t.id = a1.t.id JOIN a2.t ON main.t.id = a2.t.id;
-- tursodb: SCAN t / SCAN t / SCAN t
-- sqlite3: SCAN main.t / SEARCH a1.t USING INTEGER PRIMARY KEY / SEARCH a2.t USING INTEGER PRIMARY KEY
```

Also fails for 2-way joins:
```sql
EXPLAIN QUERY PLAN SELECT main.t.val, a1.t.val FROM main.t JOIN a1.t ON main.t.id = a1.t.id;
-- tursodb: SCAN t / SCAN t
-- sqlite3: SCAN main.t / SEARCH a1.t USING INTEGER PRIMARY KEY (rowid=?)
```

**Workaround:** Use table aliases:
```sql
SELECT x.val, y.val FROM main.t x JOIN a1.t y ON x.id = y.id;
-- tursodb: SCAN t AS x / SEARCH y USING INTEGER PRIMARY KEY (rowid=?)  ← CORRECT
```

**Root cause:** When tables share the same name across schemas, the optimizer's name resolution fails and it can't match the join condition to a rowid lookup. The optimizer effectively gives up and does full scans on all tables. With aliases or different table names, the optimizer correctly uses PK search.

**Severity:** High — O(n²) or O(n³) performance instead of O(n) for joins involving same-name tables across schemas. Very common pattern since users often have identically-named tables across databases.

---

## Bug 38: Unqualified statement-style PRAGMAs don't search attached databases

**Repro:**
```sql
ATTACH ':memory:' AS aux;
CREATE TABLE aux.t(id INTEGER PRIMARY KEY, name TEXT, age INTEGER);
-- Qualified: works correctly
PRAGMA aux.table_info(t);
-- 0|id|INTEGER|0||1
-- 1|name|TEXT|0||0
-- 2|age|INTEGER|0||0

-- Unqualified: returns empty (should search all schemas)
PRAGMA table_info(t);
-- (empty)
```

**Expected (sqlite3):**
```sql
PRAGMA table_info(t);
-- Searches main → temp → attached DBs and returns:
-- 0|id|INTEGER|0||1
-- 1|name|TEXT|0||0
-- 2|age|INTEGER|0||0
```

**Root cause:** The statement-style PRAGMA code path only searches the main schema when no schema qualifier is provided. sqlite3 follows the standard name resolution order (main → temp → attached DBs).

**Severity:** Medium — forces always-qualified PRAGMA calls. Breaks patterns like `PRAGMA table_info(t)` when table only exists in attached DB.

---

## Bug 39: CREATE TYPE can't target attached databases (always stored in main)

**Repro:**
```sql
ATTACH ':memory:' AS aux;
-- Schema-qualified CREATE TYPE fails to parse:
CREATE TYPE aux.mytype BASE integer ENCODE value * 2 DECODE value / 2;
-- Error: expected BASE keyword

-- Unqualified CREATE TYPE always creates in main:
CREATE TYPE mytype BASE integer ENCODE value * 2 DECODE value / 2;
SELECT * FROM main.sqlite_master WHERE name LIKE '%turso%';
-- Shows: __turso_internal_types in main
SELECT * FROM aux.sqlite_master WHERE name LIKE '%turso%';
-- (empty) -- no types table created in aux
```

**Root cause:** In `core/translate/schema.rs`, all type-related operations hardcode `db: 0` (MAIN_DB_ID). The `CreateBtree`, `OpenWrite`, `AddType`, `SetCookie`, and `DropType` instructions all target the main database. The parser also doesn't support schema-qualified `CREATE TYPE schema.name`.

**Code references:**
- `core/translate/schema.rs:2060` — `CreateBtree { db: 0, ... }`
- `core/translate/schema.rs:2075` — `OpenWrite { db: 0 }`
- `core/translate/schema.rs:2132` — `AddType { db: 0, sql }`
- `core/translate/schema.rs:2135` — `SetCookie { db: 0, ... }`

**Severity:** Medium — custom types cannot be stored in attached database files. Type definitions are lost when the attached DB is detached and reattached in a new session (unless the main DB also defines the same types).

---

## Bug 40: Same-name tables in cross-DB SELECT * with schema-qualified ON clause fails with "ambiguous column name"

**Repro:**
```sql
CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO t VALUES(1, 'main');
ATTACH ':memory:' AS aux;
CREATE TABLE aux.t(id INTEGER PRIMARY KEY, val TEXT);
INSERT INTO aux.t VALUES(2, 'aux');
-- Fails even with schema-qualified table references
SELECT * FROM main.t JOIN aux.t ON main.t.id != aux.t.id;
-- Error: ambiguous column name: t.id
```

**Expected (sqlite3):**
```sql
SELECT * FROM main.t JOIN aux.t ON main.t.id != aux.t.id;
-- Returns: 1|main|2|aux
```

**Workaround:** Use table aliases:
```sql
SELECT * FROM main.t x JOIN aux.t y ON x.id != y.id;
-- Returns: 1|main|2|aux  ← works correctly
```

This is a specific manifestation of Bug 10 (three-part references broken) for SELECT * expansion. When `*` is expanded, the schema qualifier is lost, causing the column names to appear ambiguous.

**Root cause:** The `SELECT *` expansion doesn't preserve the database context in the generated column expressions. The code at `core/translate/select.rs:457` sets `database: None` for expanded columns.

**Severity:** High — prevents basic cross-DB queries involving same-name tables without workarounds.

---

## Bug 41: Non-PK indexes completely ignored by optimizer on attached databases (additional confirmation with covering, expression, partial, multi-column, MIN/MAX, BETWEEN, ORDER BY)

**Extended repro from Bug 11, confirming ALL non-PK index types are affected:**

```sql
ATTACH ':memory:' AS aux;
CREATE TABLE aux.t(id INTEGER PRIMARY KEY, val TEXT, score INTEGER, category TEXT);
CREATE INDEX aux.idx_score ON t(score);
CREATE INDEX aux.idx_cat ON t(category);
CREATE INDEX aux.idx_multi ON t(score, val);
CREATE INDEX aux.idx_expr ON t(lower(val));
CREATE INDEX aux.idx_partial ON t(val) WHERE score > 0;
INSERT INTO aux.t VALUES(1,'a',10,'A'),(2,'b',20,'B'),(3,'c',30,'A');

-- ALL of these do SCAN on attached (should use index):
EXPLAIN QUERY PLAN SELECT * FROM aux.t WHERE score = 20;         -- SCAN (should SEARCH idx_score)
EXPLAIN QUERY PLAN SELECT * FROM aux.t WHERE score BETWEEN 10 AND 30;  -- SCAN
EXPLAIN QUERY PLAN SELECT * FROM aux.t ORDER BY score;           -- SCAN + SORTER
EXPLAIN QUERY PLAN SELECT MIN(score) FROM aux.t;                 -- SCAN (should use covering index)
EXPLAIN QUERY PLAN SELECT val FROM aux.t WHERE score > 15;       -- SCAN (covering index not used)
EXPLAIN QUERY PLAN SELECT * FROM aux.t WHERE lower(val) = 'a';   -- SCAN (expression index not used)
EXPLAIN QUERY PLAN SELECT * FROM aux.t WHERE score = 20 AND category = 'B';  -- SCAN (multi-column not used)
```

All equivalent queries on main correctly use their respective indexes.

**Bytecode evidence (equality search):**
```
-- Main: OpenRead using index root page, SeekGE + IdxGT
-- Attached: OpenRead using table root page, Rewind + Next + Ne comparison
```

The optimizer DOES correctly handle:
- PK (rowid) lookups on attached DBs in JOINs (when table names differ)
- Single-table PK WHERE clause on attached DBs

**Root cause:** The index selection logic in the optimizer doesn't search the attached database schema for available indexes. It only considers indexes from the main schema.

**Severity:** Critical — every non-PK indexed query on attached databases does O(n) full table scan instead of O(log n) index lookup. This makes attached databases unsuitable for any performance-sensitive workload.
