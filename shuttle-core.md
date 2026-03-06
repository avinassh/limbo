# Shuttle Internals & Turso Integration Notes

## How Shuttle Works

### Execution Model
- All shuttle coroutines run on a **single OS thread** (stackful coroutines via `corosensei` crate)
- `thread::switch()` is the primary yield point -- suspends the current coroutine, hands control to shuttle's scheduler
- `thread::spawn` and `shuttle::future::spawn` both create shuttle-managed coroutines, all on the same OS thread
- Shuttle's scheduler explores all possible interleavings by replaying with different scheduling decisions
- Each `shuttle::Runner::run` call owns its entire execution: it creates a fresh `ExecutionState`, runs all coroutines on the calling OS thread, and tears down when done
- `EXECUTION_STATE` is a `scoped_thread_local!` -- each OS thread gets its own, so two runners on two OS threads don't interfere with each other's scheduling state

### RefCell Internals and Thread Safety

`RefCell<T>` tracks borrows via a `Cell<isize>` counter stored inline in the struct:
- `0` = not borrowed
- positive = number of active immutable borrows
- `-1` = exclusively (mutably) borrowed

`Cell<isize>` uses **non-atomic** reads and writes. This means:
- **Single OS thread**: Always safe. Borrows are sequential, counter is consistent. This is the normal Rust guarantee.
- **Multiple OS threads**: Undefined behavior. Two threads can read/write the counter simultaneously, corrupting it. A thread may read a stale value (e.g., `0` when another thread just set it to `-1`), leading to spurious `BorrowMutError` or `BorrowError` panics -- or worse, two threads both believing they have exclusive access.

Within shuttle's single-OS-thread model, `RefCell` inside `Atomic<T>` is safe: coroutines yield only at `thread::switch()` points, and all `RefCell` borrows within `fetch_update` are taken and dropped between switch points.

### Shuttle-Instrumented `Atomic<T>` Internals

Confirmed from `shuttle/src/sync/atomic/mod.rs`:

```rust
struct Atomic<T> {
    inner: RefCell<T>,
    clock: RefCell<Option<VectorClock>>,
    signature: ResourceSignature,
}
```

Safety comment in source (lines 128-130, `shuttle/src/sync/atomic/mod.rs`):
> "Atomic is never actually passed across true threads, only across continuations. The RefCell<_> type therefore can't be preempted mid-bookkeeping-operation."

Separately, inside `Atomic::new()` (line 142), there is a TODO about a different concern -- whether initializing the vector clock to all-zeros in a `const fn` is sound:
> "TODO Check that the argument above is sound"

This TODO is about clock initialization, **not** about the `unsafe impl Sync`.

The `unsafe impl Sync for Atomic<T>` is justified **only** because shuttle guarantees all coroutines run on one OS thread. The safety invariant breaks if the `Atomic<T>` is placed in a `static` and accessed from multiple OS threads.

### `fetch_update` -- The Core Read-Modify-Write Path

```rust
fn fetch_update<F>(&self, set_order, fetch_order, mut f: F) -> Result<T, T> {
    maybe_warn_about_ordering(set_order);
    maybe_warn_about_ordering(fetch_order);

    thread::switch();          // ONLY yield point -- shuttle can schedule here
    self.exhale_clock();       // borrows self.clock temporarily (no switch inside)
    let current = *self.inner.borrow();
    let ret = if let Some(new) = f(current) {
        *self.inner.borrow_mut() = new;
        self.inhale_clock();   // borrows self.clock temporarily (no switch inside)
        Ok(current)
    } else {
        Err(current)
    };
    ret
}
```

`fetch_add` calls `fetch_update(order, order, |old| Some(old.wrapping_add(val)))`.

Key observations:
- There is only ONE `thread::switch()`, at the very top. After that, the entire read-modify-write runs without yielding.
- `exhale_clock` internally calls `init_clock()` (which does `self.clock.borrow_mut()`, dropped immediately) then borrows `self.clock` for pure `SmallVec` operations. No `thread::switch()` inside.
- `inhale_clock` follows the same pattern.
- All `RefCell` borrows on `self.clock` and `self.inner` are sequential and non-overlapping **within a single OS thread**. No coroutine can interleave between them because there's no yield point.

