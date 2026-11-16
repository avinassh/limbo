//! Deterministic encryption authentication, API-misuse, and I/O-failure scenarios.
//! Each test asserts the observations that show it reached the
//! state it is named after; a scenario that never reaches its state fails.
//!
//! S04 (connect resumed after encryption was established) is a core unit
//! test because it needs private connection-construction steps; S09 and S10
//! (concurrent configuration, plaintext first write against encryption) are
//! seeded Shuttle tests in `core/connection.rs`. S18 reads encrypted files
//! made by the older base commit recorded with the fixtures.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

const KEY_A: u8 = 1;
const KEY_B: u8 = 2;
const KEY_C: u8 = 3;

fn unique(name: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        ":encryption-scenario:{name}:{}:{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

struct Env {
    inner: Arc<MemoryIO>,
    io: Arc<QueuedIo>,
    path: String,
}

impl Env {
    fn new(name: &str) -> Env {
        let inner = Arc::new(MemoryIO::new());
        let io = Arc::new(QueuedIo::new_with_io(inner.clone()));
        io.set_stall_limit(Some(STALL_LIMIT));
        Env {
            inner,
            io,
            path: unique(name),
        }
    }

    fn base(&self) -> OpenOptions {
        OpenOptions::new(Arc::new(SqliteDialect))
            .db_opts(DatabaseOpts::new().with_encryption(true).with_attach(true))
    }

    fn encrypted(&self, cipher: CipherMode, byte: u8) -> OpenOptions {
        self.base().encryption(encryption_options(cipher, byte))
    }

    fn open(&self, options: OpenOptions) -> Arc<Database> {
        Database::open(self.io.clone(), &self.path, options).unwrap()
    }

    fn size(&self, path: &str) -> u64 {
        self.inner
            .open_file(path, OpenFlags::Create, false)
            .unwrap()
            .size()
            .unwrap()
    }

    fn writes_to(&self, path: &str) -> usize {
        self.io
            .records()
            .iter()
            .filter(|record| {
                record.event.path == path
                    && matches!(
                        record.event.kind,
                        QueuedIoOpKind::Pwrite | QueuedIoOpKind::Pwritev
                    )
            })
            .count()
    }
}

fn connect(
    db: &Arc<Database>,
    cipher: CipherMode,
    byte: Option<u8>,
) -> turso_core::Result<Arc<Connection>> {
    db.connect(ConnectOpts::new().with_encryption_key(byte.map(|byte| key(cipher, byte))))
}

fn assert_refused<T>(result: turso_core::Result<T>, context: &str) -> Category {
    match result {
        Ok(_) => panic!("{context}: expected a credential refusal, got success"),
        Err(error) => {
            let category = classify(&error);
            assert!(
                matches!(
                    category,
                    Category::CredentialMissing
                        | Category::CredentialWrong
                        | Category::CipherMismatch
                ),
                "{context}: expected a credential refusal, got {error}"
            );
            category
        }
    }
}

fn populate(connection: &Arc<Connection>, marker: &str) {
    connection
        .execute("CREATE TABLE secrets(id INTEGER PRIMARY KEY, value TEXT)")
        .unwrap();
    connection
        .execute(format!("INSERT INTO secrets VALUES (0, '{marker}')"))
        .unwrap();
}

fn marker(connection: &Arc<Connection>) -> Vec<String> {
    first_column(connection, "SELECT value FROM secrets").unwrap()
}

/// Observations a scenario must make before it counts as covered.
#[derive(Default)]
struct Coverage(BTreeSet<String>);

impl Coverage {
    fn see(&mut self, observation: impl Into<String>) {
        self.0.insert(observation.into());
    }

    fn require(&self, scenario: &str, required: &[&str]) {
        let missing: Vec<_> = required
            .iter()
            .filter(|observation| !self.0.contains(**observation))
            .collect();
        assert!(
            missing.is_empty(),
            "{scenario} did not reach {missing:?}; observed {:?}",
            self.0
        );
    }
}

#[test]
fn s01_public_uri_access_to_a_cached_database() {
    for mvcc in [true, false] {
        let mut coverage = Coverage::default();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("s01.db");
        let uri = |byte: Option<u8>| match byte {
            Some(byte) => format!(
                "file:{}?cipher=aegis256&hexkey={}",
                path.display(),
                encryption_options(CipherMode::Aegis256, byte).hexkey
            ),
            None => format!("file:{}", path.display()),
        };
        let opts = || DatabaseOpts::new().with_encryption(true);
        let (_io, owner) =
            Connection::from_uri(&uri(Some(KEY_A)), opts(), Arc::new(SqliteDialect)).unwrap();
        if mvcc {
            owner.execute("PRAGMA journal_mode = 'mvcc'").unwrap();
        }
        populate(&owner, "protected value");
        assert_eq!(marker(&owner), ["protected value"]);
        // `owner` keeps the database open for the rest of the scenario, so the
        // URI opens below find it in the registry.
        let wrong = Connection::from_uri(&uri(Some(KEY_B)), opts(), Arc::new(SqliteDialect));
        assert_eq!(
            assert_refused(wrong, "public from_uri with the wrong key"),
            Category::CredentialWrong
        );
        coverage.see("public_from_uri_wrong_key_refused");
        let missing = Connection::from_uri(&uri(None), opts(), Arc::new(SqliteDialect));
        assert_refused(missing, "public from_uri without a key");
        coverage.see("public_from_uri_missing_key_refused");
        let (_io, peer) =
            Connection::from_uri(&uri(Some(KEY_A)), opts(), Arc::new(SqliteDialect)).unwrap();
        assert_eq!(marker(&peer), ["protected value"]);
        coverage.see("public_from_uri_right_key_reads");
        assert_eq!(marker(&owner), ["protected value"]);
        coverage.see("original_connection_still_reads");
        coverage.require(
            &format!("S01 mvcc={mvcc}"),
            &[
                "public_from_uri_wrong_key_refused",
                "public_from_uri_missing_key_refused",
                "public_from_uri_right_key_reads",
                "original_connection_still_reads",
            ],
        );
    }
}

#[test]
fn s01_cached_database_open_rejects_wrong_encryption_key() {
    let (env, _database, owner) = authenticated_mvcc("s01-open");
    owner
        .execute("INSERT INTO secrets VALUES (0, 'protected value')")
        .unwrap();

    let opened = Database::open(
        env.io.clone(),
        &env.path,
        env.encrypted(CipherMode::Aegis256, KEY_B),
    );

    assert_eq!(
        assert_refused(opened, "cached Database::open with the wrong key"),
        Category::CredentialWrong
    );
}

#[test]
fn s01_cached_database_open_async_rejects_wrong_encryption_key() {
    let (env, _database, owner) = authenticated_mvcc("s01-open-async");
    owner
        .execute("INSERT INTO secrets VALUES (0, 'protected value')")
        .unwrap();

    let opened = open_async_to_completion(&env, env.encrypted(CipherMode::Aegis256, KEY_B));

    assert_eq!(
        assert_refused(opened, "cached Database::open_async with the wrong key"),
        Category::CredentialWrong
    );
}

#[cfg(unix)]
#[test]
fn s01_filesystem_aliases_cannot_bypass_cached_database_credentials() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("main.db");
    let symlink_path = directory.path().join("symlink.db");
    let hardlink_path = directory.path().join("hardlink.db");
    let uri = |path: &Path, key: Option<u8>| match key {
        Some(key) => format!(
            "file:{}?cipher=aegis256&hexkey={}",
            path.display(),
            encryption_options(CipherMode::Aegis256, key).hexkey
        ),
        None => format!("file:{}", path.display()),
    };
    let options = || DatabaseOpts::new().with_encryption(true);
    let (_io, owner) =
        Connection::from_uri(&uri(&path, Some(KEY_A)), options(), Arc::new(SqliteDialect)).unwrap();
    populate(&owner, "protected value");
    owner.execute("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    symlink(&path, &symlink_path).unwrap();
    std::fs::hard_link(&path, &hardlink_path).unwrap();

    for (alias, label) in [(&symlink_path, "symlink"), (&hardlink_path, "hard link")] {
        assert_refused(
            Connection::from_uri(&uri(alias, None), options(), Arc::new(SqliteDialect))
                .map(|(_, connection)| connection),
            &format!("{label} without a key"),
        );
        assert_refused(
            Connection::from_uri(&uri(alias, Some(KEY_B)), options(), Arc::new(SqliteDialect))
                .map(|(_, connection)| connection),
            &format!("{label} with the wrong key"),
        );
        let (_io, peer) =
            Connection::from_uri(&uri(alias, Some(KEY_A)), options(), Arc::new(SqliteDialect))
                .unwrap();
        assert_eq!(marker(&peer), ["protected value"]);
    }
    assert_eq!(marker(&owner), ["protected value"]);
}

#[test]
fn s11_ciphertext_corruption_is_refused_without_poisoning_a_later_open() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("corrupt.db");
    let uri = |key| {
        format!(
            "file:{}?cipher=aegis256&hexkey={}",
            path.display(),
            encryption_options(CipherMode::Aegis256, key).hexkey
        )
    };
    let options = || DatabaseOpts::new().with_encryption(true);
    let (io, owner) =
        Connection::from_uri(&uri(KEY_A), options(), Arc::new(SqliteDialect)).unwrap();
    populate(&owner, "protected value");
    owner.execute("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    for completion in owner.cacheflush().unwrap() {
        io.wait_for_completion(completion).unwrap();
    }
    owner.close().unwrap();
    drop(owner);
    drop(io);

    let original = std::fs::read(&path).unwrap();
    assert!(original.len() > 200);
    let mut corrupted = original.clone();
    corrupted[200] ^= 0x80;
    std::fs::write(&path, &corrupted).unwrap();

    match Connection::from_uri(&uri(KEY_A), options(), Arc::new(SqliteDialect)) {
        Ok((_io, connection)) => {
            assert!(
                first_column(&connection, "SELECT value FROM secrets").is_err(),
                "corrupted encrypted page returned protected data"
            );
            connection.close().unwrap();
        }
        Err(error) => assert!(
            matches!(
                classify(&error),
                Category::CredentialWrong | Category::CredentialMissing
            ) || matches!(error, LimboError::NotADB | LimboError::Corrupt(_)),
            "corrupted encrypted page failed with {error}"
        ),
    }

    std::fs::write(&path, &original).unwrap();
    let (_io, recovered) =
        Connection::from_uri(&uri(KEY_A), options(), Arc::new(SqliteDialect)).unwrap();
    assert_eq!(marker(&recovered), ["protected value"]);
    assert_refused(
        Connection::from_uri(&uri(KEY_B), options(), Arc::new(SqliteDialect))
            .map(|(_, connection)| connection),
        "wrong key after restoring valid ciphertext",
    );
}

