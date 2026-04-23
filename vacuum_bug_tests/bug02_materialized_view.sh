#!/usr/bin/env bash
# Bug 2: VACUUM fails on databases with MATERIALIZED VIEWs.
# VACUUM-specific: the DBSP backing table is created in phase 1 (tables_to_create),
# then the CREATE MATERIALIZED VIEW replay in phase 4 tries to create it again.
# Lives in core/vdbe/vacuum.rs::vacuum_target_build_step.
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- --experimental-views -m list -q"

$TURSO "$D/src.db" 2>/dev/null <<'SQL'
CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT);
INSERT INTO t VALUES (1, 'hello');
CREATE MATERIALIZED VIEW mv AS SELECT * FROM t WHERE a > 0;
SELECT 'setup-ok';
SQL

echo '---VACUUM (expected: succeeds; actual: errors)---'
$TURSO "$D/src.db" "VACUUM;" 2>&1 | grep -v '^warning' | head

echo '---VACUUM INTO (same failure)---'
$TURSO "$D/src.db" "VACUUM INTO '$D/mv_out.db';" 2>&1 | grep -v '^warning' | head

# Bug manifests: "table __turso_internal_dbsp_state_v1_mv already exists".
