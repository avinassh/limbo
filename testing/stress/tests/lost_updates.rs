#![cfg(shuttle)]

use shuttle::scheduler::RandomScheduler;
use shuttle::sync::Barrier;
use turso_core::{Database, MemoryIO};
use turso_stress::sync::atomic::{AtomicI64, Ordering};
use turso_stress::sync::Arc;
use turso_stress::thread;

fn shuttle_config() -> shuttle::Config {
    let mut config = shuttle::Config::default();
    config.stack_size *= 10;
    config.max_steps = shuttle::MaxSteps::FailAfter(10_000_000);
    config
}

fn setup_mvcc_db(schema: &str) -> Arc<Database> {
    let io = Arc::new(MemoryIO::new());
    let db = Database::open_file(io, ":memory:").unwrap();
    let conn = db.connect().unwrap();
    conn.execute("PRAGMA journal_mode = 'mvcc'").unwrap();
    if !schema.is_empty() {
        conn.execute(schema).unwrap();
    }
    db
}

fn query_i64(conn: &Arc<turso_core::Connection>, sql: &str) -> i64 {
    let mut stmt = conn.prepare(sql).unwrap();
    let rows = stmt.run_collect_rows().unwrap();
    rows[0][0].as_int().unwrap()
}

fn lost_updates_scenario(num_workers: usize, rounds: usize) {
    let db = Arc::new(setup_mvcc_db(
        "CREATE TABLE counter(id INTEGER PRIMARY KEY, val INTEGER);
         INSERT INTO counter VALUES(1, 0);",
    ));

    let total_committed = Arc::new(AtomicI64::new(0));

    for _round in 0..rounds {
        let barrier = Arc::new(Barrier::new(num_workers));
        let mut handles = Vec::new();

        for _ in 0..num_workers {
            let db = Arc::clone(&db);
            let barrier = Arc::clone(&barrier);
            let total_committed = Arc::clone(&total_committed);
            handles.push(thread::spawn(move || {
                let conn = db.connect().unwrap();
                barrier.wait();
                if conn.execute("BEGIN CONCURRENT").is_err() {
                    return;
                }
                if conn
                    .execute("UPDATE counter SET val = val + 1 WHERE id = 1")
                    .is_err()
                {
                    let _ = conn.execute("ROLLBACK");
                    return;
                }
                match conn.execute("COMMIT") {
                    Ok(_) => {
                        total_committed.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(_) => {
                        let _ = conn.execute("ROLLBACK");
                    }
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }

    let conn = db.connect().unwrap();
    let val = query_i64(&conn, "SELECT val FROM counter WHERE id = 1");
    let committed = total_committed.load(Ordering::SeqCst);
    assert_eq!(
        val, committed,
        "Lost updates! counter={val} but {committed} transactions committed successfully"
    );
}

#[test]
fn shuttle_test_lost_updates() {
    let scheduler = RandomScheduler::new(100);
    let runner = shuttle::Runner::new(scheduler, shuttle_config());
    runner.run(|| lost_updates_scenario(2, 3));
}

#[test]
fn shuttle_test_lost_updates_slow() {
    let scheduler = RandomScheduler::new(10);
    let runner = shuttle::Runner::new(scheduler, shuttle_config());
    runner.run(|| lost_updates_scenario(4, 20));
}
