#!/usr/bin/env bash
# Bug 12: VACUUM INTO rejects pre-existing zero-length destination files.
# VACUUM-specific: core/vdbe/execute.rs:14394 calls Path::exists() as a
# preflight and rejects with "output file already exists" for any existing
# dentry — including a 0-byte file that SQLite is happy to fill.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"
SQ="/home/ubuntu/sqlite/sqlite3"

$TURSO "$D/src.db" "CREATE TABLE t(a); INSERT INTO t VALUES(1);" 2>/dev/null

echo '---SQLite oracle: zero-byte dest accepted---'
touch "$D/sq_out.db"
$SQ "$D/src.db" "VACUUM INTO '$D/sq_out.db';" && echo "sq ok, size=$(stat -c%s $D/sq_out.db)"

echo '---Turso: zero-byte dest rejected---'
touch "$D/out.db"
$TURSO "$D/src.db" "VACUUM INTO '$D/out.db';" 2>&1 | grep -v warning | head

# Bug manifests: Turso errors with "output file already exists" on a 0-byte
# placeholder; SQLite fills it happily.
