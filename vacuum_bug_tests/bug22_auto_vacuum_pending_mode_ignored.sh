#!/usr/bin/env bash
# Bug 22: PRAGMA auto_vacuum=MODE; VACUUM; does not apply the pending mode.
# VACUUM-specific: VacuumInPlacePhase::ReadSourceMetadata and
# VacuumIntoOpPhase::Init set target_auto_vacuum_mode by calling
# source_pager.get_auto_vacuum_mode() which returns the pager's *current*
# mode, ignoring the pending pragma override that SQLite's vacuum.c honours.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- --experimental-autovacuum -m list -q"
SQ="/home/ubuntu/sqlite/sqlite3"

echo '---Case A: enable auto_vacuum via pragma+VACUUM on a non-AV source---'
$TURSO "$D/a.db" "CREATE TABLE t(a); INSERT INTO t VALUES(1);" 2>/dev/null
cp "$D/a.db" "$D/sa.db"

$TURSO "$D/a.db" "PRAGMA auto_vacuum=FULL; VACUUM; PRAGMA auto_vacuum;" 2>/dev/null
echo 'Turso header byte 52 (largest_root_btree_page):'
xxd -s 52 -l 4 "$D/a.db"

$SQ "$D/sa.db" "PRAGMA auto_vacuum=FULL; VACUUM; PRAGMA auto_vacuum;"
echo 'SQLite header byte 52:'
xxd -s 52 -l 4 "$D/sa.db"

echo ''
echo '---Case B: disable auto_vacuum via pragma+VACUUM on a FULL source---'
$SQ "$D/b.db" "PRAGMA auto_vacuum=FULL; CREATE TABLE t(a); INSERT INTO t VALUES(1);"
cp "$D/b.db" "$D/tb.db"
cp "$D/b.db" "$D/sb.db"

$TURSO "$D/tb.db" "PRAGMA auto_vacuum=NONE; VACUUM; PRAGMA auto_vacuum;" 2>/dev/null
echo 'Turso header byte 52:'
xxd -s 52 -l 4 "$D/tb.db"

$SQ "$D/sb.db" "PRAGMA auto_vacuum=NONE; VACUUM; PRAGMA auto_vacuum;"
echo 'SQLite header byte 52:'
xxd -s 52 -l 4 "$D/sb.db"

# Bug manifests:
#  Case A Turso:   00000034: 0000 0000 (unchanged); SQLite: 0000 0003 (FULL)
#  Case B Turso:   00000034: 0000 0003 (still FULL); SQLite: 0000 0000 (NONE)
