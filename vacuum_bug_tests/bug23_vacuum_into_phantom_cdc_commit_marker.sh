#!/usr/bin/env bash
# Bug 23: VACUUM INTO's spurious CDC commit marker is a connection-visible
# phantom row that never lands on disk.
# VACUUM-specific extension of Bug 16: op_vacuum_into_inner's wrapping
# BEGIN/COMMIT on the source emits a change_type=2 CDC row into the source
# connection's in-memory view of turso_cdc, but the row is not durable.
# A checkpoint, connection close, or second process all reveal its absence.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"
SQ="/home/ubuntu/sqlite/sqlite3"

echo '---Session 1: insert + VACUUM INTO, observe 3 CDC rows immediately---'
$TURSO "$D/c.db" 2>/dev/null <<'SQL'
CREATE TABLE t(a);
PRAGMA unstable_capture_data_changes_conn='full';
INSERT INTO t VALUES(1);
SELECT 'pre-INTO:', count(*) FROM turso_cdc;
VACUUM INTO '/tmp/cdc_out.db';
SELECT 'post-INTO (in-session):', count(*) FROM turso_cdc;
PRAGMA wal_checkpoint(FULL);
SELECT 'after checkpoint (in-session):', count(*) FROM turso_cdc;
SQL
rm -f /tmp/cdc_out.db /tmp/cdc_out.db-wal

echo ''
echo '---Session 2 (separate process): sees only 2 rows on disk---'
$TURSO "$D/c.db" "SELECT count(*) FROM turso_cdc;" 2>/dev/null

echo ''
echo '---SQLite cross-check: only 2 rows persist---'
$SQ "$D/c.db" "SELECT change_id, change_type FROM turso_cdc;"

# Bug manifests: in-session count goes 2 → 3 after VACUUM INTO,
# then back to 2 after PRAGMA wal_checkpoint(FULL). A fresh connection
# (same process or external) always sees 2, confirming the row is phantom.