### `ExecutionState::with` and Re-entrancy

- `EXECUTION_STATE` is `scoped_thread_local!` -- per OS thread, not global
- `ExecutionState::with` calls `try_borrow_mut()` on it. If already borrowed, it **panics** ("already borrowed")
- `thread::switch()` calls `maybe_yield()` which calls `ExecutionState::with(...)`. Calling `switch()` from inside an `ExecutionState::with` closure would therefore panic
- `exhale_clock` and `inhale_clock` closures only do `SmallVec` operations (confirmed from `clock.rs`). They never call `thread::switch()`

---

## Root Cause: Static Shuttle Atomic + Parallel Cargo Test Threads

### The Bug

`core/storage/buffer_pool.rs:372`:
```rust
static NEXT_ID: AtomicU32 = AtomicU32::new(UNREGISTERED_START);
```

Under `--cfg shuttle`, `AtomicU32` resolves to `shuttle::sync::atomic::AtomicU32` (via `core/sync.rs` which re-exports `shuttle::sync::atomic`). This type wraps `RefCell<u32>` + `RefCell<Option<VectorClock>>`.

The test file `testing/stress/tests/lost_updates.rs` contains TWO `#[test]` functions:
- `shuttle_test_lost_updates` (100 iterations, 2 workers, 3 rounds)
- `shuttle_test_lost_updates_slow` (10 iterations, 4 workers, 20 rounds)

By default, `cargo test` runs `#[test]` functions on **separate OS threads in parallel**. Both test functions open databases, which triggers `Arena::new` -> `NEXT_ID.fetch_add`. Since `NEXT_ID` is a `static`, it is process-global -- both OS threads access the same `RefCell` instances concurrently.

### The Race

```
OS Thread 1 (shuttle_test_lost_updates)      OS Thread 2 (shuttle_test_lost_updates_slow)
--------------------------------------------  --------------------------------------------
NEXT_ID.fetch_add(1, SeqCst)
  fetch_update()
    self.exhale_clock()
      init_clock()
        self.clock.borrow_mut()              NEXT_ID.fetch_add(1, SeqCst)
        [Cell counter = -1 (mut borrow)]       fetch_update()
        (drops borrow_mut, counter = 0)          self.exhale_clock()
      self.clock.borrow()                          init_clock()
      [Cell counter = 1 (shared borrow)]             self.clock.borrow_mut()
                                                     Cell reads counter... could be 0 or 1
                                                     depending on CPU cache/reordering.
                                                     If it reads 1: PANIC "already borrowed"
                                                     If it reads 0: sets to -1, appears to
                                                       have exclusive access while Thread 1
                                                       also has a shared borrow. UB.
```

Two failure modes observed:
1. `already borrowed: BorrowMutError` -- Thread 2 tries `borrow_mut()` while Thread 1 holds `borrow()` (counter > 0)
2. `already mutably borrowed: BorrowError` -- Thread 2 tries `borrow()` while Thread 1 holds `borrow_mut()` (counter = -1)

### Why This Is Intermittent

The race window is tiny -- both threads must be inside `NEXT_ID`'s `RefCell` borrow operations at the same instant. Each shuttle `Runner::run` call re-executes the test closure many times (100 or 10 iterations respectively), and each iteration opens a new DB and triggers `NEXT_ID.fetch_add`. This gives many opportunities for the two OS threads to collide, but the window per-call is nanoseconds.

The `OnceLock` in `finalize_with_page_size` serializes arena init **within a single buffer pool** but does nothing across OS threads -- each test creates its own DB, its own buffer pool, and its own path through `Arena::new` to `NEXT_ID`.

### Backtrace Confirmation

