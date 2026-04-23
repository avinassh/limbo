#!/usr/bin/env bash
# Bug 24: VACUUM fails after ALTER TABLE RENAME COLUMN on a column that's
# referenced by an expression index or a COLLATE-suffixed index column.
# VACUUM-specific failure mode: ALTER TABLE RENAME COLUMN leaves stale
# `old_col` references in the stored CREATE INDEX SQL (U17). Turso's VACUUM
# target build does prepare() on each CREATE INDEX, which rejects the stale
# column and aborts. The DB remains readable but is un-vacuumable.
#
# RENAME COLUMN correctly updates direct refs, partial index WHERE, CHECK,
# triggers, generated column expressions, and FK refs — only CREATE INDEX
# expression entries and COLLATE suffixes are missed.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"
SQ="/home/ubuntu/sqlite/sqlite3"

SETUP="
CREATE TABLE t(old_col INTEGER, b TEXT);
CREATE INDEX ix_direct  ON t(old_col);
CREATE INDEX ix_expr    ON t(old_col * 2);
CREATE INDEX ix_partial ON t(b) WHERE old_col > 0;
CREATE INDEX ix_collate ON t(old_col COLLATE BINARY);
INSERT INTO t VALUES (1, 'x');
ALTER TABLE t RENAME COLUMN old_col TO new_col;
SELECT sql FROM sqlite_master WHERE type='index';
"

echo '---Turso schema after RENAME (ix_expr and ix_collate have stale old_col)---'
$TURSO "$D/t.db" "$SETUP" 2>/dev/null

echo '---Turso: VACUUM fails replaying CREATE INDEX with stale name---'
$TURSO "$D/t.db" "VACUUM;" 2>&1 | grep -v warning | head

echo '---Turso: VACUUM INTO has the same failure---'
$TURSO "$D/t.db" "VACUUM INTO '$D/out.db';" 2>&1 | grep -v warning | head
# Note: for this specific failure point (PrepareCreateIndex, after tables
# created + data copied) the dest happens NOT to be leaked — different from
# Bug 20 (WITHOUT ROWID, fails at PrepareCreateTable, dest is leaked).
ls "$D/out.db"* 2>/dev/null || echo '(dest not written — no leak here)'

echo ''
echo '---SQLite oracle: RENAME correctly rewrites all index SQL---'
$SQ "$D/sq.db" "$SETUP"
$SQ "$D/sq.db" "VACUUM;" 2>&1; echo "sq VACUUM exit=$?"

# Bug manifests: Turso's VACUUM and VACUUM INTO fail with
# "invalid expression in CREATE INDEX: old_col * 2" (or similar for the
# COLLATE index). SQLite successfully rewrites both index SQLs during
# RENAME and VACUUMs cleanly.
