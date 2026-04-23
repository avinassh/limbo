# Unrelated Bugs Found

## U1: Recursive trigger doesn't re-fire

```
CREATE TABLE t (a INTEGER PRIMARY KEY);
CREATE TRIGGER t_rec AFTER INSERT ON t
WHEN NEW.a < 10 BEGIN
  INSERT INTO t VALUES (NEW.a + 1);
END;
INSERT INTO t VALUES (1);
SELECT * FROM t;
-- SQLite (with PRAGMA recursive_triggers=ON): 1..10
-- Turso: just 1, 2
```

The recursive trigger fires once then stops. Turso doesn't appear to honor
`recursive_triggers` even with it explicitly ON.

## U2: PRAGMA auto_vacuum doesn't persist the mode

```
tursodb --experimental-autovacuum fresh.db "PRAGMA auto_vacuum=FULL; CREATE TABLE t(a); PRAGMA auto_vacuum;"
-- Returns 0 (not 1 for FULL).
```

Setting `PRAGMA auto_vacuum=FULL` (or `=1` / `=2`) on a fresh DB before any
tables reports 0 back when querying. Looked at the translate code — it
unconditionally calls `persist_auto_vacuum_mode` but the query returns 0,
so something between the set and the get is swallowing the mode. VACUUM
still rejects Incremental via `reject_unsupported_vacuum_auto_vacuum_mode`,
so the rejection path is unreachable in practice.

## U3: Comments/whitespace in CREATE TABLE SQL are normalized

SQLite preserves the original CREATE TABLE source (newlines, comments,
extra whitespace) in sqlite_master. Turso's parser re-stringifies after
parse, losing the formatting. Not a VACUUM bug — happens on the initial
CREATE TABLE — but VACUUM makes this more visible because users commonly
check sqlite_master to verify VACUUM didn't disturb their schema.

## U4: Confusing error message for unrelated filesystem failures

```
VACUUM INTO '/nonexistent/dir/out.db';
-- Error: I/O error (statfs shared WAL coordination path): entity not found
```

The error references "shared WAL coordination path" even when the problem
is that the output directory doesn't exist. This leaks internal state into
the user-facing error.

## U5: Stack overflow on self-referential FK + `ON DELETE CASCADE`

```
PRAGMA foreign_keys=ON;
CREATE TABLE t(id INTEGER PRIMARY KEY, parent INTEGER
  REFERENCES t(id) ON DELETE CASCADE);
INSERT INTO t VALUES(1, NULL), (2, 1), (3, 2), (4, 2);
DELETE FROM t WHERE id=1;
-- thread 'main' has overflowed its stack
-- fatal runtime error: stack overflow, aborting
```

Crashes the process with a stack overflow instead of performing the cascade
deletion or returning a controlled error. Not a VACUUM bug — pure
FK/trigger evaluation issue — but the crash is severe (SIGABRT, no recovery).

## U6: MVCC databases silently ignore `PRAGMA page_size`

```
$ tursodb fresh.db "PRAGMA journal_mode='mvcc'; PRAGMA page_size=8192;
                    CREATE TABLE t(a); PRAGMA page_size;"
4096   -- expected 8192
```

`PRAGMA page_size` works on non-MVCC fresh DBs but is silently dropped on
MVCC-mode fresh DBs. Not a VACUUM bug; surfaced while testing VACUUM
page_size handling under MVCC.

## U7: CLI fails to open an encrypted DB on second invocation

The CLI reads page 1 eagerly during `open`, before any user SQL runs. That
means for an encrypted database, the first `PRAGMA cipher`/`hexkey` pair
after `open` arrives too late — header magic check rejects the file as
"not a database". Workaround requires passing the encryption key via CLI
flags that don't currently exist (`--cipher`/`--hexkey`). Only the original
create-and-use session works; any subsequent `tursodb file.db ...` command
fails. Not VACUUM-specific, but bit me while investigating Bug 5.


## U8: Stack overflow on CHECK constraint with many AND clauses during INSERT

```
CREATE TABLE t(a INTEGER CHECK(a != 0 AND a != 1 AND ... AND a != 79));
INSERT INTO t VALUES(999);
-- thread 'main' has overflowed its stack
-- fatal runtime error: stack overflow, aborting
```