#[test]
fn s08_database_opened_before_encryption_cannot_access_encrypted_mv_store() {
    let (env, database, owner) = authenticated_mvcc(
        "s08_database_opened_before_encryption_cannot_access_encrypted_mv_store",
    );
    owner
        .execute("INSERT INTO secrets VALUES (0, 'protected value')")
        .unwrap();
    assert_refused(
        connect(&database, CipherMode::Aegis256, None),
        "missing-key connection capability",
    );
    assert_refused(
        connect(&database, CipherMode::Aegis256, Some(KEY_B)),
        "wrong-key connection capability",
    );
    assert_refused(
        Database::open(env.io.clone(), &env.path, env.base()),
        "missing-key database capability",
    );
    assert_refused(
        Database::open(
            env.io.clone(),
            &env.path,
            env.encrypted(CipherMode::Aegis256, KEY_B),
        ),
        "wrong-key database capability",
    );
    assert!(database_mv_store_contains(&database, b"protected value"));
    assert_eq!(marker(&owner), ["protected value"]);
}

#[test]
fn s08_existing_keyless_connection_cannot_access_encrypted_mv_store() {
    let (env, database, owner) =
        authenticated_mvcc("s08_existing_keyless_connection_cannot_access_encrypted_mv_store");
    owner
        .execute("INSERT INTO secrets VALUES (0, 'protected value')")
        .unwrap();
    assert_refused(
        connect(&database, CipherMode::Aegis256, None),
        "missing-key connection capability",
    );
    assert_refused(
        connect(&database, CipherMode::Aegis256, Some(KEY_B)),
        "wrong-key connection capability",
    );
    assert_refused(
        Database::open(env.io.clone(), &env.path, env.base()),
        "missing-key database capability",
    );
    assert_refused(
        Database::open(
            env.io.clone(),
            &env.path,
            env.encrypted(CipherMode::Aegis256, KEY_B),
        ),
        "wrong-key database capability",
    );
    assert!(database_mv_store_contains(&database, b"protected value"));
    assert_eq!(marker(&owner), ["protected value"]);
}

#[test]
fn s08_existing_wrong_key_connection_cannot_access_encrypted_mv_store() {
    let (env, database, owner) =
        authenticated_mvcc("s08_existing_wrong_key_connection_cannot_access_encrypted_mv_store");
    owner
        .execute("INSERT INTO secrets VALUES (0, 'protected value')")
        .unwrap();
    assert_refused(
        connect(&database, CipherMode::Aegis256, None),
        "missing-key connection capability",
    );
    assert_refused(
        connect(&database, CipherMode::Aegis256, Some(KEY_B)),
        "wrong-key connection capability",
    );
    assert_refused(
        Database::open(env.io.clone(), &env.path, env.base()),
        "missing-key database capability",
    );
    assert_refused(
        Database::open(
            env.io.clone(),
            &env.path,
            env.encrypted(CipherMode::Aegis256, KEY_B),
        ),
        "wrong-key database capability",
    );
    assert!(database_mv_store_contains(&database, b"protected value"));
    assert_eq!(marker(&owner), ["protected value"]);
}

