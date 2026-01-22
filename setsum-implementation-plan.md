# Incremental Setsum Checksum Implementation Plan

## Overview

Implement order-agnostic, incremental checksums for tursodb using the Setsum algorithm. Unlike traditional dbhash (which requires full table scans), setsum allows O(1) updates on INSERT/DELETE/UPDATE operations.

**Key Properties:**
- Order agnostic: `INSERT a, b` = `INSERT b, a`
- Additive: `INSERT → setsum.Insert(row_hash)`
- Subtractive: `DELETE → setsum.Remove(row_hash)`
- Combinable: Can merge setsums from different sources
- Fixed size: Always 256 bits regardless of database size

**Use Cases:**
- Replication verification (compare primary/replica setsums)
- Incremental backup validation
- Data integrity monitoring
- Change detection without full scans

---

## Phase 1: Core Setsum Implementation

### Task 1.1: Create Setsum Module

**Location:** `core/setsum/mod.rs`

**Files to create:**
```
core/setsum/
├── mod.rs          # Main module, re-exports
└── setsum.rs       # Core implementation
```

**Reference:** Port from `/Users/avi/.tmp/claude/limbo/setsum/setsum.go`

**Core Structure:**
```rust
// core/setsum/setsum.rs

use sha3::{Sha3_256, Digest};

const SETSUM_BYTES: usize = 32;
const SETSUM_COLUMNS: usize = 8;
const BYTES_PER_COLUMN: usize = 4;

/// Prime numbers for each column (close to u32::MAX)
const PRIMES: [u32; SETSUM_COLUMNS] = [
    4294967291, 4294967279, 4294967231, 4294967197,
    4294967189, 4294967161, 4294967143, 4294967111,
];

/// Order-agnostic, additive/subtractive checksum
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Setsum {
    state: [u32; SETSUM_COLUMNS],
}

impl Setsum {
    pub fn new() -> Self {
        Self { state: [0; SETSUM_COLUMNS] }
    }

    /// Insert an item into the setsum
    pub fn insert(&mut self, item: &[u8]) {
        let item_state = Self::hash_to_state(item);
        self.state = Self::add_state(self.state, item_state);
    }

    /// Remove an item from the setsum
    pub fn remove(&mut self, item: &[u8]) {
        let item_state = Self::hash_to_state(item);
        let inverted = Self::invert_state(item_state);
        self.state = Self::add_state(self.state, inverted);
    }

    /// Combine with another setsum (additive)
    pub fn add(&mut self, other: &Setsum) {
        self.state = Self::add_state(self.state, other.state);
    }

    /// Subtract another setsum
    pub fn subtract(&mut self, other: &Setsum) {
        let inverted = Self::invert_state(other.state);
        self.state = Self::add_state(self.state, inverted);
    }

    /// Get the 256-bit digest
    pub fn digest(&self) -> [u8; SETSUM_BYTES] {
        let mut result = [0u8; SETSUM_BYTES];
        for i in 0..SETSUM_COLUMNS {
            let start = i * BYTES_PER_COLUMN;
            result[start..start + 4].copy_from_slice(&self.state[i].to_le_bytes());
        }
        result
    }

    /// Get hex string representation
    pub fn hex_digest(&self) -> String {
        hex::encode(self.digest())
    }

    /// Check if setsum is empty (all zeros)
    pub fn is_empty(&self) -> bool {
        self.state.iter().all(|&x| x == 0)
    }

    // --- Internal helpers ---

    fn hash_to_state(item: &[u8]) -> [u32; SETSUM_COLUMNS] {
        let mut hasher = Sha3_256::new();
        hasher.update(item);
        let hash = hasher.finalize();

        let mut state = [0u32; SETSUM_COLUMNS];
        for i in 0..SETSUM_COLUMNS {
            let start = i * BYTES_PER_COLUMN;
            let num = u32::from_le_bytes(hash[start..start + 4].try_into().unwrap());
            // Reduce modulo prime
            state[i] = if num >= PRIMES[i] { num - PRIMES[i] } else { num };
        }
        state
    }

    fn add_state(lhs: [u32; SETSUM_COLUMNS], rhs: [u32; SETSUM_COLUMNS]) -> [u32; SETSUM_COLUMNS] {
        let mut result = [0u32; SETSUM_COLUMNS];
        for i in 0..SETSUM_COLUMNS {
            let sum = u64::from(lhs[i]) + u64::from(rhs[i]);
            result[i] = if sum >= u64::from(PRIMES[i]) {
                (sum - u64::from(PRIMES[i])) as u32
            } else {
                sum as u32
            };
        }
        result
    }

    fn invert_state(state: [u32; SETSUM_COLUMNS]) -> [u32; SETSUM_COLUMNS] {
        let mut result = [0u32; SETSUM_COLUMNS];
        for i in 0..SETSUM_COLUMNS {
            result[i] = PRIMES[i] - state[i];
        }
        result
    }
}
```

