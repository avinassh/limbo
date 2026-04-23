#!/usr/bin/env bash
# Bug 7: PRAGMA page_size=N; VACUUM does not change page_size.
# VACUUM-specific: both VacuumInPlacePhase::ReadSourceMetadata and
# VacuumIntoOpPhase::Init read page_size from the source pager (current value)
# instead of honouring the pending pragma override.
#
# In-place VACUUM is hard to oracle against SQLite because Turso opens all
# DBs in WAL mode and SQLite can't change page_size on a WAL-mode DB. So we
# focus on VACUUM INTO, where Turso's default output is WAL-mode but the
# dest is fresh, so SQLite has no such limitation on its side.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"
SQ="/home/ubuntu/sqlite/sqlite3"

$TURSO "$D/t.db" "CREATE TABLE t(a); INSERT INTO t VALUES(1);" 2>/dev/null
$SQ "$D/sq.db" "CREATE TABLE t(a); INSERT INTO t VALUES(1);"

echo '---Source page sizes (both 4096)---'
xxd -s 16 -l 2 "$D/t.db";  echo " (Turso source)"
xxd -s 16 -l 2 "$D/sq.db"; echo " (SQLite source)"

echo ''
echo '---Turso VACUUM INTO with pending page_size=8192 pragma: dest still 4096---'
$TURSO "$D/t.db" "PRAGMA page_size=8192; VACUUM INTO '$D/t_out.db';" 2>/dev/null
xxd -s 16 -l 2 "$D/t_out.db"
$TURSO "$D/t_out.db" "PRAGMA page_size;" 2>/dev/null

echo '---SQLite VACUUM INTO same pragma: dest correctly 8192---'
$SQ "$D/sq.db" "PRAGMA page_size=8192; VACUUM INTO '$D/sq_out.db';"
xxd -s 16 -l 2 "$D/sq_out.db"
$SQ "$D/sq_out.db" "PRAGMA page_size;"

echo ''
echo '---In-place Turso VACUUM also ignores pending pragma---'
$TURSO "$D/t.db" "PRAGMA page_size=8192; VACUUM; PRAGMA page_size;" 2>/dev/null
echo '(Turso: reports 4096 / header stays at 0x1000)'

# Bug manifests: Turso VACUUM INTO dest has page_size 0x1000 (4096) even when
# the source connection set PRAGMA page_size=8192 first. SQLite VACUUM INTO
# correctly produces a dest with page_size 0x2000 (8192).