Crash occurs at 80 AND clauses (70 works). Deep recursive AST evaluation
during constraint checking. The CREATE TABLE succeeds and stores the
schema, so subsequent INSERT and VACUUM attempts both abort the whole
process. Not VACUUM-specific, but VACUUM is unrecoverable on any such DB.

## U9: sqlite_master.name column is stored lowercased

```
$ tursodb x.db 'Create Table MyTable(MyColumn INTEGER);'
$ tursodb x.db 'SELECT name, sql FROM sqlite_master;'
mytable|CREATE TABLE MyTable (MyColumn INTEGER)
```

Turso stores the TABLE name in `sqlite_master.name` lowercased (`mytable`),
while `sql` preserves the original case (`MyTable`). SQLite preserves the
exact case in both (`MyTable|CREATE TABLE MyTable(...)`).

Introspection tools that read `sqlite_master.name` for display or for
case-sensitive matching against another store break on Turso. Not a VACUUM
bug (surfaces on the initial CREATE TABLE), but it affects users who inspect
sqlite_master after VACUUM.

## U10: PRAGMA ignore_check_constraints accepted but scoped to a single statement only

`PRAGMA ignore_check_constraints=ON;` is accepted as valid SQL by Turso. It
is effective inside the same statement batch as an INSERT that would
otherwise fail CHECK — the row is persisted. But the effect does not persist
for a subsequent VACUUM issued later in the same connection (see Bug 11).
The exact scope of the pragma (compile-time flag on individual statements?
connection-wide?) is unclear, and the flag is not mirrored to the VACUUM
target connection anyway.

## U11: Cryptic "statfs shared WAL coordination path" error for any I/O open failure on VACUUM INTO

```
VACUUM INTO '/nonexistent/dir/out.db';
-- Error: I/O error (statfs shared WAL coordination path): entity not found

VACUUM INTO 'file:/tmp/out.db';
-- Error: I/O error (statfs shared WAL coordination path): entity not found
```

The "shared WAL coordination path" part of the message leaks internal state
about the WAL file coordination mechanism, and it surfaces for every open
failure (parent directory missing, unsupported URI path, etc.). This is an
extension of U4 — now observed in three distinct paths — suggesting the
error lives in a generic `statfs` wrapper that all file opens go through.

## U12: DROP TABLE / DROP INDEX doesn't clean up sqlite_stat1 rows

SQLite removes stat1 rows when the owning table or index is dropped, so
`sqlite_stat1` never accumulates entries for objects that no longer exist.
Turso's DROP doesn't touch `sqlite_stat1`, so stale rows accumulate for
any object that was ANALYZEd before being dropped.

```
CREATE TABLE t1(a);
CREATE INDEX ix1 ON t1(a);
INSERT INTO t1 VALUES(1);
ANALYZE;
DROP TABLE t1;
SELECT * FROM sqlite_stat1;
-- SQLite: no rows
-- Turso:  t1||1, t1|ix1|1 1   (stale rows for dropped table)
```

Same behaviour for `DROP INDEX`. Not a VACUUM bug — happens independent of
VACUUM — but VACUUM happily copies the stale stat rows to the target, so a
database that survives a VACUUM carries the extra `sqlite_stat1` footprint
forward. Caught while comparing `sqlite_stat1` contents between source and
VACUUM target.

## U13: Turso never increments the header `change_counter` on writes

```
$ sqlite3 /tmp/x.db "CREATE TABLE t(a); INSERT INTO t VALUES(1);
                     INSERT INTO t VALUES(2); INSERT INTO t VALUES(3);"
$ xxd -s 24 -l 4 /tmp/x.db    # 0000 0004 (4)  — SQLite bumped on each write

$ tursodb /tmp/x.db "CREATE TABLE u(b); INSERT INTO u VALUES(1);"
# Turso's change_counter never left 1; SQLite's header bumped to 2 after open.
```

