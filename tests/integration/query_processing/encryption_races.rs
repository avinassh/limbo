use crate::common::ExecRows;
use crate::queued_io::{QueuedIo, QueuedIoOpKind, QueuedIoRequestId};
use std::sync::Arc;
use turso_core::types::IOCompletions;
use turso_core::{
    Buffer, CipherMode, Completion, ConnectOpts, Connection, Database, DatabaseOpts, EncryptionKey,
    EncryptionOpts, IOResult, LimboError, MemoryIO, OpenDbAsyncState, OpenFlags, OpenOptions, IO,
};
use turso_sqlite::SqliteDialect;

/// A broken encryption or storage invariant. Runners raise it with `check!`
/// so the fuzzer can tell failures apart without parsing panic messages.
#[derive(Clone, Debug)]
pub(super) struct Violation {
    pub(super) invariant: &'static str,
    pub(super) message: String,
}

/// A recorded schedule asked for an action the run did not offer.
#[derive(Clone, Debug)]
pub(super) struct ReplayDivergence(pub(super) String);

macro_rules! check {
    ($condition:expr, $invariant:expr, $($message:tt)+) => {
        if !$condition {
            std::panic::panic_any(Violation {
                invariant: $invariant,
                message: format!($($message)+),
            });
        }
    };
}

#[path = "encryption_fuzz.rs"]
mod randomized;

#[test]
#[ignore = "exhaustive I/O schedule enumeration; execute explicitly"]
fn exhaustive_encrypted_page_one_aes_schedules() {
    page_one_schedules(&[CipherMode::Aes128Gcm, CipherMode::Aes256Gcm]);
}

#[test]
#[ignore = "exhaustive I/O schedule enumeration; execute explicitly"]
fn exhaustive_encrypted_page_one_aegis128_schedules() {
    page_one_schedules(&[
        CipherMode::Aegis128L,
        CipherMode::Aegis128X2,
        CipherMode::Aegis128X4,
    ]);
}

#[test]
#[ignore = "exhaustive I/O schedule enumeration; execute explicitly"]
fn exhaustive_encrypted_page_one_aegis256_schedules() {
    page_one_schedules(&[
        CipherMode::Aegis256,
        CipherMode::Aegis256X2,
        CipherMode::Aegis256X4,
    ]);
}

#[test]
#[ignore = "exhaustive I/O schedule enumeration; execute explicitly"]
fn exhaustive_encrypted_database_manager_schedules() {
    for (existing, mvcc) in [(false, true), (true, false), (true, true)] {
        for credential in [
            Credential::Missing,
            Credential::Wrong,
            Credential::WrongCipher,
            Credential::Correct,
        ] {
            let name = format!("manager: existing={existing}, mvcc={mvcc}, {credential:?}");
            for cancel_waiter in [false, true] {
                explore(&format!("{name}, cancel_waiter={cancel_waiter}"), |trace| {
                    manager_schedule(trace, existing, mvcc, credential, cancel_waiter, true)
                });
            }
        }
    }
}

#[test]
fn abandoned_encrypted_mvcc_open_preserves_metadata() {
    for stop_after in 0.. {
        let io = Arc::new(QueuedIo::new());
        let path = ":abandoned-encrypted-mvcc-open:";
        let file = io.open_file(path, OpenFlags::Create, false).unwrap();
        let options = OpenOptions::new(Arc::new(SqliteDialect))
            .storage(Arc::new(turso_core::storage::database::DatabaseFile::new(
                file,
            )))
            .db_opts(DatabaseOpts::new().with_encryption(true).with_mvcc(true))
            .encryption(encryption_options(CipherMode::Aegis256, 1));
        let mut state = OpenDbAsyncState::new();
        let mut reached_done = false;
        for step in 0..=stop_after {
            match Database::open_async(&mut state, io.clone(), path, &options).unwrap() {
                IOResult::Done(_) => {
                    reached_done = true;
                    break;
                }
                IOResult::IO(pending) if step < stop_after => pending.wait(io.as_ref()).unwrap(),
                IOResult::IO(_) => {}
            }
        }
        drop(state);
        while io.step_one().unwrap().is_some() {}
        let db = Database::open(io.clone(), path, options)
            .unwrap_or_else(|error| panic!("abandoning after yield {stop_after}: {error}"));
        let connection = db
            .connect(ConnectOpts::new().with_encryption_key(Some(key(CipherMode::Aegis256, 1))))
            .unwrap();
        connection
            .execute("CREATE TABLE secrets(value TEXT)")
            .unwrap();
        connection
            .execute("INSERT INTO secrets VALUES ('protected value')")
            .unwrap();
        let rows: Vec<(String,)> = connection.exec_rows("SELECT value FROM secrets");
        assert_eq!(rows, [("protected value".to_string(),)]);
        if reached_done {
            break;
        }
    }
}

