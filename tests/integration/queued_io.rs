use std::{
    collections::{HashMap, VecDeque},
    io::ErrorKind,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use turso_core::{
    io::{FileId, FileSyncType},
    Buffer, Clock, Completion, CompletionError, File, LimboError, MonotonicInstant, OpenFlags,
    WallClockInstant, IO,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueuedIoOpKind {
    Pread,
    Pwrite,
    Pwritev,
    Sync,
    Truncate,
}

/// Identifies a request by the logical operation that submitted it and its
/// position among that operation's requests. Unlike a queue index or a global
/// counter, it does not change when an unrelated operation's requests are
/// removed from a schedule.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct QueuedIoRequestId {
    pub(crate) owner: String,
    pub(crate) ordinal: u32,
}

impl std::fmt::Display for QueuedIoRequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}", self.owner, self.ordinal)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct QueuedIoEvent {
    pub(crate) path: String,
    pub(crate) kind: QueuedIoOpKind,
    pub(crate) id: QueuedIoRequestId,
    /// Submission order across all owners. For diagnostics only.
    pub(crate) serial: u64,
    /// Byte offset for reads and writes; the new length for truncate; 0 for sync.
    pub(crate) offset: u64,
    /// Bytes requested by a read or carried by a write; 0 for sync and truncate.
    pub(crate) len: usize,
}

/// Who made a queued request run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueuedIoCause {
    /// The test chose this request with `step_at`, `step_one` or `step_request`.
    Selected,
    /// Engine code waited for I/O (`IO::step` or `drain_completions`), which ran
    /// the request at the front of the queue.
    Pump,
    /// The test ran every remaining request with `drain_all`.
    Drain,
}

#[derive(Clone, Debug)]
pub(crate) struct QueuedIoRecord {
    pub(crate) event: QueuedIoEvent,
    pub(crate) cause: QueuedIoCause,
    /// The owner whose code was running when the request ran.
    pub(crate) running_owner: String,
    pub(crate) injected_error: bool,
    /// The completion was aborted before the request ran. The underlying action
    /// still runs in that case; see `QueuedIo::cancel`.
    pub(crate) aborted_before_dispatch: bool,
    completion: Completion,
}

impl QueuedIoRecord {
    pub(crate) fn finished(&self) -> bool {
        self.completion.finished()
    }

    pub(crate) fn succeeded(&self) -> bool {
        self.completion.succeeded()
    }
}

struct QueuedIoOp {
    event: QueuedIoEvent,
    completion: Completion,
    action: Box<dyn FnOnce() -> turso_core::Result<()> + Send>,
    /// The request was armed to fail when submitted (`fault_after`,
    /// `fail_next_request`).
    armed_fault: bool,
}

#[derive(Debug)]
struct QueuedIoFault {
    path_suffix: String,
    kind: QueuedIoOpKind,
    allowed_successes: usize,
    seen: usize,
}

struct QueuedIoState {
    pending: Mutex<VecDeque<QueuedIoOp>>,
    underlying_pending: Mutex<Vec<Completion>>,
    records: Mutex<Vec<QueuedIoRecord>>,
    fault: Mutex<Option<QueuedIoFault>>,
    owner: Mutex<String>,
    ordinals: Mutex<HashMap<String, u32>>,
    serial: AtomicU64,
    stall: Mutex<StallDetector>,
    fail_next: Mutex<Option<QueuedIoOpKind>>,
}

#[derive(Default)]
struct StallDetector {
    limit: Option<u64>,
    empty_steps: u64,
}

impl QueuedIoState {
    fn new() -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
            underlying_pending: Mutex::new(Vec::new()),
            records: Mutex::new(Vec::new()),
            fault: Mutex::new(None),
            owner: Mutex::new("unowned".to_string()),
            ordinals: Mutex::new(HashMap::new()),
            serial: AtomicU64::new(0),
            stall: Mutex::new(StallDetector::default()),
            fail_next: Mutex::new(None),
        }
    }
}

pub(crate) struct QueuedIo {
    inner: Arc<dyn IO>,
    state: Arc<QueuedIoState>,
}