### Task 1.2: Add SHA3 Dependency

**File:** `core/Cargo.toml`

Add to dependencies:
```toml
sha3 = "0.10"
hex = "0.4"  # If not already present
```

### Task 1.3: Row Serialization for Hashing

**File:** `core/setsum/row_encoder.rs`

The row encoder creates deterministic byte representation for hashing:

```rust
// core/setsum/row_encoder.rs

use crate::types::Value;

/// Encode a row for setsum hashing
/// Format: table_name + \x00 + rowid_be + \x00 + col1_encoded + col2_encoded + ...
pub fn encode_row(table_name: &str, rowid: i64, values: &[Value]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(256);

    // Table name (for table-global uniqueness)
    buf.extend_from_slice(table_name.as_bytes());
    buf.push(0x00);

    // Rowid in big-endian (canonical ordering)
    buf.extend_from_slice(&rowid.to_be_bytes());
    buf.push(0x00);

    // Each column value
    for value in values {
        encode_value(value, &mut buf);
    }

    buf
}

fn encode_value(value: &Value, buf: &mut Vec<u8>) {
    match value {
        Value::Null => {
            buf.push(b'0');  // Type tag
        }
        Value::Integer(v) => {
            buf.push(b'1');
            buf.extend_from_slice(&v.to_be_bytes());
        }
        Value::Float(v) => {
            buf.push(b'2');
            buf.extend_from_slice(&v.to_bits().to_be_bytes());
        }
        Value::Text(text) => {
            buf.push(b'3');
            let bytes = text.as_str().as_bytes();
            buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            buf.extend_from_slice(bytes);
        }
        Value::Blob(blob) => {
            buf.push(b'4');
            buf.extend_from_slice(&(blob.len() as u32).to_be_bytes());
            buf.extend_from_slice(blob);
        }
    }
}
```

### Task 1.4: Unit Tests for Setsum

**File:** `core/setsum/tests.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_setsum() {
        let s = Setsum::new();
        assert!(s.is_empty());
    }

    #[test]
    fn test_insert_order_agnostic() {
        let mut s1 = Setsum::new();
        s1.insert(b"apple");
        s1.insert(b"banana");

        let mut s2 = Setsum::new();
        s2.insert(b"banana");
        s2.insert(b"apple");

        assert_eq!(s1, s2);
    }

    #[test]
    fn test_remove() {
        let mut s1 = Setsum::new();
        s1.insert(b"apple");
        s1.insert(b"banana");
        s1.remove(b"apple");

        let mut s2 = Setsum::new();
        s2.insert(b"banana");

        assert_eq!(s1, s2);
    }

    #[test]
    fn test_add_setsums() {
        let mut s1 = Setsum::new();
        s1.insert(b"apple");

        let mut s2 = Setsum::new();
        s2.insert(b"banana");

        let mut s3 = Setsum::new();
        s3.insert(b"apple");
        s3.insert(b"banana");

        s1.add(&s2);
        assert_eq!(s1, s3);
    }

    #[test]
    fn test_subtract_setsums() {
        let mut s1 = Setsum::new();
        s1.insert(b"apple");
        s1.insert(b"banana");

        let mut s2 = Setsum::new();
        s2.insert(b"banana");

        let mut expected = Setsum::new();
        expected.insert(b"apple");

        s1.subtract(&s2);
        assert_eq!(s1, expected);
    }

    #[test]
    fn test_hex_digest() {
        let mut s = Setsum::new();
        s.insert(b"hello");
        let hex = s.hex_digest();
        assert_eq!(hex.len(), 64); // 32 bytes = 64 hex chars
    }
}
```

