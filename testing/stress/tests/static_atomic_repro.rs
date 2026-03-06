#![cfg(shuttle)]

/// A shuttle-instrumented atomic used as a `static`. Internally wraps RefCell,
/// which is not thread-safe. Two real OS threads accessing this concurrently
/// corrupt the RefCell borrow counter -> BorrowMutError / BorrowError.
static COUNTER: shuttle::sync::atomic::AtomicU32 = shuttle::sync::atomic::AtomicU32::new(0);

#[test]
fn static_shuttle_atomic_cross_thread_race() {
    // Spawn two real OS threads, each running its own shuttle runner,
    // both touching the same static shuttle atomic. This is exactly what
    // cargo does when it runs two #[test] functions in parallel.
    let handles: Vec<_> = (0..2)
        .map(|_| {
            std::thread::spawn(|| {
                let scheduler = shuttle::scheduler::RandomScheduler::new(100);
                let runner = shuttle::Runner::new(scheduler, Default::default());
                runner.run(|| {
                    COUNTER.fetch_add(1, shuttle::sync::atomic::Ordering::SeqCst);
                });
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }
}
