use std::sync::Arc;

use crate::error::LimboError;
use crate::schema::TypeDef;
use crate::storage::sqlite3_ondisk::{CacheSize, DatabaseHeader, TextEncoding};
use crate::util::IOExt;
use crate::vdbe::execute::InsnFunctionStepResult;
use crate::Result;
use crate::{Connection, Database, DatabaseOpts, EncryptionOpts, OpenFlags};
use turso_macros::turso_assert;

/// A representation of a row from `sqlite_schema`.
///
/// Carries `rootpage` so we can distinguish storage-backed tables/indexes
/// (rootpage != 0) from virtual tables, custom index-method indexes, views,
/// and triggers (rootpage = 0).
#[derive(Debug)]
pub(crate) struct SchemaEntry {
    pub entry_type: SchemaEntryType,
    pub name: String,
    /// `sqlite_schema.tbl_name`: for indexes and triggers, this is the table
    /// the object belongs to; for tables and views it usually matches `name`.
    pub tbl_name: String,
    pub rootpage: i64,
    pub sql: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchemaEntryType {
    Table,
    Index,
    Trigger,
    View,
}

impl SchemaEntryType {
    pub fn from_str(s: &str) -> crate::Result<Self> {
        match s {
            "table" => Ok(Self::Table),
            "index" => Ok(Self::Index),
            "trigger" => Ok(Self::Trigger),
            "view" => Ok(Self::View),
            other => Err(crate::error::LimboError::Corrupt(format!(
                "unexpected sqlite_schema type: {other}"
            ))),
        }
    }
}

impl SchemaEntry {
    /// Parse from a sqlite_schema row: (type, name, tbl_name, rootpage, sql)
    pub fn from_row(row: &crate::vdbe::Row) -> crate::Result<Self> {
        let entry_type = SchemaEntryType::from_str(row.get::<&str>(0)?)?;
        Ok(Self {
            entry_type,
            name: row.get::<&str>(1)?.to_string(),
            tbl_name: row.get::<&str>(2)?.to_string(),
            rootpage: row.get::<i64>(3)?,
            sql: row.get::<&str>(4)?.to_string(),
        })
    }

    /// Whether this entry represents a storage-backed object (table or index
    /// with rootpage != 0). MVCC can use negative rootpage values before
    /// checkpointing, so this checks `!= 0` rather than `> 0`.
    pub fn is_storage_backed(&self) -> bool {
        self.rootpage != 0
    }

    pub fn is_sqlite_sequence(&self) -> bool {
        self.name == "sqlite_sequence"
    }
}

/// Split rowid-ordered schema entries into the replay phases used by VACUUM.
/// Returns indices into `entries` for `(tables_to_create, tables_to_copy,
/// indexes_to_create, post_data_entries)`.
pub(crate) fn classify_schema_entries(
    entries: &[SchemaEntry],
) -> (Vec<usize>, Vec<usize>, Vec<usize>, Vec<usize>) {
    let mut tables_to_create: Vec<usize> = Vec::new();
    let mut tables_to_copy: Vec<usize> = Vec::new();
    let mut indexes_to_create: Vec<usize> = Vec::new();
    let mut post_data_entries: Vec<usize> = Vec::new();

    for (idx, entry) in entries.iter().enumerate() {
        match entry.entry_type {
            SchemaEntryType::Table if entry.is_storage_backed() => {
                // Skip sqlite_sequence in the schema creation phase. When we
                // create an AUTOINCREMENT table, Turso automatically creates
                // sqlite_sequence if it doesn't exist (see translate/schema.rs).
                // Since entries are ordered by rowid, an AUTOINCREMENT table may
                // appear before sqlite_sequence. If we create that table first
                // (which auto-creates sqlite_sequence), then later try to run
                // "CREATE TABLE sqlite_sequence(name,seq)", it fails with
                // "table already exists".
                if !entry.is_sqlite_sequence() {
                    tables_to_create.push(idx);
                }
                // All storage-backed tables get their data copied, including
                // sqlite_stat1 and other internal storage-backed tables.
                // sqlite_sequence data copy is handled specially by the caller
                // (only if the target materialized it).
                tables_to_copy.push(idx);
            }
            SchemaEntryType::Index if entry.is_storage_backed() => {
                // Storage-backed index (rootpage != 0). Includes both normal
                // user-defined indexes and backing_btree indexes for custom index
                // methods. The caller filters out backing_btree indexes since
                // those are recreated by the parent index method in the post-data
                // phase.
                indexes_to_create.push(idx);
            }
            SchemaEntryType::Trigger | SchemaEntryType::View => {
                // Triggers and views are replayed after data copy to avoid
                // triggers firing during the copy phase.
                post_data_entries.push(idx);
            }
            SchemaEntryType::Table => {
                // Virtual tables (rootpage = 0, type = table) land here.
                // Replayed after data copy alongside triggers and views.
                turso_assert!(
                    !entry.is_storage_backed(),
                    "unexpected storage-backed table (rootpage = 0): {entry.name}"
                );
                post_data_entries.push(idx);
            }
            SchemaEntryType::Index => {
                // Custom index-method indexes (FTS, vector, etc.) have rootpage = 0
                // in sqlite_schema because their storage is managed by the index
                // method, not a B-tree.
                // Replayed after data copy.
                post_data_entries.push(idx);
            }
        }
    }

    (
        tables_to_create,
        tables_to_copy,
        indexes_to_create,
        post_data_entries,
    )
}

// ---------------------------------------------------------------------------
// Destination configuration and metadata policy
// ---------------------------------------------------------------------------

/// Target database feature flags needed for schema replay during a vacuum build.
pub(crate) fn vacuum_target_opts_from_source(source_db: &Database) -> DatabaseOpts {
    DatabaseOpts::new()
        .with_views(source_db.experimental_views_enabled())
        .with_index_method(source_db.experimental_index_method_enabled())
        .with_custom_types(source_db.experimental_custom_types_enabled())
        .with_encryption(source_db.experimental_encryption_enabled())
        .with_attach(source_db.experimental_attach_enabled())
        .with_generated_columns(source_db.experimental_generated_columns_enabled())
}

/// Database header metadata that the target build must finalize before commit.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VacuumDbHeaderMeta {
    schema_cookie: u32,
    default_page_cache_size: CacheSize,
    text_encoding: TextEncoding,
    user_version: i32,
    application_id: i32,
}

impl VacuumDbHeaderMeta {
    pub(crate) fn from_source_header(source: &DatabaseHeader) -> Self {
        Self {
            schema_cookie: source.schema_cookie.get().wrapping_add(1),
            default_page_cache_size: source.default_page_cache_size,
            text_encoding: source.text_encoding,
            user_version: source.user_version.get(),
            application_id: source.application_id.get(),
        }
    }

    fn apply_to(self, header: &mut DatabaseHeader) {
        header.schema_cookie = self.schema_cookie.into();
        header.default_page_cache_size = self.default_page_cache_size;
        header.text_encoding = self.text_encoding;
        header.user_version = self.user_version.into();
        header.application_id = self.application_id.into();
    }
}

/// File-backed internal temp database used by in-place `VACUUM`.
///
/// The temp directory is dropped after the connection and database handles so
/// host files can be closed before the directory cleanup runs.
pub(crate) struct VacuumTempDb {
    pub conn: Arc<Connection>,
    _db: Arc<Database>,
    #[cfg(test)]
    path: String,
    #[cfg(not(target_family = "wasm"))]
    _temp_dir: tempfile::TempDir,
}

#[cfg(not(target_family = "wasm"))]
fn vacuum_temp_db_encryption(
    source_conn: &Arc<Connection>,
) -> Result<(Option<EncryptionOpts>, Option<crate::EncryptionKey>)> {
    let Some(cipher_mode) = source_conn.get_encryption_cipher_mode() else {
        return Ok((None, None));
    };
    let encryption_key = source_conn.encryption_key.read().clone().ok_or_else(|| {
        LimboError::InternalError(
            "encrypted in-place VACUUM temp database requires source encryption key".to_string(),
        )
    })?;
    let encryption_opts = EncryptionOpts {
        cipher: cipher_mode.to_string(),
        hexkey: hex::encode(encryption_key.as_slice()),
    };
    Ok((Some(encryption_opts), Some(encryption_key)))
}

#[cfg(not(target_family = "wasm"))]
pub(crate) fn open_vacuum_temp_db(
    source_conn: &Arc<Connection>,
    source_db: &Arc<Database>,
    page_size: u32,
    reserved_space: u8,
) -> Result<VacuumTempDb> {
    let temp_dir = tempfile::tempdir().map_err(|e| crate::error::io_error(e, "tempdir"))?;
    let path = temp_dir.path().join("tursodb_vacuum_temp.db");
    let path = path
        .to_str()
        .ok_or_else(|| LimboError::InternalError("vacuum temp path is not valid UTF-8".into()))?
        .to_string();
    #[cfg(test)]
    let test_path = path.clone();

    let (encryption_opts, encryption_key) = vacuum_temp_db_encryption(source_conn)?;
    let db = Database::open_file_with_flags(
        source_db.io.clone(),
        &path,
        OpenFlags::Create,
        vacuum_target_opts_from_source(source_db),
        encryption_opts,
    )?;
    let conn = db.connect_with_encryption(encryption_key)?;
    conn.reset_page_size(page_size)?;
    conn.set_reserved_bytes(reserved_space)?;
    conn.wal_auto_checkpoint_disable();

    Ok(VacuumTempDb {
        conn,
        _db: db,
        #[cfg(test)]
        path: test_path,
        _temp_dir: temp_dir,
    })
}

#[cfg(target_family = "wasm")]
pub(crate) fn open_vacuum_temp_db(
    _source_conn: &Arc<Connection>,
    _source_db: &Arc<Database>,
    _page_size: u32,
    _reserved_space: u8,
) -> Result<VacuumTempDb> {
    Err(LimboError::InternalError(
        "in-place VACUUM requires a file-backed internal temp database".to_string(),
    ))
}

fn finalize_vacuum_target_header(
    target_conn: &Arc<Connection>,
    header_meta: &VacuumDbHeaderMeta,
) -> Result<crate::IOResult<()>> {
    if let Some(mv_store) = target_conn.mv_store_for_db(crate::MAIN_DB_ID) {
        let tx_id = target_conn.get_mv_tx_id_for_db(crate::MAIN_DB_ID);
        return mv_store
            .with_header_mut(|header| header_meta.apply_to(header), tx_id.as_ref())
            .map(crate::IOResult::Done);
    }
    let pager = target_conn.pager.load();
    pager.with_header_mut(|header| header_meta.apply_to(header))
}

// ---------------------------------------------------------------------------
// Vacuum target build engine
// ---------------------------------------------------------------------------

