#!/usr/bin/env bash
# Bug 21: VACUUM INTO destination header file-format bytes (offset 18-19) are
# always 02 02 (WAL), diverging from SQLite's 01 01 (rollback).
# VACUUM-specific: op_vacuum_into_inner opens the dest in Turso's default
# WAL mode without honouring the source's journal_mode bytes.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"
SQ="/home/ubuntu/sqlite/sqlite3"

$SQ "$D/src.db" "CREATE TABLE t(a); INSERT INTO t VALUES(1);"
echo '---Source (SQLite, rollback mode): offset 18-19 = 01 01---'
xxd -s 18 -l 2 "$D/src.db"

cp "$D/src.db" "$D/src_t.db"
cp "$D/src.db" "$D/src_s.db"
$TURSO "$D/src_t.db" "VACUUM INTO '$D/t_out.db';" 2>/dev/null
$SQ "$D/src_s.db" "VACUUM INTO '$D/sq_out.db';"

echo '---Turso dest (WAL bytes): 02 02 (diverges from source)---'
xxd -s 18 -l 2 "$D/t_out.db"
echo '---SQLite dest: 01 01 (preserves source, rollback)---'
xxd -s 18 -l 2 "$D/sq_out.db"

# Bug manifests: file_format_write_version and file_format_read_version bytes
# of the dest header diverge from SQLite's expected output even for identical
# rollback-mode sources.
