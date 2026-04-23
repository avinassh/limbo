# VACUUM bug reproducers

One self-contained bash reproducer per bug logged in `../vacuum_bugs.md`.
Each script creates its own temp directory, runs `tursodb` via `cargo run`,
often cross-checks with `/home/ubuntu/sqlite/sqlite3` as an oracle, and
leaves no files behind. No script depends on the existing Turso test
harness, fuzzer, simulator, or oracle.

All 24 bugs are confirmed VACUUM-specific. The column below notes the exact
Turso code path implicated.

| Bug | Script                                              | VACUUM code path implicated                                                                                                         |
|----:|-----------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------|
|   1 | bug01_path_quote_escape.sh                          | `core/translate/vacuum.rs::extract_path_from_expr` trims outer quotes but does not unescape doubled single quotes.                   |
|   2 | bug02_materialized_view.sh                          | `core/vdbe/vacuum.rs::vacuum_target_build_step` phase-1 `PrepareCreateTable` creates the DBSP backing table, phase-4 recreates it.  |
|   3 | bug03_empty_db_panic.sh                             | `VacuumInPlacePhase::BeginSourceTx` → `begin_exclusive_tx` asserts page 1 allocated; VACUUM INTO avoids this via `BEGIN`.            |
|   4 | bug04_schema_case_sensitivity.sh                    | `core/translate/vacuum.rs:47-53` compares schema name with `!= "main"` instead of case-insensitive.                                  |
|   5 | bug05_vacuum_into_plaintext_from_encrypted.sh       | `core/vdbe/execute.rs::op_vacuum_into_inner::Init` opens output with `encryption_opts=None`; in-place path uses `open_vacuum_temp_db`. |
|   6 | bug06_autoincrement_sequence_clobber.sh             | `core/vdbe/vacuum.rs::build_copy_sql` + target INSERT fires AUTOINCREMENT counter; no way to disable (todo comment at vacuum.rs:725). |
|   7 | bug07_page_size_pragma_not_applied.sh               | `VacuumInPlacePhase::ReadSourceMetadata` + `VacuumIntoOpPhase::Init` both read current pager page_size, ignoring pending pragma.     |
|   8 | bug08_vacuum_into_no_expression_path.sh             | `extract_path_from_expr` only matches `Expr::Literal(Literal::String)` / `Expr::Id`, rejecting `?`, `||`, named params.              |
|   9 | bug09_custom_types_spurious_autoindex.sh            | `vacuum_target_build_step::PrepareCreateTable` CREATE TABLE replay registers an implicit PK autoindex the source never had.          |
|  10 | bug10_rowid_not_renumbered.sh                       | `build_copy_sql` unconditionally prepends a rowid alias pseudo-column when `has_rowid`, preserving source rowids verbatim.           |
|  11 | bug11_check_constraint_fires_during_copy.sh         | `build_copy_sql` + target `INSERT` path re-evaluates CHECK; SQLite's vacuum.c bypasses constraints via xfer.                         |
|  12 | bug12_vacuum_into_rejects_empty_file.sh             | `op_vacuum_into_inner::Init` line 14394 `Path::exists()` rejects any existing dentry, including 0-byte placeholders.                |
|  13 | bug13_wide_table_sqlite_max_column.sh               | `build_copy_sql` rowid alias prepend pushes SELECT past `SQLITE_MAX_COLUMN`; SQLite uses page/xfer with no column limit.            |
|  14 | bug14_vacuum_into_leaks_dest_on_check_fail.sh       | `core/vdbe/execute.rs::cleanup_op_vacuum_into` drops handles and rolls back source tx, but never unlinks dest_path.                  |
|  15 | bug15_inplace_vacuum_demotes_mvcc_to_wal.sh         | VACUUM copy-back + final TRUNCATE checkpoint leaves source looking like a plain WAL DB; fresh connections report `journal_mode=wal`. |
|  16 | bug16_vacuum_into_spurious_cdc_commit.sh            | `op_vacuum_into_inner::Init` `BEGIN` / `::Done` `COMMIT` on source fires CDC commit-marker emission path.                            |
|  17 | bug17_change_counter_reset.sh                       | `VacuumDbHeaderMeta::from_source_header` does not copy `change_counter` or `version_valid_for`; target header uses defaults.         |
|  18 | bug18_without_rowid_vacuum_fails.sh                 | `vacuum_target_build_step::PrepareCreateTable` does `prepare()` on source CREATE SQL; Turso's parser rejects WITHOUT ROWID.          |
|  19 | bug19_vacuum_into_extra_wal_sidecar.sh              | `op_vacuum_into_inner::Init` opens dest via `Database::open_file_with_flags` → default WAL mode → `.db-wal` persists after truncate. |
|  20 | bug20_vacuum_into_leaks_on_parse_fail.sh            | Same cleanup gap as Bug 14 (`cleanup_op_vacuum_into`), triggered by Bug 18's target-build parse rejection.                           |
|  21 | bug21_vacuum_into_dest_header_journal_mode.sh       | `op_vacuum_into_inner::Init` WAL-mode open writes `02 02` into dest header bytes 18/19 regardless of source's journal_mode.          |
|  22 | bug22_auto_vacuum_pending_mode_ignored.sh           | Both VACUUM paths set `target_auto_vacuum_mode = source_pager.get_auto_vacuum_mode()`, ignoring any pending `PRAGMA auto_vacuum=...`. |
|  23 | bug23_vacuum_into_phantom_cdc_commit_marker.sh      | Extension of Bug 16: `op_vacuum_into_inner`'s COMMIT emits a CDC row visible in-session but not durable on disk.                    |
|  24 | bug24_vacuum_fails_after_rename_col_on_expr_index.sh| `vacuum_target_build_step::PrepareCreateIndex` replays stale CREATE INDEX SQL (expression/COLLATE forms) left by RENAME COLUMN.      |

## Running

```
bash vacuum_bug_tests/bug01_path_quote_escape.sh
```

Each script's final comment describes the bug signature — the output it
prints directly shows the divergence from SQLite (or the hard error / leaked
file).

## Caveats

- Scripts assume the Turso workspace at `/home/ubuntu/limbo` and an SQLite
  binary at `/home/ubuntu/sqlite/sqlite3`.
- `cargo run --manifest-path …` triggers an incremental build on first
  invocation; subsequent runs hit the build cache.
- Scripts that need feature flags pass them on the tursodb command line:
  - Bug 2: `--experimental-views` (materialized view)
  - Bug 5: `--experimental-encryption`
  - Bug 9: `--experimental-custom-types`
  - Bug 22: `--experimental-autovacuum`
- Oracle comparison uses SQLite 3.53.0 at `/home/ubuntu/sqlite/sqlite3`.
- Scripts trap EXIT and remove their own temp dir, but bugs that write to
  the destination path may intentionally leak (Bug 14, 19, 20) — those
  scripts show the leaked files with `ls` before the temp dir is cleaned.