#[test]
fn manager_transaction_cleanup_repro() {
    let mut trace = Trace::default();
    manager_schedule(&mut trace, false, true, Credential::Correct, false, true);
}

fn manager_schedule(
    trace: &mut Trace,
    existing: bool,
    mvcc: bool,
    credential: Credential,
    cancel_waiter: bool,
    wait_for_bytes: bool,
) {
    let inner = Arc::new(MemoryIO::new());
    let io = Arc::new(QueuedIo::new_with_io(inner.clone()));
    let path_name = format!(":encrypted-manager-schedules:{}", trace.namespace);
    let path = path_name.as_str();
    io.set_owner("setup");
    let raw = inner.open_file(path, OpenFlags::Create, false).unwrap();
    let file = io.open_file(path, OpenFlags::Create, false).unwrap();
    let base = OpenOptions::new(Arc::new(SqliteDialect))
        .storage(Arc::new(turso_core::storage::database::DatabaseFile::new(
            file,
        )))
        .db_opts(DatabaseOpts::new().with_encryption(true).with_mvcc(mvcc));
    let encrypted = base
        .clone()
        .encryption(encryption_options(CipherMode::Aegis256, 1));
    if existing {
        let db = Database::open(io.clone(), path, encrypted.clone()).unwrap();
        let conn = db
            .connect(ConnectOpts::new().with_encryption_key(Some(key(CipherMode::Aegis256, 1))))
            .unwrap();
        conn.execute("CREATE TABLE secrets(value TEXT)").unwrap();
        conn.execute("INSERT INTO secrets VALUES ('protected value')")
            .unwrap();
        conn.close().unwrap();
    }
    let mut owner = OpeningActor::new(encrypted.clone(), Some(key(CipherMode::Aegis256, 1)));
    io.set_owner("actor0");
    loop {
        let result = Database::open_async(
            owner.state.as_mut().unwrap(),
            io.clone(),
            path,
            &owner.options,
        )
        .unwrap();
        let IOResult::IO(pending) = result else {
            panic!("opening must yield before registry publication")
        };
        owner.pending = Some(IOCompletions(pending.0.clone()));
        if existing || !wait_for_bytes {
            break;
        }
        pending.wait(io.as_ref()).unwrap();
        if raw.size().unwrap() > 0 {
            break;
        }
    }
    let (options, reader_key) = match credential {
        Credential::Missing => (base.clone(), None),
        Credential::Wrong => (
            base.clone()
                .encryption(encryption_options(CipherMode::Aegis256, 2)),
            Some(key(CipherMode::Aegis256, 2)),
        ),
        Credential::WrongCipher => (
            base.clone()
                .encryption(encryption_options(CipherMode::Aes256Gcm, 2)),
            Some(key(CipherMode::Aes256Gcm, 2)),
        ),
        Credential::Correct => (encrypted.clone(), Some(key(CipherMode::Aegis256, 1))),
    };
    let mut peer = OpeningActor::new(options, reader_key);
    io.set_owner("actor1");
    trace.observe(format!(
        "creator paused, database bytes={}",
        raw.size().unwrap()
    ));
    let pending = Database::open_async(
        peer.state.as_mut().unwrap(),
        io.clone(),
        path,
        &peer.options,
    )
    .unwrap();
    let IOResult::IO(pending) = pending else {
        panic!("the competing open must wait for the registry owner");
    };
    check!(
        pending.is_explicit_yield(),
        "registry_reservation",
        "a competing open must wait on the registry owner"
    );
    trace.observe("waiter yielded on the registry owner");
    peer.pending = Some(pending);
    peer.waiting_epoch = Some(0);
    if cancel_waiter {
        peer.state = None;
        peer.pending = None;
        peer.done = true;
        peer.dropped = true;
    }
    let mut actors = [owner, peer];
    let mut registry_epoch = 0;
    let mut injected_failure = false;
    // Whether the creator wrote database bytes or published the database
    // before it went away. Until then no key is established, and a waiting
    // opener may create the database with its own credentials.
    let mut creator_established = existing;
    loop {
        let pending = io.pending_events();
        if actors.iter().all(|actor| actor.done)
            && pending.is_empty()
            && !io.has_underlying_completions()
        {
            break;
        }
        let mut enabled = Vec::new();
        for (index, actor) in actors.iter().enumerate() {
            if !actor.done
                && actor
                    .waiting_epoch
                    .is_none_or(|epoch| epoch != registry_epoch)
                && actor
                    .pending
                    .as_ref()
                    .is_none_or(|pending| pending.finished())
            {
                enabled.push(Action::Poll(index));
            }
            if !actor.dropped
                && actor.waiting_epoch != Some(registry_epoch)
                && (actor.state.is_some() || actor.db.is_some() || actor.connection.is_some())
            {
                enabled.push(Action::Drop(index));
            }
        }
        for operation in &pending {
            enabled.push(Action::Complete(operation.id.clone()));
            if trace.allow_failures
                && !injected_failure
                && matches!(
                    operation.kind,
                    QueuedIoOpKind::Pread
                        | QueuedIoOpKind::Pwrite
                        | QueuedIoOpKind::Pwritev
                        | QueuedIoOpKind::Sync
                )
            {
                enabled.push(Action::Fail(operation.id.clone()));
            }
        }
        if io.has_underlying_completions() {
            enabled.push(Action::CompleteUnderlying);
        }
        check!(
            !enabled.is_empty(),
            "stuck_state",
            "registry opening deadlocked"
        );
        let action = trace.choose(&enabled);
        if let Action::Poll(index) | Action::Drop(index) = action {
            io.set_owner(format!("actor{index}"));
        }
        match action {
            Action::Poll(index) => {
                let actor = &mut actors[index];
                if actor
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.get_error().is_some())
                {
                    if index == 0 {
                        creator_established |= raw.size().unwrap() > 0;
                    }
                    actor.state = None;
                    actor.pending = None;
                    actor.done = true;
                    registry_epoch += 1;
                    continue;
                }
                actor.pending = None;
                actor.waiting_epoch = None;
                if let Some(db) = &actor.db {
                    let result =
                        db.connect(ConnectOpts::new().with_encryption_key(actor.key.clone()));
                    let peer_created = index == 1 && !creator_established;
                    if peer_created {
                        trace.observe("the waiter created the database after the creator left");
                    }
                    if index == 0 || credential == Credential::Correct || peer_created {
                        check!(
                            result
                                .as_ref()
                                .err()
                                .is_none_or(|error| injected_failure && injected_io_error(error)),
                            "authorized_operation_failed",
                            "valid connection failed: {:?}",
                            result.as_ref().err()
                        );
                    }
                    actor.connection = result.ok();
                    if index == 1 && credential != Credential::Correct && !peer_created {
                        check!(
                            actor.connection.is_none(),
                            "unauthorized_connection",
                            "invalid credentials connected to an encrypted database"
                        );
                    }
                    actor.done = true;
                } else {
                    match Database::open_async(
                        actor.state.as_mut().unwrap(),
                        io.clone(),
                        path,
                        &actor.options,
                    ) {
                        Ok(IOResult::Done(db)) => {
                            if index == 0 {
                                creator_established = true;
                            }
                            actor.db = Some(db);
                            actor.state = None;
                            registry_epoch += 1;
                        }
                        Ok(IOResult::IO(pending)) => {
                            if pending.is_explicit_yield() {
                                actor.waiting_epoch = Some(registry_epoch);
                            }
                            actor.pending = Some(pending);
                        }
                        Err(error) => {
                            check!(
                                index != 0 || (injected_failure && injected_io_error(&error)),
                                "authorized_operation_failed",
                                "creator failed without an injected error: {error}"
                            );
                            check!(
                                (credential != Credential::Correct
                                    && (index == 0 || creator_established))
                                    || (injected_failure && injected_io_error(&error)),
                                "authorized_operation_failed",
                                "a peer that may open the database failed: {error}"
                            );
                            if index == 0 {
                                creator_established |= raw.size().unwrap() > 0;
                            }
                            actor.state = None;
                            actor.done = true;
                            registry_epoch += 1;
                        }
                    }
                }
            }
            Action::Drop(index) => {
                if index == 0 && actors[0].db.is_none() {
                    creator_established |= raw.size().unwrap() > 0;
                }
                actors[index].state = None;
                actors[index].connection = None;
                actors[index].db = None;
                actors[index].pending = None;
                actors[index].done = true;
                actors[index].dropped = true;
                registry_epoch += 1;
            }
            Action::Complete(id) => {
                io.step_request(&id, false).unwrap().unwrap();
            }
            Action::Fail(id) => {
                injected_failure = true;
                io.step_request(&id, true).unwrap().unwrap();
            }
            Action::CompleteUnderlying => io.step_underlying().unwrap(),
            Action::Start(_) => unreachable!(),
        }
        io.set_owner("scheduler");
        trace.after_action(&io);
    }
    io.set_owner("recovery");
    // The surviving first page decides the database's key: normally the
    // creator's, but the waiter's when it created the database after the
    // creator left without establishing anything.
    let peer_owns_file = credential != Credential::Correct
        && io
            .records()
            .iter()
            .rev()
            .find(|record| {
                record.event.path == path
                    && record.event.offset == 0
                    && record.succeeded()
                    && matches!(
                        record.event.kind,
                        QueuedIoOpKind::Pwrite | QueuedIoOpKind::Pwritev
                    )
            })
            .is_some_and(|record| record.event.id.owner == "actor1");
    let peer_options = actors[1].options.clone();
    let peer_key = actors[1].key.clone();
    let (recovery_options, recovery_key) = if peer_owns_file {
        check!(
            !existing,
            "binding_changed",
            "an invalid credential rewrote page one of an existing database"
        );
        trace.observe("the waiter's credentials own the database");
        (peer_options, peer_key)
    } else {
        (encrypted.clone(), Some(key(CipherMode::Aegis256, 1)))
    };
    let opened = Database::open(io.clone(), path, recovery_options);
    check!(
        opened.is_ok(),
        "recovery_failed",
        "the credentials of the surviving first page cannot open it: {:?}",
        opened.as_ref().err()
    );
    let db = opened.unwrap();
    let connected = db.connect(ConnectOpts::new().with_encryption_key(recovery_key));
    check!(
        connected.is_ok(),
        "recovery_failed",
        "the credentials of the surviving first page cannot connect: {:?}",
        connected.as_ref().err()
    );
    let conn = connected.unwrap();
    conn.execute("CREATE TABLE IF NOT EXISTS secrets(value TEXT)")
        .unwrap();
    if !existing {
        conn.execute("INSERT INTO secrets VALUES ('protected value')")
            .unwrap();
    }
    let rows: Vec<(String,)> = conn.exec_rows("SELECT value FROM secrets");
    check!(
        rows == [("protected value".to_string(),)],
        "acknowledged_write_lost",
        "recovery read {rows:?}"
    );
    let integrity: Vec<(String,)> = conn.exec_rows("PRAGMA integrity_check");
    check!(
        integrity == [("ok".to_string(),)],
        "recovery_failed",
        "the recovered database failed integrity_check: {integrity:?}"
    );
    if peer_owns_file {
        trace.observe("the replacement waiter wrote and read the marker");
        check!(
            db.connect(ConnectOpts::new().with_encryption_key(Some(key(CipherMode::Aegis256, 1))))
                .is_err(),
            "binding_changed",
            "the creator's abandoned key still opens a database the waiter created"
        );
        trace.after_action(&io);
        return;
    }
    for actor in actors {
        if let Some(connection) = actor.connection {
            let rows: Vec<(String,)> = connection.exec_rows("SELECT value FROM secrets");
            check!(
                rows == [("protected value".to_string(),)],
                "authorized_operation_failed",
                "a valid connection read {rows:?}"
            );
        }
    }
    check!(
        Database::open(io.clone(), path, base).is_err(),
        "unauthorized_connection",
        "a cached encrypted database opened without credentials"
    );
    trace.after_action(&io);
}