impl QueuedIo {
    pub(crate) fn new() -> Self {
        Self::new_with_io(Arc::new(turso_core::MemoryIO::new()))
    }

    pub(crate) fn new_with_io(inner: Arc<dyn IO>) -> Self {
        Self {
            inner,
            state: Arc::new(QueuedIoState::new()),
        }
    }

    pub(crate) fn step_one(&self) -> turso_core::Result<Option<QueuedIoEvent>> {
        self.step_at(0, false)
    }

    pub(crate) fn pending_events(&self) -> Vec<QueuedIoEvent> {
        self.state
            .pending
            .lock()
            .unwrap()
            .iter()
            .map(|operation| operation.event.clone())
            .collect()
    }

    pub(crate) fn records(&self) -> Vec<QueuedIoRecord> {
        self.state.records.lock().unwrap().clone()
    }

    pub(crate) fn step_at(
        &self,
        index: usize,
        fail: bool,
    ) -> turso_core::Result<Option<QueuedIoEvent>> {
        self.dispatch(index, fail, QueuedIoCause::Selected)
    }

    /// Runs the queued request with this identity, or returns `None` if no such
    /// request is waiting. Never runs a different request in its place.
    pub(crate) fn step_request(
        &self,
        id: &QueuedIoRequestId,
        fail: bool,
    ) -> turso_core::Result<Option<QueuedIoEvent>> {
        let index = self
            .state
            .pending
            .lock()
            .unwrap()
            .iter()
            .position(|operation| &operation.event.id == id);
        match index {
            Some(index) => self.dispatch(index, fail, QueuedIoCause::Selected),
            None => Ok(None),
        }
    }

    /// Runs every queued request, including ones submitted while draining.
    pub(crate) fn drain_all(&self) -> turso_core::Result<usize> {
        let mut count = 0;
        loop {
            if self.dispatch(0, false, QueuedIoCause::Drain)?.is_some() {
                count += 1;
                continue;
            }
            if self.has_underlying_completions() {
                self.step_underlying()?;
                count += 1;
                continue;
            }
            break;
        }
        Ok(count)
    }

    /// Whether a dispatched request is still waiting for the wrapped backend
    /// to deliver its completion.
    pub(crate) fn has_underlying_completions(&self) -> bool {
        let mut pending = self.state.underlying_pending.lock().unwrap();
        pending.retain(|completion| !completion.finished());
        !pending.is_empty()
    }

    /// Gives the wrapped backend one chance to deliver completions without
    /// dispatching another queued request.
    pub(crate) fn step_underlying(&self) -> turso_core::Result<()> {
        self.inner.step()?;
        let mut pending = self.state.underlying_pending.lock().unwrap();
        pending.retain(|completion| !completion.finished());
        Ok(())
    }

    /// Sets the owner recorded for requests submitted from now on and returns
    /// the previous owner.
    pub(crate) fn set_owner(&self, owner: impl Into<String>) -> String {
        std::mem::replace(&mut *self.state.owner.lock().unwrap(), owner.into())
    }

    /// Makes `IO::step` fail after `limit` consecutive calls that find no queued
    /// request, so a blocking wait on I/O that will never be queued reports an
    /// error instead of spinning forever. `None` turns the check off.
    pub(crate) fn set_stall_limit(&self, limit: Option<u64>) {
        *self.state.stall.lock().unwrap() = StallDetector {
            limit,
            empty_steps: 0,
        };
    }

    fn dispatch(
        &self,
        index: usize,
        fail: bool,
        cause: QueuedIoCause,
    ) -> turso_core::Result<Option<QueuedIoEvent>> {
        let Some(op) = self.state.pending.lock().unwrap().remove(index) else {
            return Ok(None);
        };
        self.state.stall.lock().unwrap().empty_steps = 0;
        let event = op.event.clone();
        let aborted_before_dispatch =
            matches!(op.completion.get_error(), Some(CompletionError::Aborted));
        // Requests submitted by this request's callbacks belong to its owner.
        let running_owner = self.set_owner(event.id.owner.clone());
        let result = if fail {
            op.completion
                .error(CompletionError::IOError(ErrorKind::Other, "queued_io"));
            Ok(())
        } else {
            (op.action)()
        };
        if result.is_ok() && !op.completion.finished() {
            self.state
                .underlying_pending
                .lock()
                .unwrap()
                .push(op.completion.clone());
        }
        self.set_owner(running_owner.clone());
        self.state.records.lock().unwrap().push(QueuedIoRecord {
            event: event.clone(),
            cause,
            running_owner,
            injected_error: fail || op.armed_fault,
            aborted_before_dispatch,
            completion: op.completion,
        });
        result?;
        Ok(Some(event))
    }