Both observed backtraces (see `backtrace.md`) trace through:
```
Arena::new::{{closure}}
  -> AtomicU32::fetch_add
    -> fetch_update
      -> init_clock / exhale_clock
        -> RefCell::borrow_mut / RefCell::borrow  [PANIC]
```
This matches exactly: the crash site is `NEXT_ID`'s internal `RefCell` operations, accessed from two real OS threads simultaneously.

### Minimal Reproduction

`testing/stress/tests/static_atomic_repro.rs` demonstrates this with ~25 lines:

```rust
#![cfg(shuttle)]

static COUNTER: shuttle::sync::atomic::AtomicU32 = shuttle::sync::atomic::AtomicU32::new(0);

#[test]
fn static_shuttle_atomic_cross_thread_race() {
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
```

Two real OS threads, each with their own shuttle runner, both calling `fetch_add` on the same `static` shuttle atomic. Fails intermittently with `BorrowMutError` or `BorrowError`. Confirmed to reproduce.

### Call Chain: DB Init -> `NEXT_ID`

When a new database is opened, the first write transaction triggers:

```
VDBE op_transaction
  -> Pager::begin_write_tx          (pager.rs)
    -> Pager::maybe_allocate_page1  (pager.rs)
      -> Pager::allocate_page1      (pager.rs)
        -> BufferPool::finalize_with_page_size  (buffer_pool.rs)
          -> PoolInner::init_arenas (buffer_pool.rs) -- OnceLock guards single init per pool
            -> Arena::new           (buffer_pool.rs)
              -> io.register_fixed_buffer()
                 returns Err for MemoryIO and all non-io_uring backends
              -> NEXT_ID.fetch_add(1, SeqCst)  (buffer_pool.rs:395)
```

`NEXT_ID` is reached for every `Arena::new` call where `register_fixed_buffer` fails (the common case -- only io_uring succeeds). Each test creates its own DB, so each shuttle runner hits this path independently.

### Other Affected Statics

`NEXT_ID` is not the only static shuttle atomic in `turso_core`. All of these use `crate::sync::atomic` and have the same vulnerability:

| File | Line | Static | Type | Context |
|------|------|--------|------|---------|
| `core/storage/buffer_pool.rs` | 372 | `NEXT_ID` | `AtomicU32` | Arena ID counter (hit on every DB open) |
| `core/vtab.rs` | 394 | `VTAB_ID_COUNTER` | `AtomicU64` | Virtual table ID counter |
| `core/storage/pager.rs` | 64 | `PENDING_BYTE` | `AtomicU32` | Only under `feature = "test_helper"` |
| `core/io/mod.rs` | 544 | `COUNTER` | `AtomicU64` | `MemoryIOFactory` instance counter |
| `core/mvcc/mod.rs` | 53 | `IDS` | `AtomicI64` | Inside `#[cfg(test)] mod tests` only |

All must be changed to `std::sync::atomic` types. None of these are application state whose orderings shuttle needs to explore -- they are infrastructure counters and flags.

### Fix Options

1. **Use `std::sync::atomic` for all static atomics** (correct fix) -- change all five statics listed above. This is what shuttle's own codebase does for static atomics.
2. **Run with `--test-threads=1`** (workaround) -- eliminates the race by running tests sequentially, but slows down the suite.
3. **Mark tests `#[serial]`** (workaround, from `serial_test` crate) -- prevents parallel execution of specific tests that share the static.

Option 1 is the correct fix. Options 2/3 are workarounds that don't address the root cause.

### Verification

Running with `--test-threads=1` eliminates this crash, confirming the parallel-thread root cause.

---

## The Rule: When to Use Shuttle Atomics vs `std::sync::atomic`

### Shuttle's Own Practice

Shuttle's own codebase consistently uses `std::sync::atomic` for static atomics:
- `shuttle/src/sync/atomic/mod.rs:70`: `static PRINTED_ORDERING_WARNING: std::sync::atomic::AtomicBool`
- `shuttle/src/sync/once.rs:60`: `static NEXT_ID: StdAtomicUsize`
- `shuttle/tests/basic/thread.rs:6`: `use std::sync::atomic::{AtomicBool, AtomicU8, Ordering}` for all statics
- `shuttle/tests/basic/lazy_static.rs`: all static counters use `std::sync::atomic::AtomicUsize`