struct OpeningActor {
    options: OpenOptions,
    key: Option<EncryptionKey>,
    state: Option<OpenDbAsyncState>,
    pending: Option<IOCompletions>,
    waiting_epoch: Option<usize>,
    db: Option<Arc<Database>>,
    connection: Option<Arc<Connection>>,
    done: bool,
    dropped: bool,
}

impl OpeningActor {
    fn new(options: OpenOptions, key: Option<EncryptionKey>) -> Self {
        Self {
            options,
            key,
            state: Some(OpenDbAsyncState::new()),
            pending: None,
            waiting_epoch: None,
            db: None,
            connection: None,
            done: false,
            dropped: false,
        }
    }
}

fn encryption_options(cipher: CipherMode, byte: u8) -> EncryptionOpts {
    EncryptionOpts {
        cipher: cipher.to_string(),
        hexkey: format!("{byte:02x}").repeat(cipher.required_key_size()),
    }
}

fn page_one_schedules(ciphers: &[CipherMode]) {
    for &cipher in ciphers {
        for page_size in [512, 1024, 2048, 4096, 8192, 16384, 32768, 65536] {
            for credential in [
                Credential::Missing,
                Credential::Wrong,
                Credential::WrongCipher,
                Credential::Correct,
            ] {
                let name = format!("page one: {cipher:?}, {page_size}, {credential:?}");
                explore(&name, |trace| {
                    page_one_schedule(trace, cipher, page_size, credential);
                });
            }
        }
    }
}

