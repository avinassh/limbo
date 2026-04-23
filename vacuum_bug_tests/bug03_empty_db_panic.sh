#!/usr/bin/env bash
# Bug 3: In-place VACUUM InternalError on never-initialized (empty) database.
# VACUUM-specific: begin_exclusive_tx (called from VacuumInPlacePhase::BeginSourceTx)
# asserts page 1 is allocated. VACUUM INTO succeeds on the same empty DB.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"

echo '---VACUUM on truly empty DB---'
$TURSO "$D/empty.db" "VACUUM;" 2>&1 | grep -v warning | head

echo ''
echo '---SQLite oracle: no error, VACUUM is a no-op---'
/home/ubuntu/sqlite/sqlite3 "$D/sq_empty.db" "VACUUM;" 2>&1; echo "exit=$?"

echo ''
echo '---VACUUM INTO on same empty DB (works)---'
$TURSO "$D/empty2.db" "VACUUM INTO '$D/out.db';" 2>&1 | grep -v warning | head
ls "$D/out.db" 2>/dev/null && echo "dest created"

# Bug manifests: in-place VACUUM errors with
# "begin_exclusive_tx can be done on an initialized database (page 1 must already be allocated)".
