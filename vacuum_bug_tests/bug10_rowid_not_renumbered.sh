#!/usr/bin/env bash
# Bug 10: VACUUM does not renumber rowids for tables without INTEGER PRIMARY KEY.
# VACUUM-specific: core/vdbe/vacuum.rs::build_copy_sql prepends a rowid-alias
# pseudo-column whenever has_rowid is true, so rowids are preserved verbatim.
# SQLite documents that VACUUM MAY change rowids for tables without IPK, and
# its page/xfer copy path renumbers contiguously.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"
SQ="/home/ubuntu/sqlite/sqlite3"

CMDS="
CREATE TABLE t(a TEXT);
INSERT INTO t VALUES('a'), ('b'), ('c'), ('d');
DELETE FROM t WHERE a IN ('b','c');
SELECT 'pre:', rowid, a FROM t;
VACUUM;
SELECT 'post:', rowid, a FROM t;
"

echo '---Turso: rowids stay 1,4 (not renumbered)---'
$TURSO "$D/t.db" "$CMDS" 2>/dev/null

echo '---SQLite oracle: rowids renumber to 1,2---'
$SQ "$D/sq.db" "$CMDS"

# Bug manifests: Turso pre=(1,4), post=(1,4); SQLite pre=(1,4), post=(1,2).
