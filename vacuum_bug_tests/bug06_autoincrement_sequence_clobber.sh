#!/usr/bin/env bash
# Bug 6: VACUUM clobbers sqlite_sequence.seq when manual value < max(rowid).
# VACUUM-specific: core/vdbe/vacuum.rs:725 notes that Turso lacks a way to
# disable AUTOINCREMENT during VACUUM copy, so each INSERT fires the target
# counter machinery and overwrites the preserved seq.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"
SQ="/home/ubuntu/sqlite/sqlite3"

CMDS="
CREATE TABLE t(id INTEGER PRIMARY KEY AUTOINCREMENT, val);
INSERT INTO t(val) VALUES('a');
INSERT INTO t(id,val) VALUES(100, 'b');
UPDATE sqlite_sequence SET seq = 50 WHERE name = 't';
SELECT 'before:', seq FROM sqlite_sequence;
VACUUM;
SELECT 'after:',  seq FROM sqlite_sequence;
"

echo '---Turso---'
$TURSO "$D/t.db" "$CMDS" 2>/dev/null

echo '---SQLite oracle---'
$SQ "$D/sq.db" "$CMDS"

# Bug manifests: Turso reports seq=100 after VACUUM; SQLite reports seq=50.