**Checklist:**
- [ ] Create `core/setsum/mod.rs`
- [ ] Create `core/setsum/setsum.rs` with core implementation
- [ ] Create `core/setsum/row_encoder.rs` for row serialization
- [ ] Add `sha3` dependency to `core/Cargo.toml`
- [ ] Add `pub mod setsum;` to `core/lib.rs`
- [ ] Write unit tests
- [ ] Verify tests pass with `cargo test -p turso-core setsum`

---

## Phase 2: State Management

### Task 2.1: Per-Table Setsum State

Store setsum state per table in the Connection. Two options:

**Option A: In-Memory Only (Simpler)**
- Compute on-demand via `PRAGMA setsum`
- No persistence, recomputed each time
- Good for verification, not incremental tracking

**Option B: Incremental Tracking (Full Feature)**
- Maintain running setsum during transaction
- Persist to system table on COMMIT
- True incremental updates

**Recommended:** Start with Option A, evolve to Option B.

### Task 2.2: SetsumState Structure

**File:** `core/setsum/state.rs`

```rust
// core/setsum/state.rs

use std::collections::HashMap;
use crate::setsum::Setsum;

/// Tracks setsum state for all tables in a connection
#[derive(Default)]
pub struct SetsumState {
    /// Per-table setsum (accumulated during transaction)
    tables: HashMap<String, Setsum>,

    /// Transaction-level changes (for rollback)
    tx_changes: HashMap<String, Setsum>,

    /// Whether incremental tracking is enabled
    enabled: bool,
}

impl SetsumState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable/disable incremental tracking
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Record a row insertion
    pub fn record_insert(&mut self, table: &str, row_bytes: &[u8]) {
        if !self.enabled {
            return;
        }
        self.tables
            .entry(table.to_string())
            .or_default()
            .insert(row_bytes);

        self.tx_changes
            .entry(table.to_string())
            .or_default()
            .insert(row_bytes);
    }

    /// Record a row deletion
    pub fn record_delete(&mut self, table: &str, row_bytes: &[u8]) {
        if !self.enabled {
            return;
        }
        self.tables
            .entry(table.to_string())
            .or_default()
            .remove(row_bytes);

        // For rollback, we need to re-add this row
        // Store inverted change
        self.tx_changes
            .entry(table.to_string())
            .or_default()
            .remove(row_bytes);
    }

    /// Get setsum for a specific table
    pub fn get_table_setsum(&self, table: &str) -> Option<&Setsum> {
        self.tables.get(table)
    }

    /// Get combined setsum for all tables
    pub fn get_database_setsum(&self) -> Setsum {
        let mut combined = Setsum::new();
        for setsum in self.tables.values() {
            combined.add(setsum);
        }
        combined
    }

    /// Commit transaction changes (clear tx_changes)
    pub fn commit_transaction(&mut self) {
        self.tx_changes.clear();
    }

    /// Rollback transaction (undo tx_changes)
    pub fn rollback_transaction(&mut self) {
        for (table, changes) in self.tx_changes.drain() {
            if let Some(table_setsum) = self.tables.get_mut(&table) {
                table_setsum.subtract(&changes);
            }
        }
    }
}
```

### Task 2.3: Integration Point - Connection

**File:** `core/vdbe/mod.rs` or wherever Connection is defined

Add `SetsumState` to Connection/Program state:

```rust
// In Connection or Program struct
pub struct Connection {
    // ... existing fields ...

    /// Incremental setsum tracking state
    setsum_state: SetsumState,
}
```

