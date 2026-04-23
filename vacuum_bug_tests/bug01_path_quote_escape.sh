#!/usr/bin/env bash
# Bug 1: VACUUM INTO doesn't unescape doubled single quotes in path.
# VACUUM-specific: only the VACUUM INTO code path takes a user-supplied
# string literal as a filesystem path; the buggy `trim_matches` call lives
# in core/translate/vacuum.rs::extract_path_from_expr.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"

$TURSO "$D/src.db" 2>/dev/null <<'SQL'
CREATE TABLE t(a);
INSERT INTO t VALUES (1);
VACUUM INTO '/tmp/foo''.db';
SQL

echo '---Expected SQLite behaviour: filename "foo'\''.db" (one quote)---'
SQ="/home/ubuntu/sqlite/sqlite3"
rm -f "$D/sq_src.db" /tmp/sq_foo\'.db
$SQ "$D/sq_src.db" "CREATE TABLE t(a); INSERT INTO t VALUES (1); VACUUM INTO '/tmp/sq_foo''.db';"
ls /tmp/sq_foo\'.db 2>&1
rm -f /tmp/sq_foo\'.db

echo "---Turso: filename written (note literal two quotes)---"
ls "/tmp/foo''.db" 2>&1
rm -f "/tmp/foo''.db"

# Bug manifests: /tmp/foo''.db is created (wrong); expected /tmp/foo'.db.