Shuttle-instrumented atomics in shuttle's own tests are **always** heap-allocated and `Arc`-shared. No test in shuttle's codebase uses `shuttle::sync::atomic` as a `static`.

### Decision Table

| Atomic location | Use shuttle? | Why |
|-----------------|-------------|-----|
| `static` counter / flag | **NO** -- use `std::sync::atomic` | Process-global, cargo test threads access it from multiple OS threads. Shuttle's `RefCell` is not thread-safe |
| `static` once-init flag | **NO** -- use `std::sync::Once` or `OnceLock` | Same reason |
| Heap-allocated, `Arc`-shared between shuttle coroutines | **YES** | Lives within a single shuttle `Runner::run` execution, all accesses on one OS thread. Shuttle can explore orderings |
| Heap-allocated MVCC version counters | **YES** | Shuttle needs to explore their interleavings to find bugs |

### The Key Invariant

Shuttle's `Atomic<T>` contains `RefCell`, which is safe **only on a single OS thread**. Shuttle guarantees single-OS-thread execution within one `Runner::run` call. Therefore:

- **Within a `Runner::run`**: shuttle atomics are safe. All coroutines (whether `thread::spawn` or `future::spawn`) run on the runner's OS thread.
- **Across `Runner::run` calls on different OS threads**: shuttle atomics in `static` variables are **unsound**. Each runner's OS thread accesses the same `RefCell` without synchronization.

This is not documented in shuttle's API docs -- it must be inferred from the `unsafe impl Sync` safety comment and the `RefCell` implementation detail.

---

## Separate Concern: Bindings Layer and Shuttle Visibility

This is **unrelated** to the BorrowMutError but relevant for shuttle testing quality.

### Untracked Sync Primitives

`bindings/rust` and `sdk-kit` use `std::sync::{Arc, Mutex}` -- opaque to shuttle. Shuttle cannot schedule around mutex acquisitions it can't see, which limits its ability to explore interleavings through the bindings layer.

### `std::sync::Mutex` on the Same OS Thread

Since all shuttle coroutines run on one OS thread, a `std::sync::Mutex` behaves differently than expected:
- If coroutine A locks a `std::sync::Mutex`, shuttle yields (at an `.await` or `thread::switch()`), and coroutine B tries to lock the same mutex: the OS thread **deadlocks**. `std::sync::Mutex` is not reentrant, and both coroutines are on the same OS thread, so the `lock()` call blocks the thread forever.
- This is a **liveness** issue (hang/deadlock), NOT a `BorrowMutError`. Untracked `std::sync::Mutex` cannot cause RefCell borrow panics.
- Shuttle has no way to detect or recover from this deadlock since it doesn't know the mutex exists.

### Implication for Testing

To properly test `BEGIN CONCURRENT` through the bindings layer with shuttle, the bindings' sync primitives would need to be replaced with shuttle-instrumented versions (e.g., `turso_core::sync::Mutex` which re-exports `shuttle::sync::Mutex` under `--cfg shuttle`). Without this, shuttle sees an incomplete picture of the concurrency.

---

## Shuttle Async Runtime Notes

- `shuttle::future::spawn` creates a shuttle-managed async task (a coroutine on the shared OS thread)
- `shuttle::future::block_on` runs the top-level future, driving the executor
- When a future returns `Poll::Pending`, shuttle calls `thread::switch()` to schedule another task
- Async tasks can interleave at every `.await` / `Poll::Pending` return -- these are shuttle yield points
- `shuttle::thread::spawn` creates shuttle-managed threads (also coroutines on the same OS thread), which yield only at explicit `thread::switch()` calls inside shuttle-instrumented sync primitives
- Both `future::spawn` and `thread::spawn` are fully within shuttle's control -- the distinction is yield frequency, not thread safety
