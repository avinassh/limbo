#!/usr/bin/env bash
# Bug 15: In-place VACUUM on MVCC source silently demotes to WAL journal mode.
# VACUUM-specific: after the copy-back commits, the source file still
# physically contains __turso_internal_mvcc_meta, but the journal-mode
# detection that fresh connections run at open reports 'wal'. VACUUM INTO
# on the same MVCC source is unaffected.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"

$TURSO "$D/mvcc.db" 2>/dev/null <<'SQL'
PRAGMA journal_mode='mvcc';
CREATE TABLE t(a);
INSERT INTO t VALUES (1);
PRAGMA wal_checkpoint(TRUNCATE);
SQL

echo '---Fresh connection BEFORE in-place VACUUM: reports mvcc---'
$TURSO "$D/mvcc.db" "PRAGMA journal_mode;" 2>/dev/null

echo '---Run in-place VACUUM (with checkpoint preflight)---'
$TURSO "$D/mvcc.db" "PRAGMA wal_checkpoint(TRUNCATE); VACUUM;" 2>/dev/null

echo '---Fresh connection AFTER: reports wal (demoted!)---'
$TURSO "$D/mvcc.db" "PRAGMA journal_mode;" 2>/dev/null
echo 'but the meta table is still physically present:'
$TURSO "$D/mvcc.db" "SELECT type, name FROM sqlite_master;" 2>/dev/null

echo ''
echo '---VACUUM INTO on same MVCC source: dest correctly MVCC---'
$TURSO "$D/mvcc2.db" 2>/dev/null <<'SQL'
PRAGMA journal_mode='mvcc';
CREATE TABLE t(a);
INSERT INTO t VALUES (2);
PRAGMA wal_checkpoint(TRUNCATE);
VACUUM INTO '/tmp/mv_out.db';
SQL
$TURSO /tmp/mv_out.db "PRAGMA journal_mode;" 2>/dev/null
rm -f /tmp/mv_out.db /tmp/mv_out.db-wal /tmp/mv_out.db-log

# Bug manifests: in-place VACUUM toggles journal_mode from mvcc to wal silently.
