#!/usr/bin/env bash
# Bug 9: VACUUM adds a spurious sqlite_autoindex_* row for __turso_internal_types.
# VACUUM-specific: vacuum_target_build_step replays the source's CREATE TABLE
# SQL, which registers the implicit PK autoindex in sqlite_master. The source's
# bootstrap path never wrote that row, so VACUUM adds a new permanent row.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- --experimental-custom-types -m list -q"

$TURSO "$D/t.db" 2>/dev/null <<'SQL'
CREATE TYPE pos_int BASE INTEGER;
CREATE TABLE t(a pos_int);
SQL

echo '---Before VACUUM: sqlite_master---'
$TURSO "$D/t.db" "SELECT type, name FROM sqlite_master;" 2>/dev/null

echo '---VACUUM---'
$TURSO "$D/t.db" "VACUUM;" 2>/dev/null

echo '---After VACUUM: sqlite_master (spurious sqlite_autoindex___turso_internal_types_1 row)---'
$TURSO "$D/t.db" "SELECT type, name FROM sqlite_master;" 2>/dev/null

# Bug manifests: before=2 rows (t, __turso_internal_types); after=3 rows
# (adds sqlite_autoindex___turso_internal_types_1). Running VACUUM again
# keeps the extra row; there is no way back.
