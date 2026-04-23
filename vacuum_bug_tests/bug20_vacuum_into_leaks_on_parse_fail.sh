#!/usr/bin/env bash
# Bug 20: VACUUM INTO parse-time failure leaks the destination file.
# VACUUM-specific: extension of Bug 14's cleanup gap, triggered by the
# target-build CREATE TABLE parser rejection (Bug 18's WITHOUT ROWID). The
# cleanup_op_vacuum_into function does not unlink on parse-error paths either.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"
SQ="/home/ubuntu/sqlite/sqlite3"

# A SQLite FTS5-backed source has backing tables whose schema includes
# WITHOUT ROWID clauses. Turso's schema load tolerates these (schemas that
# don't parse are skipped) so the connection opens. VACUUM INTO then reaches
# the target-build CREATE TABLE replay and fails the WITHOUT ROWID parser
# rejection, AFTER having already opened the dest file — so the dest leaks.
$SQ "$D/fts.db" "CREATE VIRTUAL TABLE fts USING fts5(c); INSERT INTO fts VALUES('hello');"

OUT="$D/fts_dst.db"
echo '---VACUUM INTO against SQLite FTS5 source: fails at CREATE TABLE replay---'
$TURSO "$D/fts.db" "VACUUM INTO '$OUT';" 2>&1 | grep -v '^warning' | head -3

echo ''
echo 'Leaked files (dest + zero-byte WAL sidecar):'
ls -la "${OUT}"* 2>/dev/null

echo ''
echo '---Retry: preflight existence check rejects (no cleanup, so user is stuck)---'
$TURSO "$D/fts.db" "VACUUM INTO '$OUT';" 2>&1 | grep -v '^warning' | head -3