**Checklist:**
- [ ] Create `core/setsum/state.rs`
- [ ] Add `SetsumState` to Connection/Program
- [ ] Initialize state on connection open
- [ ] Wire up commit/rollback to SetsumState

---

## Phase 3: VDBE Integration (Incremental Updates)

### Task 3.1: Hook into op_insert

**File:** `core/vdbe/execute.rs` around line 6498

After successful btree insert, update setsum:

```rust
// In op_insert(), after:
// cursor.insert(&BTreeKey::new_table_rowid(key, Some(&record)))

// Add setsum tracking
if let Some(table_name) = cursor.table_name() {
    let row_bytes = encode_row(
        table_name,
        key,  // rowid
        &record_values,  // column values from record
    );
    self.setsum_state.record_insert(table_name, &row_bytes);
}
```

### Task 3.2: Hook into op_delete

**File:** `core/vdbe/execute.rs` around line 6704

Before btree delete, capture row for setsum removal:

```rust
// In op_delete(), before actual deletion:

if let Some(table_name) = cursor.table_name() {
    // Must read current row values before delete
    let rowid = cursor.rowid()?;
    let record_values = cursor.record_values()?;

    let row_bytes = encode_row(
        table_name,
        rowid,
        &record_values,
    );
    self.setsum_state.record_delete(table_name, &row_bytes);
}

// Then proceed with actual deletion
```

### Task 3.3: UPDATE Handling

UPDATE = DELETE old row + INSERT new row

The trigger framework already provides OLD and NEW row values:
- `core/translate/trigger_exec.rs` lines 14-29: `TriggerContext`
- OLD registers contain pre-update values
- NEW registers contain post-update values

For setsum:
1. `record_delete(table, old_row_bytes)`
2. `record_insert(table, new_row_bytes)`

### Task 3.4: Transaction Boundaries

**COMMIT:** `setsum_state.commit_transaction()`
**ROLLBACK:** `setsum_state.rollback_transaction()`

Hook into transaction commit/rollback in VDBE:
- Look for `Insn::Commit` or `Insn::AutoCommit` handling
- Call appropriate SetsumState method

**Checklist:**
- [ ] Add setsum update call in `op_insert()`
- [ ] Add setsum update call in `op_delete()`
- [ ] Handle UPDATE via DELETE+INSERT pattern
- [ ] Hook COMMIT to `commit_transaction()`
- [ ] Hook ROLLBACK to `rollback_transaction()`
- [ ] Add cursor methods to get table_name and record_values if needed

---

## Phase 4: PRAGMA Interface

### Task 4.1: Parser Changes

**File:** `parser/src/ast.rs`

Add to `PragmaName` enum:
```rust
pub enum PragmaName {
    // ... existing variants ...

    /// PRAGMA setsum - database-wide setsum
    Setsum,
    /// PRAGMA setsum('table') - table-specific setsum
    SetsumTable,
    /// PRAGMA setsum_enabled - enable/disable incremental tracking
    SetsumEnabled,
    /// PRAGMA setsum_reset - reset all setsums to zero
    SetsumReset,
}
```

### Task 4.2: PRAGMA Registration

**File:** `core/pragma.rs`

```rust
pub fn pragma_for(name: &str) -> Option<Pragma> {
    match name.to_lowercase().as_str() {
        // ... existing pragmas ...

        "setsum" => Some(Pragma {
            name: PragmaName::Setsum,
            requires_schema: false,
            requires_arg: false,  // Optional table name arg
        }),
        "setsum_enabled" => Some(Pragma {
            name: PragmaName::SetsumEnabled,
            requires_schema: false,
            requires_arg: false,  // Optional: true/false to set
        }),
        "setsum_reset" => Some(Pragma {
            name: PragmaName::SetsumReset,
            requires_schema: false,
            requires_arg: false,
        }),
        _ => None,
    }
}
```

### Task 4.3: PRAGMA Translation

**File:** `core/translate/pragma.rs`

Add translation cases:

