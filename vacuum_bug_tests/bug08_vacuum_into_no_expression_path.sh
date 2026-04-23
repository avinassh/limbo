#!/usr/bin/env bash
# Bug 8: VACUUM INTO does not accept expressions or parameter binding for the path.
# VACUUM-specific: core/translate/vacuum.rs:67-85 extract_path_from_expr only
# matches Expr::Literal(Literal::String) or Expr::Id.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"

$TURSO "$D/t.db" "CREATE TABLE t(a); INSERT INTO t VALUES (1);" 2>/dev/null

echo "---Turso rejects all: expression, parameter, named parameter---"
$TURSO "$D/t.db" "VACUUM INTO '/tmp/' || 'foo.db';" 2>&1 | grep -v warning | head -1
$TURSO "$D/t.db" "VACUUM INTO ?;" 2>&1 | grep -v warning | head -1
$TURSO "$D/t.db" "VACUUM INTO :dest;" 2>&1 | grep -v warning | head -1

echo ''
echo "---SQLite oracle: expression path works---"
/home/ubuntu/sqlite/sqlite3 "$D/t.db" "VACUUM INTO '/tmp/' || 'sq_foo.db';"
ls /tmp/sq_foo.db && rm -f /tmp/sq_foo.db

# Bug manifests: each form emits "VACUUM INTO requires a string literal path".
