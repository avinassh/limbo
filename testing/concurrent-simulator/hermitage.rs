//! Hermitage anomaly workloads for the concurrent simulator.
//!
//! Each anomaly is a state machine that emits one Operation at a time
//! and receives the result of the previous operation. The simulator's
//! round-robin fiber scheduler creates random interleavings across fibers.
//!
//! Three anomalies are tested:
//! - P4 (Lost Update): fibers race to read-then-increment counters
//! - G-single (Read Skew): fibers transfer value between row pairs while
//!   readers verify a conservation invariant
//! - G1a (Aborted Reads): fibers write poison values and rollback while
//!   readers verify they never see aborted data

use std::sync::{Arc, Mutex};

use rand::Rng;
use rand_chacha::ChaCha8Rng;

use crate::operations::{OpResult, Operation};

// ============================================================================
// Workload trait
// ============================================================================

/// A hermitage workload instance (one per fiber, drives a single transaction).
///
/// Called with `next(None)` to get the first operation, then `next(Some(result))`
/// after each operation completes. Returns `None` when the workload is done.
pub trait HermitageWorkload {
    fn next(&mut self, result: Option<OpResult>) -> Option<Operation>;
}

/// Factory that produces hermitage workload instances.
pub trait HermitageWorkloadProfile {
    fn generate(&self, rng: ChaCha8Rng, fiber_id: usize) -> Box<dyn HermitageWorkload>;
}

// ============================================================================
// P4: Lost Update
// ============================================================================

/// Tracks committed increments per counter for P4 verification.
pub struct P4Tracker {
    pub committed_increments: Vec<u64>,
}

impl P4Tracker {
    pub fn new(num_counters: usize) -> Self {
        Self {
            committed_increments: vec![0; num_counters],
        }
    }
}

enum P4Phase {
    Begin,
    Select,
    Noise { remaining: usize },
    Update,
    Commit,
}

/// P4 (Lost Update) workload.
///
/// Each instance does:
/// 1. BEGIN CONCURRENT
/// 2. SELECT counter FROM hermitage_p4 WHERE id = ?
/// 3. Optional noise reads (stretches the timing window)
/// 4. UPDATE hermitage_p4 SET counter = <old+1> WHERE id = ?
/// 5. COMMIT
/// 6. Records the successful increment in the tracker
///
/// If any step hits an error (e.g. WriteWriteConflict), the workload
/// exits and the increment is NOT recorded.
struct P4Workload {
    phase: P4Phase,
    rng: ChaCha8Rng,
    num_counters: usize,
    counter_id: usize,
    old_value: Option<i64>,
    tracker: Arc<Mutex<P4Tracker>>,
}

impl HermitageWorkload for P4Workload {
    fn next(&mut self, result: Option<OpResult>) -> Option<Operation> {
        match self.phase {
            P4Phase::Begin => {
                self.counter_id = self.rng.random_range(0..self.num_counters);
                self.phase = P4Phase::Select;
                Some(Operation::Begin {
                    mode: "BEGIN CONCURRENT".into(),
                })
            }
            P4Phase::Select => {
                result?.ok()?; // bail if BEGIN failed
                self.phase = P4Phase::Noise {
                    remaining: self.rng.random_range(0..=3),
                };
                Some(Operation::Select {
                    sql: format!(
                        "SELECT counter FROM hermitage_p4 WHERE id = {}",
                        self.counter_id
                    ),
                })
            }
            P4Phase::Noise { remaining } => {
                let rows = result?.ok()?;
                // On first entry (from Select), capture the counter value
                if self.old_value.is_none() {
                    self.old_value = Some(
                        rows[0][0]
                            .as_int()
                            .expect("P4: counter column must be an integer"),
                    );
                }
                if remaining > 0 {
                    self.phase = P4Phase::Noise {
                        remaining: remaining - 1,
                    };
                    let noise_id = self.rng.random_range(0..self.num_counters);
                    Some(Operation::Select {
                        sql: format!("SELECT counter FROM hermitage_p4 WHERE id = {noise_id}"),
                    })
                } else {
                    self.phase = P4Phase::Update;
                    let old = self
                        .old_value
                        .expect("P4: old_value must be set before Update");
                    Some(Operation::Update {
                        sql: format!(
                            "UPDATE hermitage_p4 SET counter = {} WHERE id = {}",
                            old + 1,
                            self.counter_id
                        ),
                    })
                }
            }
            P4Phase::Update => {
                result?.ok()?; // bail on WriteWriteConflict
                self.phase = P4Phase::Commit;
                Some(Operation::Commit)
            }
            P4Phase::Commit => {
                result?.ok()?;
                self.tracker.lock().unwrap().committed_increments[self.counter_id] += 1;
                None
            }
        }
    }
}

