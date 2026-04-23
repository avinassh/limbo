#!/usr/bin/env bash
# Bug 4: VACUUM schema-name comparison is case-sensitive.
# VACUUM-specific: translate_vacuum (core/translate/vacuum.rs:47-53) compares
# the raw schema name with != "main" instead of case-insensitive.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"
SQ="/home/ubuntu/sqlite/sqlite3"

for DB in "$D/a.db" "$D/b.db" "$D/c.db"; do
  $TURSO "$DB" "CREATE TABLE t(a); INSERT INTO t VALUES (1);" 2>/dev/null
done

echo '---VACUUM MAIN (upper) — expected ok, actual Parse error---'
$TURSO "$D/a.db" "VACUUM MAIN;" 2>&1 | grep -v warning | head

echo '---VACUUM Main (mixed) — expected ok, actual Parse error---'
$TURSO "$D/b.db" "VACUUM Main;" 2>&1 | grep -v warning | head

echo '---VACUUM main (lower) — ok---'
$TURSO "$D/c.db" "VACUUM main;" 2>&1 | grep -v warning | head; echo "ok"

echo ''
echo '---SQLite oracle: all three accepted---'
$SQ "$D/a.db" "VACUUM MAIN;"; echo "MAIN exit=$?"
$SQ "$D/b.db" "VACUUM Main;"; echo "Main exit=$?"
$SQ "$D/c.db" "VACUUM main;"; echo "main exit=$?"

# Bug manifests: MAIN/Main rejected with "VACUUM is only supported for the main database".
