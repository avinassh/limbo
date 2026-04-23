#!/usr/bin/env bash
# Bug 19: VACUUM INTO writes an extra empty .db-wal sidecar file.
# VACUUM-specific: op_vacuum_into_inner opens the dest via Database::open_file_with_flags
# which defaults to WAL mode. Even after the final TRUNCATE checkpoint, the
# zero-byte .db-wal file remains. SQLite's VACUUM INTO opens dest in rollback
# mode and produces a single .db file.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"
SQ="/home/ubuntu/sqlite/sqlite3"

$TURSO "$D/src.db" "CREATE TABLE t(a); INSERT INTO t VALUES(1);" 2>/dev/null
cp "$D/src.db" "$D/sq_src.db"

$TURSO "$D/src.db" "VACUUM INTO '$D/t_out.db';" 2>/dev/null
echo '---Turso VACUUM INTO output: .db + zero-byte .db-wal sidecar---'
ls -la "$D/t_out.db"* 2>/dev/null

$SQ "$D/sq_src.db" "VACUUM INTO '$D/sq_out.db';"
echo '---SQLite VACUUM INTO output: only .db (no sidecar)---'
ls -la "$D/sq_out.db"* 2>/dev/null

# Bug manifests: Turso leaves dest.db-wal (0 bytes) alongside dest.db.