pub struct P4Profile {
    num_counters: usize,
    tracker: Arc<Mutex<P4Tracker>>,
}

impl P4Profile {
    pub fn new(num_counters: usize, tracker: Arc<Mutex<P4Tracker>>) -> Self {
        Self {
            num_counters,
            tracker,
        }
    }
}

impl HermitageWorkloadProfile for P4Profile {
    fn generate(&self, rng: ChaCha8Rng, _fiber_id: usize) -> Box<dyn HermitageWorkload> {
        Box::new(P4Workload {
            phase: P4Phase::Begin,
            rng,
            num_counters: self.num_counters,
            counter_id: 0,
            old_value: None,
            tracker: self.tracker.clone(),
        })
    }
}

// ============================================================================
// G-single: Read Skew — Writer
// ============================================================================

enum GsingleWriterPhase {
    Begin,
    UpdateFirst,
    UpdateSecond,
    Commit,
    Done,
}

struct GsingleWriterWorkload {
    phase: GsingleWriterPhase,
    rng: ChaCha8Rng,
    num_pairs: usize,
    max_delta: i64,
    from: usize,
    to: usize,
    delta: i64,
}

impl HermitageWorkload for GsingleWriterWorkload {
    fn next(&mut self, result: Option<OpResult>) -> Option<Operation> {
        match self.phase {
            GsingleWriterPhase::Begin => {
                let pair = self.rng.random_range(0..self.num_pairs);
                let row_a = pair * 2;
                let row_b = pair * 2 + 1;
                self.delta = self.rng.random_range(1..=self.max_delta);
                if self.rng.random_bool(0.5) {
                    self.from = row_a;
                    self.to = row_b;
                } else {
                    self.from = row_b;
                    self.to = row_a;
                }
                self.phase = GsingleWriterPhase::UpdateFirst;
                Some(Operation::Begin {
                    mode: "BEGIN CONCURRENT".into(),
                })
            }
            GsingleWriterPhase::UpdateFirst => {
                result?.ok()?;
                self.phase = GsingleWriterPhase::UpdateSecond;
                Some(Operation::Update {
                    sql: format!(
                        "UPDATE hermitage_gsingle SET value = value - {} WHERE id = {}",
                        self.delta, self.from
                    ),
                })
            }
            GsingleWriterPhase::UpdateSecond => {
                result?.ok()?;
                self.phase = GsingleWriterPhase::Commit;
                Some(Operation::Update {
                    sql: format!(
                        "UPDATE hermitage_gsingle SET value = value + {} WHERE id = {}",
                        self.delta, self.to
                    ),
                })
            }
            GsingleWriterPhase::Commit => {
                result?.ok()?;
                self.phase = GsingleWriterPhase::Done;
                Some(Operation::Commit)
            }
            GsingleWriterPhase::Done => None,
        }
    }
}

pub struct GsingleWriterProfile {
    num_pairs: usize,
    max_delta: i64,
}

impl GsingleWriterProfile {
    pub fn new(num_pairs: usize, max_delta: i64) -> Self {
        Self {
            num_pairs,
            max_delta,
        }
    }
}

impl HermitageWorkloadProfile for GsingleWriterProfile {
    fn generate(&self, rng: ChaCha8Rng, _fiber_id: usize) -> Box<dyn HermitageWorkload> {
        Box::new(GsingleWriterWorkload {
            phase: GsingleWriterPhase::Begin,
            rng,
            num_pairs: self.num_pairs,
            max_delta: self.max_delta,
            from: 0,
            to: 0,
            delta: 0,
        })
    }
}

// ============================================================================
// G-single: Read Skew — Reader
// ============================================================================

enum GsingleReaderPhase {
    Begin,
    Select,
    Commit,
    Done,
}

struct GsingleReaderWorkload {
    phase: GsingleReaderPhase,
    fiber_id: usize,
    num_pairs: usize,
    expected_sum: i64,
}