fn page_one_schedule(
    trace: &mut Trace,
    cipher: CipherMode,
    page_size: u32,
    credential: Credential,
) {
    let inner = Arc::new(MemoryIO::new());
    let io = Arc::new(QueuedIo::new_with_io(inner.clone()));
    let path = ":memory:exhaustive-encrypted-page-one";
    io.set_owner("setup");
    let db = Database::open(
        io.clone(),
        path,
        OpenOptions::new(Arc::new(SqliteDialect))
            .db_opts(DatabaseOpts::new().with_encryption(true))
            .encryption(encryption_options(cipher, 1))
            .page_size(turso_core::storage::sqlite3_ondisk::PageSize::new(page_size).unwrap()),
    )
    .unwrap();
    let writer = db
        .connect(ConnectOpts::new().with_encryption_key(Some(key(cipher, 1))))
        .unwrap();
    let reader_opts = ConnectOpts::new().with_encryption_key(match credential {
        Credential::Missing => None,
        Credential::Wrong | Credential::WrongCipher => Some(key(cipher, 2)),
        Credential::Correct => Some(key(cipher, 1)),
    });
    let mut actors = [Actor::new(writer), Actor::connecting(reader_opts)];
    let mut injected_failure = false;
    loop {
        let pending = io.pending_events();
        if actors.iter().all(|actor| actor.done)
            && pending.is_empty()
            && !io.has_underlying_completions()
        {
            break;
        }
        let mut enabled = Vec::new();
        for (index, actor) in actors.iter().enumerate() {
            if actor.done {
                continue;
            }
            if actor
                .pending
                .as_ref()
                .is_none_or(|pending| pending.finished())
            {
                enabled.push(Action::Poll(index));
            }
            enabled.push(Action::Drop(index));
        }
        for operation in &pending {
            enabled.push(Action::Complete(operation.id.clone()));
            if trace.allow_failures
                && !injected_failure
                && matches!(
                    operation.kind,
                    QueuedIoOpKind::Pread | QueuedIoOpKind::Pwrite | QueuedIoOpKind::Sync
                )
            {
                enabled.push(Action::Fail(operation.id.clone()));
            }
        }
        if io.has_underlying_completions() {
            enabled.push(Action::CompleteUnderlying);
        }
        check!(
            !enabled.is_empty(),
            "stuck_state",
            "initialization deadlocked"
        );
        let action = trace.choose(&enabled);
        if let Action::Poll(index) | Action::Drop(index) = action {
            io.set_owner(format!("actor{index}"));
            let other = &actors[1 - index];
            if matches!(action, Action::Poll(_))
                && other
                    .pending
                    .as_ref()
                    .is_some_and(|pending| !pending.finished())
            {
                trace.observe(format!(
                    "actor{index} polled while the other actor waited on I/O"
                ));
            }
        }
        match action {
            Action::Poll(index) => {
                let actor = &mut actors[index];
                let allowed = index == 0 || credential == Credential::Correct;
                actor.pending = None;
                if actor.connection.is_none() {
                    match db.connect_async(
                        actor.connect_opts.as_ref().unwrap().clone(),
                        &mut actor.connect_state,
                    ) {
                        Ok(IOResult::Done(connection)) => actor.connection = Some(connection),
                        Ok(IOResult::IO(pending)) => {
                            actor.pending = Some(pending);
                            continue;
                        }
                        Err(error) => {
                            check!(
                                !allowed || (injected_failure && injected_io_error(&error)),
                                "unauthorized_connection",
                                "connection result: {error}"
                            );
                            actor.done = true;
                            continue;
                        }
                    }
                }
                match actor
                    .connection
                    .as_ref()
                    .unwrap()
                    .get_pager()
                    .maybe_allocate_page1()
                {
                    Ok(IOResult::Done(())) => {
                        check!(
                            allowed,
                            "unauthorized_initialization",
                            "an invalid key completed initialization"
                        );
                        actor.done = true;
                    }
                    Ok(IOResult::IO(pending)) => {
                        check!(
                            !pending.is_explicit_yield(),
                            "stuck_state",
                            "unexpected lock wait"
                        );
                        actor.pending = Some(pending);
                    }
                    Err(error) => {
                        check!(
                            !allowed || (injected_failure && injected_io_error(&error)),
                            "authorized_operation_failed",
                            "valid key rejected: {error}"
                        );
                        actor.done = true;
                    }
                }
            }
            Action::Drop(index) => {
                actors[index].connect_state = turso_core::ConnectAsyncState::new();
                actors[index].connection = None;
                actors[index].pending = None;
                actors[index].done = true;
            }
            Action::Complete(id) => {
                io.step_request(&id, false).unwrap().unwrap();
            }
            Action::Fail(id) => {
                injected_failure = true;
                io.step_request(&id, true).unwrap().unwrap();
            }
            Action::CompleteUnderlying => io.step_underlying().unwrap(),
            Action::Start(_) => unreachable!(),
        }
        io.set_owner("scheduler");
        trace.after_action(&io);
        let raw = inner.open_file(path, OpenFlags::None, false).unwrap();
        if raw.size().unwrap() > 0 {
            let buffer = Arc::new(Buffer::new_temporary(100));
            let completion = Completion::new_read(buffer.clone(), Box::new(|_| None));
            let completion = raw.pread(0, completion).unwrap();
            assert!(completion.succeeded());
            check!(
                &buffer.as_slice()[..5] == b"Turso" && buffer.as_slice()[6] == cipher.cipher_id(),
                "plaintext_page_one",
                "page one header is not encrypted with the established cipher"
            );
        }
    }

    if !injected_failure {
        let allocations = io
            .records()
            .iter()
            .filter(|record| {
                record.event.path == path
                    && record.event.kind == QueuedIoOpKind::Pwrite
                    && record.event.offset == 0
                    && record.succeeded()
            })
            .count();
        trace.observe(format!("initial page-one writes: {allocations}"));
        check!(
            allocations <= 1,
            "duplicate_initial_allocation",
            "page one was allocated {allocations} times"
        );
        if actors.iter().any(|actor| {
            actor.done
                && actor.connection.is_some()
                && (credential == Credential::Correct || std::ptr::eq(actor, &actors[0]))
        }) {
            check!(
                allocations == 1,
                "missing_initial_allocation",
                "successful initialization did not allocate page one"
            );
        }
    }
    io.set_owner("recovery");
    let recovery = db
        .connect(ConnectOpts::new().with_encryption_key(Some(key(cipher, 1))))
        .unwrap();
    let result = recovery.execute("CREATE TABLE secrets(value TEXT)");
    if let Err(error) = result {
        check!(
            injected_failure && injected_io_error(&error),
            "recovery_failed",
            "recovery failed: {error}"
        );
        recovery
            .execute("CREATE TABLE secrets(value TEXT)")
            .unwrap();
    }
    recovery
        .execute("INSERT INTO secrets VALUES ('protected value')")
        .unwrap();
    for (index, actor) in actors.iter().enumerate() {
        if let Some(connection) = &actor.connection {
            if index == 0 || credential == Credential::Correct {
                let rows: Vec<(String,)> = connection.exec_rows("SELECT value FROM secrets");
                check!(
                    rows == [("protected value".to_string(),)],
                    "authorized_operation_failed",
                    "a valid connection read {rows:?}"
                );
            } else {
                check!(
                    connection.execute("SELECT value FROM secrets").is_err(),
                    "unauthorized_row",
                    "an invalid key read protected rows"
                );
                check!(
                    connection
                        .execute("SELECT name FROM sqlite_schema")
                        .is_err(),
                    "unauthorized_schema",
                    "an invalid key read protected schema"
                );
            }
        }
    }
    check!(
        db.connect(ConnectOpts::new()).is_err(),
        "unauthorized_connection",
        "a keyless connection opened an encrypted database"
    );
    check!(
        db.connect(ConnectOpts::new().with_encryption_key(Some(key(cipher, 2))))
            .is_err(),
        "unauthorized_connection",
        "a wrong key opened an encrypted database"
    );
    trace.after_action(&io);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Credential {
    Missing,
    Wrong,
    WrongCipher,
    Correct,
}

struct Actor {
    connect_opts: Option<ConnectOpts>,
    connect_state: turso_core::ConnectAsyncState,
    connection: Option<Arc<Connection>>,
    pending: Option<IOCompletions>,
    done: bool,
}

impl Actor {
    fn connecting(opts: ConnectOpts) -> Self {
        Self {
            connection: None,
            connect_opts: Some(opts),
            connect_state: turso_core::ConnectAsyncState::new(),
            pending: None,
            done: false,
        }
    }

    fn new(connection: Arc<Connection>) -> Self {
        Self {
            connect_opts: None,
            connect_state: turso_core::ConnectAsyncState::new(),
            connection: Some(connection),
            pending: None,
            done: false,
        }
    }
}

/// One scheduling decision. Requests are named by their logical identity, so
/// a recorded decision never refers to whatever request now sits at a queue
/// position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Action {
    /// Resume the actor's current operation.
    Poll(usize),
    /// Let a queued request run.
    Complete(QueuedIoRequestId),
    /// Make a queued request fail with an injected I/O error.
    Fail(QueuedIoRequestId),
    /// Let the wrapped asynchronous backend deliver dispatched completions.
    CompleteUnderlying,
    /// Drop the actor's current operation or handles.
    Drop(usize),
    /// Start the actor's next scripted command.
    Start(usize),
}

