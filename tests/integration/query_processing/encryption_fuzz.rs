//! Seeded, model-based fuzzing of connection encryption.
//!
//! Each case is a typed scenario plus a schedule of decisions: which actor to
//! advance, which queued request to complete or fail, which operation to drop.
//! A case runs in a child process so a hang inside a blocking API call is
//! caught by a watchdog. Completed passes and structured failures have version-2
//! artifacts for exact replay (`ENCRYPTION_FUZZ_REPLAY`); invariant failures are
//! shrunk. Timeouts retain partial journals and logs without replay or shrinking.

use super::*;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde_json::{json, Value as Json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs,
    hash::Hasher,
    io::Read,
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    time::{Duration, Instant},
};
use turso_core::{CompletionError, Statement, StepResult};

#[path = "encryption_scenarios.rs"]
mod scenarios;

const VERSION: u64 = 2;
const MODEL_VERSION: u64 = 4;
const GENERATED_DECISIONS: usize = 128;
const MAX_DECISIONS: usize = 1024;
const MAX_UNCHANGED_RETRIES: usize = 64;
const STALL_LIMIT: u64 = 10_000;

pub(super) const CIPHERS: [CipherMode; 8] = [
    CipherMode::Aes128Gcm,
    CipherMode::Aes256Gcm,
    CipherMode::Aegis128L,
    CipherMode::Aegis128X2,
    CipherMode::Aegis128X4,
    CipherMode::Aegis256,
    CipherMode::Aegis256X2,
    CipherMode::Aegis256X4,
];
const PAGE_SIZES: [u32; 8] = [512, 1024, 2048, 4096, 8192, 16384, 32768, 65536];

thread_local! {
    static CURRENT_OPERATION: RefCell<String> = const { RefCell::new(String::new()) };
    static COVERAGE: RefCell<BTreeMap<String, u64>> = const { RefCell::new(BTreeMap::new()) };
}

fn set_operation(operation: impl Into<String>) {
    CURRENT_OPERATION.with(|current| *current.borrow_mut() = operation.into());
}

fn cover(name: impl Into<String>) {
    COVERAGE.with(|coverage| *coverage.borrow_mut().entry(name.into()).or_default() += 1);
}

fn take_coverage() -> BTreeMap<String, u64> {
    COVERAGE.with(|coverage| std::mem::take(&mut *coverage.borrow_mut()))
}

/// A shrink candidate's prefix asked for an action the run did not offer.
#[derive(Clone, Debug)]
struct InvalidCandidate(String);

// ---------------------------------------------------------------------------
// Parent process: run a worker, then shrink and verify any failure.
// ---------------------------------------------------------------------------

#[test]
fn seeded_encryption_state_machine() {
    if std::env::var_os("ENCRYPTION_FUZZ_CHILD").is_some() {
        child_main();
        return;
    }
    let (directory, keep) = artifact_directory();
    let deadline = Instant::now()
        + Duration::from_secs(env_number("ENCRYPTION_FUZZ_CAMPAIGN_TIMEOUT_SECS", 900));
    let build = build_identity();
    if let Some(replay) = std::env::var_os("ENCRYPTION_FUZZ_REPLAY").map(PathBuf::from) {
        let outcome = run_worker(&directory, WorkerMode::Strict(&replay), &build, deadline);
        let result = finished_result(&directory, outcome);
        let terminal = Terminal::from_json(&result["terminal"]);
        let recorded = Terminal::from_json(&result["recorded_terminal"]);
        assert!(
            terminal.same_failure(&recorded),
            "replay of {} did not reproduce: recorded {recorded:?}, actual {terminal:?}\nLog: {}",
            replay.display(),
            directory.join("worker.log").display()
        );
        assert!(
            matches!(terminal, Terminal::Pass),
            "replay reproduced the recorded failure: {terminal:?}"
        );
        eprintln!("encryption replay passed: {}", replay.display());
        return;
    }
    let outcome = run_worker(&directory, WorkerMode::Campaign, &build, deadline);
    let result = finished_result(&directory, outcome);
    let terminal = Terminal::from_json(&result["terminal"]);
    if matches!(terminal, Terminal::Pass) {
        eprintln!("{}", result["report"].as_str().unwrap_or_default());
        if keep {
            eprintln!("coverage: {}", directory.join("result.json").display());
        } else {
            let _ = fs::remove_dir_all(&directory);
        }
        return;
    }
    let failure = directory.join("original.json");
    let journal = directory.join("latest.json");
    assert!(
        journal.exists(),
        "encryption fuzz worker failed before creating a scenario: {terminal:?}\nResult: {}\nLog: {}",
        directory.join("result.json").display(),
        directory.join("worker.log").display()
    );
    fs::copy(journal, &failure).unwrap();
    let summary = shrink(&directory, &failure, &build, deadline);
    let minimized = directory.join("minimized.json");
    let replay = if minimized.exists() {
        &minimized
    } else {
        &failure
    };
    panic!(
        "encryption fuzz failure: {terminal:?}\n{summary}\nOriginal: {}\nReplay: ENCRYPTION_FUZZ_REPLAY={} cargo test --offline -p core_tester --test integration_tests --features io_memory_yield seeded_encryption_state_machine -- --nocapture --test-threads=1\nLog: {}",
        failure.display(),
        replay.display(),
        directory.join("worker.log").display()
    );
}

/// Returns the worker's result, or panics with the preserved log when the
/// worker timed out or exited without writing one.
fn finished_result(directory: &Path, outcome: WorkerOutcome) -> Json {
    match outcome {
        WorkerOutcome::Finished(result) => result,
        WorkerOutcome::Timeout(reason) => {
            let journal = directory.join("latest.json");
            if journal.exists() {
                fs::copy(&journal, directory.join("timeout.json")).unwrap();
            }
            panic!(
                "encryption fuzz worker timed out: {reason}\nLast case: {}\nLog: {}",
                directory.join("timeout.json").display(),
                directory.join("worker.log").display()
            );
        }
        WorkerOutcome::Crashed(status) => panic!(
            "encryption fuzz worker exited without a result ({status})\nLast case: {}\nLog: {}",
            directory.join("latest.json").display(),
            directory.join("worker.log").display()
        ),
    }
}

