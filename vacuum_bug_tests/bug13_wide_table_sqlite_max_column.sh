#!/usr/bin/env bash
# Bug 13: VACUUM fails on user tables with SQLITE_MAX_COLUMN columns.
# VACUUM-specific: build_copy_sql prepends a rowid alias pseudo-column to
# the SELECT whenever has_rowid is true, pushing a 2000-column source's SELECT
# to 2001 columns which exceeds SQLITE_MAX_COLUMN.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"
SQ="/home/ubuntu/sqlite/sqlite3"

# Build a CREATE TABLE with exactly 2000 columns (no INTEGER PRIMARY KEY).
python3 -c "
cols = ','.join(f'c{i} INTEGER DEFAULT 0' for i in range(1, 2001))
print(f'CREATE TABLE t({cols});')
print('INSERT INTO t DEFAULT VALUES;')
" > "$D/schema.sql"

echo '---Turso: VACUUM fails at SQLITE_MAX_COLUMN boundary (SELECT has 2001 cols)---'
$TURSO "$D/t.db" < "$D/schema.sql" 2>/dev/null
$TURSO "$D/t.db" "VACUUM;" 2>&1 | grep -v warning | head -2

echo ''
echo '---SQLite oracle: VACUUM succeeds (xfer path, no column limit)---'
$SQ "$D/sq.db" < "$D/schema.sql"
$SQ "$D/sq.db" "VACUUM;" 2>&1; echo "sq exit=$?"

# Bug manifests: Turso errors with "too many columns in result set"; SQLite ok.
