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