/// Chooses among enabled actions for one run: DFS exploration, a fixed
/// prefix, or a policy supplied by the fuzzer.
pub(super) struct Trace {
    pub(super) prefix: Vec<Action>,
    pub(super) actions: Vec<Action>,
    pub(super) alternatives: Vec<Vec<Action>>,
    pub(super) allow_failures: bool,
    pub(super) namespace: String,
    pub(super) chooser: Option<Box<dyn randomized::Chooser>>,
    /// Observations made while executing each decision, indexed like `actions`.
    /// Index 0 holds what happened before the first decision.
    pub(super) outcomes: Vec<Vec<String>>,
    pub(super) max_decisions: usize,
    /// Decisions during which new work, failures and drops may be chosen.
    /// After that only pending work is finished.
    pub(super) generated: usize,
    io_records_seen: usize,
    io_completions_seen: Vec<QueuedIoRequestId>,
}

impl Default for Trace {
    fn default() -> Self {
        Self {
            prefix: Vec::new(),
            actions: Vec::new(),
            alternatives: Vec::new(),
            allow_failures: true,
            namespace: String::new(),
            chooser: None,
            outcomes: vec![Vec::new()],
            max_decisions: 1024,
            generated: usize::MAX,
            io_records_seen: 0,
            io_completions_seen: Vec::new(),
        }
    }
}