#[test]
fn s08_existing_keyless_connection_cannot_corrupt_encrypted_mvcc_database() {
    let (env, database, owner) = authenticated_mvcc(
        "s08_existing_keyless_connection_cannot_corrupt_encrypted_mvcc_database",
    );
    owner
        .execute("INSERT INTO secrets VALUES (0, 'protected value')")
        .unwrap();
    assert_refused(
        connect(&database, CipherMode::Aegis256, None),
        "missing-key connection capability",
    );
    assert_refused(
        connect(&database, CipherMode::Aegis256, Some(KEY_B)),
        "wrong-key connection capability",
    );
    assert_refused(
        Database::open(env.io.clone(), &env.path, env.base()),
        "missing-key database capability",
    );
    assert_refused(
        Database::open(
            env.io.clone(),
            &env.path,
            env.encrypted(CipherMode::Aegis256, KEY_B),
        ),
        "wrong-key database capability",
    );
    assert!(database_mv_store_contains(&database, b"protected value"));
    assert_eq!(marker(&owner), ["protected value"]);
    owner.execute("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    drop((owner, database));
    let reopened = env.open(env.encrypted(CipherMode::Aegis256, KEY_A));
    assert_eq!(
        marker(&connect(&reopened, CipherMode::Aegis256, Some(KEY_A)).unwrap()),
        ["protected value"]
    );
}

#[test]
fn s08_existing_wrong_key_connection_cannot_corrupt_encrypted_mvcc_database() {
    let (env, database, owner) = authenticated_mvcc(
        "s08_existing_wrong_key_connection_cannot_corrupt_encrypted_mvcc_database",
    );
    owner
        .execute("INSERT INTO secrets VALUES (0, 'protected value')")
        .unwrap();
    assert_refused(
        connect(&database, CipherMode::Aegis256, None),
        "missing-key connection capability",
    );
    assert_refused(
        connect(&database, CipherMode::Aegis256, Some(KEY_B)),
        "wrong-key connection capability",
    );
    assert_refused(
        Database::open(env.io.clone(), &env.path, env.base()),
        "missing-key database capability",
    );
    assert_refused(
        Database::open(
            env.io.clone(),
            &env.path,
            env.encrypted(CipherMode::Aegis256, KEY_B),
        ),
        "wrong-key database capability",
    );
    assert!(database_mv_store_contains(&database, b"protected value"));
    assert_eq!(marker(&owner), ["protected value"]);
    owner.execute("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    drop((owner, database));
    let reopened = env.open(env.encrypted(CipherMode::Aegis256, KEY_A));
    assert_eq!(
        marker(&connect(&reopened, CipherMode::Aegis256, Some(KEY_A)).unwrap()),
        ["protected value"]
    );
}

#[test]
fn s08_existing_connections_cannot_access_encrypted_statement_mv_store() {
    let (env, database, owner) =
        authenticated_mvcc("s08_existing_connections_cannot_access_encrypted_statement_mv_store");
    owner
        .execute("INSERT INTO secrets VALUES (0, 'protected value')")
        .unwrap();
    assert_refused(
        connect(&database, CipherMode::Aegis256, None),
        "missing-key connection capability",
    );
    assert_refused(
        connect(&database, CipherMode::Aegis256, Some(KEY_B)),
        "wrong-key connection capability",
    );
    assert_refused(
        Database::open(env.io.clone(), &env.path, env.base()),
        "missing-key database capability",
    );
    assert_refused(
        Database::open(
            env.io.clone(),
            &env.path,
            env.encrypted(CipherMode::Aegis256, KEY_B),
        ),
        "wrong-key database capability",
    );
    assert!(database_mv_store_contains(&database, b"protected value"));
    assert_eq!(marker(&owner), ["protected value"]);
    let statement = owner.prepare("SELECT 1").unwrap();
    assert!(statement_mv_store_contains(&statement, b"protected value"));
}

#[test]
fn s08_existing_connections_cannot_access_or_change_encrypted_schema() {
    let (env, database, owner) =
        authenticated_mvcc("s08_existing_connections_cannot_access_or_change_encrypted_schema");
    owner
        .execute("INSERT INTO secrets VALUES (0, 'protected value')")
        .unwrap();
    assert_refused(
        connect(&database, CipherMode::Aegis256, None),
        "missing-key connection capability",
    );
    assert_refused(
        connect(&database, CipherMode::Aegis256, Some(KEY_B)),
        "wrong-key connection capability",
    );
    assert_refused(
        Database::open(env.io.clone(), &env.path, env.base()),
        "missing-key database capability",
    );
    assert_refused(
        Database::open(
            env.io.clone(),
            &env.path,
            env.encrypted(CipherMode::Aegis256, KEY_B),
        ),
        "wrong-key database capability",
    );
    assert!(database_mv_store_contains(&database, b"protected value"));
    assert_eq!(marker(&owner), ["protected value"]);
}

#[test]
fn s08_existing_connections_cannot_read_decrypted_pages_directly() {
    let (env, database, owner) =
        authenticated_mvcc("s08_existing_connections_cannot_read_decrypted_pages_directly");
    owner
        .execute("INSERT INTO secrets VALUES (0, 'protected value')")
        .unwrap();
    assert_refused(
        connect(&database, CipherMode::Aegis256, None),
        "missing-key connection capability",
    );
    assert_refused(
        connect(&database, CipherMode::Aegis256, Some(KEY_B)),
        "wrong-key connection capability",
    );
    assert_refused(
        Database::open(env.io.clone(), &env.path, env.base()),
        "missing-key database capability",
    );
    assert_refused(
        Database::open(
            env.io.clone(),
            &env.path,
            env.encrypted(CipherMode::Aegis256, KEY_B),
        ),
        "wrong-key database capability",
    );
    assert!(database_mv_store_contains(&database, b"protected value"));
    assert_eq!(marker(&owner), ["protected value"]);
}

#[test]
fn s08_existing_connections_cannot_start_raw_wal_writes() {
    let (env, database, owner) = authenticated_mvcc("s08-raw");
    let before = env.writes_to(&env.path);
    assert!(owner.wal_insert_begin().is_err());
    assert!(owner.preflight_external_restore().is_err());
    assert!(database.preflight_external_restore().is_err());
    assert_eq!(env.writes_to(&env.path), before);
    assert!(!owner.get_pager().holds_read_lock());
}

#[derive(Clone, Copy, Debug)]
enum RestoreHook {
    PublishSchema,
    ReloadWal,
}

#[test]
fn s08_keyless_connection_cannot_publish_schema_after_external_restore() {
    assert_restore_hook_is_refused(false, RestoreHook::PublishSchema);
}

#[test]
fn s08_wrong_key_connection_cannot_publish_schema_after_external_restore() {
    assert_restore_hook_is_refused(true, RestoreHook::PublishSchema);
}

#[test]
fn s08_keyless_connection_cannot_reload_wal_after_external_restore() {
    assert_restore_hook_is_refused(false, RestoreHook::ReloadWal);
}

#[test]
fn s08_wrong_key_connection_cannot_reload_wal_after_external_restore() {
    assert_restore_hook_is_refused(true, RestoreHook::ReloadWal);
}

fn assert_restore_hook_is_refused(use_wrong_key: bool, hook: RestoreHook) {
    let (env, database, owner) = authenticated_mvcc("s08-restore");
    owner
        .execute("INSERT INTO secrets VALUES (0, 'protected value')")
        .unwrap();
    let before = env.writes_to(&env.path);
    assert!(owner.preflight_external_restore().is_err());
    assert!(database.preflight_external_restore().is_err());
    assert_refused(
        connect(
            &database,
            CipherMode::Aegis256,
            if use_wrong_key { Some(KEY_B) } else { None },
        ),
        "restore connection",
    );
    assert_eq!(env.writes_to(&env.path), before, "{hook:?}");
    assert_eq!(marker(&owner), ["protected value"]);
}

#[test]
fn s08_savepoint_rollback_hides_uncommitted_mvcc_data_from_old_connections() {
    let (_env, database, owner) = authenticated_mvcc("s08-savepoint");
    let peer = connect(&database, CipherMode::Aegis256, Some(KEY_A)).unwrap();
    owner.execute("BEGIN").unwrap();
    owner
        .execute("INSERT INTO secrets VALUES (0, 'protected value')")
        .unwrap();
    owner.execute("SAVEPOINT sp").unwrap();
    owner
        .execute("INSERT INTO secrets VALUES (1, 'temporary value')")
        .unwrap();
    owner.execute("ROLLBACK TO sp").unwrap();
    owner.execute("RELEASE sp").unwrap();
    assert!(marker(&peer).is_empty());
    assert_refused(
        connect(&database, CipherMode::Aegis256, Some(KEY_B)),
        "wrong key during savepoint",
    );
    owner.execute("COMMIT").unwrap();
    assert_eq!(marker(&peer), ["protected value"]);
}

fn authenticated_mvcc(name: &str) -> (Env, Arc<Database>, Arc<Connection>) {
    let env = Env::new(name);
    let database = env.open(env.encrypted(CipherMode::Aegis256, KEY_A));
    let owner = connect(&database, CipherMode::Aegis256, Some(KEY_A)).unwrap();
    owner.execute("PRAGMA journal_mode = 'mvcc'").unwrap();
    owner
        .execute("CREATE TABLE secrets(id INTEGER PRIMARY KEY, value TEXT)")
        .unwrap();
    (env, database, owner)
}

fn open_async_to_completion(env: &Env, options: OpenOptions) -> turso_core::Result<Arc<Database>> {
    let file = env
        .io
        .open_file(&env.path, OpenFlags::Create, false)
        .unwrap();
    let options = options.storage(Arc::new(turso_core::storage::database::DatabaseFile::new(
        file,
    )));
    let mut state = OpenDbAsyncState::new();
    loop {
        match Database::open_async(&mut state, env.io.clone(), &env.path, &options)
            .map_err(|error| *error)?
        {
            IOResult::Done(database) => return Ok(database),
            IOResult::IO(pending) => pending.wait(env.io.as_ref())?,
        }
    }
}

#[test]
fn s02_wrong_key_before_the_first_database_write() {
    for cipher in [CipherMode::Aes128Gcm, CipherMode::Aegis256X2] {
        let mut coverage = Coverage::default();
        let env = Env::new("s02");
        let db = env.open(env.encrypted(cipher, KEY_A));
        assert_eq!(
            assert_refused(connect(&db, cipher, None), "missing key on a new database"),
            Category::CredentialMissing
        );
        assert_eq!(
            assert_refused(
                connect(&db, cipher, Some(KEY_B)),
                "wrong key on a new database"
            ),
            Category::CredentialWrong
        );
        assert_eq!(
            env.size(&env.path),
            0,
            "refusals must not write the database"
        );
        assert_eq!(
            env.writes_to(&env.path),
            0,
            "refusals must not submit writes"
        );
        coverage.see("refused_before_any_write");
        let first = connect(&db, cipher, Some(KEY_A)).unwrap();
        let second = connect(&db, cipher, Some(KEY_A)).unwrap();
        assert_eq!(env.size(&env.path), 0);
        coverage.see("right_key_authenticated_against_bootstrap_page");
        populate(&first, "protected value");
        assert_eq!(marker(&second), ["protected value"]);
        assert!(env.size(&env.path) > 0);
        coverage.see("peer_reads_after_first_write");
        assert_refused(
            connect(&db, cipher, Some(KEY_B)),
            "wrong key after first write",
        );
        coverage.require(
            "S02",
            &[
                "refused_before_any_write",
                "right_key_authenticated_against_bootstrap_page",
                "peer_reads_after_first_write",
            ],
        );
    }
}

/// Sets up a database whose only connection has executed `first`, and returns
/// the database and that connection.

#[test]
fn s03_preparation_explain_drop_and_reset() {
    let env = Env::new("s03");
    for encrypted in [false, true] {
        let env = Env::new(if encrypted {
            "s03-encrypted"
        } else {
            "s03-plain"
        });
        let db = env.open(if encrypted {
            env.encrypted(CipherMode::Aegis256, KEY_A)
        } else {
            env.base()
        });
        let connection = if encrypted {
            connect(&db, CipherMode::Aegis256, Some(KEY_A)).unwrap()
        } else {
            db.connect(ConnectOpts::new()).unwrap()
        };
        for sql in [
            format!("PRAGMA cipher = '{}'", CipherMode::Aegis256),
            format!(
                "PRAGMA hexkey = '{}'",
                encryption_options(CipherMode::Aegis256, KEY_A).hexkey
            ),
        ] {
            assert!(connection.prepare(&sql).is_err());
            assert!(connection.prepare(format!("EXPLAIN {sql}")).is_err());
            assert!(connection.execute(sql).is_err());
        }
        assert_eq!(env.size(&env.path), 0);
        assert_eq!(env.writes_to(&env.path), 0);
    }
    assert_eq!(env.size(&env.path), 0);
}

#[test]
fn s05_independently_encrypted_attach_target() {
    let cipher = CipherMode::Aegis256;
    for mvcc in [false, true] {
        for cached in [false, true] {
            let mut coverage = Coverage::default();
            let env = Env::new("s05");
            let db = env.open(env.encrypted(cipher, KEY_A));
            let primary = connect(&db, cipher, Some(KEY_A)).unwrap();
            if mvcc {
                primary.execute("PRAGMA journal_mode = 'mvcc'").unwrap();
            }
            populate(&primary, "main value");
            let target_path = format!("{}-target", env.path);
            let target =
                Database::open(env.io.clone(), &target_path, env.encrypted(cipher, KEY_B)).unwrap();
            let target_connection = connect(&target, cipher, Some(KEY_B)).unwrap();
            if mvcc {
                target_connection
                    .execute("PRAGMA journal_mode = 'mvcc'")
                    .unwrap();
            }
            populate(&target_connection, "target value");
            let mut kept = Some((target, target_connection));
            if !cached {
                let (target, connection) = kept.take().unwrap();
                connection.close().unwrap();
                let weak = Arc::downgrade(&target);
                drop(target);
                drop(connection);
                assert_eq!(
                    weak.strong_count(),
                    0,
                    "the target must be closed for the cold case"
                );
                coverage.see("target_cold");
            } else {
                coverage.see("target_cached");
            }
            let uri = |byte: Option<u8>| match byte {
                Some(byte) => format!(
                    "file:{target_path}?cipher={cipher}&hexkey={}",
                    encryption_options(cipher, byte).hexkey
                ),
                None => target_path.clone(),
            };
            let aliases = |connection: &Arc<Connection>| -> Vec<String> {
                let rows: Vec<(i64, String, String)> = connection.exec_rows("PRAGMA database_list");
                rows.into_iter().map(|row| row.1).collect()
            };
            for (byte, label) in [
                (None, "without a key"),
                (Some(KEY_A), "with the primary key"),
            ] {
                assert_refused(
                    primary.execute(format!("ATTACH '{}' AS aux", uri(byte))),
                    &format!("ATTACH {label}"),
                );
                assert!(!aliases(&primary).contains(&"aux".to_string()));
                assert_eq!(marker(&primary), ["main value"]);
                coverage.see(format!("attach_refused {label}"));
            }
            primary
                .execute(format!("ATTACH '{}' AS aux", uri(Some(KEY_B))))
                .unwrap();
            assert_eq!(
                first_column(&primary, "SELECT value FROM aux.secrets").unwrap(),
                ["target value"]
            );
            primary.execute("DETACH aux").unwrap();
            coverage.see("attach_with_target_key_reads_target");
            assert_refused(
                primary.execute(format!("ATTACH '{}' AS aux", uri(None))),
                "ATTACH without a key after a valid attach",
            );
            coverage.see("attach_refused_again");
            drop(kept);
            coverage.require(
                &format!("S05 mvcc={mvcc} cached={cached}"),
                &[
                    if cached {
                        "target_cached"
                    } else {
                        "target_cold"
                    },
                    "attach_refused without a key",
                    "attach_refused with the primary key",
                    "attach_with_target_key_reads_target",
                    "attach_refused_again",
                ],
            );
        }
    }
}

#[test]
fn s05_two_attached_databases_keep_credentials_and_aliases_separate() {
    let cipher = CipherMode::Aegis256;
    let env = Env::new("s05-two-targets");
    let primary_db = env.open(env.encrypted(cipher, KEY_A));
    let primary = connect(&primary_db, cipher, Some(KEY_A)).unwrap();
    populate(&primary, "main value");

    let target_a_path = format!("{}-target-a", env.path);
    let target_b_path = format!("{}-target-b", env.path);
    let target_a =
        Database::open(env.io.clone(), &target_a_path, env.encrypted(cipher, KEY_B)).unwrap();
    let target_b =
        Database::open(env.io.clone(), &target_b_path, env.encrypted(cipher, KEY_C)).unwrap();
    let target_a_connection = connect(&target_a, cipher, Some(KEY_B)).unwrap();
    let target_b_connection = connect(&target_b, cipher, Some(KEY_C)).unwrap();
    populate(&target_a_connection, "target a value");
    populate(&target_b_connection, "target b value");

    let uri = |path: &str, key| {
        format!(
            "file:{path}?cipher={cipher}&hexkey={}",
            encryption_options(cipher, key).hexkey
        )
    };
    assert_refused(
        primary.execute(format!(
            "ATTACH '{}' AS target_a",
            uri(&target_a_path, KEY_C)
        )),
        "first target with the second target's key",
    );
    let aliases: Vec<(i64, String, String)> = primary.exec_rows("PRAGMA database_list");
    assert!(!aliases.iter().any(|alias| alias.1 == "target_a"));

    primary
        .execute(format!(
            "ATTACH '{}' AS target_a",
            uri(&target_a_path, KEY_B)
        ))
        .unwrap();
    primary
        .execute(format!(
            "ATTACH '{}' AS target_b",
            uri(&target_b_path, KEY_C)
        ))
        .unwrap();
    assert_eq!(
        first_column(&primary, "SELECT value FROM target_a.secrets").unwrap(),
        ["target a value"]
    );
    assert_eq!(
        first_column(&primary, "SELECT value FROM target_b.secrets").unwrap(),
        ["target b value"]
    );
    assert_eq!(marker(&primary), ["main value"]);

    primary.execute("DETACH target_a").unwrap();
    assert_eq!(
        first_column(&primary, "SELECT value FROM target_b.secrets").unwrap(),
        ["target b value"]
    );
    primary.execute("DETACH target_b").unwrap();
    assert_eq!(marker(&primary), ["main value"]);
}

/// Polls whichever actor can run, completes the oldest request otherwise,
/// and never drops work or injects failures. Used for the page-one and
/// registry recipes so both actors are invoked while the other waits.
struct PollFirst;

impl Chooser for PollFirst {
    fn choose(&mut self, enabled: &[Action], decided: &[Action]) -> Action {
        let last = decided.iter().rev().find_map(|action| match action {
            Action::Poll(actor) => Some(*actor),
            _ => None,
        });
        enabled
            .iter()
            .find(|action| matches!(action, Action::Poll(actor) if Some(*actor) != last))
            .or_else(|| {
                enabled
                    .iter()
                    .find(|action| matches!(action, Action::Complete(_)))
            })
            .or_else(|| {
                enabled
                    .iter()
                    .find(|action| matches!(action, Action::Poll(_)))
            })
            .cloned()
            .expect("an enabled poll or completion")
    }
}

#[test]
fn s06_exactly_one_initial_allocation() {
    for cipher in CIPHERS {
        for page_size in PAGE_SIZES {
            let mut trace = Trace {
                allow_failures: false,
                chooser: Some(Box::new(PollFirst)),
                ..Trace::default()
            };
            page_one_schedule(&mut trace, cipher, page_size, Credential::Correct);
            let observations: Vec<&String> = trace.outcomes.iter().flatten().collect();
            assert!(
                observations
                    .iter()
                    .any(|o| o.contains("polled while the other actor waited on I/O")),
                "S06 {cipher:?} page size {page_size}: the second writer never ran while the first waited: {observations:?}"
            );
            assert!(
                observations
                    .iter()
                    .any(|o| o.as_str() == "initial page-one writes: 1"),
                "S06 {cipher:?} page size {page_size}: exactly one initial page-one write was not observed"
            );
        }
    }
}

#[test]
fn s07_registry_absent_opening_and_ready_windows() {
    for wait_for_bytes in [false, true] {
        for credential in [Credential::Missing, Credential::Wrong, Credential::Correct] {
            for cancel_waiter in [false, true] {
                let mut trace = Trace {
                    allow_failures: false,
                    chooser: Some(Box::new(PollFirst)),
                    ..Trace::default()
                };
                manager_schedule(
                    &mut trace,
                    false,
                    true,
                    credential,
                    cancel_waiter,
                    wait_for_bytes,
                );
                let observations: Vec<&String> = trace.outcomes.iter().flatten().collect();
                let window = if wait_for_bytes {
                    observations.iter().any(|o| {
                        o.starts_with("creator paused, database bytes=") && !o.ends_with("=0")
                    })
                } else {
                    observations
                        .iter()
                        .any(|o| o.as_str() == "creator paused, database bytes=0")
                };
                assert!(
                    window,
                    "S07 did not pause the creator in the intended window"
                );
                assert!(
                    observations
                        .iter()
                        .any(|o| o.as_str() == "waiter yielded on the registry owner"),
                    "S07 waiter never waited on the registry owner"
                );
            }
        }
    }
    for wait_for_bytes in [false, true] {
        for credential in [Credential::Missing, Credential::Wrong, Credential::Correct] {
            let mut trace = Trace {
                allow_failures: false,
                prefix: Vec::new(),
                chooser: Some(Box::new(DropCreatorThenPoll { dropped: false })),
                ..Trace::default()
            };
            manager_schedule(&mut trace, false, true, credential, false, wait_for_bytes);
            assert!(trace.actions.contains(&Action::Drop(0)));
        }
    }
}

struct DropCreatorThenPoll {
    dropped: bool,
}

impl Chooser for DropCreatorThenPoll {
    fn choose(&mut self, enabled: &[Action], decided: &[Action]) -> Action {
        if !self.dropped && enabled.contains(&Action::Drop(0)) {
            self.dropped = true;
            return Action::Drop(0);
        }
        PollFirst.choose(enabled, decided)
    }
}

#[test]
fn s08_an_earlier_connection_and_earlier_schema_statement() {
    let env = Env::new("s08-plaintext");
    let db = env.open(env.base());
    let owner = db.connect(ConnectOpts::new()).unwrap();
    let peer = db.connect(ConnectOpts::new()).unwrap();
    let mut early = peer.prepare("SELECT name FROM sqlite_schema").unwrap();
    assert!(owner.execute("PRAGMA cipher = 'aegis256'").is_err());
    assert!(owner
        .execute(format!(
            "PRAGMA hexkey = '{}'",
            encryption_options(CipherMode::Aegis256, KEY_A).hexkey
        ))
        .is_err());
    assert!(Database::open(
        env.io.clone(),
        &env.path,
        env.encrypted(CipherMode::Aegis256, KEY_A)
    )
    .is_err());
    populate(&owner, "public value");
    early.run_ignore_rows().unwrap();
    assert_eq!(peer.get_encryption_cipher_mode(), None);
    assert_eq!(marker(&peer), ["public value"]);
}

#[test]
fn s11_failed_page_one_authentication_read_and_retry() {
    let cipher = CipherMode::Aegis256;
    let env = Env::new("s11");
    let db = env.open(env.encrypted(cipher, KEY_A));
    let owner = connect(&db, cipher, Some(KEY_A)).unwrap();
    populate(&owner, "protected value");
    for (byte, valid) in [(KEY_A, true), (KEY_B, false)] {
        env.io.fail_next_request(QueuedIoOpKind::Pread);
        let error = connect(&db, cipher, Some(byte))
            .err()
            .expect("injected read must fail");
        assert_eq!(classify(&error), Category::InjectedIo);
        let retry = connect(&db, cipher, Some(byte));
        if valid {
            assert_eq!(marker(&retry.unwrap()), ["protected value"]);
        } else {
            assert_refused(retry, "wrong key after I/O failure");
        }
    }
    owner.execute("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    assert_eq!(env.size(&format!("{}-wal", env.path)), 0);
}

#[test]
fn s12_page_size_and_first_write_retries() {
    let cipher = CipherMode::Aes128Gcm;
    for failed in [QueuedIoOpKind::Pwrite, QueuedIoOpKind::Sync] {
        let env = Env::new("s12");
        let db = env.open(
            env.encrypted(cipher, KEY_A)
                .page_size(turso_core::storage::sqlite3_ondisk::PageSize::new(8192).unwrap()),
        );
        let writer = connect(&db, cipher, Some(KEY_A)).unwrap();
        let peer = connect(&db, cipher, Some(KEY_A)).unwrap();
        assert_refused(
            connect(&db, cipher, Some(KEY_B)),
            "wrong key before first write",
        );
        assert!(writer.execute("PRAGMA page_size = 4096").is_err());
        env.io.fail_next_request(failed);
        assert!(writer
            .execute("CREATE TABLE secrets(id INTEGER PRIMARY KEY, value TEXT)")
            .is_err());
        populate(&writer, "protected value");
        assert_eq!(marker(&peer), ["protected value"]);
        assert_eq!(writer.get_page_size().get(), 8192);
        drop((writer, peer, db));
        let reopened = env.open(env.encrypted(cipher, KEY_A));
        let peer = connect(&reopened, cipher, Some(KEY_A)).unwrap();
        assert_eq!(peer.get_page_size().get(), 8192);
        assert_eq!(marker(&peer), ["protected value"]);
    }
}

#[test]
fn s13_abandoned_encrypted_mvcc_bootstrap() {
    let cipher = CipherMode::Aegis256;
    let mut phases = BTreeSet::new();
    for stop_after in 0.. {
        let env = Env::new("s13");
        let file = env
            .io
            .open_file(&env.path, OpenFlags::Create, false)
            .unwrap();
        let storage = || {
            Arc::new(turso_core::storage::database::DatabaseFile::new(
                file.clone(),
            ))
        };
        let options = |encryption: Option<EncryptionOpts>| {
            OpenOptions::new(Arc::new(SqliteDialect))
                .storage(storage())
                .db_opts(DatabaseOpts::new().with_encryption(true).with_mvcc(true))
                .encryption(encryption)
        };
        let creator_options = options(Some(encryption_options(cipher, KEY_A)));
        let mut state = OpenDbAsyncState::new();
        let mut finished = false;
        for step in 0..=stop_after {
            match Database::open_async(&mut state, env.io.clone(), &env.path, &creator_options)
                .unwrap()
            {
                IOResult::Done(_) => {
                    finished = true;
                    break;
                }
                IOResult::IO(pending) if step < stop_after => {
                    pending.wait(env.io.as_ref()).unwrap()
                }
                IOResult::IO(_) => {}
            }
        }
        // A waiter with the wrong key queues behind the paused creator.
        let mut waiter = OpenDbAsyncState::new();
        let waiter_options = options(Some(encryption_options(cipher, KEY_B)));
        let waited = !finished
            && matches!(
                Database::open_async(&mut waiter, env.io.clone(), &env.path, &waiter_options),
                Ok(IOResult::IO(pending)) if pending.is_explicit_yield()
            );
        let paused_page_one = env.size(&env.path) > 0;
        let paused_log = env.size(&format!("{}-log", env.path)) > 0;
        phases.insert(match (finished, paused_page_one, paused_log) {
            (true, ..) => "finished",
            (false, false, _) => "before page one",
            (false, true, false) => "after page one",
            (false, true, true) => "after the MVCC log header",
        });
        drop(state);
        // Requests the creator already submitted still run after it is
        // dropped, so page one may land now.
        env.io.drain_all().unwrap();
        let page_one_written = env.size(&env.path) > 0;
        if waited {
            // The creator is gone. With page one on disk the waiter's key is
            // refused; without it the waiter would create the database itself.
            let mut result = None;
            for _ in 0..1000 {
                match Database::open_async(&mut waiter, env.io.clone(), &env.path, &waiter_options)
                {
                    Ok(IOResult::Done(db)) => result = Some(Ok(db)),
                    Ok(IOResult::IO(pending)) => match pending.wait(env.io.as_ref()) {
                        Ok(()) => continue,
                        Err(error) => result = Some(Err(error)),
                    },
                    Err(error) => result = Some(Err(*error)),
                }
                break;
            }
            let result = result.expect("the waiter finished after the creator left");
            if page_one_written {
                assert_refused(
                    result,
                    "a waiting wrong key after the creator wrote page one",
                );
            }
        }
        drop(waiter);
        env.io.drain_all().unwrap();
        let surviving = if page_one_written || finished {
            KEY_A
        } else {
            // Nothing was established; whoever opens next decides the key.
            KEY_B
        };
        let surviving = if !waited { KEY_A } else { surviving };
        let db = Database::open(
            env.io.clone(),
            &env.path,
            options(Some(encryption_options(cipher, surviving))),
        )
        .unwrap_or_else(|error| panic!("reopening after yield {stop_after}: {error}"));
        let connection = connect(&db, cipher, Some(surviving)).unwrap();
        connection
            .execute("CREATE TABLE IF NOT EXISTS secrets(id INTEGER PRIMARY KEY, value TEXT)")
            .unwrap();
        connection
            .execute("INSERT OR REPLACE INTO secrets VALUES (0, 'protected value')")
            .unwrap();
        assert_eq!(marker(&connection), ["protected value"]);
        let integrity = first_column(&connection, "PRAGMA integrity_check").unwrap();
        assert_eq!(integrity, ["ok"]);
        if finished {
            break;
        }
    }
    for phase in ["before page one", "after page one", "finished"] {
        assert!(
            phases.contains(phase),
            "S13 never abandoned the bootstrap {phase}: {phases:?}"
        );
    }
}

#[test]
fn s14_uncached_validation_from_wal_and_main_file() {
    let mut coverage = Coverage::default();
    let cipher = CipherMode::Aegis256;
    let page_size = 4096;
    for source in ["wal", "main"] {
        let env = Env::new("s14");
        let db = env.open(env.encrypted(cipher, KEY_A));
        let owner = connect(&db, cipher, Some(KEY_A)).unwrap();
        populate(&owner, "protected value");
        let wal = format!("{}-wal", env.path);
        if source == "main" {
            owner.execute("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
            assert_eq!(env.size(&wal), 0, "page one must only be in the main file");
        } else {
            assert!(env.size(&wal) > 0, "page one must have a newer WAL version");
        }
        let path = if source == "wal" {
            wal.clone()
        } else {
            env.path.clone()
        };
        let full_page_one_reads = |owner_name: &str| -> usize {
            env.io
                .records()
                .iter()
                .filter(|record| {
                    record.event.id.owner == owner_name
                        && record.event.path == path
                        && record.event.kind == QueuedIoOpKind::Pread
                        && record.event.len == page_size
                        && if source == "wal" {
                            // Frame data follows the 32-byte WAL header and a
                            // 24-byte frame header.
                            (record.event.offset as usize)
                                .checked_sub(32 + 24)
                                .is_some_and(|offset| offset % (page_size + 24) == 0)
                        } else {
                            record.event.offset == 0
                        }
                })
                .count()
        };
        env.io.set_owner(format!("s14.{source}.right"));
        let peer = connect(&db, cipher, Some(KEY_A)).unwrap();
        env.io.set_owner(format!("s14.{source}.wrong"));
        // A header-only read could not tell the keys apart: the refusal is an
        // authentication failure of the full page.
        assert_eq!(
            assert_refused(connect(&db, cipher, Some(KEY_B)), "wrong key"),
            Category::CredentialWrong
        );
        assert!(full_page_one_reads(&format!("s14.{source}.right")) > 0);
        assert!(full_page_one_reads(&format!("s14.{source}.wrong")) > 0);
        assert_eq!(marker(&peer), ["protected value"]);
        coverage.see(format!("validated_from_{source}"));
    }
    coverage.require("S14", &["validated_from_wal", "validated_from_main"]);
}

#[test]
fn s15_schema_refresh_and_transaction_entry_authorization() {
    let cipher = CipherMode::Aegis256;
    for mvcc in [false, true] {
        let env = Env::new("s15");
        let db = env.open(env.encrypted(cipher, KEY_A));
        let owner = connect(&db, cipher, Some(KEY_A)).unwrap();
        if mvcc {
            owner.execute("PRAGMA journal_mode = 'mvcc'").unwrap();
        }
        let peer = connect(&db, cipher, Some(KEY_A)).unwrap();
        assert_refused(connect(&db, cipher, None), "keyless transaction entry");
        assert_refused(
            connect(&db, cipher, Some(KEY_B)),
            "wrong-key transaction entry",
        );
        populate(&owner, "protected value");
        assert_eq!(marker(&peer), ["protected value"]);
        peer.execute("BEGIN").unwrap();
        peer.execute("INSERT INTO secrets VALUES (1, 'protected value')")
            .unwrap();
        peer.execute("COMMIT").unwrap();
        assert_eq!(marker(&owner), ["protected value", "protected value"]);
    }
}

/// Fair scheduling, except that it drops actor 1's first operation while it
/// waits on I/O and fails the first queued write once.
struct AttackThenRecover {
    dropped: bool,
    failed: bool,
}

impl Chooser for AttackThenRecover {
    fn choose(&mut self, enabled: &[Action], decided: &[Action]) -> Action {
        if !self.failed {
            if let Some(action) = enabled
                .iter()
                .find(|action| matches!(action, Action::Fail(id) if id.owner.starts_with("a0.")))
            {
                self.failed = true;
                return action.clone();
            }
        }
        if !self.dropped
            && enabled.contains(&Action::Drop(1))
            && !enabled.contains(&Action::Poll(1))
        {
            self.dropped = true;
            return Action::Drop(1);
        }
        let last = decided.iter().rev().find_map(|action| match action {
            Action::Poll(actor) => Some(*actor),
            _ => None,
        });
        enabled
            .iter()
            .find(|action| matches!(action, Action::Poll(actor) if Some(*actor) != last))
            .or_else(|| {
                enabled
                    .iter()
                    .find(|action| matches!(action, Action::Poll(_)))
            })
            .or_else(|| {
                enabled
                    .iter()
                    .find(|action| matches!(action, Action::Start(_)))
            })
            .cloned()
            .unwrap_or_else(|| fair(enabled, decided))
    }
}

#[test]
fn s16_normal_lifecycle_after_failed_attacks() {
    let spec = LifecycleSpec {
        start: Start::Earlier,
        plaintext: false,
        cipher: CipherMode::Aegis256,
        other: CipherMode::Aegis256X2,
        page_size: 4096,
        mvcc: false,
        target_cached: false,
        setup: vec![vec![Command::Set(Setting::Key(2))], Vec::new(), Vec::new()],
        scripts: vec![
            vec![
                Command::Connect(Cred::Bound),
                Command::Insert(1001),
                Command::Insert(1002),
            ],
            vec![
                Command::Connect(Cred::Bound),
                Command::Insert(2001),
                Command::Read,
            ],
            vec![
                Command::OpenCached(Cred::OtherKey),
                Command::OpenCached(Cred::WrongCipher),
                Command::Connect(Cred::Missing),
                Command::Read,
                Command::Attach(AttachCred::PrimaryKey),
                Command::OpenCached(Cred::Bound),
                Command::Attach(AttachCred::Missing),
                Command::Attach(AttachCred::TargetKey),
            ],
        ],
        sabotage: None,
    };
    let case = Case {
        build: Json::Null,
        seeds: Seeds {
            campaign: 16,
            index: 0,
            scenario: 0,
            operations: 0,
            schedule: 0,
        },
        faults: true,
        family: Family::Lifecycle(spec),
        decisions: Vec::new(),
        outcomes: Vec::new(),
        terminal: None,
    };
    take_coverage();
    let (_, outcomes, terminal) = execute(
        &case,
        Box::new(AttackThenRecover {
            dropped: false,
            failed: false,
        }),
    );
    let coverage = take_coverage();
    assert_eq!(terminal, Terminal::Pass, "{outcomes:#?}");
    for required in [
        "lifecycle.open.cached",
        "lifecycle.cold_reopen",
        "lifecycle.recovered",
        "refusal.CredentialMissing",
        "refusal.CredentialWrong",
        "refusal.CipherMismatch",
        "lifecycle.attach.TargetKey.cold",
    ] {
        assert!(
            coverage.contains_key(required),
            "S16 did not reach {required}: {coverage:?}"
        );
    }
    assert!(
        coverage.keys().any(|key| key.starts_with("fault.")),
        "S16 injected no failure: {outcomes:#?}"
    );
    assert!(
        coverage
            .keys()
            .any(|key| key.starts_with("lifecycle.cancel.")),
        "S16 cancelled nothing"
    );
}

#[test]
fn s17_negative_and_positive_boundary_controls() {
    let env = Env::new("s17");
    let plain = env.open(env.base());
    assert!(Database::open(
        env.io.clone(),
        &env.path,
        env.encrypted(CipherMode::Aegis256, KEY_A)
    )
    .is_err());
    assert!(connect(&plain, CipherMode::Aegis256, Some(KEY_A)).is_err());
    let conn = plain.connect(ConnectOpts::new()).unwrap();
    assert!(conn.set_encryption_cipher(CipherMode::Aegis256).is_err());
    assert!(conn
        .set_encryption_key(key(CipherMode::Aegis256, KEY_A))
        .is_err());
    assert!(Database::open(
        env.io.clone(),
        &unique("disabled"),
        OpenOptions::new(Arc::new(SqliteDialect))
            .encryption(encryption_options(CipherMode::Aegis256, KEY_A))
    )
    .is_err());
    populate(&conn, "public value");
    assert_eq!(marker(&conn), ["public value"]);
}

#[test]
fn s18_reads_encrypted_files_from_the_old_base_commit() {
    use turso_core::PlatformIO;

    struct Fixture {
        name: &'static str,
        cipher: CipherMode,
        page_size: i64,
        mvcc: bool,
        files: &'static [(&'static str, &'static [u8])],
    }

    let fixtures = [
        Fixture {
            name: "aegis256-wal",
            cipher: CipherMode::Aegis256,
            page_size: 4096,
            mvcc: false,
            files: &[
                (
                    "",
                    include_bytes!("../fixtures/encryption/4b3befbd6/aegis256-wal.db"),
                ),
                (
                    "-wal",
                    include_bytes!("../fixtures/encryption/4b3befbd6/aegis256-wal.db-wal"),
                ),
            ],
        },
        Fixture {
            name: "aes256-wal",
            cipher: CipherMode::Aes256Gcm,
            page_size: 8192,
            mvcc: false,
            files: &[
                (
                    "",
                    include_bytes!("../fixtures/encryption/4b3befbd6/aes256-wal.db"),
                ),
                (
                    "-wal",
                    include_bytes!("../fixtures/encryption/4b3befbd6/aes256-wal.db-wal"),
                ),
            ],
        },
        Fixture {
            name: "aegis256-mvcc",
            cipher: CipherMode::Aegis256,
            page_size: 4096,
            mvcc: true,
            files: &[
                (
                    "",
                    include_bytes!("../fixtures/encryption/4b3befbd6/aegis256-mvcc.db"),
                ),
                (
                    "-log",
                    include_bytes!("../fixtures/encryption/4b3befbd6/aegis256-mvcc.db-log"),
                ),
            ],
        },
        Fixture {
            name: "aes256-mvcc",
            cipher: CipherMode::Aes256Gcm,
            page_size: 8192,
            mvcc: true,
            files: &[
                (
                    "",
                    include_bytes!("../fixtures/encryption/4b3befbd6/aes256-mvcc.db"),
                ),
                (
                    "-log",
                    include_bytes!("../fixtures/encryption/4b3befbd6/aes256-mvcc.db-log"),
                ),
            ],
        },
    ];
    for fixture in fixtures {
        for cached in [false, true] {
            let mut coverage = Coverage::default();
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("legacy.db");
            let path = path.to_str().unwrap();
            for (suffix, bytes) in fixture.files {
                std::fs::write(format!("{path}{suffix}"), bytes).unwrap();
            }
            let io: Arc<dyn IO> = Arc::new(PlatformIO::new().unwrap());
            let base = OpenOptions::new(Arc::new(SqliteDialect))
                .db_opts(DatabaseOpts::new().with_encryption(true));
            let options = base
                .clone()
                .encryption(encryption_options(fixture.cipher, KEY_A));
            let mut kept = if cached {
                let db = Database::open(io.clone(), path, options.clone()).unwrap();
                let owner = connect(&db, fixture.cipher, Some(KEY_A)).unwrap();
                let rows: Vec<(i64, String)> =
                    owner.exec_rows("SELECT id, value FROM legacy_secrets ORDER BY id");
                assert_eq!(
                    rows,
                    [(1, "legacy-one".to_string()), (2, "legacy-two".to_string())],
                    "{}: warm the old encrypted data before invalid opens",
                    fixture.name
                );
                coverage.see("old_data_cached");
                Some((db, owner))
            } else {
                coverage.see("old_file_cold");
                None
            };
            assert_refused(
                Database::open(io.clone(), path, base.clone()),
                "old encrypted file without credentials",
            );
            coverage.see("missing_key_refused");
            assert_refused(
                Database::open(
                    io.clone(),
                    path,
                    base.clone()
                        .encryption(encryption_options(fixture.cipher, KEY_B)),
                ),
                "old encrypted file with the wrong key",
            );
            coverage.see("wrong_key_refused");
            let other = if fixture.cipher == CipherMode::Aegis256 {
                CipherMode::Aes256Gcm
            } else {
                CipherMode::Aegis256
            };
            assert_refused(
                Database::open(
                    io.clone(),
                    path,
                    base.clone().encryption(encryption_options(other, KEY_A)),
                ),
                "old encrypted file with a different cipher",
            );
            coverage.see("different_cipher_refused");
            let db = Database::open(io.clone(), path, options.clone()).unwrap();
            assert_eq!(db.get_mv_store().is_some(), fixture.mvcc);
            if let Some((cached_db, _)) = &kept {
                assert!(Arc::ptr_eq(&db, cached_db));
            }
            let connection = connect(&db, fixture.cipher, Some(KEY_A)).unwrap();
            let pages: Vec<(i64,)> = connection.exec_rows("PRAGMA page_size");
            assert_eq!(pages, [(fixture.page_size,)]);
            connection
                .execute("INSERT INTO legacy_secrets VALUES (3, 'new-three')")
                .unwrap();
            let expected = [
                (1, "legacy-one".to_string()),
                (2, "legacy-two".to_string()),
                (3, "new-three".to_string()),
            ];
            let rows: Vec<(i64, String)> =
                connection.exec_rows("SELECT id, value FROM legacy_secrets ORDER BY id");
            assert_eq!(rows, expected, "{}", fixture.name);
            let weak = Arc::downgrade(&db);
            connection.close().unwrap();
            drop(connection);
            drop(db);
            if let Some((db, owner)) = kept.take() {
                owner.close().unwrap();
                drop(owner);
                drop(db);
            }
            assert_eq!(weak.strong_count(), 0);
            let reopened = Database::open(io, path, options).unwrap();
            let reader = connect(&reopened, fixture.cipher, Some(KEY_A)).unwrap();
            let rows: Vec<(i64, String)> =
                reader.exec_rows("SELECT id, value FROM legacy_secrets ORDER BY id");
            assert_eq!(rows, expected, "{} after cold reopen", fixture.name);
            let integrity: Vec<(String,)> = reader.exec_rows("PRAGMA integrity_check");
            assert_eq!(integrity, [("ok".to_string(),)]);
            coverage.see("old_and_new_rows_survive_reopen");
            coverage.require(
                "S18",
                &[
                    if cached {
                        "old_data_cached"
                    } else {
                        "old_file_cold"
                    },
                    "missing_key_refused",
                    "wrong_key_refused",
                    "different_cipher_refused",
                    "old_and_new_rows_survive_reopen",
                ],
            );
        }
    }
}
