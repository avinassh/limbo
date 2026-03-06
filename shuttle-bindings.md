# Making Rust Bindings Shuttle-Compatible

## Dependency Chain

```
bindings/rust  →  turso_sdk_kit  →  turso_core
```

Bindings do NOT depend on `turso_core` directly. All sync imports in bindings
must go through `turso_sdk_kit`. `sdk-kit` already depends on `turso_core`
directly and can use `turso_core::sync` without any new dependency.

---

## Problem: Untracked Sync Primitives

Shuttle instruments sync primitives to control and explore all possible
interleavings. `std::sync::*` is opaque to shuttle. The binding and sdk-kit
layers are full of `std::sync` — shuttle cannot schedule around any of it.

---

## All Untracked Primitives

### `sdk-kit/src/lib.rs`

| Line | Primitive | Usage |
|------|-----------|-------|
| 1 | `std::sync::atomic::AtomicU32` | `ConcurrentGuard.in_use` |
| 1 | `std::sync::atomic::Ordering` | `compare_exchange`, `swap` |

### `sdk-kit/src/rsapi.rs`

| Line | Primitive | Usage |
|------|-----------|-------|
| 6 | `std::sync::Arc` | `TursoDatabase`, `TursoConnection`, `CachedStatement`, etc. |
| 6 | `std::sync::Mutex` | `TursoDatabase.{open_state, db, io}`, `TursoConnection.cached_statements` |
| 6 | `std::sync::Once` | `static SETUP: Once` — **KEEP**, no shuttle equivalent |
| 6 | `std::sync::RwLock` | `static LOGGER: RwLock<...>` — **KEEP**, static initializer |

### `bindings/rust/src/lib.rs`

| Line | Primitive | Usage |
|------|-----------|-------|
| 57 | `std::sync::Arc` | `Database.inner`, `Statement.inner` |
| 58 | `std::sync::Mutex` | `Statement.inner: Arc<Mutex<Box<TursoStatement>>>` — hottest lock |

### `bindings/rust/src/connection.rs`

| Line | Primitive | Usage |
|------|-----------|-------|
| 10 | `std::sync::atomic::AtomicU8` | `AtomicDropBehavior.inner` |
| 11 | `std::sync::atomic::Ordering` | `load()`, `store()` on `AtomicDropBehavior` |
| 12 | `std::sync::Arc` | `Connection.inner`, `Connection.extra_io` |
| 13 | `std::sync::Mutex` | Creating `Statement.inner` in `prepare()`/`prepare_cached()` |

### `bindings/rust/src/transaction.rs`

| Line | Primitive | Usage |
|------|-----------|-------|
| 1 | `std::sync::atomic::Ordering` | `AtomicDropBehavior.store()` in `Drop` |

### `bindings/rust/src/sync.rs` (feature = "sync" only — OUT OF SCOPE)

Not active in shuttle builds. Leave as-is.

---

## What `turso_core::sync` Provides

`core/sync.rs` is a complete shuttle shim — currently `pub(crate)`.

| Primitive | Under `--cfg shuttle` | Under normal build |
|-----------|-----------------------|--------------------|
| `atomic::AtomicU8/U32/I64/...` | `shuttle::sync::atomic` (full module) | `std::sync::atomic` |
| `atomic::Ordering` | same | same |
| `Arc` | `shuttle::sync::Arc` (= `std::sync::Arc`, not instrumented) | `std::sync::Arc` |
| `Mutex` | wrapper over `shuttle::sync::Mutex` | `parking_lot::Mutex` |
| `RwLock` | wrapper over `shuttle::sync::RwLock` | `parking_lot::RwLock` |

All primitives we need are present. `Once` is the only missing type — kept as
`std::sync::Once` where used.

**Key API difference**: `turso_core::sync::Mutex::lock()` and
`parking_lot::Mutex::lock()` return a guard **directly**, not `Result`. Every
`.lock().unwrap()` call site must become `.lock()`.

---

## Plan

### Step 1 — Make `core/sync.rs` public

Change the two top-level re-exports in `core/sync.rs`:

```rust
// Before:
pub(crate) use shuttle_adapter::*;
pub(crate) use std_adapter::*;

// After:
pub use shuttle_adapter::*;
pub use std_adapter::*;
```

No other changes to `turso_core`.

---

### Step 2 — sdk-kit re-exports `turso_core::sync`

Add one line to `sdk-kit/src/lib.rs`:

```rust
pub use turso_core::sync;
```

This makes `turso_sdk_kit::sync::Mutex`, `turso_sdk_kit::sync::Arc`, etc.
available to bindings without adding any new dependency.

---

### Step 3 — Replace `std::sync` in `sdk-kit/src/lib.rs`

```rust
// Before:
use std::sync::atomic::{AtomicU32, Ordering};

// After:
use turso_core::sync::atomic::{AtomicU32, Ordering};
```

No call-site changes needed — `compare_exchange` and `swap` have the same
API.

---

