use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::{collections::HashMap, convert::TryInto};

use turso_core::io::FileSyncType;
use turso_core::{
    Buffer, Clock, Completion, Database, DatabaseOpts, File, MemoryIO, MonotonicInstant, OpenFlags,
    StepResult, WallClockInstant, IO,
};

struct PendingRead {
    file: Arc<dyn File>,
    pos: u64,
    completion: Completion,
}

struct DelayedReadIO {
    inner: Arc<MemoryIO>,
    delay_page_id: u32,
    db_page_size: u64,
    armed: Arc<AtomicBool>,
    delayed: Arc<AtomicBool>,
    skip_matching_reads: Arc<Mutex<usize>>,
    pending: Arc<Mutex<Vec<PendingRead>>>,
    wal_frame_pages: Arc<Mutex<HashMap<u64, u32>>>,
}

impl DelayedReadIO {
    fn new(delay_page_id: u32) -> Self {
        Self::new_with_page_size(delay_page_id, 4096)
    }

    fn new_with_page_size(delay_page_id: u32, db_page_size: u64) -> Self {
        Self {
            inner: Arc::new(MemoryIO::new()),
            delay_page_id,
            db_page_size,
            armed: Arc::new(AtomicBool::new(false)),
            delayed: Arc::new(AtomicBool::new(false)),
            skip_matching_reads: Arc::new(Mutex::new(0)),
            pending: Arc::new(Mutex::new(Vec::new())),
            wal_frame_pages: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn arm(&self) {
        self.arm_after_skips(0);
    }

    fn arm_after_skips(&self, skips: usize) {
        self.armed.store(true, Ordering::SeqCst);
        self.delayed.store(false, Ordering::SeqCst);
        *self.skip_matching_reads.lock().unwrap() = skips;
    }

    fn did_delay(&self) -> bool {
        self.delayed.load(Ordering::SeqCst)
    }
}

impl Clock for DelayedReadIO {
    fn current_time_monotonic(&self) -> MonotonicInstant {
        self.inner.current_time_monotonic()
    }

    fn current_time_wall_clock(&self) -> WallClockInstant {
        self.inner.current_time_wall_clock()
    }
}

impl IO for DelayedReadIO {
    fn open_file(
        &self,
        path: &str,
        flags: OpenFlags,
        direct: bool,
    ) -> turso_core::Result<Arc<dyn File>> {
        let inner = self.inner.open_file(path, flags, direct)?;
        Ok(Arc::new(DelayedReadFile {
            inner,
            path: path.to_string(),
            delay_page_id: self.delay_page_id,
            db_page_size: self.db_page_size,
            armed: self.armed.clone(),
            delayed: self.delayed.clone(),
            skip_matching_reads: self.skip_matching_reads.clone(),
            pending: self.pending.clone(),
            wal_frame_pages: self.wal_frame_pages.clone(),
        }))
    }

    fn remove_file(&self, path: &str) -> turso_core::Result<()> {
        self.inner.remove_file(path)
    }

    fn file_id(&self, path: &str) -> turso_core::Result<turso_core::io::FileId> {
        self.inner.file_id(path)
    }

    fn supports_shared_wal_coordination(&self) -> bool {
        self.inner.supports_shared_wal_coordination()
    }

    fn step(&self) -> turso_core::Result<()> {
        let pending = std::mem::take(&mut *self.pending.lock().unwrap());
        for read in pending {
            let _ = read.file.pread(read.pos, read.completion)?;
        }
        self.inner.step()
    }
}

struct DelayedReadFile {
    inner: Arc<dyn File>,
    path: String,
    delay_page_id: u32,
    db_page_size: u64,
    armed: Arc<AtomicBool>,
    delayed: Arc<AtomicBool>,
    skip_matching_reads: Arc<Mutex<usize>>,
    pending: Arc<Mutex<Vec<PendingRead>>>,
    wal_frame_pages: Arc<Mutex<HashMap<u64, u32>>>,
}

impl DelayedReadFile {
    const WAL_HEADER_SIZE: u64 = 32;
    const WAL_FRAME_HEADER_SIZE: u64 = 24;

    fn page_id_for_read(&self, pos: u64) -> Option<u32> {
        if self.path.ends_with("-wal") {
            return self.wal_frame_pages.lock().unwrap().get(&pos).copied();
        }
        if pos % self.db_page_size == 0 {
            Some((pos / self.db_page_size + 1) as u32)
        } else {
            None
        }
    }

    fn record_wal_frame_write(&self, pos: u64, buffer: &Buffer) {
        if !self.path.ends_with("-wal")
            || pos < Self::WAL_HEADER_SIZE
            || (pos - Self::WAL_HEADER_SIZE) % (self.db_page_size + Self::WAL_FRAME_HEADER_SIZE)
                != 0
            || buffer.len() < Self::WAL_FRAME_HEADER_SIZE as usize
        {
            return;
        }

        let page_id = u32::from_be_bytes(
            buffer.as_slice()[0..4]
                .try_into()
                .expect("WAL frame header page id"),
        );
        self.wal_frame_pages
            .lock()
            .unwrap()
            .insert(pos + Self::WAL_FRAME_HEADER_SIZE, page_id);
    }
}

impl File for DelayedReadFile {
    fn lock_file(&self, exclusive: bool) -> turso_core::Result<()> {
        self.inner.lock_file(exclusive)
    }

    fn unlock_file(&self) -> turso_core::Result<()> {
        self.inner.unlock_file()
    }

    fn pread(&self, pos: u64, c: Completion) -> turso_core::Result<Completion> {
        if self.page_id_for_read(pos) == Some(self.delay_page_id)
            && self.armed.load(Ordering::SeqCst)
            && !self.delayed.swap(true, Ordering::SeqCst)
        {
            let mut skips = self.skip_matching_reads.lock().unwrap();
            if *skips > 0 {
                *skips -= 1;
                self.delayed.store(false, Ordering::SeqCst);
                drop(skips);
                return self.inner.pread(pos, c);
            }
            self.pending.lock().unwrap().push(PendingRead {
                file: self.inner.clone(),
                pos,
                completion: c.clone(),
            });
            return Ok(c);
        }
        self.inner.pread(pos, c)
    }

    fn pwrite(
        &self,
        pos: u64,
        buffer: Arc<turso_core::Buffer>,
        c: Completion,
    ) -> turso_core::Result<Completion> {
        self.record_wal_frame_write(pos, buffer.as_ref());
        self.inner.pwrite(pos, buffer, c)
    }

    fn sync(&self, c: Completion, sync_type: FileSyncType) -> turso_core::Result<Completion> {
        self.inner.sync(c, sync_type)
    }

    fn pwritev(
        &self,
        pos: u64,
        buffers: Vec<Arc<turso_core::Buffer>>,
        c: Completion,
    ) -> turso_core::Result<Completion> {
        let mut offset = pos;
        for buffer in &buffers {
            self.record_wal_frame_write(offset, buffer.as_ref());
            offset += buffer.len() as u64;
        }
        self.inner.pwritev(pos, buffers, c)
    }

    fn size(&self) -> turso_core::Result<u64> {
        self.inner.size()
    }

    fn truncate(&self, len: u64, c: Completion) -> turso_core::Result<Completion> {
        self.inner.truncate(len, c)
    }
}

fn query_single_text(
    conn: &Arc<turso_core::Connection>,
    io: &dyn IO,
    sql: &str,
) -> turso_core::Result<String> {
    let mut stmt = conn.prepare(sql)?;
    loop {
        match stmt.step()? {
            StepResult::Row => {
                return Ok(stmt
                    .row()
                    .expect("row should be available")
                    .get::<String>(0)
                    .expect("column should be text"));
            }
            StepResult::IO => stmt
                .take_io_completions()
                .expect("IO step should expose completions")
                .wait(io)?,
            StepResult::Done => panic!("{sql} returned no rows"),
            StepResult::Interrupt | StepResult::Busy => return Err(turso_core::LimboError::Busy),
        }
    }
}

fn query_single_i64(
    conn: &Arc<turso_core::Connection>,
    io: &dyn IO,
    sql: &str,
) -> turso_core::Result<i64> {
    let mut stmt = conn.prepare(sql)?;
    loop {
        match stmt.step()? {
            StepResult::Row => {
                return Ok(stmt
                    .row()
                    .expect("row should be available")
                    .get::<i64>(0)
                    .expect("column should be an integer"));
            }
            StepResult::IO => stmt
                .take_io_completions()
                .expect("IO step should expose completions")
                .wait(io)?,
            StepResult::Done => panic!("{sql} returned no rows"),
            StepResult::Interrupt | StepResult::Busy => return Err(turso_core::LimboError::Busy),
        }
    }
}

fn setup_antithesis_like_freelist(conn: &Arc<turso_core::Connection>) -> turso_core::Result<()> {
    conn.execute("PRAGMA page_size=512")?;
    conn.execute("PRAGMA journal_mode=WAL")?;
    conn.execute(
        "CREATE TABLE calm_roof_888 (
            cold_door_963 BLOB,
            big_bird_239 REAL,
            wet_stone_286 INTEGER NOT NULL PRIMARY KEY,
            hot_door_96 INTEGER,
            brave_lake_630 INTEGER
        )",
    )?;
    for id in 1..=1000 {
        conn.execute(format!(
            "INSERT INTO calm_roof_888 (
                cold_door_963, big_bird_239, wet_stone_286, hot_door_96, brave_lake_630
            ) VALUES (zeroblob(20), 1.0, {id}, {id}, {id})"
        ))?;
    }
    conn.execute("DELETE FROM calm_roof_888 WHERE wet_stone_286 BETWEEN 1 AND 495")?;
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")?;
    Ok(())
}