impl HermitageWorkload for GsingleReaderWorkload {
    fn next(&mut self, result: Option<OpResult>) -> Option<Operation> {
        match self.phase {
            GsingleReaderPhase::Begin => {
                self.phase = GsingleReaderPhase::Select;
                Some(Operation::Begin {
                    mode: "BEGIN CONCURRENT".into(),
                })
            }
            GsingleReaderPhase::Select => {
                result?.ok()?;
                self.phase = GsingleReaderPhase::Commit;
                Some(Operation::Select {
                    sql: "SELECT id, value FROM hermitage_gsingle ORDER BY id".into(),
                })
            }
            GsingleReaderPhase::Commit => {
                let rows = result?.ok()?;
                // Check the conservation invariant
                for pair in 0..self.num_pairs {
                    let idx_a = pair * 2;
                    let idx_b = pair * 2 + 1;
                    if idx_b >= rows.len() {
                        break;
                    }
                    let a = rows[idx_a][1]
                        .as_int()
                        .expect("G-single: value column must be an integer");
                    let b = rows[idx_b][1]
                        .as_int()
                        .expect("G-single: value column must be an integer");
                    assert_eq!(
                        a + b,
                        self.expected_sum,
                        "G-SINGLE VIOLATION (read skew): pair {pair} has values ({a}, {b}), \
                         sum {} != expected {}. Fiber {} saw inconsistent snapshot.",
                        a + b,
                        self.expected_sum,
                        self.fiber_id,
                    );
                }
                self.phase = GsingleReaderPhase::Done;
                Some(Operation::Commit)
            }
            GsingleReaderPhase::Done => None,
        }
    }
}

pub struct GsingleReaderProfile {
    num_pairs: usize,
    expected_sum: i64,
}

impl GsingleReaderProfile {
    pub fn new(num_pairs: usize, expected_sum: i64) -> Self {
        Self {
            num_pairs,
            expected_sum,
        }
    }
}

impl HermitageWorkloadProfile for GsingleReaderProfile {
    fn generate(&self, _rng: ChaCha8Rng, fiber_id: usize) -> Box<dyn HermitageWorkload> {
        Box::new(GsingleReaderWorkload {
            phase: GsingleReaderPhase::Begin,
            fiber_id,
            num_pairs: self.num_pairs,
            expected_sum: self.expected_sum,
        })
    }
}

// ============================================================================
// G1a: Aborted Reads — Writer
// ============================================================================

enum G1aWriterPhase {
    Begin,
    Update,
    Rollback,
    Done,
}

struct G1aWriterWorkload {
    phase: G1aWriterPhase,
    rng: ChaCha8Rng,
    num_rows: usize,
}

impl HermitageWorkload for G1aWriterWorkload {
    fn next(&mut self, result: Option<OpResult>) -> Option<Operation> {
        match self.phase {
            G1aWriterPhase::Begin => {
                self.phase = G1aWriterPhase::Update;
                Some(Operation::Begin {
                    mode: "BEGIN CONCURRENT".into(),
                })
            }
            G1aWriterPhase::Update => {
                result?.ok()?;
                let row_id = self.rng.random_range(0..self.num_rows);
                self.phase = G1aWriterPhase::Rollback;
                Some(Operation::Update {
                    sql: format!("UPDATE hermitage_g1a SET value = -999 WHERE id = {row_id}"),
                })
            }
            G1aWriterPhase::Rollback => {
                result?.ok()?;
                // Always rollback — -999 must never be visible
                self.phase = G1aWriterPhase::Done;
                Some(Operation::Rollback)
            }
            G1aWriterPhase::Done => None,
        }
    }
}

pub struct G1aWriterProfile {
    num_rows: usize,
}

impl G1aWriterProfile {
    pub fn new(num_rows: usize) -> Self {
        Self { num_rows }
    }
}

impl HermitageWorkloadProfile for G1aWriterProfile {
    fn generate(&self, rng: ChaCha8Rng, _fiber_id: usize) -> Box<dyn HermitageWorkload> {
        Box::new(G1aWriterWorkload {
            phase: G1aWriterPhase::Begin,
            rng,
            num_rows: self.num_rows,
        })
    }
}

// ============================================================================
// G1a: Aborted Reads — Reader
// ============================================================================

enum G1aReaderPhase {
    Begin,
    Select,
    Commit,
    Done,
}

struct G1aReaderWorkload {
    phase: G1aReaderPhase,
    fiber_id: usize,
}