```rust
PragmaName::Setsum => {
    if let Some(table_arg) = &pragma.arg {
        // PRAGMA setsum('table_name') - compute for specific table
        translate_setsum_table(program, table_arg)
    } else {
        // PRAGMA setsum - compute for entire database
        translate_setsum_database(program)
    }
}

PragmaName::SetsumEnabled => {
    if let Some(value) = &pragma.arg {
        // Set enabled state
        translate_setsum_set_enabled(program, value)
    } else {
        // Query enabled state
        translate_setsum_get_enabled(program)
    }
}

PragmaName::SetsumReset => {
    translate_setsum_reset(program)
}
```

### Task 4.4: On-Demand Computation (Option A)

For `PRAGMA setsum('table')` without incremental tracking:

```rust
fn translate_setsum_table(program: &mut ProgramBuilder, table_name: &str) {
    // 1. Open table cursor
    // 2. Loop through all rows
    // 3. For each row: encode and insert into local Setsum
    // 4. Return hex digest

    // Similar to PRAGMA hash implementation:
    // - Insn::OpenRead to open table
    // - Insn::Rewind to start
    // - Loop: Insn::Column for each column, encode, hash
    // - Insn::Next to continue
    // - Insn::ResultRow with hex digest
}
```

**SQL Equivalent:**
```sql
-- What PRAGMA setsum('users') effectively does:
SELECT setsum_aggregate(rowid, *) FROM users;
```

### Task 4.5: New VDBE Instruction (Optional)

For better performance, add dedicated instruction:

**File:** `core/vdbe/insn.rs`

```rust
pub enum Insn {
    // ... existing ...

    /// Compute setsum for current cursor row
    /// P1 = cursor, P2 = dest register for row_bytes
    SetsumRow { cursor: CursorID, dest: usize },

    /// Accumulate row into setsum state
    /// P1 = setsum state register, P2 = row_bytes register
    SetsumAccum { state: usize, row_bytes: usize },

    /// Finalize setsum and produce hex digest
    /// P1 = setsum state register, P2 = dest register for hex string
    SetsumFinalize { state: usize, dest: usize },
}
```

**Checklist:**
- [ ] Add `Setsum`, `SetsumEnabled`, `SetsumReset` to `PragmaName` enum
- [ ] Register pragmas in `pragma_for()`
- [ ] Implement `translate_setsum_table()` for on-demand computation
- [ ] Implement `translate_setsum_database()` for full DB setsum
- [ ] Implement `translate_setsum_set_enabled()` and `translate_setsum_get_enabled()`
- [ ] (Optional) Add VDBE instructions for optimized computation
- [ ] Add execution handlers in `execute.rs`

---

## Phase 5: Persistence (Optional Enhancement)

### Task 5.1: System Table for Setsum State

Create internal table to persist setsum state across sessions:

```sql
CREATE TABLE IF NOT EXISTS _turso_setsum (
    table_name TEXT PRIMARY KEY,
    setsum_hex TEXT NOT NULL,
    row_count INTEGER NOT NULL,
    updated_at INTEGER NOT NULL  -- Unix timestamp
);
```

### Task 5.2: Persist on Checkpoint

During WAL checkpoint or explicit PRAGMA:
1. Write current setsum state to `_turso_setsum`
2. Include in checkpoint process

### Task 5.3: Load on Connection Open

When opening database:
1. Check if `_turso_setsum` exists
2. Load persisted state into `SetsumState`
3. Mark as "potentially stale" if WAL has uncommitted changes

### Task 5.4: Recovery Validation

On recovery after crash:
1. Recompute setsum from actual table data
2. Compare with persisted state
3. Log warning if mismatch (indicates corruption or incomplete recovery)

**Checklist:**
- [ ] Define `_turso_setsum` schema
- [ ] Create table on first setsum enable
- [ ] Persist state during checkpoint
- [ ] Load state on connection open
- [ ] Add staleness tracking
- [ ] Implement recovery validation

---

## Phase 6: Testing

### Task 6.1: Unit Tests

**File:** `core/setsum/tests.rs`

Already covered in Phase 1.

### Task 6.2: Integration Tests

**File:** `testing/setsum_test.rs` or similar