#[test]
fn abandoned_delete_at_freelist_trunk_read_preserves_integrity() -> anyhow::Result<()> {
    const FIRST_FREELIST_TRUNK_PAGE: u32 = 3;

    let io = Arc::new(DelayedReadIO::new(FIRST_FREELIST_TRUNK_PAGE));
    let db = Database::open_file_with_flags(
        io.clone(),
        "freelist-user-facing-regression.db",
        OpenFlags::Create,
        DatabaseOpts::new(),
        None,
    )?;

    let setup = db.connect()?;
    setup.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, b BLOB)")?;
    for id in 1..=3 {
        setup.execute(format!("INSERT INTO t VALUES ({id}, zeroblob(9000))"))?;
    }
    setup.execute("DELETE FROM t WHERE id = 1")?;

    let conn = db.connect()?;
    conn.execute("BEGIN")?;
    assert!(
        conn.execute("INSERT INTO t VALUES (2, zeroblob(16))")
            .is_err(),
        "duplicate primary key insert should fail"
    );
    conn.execute("SAVEPOINT sp")?;

    io.arm();
    let mut delete = conn.prepare("DELETE FROM t WHERE id = 2")?;
    loop {
        match delete.step()? {
            StepResult::IO => {
                break;
            }
            StepResult::Done => panic!("DELETE should yield at the delayed freelist trunk read"),
            StepResult::Row => panic!("DELETE without RETURNING should not produce rows"),
            StepResult::Interrupt | StepResult::Busy => {
                return Err(turso_core::LimboError::Busy.into())
            }
        }
    }
    assert!(
        io.did_delay(),
        "DELETE should have delayed the freelist trunk page read"
    );

    delete
        .take_io_completions()
        .expect("DELETE should expose the delayed read completion")
        .wait(io.as_ref())?;
    conn.execute("ROLLBACK TO sp")?;
    drop(delete);
    conn.execute("RELEASE sp")?;
    conn.execute("DELETE FROM t WHERE id = 3")?;
    conn.execute("COMMIT")?;

    assert_eq!(
        query_single_text(&conn, io.as_ref(), "PRAGMA integrity_check")?,
        "ok"
    );
    Ok(())
}