    /// Injects an I/O error after `allowed_successes` matching queued operations.
    ///
    /// REINDEX rollback tests use this to fail the destructive refill path at a
    /// deterministic write boundary while still exercising the normal queued I/O
    /// completion machinery.
    pub(crate) fn fault_after(
        &self,
        path_suffix: impl Into<String>,
        kind: QueuedIoOpKind,
        allowed_successes: usize,
    ) {
        *self.state.fault.lock().unwrap() = Some(QueuedIoFault {
            path_suffix: path_suffix.into(),
            kind,
            allowed_successes,
            seen: 0,
        });
    }

    /// Makes the next submitted request of this kind, on any file, fail with an
    /// injected error when it runs. Only one request fails.
    pub(crate) fn fail_next_request(&self, kind: QueuedIoOpKind) {
        *self.state.fail_next.lock().unwrap() = Some(kind);
    }

    /// Removes the active queued-I/O fault so cleanup and integrity checks can run normally.
    pub(crate) fn clear_fault(&self) {
        *self.state.fault.lock().unwrap() = None;
        *self.state.fail_next.lock().unwrap() = None;
    }
}

impl Clock for QueuedIo {
    fn current_time_monotonic(&self) -> MonotonicInstant {
        self.inner.current_time_monotonic()
    }

    fn current_time_wall_clock(&self) -> WallClockInstant {
        self.inner.current_time_wall_clock()
    }
}

impl IO for QueuedIo {
    fn open_file(
        &self,
        path: &str,
        flags: OpenFlags,
        direct: bool,
    ) -> turso_core::Result<Arc<dyn File>> {
        let inner = self.inner.open_file(path, flags, direct)?;
        Ok(Arc::new(QueuedFile {
            path: path.to_string(),
            inner,
            state: self.state.clone(),
        }))
    }

    fn remove_file(&self, path: &str) -> turso_core::Result<()> {
        self.inner.remove_file(path)
    }