impl Trace {
    pub(super) fn choose(&mut self, enabled: &[Action]) -> Action {
        let action = if let Some(chooser) = &mut self.chooser {
            chooser.choose(enabled, &self.actions)
        } else if let Some(action) = self.prefix.get(self.actions.len()) {
            if !enabled.contains(action) {
                std::panic::panic_any(ReplayDivergence(format!(
                    "decision {} {action:?} is not enabled; enabled: {enabled:?}",
                    self.actions.len()
                )));
            }
            action.clone()
        } else {
            for action in &enabled[1..] {
                let mut prefix = self.actions.clone();
                prefix.push(action.clone());
                self.alternatives.push(prefix);
            }
            enabled[0].clone()
        };
        self.actions.push(action.clone());
        self.outcomes.push(Vec::new());
        check!(
            self.actions.len() <= self.max_decisions,
            "stuck_state",
            "the schedule did not finish within {} decisions",
            self.max_decisions
        );
        if let Some(chooser) = &mut self.chooser {
            chooser.record(&self.actions, &self.outcomes);
        }
        action
    }

    /// Records an observation for the latest decision.
    pub(super) fn observe(&mut self, observation: impl Into<String>) {
        self.outcomes.last_mut().unwrap().push(observation.into());
        if let Some(chooser) = &mut self.chooser {
            chooser.record(&self.actions, &self.outcomes);
        }
    }