/// Copy functions, vtab modules, and index methods from one connection to
/// another so that schema replay on the target sees the same symbols as
/// the source.
pub(crate) fn mirror_symbols(source: &Connection, target: &Connection) {
    let source_syms = source.syms.read();
    let mut target_syms = target.syms.write();
    target_syms.functions.extend(
        source_syms
            .functions
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    target_syms.vtab_modules.extend(
        source_syms
            .vtab_modules
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    target_syms.index_methods.extend(
        source_syms
            .index_methods
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
}

/// Capture non-builtin custom type definitions from the source schema.
pub(crate) fn capture_custom_types(
    source: &Connection,
    db_id: usize,
) -> Vec<(String, Arc<TypeDef>)> {
    source.with_schema(db_id, |schema| {
        schema
            .type_registry
            .iter()
            .filter(|(_, td)| !td.is_builtin)
            .map(|(name, td)| (name.clone(), td.clone()))
            .collect()
    })
}

/// Configuration for building a compacted vacuum target database. Provided by
/// the caller after reading source metadata and setting up the target DB.
/// Callers must mirror source symbols (functions, vtab modules, index methods)
/// directly into the target connection before starting the state machine.
pub(crate) struct VacuumTargetBuildConfig {
    /// Source connection - used for `prepare_internal` and `with_schema` during
    /// schema collection and data copy.
    pub source_conn: Arc<Connection>,
    /// Escaped schema name for safe SQL interpolation (e.g. `"main"`).
    pub escaped_schema_name: String,
    /// Database index for schema lookups on the source connection.
    pub source_db_id: usize,
    /// Database header metadata to write before committing the target database.
    pub header_meta: VacuumDbHeaderMeta,
    /// Pre-captured source custom type definitions for STRICT table replay.
    pub source_custom_types: Vec<(String, Arc<TypeDef>)>,
    /// Whether the source database has MVCC enabled.
    pub source_mvcc_enabled: bool,
}

/// Context for the vacuum target build. Holds the target connection and all
/// intermediate state needed across async yields.
pub(crate) struct VacuumTargetBuildContext {
    target_conn: Arc<Connection>,
    phase: VacuumTargetBuildPhase,
    /// Typed schema entries collected from sqlite_schema, ordered by rowid.
    schema_entries: Vec<SchemaEntry>,
    /// Storage-backed tables to CREATE (excludes sqlite_sequence).
    tables_to_create: Vec<usize>,
    /// Storage-backed tables whose data to copy.
    tables_to_copy: Vec<usize>,
    /// User-defined secondary indexes to CREATE (deferred for performance).
    indexes_to_create: Vec<usize>,
    /// Triggers, views, and rootpage = 0 objects (deferred to avoid trigger firing).
    post_data_entries: Vec<usize>,
}

impl VacuumTargetBuildContext {
    pub fn new(target_conn: Arc<Connection>) -> Self {
        Self {
            target_conn,
            phase: VacuumTargetBuildPhase::Init,
            schema_entries: Vec::new(),
            tables_to_create: Vec::new(),
            tables_to_copy: Vec::new(),
            indexes_to_create: Vec::new(),
            post_data_entries: Vec::new(),
        }
    }

    pub(crate) fn cleanup_after_error(&mut self) {
        self.phase = VacuumTargetBuildPhase::Done;
    }
}

/// Phases for the vacuum target build state machine.
#[derive(Default)]
pub(crate) enum VacuumTargetBuildPhase {
    /// Mirror symbols/types, set perf flags, begin target tx, prepare schema query.
    #[default]
    Init,
    /// Step through schema query to collect rows
    CollectSchemaRows { schema_stmt: Box<crate::Statement> },
    /// Prepare CREATE TABLE statement on the target (idx into tables_to_create)
    PrepareCreateTable { idx: usize },
    /// Step through CREATE TABLE statement on the target (async)
    StepCreateTable {
        target_schema_stmt: Box<crate::Statement>,
        idx: usize,
    },
    /// Start copying a table's data
    StartCopyTable { table_idx: usize },
    /// Select rows from source table and insert into the target.
    CopyRows {
        select_stmt: Box<crate::Statement>,
        target_insert_stmt: Box<crate::Statement>,
        table_idx: usize,
    },
    /// Step through INSERT statement on the target.
    StepTargetInsert {
        select_stmt: Box<crate::Statement>,
        target_insert_stmt: Box<crate::Statement>,
        table_idx: usize,
    },
    /// Prepare CREATE INDEX statement on the target (idx into indexes_to_create)
    PrepareCreateIndex { idx: usize },
    /// Step through CREATE INDEX statement on the target.
    StepCreateIndex {
        target_schema_stmt: Box<crate::Statement>,
        idx: usize,
    },
    /// Prepare post-data schema objects (triggers, views, rootpage = 0 entries)
    PreparePostData { idx: usize },
    /// Step through post-data CREATE statement on the target.
    StepPostData {
        target_schema_stmt: Box<crate::Statement>,
        idx: usize,
    },
    /// Finalize database header metadata before committing the target database.
    FinalizeTargetHeader,
    /// Operation complete
    Done,
}

/// Build a compacted vacuum target database.
///
/// This is an async state machine implementation that yields on I/O operations.
/// The caller creates a target database with matching
/// page_size, reserved bytes, source feature flags, and schema-replay symbols.
///
/// It:
/// 1. Mirrors source symbols and custom types to the target
/// 2. Queries sqlite_schema for all schema objects including rootpage, ordered by rowid
/// 3. Creates storage-backed tables (rootpage != 0) in the target, excluding
///    sqlite_sequence (auto-created when AUTOINCREMENT tables are created)
/// 4. Copies data for all storage-backed tables, including sqlite_stat1 and other
///    internal storage-backed tables
/// 5. Creates user-defined secondary indexes after data copy for performance
///    (backing-btree indexes for custom index methods are excluded here)
/// 6. Creates triggers, views, and rootpage = 0 objects last (after data copy).
///    Custom index methods (FTS, vector) recreate and backfill their backing
///    indexes from the copied table data in this phase.
/// 7. Finalizes target database header metadata, then commits the target transaction.
pub(crate) fn vacuum_target_build_step(
    config: &VacuumTargetBuildConfig,
    state: &mut VacuumTargetBuildContext,
) -> Result<crate::IOResult<()>> {
    loop {
        let current_phase = std::mem::take(&mut state.phase);

        match current_phase {
            VacuumTargetBuildPhase::Init => {
                // Mirror source custom type definitions into the target schema
                // so that STRICT tables with custom type columns can resolve
                // those types during CREATE TABLE replay.
                if !config.source_custom_types.is_empty() {
                    state.target_conn.with_schema_mut(|target_schema| {
                        for (name, td) in &config.source_custom_types {
                            target_schema.type_registry.insert(name.clone(), td.clone());
                        }
                    });
                }

                // Enable MVCC on the target if source has it enabled.
                // Must be done before any schema operations to ensure the log file is created
                if config.source_mvcc_enabled {
                    state.target_conn.execute("PRAGMA journal_mode = 'mvcc'")?;
                }

                // Performance optimizations for the target database (matches SQLite vacuum.c):
                // 1. Disable fsync - the target is a new output/temp database
                // 2. Disable foreign key checks - source data is already consistent
                // These match SQLite's vacuum.c optimizations (PAGER_SYNCHRONOUS_OFF, ~SQLITE_ForeignKeys)
                state.target_conn.set_sync_mode(crate::SyncMode::Off);
                state.target_conn.set_foreign_keys_enabled(false);

                // Wrap all operations in a single transaction for atomicity.
                state.target_conn.execute("BEGIN")?;

                // Query sqlite_schema with rootpage, ordered by rowid.
                // Exclude the MVCC metadata table - it is an internal artifact.
                let escaped_schema_name = &config.escaped_schema_name;
                let schema_sql = format!(
                    "SELECT type, name, tbl_name, rootpage, sql \
                     FROM \"{escaped_schema_name}\".sqlite_schema \
                     WHERE sql IS NOT NULL AND name <> '{}' ORDER BY rowid",
                    crate::mvcc::database::MVCC_META_TABLE_NAME
                );
                let schema_stmt = config.source_conn.prepare_internal(schema_sql.as_str())?;

                state.phase = VacuumTargetBuildPhase::CollectSchemaRows {
                    schema_stmt: Box::new(schema_stmt),
                };
                continue;
            }

            VacuumTargetBuildPhase::CollectSchemaRows { mut schema_stmt } => {
                match schema_stmt.step()? {
                    crate::StepResult::Row => {
                        let row = schema_stmt
                            .row()
                            .expect("StepResult::Row but row() returned None");
                        state.schema_entries.push(SchemaEntry::from_row(row)?);
                        state.phase = VacuumTargetBuildPhase::CollectSchemaRows { schema_stmt };
                        continue;
                    }
                    crate::StepResult::Done => {
                        // Classify schema entries into replay phases using rootpage.
                        let (tables_create, tables_copy, indexes_create, post_data) =
                            classify_schema_entries(&state.schema_entries);
                        state.tables_to_create = tables_create;
                        state.tables_to_copy = tables_copy;
                        // Backing-btree indexes are implementation details of custom index
                        // methods. i.e. when custom indexes are created, they are created automatically
                        // The user-visible custom-index CREATE in post_data_entries
                        // recreates and backfills those backing indexes from the copied rows.
                        // for now, we will skip them
                        state.indexes_to_create = indexes_create
                            .into_iter()
                            .filter(|entry_ordinal| {
                                let entry = &state.schema_entries[*entry_ordinal];
                                !config
                                    .source_conn
                                    .with_schema(config.source_db_id, |schema| {
                                        schema
                                            .get_index(&entry.tbl_name, &entry.name)
                                            .is_some_and(|idx| idx.is_backing_btree_index())
                                    })
                            })
                            .collect();
                        state.post_data_entries = post_data;

                        state.phase = VacuumTargetBuildPhase::PrepareCreateTable { idx: 0 };
                        continue;
                    }
                    crate::StepResult::IO => {
                        let io = schema_stmt
                            .take_io_completions()
                            .expect("StepResult::IO returned but no completions available");
                        state.phase = VacuumTargetBuildPhase::CollectSchemaRows { schema_stmt };
                        return Ok(crate::IOResult::IO(io));
                    }
                    crate::StepResult::Busy | crate::StepResult::Interrupt => {
                        return Err(LimboError::Busy);
                    }
                }
            }

            // Phase 1: Create storage-backed tables (rootpage != 0, type=table),
            // excluding sqlite_sequence (auto-created by AUTOINCREMENT tables).
            VacuumTargetBuildPhase::PrepareCreateTable { idx } => {
                let entries_len = state.tables_to_create.len();
                if idx >= entries_len {
                    // Done creating tables, start copying data
                    state.phase = VacuumTargetBuildPhase::StartCopyTable { table_idx: 0 };
                    continue;
                }

                let entry_ordinal = state.tables_to_create[idx];
                let entry = &state.schema_entries[entry_ordinal];
                let sql_str = &entry.sql;

                // System tables (sqlite_stat1, __turso_internal_types, etc.) have
                // reserved name prefixes that translate_create_table rejects for
                // user SQL. Temporarily mark the target connection as nested during
                // prepare() so the reserved-name check is bypassed at compile
                // time. The guard is only for prepare: keeping it during step()
                // would make this CREATE TABLE look nested, so its Transaction
                // opcode would skip write setup.
                let is_system = crate::schema::is_system_table(&entry.name);
                if is_system {
                    state.target_conn.start_nested();
                }
                let target_stmt = state.target_conn.prepare(sql_str);
                if is_system {
                    state.target_conn.end_nested();
                }
                let target_stmt = target_stmt?;
                state.phase = VacuumTargetBuildPhase::StepCreateTable {
                    target_schema_stmt: Box::new(target_stmt),
                    idx,
                };
                continue;
            }

            VacuumTargetBuildPhase::StepCreateTable {
                mut target_schema_stmt,
                idx,
            } => match target_schema_stmt.step()? {
                crate::StepResult::Row => {
                    unreachable!("CREATE TABLE statement unexpectedly returned a row");
                }
                crate::StepResult::Done => {
                    state.phase = VacuumTargetBuildPhase::PrepareCreateTable { idx: idx + 1 };
                    continue;
                }
                crate::StepResult::IO => {
                    let io = target_schema_stmt
                        .take_io_completions()
                        .expect("StepResult::IO returned but no completions available");
                    state.phase = VacuumTargetBuildPhase::StepCreateTable {
                        target_schema_stmt,
                        idx,
                    };
                    return Ok(crate::IOResult::IO(io));
                }
                crate::StepResult::Busy | crate::StepResult::Interrupt => {
                    return Err(LimboError::Busy);
                }
            },

            // Phase 2: Copy data for all storage-backed tables.
            // Column lists are derived from BTreeTable.columns in the schema,
            // not PRAGMA table_info, because table_info omits generated columns
            // while SELECT * includes them - causing a column count mismatch.
            VacuumTargetBuildPhase::StartCopyTable { table_idx } => {
                let tables_len = state.tables_to_copy.len();
                if table_idx >= tables_len {
                    // Done copying all tables, proceed to deferred indexes.
                    state.phase = VacuumTargetBuildPhase::PrepareCreateIndex { idx: 0 };
                    continue;
                }

                let entry_ordinal = state.tables_to_copy[table_idx];
                let entry = &state.schema_entries[entry_ordinal];
                let table_name = &entry.name;

                // sqlite_sequence: only copy data if the target has it
                // (auto-created when an AUTOINCREMENT table was created in
                // phase 1). If not present, skip — no AUTOINCREMENT tables
                // means no counters to preserve. The explicit copy is needed
                // because inserting rows with the `rowid` pseudo-column does
                // not update sqlite_sequence counters automatically.
                if entry.is_sqlite_sequence() {
                    let target_has_sequence = state
                        .target_conn
                        .schema
                        .read()
                        .get_btree_table(crate::schema::SQLITE_SEQUENCE_TABLE_NAME)
                        .is_some();
                    if !target_has_sequence {
                        state.phase = VacuumTargetBuildPhase::StartCopyTable {
                            table_idx: table_idx + 1,
                        };
                        continue;
                    }
                }

                let escaped_table_name = table_name.replace('"', "\"\"");
                // Derive copy-column list from BTreeTable.columns in schema,
                // filtering out virtual generated columns so the SELECT and
                // INSERT arities stay aligned.
                let source_btree_table = config
                    .source_conn
                    .with_schema(config.source_db_id, |schema| {
                        schema.get_btree_table(table_name)
                    });

                // sqlite_sequence may already have rows from the AUTOINCREMENT
                // tracking that ran during the table data copy. Use INSERT OR
                // REPLACE so the source counter values overwrite any stale
                // auto-generated ones (matches SQLite vacuum.c behavior).
                let (select_sql, insert_sql) = build_copy_sql(
                    &config.escaped_schema_name,
                    &escaped_table_name,
                    source_btree_table.as_deref(),
                    entry.is_sqlite_sequence(),
                )?;

                // SELECT from source, INSERT into the target.
                let select_stmt = config.source_conn.prepare_internal(&select_sql)?;

                // System tables need nested mode during prepare() to bypass
                // "may not be modified" checks. Can't use prepare_internal()
                // because the nested guard must not persist into step() - the
                // Transaction opcode needs to run for page-level write setup.
                let is_system = crate::schema::is_system_table(table_name);
                if is_system {
                    state.target_conn.start_nested();
                }
                let target_insert_stmt = state.target_conn.prepare(&insert_sql);
                if is_system {
                    state.target_conn.end_nested();
                }
                let target_insert_stmt = target_insert_stmt?;

                state.phase = VacuumTargetBuildPhase::CopyRows {
                    select_stmt: Box::new(select_stmt),
                    target_insert_stmt: Box::new(target_insert_stmt),
                    table_idx,
                };
                continue;
            }

            VacuumTargetBuildPhase::CopyRows {
                mut select_stmt,
                mut target_insert_stmt,
                table_idx,
            } => match select_stmt.step()? {
                crate::StepResult::Row => {
                    let row = select_stmt
                        .row()
                        .expect("StepResult::Row but row() returned None");

                    target_insert_stmt.reset()?;
                    target_insert_stmt.clear_bindings();
                    for (i, value) in row.get_values().cloned().enumerate() {
                        let index =
                            std::num::NonZero::new(i + 1).expect("i + 1 is always non-zero");
                        target_insert_stmt.bind_at(index, value);
                    }

                    state.phase = VacuumTargetBuildPhase::StepTargetInsert {
                        select_stmt,
                        target_insert_stmt,
                        table_idx,
                    };
                    continue;
                }
                crate::StepResult::Done => {
                    // Move to next table
                    state.phase = VacuumTargetBuildPhase::StartCopyTable {
                        table_idx: table_idx + 1,
                    };
                    continue;
                }
                crate::StepResult::IO => {
                    let io = select_stmt
                        .take_io_completions()
                        .expect("StepResult::IO returned but no completions available");
                    state.phase = VacuumTargetBuildPhase::CopyRows {
                        select_stmt,
                        target_insert_stmt,
                        table_idx,
                    };
                    return Ok(crate::IOResult::IO(io));
                }
                crate::StepResult::Busy | crate::StepResult::Interrupt => {
                    return Err(LimboError::Busy);
                }
            },

            VacuumTargetBuildPhase::StepTargetInsert {
                select_stmt,
                mut target_insert_stmt,
                table_idx,
            } => match target_insert_stmt.step()? {
                crate::StepResult::Row => {
                    unreachable!("INSERT statement unexpectedly returned a row");
                }
                crate::StepResult::Done => {
                    // Go back to get next row from source
                    state.phase = VacuumTargetBuildPhase::CopyRows {
                        select_stmt,
                        target_insert_stmt,
                        table_idx,
                    };
                    continue;
                }
                crate::StepResult::IO => {
                    let io = target_insert_stmt
                        .take_io_completions()
                        .expect("StepResult::IO returned but no completions available");
                    state.phase = VacuumTargetBuildPhase::StepTargetInsert {
                        select_stmt,
                        target_insert_stmt,
                        table_idx,
                    };
                    return Ok(crate::IOResult::IO(io));
                }
                crate::StepResult::Busy | crate::StepResult::Interrupt => {
                    return Err(LimboError::Busy);
                }
            },

            // Phase 3: Create user-defined secondary indexes.
            VacuumTargetBuildPhase::PrepareCreateIndex { idx } => {
                let entries_len = state.indexes_to_create.len();
                if idx >= entries_len {
                    // Done creating indexes, move to post-data objects
                    state.phase = VacuumTargetBuildPhase::PreparePostData { idx: 0 };
                    continue;
                }

                let entry_ordinal = state.indexes_to_create[idx];
                let entry = &state.schema_entries[entry_ordinal];
                // Backing-btree indexes for custom index methods were filtered
                // out when indexes_to_create was built. The remaining CREATE
                // INDEX statements are user-visible and can use ordinary prepare.
                let target_stmt = state.target_conn.prepare(&entry.sql)?;
                state.phase = VacuumTargetBuildPhase::StepCreateIndex {
                    target_schema_stmt: Box::new(target_stmt),
                    idx,
                };
                continue;
            }

            VacuumTargetBuildPhase::StepCreateIndex {
                mut target_schema_stmt,
                idx,
            } => match target_schema_stmt.step()? {
                crate::StepResult::Row => {
                    unreachable!("CREATE INDEX statement unexpectedly returned a row");
                }
                crate::StepResult::Done => {
                    state.phase = VacuumTargetBuildPhase::PrepareCreateIndex { idx: idx + 1 };
                    continue;
                }
                crate::StepResult::IO => {
                    let io = target_schema_stmt
                        .take_io_completions()
                        .expect("StepResult::IO returned but no completions available");
                    state.phase = VacuumTargetBuildPhase::StepCreateIndex {
                        target_schema_stmt,
                        idx,
                    };
                    return Ok(crate::IOResult::IO(io));
                }
                crate::StepResult::Busy | crate::StepResult::Interrupt => {
                    return Err(LimboError::Busy);
                }
            },

            // Phase 4: Create triggers, views, and rootpage=0 schema objects.
            VacuumTargetBuildPhase::PreparePostData { idx } => {
                let entries_len = state.post_data_entries.len();
                if idx >= entries_len {
                    state.phase = VacuumTargetBuildPhase::FinalizeTargetHeader;
                    continue;
                }

                let entry_ordinal = state.post_data_entries[idx];
                let entry = &state.schema_entries[entry_ordinal];
                let target_stmt = state.target_conn.prepare(&entry.sql)?;
                state.phase = VacuumTargetBuildPhase::StepPostData {
                    target_schema_stmt: Box::new(target_stmt),
                    idx,
                };
                continue;
            }

            VacuumTargetBuildPhase::StepPostData {
                mut target_schema_stmt,
                idx,
            } => match target_schema_stmt.step()? {
                crate::StepResult::Row => {
                    unreachable!("CREATE statement unexpectedly returned a row");
                }
                crate::StepResult::Done => {
                    state.phase = VacuumTargetBuildPhase::PreparePostData { idx: idx + 1 };
                    continue;
                }
                crate::StepResult::IO => {
                    let io = target_schema_stmt
                        .take_io_completions()
                        .expect("StepResult::IO returned but no completions available");
                    state.phase = VacuumTargetBuildPhase::StepPostData {
                        target_schema_stmt,
                        idx,
                    };
                    return Ok(crate::IOResult::IO(io));
                }
                crate::StepResult::Busy | crate::StepResult::Interrupt => {
                    return Err(LimboError::Busy);
                }
            },

            VacuumTargetBuildPhase::FinalizeTargetHeader => {
                match finalize_vacuum_target_header(&state.target_conn, &config.header_meta)? {
                    crate::IOResult::Done(()) => {}
                    crate::IOResult::IO(io) => {
                        state.phase = VacuumTargetBuildPhase::FinalizeTargetHeader;
                        return Ok(crate::IOResult::IO(io));
                    }
                }

                state.phase = VacuumTargetBuildPhase::Done;
                continue;
            }

            VacuumTargetBuildPhase::Done => {
                // Commit the target transaction started in Init phase.
                state.target_conn.execute("COMMIT")?;
                return Ok(crate::IOResult::Done(()));
            }
        }
    }
}

// Build the SELECT and INSERT SQL strings for copying a table's data.
//
// Uses the in-memory `BTreeTable` column metadata from the schema to derive
// the copy-column list. Virtual generated columns are excluded from both
// SELECT and INSERT since they are computed, not stored. This keeps both
// lists tied to the same stored-column model.
//
// For e.g. given a schema:
// CREATE TABLE employees (
//       id INTEGER PRIMARY KEY,
//       name TEXT,
//       salary INTEGER,
//       bonus INTEGER GENERATED ALWAYS AS (salary * 0.1) VIRTUAL
//   )
//
//   Output:
//   - select_sql: SELECT rowid, "name", "salary" FROM "main"."employees"
//   - insert_sql: INSERT INTO "main"."employees" (rowid, "name", "salary") VALUES (?, ?, ?)
pub(crate) fn build_copy_sql(
    escaped_schema_name: &str,
    escaped_table_name: &str,
    source_btree_table: Option<&crate::schema::BTreeTable>,
    or_replace: bool,
) -> Result<(String, String)> {
    let Some(btree) = source_btree_table else {
        // Storage-backed tables must have schema metadata. If we get here,
        // the schema is inconsistent - somewhere it has gone terribly wrong
        return Err(LimboError::Corrupt(format!(
            "no schema metadata for storage-backed table \"{escaped_table_name}\""
        )));
    };

    // Collect non-virtual-generated columns with their quoted names.
    let mut data_columns: Vec<String> = Vec::new();
    let mut rowid_alias_col_idx: Option<usize> = None;
    for (i, col) in btree.columns.iter().enumerate() {
        if col.is_virtual_generated() {
            continue;
        }
        if col.is_rowid_alias() {
            rowid_alias_col_idx = Some(i);
        }
        let Some(name) = col.name.as_deref() else {
            return Err(LimboError::Corrupt(format!(
                "missing column name for table \"{escaped_table_name}\""
            )));
        };
        let escaped = name.replace('"', "\"\"");
        data_columns.push(format!("\"{escaped}\""));
    }

    if data_columns.is_empty() {
        return Err(LimboError::Corrupt(
            "found a table without any columns".to_string(),
        ));
    }

    // Determine rowid handling: for has_rowid tables, we need to preserve the
    // rowid. Find an alias name (rowid, _rowid_, or oid) that doesn't conflict
    // with an actual column name.
    let rowid_alias = if btree.has_rowid {
        ["rowid", "_rowid_", "oid"]
            .iter()
            .copied()
            .find(|alias| btree.get_column(alias).is_none())
    } else {
        None
    };

    // Build the column lists. If there's a rowid alias column (INTEGER PRIMARY KEY),
    // we exclude it from the data columns and use the rowid alias instead, since
    // the rowid alias column IS the rowid.
    //
    // Track bind_count explicitly instead of parsing the joined string - column
    // names can contain commas inside quotes which would miscount.
    let (select_cols, insert_cols, bind_count) = if let Some(alias) = rowid_alias {
        if let Some(alias_idx) = rowid_alias_col_idx {
            // Remove the rowid alias column from data_columns (it IS the rowid)
            let mut filtered: Vec<&str> = Vec::new();
            let mut col_physical_idx = 0;
            for (i, col) in btree.columns.iter().enumerate() {
                if col.is_virtual_generated() {
                    continue;
                }
                if i != alias_idx {
                    filtered.push(&data_columns[col_physical_idx]);
                }
                col_physical_idx += 1;
            }
            if filtered.is_empty() {
                // Table only has the rowid alias column
                (alias.to_string(), alias.to_string(), 1)
            } else {
                let count = filtered.len() + 1; // +1 for rowid alias
                let cols = filtered.join(", ");
                (
                    format!("{alias}, {cols}"),
                    format!("{alias}, {cols}"),
                    count,
                )
            }
        } else {
            // has_rowid but no explicit alias column - prepend the chosen rowid
            // pseudo-column to the stored column list.
            let count = data_columns.len() + 1; // +1 for rowid alias
            let cols = data_columns.join(", ");
            (
                format!("{alias}, {cols}"),
                format!("{alias}, {cols}"),
                count,
            )
        }
    } else {
        // Either WITHOUT ROWID, or a rowid table where all three pseudo-names
        // are shadowed by real columns. In the shadowed case SQL cannot name
        // the hidden rowid, and SQLite does not require rowid stability for
        // tables without an INTEGER PRIMARY KEY during VACUUM.
        let count = data_columns.len();
        let cols = data_columns.join(", ");
        (cols.clone(), cols, count)
    };

    // The first placeholder is just "?"; each later placeholder adds ", ?".
    // Reserve 3 bytes per placeholder, then subtract the 2-byte separator that
    // the first placeholder does not need.
    let mut placeholders = String::with_capacity(bind_count.saturating_mul(3).saturating_sub(2));
    for i in 0..bind_count {
        if i > 0 {
            placeholders.push_str(", ");
        }
        placeholders.push('?');
    }

    let select_sql =
        format!("SELECT {select_cols} FROM \"{escaped_schema_name}\".\"{escaped_table_name}\"");
    let insert_prefix = if or_replace {
        "INSERT OR REPLACE INTO"
    } else {
        "INSERT INTO"
    };
    let insert_sql =
        format!("{insert_prefix} \"{escaped_table_name}\" ({insert_cols}) VALUES ({placeholders})");

    Ok((select_sql, insert_sql))
}

// ---------------------------------------------------------------------------
// In-place VACUUM engine - copy-back state machine
// ---------------------------------------------------------------------------

/// Independent cleanup flags for in-place VACUUM. These track which
/// resources the opcode has acquired and are **not** cleared by
/// `std::mem::take` on the phase enum. The cleanup function reads these
/// to decide what to roll back regardless of where the state machine was
/// when the error occurred.
#[derive(Default)]
pub(crate) struct VacuumInPlaceCleanupState {
    /// WAL checkpoint serialization lock is held.
    pub checkpoint_lock_held: bool,
    /// Final VACUUM checkpoint owns checkpoint/read0/write locks.
    pub checkpoint_locks_handed_off: bool,
    /// WAL VACUUM lock is held exclusively.
    pub vacuum_lock_held: bool,
    /// Source pager read transaction was started.
    pub source_read_tx_open: bool,
    /// Source pager write transaction was acquired and `auto_commit`/`tx_state`
    /// were modified.
    pub source_write_tx_open: bool,
    /// WAL commit was published via `finish_append_frames_commit`. Once true
    /// the compacted image is durable and rollback must not be attempted.
    pub wal_commit_published: bool,
    /// Scratch buffer for `Wal::read_frames_batch`, sized to one full copy
    /// batch. Allocated lazily on first use and reused across every batch to
    /// avoid a ~(page_size + 24) × `VACUUM_COPY_BATCH_SIZE` allocation per
    /// batch. Only runs whose total size matches this buffer's length can
    /// reuse it (which is the common, max-sized run case).
    pub read_scratch_buf: Option<Arc<crate::io::Buffer>>,
}

/// Phases for the in-place VACUUM state machine.
///
/// The opcode owns the source transaction lifecycle directly: it begins the
/// read and write transactions on the source pager, builds a temp image via
/// the shared target-build engine, then copies the compacted target back into the
/// source WAL using the batched prepare_frames → WriteBatch → commit path.
pub(crate) enum VacuumInPlacePhase {
    /// Validate preconditions (auto_commit, active statements, readonly, memory,
    /// MVCC, WAL-backed pager).
    Preflight,
    /// Acquire exclusive access on the source WAL via `begin_exclusive_tx`:
    /// checkpoint_lock is already held from Preflight; this acquires the
    /// exclusive VACUUM lock, read_locks[0] exclusively, and write_lock in one
    /// shot.
    BeginSourceTx,
    /// Read source database header metadata for the target-build config.
    ReadSourceMetadata,
    /// Create temp DB and run the shared target-build engine.
    TargetBuild {
        config: Box<VacuumTargetBuildConfig>,
        temp_db: Box<VacuumTempDb>,
        context: Box<VacuumTargetBuildContext>,
    },
    /// Open a read transaction on the committed temp pager for copy-back reads.
    BeginTempReadTx { temp_db: Box<VacuumTempDb> },
    /// Initialize the source WAL header if needed (one-time before first batch).
    /// Two IO steps: write the header, then fsync to set `initialized = true`.
    InitSourceWalHeader {
        temp_db: Box<VacuumTempDb>,
        total_pages: u32,
        /// The completion to wait on: first the header write, then the fsync.
        completion: crate::io::Completion,
        /// `false` while waiting for the header write, `true` while waiting for
        /// the fsync from `prepare_wal_finish`.
        fsync_phase: bool,
    },
    /// Read a batch of temp pages in parallel using multi-inflight
    /// `Wal::read_frames_batch` calls, one per contiguous frame-id run.
    /// The state waits on a single `CompletionGroup` completion that
    /// covers every run in the batch.
    ReadTempBatch {
        temp_db: Box<VacuumTempDb>,
        total_pages: u32,
        /// First page id NOT included in this batch (i.e., the page id
        /// where the next batch will start, 1-based).
        next_page: u32,
        /// Boxed to shrink the enum — PreparedFrames contains Vecs and is ~80+
        /// bytes unboxed. Only created once per batch, so boxing adds no
        /// allocation overhead in the hot path.
        prev_prepared: Option<Box<crate::storage::wal::PreparedFrames>>,
        /// Pages for this batch, pre-allocated in logical page-id order.
        batch_pages: Vec<crate::storage::pager::PageRef>,
        /// Aggregate completion for every run issued for this batch.
        read_completion: crate::io::Completion,
    },
    /// Write a prepared batch to the WAL file.
    WriteWalBatch {
        temp_db: Box<VacuumTempDb>,
        total_pages: u32,
        next_page: u32,
        prev_prepared: Option<Box<crate::storage::wal::PreparedFrames>>,
        completions: Vec<crate::io::Completion>,
    },
    /// Fsync the WAL if sync mode requires it.
    SyncSourceWal {
        temp_db: Box<VacuumTempDb>,
        sync_completion: crate::io::Completion,
    },
    /// Publish: commit_prepared_frames + finish_append_frames_commit, release
    /// source WAL locks, keep the VACUUM lock, and drop temp resources.
    PublishWalCommit { temp_db: Box<VacuumTempDb> },
    /// TRUNCATE checkpoint: copy WAL frames back into the DB file and truncate
    /// the WAL to zero. The pager's internal state machine handles the multi-step
    /// IO (backfill → sync DB → truncate WAL). Plain VACUUM requires this fold
    /// to complete before reporting success.
    Checkpoint,
    /// Reload the schema from the freshly committed (and possibly checkpointed) DB.
    SchemaReload,
    /// Clean up after successful commit.
    Done,
}

impl Default for VacuumInPlacePhase {
    fn default() -> Self {
        Self::Preflight
    }
}

/// The batch size for copy-back: how many temp pages to read per batch.
const VACUUM_COPY_BATCH_SIZE: u32 = 64;

fn vacuum_copy_batch_end(start_page: u32, total_pages: u32) -> u32 {
    turso_assert!(
        start_page > 0,
        "vacuum copy batch start page must be 1-based",
        { "start_page": start_page, "total_pages": total_pages }
    );
    turso_assert!(
        start_page <= total_pages,
        "vacuum copy batch start must be inside database image",
        { "start_page": start_page, "total_pages": total_pages }
    );
    turso_assert!(
        total_pages < u32::MAX,
        "vacuum copy batch requires a representable exclusive end page",
        { "total_pages": total_pages }
    );
    start_page
        .saturating_add(VACUUM_COPY_BATCH_SIZE)
        .min(total_pages + 1)
}

#[cfg(test)]
fn vacuum_copy_batch_ranges(total_pages: u32) -> Vec<(u32, u32)> {
    let mut ranges = Vec::new();
    let mut start = 1;
    while start <= total_pages {
        let end = vacuum_copy_batch_end(start, total_pages);
        ranges.push((start, end));
        start = end;
    }
    ranges
}

/// Coalesce `(page_ref, frame_id)` pairs into contiguous-frame-id runs.
///
/// Plain `VACUUM` relies on the temp-WAL-only invariant: every temp page
/// lives in the temp WAL (auto-checkpoint is disabled on the temp
/// connection), so for each logical page id we get a single frame id via
/// `find_frame`. Because target-build appends frames in phase order
/// (schema → rows → indexes → post-data), `find_frame(1), find_frame(2),
/// …` on the temp WAL returns a non-monotonic frame-id sequence. Sorting
/// by `frame_id` before splitting into runs lets a single `pread` over a
/// contiguous range serve many logical pages at once.
fn coalesce_frame_runs(
    mut pairs: Vec<(crate::storage::pager::PageRef, u64)>,
) -> Vec<(u64, Vec<crate::storage::pager::PageRef>)> {
    if pairs.is_empty() {
        return Vec::new();
    }
    pairs.sort_by_key(|(_, f)| *f);
    let mut runs: Vec<(u64, Vec<crate::storage::pager::PageRef>)> = Vec::new();
    let mut prev_frame_id = None;
    for (page, frame_id) in pairs {
        turso_assert!(frame_id > 0, "WAL frame ids must be 1-based");
        if let Some(prev) = prev_frame_id {
            turso_assert!(
                frame_id > prev,
                "VACUUM temp WAL frame ids must be unique",
                { "previous_frame_id": prev, "frame_id": frame_id }
            );
        }
        prev_frame_id = Some(frame_id);
        if let Some((run_start, run_pages)) = runs.last_mut() {
            let next_expected = *run_start + run_pages.len() as u64;
            if frame_id == next_expected {
                run_pages.push(page);
                continue;
            }
        }
        runs.push((frame_id, vec![page]));
    }
    runs
}

/// Start multi-inflight reads for one copy-back batch spanning logical
/// pages `[batch_start, batch_end)`. Every page must be resident in the
/// temp WAL (temp-WAL-only invariant). Returns the pre-allocated
/// per-page `PageRef`s in logical page-id order plus the aggregate
/// completion covering every run.
fn start_temp_batch_reads(
    temp_pager: &crate::storage::pager::Pager,
    batch_start: u32,
    batch_end: u32,
    scratch_buf: &mut Option<Arc<crate::io::Buffer>>,
) -> Result<(Vec<crate::storage::pager::PageRef>, crate::io::Completion)> {
    use crate::io::CompletionGroup;
    use crate::storage::pager::Page;
    use crate::storage::sqlite3_ondisk::WAL_FRAME_HEADER_SIZE;

    turso_assert!(
        batch_start < batch_end,
        "empty vacuum read batch",
        { "batch_start": batch_start, "batch_end": batch_end }
    );
    turso_assert!(
        batch_start > 0,
        "vacuum read batch start page must be 1-based",
        { "batch_start": batch_start, "batch_end": batch_end }
    );
    turso_assert!(
        batch_end - batch_start <= VACUUM_COPY_BATCH_SIZE,
        "vacuum read batch exceeds configured copy batch size",
        { "batch_start": batch_start, "batch_end": batch_end, "batch_size": VACUUM_COPY_BATCH_SIZE }
    );
    let wal = temp_pager
        .wal
        .as_ref()
        .ok_or_else(|| LimboError::InternalError("VACUUM requires a temp WAL pager".into()))?;

    let len = (batch_end - batch_start) as usize;
    let mut logical_pages: Vec<crate::storage::pager::PageRef> = Vec::with_capacity(len);
    let mut pairs: Vec<(crate::storage::pager::PageRef, u64)> = Vec::with_capacity(len);

    for page_id in batch_start..batch_end {
        let page: crate::storage::pager::PageRef = Arc::new(Page::new(page_id as i64));
        let frame_id = wal.find_frame(page_id as u64, None)?.ok_or_else(|| {
            LimboError::InternalError(format!(
                "VACUUM: temp page {page_id} not found in temp WAL (temp-WAL-only invariant violated)"
            ))
        })?;
        logical_pages.push(page.clone());
        pairs.push((page, frame_id));
    }

    let runs = coalesce_frame_runs(pairs);

    // Lazy-init the scratch buffer at max-batch size. It is reused by any run
    // whose read length matches exactly (the common case where target-build
    // wrote a contiguous chunk of at least `VACUUM_COPY_BATCH_SIZE` pages).
    let frame_size = temp_pager.get_page_size_unchecked().get() as usize + WAL_FRAME_HEADER_SIZE;
    let scratch_total = frame_size * VACUUM_COPY_BATCH_SIZE as usize;
    if scratch_buf
        .as_ref()
        .is_none_or(|b| b.len() != scratch_total)
    {
        *scratch_buf = Some(Arc::new(crate::io::Buffer::new_temporary(scratch_total)));
    }

    let mut group = CompletionGroup::new(|_| {});
    let mut scratch_claimed = false;
    for (start_frame, run_pages) in &runs {
        let run_total = frame_size * run_pages.len();
        let run_scratch = if !scratch_claimed && run_total == scratch_total {
            scratch_claimed = true;
            scratch_buf.clone()
        } else {
            None
        };
        let c = wal.read_frames_batch(
            *start_frame,
            run_pages,
            temp_pager.buffer_pool.clone(),
            run_scratch,
        )?;
        group.add(&c);
    }
    let combined = group.build();

    Ok((logical_pages, combined))
}

fn reload_schema_after_vacuum_commit(
    connection: &Arc<Connection>,
    db: usize,
    source_pager: &crate::storage::pager::Pager,
) -> Result<()> {
    turso_assert!(
        connection
            .auto_commit
            .load(std::sync::atomic::Ordering::SeqCst),
        "VACUUM schema reload must run after restoring auto-commit"
    );

    // Schema reload under a self-contained read guard. If this fails the
    // cleared schema cookie will trigger a re-read on the next operation.
    source_pager.begin_read_tx()?;
    connection.set_tx_state(crate::connection::TransactionState::Read);

    let reload_result = connection.reparse_schema();

    if reload_result.is_ok() {
        // Publish the freshly parsed schema to the shared Database so other
        // connections see the new cookie and table defs.
        let schema = connection.schema.read().clone();
        let source_db = connection.get_source_database(db);
        source_db.update_schema_if_newer(schema);
    }

    // Always end the schema-reload read tx and restore state, whether the
    // reload succeeded or failed.
    source_pager.end_read_tx();
    connection.set_tx_state(crate::connection::TransactionState::None);

    reload_result
}

/// Step the in-place VACUUM state machine once. Returns `IO` to yield or `Step`
/// when the entire operation is complete.
///
/// `cleanup_state` independently tracks which resources the opcode has acquired so
/// that cleanup can roll back correctly even when `phase` has been taken
/// by `std::mem::take` at the top of the loop.
pub(crate) fn vacuum_in_place_step(
    connection: &Arc<Connection>,
    db: usize,
    phase: &mut VacuumInPlacePhase,
    cleanup_state: &mut VacuumInPlaceCleanupState,
) -> Result<InsnFunctionStepResult> {
    use crate::io::WriteBatch as IOWriteBatch;
    use crate::types::IOCompletions;
    use crate::SyncMode;
    use std::sync::atomic::Ordering;

    // Capture the source pager once per step call. The pager never changes
    // during a VACUUM, so this avoids repeated ArcSwap loads in the loop.
    let source_pager = connection.get_pager_from_database_index(&db);

    loop {
        let current = std::mem::take(phase);
        match current {
            VacuumInPlacePhase::Preflight => {
                // 1. Must be in auto-commit mode (no explicit transaction).
                if !connection.auto_commit.load(Ordering::SeqCst) {
                    return Err(LimboError::TxError(
                        "cannot VACUUM from within a transaction".to_string(),
                    ));
                }
                // 2. No other active root statements on this connection.
                if connection.n_active_root_statements.load(Ordering::SeqCst) != 1 {
                    return Err(LimboError::TxError(
                        "cannot VACUUM - SQL statements in progress".to_string(),
                    ));
                }
                // 3. Reject readonly database.
                if connection.is_readonly(db) {
                    return Err(LimboError::ReadOnly);
                }
                // 4. Reject MVCC mode.
                let source_db = connection.get_source_database(db);
                if source_db.mvcc_enabled() {
                    return Err(LimboError::InternalError(
                        "VACUUM is not supported in MVCC mode yet".to_string(),
                    ));
                }
                // 5. Reject in-memory databases. Use the same memory-like
                // path classification as attach/open (connection.rs:2216).
                {
                    let path = &source_db.path;
                    let is_memory_db =
                        path == ":memory:" || path.starts_with("file::memory:") || path.is_empty();
                    if is_memory_db {
                        return Err(LimboError::InternalError(
                            "cannot VACUUM an in-memory database".to_string(),
                        ));
                    }
                }
                // 6. Reject auto-vacuum databases until preservation is
                // implemented. An auto-vacuum source overwritten with a
                // non-auto-vacuum temp image would leave the pager's
                // in-memory mode stale, causing pointer-map corruption.
                if source_pager.get_auto_vacuum_mode()
                    != crate::storage::pager::AutoVacuumMode::None
                {
                    return Err(LimboError::InternalError(
                        "VACUUM is not supported for auto-vacuum databases yet".to_string(),
                    ));
                }
                // 7. Reject non-WAL pagers.
                let wal = source_pager.wal.as_ref().ok_or_else(|| {
                    LimboError::InternalError("VACUUM requires a WAL-mode database".to_string())
                })?;
                // 8. Acquire the checkpoint serialization lock early. This
                // ensures a post-commit TRUNCATE checkpoint won't fail due
                // to a concurrent checkpointer. If the lock is unavailable
                // we fail fast before doing any expensive work.
                wal.try_begin_checkpoint_lock()?;
                cleanup_state.checkpoint_lock_held = true;
                *phase = VacuumInPlacePhase::BeginSourceTx;
                continue;
            }

            VacuumInPlacePhase::BeginSourceTx => {
                // Acquire exclusive WAL access in one shot:
                // vacuum lock + read_locks[0] exclusively + write_lock + connection snapshot.
                // Checkpoint lock was already acquired in Preflight.
                match source_pager.begin_exclusive_tx()? {
                    crate::IOResult::Done(()) => {
                        connection.auto_commit.store(false, Ordering::SeqCst);
                        connection.set_tx_state(crate::connection::TransactionState::Write {
                            schema_did_change: false,
                        });
                        cleanup_state.source_read_tx_open = true;
                        cleanup_state.source_write_tx_open = true;
                        cleanup_state.vacuum_lock_held = true;
                        *phase = VacuumInPlacePhase::ReadSourceMetadata;
                        continue;
                    }
                    crate::IOResult::IO(io) => {
                        *phase = VacuumInPlacePhase::BeginSourceTx;
                        return Ok(InsnFunctionStepResult::IO(io));
                    }
                }
            }

            VacuumInPlacePhase::ReadSourceMetadata => {
                turso_assert!(
                    cleanup_state.checkpoint_lock_held,
                    "VACUUM source metadata phase requires checkpoint lock"
                );
                turso_assert!(
                    cleanup_state.source_read_tx_open,
                    "VACUUM source metadata phase requires source read tx"
                );
                turso_assert!(
                    cleanup_state.source_write_tx_open,
                    "VACUUM source metadata phase requires source write tx"
                );
                let source_db = connection.get_source_database(db);

                // Read page size from pager (cached after begin_read_tx).
                let page_size = source_pager
                    .get_page_size()
                    .map(|ps| ps.get())
                    .unwrap_or(4096);

                // Read reserved bytes and header metadata in a single
                // with_header call to avoid redundant page-1 access.
                let io = &*source_pager.io;
                let (reserved_space, header_meta): (u8, VacuumDbHeaderMeta) =
                    match connection.get_reserved_bytes() {
                        Some(val) => {
                            let dh = io.block(|| {
                                source_pager.with_header(VacuumDbHeaderMeta::from_source_header)
                            })?;
                            (val, dh)
                        }
                        None => io.block(|| {
                            source_pager.with_header(|h| {
                                (h.reserved_space, VacuumDbHeaderMeta::from_source_header(h))
                            })
                        })?,
                    };
                // Create temp database.
                let temp_db =
                    open_vacuum_temp_db(connection, &source_db, page_size, reserved_space)?;

                mirror_symbols(connection, &temp_db.conn);
                let source_custom_types = capture_custom_types(connection, db);

                let config = VacuumTargetBuildConfig {
                    source_conn: connection.clone(),
                    escaped_schema_name: "main".to_string(),
                    source_db_id: db,
                    header_meta,
                    source_custom_types,
                    source_mvcc_enabled: false, // Rejected MVCC above
                };

                let target_build_context = VacuumTargetBuildContext::new(temp_db.conn.clone());

                *phase = VacuumInPlacePhase::TargetBuild {
                    config: Box::new(config),
                    temp_db: Box::new(temp_db),
                    context: Box::new(target_build_context),
                };
                continue;
            }

            VacuumInPlacePhase::TargetBuild {
                config,
                temp_db,
                mut context,
            } => {
                match vacuum_target_build_step(&config, &mut context)? {
                    crate::IOResult::Done(()) => {
                        // Temp build complete. Move to read tx on temp.
                        drop(config);
                        drop(context);
                        *phase = VacuumInPlacePhase::BeginTempReadTx { temp_db };
                        continue;
                    }
                    crate::IOResult::IO(io) => {
                        *phase = VacuumInPlacePhase::TargetBuild {
                            config,
                            temp_db,
                            context,
                        };
                        return Ok(InsnFunctionStepResult::IO(io));
                    }
                }
            }

            VacuumInPlacePhase::BeginTempReadTx { temp_db } => {
                // The shared temp-build engine committed via SQL COMMIT, which
                // ends the write transaction but leaves the WAL read mark held.
                // End that read tx first, then start a fresh one so WAL lookups
                // see the committed temp image.
                let temp_pager = temp_db.conn.get_pager();
                temp_pager.end_read_tx();
                temp_pager.begin_read_tx()?;

                // Determine the compacted image size from temp page 1 header.
                let io = &*temp_pager.io;
                let total_pages: u32 =
                    io.block(|| temp_pager.with_header(|h| h.database_size.get()))?;

                if total_pages == 0 {
                    // Empty database — nothing to copy back. Clean up the
                    // source write tx and connection state we acquired earlier.
                    drop(temp_db);
                    let wal = source_pager
                        .wal
                        .as_ref()
                        .ok_or_else(|| LimboError::InternalError("VACUUM requires WAL".into()))?;
                    source_pager.end_write_tx();
                    source_pager.end_read_tx();
                    cleanup_state.source_write_tx_open = false;
                    cleanup_state.source_read_tx_open = false;
                    if cleanup_state.checkpoint_lock_held {
                        wal.release_checkpoint_lock();
                        cleanup_state.checkpoint_lock_held = false;
                    }
                    if cleanup_state.vacuum_lock_held {
                        wal.release_vacuum_lock();
                        cleanup_state.vacuum_lock_held = false;
                    }
                    connection.auto_commit.store(true, Ordering::SeqCst);
                    connection.set_tx_state(crate::connection::TransactionState::None);
                    *phase = VacuumInPlacePhase::Done;
                    continue;
                }

                // Initialize the source WAL header before the first batch.
                let wal = source_pager
                    .wal
                    .as_ref()
                    .ok_or_else(|| LimboError::InternalError("VACUUM requires WAL".into()))?;
                let page_sz = source_pager.get_page_size_unchecked();

                if let Some(header_write_c) = wal.prepare_wal_start(page_sz)? {
                    // WAL not yet initialized — yield on the header write.
                    *phase = VacuumInPlacePhase::InitSourceWalHeader {
                        temp_db,
                        total_pages,
                        completion: header_write_c,
                        fsync_phase: false,
                    };
                    continue;
                }

                // WAL already initialized — kick off the first read batch.
                let temp_pager = temp_db.conn.get_pager();
                let batch_end = vacuum_copy_batch_end(1, total_pages);
                let (batch_pages, read_completion) = start_temp_batch_reads(
                    &temp_pager,
                    1,
                    batch_end,
                    &mut cleanup_state.read_scratch_buf,
                )?;

                *phase = VacuumInPlacePhase::ReadTempBatch {
                    temp_db,
                    total_pages,
                    next_page: batch_end,
                    prev_prepared: None,
                    batch_pages,
                    read_completion,
                };
                continue;
            }

            VacuumInPlacePhase::InitSourceWalHeader {
                temp_db,
                total_pages,
                completion,
                fsync_phase,
            } => {
                turso_assert!(
                    total_pages > 0,
                    "VACUUM source WAL header initialization requires pages to copy"
                );
                if !completion.finished() {
                    *phase = VacuumInPlacePhase::InitSourceWalHeader {
                        temp_db,
                        total_pages,
                        completion: completion.clone(),
                        fsync_phase,
                    };
                    return Ok(InsnFunctionStepResult::IO(IOCompletions::Single(
                        completion,
                    )));
                }
                if !completion.succeeded() {
                    return Err(LimboError::InternalError(
                        "VACUUM: WAL header init failed".to_string(),
                    ));
                }

                if !fsync_phase {
                    // Header write done — issue the fsync via prepare_wal_finish
                    // to set WAL `initialized = true`.
                    let wal = source_pager.wal.as_ref().unwrap();
                    let sync_c = wal.prepare_wal_finish(source_pager.get_sync_type())?;
                    *phase = VacuumInPlacePhase::InitSourceWalHeader {
                        temp_db,
                        total_pages,
                        completion: sync_c,
                        fsync_phase: true,
                    };
                    continue;
                }

                // WAL header fully initialized. Kick off the first read batch.
                let temp_pager = temp_db.conn.get_pager();
                let batch_end = vacuum_copy_batch_end(1, total_pages);
                let (batch_pages, read_completion) = start_temp_batch_reads(
                    &temp_pager,
                    1,
                    batch_end,
                    &mut cleanup_state.read_scratch_buf,
                )?;

                *phase = VacuumInPlacePhase::ReadTempBatch {
                    temp_db,
                    total_pages,
                    next_page: batch_end,
                    prev_prepared: None,
                    batch_pages,
                    read_completion,
                };
                continue;
            }

            VacuumInPlacePhase::ReadTempBatch {
                temp_db,
                total_pages,
                next_page,
                prev_prepared,
                batch_pages,
                read_completion,
            } => {
                turso_assert!(
                    total_pages < u32::MAX,
                    "VACUUM read batch requires a representable exclusive end page",
                    { "total_pages": total_pages }
                );
                let database_end = total_pages + 1;
                turso_assert!(
                    !batch_pages.is_empty(),
                    "VACUUM read batch must contain pages"
                );
                turso_assert!(
                    batch_pages.len() as u32 <= VACUUM_COPY_BATCH_SIZE,
                    "VACUUM read batch exceeds configured copy batch size",
                    { "batch_pages": batch_pages.len(), "batch_size": VACUUM_COPY_BATCH_SIZE }
                );
                turso_assert!(
                    next_page > 1 && next_page <= database_end,
                    "VACUUM read batch next page is outside database image",
                    { "next_page": next_page, "total_pages": total_pages }
                );
                // Wait for every run in this batch to finish.
                if !read_completion.finished() {
                    *phase = VacuumInPlacePhase::ReadTempBatch {
                        temp_db,
                        total_pages,
                        next_page,
                        prev_prepared,
                        batch_pages,
                        read_completion: read_completion.clone(),
                    };
                    return Ok(InsnFunctionStepResult::IO(IOCompletions::Single(
                        read_completion,
                    )));
                }
                if !read_completion.succeeded() {
                    return Err(LimboError::InternalError(
                        "VACUUM: temp batch read failed".to_string(),
                    ));
                }

                // All pages in this batch are loaded. Prepare WAL frames.
                for page in &batch_pages {
                    turso_assert!(
                        page.is_loaded(),
                        "VACUUM read batch page must be loaded before WAL prepare",
                        { "page_id": page.get().id }
                    );
                    turso_assert!(
                        !page.is_locked(),
                        "VACUUM read batch page lock leaked before WAL prepare",
                        { "page_id": page.get().id }
                    );
                }
                let all_read = next_page > total_pages;
                let wal = source_pager
                    .wal
                    .as_ref()
                    .ok_or_else(|| LimboError::InternalError("VACUUM requires WAL".into()))?;
                let page_sz = source_pager.get_page_size_unchecked();

                let db_size_on_commit = if all_read { Some(total_pages) } else { None };

                let prepared = wal.prepare_frames(
                    &batch_pages,
                    page_sz,
                    db_size_on_commit,
                    prev_prepared.as_deref(),
                )?;

                // Submit writes via WriteBatch.
                let wal_file = wal.wal_file()?;
                let mut batch = IOWriteBatch::new(wal_file);
                batch.writev(prepared.offset, &prepared.bufs);
                let completions = batch.submit()?;

                *phase = VacuumInPlacePhase::WriteWalBatch {
                    temp_db,
                    total_pages,
                    next_page,
                    prev_prepared: Some(Box::new(prepared)),
                    completions,
                };
                continue;
            }

            VacuumInPlacePhase::WriteWalBatch {
                temp_db,
                total_pages,
                next_page,
                prev_prepared,
                completions,
            } => {
                turso_assert!(
                    total_pages < u32::MAX,
                    "VACUUM WAL write batch requires a representable exclusive end page",
                    { "total_pages": total_pages }
                );
                let database_end = total_pages + 1;
                turso_assert!(
                    prev_prepared.is_some(),
                    "VACUUM WAL write batch requires prepared frames"
                );
                turso_assert!(
                    !completions.is_empty(),
                    "VACUUM WAL write batch requires write completions"
                );
                turso_assert!(
                    next_page > 1 && next_page <= database_end,
                    "VACUUM WAL write batch next page is outside database image",
                    { "next_page": next_page, "total_pages": total_pages }
                );
                // Wait for all writes in this batch. We yield on the first
                // unfinished completion; re-entry will re-check them all.
                let pending = completions.iter().find(|c| !c.finished()).cloned();
                if let Some(pending) = pending {
                    *phase = VacuumInPlacePhase::WriteWalBatch {
                        temp_db,
                        total_pages,
                        next_page,
                        prev_prepared,
                        completions,
                    };
                    return Ok(InsnFunctionStepResult::IO(IOCompletions::Single(pending)));
                }

                // Check for write errors.
                for c in &completions {
                    if !c.succeeded() {
                        return Err(LimboError::InternalError(
                            "VACUUM: WAL write failed".to_string(),
                        ));
                    }
                }

                // Commit this batch's prepared frames to advance local WAL
                // index state (page→frame mapping, max_frame, checksum).
                //
                // We intentionally skip finalize_committed_pages() here. That
                // call clears dirty flags and sets WAL tags on PageRefs that
                // live in the source page cache. Our pages come from the temp
                // pager's read_page_no_cache — they are not in the source page
                // cache and have no dirty state to clear. The source page cache
                // is invalidated wholesale via clear_page_cache() in Publish.
                let wal = source_pager.wal.as_ref().unwrap();
                let prepared = prev_prepared.as_ref().unwrap();
                wal.commit_prepared_frames(std::slice::from_ref(prepared.as_ref()));

                // More pages to copy?
                if next_page <= total_pages {
                    // Kick off the next read batch in parallel.
                    let temp_pager = temp_db.conn.get_pager();
                    let batch_end = vacuum_copy_batch_end(next_page, total_pages);
                    let (batch_pages, read_completion) = start_temp_batch_reads(
                        &temp_pager,
                        next_page,
                        batch_end,
                        &mut cleanup_state.read_scratch_buf,
                    )?;

                    *phase = VacuumInPlacePhase::ReadTempBatch {
                        temp_db,
                        total_pages,
                        next_page: batch_end,
                        prev_prepared,
                        batch_pages,
                        read_completion,
                    };
                    continue;
                }

                // All pages written. Fsync WAL if sync mode requires it.
                // NORMAL mode skips fsync on WAL commit (fsyncs happen on
                // checkpoint and WAL restart instead). This matches the regular
                // pager commit path in Pager::commit_tx / CommitState.
                let sync_mode = connection.get_sync_mode();
                if sync_mode == SyncMode::Full {
                    let sync_c = wal.sync(source_pager.get_sync_type())?;
                    *phase = VacuumInPlacePhase::SyncSourceWal {
                        temp_db,
                        sync_completion: sync_c,
                    };
                    continue;
                }

                // No sync needed — proceed directly to publish.
                *phase = VacuumInPlacePhase::PublishWalCommit { temp_db };
                continue;
            }

            VacuumInPlacePhase::SyncSourceWal {
                temp_db,
                sync_completion,
            } => {
                if !sync_completion.finished() {
                    *phase = VacuumInPlacePhase::SyncSourceWal {
                        temp_db,
                        sync_completion: sync_completion.clone(),
                    };
                    return Ok(InsnFunctionStepResult::IO(IOCompletions::Single(
                        sync_completion,
                    )));
                }
                if !sync_completion.succeeded() {
                    return Err(LimboError::InternalError(
                        "VACUUM: WAL fsync failed".to_string(),
                    ));
                }
                *phase = VacuumInPlacePhase::PublishWalCommit { temp_db };
                continue;
            }

            VacuumInPlacePhase::PublishWalCommit { temp_db } => {
                turso_assert!(
                    cleanup_state.checkpoint_lock_held,
                    "VACUUM publish phase requires checkpoint lock"
                );
                turso_assert!(
                    cleanup_state.source_read_tx_open,
                    "VACUUM publish phase requires source read tx"
                );
                turso_assert!(
                    cleanup_state.source_write_tx_open,
                    "VACUUM publish phase requires source write tx"
                );
                let wal = source_pager
                    .wal
                    .as_ref()
                    .ok_or_else(|| LimboError::InternalError("VACUUM requires WAL".into()))?;

                // Publish the WAL transaction to shared state. Once this
                // succeeds the compacted image is durable — rollback must
                // not be attempted even if later schema reload fails.
                wal.finish_append_frames_commit()?;
                cleanup_state.wal_commit_published = true;

                wal.handoff_exclusive_tx_to_checkpoint();
                cleanup_state.source_write_tx_open = false;
                cleanup_state.source_read_tx_open = false;
                cleanup_state.checkpoint_lock_held = false;
                cleanup_state.checkpoint_locks_handed_off = true;

                // Restore connection bookkeeping immediately. The commit is
                // durable so there is nothing to roll back; restoring here
                // means the connection is never left poisoned even if the
                // checkpoint or schema reload below fails.
                connection.auto_commit.store(true, Ordering::SeqCst);
                connection.set_tx_state(crate::connection::TransactionState::None);

                // Invalidate page cache and schema cookie so fresh reads see
                // the newly committed WAL frames.
                source_pager.clear_page_cache(false);
                source_pager.set_schema_cookie(None);

                // Drop temp resources before checkpoint.
                drop(temp_db);

                *phase = VacuumInPlacePhase::Checkpoint;
                continue;
            }

            VacuumInPlacePhase::Checkpoint => {
                // TRUNCATE checkpoint: copy WAL frames into the DB file,
                // sync the DB, then truncate the WAL to zero bytes.
                let sync_mode = connection.get_sync_mode();
                match source_pager.checkpoint(
                    crate::storage::wal::CheckpointMode::Truncate {
                        upper_bound_inclusive: None,
                    },
                    sync_mode,
                    true,
                ) {
                    Ok(crate::IOResult::Done(result)) => {
                        cleanup_state.checkpoint_locks_handed_off = false;
                        if !result.should_truncate() {
                            if cleanup_state.vacuum_lock_held {
                                let wal = source_pager.wal.as_ref().ok_or_else(|| {
                                    LimboError::InternalError("VACUUM requires WAL".into())
                                })?;
                                wal.release_vacuum_lock();
                                cleanup_state.vacuum_lock_held = false;
                            }
                            return Err(LimboError::Busy);
                        }
                        if cleanup_state.vacuum_lock_held {
                            let wal = source_pager.wal.as_ref().ok_or_else(|| {
                                LimboError::InternalError("VACUUM requires WAL".into())
                            })?;
                            wal.release_vacuum_lock();
                            cleanup_state.vacuum_lock_held = false;
                        }
                        *phase = VacuumInPlacePhase::SchemaReload;
                        continue;
                    }
                    Ok(crate::IOResult::IO(io)) => {
                        *phase = VacuumInPlacePhase::Checkpoint;
                        return Ok(InsnFunctionStepResult::IO(io));
                    }
                    Err(err) => {
                        tracing::info!("VACUUM post-commit checkpoint failed: {err}");
                        if cleanup_state.checkpoint_locks_handed_off {
                            source_pager.cleanup_after_checkpoint_failure();
                            cleanup_state.checkpoint_locks_handed_off = false;
                        }
                        if cleanup_state.vacuum_lock_held {
                            if let Some(wal) = source_pager.wal.as_ref() {
                                wal.release_vacuum_lock();
                                cleanup_state.vacuum_lock_held = false;
                            }
                        }
                        if let Err(reload_err) =
                            reload_schema_after_vacuum_commit(connection, db, &source_pager)
                        {
                            tracing::info!(
                                "VACUUM schema reload after checkpoint failure also failed: {reload_err}"
                            );
                        }
                        return Err(err);
                    }
                }
            }

            VacuumInPlacePhase::SchemaReload => {
                reload_schema_after_vacuum_commit(connection, db, &source_pager)?;

                *phase = VacuumInPlacePhase::Done;
                return Ok(InsnFunctionStepResult::Step);
            }

            VacuumInPlacePhase::Done => {
                return Ok(InsnFunctionStepResult::Step);
            }
        }
    }
}

/// Roll back the source transaction and restore connection state after a
/// in-place VACUUM failure. Uses `cleanup_state` flags to decide what to undo;
/// these are independent of the phase enum and survive `std::mem::take`.
///
/// `phase` is taken by value so helper statements inside `TargetBuild` are
/// dropped before we attempt `rollback_tx`. This
/// avoids the nestedness suppression bug where live helper statements keep
/// `Connection::nestedness > 0`, making `rollback_tx` a no-op.
pub(crate) fn vacuum_in_place_cleanup(
    connection: &Arc<Connection>,
    db: usize,
    phase: VacuumInPlacePhase,
    cleanup_state: &VacuumInPlaceCleanupState,
) {
    use std::sync::atomic::Ordering;

    turso_assert!(
        !cleanup_state.source_write_tx_open || cleanup_state.source_read_tx_open,
        "VACUUM cleanup cannot have source write tx without source read tx"
    );
    turso_assert!(
        !(cleanup_state.checkpoint_lock_held && cleanup_state.checkpoint_locks_handed_off),
        "VACUUM cleanup cannot own raw checkpoint lock and checkpoint handoff"
    );

    // Release the checkpoint lock if we acquired it.
    if cleanup_state.checkpoint_lock_held {
        let pager = connection.get_pager_from_database_index(&db);
        if let Some(wal) = pager.wal.as_ref() {
            wal.release_checkpoint_lock();
        }
    }

    if cleanup_state.checkpoint_locks_handed_off {
        let pager = connection.get_pager_from_database_index(&db);
        pager.cleanup_after_checkpoint_failure();
    }

    // Nothing else acquired — nothing to undo.
    if !cleanup_state.source_read_tx_open && !cleanup_state.source_write_tx_open {
        if cleanup_state.vacuum_lock_held {
            let pager = connection.get_pager_from_database_index(&db);
            if let Some(wal) = pager.wal.as_ref() {
                wal.release_vacuum_lock();
            }
        }
        return;
    }

    // Drop the phase first to release any target-build helper statements.
    // Their Drop impls decrement Connection::nestedness, which must happen
    // before rollback_tx (which is a no-op while nested).
    drop(phase);

    if cleanup_state.wal_commit_published {
        // The compacted image was already durably committed. We must not
        // roll back — just restore connection bookkeeping.
        let pager = connection.get_pager_from_database_index(&db);
        pager.end_write_tx();
        pager.end_read_tx();
    } else if cleanup_state.source_write_tx_open {
        // Write transaction open but not yet committed — roll back.
        let pager = connection.get_pager_from_database_index(&db);
        pager.rollback_tx(connection);
    } else {
        // Only read transaction — just release the read lock.
        let pager = connection.get_pager_from_database_index(&db);
        pager.end_read_tx();
        if cleanup_state.vacuum_lock_held {
            if let Some(wal) = pager.wal.as_ref() {
                wal.release_vacuum_lock();
            }
        }
        return; // auto_commit and tx_state were never modified.
    }

    if cleanup_state.vacuum_lock_held {
        let pager = connection.get_pager_from_database_index(&db);
        if let Some(wal) = pager.wal.as_ref() {
            wal.release_vacuum_lock();
        }
    }

    connection.auto_commit.store(true, Ordering::SeqCst);
    connection.set_tx_state(crate::connection::TransactionState::None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::encryption::{CipherMode, EncryptionKey};
    use crate::storage::pager::Page;
    use crate::util::IOExt;

    fn page_ref(id: i64) -> crate::storage::pager::PageRef {
        Arc::new(Page::new(id))
    }

    #[test]
    fn coalesce_frame_runs_empty_returns_empty() {
        let runs = coalesce_frame_runs(Vec::new());
        assert!(runs.is_empty());
    }

    #[test]
    fn coalesce_frame_runs_single_page_is_one_run_of_one() {
        let runs = coalesce_frame_runs(vec![(page_ref(1), 42)]);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, 42);
        assert_eq!(runs[0].1.len(), 1);
        assert_eq!(runs[0].1[0].get().id, 1);
    }

    #[test]
    fn coalesce_frame_runs_sorts_by_frame_id_then_coalesces_contiguous() {
        // Target-build writes frames out of page-id order. The input is
        // sorted by page id (1,2,3,4), but the frame ids interleave with a
        // gap at frame 3, producing two runs once sorted by frame id.
        let pairs = vec![
            (page_ref(1), 10),
            (page_ref(2), 11),
            (page_ref(3), 12),
            (page_ref(4), 14),
        ];
        let runs = coalesce_frame_runs(pairs);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].0, 10);
        assert_eq!(runs[0].1.len(), 3);
        assert_eq!(
            runs[0].1.iter().map(|p| p.get().id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(runs[1].0, 14);
        assert_eq!(runs[1].1.len(), 1);
        assert_eq!(runs[1].1[0].get().id, 4);
    }

    #[test]
    fn coalesce_frame_runs_handles_non_monotonic_page_to_frame_map() {
        // Realistic target-build shape: pages 1..=6 but frames assigned by
        // phase order (schema page goes last, indexes interleave). The
        // coalescer must still produce runs of length > 1 for contiguous
        // frame ranges rather than falling back to length-1 runs.
        let pairs = vec![
            (page_ref(1), 10), // schema written last (gap between 6 and 10)
            (page_ref(2), 1),  // data pages written first
            (page_ref(3), 2),
            (page_ref(4), 3),
            (page_ref(5), 5), // index pages later
            (page_ref(6), 6),
        ];
        let runs = coalesce_frame_runs(pairs);
        // After sort by frame id: frames 1,2,3 contiguous; 5,6 contiguous;
        // 10 isolated.
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].0, 1);
        assert_eq!(runs[0].1.len(), 3);
        assert_eq!(runs[1].0, 5);
        assert_eq!(runs[1].1.len(), 2);
        assert_eq!(runs[2].0, 10);
        assert_eq!(runs[2].1.len(), 1);

        // Average run length should be non-trivial (>1).
        let total: usize = runs.iter().map(|(_, pages)| pages.len()).sum();
        let avg = total as f64 / runs.len() as f64;
        assert!(avg > 1.0, "expected average run length > 1, got {avg}");
    }

    #[test]
    fn coalesce_frame_runs_all_scattered_is_singleton_runs() {
        let pairs = vec![(page_ref(1), 10), (page_ref(2), 20), (page_ref(3), 30)];
        let runs = coalesce_frame_runs(pairs);
        assert_eq!(runs.len(), 3);
        for (_, pages) in &runs {
            assert_eq!(pages.len(), 1);
        }
    }

    #[test]
    fn coalesce_frame_runs_keeps_run_pages_in_physical_frame_order() {
        let runs = coalesce_frame_runs(vec![
            (page_ref(1), 12),
            (page_ref(2), 10),
            (page_ref(3), 11),
            (page_ref(4), 13),
        ]);

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, 10);
        assert_eq!(
            runs[0].1.iter().map(|p| p.get().id).collect::<Vec<_>>(),
            vec![2, 3, 1, 4]
        );
    }

    #[test]
    fn vacuum_copy_batch_ranges_cover_boundary_page_counts() {
        let size = VACUUM_COPY_BATCH_SIZE;
        assert!(vacuum_copy_batch_ranges(0).is_empty());
        assert_eq!(vacuum_copy_batch_ranges(1), vec![(1, 2)]);
        assert_eq!(vacuum_copy_batch_ranges(2), vec![(1, 3)]);
        assert_eq!(vacuum_copy_batch_ranges(size - 1), vec![(1, size)]);
        assert_eq!(vacuum_copy_batch_ranges(size), vec![(1, size + 1)]);
        assert_eq!(
            vacuum_copy_batch_ranges(size + 1),
            vec![(1, size + 1), (size + 1, size + 2)]
        );
        assert_eq!(
            vacuum_copy_batch_ranges(size * 2),
            vec![(1, size + 1), (size + 1, size * 2 + 1)]
        );
        assert_eq!(
            vacuum_copy_batch_ranges(size * 2 + 1),
            vec![
                (1, size + 1),
                (size + 1, size * 2 + 1),
                (size * 2 + 1, size * 2 + 2),
            ]
        );
    }

    #[test]
    fn vacuum_db_header_meta_bumps_schema_cookie_and_preserves_sqlite_metadata() {
        let mut source = DatabaseHeader::default();
        source.schema_cookie = u32::MAX.into();
        source.default_page_cache_size = CacheSize::new(123);
        source.text_encoding = TextEncoding::Utf8;
        source.user_version = 7.into();
        source.application_id = 12.into();

        let VacuumDbHeaderMeta {
            schema_cookie,
            default_page_cache_size,
            text_encoding,
            user_version,
            application_id,
        } = VacuumDbHeaderMeta::from_source_header(&source);

        assert_eq!(schema_cookie, 0);
        assert_eq!(default_page_cache_size, CacheSize::new(123));
        assert_eq!(text_encoding, TextEncoding::Utf8);
        assert_eq!(user_version, 7);
        assert_eq!(application_id, 12);
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn vacuum_db_header_meta_updates_target_header() -> Result<()> {
        let io: Arc<dyn crate::IO> = Arc::new(crate::io::PlatformIO::new()?);
        let source_dir = tempfile::tempdir().unwrap();
        let source_path = source_dir.path().join("source.db");
        let source_path = source_path.to_str().unwrap();
        let source_db = Database::open_file_with_flags(
            io,
            source_path,
            OpenFlags::Create,
            DatabaseOpts::new(),
            None,
        )?;
        let source_conn = source_db.connect()?;
        let temp = open_vacuum_temp_db(&source_conn, &source_db, 4096, 0)?;

        let mut source_header = DatabaseHeader::default();
        source_header.schema_cookie = 41.into();
        source_header.default_page_cache_size = CacheSize::new(321);
        source_header.text_encoding = TextEncoding::Utf8;
        source_header.user_version = 17.into();
        source_header.application_id = 29.into();
        let header_meta = VacuumDbHeaderMeta::from_source_header(&source_header);

        temp.conn.execute("BEGIN")?;
        match finalize_vacuum_target_header(&temp.conn, &header_meta)? {
            crate::IOResult::Done(()) => {}
            crate::IOResult::IO(_) => panic!("fresh temp header should not need async I/O"),
        }
        temp.conn.execute("COMMIT")?;

        let pager = temp.conn.pager.load();
        let header = pager.io.block(|| {
            pager.with_header(|header| {
                (
                    header.schema_cookie.get(),
                    header.default_page_cache_size,
                    header.text_encoding,
                    header.user_version.get(),
                    header.application_id.get(),
                )
            })
        })?;

        assert_eq!(
            header,
            (42, CacheSize::new(321), TextEncoding::Utf8, 17, 29)
        );

        Ok(())
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn internal_vacuum_temp_db_uses_source_runtime_and_disables_auto_checkpoint() -> Result<()> {
        let io: Arc<dyn crate::IO> = Arc::new(crate::io::PlatformIO::new()?);
        let source_dir = tempfile::tempdir().unwrap();
        let source_path = source_dir.path().join("source.db");
        let source_path = source_path.to_str().unwrap();
        let source_db = Database::open_file_with_flags(
            io,
            source_path,
            OpenFlags::Create,
            DatabaseOpts::new(),
            None,
        )?;
        let source_conn = source_db.connect()?;

        let temp = open_vacuum_temp_db(&source_conn, &source_db, 4096, 0)?;

        assert!(Arc::ptr_eq(&temp._db.io, &source_db.io));
        assert_ne!(temp.path, source_db.path);
        assert!(temp.conn.is_wal_auto_checkpoint_disabled());
        assert_eq!(temp.conn.get_page_size().get(), 4096);
        assert_eq!(temp.conn.get_reserved_bytes(), Some(0));

        Ok(())
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn internal_vacuum_temp_db_preserves_source_encryption() -> Result<()> {
        let io: Arc<dyn crate::IO> = Arc::new(crate::io::PlatformIO::new()?);
        let source_dir = tempfile::tempdir().unwrap();
        let source_path = source_dir.path().join("encrypted-source.db");
        let source_path = source_path.to_str().unwrap();
        let key_hex = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        let key = EncryptionKey::from_hex_string(key_hex)?;
        let source_db = Database::open_file_with_flags(
            io,
            source_path,
            OpenFlags::Create,
            DatabaseOpts::new().with_encryption(true),
            Some(EncryptionOpts {
                cipher: CipherMode::Aes256Gcm.to_string(),
                hexkey: key_hex.to_string(),
            }),
        )?;
        let source_conn = source_db.connect_with_encryption(Some(key))?;
        let reserved_space = source_conn
            .get_reserved_bytes()
            .expect("encrypted source should have reserved bytes");

        let temp = open_vacuum_temp_db(&source_conn, &source_db, 4096, reserved_space)?;

        assert!(temp._db.experimental_encryption_enabled());
        assert_eq!(
            temp.conn.get_encryption_cipher_mode(),
            source_conn.get_encryption_cipher_mode()
        );
        assert!(temp.conn.encryption_key.read().is_some());
        assert_eq!(temp.conn.get_reserved_bytes(), Some(reserved_space));

        Ok(())
    }
}
