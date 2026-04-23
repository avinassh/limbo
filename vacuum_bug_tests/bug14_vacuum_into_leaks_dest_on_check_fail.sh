#!/usr/bin/env bash
# Bug 14: VACUUM INTO leaks the destination file on mid-vacuum failure.
# VACUUM-specific: core/vdbe/execute.rs::cleanup_op_vacuum_into drops the
# _output_db handle and rolls back the source txn but does NOT unlink
# dest_path on error. Combined with Bug 12's strict existence check, retry
# is blocked until the operator unlinks the leaked output.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"

$TURSO "$D/src.db" 2>/dev/null <<'SQL'
CREATE TABLE t(a INTEGER CHECK(a > 0));
PRAGMA ignore_check_constraints=ON;
INSERT INTO t VALUES(-5);
SQL

OUT="$D/leak_out.db"
echo '---First VACUUM INTO: fails mid-copy on CHECK, leaks dest---'
$TURSO "$D/src.db" "VACUUM INTO '$OUT';" 2>&1 | grep -v warning | head
echo 'Files left behind:'
ls -la "${OUT}"* 2>/dev/null

echo ''
echo '---Retry: rejected by preflight existence check---'
$TURSO "$D/src.db" "VACUUM INTO '$OUT';" 2>&1 | grep -v warning | head

# Bug manifests: after the CHECK-failure, $OUT and $OUT-wal remain on disk.
# Retry fails with "output file already exists".