    /// Records which requests ran since the last call and why.
    pub(super) fn after_action(&mut self, io: &QueuedIo) {
        let records = io.records();
        for record in &records[self.io_records_seen..] {
            let cause = match record.cause {
                crate::queued_io::QueuedIoCause::Pump => {
                    format!("Pump by {}", record.running_owner)
                }
                cause => format!("{cause:?}"),
            };
            let observation = format!(
                "io {} {cause} {:?} {}+{} {} aborted_before_dispatch={}",
                record.event.id,
                record.event.kind,
                record.event.offset,
                record.event.len,
                if record.injected_error {
                    "injected"
                } else if !record.finished() {
                    "pending"
                } else if record.succeeded() {
                    "ok"
                } else {
                    "error"
                },
                record.aborted_before_dispatch,
            );
            self.outcomes.last_mut().unwrap().push(observation);
            if record.finished() {
                self.io_completions_seen.push(record.event.id.clone());
            }
        }
        for record in &records[..self.io_records_seen] {
            if record.finished() && !self.io_completions_seen.contains(&record.event.id) {
                self.outcomes.last_mut().unwrap().push(format!(
                    "io completion {} {}",
                    record.event.id,
                    if record.succeeded() { "ok" } else { "error" }
                ));
                self.io_completions_seen.push(record.event.id.clone());
            }
        }
        self.io_records_seen = records.len();
        if let Some(chooser) = &mut self.chooser {
            chooser.record(&self.actions, &self.outcomes);
        }
    }
}

pub(super) fn explore(name: &str, scenario: impl Fn(&mut Trace)) {
    let mut prefixes = vec![Vec::new()];
    let mut schedules = 0;
    while let Some(prefix) = prefixes.pop() {
        let mut trace = Trace {
            prefix,
            ..Trace::default()
        };
        if let Err(error) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scenario(&mut trace)))
        {
            eprintln!("encryption schedule failed: {name}: {:?}", trace.actions);
            std::panic::resume_unwind(error);
        }
        prefixes.extend(trace.alternatives);
        schedules += 1;
        if schedules % 10_000 == 0 {
            eprintln!("encryption I/O DFS {name}: {schedules} schedules explored");
        }
    }
    eprintln!("encryption I/O DFS {name}: {schedules} complete schedules");
}

fn injected_io_error(error: &LimboError) -> bool {
    matches!(
        error,
        LimboError::CompletionError(turso_core::CompletionError::IOError(_, "queued_io"))
    )
}

fn key(cipher: CipherMode, byte: u8) -> EncryptionKey {
    match cipher.required_key_size() {
        16 => EncryptionKey::Key128([byte; 16]),
        32 => EncryptionKey::new_256([byte; 32]),
        _ => unreachable!(),
    }
}