impl HermitageWorkload for G1aReaderWorkload {
    fn next(&mut self, result: Option<OpResult>) -> Option<Operation> {
        match self.phase {
            G1aReaderPhase::Begin => {
                self.phase = G1aReaderPhase::Select;
                Some(Operation::Begin {
                    mode: "BEGIN CONCURRENT".into(),
                })
            }
            G1aReaderPhase::Select => {
                result?.ok()?;
                self.phase = G1aReaderPhase::Commit;
                Some(Operation::Select {
                    sql: "SELECT id, value FROM hermitage_g1a ORDER BY id".into(),
                })
            }
            G1aReaderPhase::Commit => {
                let rows = result?.ok()?;
                for row in &rows {
                    let id = row[0].as_int().expect("G1a: id column must be an integer");
                    let value = row[1]
                        .as_int()
                        .expect("G1a: value column must be an integer");
                    assert_ne!(
                        value, -999,
                        "G1a VIOLATION (aborted read): saw poison value -999 for id {id}. \
                         Dirty read detected on fiber {}.",
                        self.fiber_id,
                    );
                }
                self.phase = G1aReaderPhase::Done;
                Some(Operation::Commit)
            }
            G1aReaderPhase::Done => None,
        }
    }
}

pub struct G1aReaderProfile;

impl HermitageWorkloadProfile for G1aReaderProfile {
    fn generate(&self, _rng: ChaCha8Rng, fiber_id: usize) -> Box<dyn HermitageWorkload> {
        Box::new(G1aReaderWorkload {
            phase: G1aReaderPhase::Begin,
            fiber_id,
        })
    }
}

// ============================================================================
// Setup and configuration
// ============================================================================

/// Configuration for the hermitage simulation.
pub struct HermitageConfig {
    pub num_counters: usize,
    pub num_pairs: usize,
    pub sum_per_pair: i64,
    pub num_g1a_rows: usize,
    pub max_transfer_delta: i64,
}

impl Default for HermitageConfig {
    fn default() -> Self {
        Self {
            num_counters: 5,
            num_pairs: 3,
            sum_per_pair: 30,
            num_g1a_rows: 5,
            max_transfer_delta: 10,
        }
    }
}

/// Returns the SQL statements needed to set up hermitage tables.
pub fn hermitage_setup_sql(config: &HermitageConfig) -> Vec<String> {
    let mut sql = Vec::new();

    // P4 table: counters starting at 0
    sql.push("CREATE TABLE hermitage_p4 (id INTEGER PRIMARY KEY, counter INTEGER)".into());
    for i in 0..config.num_counters {
        sql.push(format!(
            "INSERT INTO hermitage_p4 (id, counter) VALUES ({i}, 0)"
        ));
    }

    // G-single table: paired rows with sum invariant
    sql.push("CREATE TABLE hermitage_gsingle (id INTEGER PRIMARY KEY, value INTEGER)".into());
    let half = config.sum_per_pair / 2;
    let other_half = config.sum_per_pair - half;
    for pair in 0..config.num_pairs {
        let id_a = pair * 2;
        let id_b = pair * 2 + 1;
        sql.push(format!(
            "INSERT INTO hermitage_gsingle (id, value) VALUES ({id_a}, {half})"
        ));
        sql.push(format!(
            "INSERT INTO hermitage_gsingle (id, value) VALUES ({id_b}, {other_half})"
        ));
    }

    // G1a table: rows with normal values (poison = -999)
    sql.push("CREATE TABLE hermitage_g1a (id INTEGER PRIMARY KEY, value INTEGER)".into());
    for i in 0..config.num_g1a_rows {
        sql.push(format!(
            "INSERT INTO hermitage_g1a (id, value) VALUES ({i}, {value})",
            value = (i + 1) * 10
        ));
    }

    sql
}

/// Build all hermitage workload profiles with weights.
pub fn hermitage_profiles(
    config: &HermitageConfig,
    tracker: Arc<Mutex<P4Tracker>>,
) -> Vec<(f64, &'static str, Box<dyn HermitageWorkloadProfile>)> {
    vec![
        (
            3.0,
            "p4_increment",
            Box::new(P4Profile::new(config.num_counters, tracker)) as _,
        ),
        (
            2.0,
            "gsingle_writer",
            Box::new(GsingleWriterProfile::new(
                config.num_pairs,
                config.max_transfer_delta,
            )) as _,
        ),
        (
            3.0,
            "gsingle_reader",
            Box::new(GsingleReaderProfile::new(
                config.num_pairs,
                config.sum_per_pair,
            )) as _,
        ),
        (
            2.0,
            "g1a_writer",
            Box::new(G1aWriterProfile::new(config.num_g1a_rows)) as _,
        ),
        (2.0, "g1a_reader", Box::new(G1aReaderProfile) as _),
    ]
}