    fn step(&self) -> turso_core::Result<()> {
        if self.dispatch(0, false, QueuedIoCause::Pump)?.is_none() {
            if self.has_underlying_completions() {
                self.step_underlying()?;
                self.state.stall.lock().unwrap().empty_steps = 0;
                return Ok(());
            }
            let mut stall = self.state.stall.lock().unwrap();
            stall.empty_steps += 1;
            if stall.limit.is_some_and(|limit| stall.empty_steps > limit) {
                stall.empty_steps = 0;
                return Err(LimboError::InternalError(
                    "queued_io: blocking wait with no queued I/O".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn drain_completions(&self, completions: &[Completion]) -> turso_core::Result<()> {
        while completions.iter().any(|c| !c.finished()) {
            if self.dispatch(0, false, QueuedIoCause::Pump)?.is_none() {
                if self.has_underlying_completions() {
                    self.step_underlying()?;
                } else {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Aborts the completions but leaves their queued actions in place: a
    /// cancelled request that has not run yet still performs its read or write
    /// when it is dispatched.
    fn cancel(&self, completions: &[Completion]) -> turso_core::Result<()> {
        for completion in completions {
            completion.abort();
        }
        Ok(())
    }

    fn file_id(&self, path: &str) -> turso_core::Result<FileId> {
        self.inner.file_id(path)
    }

    fn fill_bytes(&self, dest: &mut [u8]) {
        self.inner.fill_bytes(dest);
    }

    fn generate_random_number(&self) -> i64 {
        self.inner.generate_random_number()
    }
}

struct QueuedFile {
    path: String,
    inner: Arc<dyn File>,
    state: Arc<QueuedIoState>,
}

impl QueuedFile {
    fn enqueue(
        &self,
        kind: QueuedIoOpKind,
        offset: u64,
        len: usize,
        completion: Completion,
        action: impl FnOnce() -> turso_core::Result<()> + Send + 'static,
    ) -> turso_core::Result<Completion> {
        let owner = self.state.owner.lock().unwrap().clone();
        let ordinal = {
            let mut ordinals = self.state.ordinals.lock().unwrap();
            let next = ordinals.entry(owner.clone()).or_insert(0);
            *next += 1;
            *next - 1
        };
        let event = QueuedIoEvent {
            path: self.path.clone(),
            kind,
            id: QueuedIoRequestId { owner, ordinal },
            serial: self.state.serial.fetch_add(1, Ordering::SeqCst),
            offset,
            len,
        };
        let fail_next = {
            let mut next = self.state.fail_next.lock().unwrap();
            if *next == Some(kind) {
                *next = None;
                true
            } else {
                false
            }
        };
        let fault_this_op = fail_next || {
            let mut fault = self.state.fault.lock().unwrap();
            if let Some(fault) = fault.as_mut() {
                if self.path.ends_with(&fault.path_suffix) && kind == fault.kind {
                    fault.seen += 1;
                    fault.seen > fault.allowed_successes
                } else {
                    false
                }
            } else {
                false
            }
        };

        let queued_completion = completion.clone();
        let queued_action: Box<dyn FnOnce() -> turso_core::Result<()> + Send> = if fault_this_op {
            Box::new(move || {
                queued_completion.error(CompletionError::IOError(ErrorKind::Other, "queued_io"));
                Ok(())
            })
        } else {
            Box::new(action)
        };

        self.state.pending.lock().unwrap().push_back(QueuedIoOp {
            event,
            completion: completion.clone(),
            action: queued_action,
            armed_fault: fault_this_op,
        });
        Ok(completion)
    }
}

impl File for QueuedFile {
    fn lock_file(&self, exclusive: bool) -> turso_core::Result<()> {
        self.inner.lock_file(exclusive)
    }

    fn unlock_file(&self) -> turso_core::Result<()> {
        self.inner.unlock_file()
    }

    fn pread(&self, pos: u64, completion: Completion) -> turso_core::Result<Completion> {
        let inner = self.inner.clone();
        let c = completion.clone();
        let len = completion.as_read().buf().len();
        self.enqueue(QueuedIoOpKind::Pread, pos, len, completion, move || {
            drop(inner.pread(pos, c)?);
            Ok(())
        })
    }

    fn pwrite(
        &self,
        pos: u64,
        buffer: Arc<Buffer>,
        completion: Completion,
    ) -> turso_core::Result<Completion> {
        let inner = self.inner.clone();
        let c = completion.clone();
        let len = buffer.len();
        self.enqueue(QueuedIoOpKind::Pwrite, pos, len, completion, move || {
            drop(inner.pwrite(pos, buffer, c)?);
            Ok(())
        })
    }

    fn sync(
        &self,
        completion: Completion,
        sync_type: FileSyncType,
    ) -> turso_core::Result<Completion> {
        let inner = self.inner.clone();
        let c = completion.clone();
        self.enqueue(QueuedIoOpKind::Sync, 0, 0, completion, move || {
            drop(inner.sync(c, sync_type)?);
            Ok(())
        })
    }

    fn pwritev(
        &self,
        pos: u64,
        buffers: Vec<Arc<Buffer>>,
        completion: Completion,
    ) -> turso_core::Result<Completion> {
        let inner = self.inner.clone();
        let c = completion.clone();
        let len = buffers.iter().map(|buffer| buffer.len()).sum();
        self.enqueue(QueuedIoOpKind::Pwritev, pos, len, completion, move || {
            drop(inner.pwritev(pos, buffers, c)?);
            Ok(())
        })
    }

    fn size(&self) -> turso_core::Result<u64> {
        self.inner.size()
    }

    fn truncate(&self, len: u64, completion: Completion) -> turso_core::Result<Completion> {
        let inner = self.inner.clone();
        let c = completion.clone();
        self.enqueue(QueuedIoOpKind::Truncate, len, 0, completion, move || {
            drop(inner.truncate(len, c)?);
            Ok(())
        })
    }
}
