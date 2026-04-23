#!/usr/bin/env bash
# Bug 18: VACUUM fails on SQLite-created databases with WITHOUT ROWID tables.
# VACUUM-specific: vacuum_target_build_step calls target_conn.prepare() on
# each source-table CREATE SQL. Turso's parser rejects WITHOUT ROWID, so the
# replay errors even though the source DB is otherwise readable. Includes
# FTS5/RTREE because they create WITHOUT ROWID backing tables.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"
SQ="/home/ubuntu/sqlite/sqlite3"

echo '---Plain WITHOUT ROWID user table (SQLite-created, Turso VACUUM)---'
$SQ "$D/wor.db" "CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT) WITHOUT ROWID;
                 INSERT INTO t VALUES (1, 'hello'), (2, 'world');"
echo 'SQLite source sql:'
$SQ "$D/wor.db" "SELECT sql FROM sqlite_master;"

echo ''
echo 'Turso: VACUUM fails because target-build replay hits the WITHOUT ROWID parser rejection:'
$TURSO "$D/wor.db" "VACUUM;" 2>&1 | grep -v '^warning' | head

echo ''
echo '---SQLite oracle: VACUUM on same DB works and preserves rows---'
$SQ "$D/wor.db" "VACUUM; SELECT * FROM t;"

echo ''
echo '---FTS5 / RTREE note: same failure class applies---'
# SQLite's FTS5 and RTREE virtual tables create *backing* tables that are
# declared WITHOUT ROWID (e.g., fts_idx, fts_config, rtree_node). Turso's
# fts5 virtual-table module isn't registered so we can't open such a DB
# cleanly in a script; but the underlying class of failure is the same —
# any SQLite-created DB with WITHOUT ROWID anywhere in its sqlite_master
# breaks Turso's VACUUM target-build parse.

# Bug manifests: "WITHOUT ROWID tables are not supported". The DB is partially
# usable (SELECTs on unrelated tables work) but VACUUM fails permanently.