fn artifact_directory() -> (PathBuf, bool) {
    if let Some(root) = std::env::var_os("ENCRYPTION_FUZZ_ARTIFACTS") {
        let directory = PathBuf::from(root).join(format!(
            "run-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        fs::create_dir_all(&directory).unwrap();
        return (directory, true);
    }
    let directory = tempfile::Builder::new()
        .prefix("turso-encryption-fuzz-")
        .tempdir()
        .unwrap()
        .keep();
    (directory, false)
}

fn build_identity() -> Json {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let git = |args: &[&str]| {
        Process::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let diff = git(&["diff", "HEAD"]);
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in diff.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    json!({
        "commit": git(&["rev-parse", "HEAD"]),
        "dirty_files": git(&["status", "--porcelain"]).lines().count(),
        "diff_fnv64": format!("{hash:016x}"),
        "executable": executable_identity(),
        "configuration": {
            "architecture": std::env::consts::ARCH,
            "os": std::env::consts::OS,
            "family": std::env::consts::FAMILY,
            "debug_assertions": cfg!(debug_assertions),
            "features": {
                "checksum": cfg!(feature = "checksum"),
                "test_helper": cfg!(feature = "test_helper"),
                "fts": cfg!(feature = "fts"),
                "io_memory_yield": cfg!(feature = "io_memory_yield"),
                "io_uring": cfg!(feature = "io_uring"),
                "win_iocp": cfg!(feature = "win_iocp"),
                "pure-rust-crypto": cfg!(feature = "pure-rust-crypto"),
            },
        },
        "backend": "QueuedIo over MemoryIO",
        "model": MODEL_VERSION,
    })
}

fn executable_identity() -> Json {
    let mut file = fs::File::open(std::env::current_exe().unwrap()).unwrap();
    let mut hash = twox_hash::XxHash64::with_seed(0);
    let mut buffer = [0; 64 * 1024];
    let mut bytes = 0u64;
    loop {
        let read = file.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        hash.write(&buffer[..read]);
        bytes += read as u64;
    }
    json!({ "bytes": bytes, "xxhash64": format!("{:016x}", hash.finish()) })
}

fn compatible_build(recorded: &Json, actual: &Json) -> Result<(), String> {
    for field in ["model", "executable", "configuration", "backend"] {
        let expected = recorded.get(field).filter(|value| !value.is_null());
        if expected != actual.get(field) {
            return Err(format!(
                "incompatible {field}: recorded {expected:?}, current {:?}; regenerate the case from its seed with this executable",
                actual.get(field)
            ));
        }
    }
    Ok(())
}

enum WorkerMode<'a> {
    Campaign,
    Strict(&'a Path),
    Prefix(&'a Path),
}

enum WorkerOutcome {
    Finished(Json),
    Timeout(String),
    Crashed(String),
}

fn run_worker(
    directory: &Path,
    mode: WorkerMode<'_>,
    build: &Json,
    deadline: Instant,
) -> WorkerOutcome {
    let timeout = Duration::from_secs(env_number("ENCRYPTION_FUZZ_TIMEOUT_SECS", 30));
    run_worker_with(directory, mode, build, deadline, timeout, &[])
}

fn run_worker_with(
    directory: &Path,
    mode: WorkerMode<'_>,
    build: &Json,
    deadline: Instant,
    timeout: Duration,
    environment: &[(&str, &str)],
) -> WorkerOutcome {
    let log = fs::File::create(directory.join("worker.log")).unwrap();
    let test = format!(
        "{}::seeded_encryption_state_machine",
        module_path!().split_once("::").unwrap().1
    );
    let mut command = Process::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", &test, "--nocapture", "--test-threads=1"])
        .env("ENCRYPTION_FUZZ_CHILD", "1")
        .env("ENCRYPTION_FUZZ_DIRECTORY", directory)
        .env("ENCRYPTION_FUZZ_BUILD", build.to_string())
        .env_remove("ENCRYPTION_FUZZ_REPLAY")
        .envs(environment.iter().copied())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log));
    match mode {
        WorkerMode::Campaign => command.env("ENCRYPTION_FUZZ_MODE", "campaign"),
        WorkerMode::Strict(input) => command
            .env("ENCRYPTION_FUZZ_MODE", "strict")
            .env("ENCRYPTION_FUZZ_INPUT", input),
        WorkerMode::Prefix(input) => command
            .env("ENCRYPTION_FUZZ_MODE", "prefix")
            .env("ENCRYPTION_FUZZ_INPUT", input),
    };
    let _ = fs::remove_file(directory.join("result.json"));
    let mut child = command.spawn().unwrap();
    let mut last_progress = Instant::now();
    let mut modified = None;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            if !status.success() {
                return WorkerOutcome::Crashed(status.to_string());
            }
            let result = fs::read(directory.join("result.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            return match result {
                Some(result) => WorkerOutcome::Finished(result),
                None => WorkerOutcome::Crashed(status.to_string()),
            };
        }
        let update = fs::metadata(directory.join("latest.json"))
            .and_then(|metadata| metadata.modified())
            .ok();
        if update != modified {
            modified = update;
            last_progress = Instant::now();
        }
        let reason = if last_progress.elapsed() > timeout {
            Some(format!("no progress for {timeout:?}"))
        } else if Instant::now() > deadline {
            Some("total deadline reached".to_string())
        } else {
            None
        };
        if let Some(reason) = reason {
            child.kill().unwrap();
            child.wait().unwrap();
            return WorkerOutcome::Timeout(reason);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ---------------------------------------------------------------------------
// Shrinking.
// ---------------------------------------------------------------------------

/// Reduces a failing artifact while keeping the same structured failure. Each
/// candidate first runs in prefix mode, which may extend an edited schedule
/// with a deterministic suffix. A candidate is accepted only if a strict replay
/// of its full recorded trace in a fresh worker reproduces the failure.
fn shrink(directory: &Path, failure: &Path, build: &Json, deadline: Instant) -> String {
    let original = load_case(failure);
    let target = original.terminal.clone().unwrap();
    let shrink_deadline = (Instant::now() + Duration::from_secs(120)).min(deadline);
    let mut best = original.clone();
    let mut attempts = 0;
    let mut accepted = 0;
    if !matches!(target, Terminal::Violation { .. }) {
        return format!("not shrunk: {target:?} is not an invariant violation");
    }
    let try_candidate = |candidate: Case, attempts: &mut usize| -> Option<Case> {
        if *attempts >= 48 || Instant::now() > shrink_deadline {
            return None;
        }
        *attempts += 1;
        let attempt = directory.join(format!("shrink-{attempts}"));
        fs::create_dir_all(&attempt).unwrap();
        let input = attempt.join("candidate.json");
        write_atomic(&input, &candidate.to_json());
        let WorkerOutcome::Finished(result) =
            run_worker(&attempt, WorkerMode::Prefix(&input), build, shrink_deadline)
        else {
            return None;
        };
        if !Terminal::from_json(&result["terminal"]).same_failure(&target) {
            return None;
        }
        let actual = attempt.join("actual.json");
        fs::copy(attempt.join("latest.json"), &actual).unwrap();
        let verify = attempt.join("verify");
        fs::create_dir_all(&verify).unwrap();
        let WorkerOutcome::Finished(result) =
            run_worker(&verify, WorkerMode::Strict(&actual), build, shrink_deadline)
        else {
            return None;
        };
        Terminal::from_json(&result["terminal"])
            .same_failure(&target)
            .then(|| load_case(&actual))
    };
    // Remove whole actors, then single commands, then chunks of decisions.
    let mut progress = true;
    while progress {
        progress = false;
        for actor in (0..best.actors()).rev() {
            if best.actors() <= 1 {
                break;
            }
            if let Some(smaller) = best.without_actor(actor) {
                if let Some(found) = try_candidate(smaller, &mut attempts) {
                    best = found;
                    accepted += 1;
                    progress = true;
                }
            }
        }
        for actor in 0..best.actors() {
            let mut command = 0;
            while command < best.commands(actor) {
                match best
                    .without_command(actor, command)
                    .and_then(|smaller| try_candidate(smaller, &mut attempts))
                {
                    Some(found) => {
                        best = found;
                        accepted += 1;
                        progress = true;
                    }
                    None => command += 1,
                }
            }
        }
    }
    let mut chunk = best.decisions.len() / 2;
    while chunk > 0 {
        let mut start = 0;
        while start + chunk <= best.decisions.len() {
            let mut smaller = best.clone();
            smaller.decisions.drain(start..start + chunk);
            match try_candidate(smaller, &mut attempts) {
                Some(found) if found.decisions.len() < best.decisions.len() => {
                    best = found;
                    accepted += 1;
                }
                _ => start += chunk,
            }
        }
        chunk /= 2;
    }
    let candidate = directory.join("unverified-candidate.json");
    write_atomic(&candidate, &best.to_json());
    let mut verified = 0;
    for run in 0..2 {
        if Instant::now() >= shrink_deadline {
            break;
        }
        let verify = directory.join(format!("final-verify-{run}"));
        fs::create_dir_all(&verify).unwrap();
        if let WorkerOutcome::Finished(result) = run_worker(
            &verify,
            WorkerMode::Strict(&candidate),
            build,
            shrink_deadline,
        ) {
            if Terminal::from_json(&result["terminal"]).same_failure(&target) {
                verified += 1;
            }
        }
    }
    let verification = if verified == 2 {
        fs::rename(&candidate, directory.join("minimized.json")).unwrap();
        "minimized artifact verified".to_string()
    } else {
        format!(
            "candidate not verified; retained at {}; replay the original",
            candidate.display()
        )
    };
    let summary = format!(
        "shrink: {attempts} candidates, {accepted} accepted; decisions {} -> {}; commands {} -> {}; {verification} ({verified} of 2 fresh strict replays)",
        original.decisions.len(),
        best.decisions.len(),
        original.total_commands(),
        best.total_commands()
    );
    fs::write(directory.join("shrink.txt"), &summary).unwrap();
    summary
}

// ---------------------------------------------------------------------------
// Child process.
// ---------------------------------------------------------------------------

fn child_main() {
    let directory = PathBuf::from(std::env::var_os("ENCRYPTION_FUZZ_DIRECTORY").unwrap());
    if let Ok(selftest) = std::env::var("ENCRYPTION_FUZZ_SELFTEST") {
        match selftest.as_str() {
            "exit" => std::process::exit(3),
            "result_then_exit" => {
                write_atomic(
                    &directory.join("result.json"),
                    &json!({ "terminal": Terminal::Pass.to_json() }),
                );
                std::process::exit(3);
            }
            "hang" => loop {
                std::thread::sleep(Duration::from_secs(1));
            },
            _ => {}
        }
    }
    let requested_build: Json = std::env::var("ENCRYPTION_FUZZ_BUILD")
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Json::Null);
    let build = build_identity();
    if let Err(message) = compatible_build(&requested_build, &build) {
        write_atomic(
            &directory.join("result.json"),
            &json!({ "terminal": Terminal::HarnessFailure(message).to_json() }),
        );
        return;
    }
    let mode = std::env::var("ENCRYPTION_FUZZ_MODE").unwrap_or_else(|_| "campaign".into());
    let result = match mode.as_str() {
        "campaign" => campaign(&directory, &build),
        "strict" | "prefix" => {
            let input = PathBuf::from(std::env::var_os("ENCRYPTION_FUZZ_INPUT").unwrap());
            let mut case = match read_case(&input) {
                Ok(case) => case,
                Err(message) => {
                    write_atomic(
                        &directory.join("result.json"),
                        &json!({ "terminal": Terminal::ReplayDiverged(message).to_json() }),
                    );
                    return;
                }
            };
            let recorded = case.terminal.clone().unwrap_or(Terminal::Pass);
            let run_mode = if mode == "strict" {
                RunMode::Strict
            } else {
                RunMode::Prefix
            };
            let compatibility = compatible_build(&case.build, &build).and_then(|()| {
                if mode == "strict" && case.terminal.is_none() {
                    Err("strict replay requires a recorded terminal result".into())
                } else {
                    Ok(())
                }
            });
            let terminal = match compatibility {
                Ok(()) => run_case(&mut case, run_mode, &directory),
                Err(message) => Terminal::ReplayDiverged(message),
            };
            json!({
                "terminal": terminal.to_json(),
                "recorded_terminal": recorded.to_json(),
                "coverage": take_coverage(),
            })
        }
        other => panic!("unknown worker mode {other}"),
    };
    write_atomic(&directory.join("result.json"), &result);
}

fn campaign(directory: &Path, build: &Json) -> Json {
    let (mut rng, seed) = crate::common::rng_from_time_or_env();
    eprintln!("encryption fuzz SEED={seed}");
    let cases = env_number("ENCRYPTION_FUZZ_CASES", 48);
    assert!(cases > 0);
    let started = Instant::now();
    let mut executions = 0;
    let mut decisions = 0;
    let mut injected = 0;
    let mut coverage: BTreeMap<String, u64> = BTreeMap::new();
    for index in 0..cases {
        let base = Case::generate(&mut rng, seed, index, build);
        for faults in [false, true] {
            let mut case = base.clone();
            case.faults = faults;
            let terminal = run_case(&mut case, RunMode::Random, directory);
            executions += 1;
            decisions += case.decisions.len();
            injected += case
                .decisions
                .iter()
                .filter(|action| matches!(action, Action::Fail(_)))
                .count();
            for (name, count) in take_coverage() {
                *coverage.entry(name).or_default() += count;
            }
            if !matches!(terminal, Terminal::Pass) {
                return json!({
                    "terminal": terminal.to_json(),
                    "case": case.to_json(),
                    "coverage": coverage,
                });
            }
        }
    }
    let report = format!(
        "encryption fuzz SEED={seed}: {executions} scenario executions, {decisions} decisions, {injected} injected failures, {:?}",
        started.elapsed()
    );
    json!({ "terminal": Terminal::Pass.to_json(), "report": report, "coverage": coverage })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunMode {
    Random,
    Strict,
    Prefix,
}

/// Executes one case and records its decisions, outcomes and terminal result
/// in `case` and in the journal file.
fn run_case(case: &mut Case, mode: RunMode, directory: &Path) -> Terminal {
    let journal = Journal {
        path: Some(directory.join("latest.json")),
        case: case.clone(),
    };
    let chooser: Box<dyn Chooser> = match mode {
        RunMode::Random => Box::new(RandomChooser {
            rng: ChaCha8Rng::seed_from_u64(case.seeds.schedule),
            journal,
        }),
        RunMode::Strict => Box::new(StrictChooser {
            decisions: case.decisions.clone(),
            outcomes: case.outcomes.clone(),
            compared: 0,
            journal,
        }),
        RunMode::Prefix => Box::new(PrefixChooser {
            prefix: case.decisions.clone(),
            journal,
        }),
    };
    let (decisions, outcomes, mut terminal) = execute(case, chooser);
    if mode == RunMode::Strict
        && matches!(terminal, Terminal::Pass | Terminal::Violation { .. })
        && !case
            .terminal
            .as_ref()
            .is_some_and(|recorded| terminal.same_failure(recorded))
    {
        terminal = Terminal::ReplayDiverged(format!(
            "terminal result differs: recorded {:?}, actual {terminal:?}",
            case.terminal
        ));
    }
    case.decisions = decisions;
    case.outcomes = outcomes;
    case.terminal = Some(terminal.clone());
    write_atomic(&directory.join("latest.json"), &case.to_json());
    terminal
}

/// Runs a case under the given chooser without a worker process. Returns the
/// decisions taken, the observations of each decision, and the result.
fn execute(case: &Case, chooser: Box<dyn Chooser>) -> (Vec<Action>, Vec<Vec<String>>, Terminal) {
    // Paths never appear in observations, so a unique namespace per run keeps
    // concurrent runs apart without affecting replay.
    static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let run = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut trace = Trace {
        allow_failures: case.faults,
        namespace: format!(
            "{}-{}-{}-{}-{run}",
            case.seeds.campaign,
            case.seeds.index,
            case.faults,
            std::process::id()
        ),
        chooser: Some(chooser),
        max_decisions: MAX_DECISIONS,
        generated: GENERATED_DECISIONS,
        ..Trace::default()
    };
    trace.persist();
    set_operation(case.family.name());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &case.family {
        Family::PageOne {
            cipher,
            page_size,
            credential,
        } => {
            page_one_schedule(&mut trace, *cipher, *page_size, *credential);
            cover_page_one(&trace);
        }
        Family::Manager {
            existing,
            mvcc,
            credential,
            cancel_waiter,
            wait_for_bytes,
        } => {
            manager_schedule(
                &mut trace,
                *existing,
                *mvcc,
                *credential,
                *cancel_waiter,
                *wait_for_bytes,
            );
            cover_manager(&trace);
        }
        Family::Lifecycle(spec) => lifecycle(&mut trace, spec),
    }));
    let mut terminal = match result {
        Ok(()) => Terminal::Pass,
        Err(payload) => Terminal::from_panic(payload, trace.actions.len()),
    };
    if matches!(terminal, Terminal::Pass | Terminal::Violation { .. }) {
        let decided = trace.actions.clone();
        let outcomes = trace.outcomes.clone();
        if let Some(chooser) = &mut trace.chooser {
            if let Err(message) = chooser.finish(&decided, &outcomes) {
                terminal = Terminal::ReplayDiverged(message);
            }
        }
    }
    trace.persist();
    (trace.actions.clone(), trace.outcomes.clone(), terminal)
}

fn cover_page_one(trace: &Trace) {
    cover("page_one.executions");
    for observation in trace.outcomes.iter().flatten() {
        if observation.contains("polled while the other actor waited") {
            cover("page_one.overlap");
        }
        if observation == "initial page-one writes: 1" {
            cover("page_one.exactly_one_allocation");
        }
    }
}

fn cover_manager(trace: &Trace) {
    cover("manager.executions");
    for observation in trace.outcomes.iter().flatten() {
        if observation.starts_with("creator paused, database bytes=0") {
            cover("manager.waiter_before_first_bytes");
        } else if observation.starts_with("creator paused, database bytes=") {
            cover("manager.waiter_after_first_bytes");
        }
        if observation == "waiter yielded on the registry owner" {
            cover("manager.explicit_yield");
        }
    }
}

// ---------------------------------------------------------------------------
// Choosing decisions.
// ---------------------------------------------------------------------------

pub(in crate::query_processing) trait Chooser {
    fn choose(&mut self, enabled: &[Action], decided: &[Action]) -> Action;
    /// Called whenever a decision is taken or an observation is added.
    fn record(&mut self, _decided: &[Action], _outcomes: &[Vec<String>]) {}
    /// Saves the run so far before engine code is invoked.
    fn persist(&mut self, _decided: &[Action], _outcomes: &[Vec<String>]) {}
    /// Checks that the run consumed exactly what was recorded.
    fn finish(&mut self, _decided: &[Action], _outcomes: &[Vec<String>]) -> Result<(), String> {
        Ok(())
    }
}

impl Trace {
    pub(super) fn generating(&self) -> bool {
        self.actions.len() < self.generated
    }

    pub(super) fn persist(&mut self) {
        let decided = self.actions.clone();
        let outcomes = self.outcomes.clone();
        if let Some(chooser) = &mut self.chooser {
            chooser.persist(&decided, &outcomes);
        }
    }
}

struct Journal {
    path: Option<PathBuf>,
    case: Case,
}

impl Journal {
    fn write(&mut self, decided: &[Action], outcomes: &[Vec<String>]) {
        self.case.outcomes = outcomes.to_vec();
        self.write_decisions(decided);
    }

    /// Saves a newly chosen decision before engine code runs for it.
    fn write_decisions(&mut self, decided: &[Action]) {
        if let Some(path) = &self.path {
            self.case.decisions = decided.to_vec();
            self.case.terminal = None;
            write_atomic(path, &self.case.to_json());
        }
    }
}

/// Picks the oldest queued request first, then actors in round-robin order.
/// Never injects failures or drops work.
fn fair(enabled: &[Action], decided: &[Action]) -> Action {
    if let Some(action) = enabled
        .iter()
        .find(|action| matches!(action, Action::Complete(_)))
    {
        return action.clone();
    }
    if enabled.contains(&Action::CompleteUnderlying) {
        return Action::CompleteUnderlying;
    }
    let last = decided.iter().rev().find_map(|action| match action {
        Action::Poll(actor) => Some(*actor),
        _ => None,
    });
    let polls: Vec<usize> = enabled
        .iter()
        .filter_map(|action| match action {
            Action::Poll(actor) => Some(*actor),
            _ => None,
        })
        .collect();
    if let Some(actor) = polls
        .iter()
        .find(|actor| last.is_none_or(|last| **actor > last))
        .or_else(|| polls.first())
    {
        return Action::Poll(*actor);
    }
    enabled
        .iter()
        .find(|action| matches!(action, Action::Start(_)))
        .or_else(|| {
            enabled
                .iter()
                .find(|action| matches!(action, Action::Drop(_)))
        })
        .cloned()
        .expect("only failure injections are enabled")
}

struct RandomChooser {
    rng: ChaCha8Rng,
    journal: Journal,
}

impl Chooser for RandomChooser {
    fn choose(&mut self, enabled: &[Action], decided: &[Action]) -> Action {
        let action = if decided.len() < GENERATED_DECISIONS {
            let rng = &mut self.rng;
            let choices: Vec<_> = enabled
                .iter()
                .filter(|action| {
                    !matches!(action, Action::Drop(_) | Action::Fail(_)) || rng.random_ratio(1, 4)
                })
                .cloned()
                .collect();
            if choices.is_empty() {
                fair(enabled, decided)
            } else {
                choices[self.rng.random_range(0..choices.len())].clone()
            }
        } else {
            fair(enabled, decided)
        };
        let mut decided = decided.to_vec();
        decided.push(action.clone());
        self.journal.write_decisions(&decided);
        action
    }

    fn persist(&mut self, decided: &[Action], outcomes: &[Vec<String>]) {
        self.journal.write(decided, outcomes);
    }
}

/// Replays recorded decisions exactly and compares every decision's
/// observations. Never substitutes another action.
struct StrictChooser {
    decisions: Vec<Action>,
    outcomes: Vec<Vec<String>>,
    compared: usize,
    journal: Journal,
}

impl StrictChooser {
    fn compare_finished(&mut self, outcomes: &[Vec<String>], all: bool) -> Result<(), String> {
        let finished = if all {
            outcomes.len()
        } else {
            outcomes.len().saturating_sub(1)
        };
        while self.compared < finished {
            let index = self.compared;
            let expected = self.outcomes.get(index);
            if expected != Some(&outcomes[index]) {
                return Err(format!(
                    "observations after decision {index} differ: recorded {expected:?}, actual {:?}",
                    outcomes[index]
                ));
            }
            self.compared += 1;
        }
        Ok(())
    }
}

impl Chooser for StrictChooser {
    fn choose(&mut self, enabled: &[Action], decided: &[Action]) -> Action {
        let Some(action) = self.decisions.get(decided.len()) else {
            std::panic::panic_any(ReplayDivergence(format!(
                "the recorded schedule ended after {} decisions but the run needs more; enabled: {enabled:?}",
                decided.len()
            )));
        };
        if !enabled.contains(action) {
            std::panic::panic_any(ReplayDivergence(format!(
                "decision {} {action:?} is not enabled; enabled: {enabled:?}",
                decided.len()
            )));
        }
        let mut decided = decided.to_vec();
        decided.push(action.clone());
        self.journal.write_decisions(&decided);
        action.clone()
    }

    fn record(&mut self, _decided: &[Action], outcomes: &[Vec<String>]) {
        if let Err(message) = self.compare_finished(outcomes, false) {
            std::panic::panic_any(ReplayDivergence(message));
        }
    }

    fn persist(&mut self, decided: &[Action], outcomes: &[Vec<String>]) {
        self.journal.write(decided, outcomes);
    }

    fn finish(&mut self, decided: &[Action], outcomes: &[Vec<String>]) -> Result<(), String> {
        if decided.len() != self.decisions.len() {
            return Err(format!(
                "the run finished after {} decisions but {} were recorded",
                decided.len(),
                self.decisions.len()
            ));
        }
        self.compare_finished(outcomes, true)?;
        if outcomes.len() != self.outcomes.len() {
            return Err("the run recorded a different number of observation groups".into());
        }
        Ok(())
    }
}

/// Follows an edited prefix and then continues with the fair policy. Used only
/// to produce shrink candidates, which must then replay strictly.
struct PrefixChooser {
    prefix: Vec<Action>,
    journal: Journal,
}

impl Chooser for PrefixChooser {
    fn choose(&mut self, enabled: &[Action], decided: &[Action]) -> Action {
        let action = match self.prefix.get(decided.len()) {
            Some(action) if enabled.contains(action) => action.clone(),
            Some(action) => std::panic::panic_any(InvalidCandidate(format!(
                "decision {} {action:?} is not enabled",
                decided.len()
            ))),
            None => fair(enabled, decided),
        };
        let mut decided = decided.to_vec();
        decided.push(action.clone());
        self.journal.write_decisions(&decided);
        action
    }

    fn persist(&mut self, decided: &[Action], outcomes: &[Vec<String>]) {
        self.journal.write(decided, outcomes);
    }
}

// ---------------------------------------------------------------------------
// Results.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum Terminal {
    Pass,
    Violation {
        invariant: String,
        operation: String,
        message: String,
        decision: usize,
    },
    ReplayDiverged(String),
    InvalidCandidate(String),
    HarnessFailure(String),
}

impl Terminal {
    fn from_panic(payload: Box<dyn std::any::Any + Send>, decision: usize) -> Self {
        let operation = CURRENT_OPERATION.with(|current| current.borrow().clone());
        if let Some(violation) = payload.downcast_ref::<Violation>() {
            return Terminal::Violation {
                invariant: violation.invariant.to_string(),
                operation,
                message: violation.message.clone(),
                decision,
            };
        }
        if let Some(divergence) = payload.downcast_ref::<ReplayDivergence>() {
            return Terminal::ReplayDiverged(divergence.0.clone());
        }
        if let Some(invalid) = payload.downcast_ref::<InvalidCandidate>() {
            return Terminal::InvalidCandidate(invalid.0.clone());
        }
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("non-string panic");
        Terminal::Violation {
            invariant: "unclassified_panic".to_string(),
            operation,
            message: message.lines().next().unwrap_or(message).to_string(),
            decision,
        }
    }

    /// Whether two results are the same failure: the same invariant raised by
    /// the same kind of operation. Messages, decision numbers and paths may
    /// differ. Panics without an invariant are compared by their first line
    /// with digits removed.
    fn same_failure(&self, other: &Terminal) -> bool {
        match (self, other) {
            (
                Terminal::Violation {
                    invariant: a,
                    operation: oa,
                    message: ma,
                    ..
                },
                Terminal::Violation {
                    invariant: b,
                    operation: ob,
                    message: mb,
                    ..
                },
            ) => {
                let strip = |text: &str| -> String {
                    text.chars().filter(|c| !c.is_ascii_digit()).collect()
                };
                a == b && oa == ob && (a != "unclassified_panic" || strip(ma) == strip(mb))
            }
            (Terminal::Pass, Terminal::Pass) => true,
            _ => false,
        }
    }

    fn to_json(&self) -> Json {
        match self {
            Terminal::Pass => json!({ "kind": "Pass" }),
            Terminal::Violation {
                invariant,
                operation,
                message,
                decision,
            } => json!({
                "kind": "Violation", "invariant": invariant, "operation": operation,
                "message": message, "decision": decision
            }),
            Terminal::ReplayDiverged(message) => {
                json!({ "kind": "ReplayDiverged", "message": message })
            }
            Terminal::InvalidCandidate(message) => {
                json!({ "kind": "InvalidCandidate", "message": message })
            }
            Terminal::HarnessFailure(message) => {
                json!({ "kind": "HarnessFailure", "message": message })
            }
        }
    }

    fn from_json(value: &Json) -> Self {
        let text = |name: &str| value[name].as_str().unwrap_or_default().to_string();
        match value["kind"].as_str() {
            Some("Pass") => Terminal::Pass,
            Some("Violation") => Terminal::Violation {
                invariant: text("invariant"),
                operation: text("operation"),
                message: text("message"),
                decision: value["decision"].as_u64().unwrap_or_default() as usize,
            },
            Some("ReplayDiverged") => Terminal::ReplayDiverged(text("message")),
            Some("InvalidCandidate") => Terminal::InvalidCandidate(text("message")),
            Some("HarnessFailure") => Terminal::HarnessFailure(text("message")),
            _ => Terminal::HarnessFailure(format!("unreadable result: {value}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Cases and artifacts.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Seeds {
    campaign: u64,
    index: u64,
    scenario: u64,
    operations: u64,
    schedule: u64,
}

#[derive(Clone, Debug)]
enum Family {
    PageOne {
        cipher: CipherMode,
        page_size: u32,
        credential: Credential,
    },
    Manager {
        existing: bool,
        mvcc: bool,
        credential: Credential,
        cancel_waiter: bool,
        wait_for_bytes: bool,
    },
    Lifecycle(LifecycleSpec),
}

impl Family {
    fn name(&self) -> &'static str {
        match self {
            Family::PageOne { .. } => "page_one",
            Family::Manager { .. } => "manager",
            Family::Lifecycle(_) => "lifecycle",
        }
    }
}

#[derive(Clone, Debug)]
struct Case {
    build: Json,
    seeds: Seeds,
    faults: bool,
    family: Family,
    decisions: Vec<Action>,
    outcomes: Vec<Vec<String>>,
    terminal: Option<Terminal>,
}

const CREDENTIALS: [Credential; 4] = [
    Credential::Missing,
    Credential::Wrong,
    Credential::WrongCipher,
    Credential::Correct,
];

impl Case {
    fn generate(rng: &mut ChaCha8Rng, campaign: u64, index: u64, build: &Json) -> Case {
        let seeds = Seeds {
            campaign,
            index,
            scenario: rng.random(),
            operations: rng.random(),
            schedule: rng.random(),
        };
        let mut scenario = ChaCha8Rng::seed_from_u64(seeds.scenario);
        let mut family = match index % 3 {
            0 => Family::PageOne {
                cipher: CIPHERS[scenario.random_range(0..CIPHERS.len())],
                page_size: PAGE_SIZES[scenario.random_range(0..PAGE_SIZES.len())],
                credential: CREDENTIALS[scenario.random_range(0..4)],
            },
            1 => {
                let existing = scenario.random::<bool>();
                Family::Manager {
                    existing,
                    // A new database opened without MVCC finishes before the
                    // registry owner yields, so it has no waiting window.
                    mvcc: !existing || scenario.random::<bool>(),
                    credential: CREDENTIALS[scenario.random_range(0..4)],
                    cancel_waiter: scenario.random::<bool>(),
                    wait_for_bytes: existing || scenario.random::<bool>(),
                }
            }
            _ => Family::Lifecycle(LifecycleSpec::generate(
                &mut scenario,
                &mut ChaCha8Rng::seed_from_u64(seeds.operations),
            )),
        };
        if index == 2 {
            let Family::Lifecycle(spec) = &mut family else {
                unreachable!()
            };
            spec.require_public_accessor_sweep();
        } else if index == 5 {
            let Family::Lifecycle(spec) = &mut family else {
                unreachable!()
            };
            spec.require_transaction_sweep();
        }
        Case {
            build: build.clone(),
            seeds,
            faults: false,
            family,
            decisions: Vec::new(),
            outcomes: Vec::new(),
            terminal: None,
        }
    }

    fn actors(&self) -> usize {
        match &self.family {
            Family::Lifecycle(spec) => spec.scripts.len(),
            _ => 0,
        }
    }

    fn commands(&self, actor: usize) -> usize {
        match &self.family {
            Family::Lifecycle(spec) => spec.setup[actor].len() + spec.scripts[actor].len(),
            _ => 0,
        }
    }

    fn total_commands(&self) -> usize {
        (0..self.actors()).map(|actor| self.commands(actor)).sum()
    }

    /// Removes an actor, its decisions and its requests, and renumbers the
    /// remaining actors.
    fn without_actor(&self, removed: usize) -> Option<Case> {
        let Family::Lifecycle(spec) = &self.family else {
            return None;
        };
        let mut spec = spec.clone();
        spec.setup.remove(removed);
        spec.scripts.remove(removed);
        let rename = |actor: usize| -> Option<usize> {
            match actor.cmp(&removed) {
                std::cmp::Ordering::Less => Some(actor),
                std::cmp::Ordering::Equal => None,
                std::cmp::Ordering::Greater => Some(actor - 1),
            }
        };
        let decisions = self
            .decisions
            .iter()
            .filter_map(|action| rename_action(action, &rename, &|_, command| Some(command)))
            .collect();
        Some(Case {
            family: Family::Lifecycle(spec),
            decisions,
            outcomes: Vec::new(),
            terminal: None,
            ..self.clone()
        })
    }

    /// Removes one command of an actor together with the decisions and
    /// requests that belong to it.
    fn without_command(&self, actor: usize, command: usize) -> Option<Case> {
        let Family::Lifecycle(spec) = &self.family else {
            return None;
        };
        let mut spec = spec.clone();
        if command < spec.setup[actor].len() {
            spec.setup[actor].remove(command);
        } else {
            spec.scripts[actor].remove(command - spec.setup[actor].len());
        }
        // The n-th Start of an actor starts its n-th command; its Polls and
        // Drops follow until the actor's next Start.
        let mut starts = 0;
        let mut current = None;
        let mut decisions = Vec::new();
        for action in &self.decisions {
            match action {
                Action::Start(owner) if *owner == actor => {
                    current = Some(starts);
                    starts += 1;
                    if current == Some(command) {
                        continue;
                    }
                }
                Action::Poll(owner) | Action::Drop(owner)
                    if *owner == actor && current == Some(command) =>
                {
                    continue;
                }
                _ => {}
            }
            let renamed = rename_action(action, &|owner| Some(owner), &|owner, index| {
                if owner != actor {
                    Some(index)
                } else {
                    match index.cmp(&command) {
                        std::cmp::Ordering::Less => Some(index),
                        std::cmp::Ordering::Equal => None,
                        std::cmp::Ordering::Greater => Some(index - 1),
                    }
                }
            });
            decisions.extend(renamed);
        }
        Some(Case {
            family: Family::Lifecycle(spec),
            decisions,
            outcomes: Vec::new(),
            terminal: None,
            ..self.clone()
        })
    }

    fn to_json(&self) -> Json {
        let parameters = match &self.family {
            Family::PageOne {
                cipher,
                page_size,
                credential,
            } => json!({
                "cipher": cipher.to_string(), "page_size": page_size,
                "credential": format!("{credential:?}"),
                "unused": "manager and lifecycle fields; the second actor reads with the credential"
            }),
            Family::Manager {
                existing,
                mvcc,
                credential,
                cancel_waiter,
                wait_for_bytes,
            } => json!({
                "cipher": "aegis256", "existing": existing, "mvcc": mvcc,
                "credential": format!("{credential:?}"), "cancel_waiter": cancel_waiter,
                "wait_for_bytes": wait_for_bytes,
                "unused": "page size; the manager runner always uses AEGIS-256 and the default page size"
            }),
            Family::Lifecycle(spec) => spec.to_json(),
        };
        json!({
            "version": VERSION,
            "build": self.build,
            "scenario": self.family.name(),
            "parameters": parameters,
            "seeds": {
                "campaign": self.seeds.campaign, "index": self.seeds.index,
                "scenario": self.seeds.scenario, "operations": self.seeds.operations,
                "schedule": self.seeds.schedule,
            },
            "faults": self.faults,
            "files": { "main": "per-case :memory: path derived from the seeds", "attached": "main path + -attached" },
            "decisions": self.decisions.iter().map(encode_action).collect::<Vec<_>>(),
            "outcomes": self.outcomes,
            "terminal": self.terminal.as_ref().map(Terminal::to_json),
        })
    }

    fn from_json(value: &Json) -> Result<Case, String> {
        match value["version"].as_u64() {
            Some(VERSION) => {}
            Some(1) => {
                return Err(
                    "version-1 artifacts name completions by queue position and cannot be replayed faithfully; regenerate the case with its seed".into(),
                )
            }
            other => return Err(format!("unsupported artifact version {other:?}")),
        }
        let p = &value["parameters"];
        let cipher = |name: &str| {
            CipherMode::try_from(p[name].as_str().unwrap_or_default())
                .map_err(|error| format!("{name}: {error}"))
        };
        let credential = || {
            CREDENTIALS
                .into_iter()
                .find(|credential| p["credential"] == format!("{credential:?}"))
                .ok_or("unknown credential")
        };
        let flag = |name: &str| p[name].as_bool().ok_or(format!("missing {name}"));
        let family = match value["scenario"].as_str() {
            Some("page_one") => Family::PageOne {
                cipher: cipher("cipher")?,
                page_size: parse_page_size(&p["page_size"])?,
                credential: credential()?,
            },
            Some("manager") => Family::Manager {
                existing: flag("existing")?,
                mvcc: flag("mvcc")?,
                credential: credential()?,
                cancel_waiter: flag("cancel_waiter")?,
                wait_for_bytes: flag("wait_for_bytes")?,
            },
            Some("lifecycle") => Family::Lifecycle(LifecycleSpec::from_json(p)?),
            other => return Err(format!("unknown scenario {other:?}")),
        };
        let seed = |name: &str| {
            value["seeds"][name]
                .as_u64()
                .ok_or(format!("missing seed {name}"))
        };
        let decisions = value["decisions"]
            .as_array()
            .ok_or("missing decisions")?
            .iter()
            .map(decode_action)
            .collect::<Result<_, _>>()?;
        let outcomes = value["outcomes"]
            .as_array()
            .ok_or("missing outcomes")?
            .iter()
            .map(|group| {
                group
                    .as_array()
                    .ok_or("an outcome group must be an array")?
                    .iter()
                    .map(|text| {
                        text.as_str()
                            .map(str::to_string)
                            .ok_or("an outcome must be a string")
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let case = Case {
            build: value["build"].clone(),
            seeds: Seeds {
                campaign: seed("campaign")?,
                index: seed("index")?,
                scenario: seed("scenario")?,
                operations: seed("operations")?,
                schedule: seed("schedule")?,
            },
            faults: value["faults"].as_bool().ok_or("missing faults flag")?,
            family,
            decisions,
            outcomes,
            terminal: (!value["terminal"].is_null())
                .then(|| Terminal::from_json(&value["terminal"])),
        };
        case.validate()?;
        Ok(case)
    }

    fn validate(&self) -> Result<(), String> {
        let actors = match &self.family {
            Family::Lifecycle(spec) => spec.scripts.len(),
            Family::PageOne { .. } | Family::Manager { .. } => 2,
        };
        if let Family::PageOne { cipher, .. } = &self.family {
            if !CIPHERS.contains(cipher) {
                return Err("the page-one scenario needs a supported encryption cipher".into());
            }
        }
        if let Family::Lifecycle(spec) = &self.family {
            if actors == 0 || actors > 4 || spec.setup.len() != actors {
                return Err(
                    "setup and operations must describe the same one to four actors".into(),
                );
            }
            if spec.start == Start::Cold
                && (spec.plaintext || spec.setup.iter().any(|commands| !commands.is_empty()))
            {
                return Err(
                    "a cold lifecycle needs encrypted data and no earlier setup commands".into(),
                );
            }
            if !CIPHERS.contains(&spec.cipher)
                || !CIPHERS.contains(&spec.other)
                || spec.cipher == spec.other
                || spec.cipher.required_key_size() != spec.other.required_key_size()
            {
                return Err(
                    "the alternate cipher must be different and use the same key size".into(),
                );
            }
            for commands in &spec.setup {
                if commands.iter().any(|command| {
                    !matches!(
                        command,
                        Command::Set(_) | Command::PrepareOnly(..) | Command::PrepareEarlySchema
                    )
                }) {
                    return Err("setup contains a command that needs protected data".into());
                }
            }
            for commands in &spec.scripts {
                if commands.iter().any(|command| {
                    matches!(
                        command,
                        Command::PrepareOnly(..) | Command::PrepareEarlySchema
                    )
                }) {
                    return Err(
                        "an earlier prepare command appears in the concurrent script".into(),
                    );
                }
            }
        }
        if self.decisions.len() > MAX_DECISIONS {
            return Err("the artifact exceeds the decision budget".into());
        }
        for action in &self.decisions {
            match action {
                Action::Start(actor) | Action::Poll(actor) | Action::Drop(actor)
                    if *actor >= actors =>
                {
                    return Err(format!("action refers to absent actor {actor}"));
                }
                Action::Complete(id) | Action::Fail(id) => {
                    if let Family::Lifecycle(spec) = &self.family {
                        let (actor, command) = parse_owner(&id.owner).ok_or_else(|| {
                            format!("request has an unknown operation {}", id.owner)
                        })?;
                        if actor >= actors
                            || command >= spec.setup[actor].len() + spec.scripts[actor].len()
                        {
                            return Err(format!("request refers to absent operation {}", id.owner));
                        }
                    }
                }
                _ => {}
            }
        }
        if self.terminal.is_some() && self.outcomes.len() != self.decisions.len() + 1 {
            return Err("a completed trace needs one outcome group per decision plus setup".into());
        }
        Ok(())
    }
}

fn load_case(path: &Path) -> Case {
    read_case(path).unwrap_or_else(|error| panic!("{error}"))
}

fn read_case(path: &Path) -> Result<Case, String> {
    let bytes = fs::read(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    let value: Json = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parsing {}: {error}", path.display()))?;
    Case::from_json(&value).map_err(|error| format!("{}: {error}", path.display()))
}

fn parse_page_size(value: &Json) -> Result<u32, String> {
    let page_size = value.as_u64().and_then(|size| u32::try_from(size).ok());
    page_size
        .filter(|size| PAGE_SIZES.contains(size))
        .ok_or_else(|| format!("unsupported page size {value}"))
}

/// Applies actor and command renumbering to an action. Returns `None` when the
/// action belongs to a removed actor or command.
fn rename_action(
    action: &Action,
    actor: &dyn Fn(usize) -> Option<usize>,
    command: &dyn Fn(usize, usize) -> Option<usize>,
) -> Option<Action> {
    let request = |id: &QueuedIoRequestId| -> Option<QueuedIoRequestId> {
        let Some((owner, index)) = parse_owner(&id.owner) else {
            return Some(id.clone());
        };
        let renamed_actor = actor(owner)?;
        let renamed_command = command(owner, index)?;
        Some(QueuedIoRequestId {
            owner: operation_owner(renamed_actor, renamed_command),
            ordinal: id.ordinal,
        })
    };
    Some(match action {
        Action::Poll(index) => Action::Poll(actor(*index)?),
        Action::Drop(index) => Action::Drop(actor(*index)?),
        Action::Start(index) => Action::Start(actor(*index)?),
        Action::Complete(id) => Action::Complete(request(id)?),
        Action::Fail(id) => Action::Fail(request(id)?),
        Action::CompleteUnderlying => Action::CompleteUnderlying,
    })
}

fn operation_owner(actor: usize, command: usize) -> String {
    format!("a{actor}.c{command}")
}

fn parse_owner(owner: &str) -> Option<(usize, usize)> {
    let (actor, command) = owner.strip_prefix('a')?.split_once(".c")?;
    Some((actor.parse().ok()?, command.parse().ok()?))
}

fn encode_action(action: &Action) -> Json {
    match action {
        Action::Poll(actor) => json!(["poll", actor]),
        Action::Drop(actor) => json!(["drop", actor]),
        Action::Start(actor) => json!(["start", actor]),
        Action::Complete(id) => json!(["complete", id.owner, id.ordinal]),
        Action::Fail(id) => json!(["fail", id.owner, id.ordinal]),
        Action::CompleteUnderlying => json!(["complete_underlying"]),
    }
}

fn decode_action(value: &Json) -> Result<Action, String> {
    let actor = || {
        value[1]
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or("missing actor")
    };
    let request = || -> Result<QueuedIoRequestId, String> {
        Ok(QueuedIoRequestId {
            owner: value[1].as_str().ok_or("missing owner")?.to_string(),
            ordinal: value[2]
                .as_u64()
                .and_then(|ordinal| u32::try_from(ordinal).ok())
                .ok_or("missing or out-of-range ordinal")?,
        })
    };
    let expected_len = match value[0].as_str() {
        Some("complete_underlying") => 1,
        Some("poll" | "drop" | "start") => 2,
        Some("complete" | "fail") => 3,
        other => return Err(format!("invalid action {other:?}")),
    };
    if value.as_array().map(Vec::len) != Some(expected_len) {
        return Err("an action has missing or extra fields".into());
    }
    Ok(match value[0].as_str() {
        Some("poll") => Action::Poll(actor()?),
        Some("drop") => Action::Drop(actor()?),
        Some("start") => Action::Start(actor()?),
        Some("complete") => Action::Complete(request()?),
        Some("fail") => Action::Fail(request()?),
        Some("complete_underlying") => Action::CompleteUnderlying,
        other => return Err(format!("invalid action {other:?}")),
    })
}

fn write_atomic(path: &Path, value: &Json) {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    fs::rename(temporary, path).unwrap();
}

fn env_number(name: &str, default: u64) -> u64 {
    std::env::var(name).map_or(default, |value| value.parse().unwrap())
}

// ---------------------------------------------------------------------------
// Lifecycle scenarios: typed commands, model and scheduler.
// ---------------------------------------------------------------------------

/// Credentials for opening or connecting to the main database, relative to the
/// key that ends up established.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Cred {
    Missing,
    Bound,
    OtherKey,
    WrongCipher,
}

/// Credentials in an ATTACH URI for the separately encrypted target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AttachCred {
    Missing,
    PrimaryKey,
    TargetKey,
    WrongCipher,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CipherChoice {
    Main,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Setting {
    Cipher(CipherChoice),
    Key(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Disposal {
    Drop,
    Reset,
    Explain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OpenApi {
    Sync,
    Async,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Command {
    /// Execute an encryption PRAGMA.
    Set(Setting),
    /// Prepare an encryption PRAGMA and drop, reset or EXPLAIN it unexecuted.
    PrepareOnly(Setting, Disposal),
    /// Prepare a schema query before encryption is established.
    PrepareEarlySchema,
    Read,
    Schema,
    TableInfo,
    Insert(i64),
    Checkpoint,
    /// Step the schema query prepared before encryption was established.
    StepEarlySchema,
    Connect(Cred),
    OpenCached(Cred),
    OpenDatabase(Cred, OpenApi),
    ReadDatabaseMvStore,
    ReadConnectionMvStore,
    MutateConnectionMvStore,
    ReadStatementMvStore,
    MutateStatementMvStore,
    ReadConnectionSchema,
    MutateConnectionSchema,
    ReadConnectionPager,
    ReadStatementPager,
    BeginWalInsert,
    PublishSchemaAfterRestore,
    ReloadWalAfterRestore,
    Begin,
    Savepoint,
    RollbackTo,
    Release,
    Commit,
    Rollback,
    Attach(AttachCred),
}

impl Command {
    fn kind(&self) -> &'static str {
        match self {
            Command::Set(_) => "set",
            Command::PrepareOnly(..) => "prepare_only",
            Command::PrepareEarlySchema => "prepare_early_schema",
            Command::Read => "read",
            Command::Schema => "schema",
            Command::TableInfo => "table_info",
            Command::Insert(_) => "insert",
            Command::Checkpoint => "checkpoint",
            Command::StepEarlySchema => "step_early_schema",
            Command::Connect(_) => "connect",
            Command::OpenCached(_) => "open",
            Command::OpenDatabase(_, OpenApi::Sync) => "database_open",
            Command::OpenDatabase(_, OpenApi::Async) => "database_open_async",
            Command::ReadDatabaseMvStore => "database_mv_store_read",
            Command::ReadConnectionMvStore => "connection_mv_store_read",
            Command::MutateConnectionMvStore => "connection_mv_store_mutate",
            Command::ReadStatementMvStore => "statement_mv_store_read",
            Command::MutateStatementMvStore => "statement_mv_store_mutate",
            Command::ReadConnectionSchema => "connection_schema_read",
            Command::MutateConnectionSchema => "connection_schema_mutate",
            Command::ReadConnectionPager => "connection_pager_read",
            Command::ReadStatementPager => "statement_pager_read",
            Command::BeginWalInsert => "wal_insert_begin",
            Command::PublishSchemaAfterRestore => "publish_schema_after_restore",
            Command::ReloadWalAfterRestore => "reload_wal_after_restore",
            Command::Begin => "begin",
            Command::Savepoint => "savepoint",
            Command::RollbackTo => "rollback_to",
            Command::Release => "release",
            Command::Commit => "commit",
            Command::Rollback => "rollback",
            Command::Attach(_) => "attach",
        }
    }

    fn can_cancel(self) -> bool {
        !matches!(
            self,
            Command::Begin
                | Command::Savepoint
                | Command::RollbackTo
                | Command::Release
                | Command::Commit
                | Command::Rollback
        )
    }

    fn encode(&self) -> String {
        let setting = |setting: &Setting| match setting {
            Setting::Cipher(CipherChoice::Main) => "cipher:main".to_string(),
            Setting::Cipher(CipherChoice::Other) => "cipher:other".to_string(),
            Setting::Key(key) => format!("key:{key}"),
        };
        match self {
            Command::Set(s) => format!("set:{}", setting(s)),
            Command::PrepareOnly(s, disposal) => {
                format!("prepare_only:{}:{disposal:?}", setting(s))
            }
            Command::Insert(id) => format!("insert:{id}"),
            Command::Connect(cred) => format!("connect:{cred:?}"),
            Command::OpenCached(cred) => format!("open:{cred:?}"),
            Command::OpenDatabase(cred, api) => {
                format!("database_open:{api:?}:{cred:?}")
            }
            Command::Attach(cred) => format!("attach:{cred:?}"),
            other => other.kind().to_string(),
        }
    }

    fn decode(text: &str) -> Result<Command, String> {
        let parts: Vec<&str> = text.split(':').collect();
        let setting = |parts: &[&str]| -> Result<Setting, String> {
            match parts {
                ["cipher", "main", ..] => Ok(Setting::Cipher(CipherChoice::Main)),
                ["cipher", "other", ..] => Ok(Setting::Cipher(CipherChoice::Other)),
                ["key", key, ..] => Ok(Setting::Key(key.parse().map_err(|_| "bad key")?)),
                _ => Err(format!("bad setting in {text}")),
            }
        };
        let cred = |name: &str| -> Result<Cred, String> {
            [
                Cred::Missing,
                Cred::Bound,
                Cred::OtherKey,
                Cred::WrongCipher,
            ]
            .into_iter()
            .find(|cred| format!("{cred:?}") == name)
            .ok_or(format!("bad credential in {text}"))
        };
        Ok(match parts.as_slice() {
            ["set", rest @ ..] => Command::Set(setting(rest)?),
            ["prepare_only", a, b, disposal] => Command::PrepareOnly(
                setting(&[a, b])?,
                [Disposal::Drop, Disposal::Reset, Disposal::Explain]
                    .into_iter()
                    .find(|d| format!("{d:?}") == *disposal)
                    .ok_or("bad disposal")?,
            ),
            ["prepare_early_schema"] => Command::PrepareEarlySchema,
            ["read"] => Command::Read,
            ["schema"] => Command::Schema,
            ["table_info"] => Command::TableInfo,
            ["insert", id] => Command::Insert(id.parse().map_err(|_| "bad id")?),
            ["checkpoint"] => Command::Checkpoint,
            ["step_early_schema"] => Command::StepEarlySchema,
            ["connect", name] => Command::Connect(cred(name)?),
            ["open", name] => Command::OpenCached(cred(name)?),
            ["database_open", api, name] => Command::OpenDatabase(
                cred(name)?,
                match *api {
                    "Sync" => OpenApi::Sync,
                    "Async" => OpenApi::Async,
                    _ => return Err(format!("bad open API in {text}")),
                },
            ),
            ["database_mv_store_read"] => Command::ReadDatabaseMvStore,
            ["connection_mv_store_read"] => Command::ReadConnectionMvStore,
            ["connection_mv_store_mutate"] => Command::MutateConnectionMvStore,
            ["statement_mv_store_read"] => Command::ReadStatementMvStore,
            ["statement_mv_store_mutate"] => Command::MutateStatementMvStore,
            ["connection_schema_read"] => Command::ReadConnectionSchema,
            ["connection_schema_mutate"] => Command::MutateConnectionSchema,
            ["connection_pager_read"] => Command::ReadConnectionPager,
            ["statement_pager_read"] => Command::ReadStatementPager,
            ["wal_insert_begin"] => Command::BeginWalInsert,
            ["publish_schema_after_restore"] => Command::PublishSchemaAfterRestore,
            ["reload_wal_after_restore"] => Command::ReloadWalAfterRestore,
            ["begin"] => Command::Begin,
            ["savepoint"] => Command::Savepoint,
            ["rollback_to"] => Command::RollbackTo,
            ["release"] => Command::Release,
            ["commit"] => Command::Commit,
            ["rollback"] => Command::Rollback,
            ["attach", name] => Command::Attach(
                [
                    AttachCred::Missing,
                    AttachCred::PrimaryKey,
                    AttachCred::TargetKey,
                    AttachCred::WrongCipher,
                ]
                .into_iter()
                .find(|cred| format!("{cred:?}") == *name)
                .ok_or("bad attach credential")?,
            ),
            _ => return Err(format!("unknown command {text}")),
        })
    }
}

/// How the scenario's actors first reach the database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Start {
    /// Actors connect to a new, empty database before encryption is
    /// established and may configure it with PRAGMAs.
    Earlier,
    /// The encrypted database is created, filled and closed; actors then open
    /// it through the registry with their own credentials.
    Cold,
}

/// Test-only corruption of the harness's own observations, used to show the
/// oracle reports each kind of failure. Never generated by the campaign.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Sabotage {
    ForgeRow,
    ForgeSchema,
    LoseAcknowledgedWrite,
    ExtraWrite,
}

#[derive(Clone, Debug)]
pub(super) struct LifecycleSpec {
    pub(super) start: Start,
    pub(super) plaintext: bool,
    pub(super) cipher: CipherMode,
    pub(super) other: CipherMode,
    pub(super) page_size: u32,
    pub(super) mvcc: bool,
    pub(super) target_cached: bool,
    /// Commands run while the database is still unconfigured (`Start::Earlier`).
    pub(super) setup: Vec<Vec<Command>>,
    /// Commands run concurrently after the database holds protected data.
    pub(super) scripts: Vec<Vec<Command>>,
    pub(super) sabotage: Option<Sabotage>,
}

impl LifecycleSpec {
    fn generate(scenario: &mut ChaCha8Rng, operations: &mut ChaCha8Rng) -> LifecycleSpec {
        let cipher = CIPHERS[scenario.random_range(0..CIPHERS.len())];
        let other = *CIPHERS
            .iter()
            .filter(|other| {
                **other != cipher && other.required_key_size() == cipher.required_key_size()
            })
            .nth(scenario.random_range(0..2))
            .unwrap();
        let plaintext = scenario.random_ratio(1, 8);
        let start = if !plaintext && scenario.random_ratio(1, 3) {
            Start::Cold
        } else {
            Start::Earlier
        };
        let actors = scenario.random_range(2..=4);
        let mut spec = LifecycleSpec {
            start,
            plaintext,
            cipher,
            other,
            page_size: [1024, 4096, 8192, 65536][scenario.random_range(0..4)],
            mvcc: scenario.random::<bool>(),
            target_cached: scenario.random::<bool>(),
            setup: vec![Vec::new(); actors],
            scripts: vec![Vec::new(); actors],
            sabotage: None,
        };
        let any_setting = |rng: &mut ChaCha8Rng| match rng.random_range(0..6) {
            0..=2 => Setting::Key(1),
            3 => Setting::Key(2),
            4 => Setting::Cipher(CipherChoice::Main),
            _ => Setting::Cipher(CipherChoice::Other),
        };
        for actor in 0..actors {
            if start == Start::Earlier && !plaintext {
                for _ in 0..operations.random_range(0..=3) {
                    let command = match operations.random_range(0..4) {
                        0 | 1 => Command::Set(any_setting(operations)),
                        2 => Command::PrepareOnly(
                            any_setting(operations),
                            [Disposal::Drop, Disposal::Reset, Disposal::Explain]
                                [operations.random_range(0..3)],
                        ),
                        _ => Command::PrepareEarlySchema,
                    };
                    spec.setup[actor].push(command);
                }
            }
            if start == Start::Cold {
                spec.scripts[actor].push(Command::OpenCached(Self::cred(operations)));
            }
            for step in 0..operations.random_range(2..=8) {
                let insert = Command::Insert(1000 * (actor as i64 + 1) + step);
                let command = if plaintext {
                    match operations.random_range(0..7) {
                        0 => Command::Read,
                        1 => Command::Schema,
                        2 => Command::TableInfo,
                        3 | 4 => insert,
                        5 => Command::Checkpoint,
                        _ => Command::Set(any_setting(operations)),
                    }
                } else {
                    match operations.random_range(0..27) {
                        0 | 1 => Command::Read,
                        2 => Command::Schema,
                        3 => Command::TableInfo,
                        4 | 5 => insert,
                        6 => Command::Checkpoint,
                        7 => Command::StepEarlySchema,
                        8 => Command::Connect(Self::cred(operations)),
                        9 => Command::OpenCached(Self::cred(operations)),
                        10 => Command::Attach(
                            [
                                AttachCred::Missing,
                                AttachCred::PrimaryKey,
                                AttachCred::TargetKey,
                                AttachCred::WrongCipher,
                            ][operations.random_range(0..4)],
                        ),
                        11 => Command::OpenDatabase(Self::cred(operations), OpenApi::Sync),
                        12 => Command::OpenDatabase(Self::cred(operations), OpenApi::Async),
                        13 if spec.mvcc => Command::ReadDatabaseMvStore,
                        14 if spec.mvcc => Command::ReadConnectionMvStore,
                        15 if spec.mvcc => Command::MutateConnectionMvStore,
                        16 if spec.mvcc => Command::ReadStatementMvStore,
                        17 if spec.mvcc => Command::MutateStatementMvStore,
                        18 => Command::ReadConnectionSchema,
                        19 => Command::MutateConnectionSchema,
                        20 => Command::ReadConnectionPager,
                        21 => Command::ReadStatementPager,
                        22 if !spec.mvcc => Command::BeginWalInsert,
                        23 => Command::PublishSchemaAfterRestore,
                        24 => Command::ReloadWalAfterRestore,
                        _ => Command::Set(Setting::Key(operations.random_range(1..=2))),
                    }
                };
                spec.scripts[actor].push(command);
            }
        }
        spec
    }

    /// Makes the first lifecycle case of every campaign cover every public
    /// handle that can reach shared encrypted state without executing SQL.
    fn require_public_accessor_sweep(&mut self) {
        self.start = Start::Earlier;
        self.plaintext = false;
        self.mvcc = true;
        for commands in &mut self.setup {
            commands.clear();
        }
        self.setup[0].push(Command::PrepareEarlySchema);
        for commands in &mut self.scripts {
            commands.clear();
        }
        self.scripts[0] = vec![
            Command::OpenDatabase(Cred::Missing, OpenApi::Sync),
            Command::OpenDatabase(Cred::OtherKey, OpenApi::Async),
            Command::ReadDatabaseMvStore,
            Command::Connect(Cred::Bound),
            Command::ReadConnectionMvStore,
            Command::MutateConnectionMvStore,
            Command::ReadStatementMvStore,
            Command::MutateStatementMvStore,
            Command::ReadConnectionSchema,
            Command::MutateConnectionSchema,
            Command::ReadConnectionPager,
            Command::ReadStatementPager,
            Command::PublishSchemaAfterRestore,
            Command::ReloadWalAfterRestore,
        ];
        self.scripts[1] = vec![
            Command::Connect(Cred::Bound),
            Command::Read,
            Command::Insert(2001),
            Command::Checkpoint,
        ];
    }

    /// Gives every campaign one valid multi-step MVCC transaction whose
    /// savepoint discards one write while another actor overlaps it.
    fn require_transaction_sweep(&mut self) {
        self.start = Start::Earlier;
        self.plaintext = false;
        self.mvcc = true;
        for commands in &mut self.setup {
            commands.clear();
        }
        for commands in &mut self.scripts {
            commands.clear();
        }
        self.scripts[0] = vec![
            Command::Connect(Cred::Bound),
            Command::Begin,
            Command::Insert(2001),
            Command::Savepoint,
            Command::Insert(2002),
            Command::RollbackTo,
            Command::Release,
            Command::Commit,
        ];
        self.scripts[1] = vec![
            Command::Connect(Cred::Bound),
            Command::Read,
            Command::Checkpoint,
        ];
    }

    fn cred(rng: &mut ChaCha8Rng) -> Cred {
        [
            Cred::Missing,
            Cred::Bound,
            Cred::OtherKey,
            Cred::WrongCipher,
        ][rng.random_range(0..4)]
    }

    fn to_json(&self) -> Json {
        let commands = |scripts: &Vec<Vec<Command>>| -> Json {
            scripts
                .iter()
                .map(|script| script.iter().map(Command::encode).collect::<Vec<_>>())
                .collect::<Vec<_>>()
                .into()
        };
        json!({
            "start": format!("{:?}", self.start),
            "plaintext": self.plaintext,
            "cipher": self.cipher.to_string(),
            "other_cipher": self.other.to_string(),
            "page_size": self.page_size,
            "mvcc": self.mvcc,
            "target_cached": self.target_cached,
            "keys": "key 1 and key 2 are distinct valid-length keys; Bound means the key that ended up established",
            "setup": commands(&self.setup),
            "operations": commands(&self.scripts),
            "sabotage": self.sabotage.map(|sabotage| format!("{sabotage:?}")),
        })
    }

    fn from_json(p: &Json) -> Result<LifecycleSpec, String> {
        let commands = |name: &str| -> Result<Vec<Vec<Command>>, String> {
            p[name]
                .as_array()
                .ok_or(format!("missing {name}"))?
                .iter()
                .map(|script| {
                    script
                        .as_array()
                        .ok_or("an actor's commands must be an array")?
                        .iter()
                        .map(|text| Command::decode(text.as_str().unwrap_or_default()))
                        .collect()
                })
                .collect()
        };
        let cipher = |name: &str| {
            CipherMode::try_from(p[name].as_str().unwrap_or_default())
                .map_err(|error| format!("{name}: {error}"))
        };
        Ok(LifecycleSpec {
            start: match p["start"].as_str() {
                Some("Earlier") => Start::Earlier,
                Some("Cold") => Start::Cold,
                other => return Err(format!("bad start {other:?}")),
            },
            plaintext: p["plaintext"].as_bool().ok_or("missing plaintext flag")?,
            cipher: cipher("cipher")?,
            other: cipher("other_cipher")?,
            page_size: parse_page_size(&p["page_size"])?,
            mvcc: p["mvcc"].as_bool().ok_or("missing mvcc flag")?,
            target_cached: p["target_cached"]
                .as_bool()
                .ok_or("missing target_cached flag")?,
            setup: commands("setup")?,
            scripts: commands("operations")?,
            sabotage: match p["sabotage"].as_str() {
                None => None,
                Some("ForgeRow") => Some(Sabotage::ForgeRow),
                Some("ForgeSchema") => Some(Sabotage::ForgeSchema),
                Some("LoseAcknowledgedWrite") => Some(Sabotage::LoseAcknowledgedWrite),
                Some("ExtraWrite") => Some(Sabotage::ExtraWrite),
                Some(other) => return Err(format!("bad sabotage {other}")),
            },
        })
    }
}

/// What a failed engine call means, decided from the error and the operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Category {
    CredentialMissing,
    CredentialWrong,
    CipherMismatch,
    SettingsRejected,
    Contention,
    InjectedIo,
    Unexpected,
}

pub(super) fn classify(error: &LimboError) -> Category {
    match error {
        LimboError::CompletionError(CompletionError::DecryptionError { .. }) => {
            Category::CredentialWrong
        }
        LimboError::CompletionError(CompletionError::IOError(_, "queued_io")) => {
            Category::InjectedIo
        }
        LimboError::Busy | LimboError::BusySnapshot | LimboError::WriteWriteConflict => {
            Category::Contention
        }
        LimboError::NotADB => Category::CredentialMissing,
        // A checkpoint reports the I/O error that stopped it as text.
        LimboError::CheckpointFailed(message) if message.contains("(queued_io)") => {
            Category::InjectedIo
        }
        LimboError::InvalidArgument(message) => {
            if message.contains("requires an encryption key")
                || message.contains("encrypted but no encryption options")
            {
                Category::CredentialMissing
            } else if message.contains("cipher does not match") {
                Category::CipherMismatch
            } else if message.contains("cannot reset encryption attributes")
                || message.contains("cannot enable encryption after MVCC")
                || message.contains("must be configured before the first database write")
                || message.contains("opt in feature")
                || message.contains("encryption must be configured when opening")
                || message.contains("without encryption")
                || message.contains("external restore is not supported")
            {
                Category::SettingsRejected
            } else {
                Category::Unexpected
            }
        }
        _ => Category::Unexpected,
    }
}

/// Established encryption of the main database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Binding {
    Unbound,
    Plaintext,
    Bound(CipherMode, u8),
}

/// One connection's executed encryption settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Settings {
    pub(super) cipher: Option<CipherMode>,
    pub(super) key: Option<u8>,
    pub(super) installed: bool,
}

impl Settings {
    /// Applies an executed setting following the transition table in section
    /// 17 of the plan. Returns the expected error category when the engine must
    /// reject it; a rejected setting leaves both the connection's settings and
    /// the binding unchanged.
    pub(super) fn apply(
        &mut self,
        _setting: Setting,
        _cipher_of: impl Fn(CipherChoice) -> CipherMode,
        _binding: &mut Binding,
        _mvcc_active: bool,
    ) -> Result<(), Category> {
        Err(Category::SettingsRejected)
    }

    pub(super) fn authorized(&self, binding: Binding) -> bool {
        match binding {
            Binding::Bound(cipher, key) => {
                self.installed && self.cipher == Some(cipher) && self.key == Some(key)
            }
            Binding::Unbound | Binding::Plaintext => true,
        }
    }
}

/// A statement an actor is driving.
struct Active {
    command: Command,
    owner: String,
    statement: Statement,
    allowed: bool,
    pending: Option<IOCompletions>,
    rows: Vec<String>,
    waiting_since: Option<usize>,
    unchanged_retries: usize,
}

struct ActorState {
    connection: Option<Arc<Connection>>,
    database: Option<Arc<Database>>,
    database_authorized: bool,
    settings: Settings,
    next: usize,
    early: Option<Statement>,
    active: Option<Active>,
    transaction: Option<TransactionModel>,
}

#[derive(Default)]
struct TransactionModel {
    inserts: BTreeSet<i64>,
    savepoint: Option<BTreeSet<i64>>,
}

struct World {
    io: Arc<QueuedIo>,
    inner: Arc<dyn IO>,
    path: String,
    target_path: String,
    base: OpenOptions,
    spec: LifecycleSpec,
    binding: Binding,
    mvcc_active: bool,
    actors: Vec<ActorState>,
    owner: Option<Arc<Connection>>,
    database: Option<Arc<Database>>,
    target: Option<(Arc<Database>, Arc<Connection>)>,
    acknowledged: BTreeSet<i64>,
    possible: BTreeSet<i64>,
    fault_injected: bool,
    epoch: usize,
}

impl World {
    fn cipher_of(&self, choice: CipherChoice) -> CipherMode {
        match choice {
            CipherChoice::Main => self.spec.cipher,
            CipherChoice::Other => self.spec.other,
        }
    }

    fn bound(&self) -> (CipherMode, u8) {
        match self.binding {
            Binding::Bound(cipher, key) => (cipher, key),
            _ => (self.spec.cipher, 1),
        }
    }

    fn other_key(&self) -> u8 {
        if self.bound().1 == 1 {
            2
        } else {
            1
        }
    }

    fn wrong_cipher(&self) -> CipherMode {
        let bound = self.bound().0;
        if bound == self.spec.cipher {
            self.spec.other
        } else {
            self.spec.cipher
        }
    }

    fn options(&self, cred: Cred) -> OpenOptions {
        let (cipher, key) = self.bound();
        match cred {
            Cred::Missing => self.base.clone(),
            Cred::Bound => self
                .base
                .clone()
                .encryption(encryption_options(cipher, key)),
            Cred::OtherKey => self
                .base
                .clone()
                .encryption(encryption_options(cipher, self.other_key())),
            Cred::WrongCipher => self
                .base
                .clone()
                .encryption(encryption_options(self.wrong_cipher(), key)),
        }
    }

    fn connect_key(&self, cred: Cred) -> Option<EncryptionKey> {
        let (cipher, key) = self.bound();
        match cred {
            Cred::Missing => None,
            Cred::Bound => Some(self::key(cipher, key)),
            Cred::OtherKey => Some(self::key(cipher, self.other_key())),
            Cred::WrongCipher => Some(self::key(self.wrong_cipher(), key)),
        }
    }

    fn main_file_size(&self) -> u64 {
        self.inner
            .open_file(&self.path, OpenFlags::Create, false)
            .unwrap()
            .size()
            .unwrap()
    }

    fn any_database(&self) -> Option<Arc<Database>> {
        self.database
            .clone()
            .or_else(|| self.actors.iter().find_map(|actor| actor.database.clone()))
    }

    fn authorized(&self, actor: usize) -> bool {
        self.actors[actor].connection.is_some()
            && self.actors[actor].settings.authorized(self.binding)
    }
}

fn lifecycle(trace: &mut Trace, spec: &LifecycleSpec) {
    #[cfg(feature = "io_memory_yield")]
    let inner: Arc<dyn IO> = Arc::new(turso_core::MemoryYieldIO::new());
    #[cfg(not(feature = "io_memory_yield"))]
    let inner: Arc<dyn IO> = Arc::new(MemoryIO::new());
    let io = Arc::new(QueuedIo::new_with_io(inner.clone()));
    io.set_stall_limit(Some(STALL_LIMIT));
    io.set_owner("setup");
    let path = format!(":encryption-fuzz-lifecycle:{}", trace.namespace);
    let base = OpenOptions::new(Arc::new(SqliteDialect))
        .db_opts(DatabaseOpts::new().with_encryption(true).with_attach(true))
        .page_size(turso_core::storage::sqlite3_ondisk::PageSize::new(spec.page_size).unwrap());
    let mut world = World {
        io: io.clone(),
        inner,
        target_path: format!("{path}-attached"),
        path,
        base,
        spec: spec.clone(),
        binding: Binding::Unbound,
        mvcc_active: false,
        actors: (0..spec.scripts.len())
            .map(|_| ActorState {
                connection: None,
                database: None,
                database_authorized: false,
                settings: Settings::default(),
                next: 0,
                early: None,
                active: None,
                transaction: None,
            })
            .collect(),
        owner: None,
        database: None,
        target: None,
        acknowledged: BTreeSet::from([0]),
        possible: BTreeSet::from([0]),
        fault_injected: false,
        epoch: 0,
    };
    cover(format!(
        "lifecycle.start={:?}{}",
        spec.start,
        if spec.plaintext { ".plaintext" } else { "" }
    ));
    cover(format!("lifecycle.cipher={}", spec.cipher));
    cover(format!("lifecycle.page_size={}", spec.page_size));
    cover(format!("lifecycle.mvcc={}", spec.mvcc));
    cover(format!("lifecycle.actors={}", spec.scripts.len()));

    match spec.start {
        Start::Earlier => earlier_setup(trace, &mut world),
        Start::Cold => cold_setup(trace, &mut world),
    }
    concurrent_phase(trace, &mut world);
    postconditions(trace, world);
}

/// Connects every actor to a new empty database, runs their configuration
/// commands one at a time, then establishes encryption (if no actor did) and
/// writes the protected table through a separate owner connection.
fn earlier_setup(trace: &mut Trace, world: &mut World) {
    world.binding = if world.spec.plaintext {
        Binding::Plaintext
    } else {
        Binding::Bound(world.spec.cipher, 1)
    };
    let db = Database::open(
        world.io.clone(),
        &world.path,
        if world.spec.plaintext {
            world.base.clone()
        } else {
            world.options(Cred::Bound)
        },
    )
    .unwrap();
    let owner = db
        .connect(
            ConnectOpts::new().with_encryption_key(if world.spec.plaintext {
                None
            } else {
                Some(key(world.spec.cipher, 1))
            }),
        )
        .unwrap();
    for actor in &mut world.actors {
        if world.spec.plaintext {
            actor.connection = Some(db.connect(ConnectOpts::new()).unwrap());
        } else {
            check!(
                db.connect(ConnectOpts::new()).is_err(),
                "unauthorized_connection",
                "keyless setup returned a connection"
            );
        }
    }
    world.database = Some(db.clone());
    // Configuration commands run without I/O, so the scheduler only orders
    // them between actors.
    loop {
        let enabled: Vec<Action> = (0..world.actors.len())
            .filter(|actor| world.actors[*actor].next < world.spec.setup[*actor].len())
            .map(Action::Start)
            .collect();
        if enabled.is_empty() {
            break;
        }
        let Action::Start(actor) = trace.choose(&enabled) else {
            unreachable!()
        };
        let index = world.actors[actor].next;
        world.actors[actor].next += 1;
        let command = world.spec.setup[actor][index];
        let owner_name = operation_owner(actor, index);
        world.io.set_owner(owner_name.clone());
        set_operation(format!("{} setup", command.kind()));
        trace.persist();
        let outcome = configuration_command(world, actor, command);
        trace.observe(format!("{owner_name} {} -> {outcome}", command.encode()));
        check_unwritten(world, &db);
        world.io.set_owner("scheduler");
        trace.after_action(&world.io);
    }
    for actor in &mut world.actors {
        actor.next = 0;
    }
    world.io.set_owner("setup");
    set_operation("establish setup");
    let owner_settings = if world.spec.plaintext {
        Settings::default()
    } else {
        Settings {
            cipher: Some(world.spec.cipher),
            key: Some(1),
            installed: true,
        }
    };
    trace.observe(format!("binding {:?}", world.binding));
    populate(world, &owner);
    check!(
        owner_settings.authorized(world.binding),
        "harness_failure",
        "the setup owner is not authorized"
    );
    world.owner = Some(owner);
    create_target(world);
    trace.after_action(&world.io);
}

/// Creates, fills and closes an encrypted database. Actors then open it
/// themselves, so the first successful open is a cold registry open.
fn cold_setup(trace: &mut Trace, world: &mut World) {
    let (cipher, key) = (world.spec.cipher, 1);
    world.binding = Binding::Bound(cipher, key);
    let weak = {
        let db = Database::open(
            world.io.clone(),
            &world.path,
            world
                .base
                .clone()
                .encryption(encryption_options(cipher, key)),
        )
        .unwrap();
        let owner = db
            .connect(ConnectOpts::new().with_encryption_key(Some(self::key(cipher, key))))
            .unwrap();
        populate(world, &owner);
        owner.close().unwrap();
        Arc::downgrade(&db)
    };
    check!(
        weak.upgrade().is_none(),
        "harness_failure",
        "the setup database stayed open"
    );
    trace.observe("database closed before actors open it");
    create_target(world);
    trace.after_action(&world.io);
}

fn populate(world: &mut World, owner: &Arc<Connection>) {
    if world.spec.mvcc {
        owner.execute("PRAGMA journal_mode = 'mvcc'").unwrap();
        world.mvcc_active = true;
    }
    owner
        .execute("CREATE TABLE secrets(id INTEGER PRIMARY KEY, value TEXT)")
        .unwrap();
    owner
        .execute("INSERT INTO secrets VALUES (0, 'protected value')")
        .unwrap();
}

fn create_target(world: &mut World) {
    if world.spec.plaintext {
        return;
    }
    let (cipher, key) = world.bound();
    let target_key = if key == 1 { 2 } else { 1 };
    let db = Database::open(
        world.io.clone(),
        &world.target_path,
        world
            .base
            .clone()
            .encryption(encryption_options(cipher, target_key)),
    )
    .unwrap();
    let connection = db
        .connect(ConnectOpts::new().with_encryption_key(Some(self::key(cipher, target_key))))
        .unwrap();
    // ATTACH requires the same page size and journal mode as the main database.
    if world.spec.mvcc {
        connection.execute("PRAGMA journal_mode = 'mvcc'").unwrap();
    }
    connection
        .execute("CREATE TABLE secrets(value TEXT)")
        .unwrap();
    connection
        .execute("INSERT INTO secrets VALUES ('attached value')")
        .unwrap();
    if world.spec.target_cached {
        world.target = Some((db, connection));
    } else {
        connection.close().unwrap();
    }
}

/// Runs one configuration command on an unconfigured database and checks it
/// against the model.
fn configuration_command(world: &mut World, actor: usize, command: Command) -> String {
    let Some(connection) = world.actors[actor].connection.clone() else {
        return "refused: no authenticated connection".into();
    };
    match command {
        Command::Set(setting) => {
            let mut settings = world.actors[actor].settings;
            let mut binding = world.binding;
            let expected = settings.apply(
                setting,
                |choice| world.cipher_of(choice),
                &mut binding,
                world.mvcc_active,
            );
            let actual = connection.execute(setting_sql(world, setting));
            check_setting_outcome(&expected, &actual, setting);
            if expected.is_ok() {
                world.actors[actor].settings = settings;
                world.binding = binding;
            }
            check!(
                connection.get_encryption_cipher_mode() == world.actors[actor].settings.cipher,
                "settings_mismatch",
                "connection cipher {:?}, model {:?}",
                connection.get_encryption_cipher_mode(),
                world.actors[actor].settings.cipher
            );
            format!("{expected:?}")
        }
        Command::PrepareOnly(setting, disposal) => {
            let before = connection.get_encryption_cipher_mode();
            let sql = setting_sql(world, setting);
            let result = match disposal {
                Disposal::Drop | Disposal::Reset => connection.prepare(&sql).map(|_| ()),
                Disposal::Explain => connection.execute(format!("EXPLAIN {sql}")),
            };
            check!(
                result.is_err(),
                "prepare_side_effect",
                "encryption assignment was accepted during preparation"
            );
            check!(
                connection.get_encryption_cipher_mode() == before,
                "prepare_side_effect",
                "an unexecuted {setting:?} changed the connection cipher"
            );
            "unchanged".to_string()
        }
        Command::PrepareEarlySchema => {
            let allowed = world.actors[actor].settings.authorized(world.binding);
            match connection.prepare("SELECT name FROM sqlite_schema WHERE name = 'secrets'") {
                Ok(statement) => {
                    check!(
                        allowed,
                        "unauthorized_success",
                        "a connection without the established key prepared a schema query"
                    );
                    world.actors[actor].early = Some(statement);
                    cover(format!(
                        "lifecycle.early_schema_prepared.{}",
                        if world.binding == Binding::Unbound {
                            "before_binding"
                        } else {
                            "after_binding"
                        }
                    ));
                    "prepared".to_string()
                }
                Err(error) => {
                    check_error(world, command, allowed, &error, "prepare");
                    format!("refused: {:?}", classify(&error))
                }
            }
        }
        other => panic!("{other:?} is not a configuration command"),
    }
}

fn check_setting_outcome(
    expected: &Result<(), Category>,
    actual: &turso_core::Result<()>,
    setting: Setting,
) {
    match (expected, actual) {
        (Ok(()), Ok(())) => {}
        (Err(category), Err(error)) => check!(
            classify(error) == *category,
            "setting_outcome",
            "{setting:?} failed with {error}, expected {category:?}"
        ),
        _ => check!(
            false,
            "setting_outcome",
            "{setting:?} returned {actual:?}, model expected {expected:?}"
        ),
    }
}

/// Checks that configuration has not written the database and that only the
/// established key can connect.
fn check_unwritten(world: &World, db: &Arc<Database>) {
    check!(
        world.main_file_size() == 0,
        "bytes_before_first_write",
        "configuration wrote database bytes"
    );
    check!(
        !world
            .io
            .records()
            .iter()
            .any(|record| record.event.path == world.path
                && matches!(
                    record.event.kind,
                    QueuedIoOpKind::Pwrite | QueuedIoOpKind::Pwritev
                )),
        "bytes_before_first_write",
        "configuration submitted a database write"
    );
    match world.binding {
        Binding::Bound(cipher, key) => {
            match db.connect(ConnectOpts::new()) {
                Ok(_) => check!(
                    false,
                    "unauthorized_connection",
                    "a keyless connection opened a database bound to a key"
                ),
                Err(error) => {
                    check_credential_refusal(world, &error, "keyless bootstrap connection")
                }
            }
            check!(
                db.connect(ConnectOpts::new().with_encryption_key(Some(self::key(cipher, key))))
                    .is_ok(),
                "binding_changed",
                "the established key stopped working before the first write"
            );
            let other = if key == 1 { 2 } else { 1 };
            match db.connect(ConnectOpts::new().with_encryption_key(Some(self::key(cipher, other))))
            {
                Ok(_) => check!(
                    false,
                    "binding_changed",
                    "a different key connected before the first write"
                ),
                Err(error) => check_credential_refusal(world, &error, "wrong bootstrap connection"),
            }
        }
        _ => check!(
            db.connect(ConnectOpts::new()).is_ok(),
            "prepare_side_effect",
            "a keyless connection was refused before any key was established"
        ),
    }
}

fn setting_sql(world: &World, setting: Setting) -> String {
    match setting {
        Setting::Cipher(choice) => format!("PRAGMA cipher = '{}'", world.cipher_of(choice)),
        Setting::Key(byte) => {
            // Keys are valid for whichever cipher the connection will use.
            let size_cipher = world.spec.cipher;
            format!(
                "PRAGMA hexkey = '{}'",
                encryption_options(size_cipher, byte).hexkey
            )
        }
    }
}

/// The scheduler for overlapping actors. Any idle actor may start its next
/// command while other actors wait on I/O.
fn concurrent_phase(trace: &mut Trace, world: &mut World) {
    loop {
        let generating = trace.generating();
        let pending = world.io.pending_events();
        let mut enabled = Vec::new();
        for (index, actor) in world.actors.iter().enumerate() {
            match &actor.active {
                Some(active) => {
                    let ready = active
                        .pending
                        .as_ref()
                        .is_none_or(|pending| pending.finished());
                    let waiting = active.waiting_since == Some(world.epoch);
                    if ready && !waiting {
                        enabled.push(Action::Poll(index));
                    }
                    if generating
                        && active.command.can_cancel()
                        && world.actors[index].transaction.is_none()
                    {
                        enabled.push(Action::Drop(index));
                    }
                }
                None => {
                    if generating && actor.next < world.spec.scripts[index].len() {
                        enabled.push(Action::Start(index));
                    }
                }
            }
        }
        for request in &pending {
            enabled.push(Action::Complete(request.id.clone()));
            if generating
                && trace.allow_failures
                && !world.fault_injected
                && request_can_fail(world, &request.id)
                && matches!(
                    request.kind,
                    QueuedIoOpKind::Pread
                        | QueuedIoOpKind::Pwrite
                        | QueuedIoOpKind::Pwritev
                        | QueuedIoOpKind::Sync
                )
            {
                enabled.push(Action::Fail(request.id.clone()));
            }
        }
        if world.io.has_underlying_completions() {
            enabled.push(Action::CompleteUnderlying);
        }
        let busy = world.actors.iter().any(|actor| actor.active.is_some());
        if enabled.is_empty() {
            check!(
                !busy,
                "stuck_state",
                "no action is possible but work remains: {}",
                wait_graph(world)
            );
            break;
        }
        let action = trace.choose(&enabled);
        let changed = match action.clone() {
            Action::Start(actor) => start(trace, world, actor),
            Action::Poll(actor) => poll(trace, world, actor),
            Action::Drop(actor) => {
                cancel(trace, world, actor);
                true
            }
            Action::Complete(id) => {
                world.io.set_owner("scheduler");
                world.io.step_request(&id, false).unwrap().unwrap();
                true
            }
            Action::Fail(id) => {
                world.io.set_owner("scheduler");
                let event = world.io.step_request(&id, true).unwrap().unwrap();
                world.fault_injected = true;
                cover(format!("fault.{:?}", event.kind));
                trace.observe(format!("injected failure into {id}"));
                true
            }
            Action::CompleteUnderlying => {
                world.io.set_owner("scheduler");
                world.io.step_underlying().unwrap();
                cover("io.underlying_completion");
                true
            }
        };
        if changed {
            world.epoch += 1;
        }
        world.io.set_owner("scheduler");
        trace.after_action(&world.io);
    }
}

fn request_can_fail(world: &World, id: &QueuedIoRequestId) -> bool {
    let Some((actor, index)) = parse_owner(&id.owner) else {
        return true;
    };
    if world.actors[actor].transaction.is_some() {
        return false;
    }
    let setup = world.spec.setup[actor].len();
    if index < setup {
        return true;
    }
    world.spec.scripts[actor]
        .get(index - setup)
        .is_none_or(|command| command.can_cancel())
}

fn wait_graph(world: &World) -> String {
    let actors: Vec<String> = world
        .actors
        .iter()
        .enumerate()
        .filter_map(|(index, actor)| {
            actor.active.as_ref().map(|active| {
                format!(
                    "a{index} {} pending={:?} waiting_since={:?}",
                    active.command.encode(),
                    active.pending.as_ref().map(|pending| pending.finished()),
                    active.waiting_since
                )
            })
        })
        .collect();
    format!(
        "actors [{}], queued [{}], epoch {}",
        actors.join("; "),
        world
            .io
            .pending_events()
            .iter()
            .map(|event| format!("{}@{}", event.id, event.serial))
            .collect::<Vec<_>>()
            .join(", "),
        world.epoch
    )
}

/// Starts an actor's next command. Returns whether anything changed.
fn start(trace: &mut Trace, world: &mut World, actor: usize) -> bool {
    let index = world.spec.setup[actor].len() + world.actors[actor].next;
    let command = world.spec.scripts[actor][world.actors[actor].next];
    world.actors[actor].next += 1;
    let owner = operation_owner(actor, index);
    for (other, state) in world.actors.iter().enumerate() {
        if other != actor
            && state
                .active
                .as_ref()
                .and_then(|active| active.pending.as_ref())
                .is_some_and(|pending| !pending.finished())
        {
            cover("lifecycle.overlap");
            trace.observe(format!(
                "overlap: a{actor} started while a{other} waited on I/O"
            ));
            break;
        }
    }
    let allowed = match command {
        Command::OpenDatabase(cred, _) => match world.binding {
            Binding::Bound(..) => cred == Cred::Bound,
            Binding::Unbound | Binding::Plaintext => cred == Cred::Missing,
        },
        _ => world.authorized(actor),
    };
    let role = if allowed {
        "authorized"
    } else {
        "unauthorized"
    };
    set_operation(format!("{} {role}", command.kind()));
    world.io.set_owner(owner.clone());
    trace.persist();
    if world.actors[actor].connection.is_none()
        && !matches!(
            command,
            Command::OpenCached(_)
                | Command::OpenDatabase(..)
                | Command::ReadDatabaseMvStore
                | Command::Connect(_)
        )
    {
        sabotage_refusal(world, command);
        trace.observe(format!(
            "{owner} {} skipped: no connection",
            command.encode()
        ));
        return false;
    }
    let outcome = match command {
        Command::Connect(cred) => connect_command(world, actor, cred, false),
        Command::OpenCached(cred) => connect_command(world, actor, cred, true),
        Command::OpenDatabase(cred, api) => open_database_command(world, actor, cred, api),
        Command::ReadDatabaseMvStore => read_database_mv_store(world, actor),
        Command::ReadConnectionMvStore => read_connection_mv_store(world, actor),
        Command::MutateConnectionMvStore => mutate_connection_mv_store(world, actor),
        Command::ReadStatementMvStore => read_statement_mv_store(world, actor),
        Command::MutateStatementMvStore => mutate_statement_mv_store(world, actor),
        Command::ReadConnectionSchema => read_connection_schema(world, actor),
        Command::MutateConnectionSchema => mutate_connection_schema(world, actor),
        Command::ReadConnectionPager => read_connection_pager(world, actor),
        Command::ReadStatementPager => read_statement_pager(world, actor),
        Command::BeginWalInsert => begin_wal_insert(world, actor),
        Command::PublishSchemaAfterRestore => publish_schema_after_restore(world, actor),
        Command::ReloadWalAfterRestore => reload_wal_after_restore(world, actor),
        Command::Attach(cred) => attach_command(world, actor, cred),
        Command::Set(setting) => late_setting(world, actor, setting),
        Command::StepEarlySchema => match world.actors[actor].early.take() {
            Some(statement) => {
                cover(format!("lifecycle.early_schema_stepped.{role}"));
                world.actors[actor].active = Some(Active {
                    command,
                    owner: owner.clone(),
                    statement,
                    allowed,
                    pending: None,
                    rows: Vec::new(),
                    waiting_since: None,
                    unchanged_retries: 0,
                });
                "started".to_string()
            }
            None => "skipped: no early statement".to_string(),
        },
        Command::Read
        | Command::Schema
        | Command::TableInfo
        | Command::Insert(_)
        | Command::Checkpoint
        | Command::Begin
        | Command::Savepoint
        | Command::RollbackTo
        | Command::Release
        | Command::Commit
        | Command::Rollback => {
            let sql = match command {
                Command::Read => "SELECT value FROM secrets WHERE id = 0".to_string(),
                Command::Schema => "SELECT name FROM sqlite_schema WHERE name = 'secrets'".into(),
                Command::TableInfo => "PRAGMA table_info(secrets)".to_string(),
                Command::Insert(id) => {
                    format!("INSERT INTO secrets VALUES ({id}, 'protected value')")
                }
                Command::Checkpoint => "PRAGMA wal_checkpoint(TRUNCATE)".to_string(),
                Command::Begin => "BEGIN".to_string(),
                Command::Savepoint => "SAVEPOINT fuzz_savepoint".to_string(),
                Command::RollbackTo => "ROLLBACK TO fuzz_savepoint".to_string(),
                Command::Release => "RELEASE fuzz_savepoint".to_string(),
                Command::Commit => "COMMIT".to_string(),
                Command::Rollback => "ROLLBACK".to_string(),
                _ => unreachable!(),
            };
            cover(format!("lifecycle.{}.{role}", command.kind()));
            let connection = world.actors[actor].connection.clone().unwrap();
            match connection.prepare(&sql) {
                Ok(statement) => {
                    world.actors[actor].active = Some(Active {
                        command,
                        owner: owner.clone(),
                        statement,
                        allowed,
                        pending: None,
                        rows: Vec::new(),
                        waiting_since: None,
                        unchanged_retries: 0,
                    });
                    "prepared".to_string()
                }
                Err(error) => {
                    check_error(world, command, allowed, &error, "prepare");
                    if !allowed {
                        sabotage_refusal(world, command);
                    }
                    format!("prepare failed: {:?}", classify(&error))
                }
            }
        }
        Command::PrepareOnly(..) | Command::PrepareEarlySchema => {
            "skipped: configuration commands only run before setup".to_string()
        }
    };
    trace.observe(format!("{owner} {} -> {outcome}", command.encode()));
    true
}

/// Checks a failed engine call against the model.
fn check_error(world: &World, command: Command, allowed: bool, error: &LimboError, stage: &str) {
    let category = classify(error);
    if allowed {
        let tolerated = match category {
            Category::InjectedIo => world.fault_injected,
            Category::Contention => matches!(
                command,
                Command::Insert(_)
                    | Command::Checkpoint
                    | Command::Begin
                    | Command::Savepoint
                    | Command::RollbackTo
                    | Command::Release
                    | Command::Commit
                    | Command::Rollback
            ),
            _ => false,
        };
        check!(
            tolerated,
            "authorized_operation_failed",
            "{} failed at {stage}: {error}",
            command.encode()
        );
    } else {
        let refused = matches!(
            category,
            Category::CredentialMissing | Category::CredentialWrong | Category::CipherMismatch
        ) || (category == Category::InjectedIo && world.fault_injected);
        check!(
            refused,
            "unexpected_error",
            "unauthorized {} failed at {stage} with {error}",
            command.encode()
        );
        cover(format!("refusal.{category:?}"));
    }
}

/// Steps an actor's statement once.
fn poll(trace: &mut Trace, world: &mut World, actor: usize) -> bool {
    let mut active = world.actors[actor].active.take().unwrap();
    let role = if active.allowed {
        "authorized"
    } else {
        "unauthorized"
    };
    set_operation(format!("{} {role}", active.command.kind()));
    world.io.set_owner(active.owner.clone());
    if let Some(pending) = &active.pending {
        if let Some(error) = pending.get_error() {
            trace.observe(format!("{} completion error {error:?}", active.owner));
        }
    }
    active.pending = None;
    trace.persist();
    if let (Command::Insert(id), true) = (active.command, active.allowed) {
        world.possible.insert(id);
    }
    let result = active.statement.step();
    let mut changed = true;
    let finished = match &result {
        Ok(StepResult::Row) => {
            let column = usize::from(active.command == Command::TableInfo);
            let unauthorized_kind = match active.command {
                Command::Read => "unauthorized_row",
                Command::Schema | Command::TableInfo | Command::StepEarlySchema => {
                    "unauthorized_schema"
                }
                _ => "unauthorized_success",
            };
            check!(
                active.allowed,
                unauthorized_kind,
                "{} returned a row to a connection without the established key",
                active.command.encode()
            );
            if active.command != Command::Checkpoint {
                active.rows.push(
                    active
                        .statement
                        .row()
                        .unwrap()
                        .get::<String>(column)
                        .unwrap(),
                );
            }
            false
        }
        Ok(StepResult::Done) => {
            check!(
                active.allowed,
                if matches!(active.command, Command::Insert(_)) {
                    "unauthorized_write"
                } else {
                    "unauthorized_success"
                },
                "{} succeeded on a connection without the established key",
                active.command.encode()
            );
            let expected: &[&str] = match active.command {
                Command::Read => &["protected value"],
                Command::Schema | Command::StepEarlySchema => &["secrets"],
                Command::TableInfo => &["id", "value"],
                _ => &[],
            };
            if matches!(
                active.command,
                Command::Insert(_)
                    | Command::Begin
                    | Command::Savepoint
                    | Command::RollbackTo
                    | Command::Release
                    | Command::Commit
                    | Command::Rollback
            ) {
                finish_transaction_command(world, actor, active.command);
            } else if active.command != Command::Checkpoint {
                check!(
                    active.rows == expected,
                    "wrong_result",
                    "{} returned {:?}",
                    active.command.encode(),
                    active.rows
                );
            }
            true
        }
        Ok(StepResult::IO) => {
            let pending = active.statement.io_completions();
            if pending
                .as_ref()
                .is_some_and(|pending| pending.is_explicit_yield())
            {
                active.waiting_since = Some(world.epoch);
                cover("lifecycle.explicit_yield");
                changed = false;
            } else {
                active.waiting_since = None;
                active.pending = pending;
            }
            false
        }
        Ok(StepResult::Yield) | Ok(StepResult::Sleep { .. }) => {
            active.waiting_since = Some(world.epoch);
            active.unchanged_retries += 1;
            cover("lifecycle.explicit_yield");
            check!(
                active.unchanged_retries <= MAX_UNCHANGED_RETRIES,
                "stuck_state",
                "{} yielded {} times without progress: {}",
                active.command.encode(),
                active.unchanged_retries,
                wait_graph(world)
            );
            changed = false;
            false
        }
        Ok(StepResult::Busy) => {
            check!(
                matches!(
                    active.command,
                    Command::Insert(_)
                        | Command::Checkpoint
                        | Command::Begin
                        | Command::Savepoint
                        | Command::RollbackTo
                        | Command::Release
                        | Command::Commit
                        | Command::Rollback
                ),
                "authorized_operation_failed",
                "{} was busy",
                active.command.encode()
            );
            cover("lifecycle.contention");
            true
        }
        Ok(StepResult::Interrupt) => {
            check!(false, "unexpected_error", "statement was interrupted");
            true
        }
        Err(error) => {
            check_error(world, active.command, active.allowed, error, "step");
            true
        }
    };
    if !matches!(result, Ok(StepResult::Yield) | Ok(StepResult::Sleep { .. })) {
        active.unchanged_retries = 0;
    }
    trace.observe(format!(
        "{} step -> {}",
        active.owner,
        match &result {
            Ok(step) => format!("{step:?}"),
            Err(error) => format!("error {:?}", classify(error)),
        }
    ));
    if finished && !active.allowed {
        sabotage_refusal(world, active.command);
    }
    if !finished {
        world.actors[actor].active = Some(active);
    }
    changed
}

fn finish_transaction_command(world: &mut World, actor: usize, command: Command) {
    match command {
        Command::Begin => {
            world.actors[actor].transaction = Some(TransactionModel::default());
            cover("lifecycle.transaction.begin");
        }
        Command::Insert(id) => {
            if let Some(transaction) = &mut world.actors[actor].transaction {
                transaction.inserts.insert(id);
                cover("lifecycle.transaction.write");
            } else {
                world.acknowledged.insert(id);
                cover("lifecycle.acknowledged_write");
            }
        }
        Command::Savepoint => {
            if let Some(transaction) = &mut world.actors[actor].transaction {
                transaction.savepoint = Some(transaction.inserts.clone());
                cover("lifecycle.transaction.savepoint");
            }
        }
        Command::RollbackTo => {
            if let Some(transaction) = &mut world.actors[actor].transaction {
                if let Some(snapshot) = transaction.savepoint.clone() {
                    for id in transaction.inserts.difference(&snapshot) {
                        world.possible.remove(id);
                    }
                    transaction.inserts = snapshot;
                    cover("lifecycle.transaction.rollback_to");
                }
            }
        }
        Command::Release => {
            if let Some(transaction) = &mut world.actors[actor].transaction {
                transaction.savepoint = None;
                cover("lifecycle.transaction.release");
            }
        }
        Command::Commit => {
            if let Some(transaction) = world.actors[actor].transaction.take() {
                world.acknowledged.extend(transaction.inserts);
                cover("lifecycle.transaction.commit");
            }
        }
        Command::Rollback => {
            if let Some(transaction) = world.actors[actor].transaction.take() {
                for id in transaction.inserts {
                    world.possible.remove(&id);
                }
                cover("lifecycle.transaction.rollback");
            }
        }
        _ => unreachable!(),
    }
}

/// Test-only: pretends a refused operation returned protected data, so the
/// harness checks can show the oracle reports it.
fn sabotage_refusal(world: &World, command: Command) {
    match (world.spec.sabotage, command) {
        (Some(Sabotage::ForgeRow), Command::Read) => check!(
            false,
            "unauthorized_row",
            "sabotage: pretend the refused read returned the marker"
        ),
        (Some(Sabotage::ForgeSchema), Command::TableInfo) => check!(
            false,
            "unauthorized_schema",
            "sabotage: pretend the refused table_info returned columns"
        ),
        _ => {}
    }
}

fn cancel(trace: &mut Trace, world: &mut World, actor: usize) {
    let active = world.actors[actor].active.take().unwrap();
    world.io.set_owner(active.owner.clone());
    let phase = match &active.pending {
        Some(pending) if !pending.finished() => "waiting on I/O",
        _ if active.waiting_since.is_some() => "waiting on another actor",
        _ => "between steps",
    };
    cover(format!("lifecycle.cancel.{phase}"));
    trace.observe(format!("{} dropped while {phase}", active.owner));
    drop(active);
}

fn connect_command(world: &mut World, actor: usize, cred: Cred, open: bool) -> String {
    // A connection takes only a key; its cipher comes from the database. A
    // wrong-cipher credential therefore carries the right key bytes when it is
    // used to connect, and only an open can present a wrong cipher.
    let allowed = match world.binding {
        Binding::Bound(..) => cred == Cred::Bound || (!open && cred == Cred::WrongCipher),
        _ => cred == Cred::Missing,
    };
    let kind = if open { "open" } else { "connect" };
    cover(format!("lifecycle.{kind}.{cred:?}"));
    let key = world.connect_key(cred);
    let result = if open {
        let alive = world.any_database().is_some();
        cover(if alive {
            "lifecycle.open.cached"
        } else {
            "lifecycle.open.cold"
        });
        Database::open(world.io.clone(), &world.path, world.options(cred)).and_then(|db| {
            db.connect(ConnectOpts::new().with_encryption_key(key))
                .map(|connection| (Some(db), connection))
        })
    } else {
        match world.any_database() {
            Some(db) => db
                .connect(ConnectOpts::new().with_encryption_key(key))
                .map(|connection| (None, connection)),
            None => return "skipped: no open database".to_string(),
        }
    };
    match result {
        Ok((db, connection)) => {
            check!(
                allowed,
                "unauthorized_connection",
                "{kind} with {cred:?} succeeded on {:?}",
                world.binding
            );
            let (cipher, key) = world.bound();
            let settings = match world.binding {
                Binding::Bound(..) => Settings {
                    cipher: Some(cipher),
                    key: Some(key),
                    installed: true,
                },
                _ => Settings::default(),
            };
            discard_actor_transaction(world, actor);
            let state = &mut world.actors[actor];
            state.connection = Some(connection);
            if db.is_some() {
                state.database = db;
                state.database_authorized = allowed;
            }
            state.early = None;
            state.settings = settings;
            "connected".to_string()
        }
        Err(error) => {
            let category = classify(&error);
            if allowed {
                check!(
                    category == Category::InjectedIo && world.fault_injected,
                    "authorized_operation_failed",
                    "{kind} with the established key failed: {error}"
                );
            } else {
                check_credential_refusal(world, &error, &format!("{kind} with {cred:?}"));
                cover(format!("refusal.{category:?}"));
            }
            format!("refused: {category:?}")
        }
    }
}

fn discard_actor_transaction(world: &mut World, actor: usize) {
    let Some(transaction) = world.actors[actor].transaction.take() else {
        return;
    };
    for id in transaction.inserts {
        world.possible.remove(&id);
    }
}

fn open_database_command(world: &mut World, actor: usize, cred: Cred, api: OpenApi) -> String {
    let allowed = match world.binding {
        Binding::Bound(..) => cred == Cred::Bound,
        Binding::Unbound | Binding::Plaintext => cred == Cred::Missing,
    };
    let cached = world.any_database().is_some();
    cover(format!(
        "lifecycle.database_open.{api:?}.{cred:?}.{}",
        if cached { "cached" } else { "cold" }
    ));
    let result = match api {
        OpenApi::Sync => Database::open(world.io.clone(), &world.path, world.options(cred)),
        OpenApi::Async => open_database_async(world, cred),
    };
    match result {
        Ok(database) => {
            check!(
                allowed,
                "unauthorized_database_open",
                "{api:?} Database::open with {cred:?} returned protected shared state"
            );
            let state = &mut world.actors[actor];
            state.database = Some(database);
            state.database_authorized = true;
            "opened database".to_string()
        }
        Err(error) => {
            let category = classify(&error);
            if allowed {
                check!(
                    category == Category::InjectedIo && world.fault_injected,
                    "authorized_operation_failed",
                    "{api:?} Database::open with {cred:?} failed: {error}"
                );
            } else {
                check_credential_refusal(
                    world,
                    &error,
                    &format!("{api:?} Database::open with {cred:?}"),
                );
                cover(format!("refusal.{category:?}"));
            }
            format!("refused: {category:?}")
        }
    }
}

fn open_database_async(world: &World, cred: Cred) -> turso_core::Result<Arc<Database>> {
    let file = world.io.open_file(&world.path, OpenFlags::Create, false)?;
    let options =
        world
            .options(cred)
            .storage(Arc::new(turso_core::storage::database::DatabaseFile::new(
                file,
            )));
    let mut state = OpenDbAsyncState::new();
    loop {
        match Database::open_async(&mut state, world.io.clone(), &world.path, &options)
            .map_err(|error| *error)?
        {
            IOResult::Done(database) => return Ok(database),
            IOResult::IO(pending) => pending.wait(world.io.as_ref())?,
        }
    }
}

fn read_database_mv_store(world: &World, actor: usize) -> String {
    let (database, allowed) = match &world.actors[actor].database {
        Some(database) => (database, world.actors[actor].database_authorized),
        None => return "skipped: no authenticated database handle".to_string(),
    };
    let exposed = database_mv_store_contains(database, b"protected value");
    cover(format!(
        "lifecycle.database_mv_store_read.{}",
        if allowed {
            "authorized"
        } else {
            "unauthorized"
        }
    ));
    check!(
        allowed || !exposed,
        "unauthorized_mv_store_read",
        "Database::get_mv_store exposed protected data without authenticated credentials"
    );
    if allowed && world.spec.mvcc {
        check!(
            database
                .get_mv_store()
                .as_ref()
                .is_some_and(|store| world.any_database().is_some_and(|db| db
                    .get_mv_store()
                    .as_ref()
                    .is_some_and(|shared| Arc::ptr_eq(store, shared)))),
            "sharing_broken",
            "an authenticated Database handle did not share the MVCC store"
        );
    }
    format!("protected marker exposed={exposed}")
}

fn read_connection_mv_store(world: &World, actor: usize) -> String {
    let connection = world.actors[actor].connection.as_ref().unwrap();
    let allowed = world.authorized(actor);
    let exposed = connection_mv_store_contains(connection, b"protected value");
    cover(format!(
        "lifecycle.connection_mv_store_read.{}",
        if allowed {
            "authorized"
        } else {
            "unauthorized"
        }
    ));
    check!(
        allowed || !exposed,
        "unauthorized_mv_store_read",
        "Connection::mv_store exposed protected data without authenticated credentials"
    );
    if allowed && world.spec.mvcc {
        check!(
            connection
                .mv_store()
                .as_ref()
                .is_some_and(|store| world.any_database().is_some_and(|db| db
                    .get_mv_store()
                    .as_ref()
                    .is_some_and(|shared| Arc::ptr_eq(store, shared)))),
            "sharing_broken",
            "an authenticated connection did not share the MVCC store"
        );
    }
    format!("protected marker exposed={exposed}")
}

fn mutate_connection_mv_store(world: &World, actor: usize) -> String {
    if world.authorized(actor) {
        return "skipped: authenticated connection".to_string();
    }
    let connection = world.actors[actor].connection.as_ref().unwrap();
    let changed =
        replace_connection_mv_store_value(connection, b"protected value", b"corrupted value");
    cover("lifecycle.connection_mv_store_mutate.unauthorized");
    check!(
        !changed,
        "unauthorized_mv_store_write",
        "Connection::mv_store let unauthenticated code change protected MVCC data"
    );
    "protected marker unavailable".to_string()
}

fn read_statement_mv_store(world: &World, actor: usize) -> String {
    let connection = world.actors[actor].connection.as_ref().unwrap();
    let prepared = if world.actors[actor].early.is_none() {
        match connection.prepare("SELECT 1") {
            Ok(statement) => Some(statement),
            Err(error) => {
                check_credential_refusal(world, &error, "Statement::mv_store preparation");
                return format!("refused: {:?}", classify(&error));
            }
        }
    } else {
        None
    };
    let statement = world.actors[actor]
        .early
        .as_ref()
        .or(prepared.as_ref())
        .unwrap();
    let allowed = world.authorized(actor);
    let exposed = statement_mv_store_contains(statement, b"protected value");
    cover(format!(
        "lifecycle.statement_mv_store_read.{}",
        if allowed {
            "authorized"
        } else {
            "unauthorized"
        }
    ));
    check!(
        allowed || !exposed,
        "unauthorized_mv_store_read",
        "Statement::mv_store exposed protected data without authenticated credentials"
    );
    format!("protected marker exposed={exposed}")
}

fn mutate_statement_mv_store(world: &World, actor: usize) -> String {
    if world.authorized(actor) {
        return "skipped: authenticated connection".to_string();
    }
    let connection = world.actors[actor].connection.as_ref().unwrap();
    let prepared = if world.actors[actor].early.is_none() {
        match connection.prepare("SELECT 1") {
            Ok(statement) => Some(statement),
            Err(error) => {
                check_credential_refusal(world, &error, "Statement::mv_store preparation");
                return format!("refused: {:?}", classify(&error));
            }
        }
    } else {
        None
    };
    let statement = world.actors[actor]
        .early
        .as_ref()
        .or(prepared.as_ref())
        .unwrap();
    let changed =
        replace_statement_mv_store_value(statement, b"protected value", b"corrupted value");
    cover("lifecycle.statement_mv_store_mutate.unauthorized");
    check!(
        !changed,
        "unauthorized_mv_store_write",
        "Statement::mv_store let unauthenticated code change protected MVCC data"
    );
    "protected marker unavailable".to_string()
}

fn read_connection_schema(world: &World, actor: usize) -> String {
    let connection = world.actors[actor].connection.as_ref().unwrap();
    let allowed = world.authorized(actor);
    let exposed = connection
        .current_schema()
        .has_table(turso_core::NamespaceId::ROOT, "secrets")
        || connection.with_schema(turso_core::MAIN_DB_ID, |schema| {
            schema.has_table(turso_core::NamespaceId::ROOT, "secrets")
        });
    cover(format!(
        "lifecycle.connection_schema_read.{}",
        if allowed {
            "authorized"
        } else {
            "unauthorized"
        }
    ));
    check!(
        allowed || !exposed,
        "unauthorized_schema",
        "Connection schema accessors exposed the protected table without authenticated credentials"
    );
    format!("protected schema exposed={exposed}")
}

fn mutate_connection_schema(world: &World, actor: usize) -> String {
    if world.authorized(actor) {
        return "skipped: authenticated connection".to_string();
    }
    let connection = world.actors[actor].connection.as_ref().unwrap();
    let changed = match connection.with_schema_mut(|schema| {
        schema
            .take_table(turso_core::NamespaceId::ROOT, "secrets")
            .is_some()
    }) {
        Ok(changed) => changed,
        Err(error) => {
            check_credential_refusal(world, &error, "Connection::with_schema_mut");
            return format!("refused: {:?}", classify(&error));
        }
    };
    cover("lifecycle.connection_schema_mutate.unauthorized");
    check!(
        !changed,
        "unauthorized_schema_write",
        "Connection::with_schema_mut let unauthenticated code remove the protected table"
    );
    "protected schema unavailable".to_string()
}

fn read_connection_pager(world: &World, actor: usize) -> String {
    let connection = world.actors[actor].connection.as_ref().unwrap();
    read_pager(
        world,
        actor,
        &connection.get_pager(),
        "Connection::get_pager",
    )
}

fn read_statement_pager(world: &World, actor: usize) -> String {
    let connection = world.actors[actor].connection.as_ref().unwrap();
    let prepared = if world.actors[actor].early.is_none() {
        match connection.prepare("SELECT 1") {
            Ok(statement) => Some(statement),
            Err(error) => {
                check_credential_refusal(world, &error, "Statement::get_pager preparation");
                return format!("refused: {:?}", classify(&error));
            }
        }
    } else {
        None
    };
    let statement = world.actors[actor]
        .early
        .as_ref()
        .or(prepared.as_ref())
        .unwrap();
    read_pager(world, actor, statement.get_pager(), "Statement::get_pager")
}

fn read_pager(world: &World, actor: usize, pager: &Arc<turso_core::Pager>, api: &str) -> String {
    let allowed = world.authorized(actor);
    let result = pager_contains(pager, world.io.as_ref(), b"protected value");
    cover(format!(
        "lifecycle.pager_read.{}",
        if allowed {
            "authorized"
        } else {
            "unauthorized"
        }
    ));
    match result {
        Ok(exposed) => {
            check!(
                allowed || !exposed,
                "unauthorized_page_read",
                "{api} exposed a decrypted protected page without authenticated credentials"
            );
            format!("protected marker exposed={exposed}")
        }
        Err(error) => {
            if allowed {
                check!(
                    world.fault_injected && classify(&error) == Category::InjectedIo,
                    "authorized_operation_failed",
                    "{api} failed for an authenticated connection: {error}"
                );
            } else {
                check_credential_refusal(world, &error, api);
            }
            format!("refused: {:?}", classify(&error))
        }
    }
}

fn pager_contains(
    pager: &Arc<turso_core::Pager>,
    io: &QueuedIo,
    expected: &[u8],
) -> turso_core::Result<bool> {
    loop {
        match pager.read_page(2).map_err(|error| *error)? {
            IOResult::Done((page, completion)) => {
                if let Some(completion) = completion {
                    io.wait_for_completion(completion)?;
                }
                let bytes = page.get_contents().as_ptr();
                return Ok(bytes
                    .windows(expected.len())
                    .any(|window| window == expected));
            }
            IOResult::IO(pending) => pending.wait(io)?,
        }
    }
}

fn begin_wal_insert(world: &World, actor: usize) -> String {
    if world.spec.plaintext {
        return "skipped: authenticated connection".to_string();
    }
    let connection = world.actors[actor].connection.as_ref().unwrap();
    cover("lifecycle.wal_insert_begin.unauthorized");
    match connection.wal_insert_begin() {
        Ok(()) => {
            let _ = connection.wal_insert_end(false);
            check!(
                false,
                "unauthorized_wal_write",
                "Connection::wal_insert_begin opened a raw write transaction without authenticated credentials"
            );
            "raw write transaction opened".to_string()
        }
        Err(error) => {
            check!(
                classify(&error) == Category::SettingsRejected,
                "unauthorized_wal_write",
                "unexpected preflight failure: {error}"
            );
            format!("refused: {:?}", classify(&error))
        }
    }
}

fn publish_schema_after_restore(world: &World, actor: usize) -> String {
    if world.spec.plaintext {
        return "skipped: authenticated connection".to_string();
    }
    let connection = world.actors[actor].connection.as_ref().unwrap();
    cover("lifecycle.publish_schema_after_restore.unauthorized");
    match connection
        .preflight_external_restore()
        .and_then(|permit| connection.publish_schema_after_external_restore(&permit))
    {
        Ok(()) => {
            check!(
                false,
                "unauthorized_restore_write",
                "Connection::publish_schema_after_external_restore replaced shared schema without authenticated credentials"
            );
            "shared schema published".to_string()
        }
        Err(error) => {
            check!(
                classify(&error) == Category::SettingsRejected,
                "unauthorized_restore_write",
                "unexpected preflight failure: {error}"
            );
            check_owner_after_restore(world, "publish_schema_after_external_restore");
            format!("refused: {:?}", classify(&error))
        }
    }
}

fn reload_wal_after_restore(world: &World, actor: usize) -> String {
    if world.spec.plaintext {
        return "skipped: authenticated connection".to_string();
    }
    let connection = world.actors[actor].connection.as_ref().unwrap();
    cover("lifecycle.reload_wal_after_restore.unauthorized");
    match connection
        .preflight_external_restore()
        .and_then(|permit| connection.reload_wal_after_external_restore(&permit))
    {
        Ok(()) => {
            check!(
                false,
                "unauthorized_restore_write",
                "Connection::reload_wal_after_external_restore changed shared WAL state without authenticated credentials"
            );
            "shared WAL reloaded".to_string()
        }
        Err(error) => {
            check!(
                classify(&error) == Category::SettingsRejected,
                "unauthorized_restore_write",
                "unexpected preflight failure: {error}"
            );
            check_owner_after_restore(world, "reload_wal_after_external_restore");
            format!("refused: {:?}", classify(&error))
        }
    }
}

fn check_owner_after_restore(world: &World, api: &str) {
    let Some(owner) = &world.owner else {
        return;
    };
    let rows = first_column(owner, "SELECT value FROM secrets WHERE id = 0");
    check!(
        matches!(&rows, Ok(rows) if rows == &["protected value"]),
        "unauthorized_restore_write",
        "{api} disturbed the authenticated owner: {rows:?}"
    );
}

fn database_mv_store_contains(database: &Database, expected: &[u8]) -> bool {
    let mv_store = database.get_mv_store();
    mv_store.as_ref().is_some_and(|mv_store| {
        mv_store.rows.iter().any(|entry| {
            entry.value().read().iter().any(|version| {
                version.data.as_ref().is_some_and(|data| {
                    data.windows(expected.len())
                        .any(|window| window == expected)
                })
            })
        })
    })
}

fn connection_mv_store_contains(connection: &Connection, expected: &[u8]) -> bool {
    let mv_store = connection.mv_store();
    mv_store.as_ref().is_some_and(|mv_store| {
        mv_store.rows.iter().any(|entry| {
            entry.value().read().iter().any(|version| {
                version.data.as_ref().is_some_and(|data| {
                    data.windows(expected.len())
                        .any(|window| window == expected)
                })
            })
        })
    })
}

fn statement_mv_store_contains(statement: &Statement, expected: &[u8]) -> bool {
    let mv_store = statement.mv_store();
    mv_store.as_ref().is_some_and(|mv_store| {
        mv_store.rows.iter().any(|entry| {
            entry.value().read().iter().any(|version| {
                version.data.as_ref().is_some_and(|data| {
                    data.windows(expected.len())
                        .any(|window| window == expected)
                })
            })
        })
    })
}

fn replace_connection_mv_store_value(
    connection: &Connection,
    current: &[u8],
    replacement: &[u8],
) -> bool {
    assert_eq!(current.len(), replacement.len());
    let mv_store = connection.mv_store();
    let Some(mv_store) = mv_store.as_ref() else {
        return false;
    };
    for entry in mv_store.rows.iter() {
        let mut versions = entry.value().write();
        for version in versions.iter_mut() {
            let Some(data) = version.data.as_ref() else {
                continue;
            };
            let Some(offset) = data
                .windows(current.len())
                .position(|window| window == current)
            else {
                continue;
            };
            let mut bytes = data.to_vec();
            bytes[offset..offset + replacement.len()].copy_from_slice(replacement);
            version.data = Some(Arc::from(bytes));
            return true;
        }
    }
    false
}

fn replace_statement_mv_store_value(
    statement: &Statement,
    current: &[u8],
    replacement: &[u8],
) -> bool {
    assert_eq!(current.len(), replacement.len());
    let mv_store = statement.mv_store();
    let Some(mv_store) = mv_store.as_ref() else {
        return false;
    };
    for entry in mv_store.rows.iter() {
        let mut versions = entry.value().write();
        for version in versions.iter_mut() {
            let Some(data) = version.data.as_ref() else {
                continue;
            };
            let Some(offset) = data
                .windows(current.len())
                .position(|window| window == current)
            else {
                continue;
            };
            let mut bytes = data.to_vec();
            bytes[offset..offset + replacement.len()].copy_from_slice(replacement);
            version.data = Some(Arc::from(bytes));
            return true;
        }
    }
    false
}

fn check_credential_refusal(world: &World, error: &LimboError, context: &str) {
    let category = classify(error);
    check!(
        matches!(
            category,
            Category::CredentialMissing | Category::CredentialWrong | Category::CipherMismatch
        ) || (category == Category::InjectedIo && world.fault_injected),
        "unexpected_error",
        "{context} failed with {error}"
    );
}

fn attach_command(world: &mut World, actor: usize, cred: AttachCred) -> String {
    let connection = world.actors[actor].connection.clone().unwrap();
    let primary = world.authorized(actor);
    let (cipher, key) = world.bound();
    let target_key = if key == 1 { 2 } else { 1 };
    let uri = match cred {
        AttachCred::Missing => world.target_path.clone(),
        AttachCred::PrimaryKey => format!(
            "file:{}?cipher={cipher}&hexkey={}",
            world.target_path,
            encryption_options(cipher, key).hexkey
        ),
        AttachCred::TargetKey => format!(
            "file:{}?cipher={cipher}&hexkey={}",
            world.target_path,
            encryption_options(cipher, target_key).hexkey
        ),
        AttachCred::WrongCipher => format!(
            "file:{}?cipher={}&hexkey={}",
            world.target_path,
            world.wrong_cipher(),
            encryption_options(world.wrong_cipher(), target_key).hexkey
        ),
    };
    let allowed = primary && cred == AttachCred::TargetKey;
    cover(format!(
        "lifecycle.attach.{cred:?}.{}",
        if world.spec.target_cached {
            "cached"
        } else {
            "cold"
        }
    ));
    let result = connection.execute(format!("ATTACH '{uri}' AS aux"));
    match result {
        Ok(()) => {
            check!(
                allowed,
                "unauthorized_attach",
                "ATTACH with {cred:?} succeeded (primary authorized: {primary})"
            );
            let rows: Vec<(String,)> = connection.exec_rows("SELECT value FROM aux.secrets");
            check!(
                rows == [("attached value".to_string(),)],
                "wrong_result",
                "attached database returned {rows:?}"
            );
            connection.execute("DETACH aux").unwrap();
            "attached and detached".to_string()
        }
        Err(error) => {
            let category = classify(&error);
            if allowed {
                check!(
                    category == Category::InjectedIo && world.fault_injected,
                    "authorized_operation_failed",
                    "ATTACH with the target key failed: {error}"
                );
            } else {
                check_credential_refusal(world, &error, &format!("ATTACH with {cred:?}"));
                cover(format!("refusal.attach.{category:?}"));
            }
            if primary {
                let aliases: Vec<(i64, String, String)> =
                    connection.exec_rows("PRAGMA database_list");
                check!(
                    !aliases.iter().any(|alias| alias.1 == "aux"),
                    "attach_alias_left",
                    "a refused ATTACH left the alias registered"
                );
                let rows: Vec<(String,)> =
                    connection.exec_rows("SELECT value FROM secrets WHERE id = 0");
                check!(
                    rows == [("protected value".to_string(),)],
                    "authorized_operation_failed",
                    "the main database became unusable after a refused ATTACH"
                );
            }
            format!("refused: {category:?}")
        }
    }
}

/// Executes an encryption PRAGMA on an actor's existing connection after the
/// database holds data, such as a connection made before encryption that now
/// supplies a key.
fn late_setting(world: &mut World, actor: usize, setting: Setting) -> String {
    let connection = world.actors[actor].connection.clone().unwrap();
    let mut settings = world.actors[actor].settings;
    let mut binding = world.binding;
    let expected = settings.apply(
        setting,
        |choice| world.cipher_of(choice),
        &mut binding,
        world.mvcc_active,
    );
    let actual = connection.execute(setting_sql(world, setting));
    check_setting_outcome(&expected, &actual, setting);
    check!(
        binding == world.binding,
        "binding_changed",
        "the model let a late setting change the binding"
    );
    if expected.is_ok() {
        world.actors[actor].settings = settings;
    }
    cover(format!("lifecycle.late_setting.{expected:?}"));
    format!("{expected:?}")
}

/// Final checks: invalid handles stay refused, sharing works for valid ones,
/// and after every owner is gone a cold reopen sees exactly the permitted rows.
fn postconditions(trace: &mut Trace, mut world: World) {
    world.io.set_owner("post");
    set_operation("postconditions post");
    trace.persist();
    world.io.drain_all().unwrap();
    for actor in &mut world.actors {
        actor.active = None;
    }
    world.io.drain_all().unwrap();
    trace.after_action(&world.io);
    let binding = world.binding;
    for (index, actor) in world.actors.iter().enumerate() {
        let Some(connection) = &actor.connection else {
            continue;
        };
        if actor.settings.authorized(binding) {
            let rows = first_column(connection, "SELECT value FROM secrets WHERE id = 0");
            check!(
                matches!(&rows, Ok(rows) if rows == &["protected value"]),
                "authorized_operation_failed",
                "a{index} could not read the marker at the end: {rows:?}"
            );
            if world.spec.mvcc {
                check!(
                    connection.mv_store().as_ref().is_some_and(|store| world
                        .any_database()
                        .is_some_and(|db| db
                            .get_mv_store()
                            .as_ref()
                            .is_some_and(|shared| Arc::ptr_eq(store, shared)))),
                    "sharing_broken",
                    "a{index} did not share the MVCC store"
                );
            }
        } else {
            for sql in [
                "SELECT value FROM secrets WHERE id = 0",
                "SELECT name FROM sqlite_schema",
                "PRAGMA table_info(secrets)",
            ] {
                match connection.execute(sql) {
                    Ok(()) => check!(
                        false,
                        "unauthorized_success",
                        "a{index} without the established key ran {sql}"
                    ),
                    Err(error) => check_credential_refusal(
                        &world,
                        &error,
                        &format!("a{index} without the established key ran {sql}"),
                    ),
                }
            }
        }
    }
    let authorized: Vec<Arc<Connection>> = world
        .actors
        .iter()
        .filter(|actor| actor.settings.authorized(binding))
        .filter_map(|actor| actor.connection.clone())
        .collect();
    let check_connection = world.owner.clone().or_else(|| authorized.first().cloned());
    // With every statement gone, nothing may still hold a read lock.
    if let (Some(owner), false) = (check_connection.clone(), world.spec.mvcc) {
        let busy = first_column(&owner, "PRAGMA wal_checkpoint(TRUNCATE)")
            .map(|rows| rows.first().is_some_and(|busy| busy != "0"))
            .unwrap_or(true);
        trace.after_action(&world.io);
        check!(
            !busy,
            "read_lock_leaked",
            "a TRUNCATE checkpoint was busy after every statement finished"
        );
        cover("lifecycle.final_checkpoint");
    }
    // Valid connections still share the parsed schema.
    if let (Some(owner), Some(peer)) = (world.owner.clone(), authorized.first()) {
        owner.execute("CREATE TABLE shared_check(x)").unwrap();
        let rows: Vec<(String,)> =
            peer.exec_rows("SELECT name FROM sqlite_schema WHERE name = 'shared_check'");
        check!(
            rows == [("shared_check".to_string(),)],
            "sharing_broken",
            "a valid connection did not see the owner's new table"
        );
        cover("lifecycle.shared_schema_seen");
    }
    drop(check_connection);
    drop(authorized);
    if let (Some(Sabotage::LoseAcknowledgedWrite), Some(owner)) =
        (world.spec.sabotage, world.owner.clone())
    {
        world.acknowledged.insert(424_242);
        drop(owner);
    }
    if let (Some(Sabotage::ExtraWrite), Some(owner)) = (world.spec.sabotage, world.owner.clone()) {
        owner
            .execute("INSERT INTO secrets VALUES (777777, 'protected value')")
            .unwrap();
    }
    // Drop every owner of the main database and prove the instance is gone.
    let weak = world.any_database().map(|db| Arc::downgrade(&db));
    for actor in &mut world.actors {
        actor.early = None;
        if let Some(connection) = actor.connection.take() {
            connection.close().unwrap();
        }
        actor.database = None;
    }
    if let Some(owner) = world.owner.take() {
        owner.close().unwrap();
    }
    world.database = None;
    world.io.drain_all().unwrap();
    if let Some(weak) = weak {
        check!(
            weak.strong_count() == 0,
            "database_kept_alive",
            "the main database instance has {} strong references after every connection, statement and handle was dropped",
            weak.strong_count()
        );
    }
    let (options, key) = match binding {
        Binding::Bound(cipher, key) => (
            world
                .base
                .clone()
                .encryption(encryption_options(cipher, key)),
            Some(self::key(cipher, key)),
        ),
        _ => (world.base.clone(), None),
    };
    let reopened = Database::open(world.io.clone(), &world.path, options).unwrap();
    let recovery = reopened
        .connect(ConnectOpts::new().with_encryption_key(key))
        .unwrap();
    cover("lifecycle.cold_reopen");
    let rows: Vec<(i64, String)> = recovery.exec_rows("SELECT id, value FROM secrets ORDER BY id");
    trace.after_action(&world.io);
    let survivors: BTreeSet<i64> = rows.iter().map(|row| row.0).collect();
    for id in &world.acknowledged {
        check!(
            survivors.contains(id),
            "acknowledged_write_lost",
            "acknowledged insert {id} is missing after reopening"
        );
    }
    for (id, value) in &rows {
        check!(
            world.possible.contains(id),
            "unexpected_write_survived",
            "row {id} was never inserted by an authorized connection"
        );
        check!(
            value == "protected value",
            "wrong_result",
            "row {id} has value {value}"
        );
    }
    cover(format!(
        "lifecycle.uncertain_writes={}",
        world.possible.len() - world.acknowledged.len().min(world.possible.len())
    ));
    let integrity: Vec<(String,)> = recovery.exec_rows("PRAGMA integrity_check");
    check!(
        integrity == [("ok".to_string(),)],
        "integrity_failed",
        "integrity check after reopening: {integrity:?}"
    );
    if let Binding::Bound(cipher, bound_key) = binding {
        let other = if bound_key == 1 { 2 } else { 1 };
        check_connection_refusal(
            &world,
            reopened.connect(ConnectOpts::new()),
            "the reopened database accepted a keyless connection",
        );
        check_connection_refusal(
            &world,
            reopened
                .connect(ConnectOpts::new().with_encryption_key(Some(self::key(cipher, other)))),
            "the reopened database accepted a different key",
        );
        check_database_open_refusal(
            &world,
            Database::open(world.io.clone(), &world.path, world.base.clone()),
            "the reopened database opened without credentials",
        );
    }
    trace.observe(format!(
        "recovered {} rows ({} acknowledged, {} possible)",
        rows.len(),
        world.acknowledged.len(),
        world.possible.len()
    ));
    cover("lifecycle.recovered");
    trace.after_action(&world.io);
}

fn check_connection_refusal(
    world: &World,
    result: turso_core::Result<Arc<Connection>>,
    context: &str,
) {
    match result {
        Ok(_) => check!(false, "unauthorized_connection", "{context}"),
        Err(error) => check_credential_refusal(world, &error, context),
    }
}

fn check_database_open_refusal(
    world: &World,
    result: turso_core::Result<Arc<Database>>,
    context: &str,
) {
    match result {
        Ok(_) => check!(false, "unauthorized_database_open", "{context}"),
        Err(error) => check_credential_refusal(world, &error, context),
    }
}

/// Runs a query and returns its first column as text.
pub(super) fn first_column(
    connection: &Arc<Connection>,
    sql: &str,
) -> turso_core::Result<Vec<String>> {
    let mut statement = connection.prepare(sql)?;
    let mut rows = Vec::new();
    statement.run_with_row_callback(|row| {
        rows.push(
            row.get_values()
                .next()
                .map(|value| value.to_string())
                .unwrap_or_default(),
        );
        Ok(())
    })?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Harness checks: they show the recorder, replay, model and oracle behave as
// intended without relying on an engine bug.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod harness {
    use super::*;

    fn lifecycle_case(seed: u64) -> Case {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        loop {
            let case = Case::generate(&mut rng, seed, 2, &Json::Null);
            if let Family::Lifecycle(spec) = &case.family {
                if spec.start == Start::Earlier && !spec.plaintext && spec.scripts.len() >= 2 {
                    return case;
                }
            }
        }
    }

    fn run_random(case: &Case) -> (Vec<Action>, Vec<Vec<String>>, Terminal) {
        execute(
            case,
            Box::new(RandomChooser {
                rng: ChaCha8Rng::seed_from_u64(case.seeds.schedule),
                journal: Journal {
                    path: None,
                    case: case.clone(),
                },
            }),
        )
    }

    fn run_strict(case: &Case) -> Terminal {
        execute(
            case,
            Box::new(StrictChooser {
                decisions: case.decisions.clone(),
                outcomes: case.outcomes.clone(),
                compared: 0,
                journal: Journal {
                    path: None,
                    case: case.clone(),
                },
            }),
        )
        .2
    }

    #[test]
    fn public_api_commands_round_trip_through_artifacts() {
        for command in [
            Command::OpenDatabase(Cred::Missing, OpenApi::Sync),
            Command::OpenDatabase(Cred::OtherKey, OpenApi::Async),
            Command::ReadDatabaseMvStore,
            Command::ReadConnectionMvStore,
            Command::MutateConnectionMvStore,
            Command::ReadStatementMvStore,
            Command::MutateStatementMvStore,
            Command::ReadConnectionSchema,
            Command::MutateConnectionSchema,
            Command::ReadConnectionPager,
            Command::ReadStatementPager,
            Command::BeginWalInsert,
            Command::PublishSchemaAfterRestore,
            Command::ReloadWalAfterRestore,
            Command::Begin,
            Command::Savepoint,
            Command::RollbackTo,
            Command::Release,
            Command::Commit,
            Command::Rollback,
        ] {
            assert_eq!(Command::decode(&command.encode()).unwrap(), command);
        }
        assert_eq!(
            decode_action(&encode_action(&Action::CompleteUnderlying)).unwrap(),
            Action::CompleteUnderlying
        );
    }

    #[test]
    fn first_lifecycle_case_contains_the_public_accessor_sweep() {
        let mut rng = ChaCha8Rng::seed_from_u64(99);
        let case = Case::generate(&mut rng, 99, 2, &Json::Null);
        let Family::Lifecycle(spec) = case.family else {
            unreachable!()
        };
        let commands: BTreeSet<_> = spec.scripts.iter().flatten().map(Command::kind).collect();
        for required in [
            "database_open",
            "database_open_async",
            "database_mv_store_read",
            "connection_mv_store_read",
            "connection_mv_store_mutate",
            "statement_mv_store_read",
            "statement_mv_store_mutate",
            "connection_schema_read",
            "connection_schema_mutate",
            "connection_pager_read",
            "statement_pager_read",
            "publish_schema_after_restore",
            "reload_wal_after_restore",
        ] {
            assert!(commands.contains(required), "missing {required}");
        }
        assert_eq!(spec.setup[0], [Command::PrepareEarlySchema]);
    }

    #[test]
    fn second_lifecycle_case_contains_the_transaction_sweep() {
        let mut rng = ChaCha8Rng::seed_from_u64(99);
        for index in 0..=5 {
            let case = Case::generate(&mut rng, 99, index, &Json::Null);
            if index != 5 {
                continue;
            }
            let Family::Lifecycle(spec) = case.family else {
                unreachable!()
            };
            assert_eq!(
                spec.scripts[0],
                [
                    Command::Connect(Cred::Bound),
                    Command::Begin,
                    Command::Insert(2001),
                    Command::Savepoint,
                    Command::Insert(2002),
                    Command::RollbackTo,
                    Command::Release,
                    Command::Commit,
                ]
            );
        }
    }

    #[test]
    fn transaction_sweep_rolls_back_the_savepoint_write() {
        let mut rng = ChaCha8Rng::seed_from_u64(99);
        let mut case = None;
        for index in 0..=5 {
            let generated = Case::generate(&mut rng, 99, index, &Json::Null);
            if index == 5 {
                case = Some(generated);
            }
        }
        let base = case.unwrap();
        take_coverage();
        for faults in [false, true] {
            let mut case = base.clone();
            case.faults = faults;
            let (_, outcomes, terminal) = run_random(&case);
            assert_eq!(terminal, Terminal::Pass, "faults={faults}\n{outcomes:#?}");
        }
        let coverage = take_coverage();
        assert!(coverage["lifecycle.transaction.rollback_to"] >= 1);
        assert!(coverage["lifecycle.transaction.commit"] >= 1);
    }

    fn recorded(seed: u64) -> Case {
        let mut case = lifecycle_case(seed);
        let Family::Lifecycle(spec) = &mut case.family else {
            unreachable!()
        };
        spec.setup = vec![Vec::new(); 2];
        spec.scripts = vec![
            vec![
                Command::Connect(Cred::Bound),
                Command::Insert(1001),
                Command::Read,
            ],
            vec![Command::Connect(Cred::Bound), Command::Schema],
        ];
        let (decisions, outcomes, terminal) = run_random(&case);
        assert_eq!(
            terminal,
            Terminal::Pass,
            "seed {seed} must pass\n{}\n{outcomes:#?}",
            case.to_json()
        );
        case.decisions = decisions;
        case.outcomes = outcomes;
        case.terminal = Some(terminal);
        case
    }

    #[test]
    fn idle_actor_starts_while_another_waits_on_io() {
        let mut case = lifecycle_case(7);
        let Family::Lifecycle(spec) = &mut case.family else {
            unreachable!()
        };
        spec.setup = vec![Vec::new(); 2];
        spec.scripts = vec![
            vec![Command::Connect(Cred::Bound), Command::Insert(1)],
            vec![Command::Connect(Cred::Bound), Command::Read],
        ];
        spec.start = Start::Earlier;
        let (_, outcomes, terminal) = execute(
            &case,
            Box::new(PrefixChooser {
                prefix: vec![
                    Action::Start(0),
                    Action::Start(0),
                    Action::Poll(0),
                    Action::Start(1),
                ],
                journal: Journal {
                    path: None,
                    case: case.clone(),
                },
            }),
        );
        assert_eq!(terminal, Terminal::Pass);
        let start_first = outcomes
            .iter()
            .flatten()
            .position(|o| o.contains("overlap: a1 started while a0 waited"));
        assert!(start_first.is_some(), "no overlap recorded: {outcomes:?}");
    }

    #[test]
    fn a_never_stepped_insert_is_not_a_possible_write() {
        let mut case = lifecycle_case(8);
        let Family::Lifecycle(spec) = &mut case.family else {
            unreachable!()
        };
        spec.setup = vec![Vec::new()];
        spec.scripts = vec![vec![Command::Connect(Cred::Bound), Command::Insert(1)]];
        let (_, outcomes, terminal) = execute(
            &case,
            Box::new(PrefixChooser {
                prefix: vec![Action::Start(0), Action::Start(0), Action::Drop(0)],
                journal: Journal {
                    path: None,
                    case: case.clone(),
                },
            }),
        );
        assert_eq!(terminal, Terminal::Pass);
        assert!(
            outcomes
                .iter()
                .flatten()
                .any(|outcome| outcome == "recovered 1 rows (1 acknowledged, 1 possible)"),
            "a prepared insert was counted as a possible write: {outcomes:?}"
        );
    }

    #[test]
    fn cold_open_checks_for_read_locks_before_dropping_connections() {
        let mut case = lifecycle_case(9);
        let Family::Lifecycle(spec) = &mut case.family else {
            unreachable!()
        };
        spec.start = Start::Cold;
        spec.mvcc = false;
        spec.setup = vec![Vec::new()];
        spec.scripts = vec![vec![Command::OpenCached(Cred::Bound), Command::Read]];
        take_coverage();
        assert_eq!(run_random(&case).2, Terminal::Pass);
        assert_eq!(take_coverage().get("lifecycle.final_checkpoint"), Some(&1));
    }

    #[test]
    fn strict_replay_reproduces_a_passing_trace() {
        let case = recorded(11);
        assert_eq!(run_strict(&case), Terminal::Pass);
    }

    #[test]
    fn strict_replay_reports_a_removed_decision_as_divergence() {
        let mut case = recorded(12);
        let middle = case.decisions.len() / 2;
        case.decisions.remove(middle);
        assert!(matches!(run_strict(&case), Terminal::ReplayDiverged(_)));
    }

    #[test]
    fn strict_replay_reports_extra_decisions_as_divergence() {
        let mut case = recorded(13);
        case.decisions.push(Action::Poll(0));
        assert!(matches!(run_strict(&case), Terminal::ReplayDiverged(_)));
    }

    #[test]
    fn strict_replay_reports_a_changed_request_as_divergence() {
        let (mut case, index) = (14..)
            .find_map(|seed| {
                let case = recorded(seed);
                let index = case
                    .decisions
                    .iter()
                    .position(|action| matches!(action, Action::Complete(_)))?;
                Some((case, index))
            })
            .unwrap();
        case.decisions[index] = Action::Complete(QueuedIoRequestId {
            owner: "a9.c9".into(),
            ordinal: 0,
        });
        assert!(matches!(run_strict(&case), Terminal::ReplayDiverged(_)));
    }

    fn recorded_failure() -> Case {
        let mut case = lifecycle_case(26);
        let Family::Lifecycle(spec) = &mut case.family else {
            unreachable!()
        };
        spec.setup = vec![Vec::new(); 2];
        spec.scripts = vec![Vec::new(); 2];
        spec.sabotage = Some(Sabotage::ExtraWrite);
        let (decisions, outcomes, terminal) = run_random(&case);
        assert!(matches!(
            &terminal,
            Terminal::Violation { invariant, .. } if invariant == "unexpected_write_survived"
        ));
        case.decisions = decisions;
        case.outcomes = outcomes;
        case.terminal = Some(terminal);
        case
    }

    #[test]
    fn strict_replay_checks_the_last_observations_of_a_failure() {
        let mut case = recorded_failure();
        let recorded = case.terminal.as_ref().unwrap();
        assert!(run_strict(&case).same_failure(recorded));
        case.outcomes
            .last_mut()
            .unwrap()
            .push("changed outcome".into());
        assert!(matches!(run_strict(&case), Terminal::ReplayDiverged(_)));
    }

    #[test]
    fn strict_replay_rejects_unused_decisions_after_a_failure() {
        let mut case = recorded_failure();
        case.decisions.push(Action::Poll(99));
        assert!(matches!(run_strict(&case), Terminal::ReplayDiverged(_)));
    }

    #[test]
    fn strict_worker_rejects_a_different_build_or_model() {
        let build = build_identity();
        for field in ["model", "executable", "configuration", "backend"] {
            let directory = tempfile::tempdir().unwrap();
            let mut case = recorded(11);
            case.build = build.clone();
            case.build[field] = json!("different");
            let input = directory.path().join("case.json");
            write_atomic(&input, &case.to_json());
            let outcome = run_worker(
                directory.path(),
                WorkerMode::Strict(&input),
                &build,
                Instant::now() + Duration::from_secs(30),
            );
            let result = finished_result(directory.path(), outcome);
            assert!(
                matches!(
                    Terminal::from_json(&result["terminal"]),
                    Terminal::ReplayDiverged(_)
                ),
                "replay accepted a different {field}: {result}"
            );
            assert!(!directory.path().join("latest.json").exists());
        }
    }

    #[test]
    fn strict_worker_checks_input_before_setting_up_the_database() {
        let build = build_identity();
        let mut case = recorded(11);
        case.build = build.clone();
        for invalid in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let mut value = case.to_json();
            if invalid {
                value["parameters"]["setup"] = json!([]);
            }
            let input = directory.path().join("case.json");
            write_atomic(&input, &value);
            let result = finished_result(
                directory.path(),
                run_worker(
                    directory.path(),
                    WorkerMode::Strict(&input),
                    &build,
                    Instant::now() + Duration::from_secs(30),
                ),
            );
            let terminal = Terminal::from_json(&result["terminal"]);
            if invalid {
                assert!(
                    matches!(terminal, Terminal::ReplayDiverged(_)),
                    "{terminal:?}"
                );
                assert!(!directory.path().join("latest.json").exists());
            } else {
                assert_eq!(terminal, Terminal::Pass);
            }
        }
    }

    #[test]
    fn strict_worker_replays_page_one_and_rejects_an_absent_actor() {
        check_family_worker_round_trip(0);
    }

    #[test]
    fn strict_worker_replays_manager_and_rejects_an_absent_actor() {
        check_family_worker_round_trip(1);
    }

    fn check_family_worker_round_trip(index: u64) {
        let build = build_identity();
        let mut rng = ChaCha8Rng::seed_from_u64(11);
        let mut case = Case::generate(&mut rng, 11, index, &build);
        let (decisions, outcomes, terminal) = run_random(&case);
        assert_eq!(terminal, Terminal::Pass);
        case.decisions = decisions;
        case.outcomes = outcomes;
        case.terminal = Some(terminal);
        for invalid in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let mut value = case.to_json();
            if invalid {
                value["decisions"] = json!([["poll", 2]]);
                value["outcomes"] = json!([[], []]);
            }
            let input = directory.path().join("case.json");
            write_atomic(&input, &value);
            let result = finished_result(
                directory.path(),
                run_worker(
                    directory.path(),
                    WorkerMode::Strict(&input),
                    &build,
                    Instant::now() + Duration::from_secs(30),
                ),
            );
            let terminal = Terminal::from_json(&result["terminal"]);
            if invalid {
                assert!(
                    matches!(&terminal, Terminal::ReplayDiverged(message)
                    if message.contains("absent actor 2")),
                    "{terminal:?}"
                );
                assert!(!directory.path().join("latest.json").exists());
            } else {
                assert_eq!(terminal, Terminal::Pass, "{} replay", case.family.name());
            }
        }
    }

    #[test]
    fn strict_replay_rejects_a_changed_terminal_result() {
        let mut case = recorded_failure();
        case.terminal = Some(Terminal::Pass);
        let directory = tempfile::tempdir().unwrap();
        assert!(matches!(
            run_case(&mut case, RunMode::Strict, directory.path()),
            Terminal::ReplayDiverged(_)
        ));
    }

    #[test]
    fn build_compatibility_uses_the_executable_and_configuration() {
        let build = build_identity();
        let mut changed = build.clone();
        changed["commit"] = json!("a later commit of the same executable");
        changed["dirty_files"] = json!(0);
        assert!(compatible_build(&changed, &build).is_ok());
        changed.as_object_mut().unwrap().remove("executable");
        assert!(compatible_build(&changed, &build).is_err());
    }

    #[test]
    fn artifact_loading_rejects_invalid_actor_and_command_references() {
        let case = recorded(11);
        let mut value = case.to_json();
        value["parameters"]["setup"] = json!([]);
        assert!(Case::from_json(&value).is_err());
        value = case.to_json();
        value["decisions"] = json!([["start", 99]]);
        assert!(Case::from_json(&value).is_err());
        value = case.to_json();
        value["decisions"] = json!([["complete", "a0.c9999", 0]]);
        assert!(Case::from_json(&value).is_err());
        for actions in [
            json!([["poll", 0, "ignored"]]),
            json!([["complete", "a0.c0", u64::MAX]]),
        ] {
            value = case.to_json();
            value["decisions"] = actions;
            assert!(Case::from_json(&value).is_err());
        }
    }

    #[test]
    fn an_unverified_shrink_candidate_is_not_reported_as_minimized() {
        let directory = tempfile::tempdir().unwrap();
        let mut case = recorded_failure();
        let build = build_identity();
        case.build = build.clone();
        let original = directory.path().join("original.json");
        write_atomic(&original, &case.to_json());
        let summary = shrink(
            directory.path(),
            &original,
            &build,
            Instant::now() - Duration::from_secs(1),
        );
        assert!(!directory.path().join("minimized.json").exists());
        assert!(directory.path().join("unverified-candidate.json").exists());
        assert!(summary.contains("not verified"), "{summary}");
        assert!(original.exists());
    }

    #[test]
    fn shrinking_verifies_the_final_failure_in_two_fresh_workers() {
        let directory = tempfile::tempdir().unwrap();
        let mut case = recorded_failure();
        let build = build_identity();
        case.build = build.clone();
        let original = directory.path().join("original.json");
        write_atomic(&original, &case.to_json());
        let summary = shrink(
            directory.path(),
            &original,
            &build,
            Instant::now() + Duration::from_secs(30),
        );
        let minimized = load_case(&directory.path().join("minimized.json"));
        assert_eq!(minimized.actors(), 1);
        assert!(minimized
            .terminal
            .unwrap()
            .same_failure(case.terminal.as_ref().unwrap()));
        for run in 0..2 {
            let result: Json = serde_json::from_slice(
                &fs::read(
                    directory
                        .path()
                        .join(format!("final-verify-{run}/result.json")),
                )
                .unwrap(),
            )
            .unwrap();
            assert!(Terminal::from_json(&result["terminal"])
                .same_failure(case.terminal.as_ref().unwrap()));
        }
        assert!(
            summary.contains("minimized artifact verified (2 of 2"),
            "{summary}"
        );
        assert_eq!(load_case(&original).actors(), 2);
    }

    #[test]
    fn deleting_a_command_keeps_its_dependent_decisions_out() {
        let case = recorded(15);
        let smaller = case.without_command(0, 0).unwrap();
        assert!(smaller.decisions.len() < case.decisions.len());
        let (Terminal::Pass | Terminal::InvalidCandidate(_) | Terminal::Violation { .. }) =
            execute(
                &smaller,
                Box::new(PrefixChooser {
                    prefix: smaller.decisions.clone(),
                    journal: Journal {
                        path: None,
                        case: smaller.clone(),
                    },
                }),
            )
            .2
        else {
            panic!("a prefix candidate must not be reported as a replay divergence")
        };
        // The edited trace is not a recording: strict replay must refuse it.
        assert!(!matches!(run_strict(&smaller), Terminal::Pass));
    }

    fn sabotaged(seed: u64, sabotage: Sabotage, scripts: Vec<Vec<Command>>) -> Terminal {
        let mut case = lifecycle_case(seed);
        case.faults = sabotage == Sabotage::ForgeRow;
        let Family::Lifecycle(spec) = &mut case.family else {
            unreachable!()
        };
        spec.sabotage = Some(sabotage);
        spec.setup = vec![Vec::new(); scripts.len()];
        spec.scripts = scripts;
        run_random(&case).2
    }

    #[test]
    fn oracle_reports_a_forged_row_even_after_an_injected_failure() {
        let terminal = sabotaged(
            21,
            Sabotage::ForgeRow,
            vec![vec![Command::Insert(1)], vec![Command::Read]],
        );
        assert!(
            matches!(&terminal, Terminal::Violation { invariant, .. } if invariant == "unauthorized_row"),
            "{terminal:?}"
        );
    }

    #[test]
    fn oracle_reports_forged_schema_without_a_row_leak() {
        let terminal = sabotaged(
            22,
            Sabotage::ForgeSchema,
            vec![vec![Command::Read], vec![Command::TableInfo]],
        );
        assert!(
            matches!(&terminal, Terminal::Violation { invariant, .. } if invariant == "unauthorized_schema"),
            "{terminal:?}"
        );
    }

    #[test]
    fn oracle_reports_a_lost_acknowledged_write() {
        let terminal = sabotaged(
            23,
            Sabotage::LoseAcknowledgedWrite,
            vec![vec![Command::Read], vec![Command::Read]],
        );
        assert!(
            matches!(&terminal, Terminal::Violation { invariant, .. } if invariant == "acknowledged_write_lost"),
            "{terminal:?}"
        );
    }

    #[test]
    fn oracle_reports_a_write_nobody_made() {
        let terminal = sabotaged(
            24,
            Sabotage::ExtraWrite,
            vec![vec![Command::Read], vec![Command::Read]],
        );
        assert!(
            matches!(&terminal, Terminal::Violation { invariant, .. } if invariant == "unexpected_write_survived"),
            "{terminal:?}"
        );
    }

    #[test]
    fn same_failure_compares_invariants_not_messages() {
        let violation = |invariant: &str, message: &str| Terminal::Violation {
            invariant: invariant.into(),
            operation: "read unauthorized".into(),
            message: message.into(),
            decision: 3,
        };
        assert!(
            violation("unauthorized_row", "a").same_failure(&violation("unauthorized_row", "b"))
        );
        assert!(!violation("unauthorized_row", "a")
            .same_failure(&violation("unauthorized_schema", "a")));
        assert!(!violation("unclassified_panic", "assertion failed: x")
            .same_failure(&violation("unclassified_panic", "assertion failed: y")));
        assert!(
            !Terminal::ReplayDiverged("x".into()).same_failure(&violation("unauthorized_row", "x"))
        );
    }

    #[test]
    fn rejected_setting_leaves_the_model_unchanged() {
        let main = |_: CipherChoice| CipherMode::Aegis256;
        let mut binding = Binding::Bound(CipherMode::Aes256Gcm, 1);
        let mut settings = Settings {
            cipher: Some(CipherMode::Aegis256),
            key: None,
            installed: false,
        };
        let before = settings;
        assert_eq!(
            settings.apply(Setting::Key(1), main, &mut binding, false),
            Err(Category::SettingsRejected)
        );
        assert_eq!(settings, before);
        assert_eq!(binding, Binding::Bound(CipherMode::Aes256Gcm, 1));
        let mut installed = Settings {
            cipher: Some(CipherMode::Aes256Gcm),
            key: Some(2),
            installed: true,
        };
        assert_eq!(
            installed.apply(Setting::Key(1), main, &mut binding, false),
            Err(Category::SettingsRejected)
        );
        assert!(!installed.authorized(binding));
    }

    #[test]
    fn a_selected_failure_hits_exactly_that_request() {
        let io = QueuedIo::new();
        io.set_owner("x");
        let file = io
            .open_file("selected-failure", OpenFlags::Create, false)
            .unwrap();
        let write = |byte: u8| {
            let buffer = Arc::new(Buffer::new_temporary(16));
            buffer.as_mut_slice().fill(byte);
            file.pwrite(0, buffer, Completion::new_write(|_| {}))
                .unwrap()
        };
        let first = write(1);
        let second = write(2);
        let target = io.pending_events()[1].id.clone();
        io.step_request(&target, true).unwrap().unwrap();
        assert!(
            io.step_request(&target, false).unwrap().is_none(),
            "a failed request cannot run again"
        );
        io.drain_all().unwrap();
        assert!(first.succeeded());
        assert!(second.failed());
        let records = io.records();
        assert_eq!(records.iter().filter(|r| r.injected_error).count(), 1);
        assert_eq!(records[0].event.id, target);
    }

    #[test]
    fn a_cancelled_request_still_runs_when_dispatched() {
        let io = QueuedIo::new();
        let file = io
            .open_file("cancelled-request", OpenFlags::Create, false)
            .unwrap();
        let buffer = Arc::new(Buffer::new_temporary(16));
        buffer.as_mut_slice().fill(7);
        let completion = file
            .pwrite(0, buffer, Completion::new_write(|_| {}))
            .unwrap();
        io.cancel(std::slice::from_ref(&completion)).unwrap();
        io.drain_all().unwrap();
        let record = &io.records()[0];
        assert!(record.aborted_before_dispatch);
        assert_eq!(
            file.size().unwrap(),
            16,
            "the cancelled write still reached the file"
        );
    }

    #[cfg(feature = "io_memory_yield")]
    #[test]
    fn queued_io_schedules_dispatch_and_underlying_completion_separately() {
        let io = QueuedIo::new_with_io(Arc::new(turso_core::MemoryYieldIO::new()));
        let file = io
            .open_file("two-stage-completion", OpenFlags::Create, false)
            .unwrap();
        let completion = file
            .pwrite(
                0,
                Arc::new(Buffer::new_temporary(16)),
                Completion::new_write(|_| {}),
            )
            .unwrap();
        let request = io.pending_events()[0].id.clone();

        io.step_request(&request, false).unwrap().unwrap();
        assert!(!completion.finished());
        assert!(io.pending_events().is_empty());
        assert!(io.has_underlying_completions());

        io.step_underlying().unwrap();
        assert!(completion.succeeded());
        assert!(!io.has_underlying_completions());
    }

    #[test]
    fn strict_replay_checks_whether_a_request_was_aborted() {
        let io = QueuedIo::new();
        let file = io
            .open_file("aborted-replay", OpenFlags::Create, false)
            .unwrap();
        let completion = file
            .pwrite(
                0,
                Arc::new(Buffer::new_temporary(16)),
                Completion::new_write(|_| {}),
            )
            .unwrap();
        io.cancel(std::slice::from_ref(&completion)).unwrap();
        io.drain_all().unwrap();
        let mut trace = Trace::default();
        trace.after_action(&io);
        let observation = &trace.outcomes[0][0];
        assert!(
            observation.contains("aborted_before_dispatch=true"),
            "{observation}"
        );
        let case = lifecycle_case(11);
        let mut chooser = StrictChooser {
            decisions: Vec::new(),
            outcomes: trace.outcomes.clone(),
            compared: 0,
            journal: Journal { path: None, case },
        };
        assert!(chooser.finish(&[], &trace.outcomes).is_ok());
        chooser.compared = 0;
        chooser.outcomes[0][0] = observation.replace(
            "aborted_before_dispatch=true",
            "aborted_before_dispatch=false",
        );
        assert!(chooser.finish(&[], &trace.outcomes).is_err());
    }

    #[test]
    fn clearing_faults_disarms_an_unmatched_one_shot() {
        let io = QueuedIo::new();
        let file = io
            .open_file("cleared-one-shot", OpenFlags::Create, false)
            .unwrap();
        io.fail_next_request(QueuedIoOpKind::Sync);
        io.clear_fault();
        let completion = file
            .sync(
                Completion::new_sync(|_| {}),
                turso_core::io::FileSyncType::Fsync,
            )
            .unwrap();
        io.drain_all().unwrap();
        assert!(
            completion.succeeded(),
            "a cleared one-shot fault fired later"
        );
        assert!(io.records().iter().all(|record| !record.injected_error));
    }

    #[test]
    fn worker_rejects_a_result_from_a_failed_process() {
        let directory = tempfile::tempdir().unwrap();
        let outcome = run_worker_with(
            directory.path(),
            WorkerMode::Campaign,
            &Json::Null,
            Instant::now() + Duration::from_secs(20),
            Duration::from_secs(2),
            &[("ENCRYPTION_FUZZ_SELFTEST", "result_then_exit")],
        );
        assert!(matches!(outcome, WorkerOutcome::Crashed(_)));
    }

    #[test]
    fn worker_failures_are_reported_with_their_logs() {
        for (selftest, expect_timeout) in [("exit", false), ("hang", true)] {
            let directory = tempfile::tempdir().unwrap();
            let outcome = run_worker_with(
                directory.path(),
                WorkerMode::Campaign,
                &Json::Null,
                Instant::now() + Duration::from_secs(20),
                Duration::from_secs(2),
                &[("ENCRYPTION_FUZZ_SELFTEST", selftest)],
            );
            match outcome {
                WorkerOutcome::Timeout(_) => assert!(expect_timeout),
                WorkerOutcome::Crashed(_) => assert!(!expect_timeout),
                WorkerOutcome::Finished(result) => panic!("unexpected result {result}"),
            }
            assert!(directory.path().join("worker.log").exists());
        }
    }
}