```rust
#[test]
fn test_pragma_setsum_empty_table() {
    let db = open_test_db();
    db.execute("CREATE TABLE t(x)");

    let result = db.query("PRAGMA setsum('t')");
    // Empty table should have zero setsum
    assert_eq!(result, ZERO_SETSUM_HEX);
}

#[test]
fn test_pragma_setsum_insert() {
    let db = open_test_db();
    db.execute("CREATE TABLE t(x)");
    db.execute("INSERT INTO t VALUES (1)");
    db.execute("INSERT INTO t VALUES (2)");

    let s1 = db.query("PRAGMA setsum('t')");

    // Insert in different order should give same result
    let db2 = open_test_db();
    db2.execute("CREATE TABLE t(x)");
    db2.execute("INSERT INTO t VALUES (2)");
    db2.execute("INSERT INTO t VALUES (1)");

    let s2 = db2.query("PRAGMA setsum('t')");

    assert_eq!(s1, s2);
}

#[test]
fn test_pragma_setsum_delete() {
    let db = open_test_db();
    db.execute("CREATE TABLE t(x)");
    db.execute("INSERT INTO t VALUES (1)");
    db.execute("INSERT INTO t VALUES (2)");
    db.execute("DELETE FROM t WHERE x = 1");

    let s1 = db.query("PRAGMA setsum('t')");

    // Should equal table with just (2)
    let db2 = open_test_db();
    db2.execute("CREATE TABLE t(x)");
    db2.execute("INSERT INTO t VALUES (2)");

    let s2 = db2.query("PRAGMA setsum('t')");

    assert_eq!(s1, s2);
}

#[test]
fn test_pragma_setsum_update() {
    let db = open_test_db();
    db.execute("CREATE TABLE t(x)");
    db.execute("INSERT INTO t VALUES (1)");
    db.execute("UPDATE t SET x = 2 WHERE x = 1");

    let s1 = db.query("PRAGMA setsum('t')");

    // Should equal table with just (2)
    let db2 = open_test_db();
    db2.execute("CREATE TABLE t(x)");
    db2.execute("INSERT INTO t VALUES (2)");

    let s2 = db2.query("PRAGMA setsum('t')");

    assert_eq!(s1, s2);
}
```

### Task 6.3: TCL Compatibility Tests

**File:** `testing/pragma_setsum.test`

```tcl
do_test setsum-1.0 {
    execsql {
        CREATE TABLE t1(a, b, c);
        PRAGMA setsum('t1');
    }
} {0000000000000000000000000000000000000000000000000000000000000000}

do_test setsum-1.1 {
    execsql {
        INSERT INTO t1 VALUES(1, 'hello', 3.14);
        PRAGMA setsum('t1');
    }
} {<non-zero-hex>}

do_test setsum-2.0-order-agnostic {
    set s1 [execsql {
        DELETE FROM t1;
        INSERT INTO t1 VALUES(1, 'a', 1.0);
        INSERT INTO t1 VALUES(2, 'b', 2.0);
        PRAGMA setsum('t1');
    }]
    set s2 [execsql {
        DELETE FROM t1;
        INSERT INTO t1 VALUES(2, 'b', 2.0);
        INSERT INTO t1 VALUES(1, 'a', 1.0);
        PRAGMA setsum('t1');
    }]
    expr {$s1 eq $s2}
} {1}
```

### Task 6.4: Replication Verification Test

Simulate primary/replica scenario:

```rust
#[test]
fn test_replication_verification() {
    let primary = open_test_db();
    let replica = open_test_db();

    // Same operations on both
    for db in [&primary, &replica] {
        db.execute("CREATE TABLE users(id, name)");
        db.execute("INSERT INTO users VALUES(1, 'alice')");
        db.execute("INSERT INTO users VALUES(2, 'bob')");
    }

    // Setsums should match
    let p_setsum = primary.query("PRAGMA setsum");
    let r_setsum = replica.query("PRAGMA setsum");
    assert_eq!(p_setsum, r_setsum);

    // Simulate divergence
    primary.execute("INSERT INTO users VALUES(3, 'charlie')");

    // Now they should differ
    let p_setsum = primary.query("PRAGMA setsum");
    let r_setsum = replica.query("PRAGMA setsum");
    assert_ne!(p_setsum, r_setsum);
}
```