The SQLite format specification (<https://sqlite.org/fileformat.html>)
defines `change_counter` at offset 24 as incrementing on each write
transaction, and pairs with `version_valid_for` at offset 92 to let readers
detect torn writes. Turso writes a new page-1 header but never increments
this counter, so both fields stay at their initial values
(`change_counter=1`, `version_valid_for=3047000` — the SQLite-3.47.0
library version).

Impact: SQLite readers reuse a cached schema across reads based on
`change_counter` comparisons. If the on-disk counter equals the cached
one, SQLite skips re-reading page 1. A Turso writer that never bumps the
counter means SQLite reader processes can miss schema changes or data
changes — they'll reuse a stale cache.

Not a VACUUM bug — happens on every write — but VACUUM specifically
*rewrites* the counter to `1` (see vacuum_bugs.md Bug 17), so a VACUUM
after a SQLite writer's edits can move the counter *backwards*, which
breaks SQLite's "monotonic counter" assumption differently from ordinary
no-op writes.

## U14: `DELETE FROM sqlite_sequence` rejected but `INSERT`/`UPDATE` allowed

```
$ tursodb x.db "CREATE TABLE t(a INTEGER PRIMARY KEY AUTOINCREMENT);
                 INSERT INTO t VALUES(5);
                 INSERT INTO sqlite_sequence VALUES('orphan', 100);   -- allowed
                 UPDATE sqlite_sequence SET seq = 999 WHERE name = 't';  -- allowed
                 DELETE FROM sqlite_sequence WHERE name = 'orphan';   -- rejected!"
  × Parse error: table sqlite_sequence may not be modified
```

SQLite's system tables have a uniform policy: sqlite_sequence is
modifiable by the user for all DML. Turso permits INSERT and UPDATE but
specifically rejects DELETE as "may not be modified" — an inconsistent
split across operations on the same table. Workaround is to `UPDATE
... SET seq = -1` or similar, but that leaves the row and changes its
AUTOINCREMENT-counter interpretation.

Not a VACUUM bug — surfaces on any DELETE attempt — but it blocks the
standard SQLite pattern of "delete orphan sqlite_sequence rows before
VACUUM" that users sometimes run to clean up after DROP TABLE on an
AUTOINCREMENT table.

## U15: `CREATE TEMP VIEW` is stored as a permanent main-schema view

```
$ tursodb x.db "CREATE TABLE t(a);
                CREATE TEMP VIEW tv AS SELECT a*10 FROM t;
                INSERT INTO t VALUES (1),(2);
                SELECT type, name FROM sqlite_master;
                SELECT type, name FROM sqlite_temp_master;"
table|t
view|tv          ← wrong: should be in temp schema only
(temp_master: empty)
```

SQLite correctly places `tv` in `sqlite_temp_master` (the TEMP schema
is destroyed on connection close). Turso instead writes the view to
the persistent `sqlite_master`, so the "temp" view survives across
processes and becomes a normal on-disk view. `CREATE TEMP TABLE` is
handled correctly (temp schema only); the bug is specific to views.

Not a VACUUM bug, but it interacts with VACUUM badly: VACUUM reads
from main `sqlite_master`, sees the misfiled "temp" view, and copies
it forward into the destination. So any VACUUM (in-place or INTO) of
a database where a user ever ran `CREATE TEMP VIEW` carries that view
forward into the cleaned copy, long after the user intended it to be
dropped.

## U16: `ALTER TABLE ADD COLUMN ... GENERATED ALWAYS AS (...) VIRTUAL` drops the `VIRTUAL` keyword from the stored CREATE TABLE SQL

```
$ tursodb x.db "
  CREATE TABLE t(a INTEGER);
  INSERT INTO t VALUES(5);
  ALTER TABLE t ADD COLUMN b INTEGER GENERATED ALWAYS AS (a*2) VIRTUAL;
  SELECT sql FROM sqlite_master;"
CREATE TABLE t (a INTEGER, b INTEGER AS (a * 2))     ← missing VIRTUAL

$ tursodb y.db "
  CREATE TABLE t(a INTEGER, b INTEGER GENERATED ALWAYS AS (a*2) VIRTUAL);
  SELECT sql FROM sqlite_master;"
CREATE TABLE t (a INTEGER, b INTEGER AS (a * 2) VIRTUAL)   ← correct
```

`CREATE TABLE` preserves the `VIRTUAL` keyword; `ALTER TABLE ADD
COLUMN` of the same declaration does not. The column still *behaves*
virtually (inserts into it are rejected with "cannot INSERT into
generated column"), so the only user-visible effect is the malformed
stored SQL. But VACUUM replays that malformed SQL verbatim, so the
post-VACUUM schema continues to miss the `VIRTUAL` suffix.

Not a VACUUM bug, but surfaces via VACUUM because post-VACUUM
introspection (e.g., schema diff tools, dump/restore pipelines) sees
a schema that's missing the storage kind annotation on generated
columns added via ALTER.

## U17: `ALTER TABLE RENAME COLUMN` does not update stale column references in stored `CREATE INDEX` SQL for expression and COLLATE indexes

SQLite's `ALTER TABLE RENAME COLUMN` rewrites every stored SQL string
that references the renamed column — table SQL, index SQL, trigger
bodies, view SELECTs, FK clauses. Turso's implementation updates
most of these but misses two shapes of CREATE INDEX:

- Expression indexes: `CREATE INDEX ix ON t(col * 2)` stays as
  `col * 2` instead of becoming `new_col * 2`.
- Column-with-COLLATE indexes: `CREATE INDEX ix ON t(col COLLATE BINARY)`
  stays as `col COLLATE BINARY` instead of `new_col COLLATE BINARY`.

```
$ tursodb x.db "
  CREATE TABLE t(old_col INTEGER, b TEXT);
  CREATE INDEX ix_expr    ON t(old_col * 2);
  CREATE INDEX ix_collate ON t(old_col COLLATE BINARY);
  ALTER TABLE t RENAME COLUMN old_col TO new_col;
  SELECT sql FROM sqlite_master;"
CREATE TABLE t (new_col INTEGER, b TEXT)                     ← updated
CREATE INDEX ix_expr ON t (old_col * 2)                      ← NOT updated
CREATE INDEX ix_collate ON t (old_col COLLATE BINARY)        ← NOT updated
```

SQLite correctly rewrites both. Turso's schema becomes internally
inconsistent: the table has column `new_col`, the indexes reference
`old_col`. The indexes continue to function at query time because
Turso's runtime resolves column names against the current schema, so
the bug is silent for most users. It turns lethal at VACUUM time — see
`vacuum_bugs.md` Bug 24.

Same class of bug: the `name` column in sqlite_sequence rows for
AUTOINCREMENT tables is stored in lowercase (U9 family — we observed
that `CREATE TABLE MyTable(id INTEGER PRIMARY KEY AUTOINCREMENT);`
followed by `INSERT` leaves `sqlite_sequence.name='mytable'` rather
than SQLite's `'MyTable'`).

## U18: UPDATE on `turso_cdc_version` panics with "cdc_rowid_before_reg must be set"

```
$ tursodb /tmp/x.db "CREATE TABLE t(a);
                     PRAGMA unstable_capture_data_changes_conn='full';
                     INSERT INTO t VALUES (1);
                     UPDATE turso_cdc_version SET version = 'v999';"
thread 'main' panicked at core/translate/emitter/update.rs:2204:42:
cdc_rowid_before_reg must be set
```

Running `UPDATE` against the `turso_cdc_version` system table (which
tracks the CDC schema version per user table) when CDC is enabled
crashes the CLI with a panic rather than returning a user-visible
error. The panic is unconditional — it fires for any UPDATE against
`turso_cdc_version`, including UPDATE of an orphan row. INSERT and
SELECT against the same table work fine, and VACUUM on a DB that has
a populated `turso_cdc_version` table is unaffected.

Not a VACUUM bug — surfaces on UPDATE in any CDC-enabled session —
but it means that users who enable CDC and then try to administer
the per-table version strings crash the process, losing any unwritten
work. Panicking on user-driven DML is a correctness violation; SQLite
would either refuse the statement or succeed.

Also: the pattern "UPDATE on a CDC-managed system table panics" is
broader than this one table. Every UPDATE against `turso_cdc_version`
or similar CDC-tracked internal tables risks the same panic whenever
the emitter path expects a before-register state that the
non-regular-INSERT source never populated.
