#!/usr/bin/env bash
# Bug 16: VACUUM INTO adds a spurious CDC commit-marker record on the source.
# VACUUM-specific: core/vdbe/execute.rs::op_vacuum_into_inner wraps the op
# in BEGIN/COMMIT on the source connection. The final COMMIT goes through
# regular transaction-commit machinery, emitting a change_type=2 CDC row.
# In-place VACUUM drives the source at the WAL layer so does NOT do this.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- -m list -q"

$TURSO "$D/t.db" "
CREATE TABLE t(a);
PRAGMA unstable_capture_data_changes_conn='full';
INSERT INTO t VALUES(1);
SELECT 'before:', change_id, change_type FROM turso_cdc;
VACUUM INTO '$D/vi_out.db';
SELECT 'after-INTO:', change_id, change_type FROM turso_cdc;
VACUUM;
SELECT 'after-inplace:', change_id, change_type FROM turso_cdc;
" 2>/dev/null

# Bug manifests: after-INTO shows 3 rows (extra change_type=2 commit marker
# from VACUUM INTO); after-inplace shows 2 rows (in-place does not add one).
# See also Bug 23: the extra row is actually a phantom (not durable).