#[test]
fn antithesis_savepoint_exact_freelist_error_regression() -> anyhow::Result<()> {
    const FIRST_FREELIST_TRUNK_PAGE: u32 = 5;

    let io = Arc::new(DelayedReadIO::new_with_page_size(
        FIRST_FREELIST_TRUNK_PAGE,
        512,
    ));
    let db_path = "freelist-antithesis-exact-regression.db";
    let db = Database::open_file_with_flags(
        io.clone(),
        db_path,
        OpenFlags::Create,
        DatabaseOpts::new(),
        None,
    )?;
    let setup = db.connect()?;
    setup_antithesis_like_freelist(&setup)?;
    assert_eq!(
        query_single_i64(&setup, io.as_ref(), "PRAGMA freelist_count")?,
        46
    );
    drop(setup);
    drop(db);

    let db = Database::open_file_with_flags(
        io.clone(),
        db_path,
        OpenFlags::Create,
        DatabaseOpts::new(),
        None,
    )?;

    let writer = db.connect()?;
    writer.execute("BEGIN")?;
    writer.execute("SAVEPOINT sp_96")?;
    writer.execute("PRAGMA cache_size=2")?;
    assert_eq!(
        query_single_i64(
            &writer,
            io.as_ref(),
            "SELECT count(*) FROM calm_roof_888 WHERE wet_stone_286 >= 496"
        )?,
        505
    );

    io.arm_after_skips(0);
    let mut update =
        writer.prepare("DELETE FROM calm_roof_888 WHERE wet_stone_286 BETWEEN 551 AND 583")?;
    loop {
        match update.step()? {
            StepResult::IO => break,
            StepResult::Done => panic!("DELETE should yield at the delayed freelist trunk read"),
            StepResult::Row => panic!("DELETE without RETURNING should not produce rows"),
            StepResult::Interrupt | StepResult::Busy => {
                return Err(turso_core::LimboError::Busy.into())
            }
        }
    }
    assert!(
        io.did_delay(),
        "DELETE should have delayed the freelist trunk page read"
    );

    let mid_delete_integrity = query_single_text(&writer, io.as_ref(), "PRAGMA integrity_check")?;
    // This is the exact Antithesis failure. The in-flight delete may still
    // expose page 53 as unreferenced, but the freelist header must not be
    // advanced before page 53 is linked into the freelist trunk.
    assert_ne!(
        mid_delete_integrity,
        "*** in database main ***\nFreelist: size is 46 but should be 47\nPage 53: never used",
        "pager exposed the exact Antithesis freelist corruption"
    );

    update
        .take_io_completions()
        .expect("DELETE should expose the delayed read completion")
        .wait(io.as_ref())?;
    writer.execute("ROLLBACK TO sp_96")?;
    drop(update);
    writer.execute("RELEASE sp_96")?;
    writer.execute("COMMIT")?;

    assert_eq!(
        query_single_text(&writer, io.as_ref(), "PRAGMA integrity_check")?,
        "ok"
    );
    Ok(())
}
