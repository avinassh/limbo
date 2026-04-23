#!/usr/bin/env bash
# Bug 11: VACUUM fails when copying CHECK-violating rows that were seeded via
# PRAGMA ignore_check_constraints or ALTER TABLE ADD COLUMN.
# VACUUM-specific: build_copy_sql+INSERT path. SQLite's vacuum.c uses xfer
# with constraints disabled; Turso uses regular INSERT which re-evaluates
# CHECK on every copied row.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"
SQ="/home/ubuntu/sqlite/sqlite3"

SETUP="
CREATE TABLE t(a INTEGER CHECK(a > 0));
PRAGMA ignore_check_constraints=ON;
INSERT INTO t VALUES(-5);
SELECT 'inserted:', * FROM t;
"

echo '---Turso: VACUUM errors on copy---'
$TURSO "$D/t.db" "$SETUP" 2>/dev/null
$TURSO "$D/t.db" "VACUUM;" 2>&1 | grep -v warning | head -2

echo '---SQLite oracle: VACUUM preserves the violating row---'
$SQ "$D/sq.db" "$SETUP"
$SQ "$D/sq.db" "VACUUM; SELECT * FROM t;"

# Bug manifests: Turso errors with "CHECK constraint failed: a > 0 (19)".
# SQLite succeeds and prints -5.