### Step 4 — Replace `std::sync` in `sdk-kit/src/rsapi.rs`

```rust
// Before:
use std::sync::{Arc, Mutex, Once, RwLock};

// After:
use turso_core::sync::{Arc, Mutex};
use std::sync::{Once, RwLock};   // Once: no shuttle equivalent; RwLock: static logger only
```

Call-site changes — drop `.unwrap()` from every Mutex lock call:

| Line | Before | After |
|------|--------|-------|
| 533 | `self.db.lock().unwrap()` | `self.db.lock()` |
| 542 | `self.io.lock().unwrap()` | `self.io.lock()` |
| 626 | `self.open_state.lock().unwrap()` | `self.open_state.lock()` |
| 629 | `self.db.lock().unwrap()` | `self.db.lock()` |
| 640 | `*self.io.lock().unwrap()` | `*self.io.lock()` |
| 702 | `self.db.lock().unwrap()` | `self.db.lock()` |
| 727 | `self.db.lock().unwrap()` | `self.db.lock()` |
| 828 | `self.cached_statements.lock().unwrap()` | `self.cached_statements.lock()` |

`LOGGER.write().unwrap()` at line 494 stays unchanged — `LOGGER` remains
`std::sync::RwLock`.

---

### Step 5 — Replace `std::sync` in `bindings/rust/src/lib.rs`

```rust
// Before:
use std::sync::Arc;
use std::sync::Mutex;

// After:
use turso_sdk_kit::sync::Arc;
use turso_sdk_kit::sync::Mutex;
```

Call-site changes — drop `.unwrap()` from every `Statement.inner` lock:

| Lines | Before | After |
|-------|--------|-------|
| 306, 320, 350, 374, 381, 387, 400, 405, 420, 433, 450, 472 | `.lock().unwrap()` | `.lock()` |

Also `rows.rs` line 61:

```rust
// Before:
self.inner.inner.lock().unwrap().column_count()

// After:
self.inner.inner.lock().column_count()
```

---

### Step 6 — Replace `std::sync` in `bindings/rust/src/connection.rs`

```rust
// Before:
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;

// After:
use turso_sdk_kit::sync::atomic::{AtomicU8, Ordering};
use turso_sdk_kit::sync::Arc;
use turso_sdk_kit::sync::Mutex;
```

No call-site changes — atomic `load`/`store` API is identical.

---

### Step 7 — Replace `std::sync` in `bindings/rust/src/transaction.rs`

```rust
// Before:
use std::{ops::Deref, sync::atomic::Ordering};

// After:
use std::ops::Deref;
use turso_sdk_kit::sync::atomic::Ordering;
```

No call-site changes.

---

### Step 8 — Verify `assert_send_sync!` still holds

`lib.rs` asserts `Execute: Send + Sync`.
`connection.rs` asserts `Connection: Send + Sync`.
`rows.rs` asserts `Next: Send + Sync` (line 58).

- `turso_core::sync::Mutex<T>`: both adapters are `Send + Sync` when `T: Send`.
  `sdk-kit/Cargo.toml` already carries `parking_lot` with `send_guard` feature
  for exactly this reason — no regression.
- `turso_core::sync::Arc<T>` = `std::sync::Arc<T>` in both adapters.

---

### Step 9 — Update `make test-shuttle` in Makefile

Current:
```makefile
test-shuttle:
	RUSTFLAGS='--cfg tokio_unstable --cfg shuttle' cargo nextest run --profile shuttle --package turso_core
```

After (add `turso_stress` to run shuttle stress tests; `turso` and `turso_sdk_kit` are
pulled in as dependencies and get `--cfg shuttle` via RUSTFLAGS automatically):
```makefile
test-shuttle:
	RUSTFLAGS='--cfg tokio_unstable --cfg shuttle' cargo nextest run \
	    --profile shuttle --package turso_core --package turso_stress
```

---

## What This Achieves

After these changes shuttle controls:

1. Task scheduling at every `.await` — already worked
2. `Statement.inner` mutex — interleave two tasks racing on the same statement
3. `AtomicDropBehavior` — interleave transaction drop racing with new executes
4. `TursoDatabase.db` mutex — interleave concurrent `connect()` calls
5. `TursoConnection.cached_statements` mutex — interleave prepared statement cache

---

## What This Does NOT Fix

- **`BorrowMutError` on static shuttle atomic** — separate issue, caused by
  parallel cargo test threads accessing `static NEXT_ID: AtomicU32` in
  `core/storage/buffer_pool.rs`. Under `--cfg shuttle`, this is a
  `shuttle::sync::atomic::AtomicU32` wrapping `RefCell`, which is not
  thread-safe across real OS threads. Fix: use `std::sync::atomic::AtomicU32`
  for `NEXT_ID`. See `shuttle-core.md` for full analysis.
- `static LOGGER: RwLock` — remains `std::sync`, acceptable.
- `std::sync::Once` — no shuttle equivalent.
- `tokio::sync::mpsc` in `sync.rs` — out of scope.