**Checklist:**
- [ ] Unit tests for Setsum struct
- [ ] Unit tests for row encoding
- [ ] Integration tests for PRAGMA setsum
- [ ] Order-agnostic property tests
- [ ] DELETE/UPDATE tests
- [ ] Rollback tests
- [ ] TCL compatibility tests
- [ ] Replication verification tests

---

## Phase 7: Documentation

### Task 7.1: API Documentation

Document in code:
- `Setsum` struct and methods
- `encode_row()` format specification
- PRAGMA syntax and semantics

### Task 7.2: User Documentation

Add to tursodb docs:
- PRAGMA setsum usage
- Use cases (replication, backup verification)
- Limitations (doesn't track schema changes, etc.)
- Performance characteristics

---

## Implementation Order

**Recommended sequence:**

1. **Phase 1** - Core Setsum implementation (standalone, testable)
2. **Phase 6.1** - Unit tests for Phase 1
3. **Phase 4.1-4.3** - PRAGMA parser/registration
4. **Phase 4.4** - On-demand computation (simplest integration)
5. **Phase 6.2-6.3** - Integration tests
6. **Phase 2** - State management (if incremental tracking needed)
7. **Phase 3** - VDBE hooks (if incremental tracking needed)
8. **Phase 5** - Persistence (if state needs to survive restarts)

**MVP (Minimum Viable Product):**
- Phases 1 + 4 + 6 = `PRAGMA setsum('table')` that computes on-demand
- No incremental tracking, no persistence
- Still useful for verification/comparison

**Full Feature:**
- All phases = incremental tracking with persistence

---

## SQLite Code References

Not applicable - this is a tursodb-specific feature. However, reference:
- `dbhash.c` in SQLite for row hashing patterns
- WAL checksum implementation in `sqlite3_ondisk.rs` for existing checksum patterns

## Turso Code References

| File | Line | Purpose |
|------|------|---------|
| `core/vdbe/execute.rs` | 6368 | `op_insert()` - INSERT execution |
| `core/vdbe/execute.rs` | 6645 | `op_delete()` - DELETE execution |
| `core/storage/btree.rs` | 2174 | BTree insertion |
| `core/storage/checksum.rs` | 82 | Existing page checksums |
| `core/translate/pragma.rs` | - | PRAGMA translation patterns |
| `core/pragma.rs` | - | PRAGMA registration |
| `core/schema.rs` | 148 | System table patterns |
| `setsum/setsum.go` | - | Reference Go implementation |

---

## Design Decisions

### Q: Why SHA3-256 instead of SHA1?
A: SHA3 is more modern, faster on modern CPUs, and provides better security properties. Setsum was designed with SHA3.

### Q: Why 8 columns with separate primes?
A: Each column provides independent verification. Collision requires matching all 8 columns simultaneously, giving 2^256 collision resistance.

### Q: Should schema changes affect setsum?
A: Initial implementation: No. Schema changes (ADD COLUMN, etc.) don't affect setsum. Could add `PRAGMA setsum_schema` separately.

### Q: How to handle WITHOUT ROWID tables?
A: Use PRIMARY KEY values instead of rowid for row identification. The `encode_row()` function should detect table type.

### Q: Performance impact of incremental tracking?
A: Minimal - one SHA3 hash per row modified. SHA3-256 is ~500 MB/s on modern CPUs. For typical row sizes (<1KB), overhead is microseconds.

---

## Limitations

1. **Doesn't detect schema changes** - Only tracks row data
2. **Requires consistent encoding** - Different tursodb versions must use identical encoding
3. **No history** - Can detect divergence but not pinpoint when/where
4. **Can remove non-existent items** - Mathematical property of setsum (usually not a problem)
5. **Floating point edge cases** - NaN, -0.0 need careful handling in encoding
