#!/usr/bin/env bash
# Bug 17: VACUUM rewrites header change_counter to 1, losing source's value.
# VACUUM-specific: VacuumDbHeaderMeta::from_source_header does not copy
# change_counter (offset 24-27) or version_valid_for (offset 92-95), so the
# target header is built with change_counter=1. For the in-place path, the
# copy-back then writes that fresh page 1 over the source, overwriting any
# higher counter that SQLite had bumped the source to.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"
SQ="/home/ubuntu/sqlite/sqlite3"

# Seed a DB with SQLite so the counter has actually been bumped past 1.
$SQ "$D/t.db" "CREATE TABLE t(a); INSERT INTO t VALUES(1);
              INSERT INTO t VALUES(2); INSERT INTO t VALUES(3);
              INSERT INTO t VALUES(4);INSERT INTO t VALUES(5);"

echo '---change_counter at offset 24-27 (big-endian u32) BEFORE Turso VACUUM---'
xxd -s 24 -l 4 "$D/t.db"

$TURSO "$D/t.db" "VACUUM;" 2>/dev/null

echo '---change_counter AFTER Turso VACUUM (should be >= source; actual: 1)---'
xxd -s 24 -l 4 "$D/t.db"

echo '---version_valid_for at offset 92-95 also frozen at 0x002e7e58 (3047000)---'
xxd -s 92 -l 4 "$D/t.db"

# Bug manifests: change_counter resets from the SQLite-bumped value back to 1.
# SQLite's VACUUM always INCREMENTS change_counter rather than resetting it.
