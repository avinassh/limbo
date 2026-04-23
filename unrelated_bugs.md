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

