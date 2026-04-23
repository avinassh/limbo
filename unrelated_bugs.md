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

