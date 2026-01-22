# VACUUM Implementation Plan

A hands-on guide for implementing VACUUM in Turso DB.

---

## Table of Contents

1. [Overview](#overview)
2. [Prerequisites](#prerequisites)
3. [Phase 1: Parser & Translation Scaffolding](#phase-1-parser--translation-scaffolding)
4. [Phase 2: Core VACUUM Logic](#phase-2-core-vacuum-logic)
5. [Phase 3: Table & Index Copying](#phase-3-table--index-copying)
6. [Phase 4: Finalization & Swap](#phase-4-finalization--swap)
7. [Phase 5: Edge Cases & Polish](#phase-5-edge-cases--polish)
8. [Phase 6: Testing](#phase-6-testing)
9. [Granular Task Checklist](#granular-task-checklist)

---

## Overview

### What VACUUM Does

```
┌─────────────────────────────────────────────────────────────┐
│                    VACUUM Algorithm                          │
├─────────────────────────────────────────────────────────────┤
│  1. Create temporary database file                          │
│  2. Copy schema from main → temp                            │
│  3. Copy all table data from main → temp                    │
│  4. Copy all index data (rebuilt during INSERT)             │
│  5. Replace main database file with temp                    │
│  6. Invalidate schema cache (bump schema cookie)            │
└─────────────────────────────────────────────────────────────┘
```

### SQLite Reference Files

| File | What to Study |
|------|---------------|
| `sqlite/src/vacuum.c` | Main VACUUM logic (~200 lines) |
| `sqlite/src/backup.c` | Page copying for final swap |
| `sqlite/src/build.c` | Schema operations |
| `sqlite/src/insert.c` | xfer optimization (bulk copy) |
| `sqlite/src/btree.c` | B-tree operations, `sqlite3BtreeBeginTrans` |

### Turso Files to Modify

| File | Purpose |
|------|---------|
| `parser/src/parser.rs` | Already parses VACUUM (verify) |
| `core/translate/mod.rs` | Entry point, currently returns "not supported" |
| `core/translate/vacuum.rs` | **Create this** - main VACUUM translation |
| `core/connection.rs` | May need vacuum-specific connection handling |
| `core/storage/pager.rs` | File operations for swap |
| `core/schema.rs` | Schema cookie invalidation |

---

## Prerequisites

Before starting, ensure you understand:

### 1. Read the Existing Plan Documents
- `plan.md` - Detailed VACUUM analysis
- `vacuum_explanation.md` - SQLite VACUUM walkthrough with code snippets

### 2. Study SQLite's vacuum.c
```bash
# Read the main vacuum function
less sqlite/src/vacuum.c
```
Focus on:
- Lines 150-250: `sqlite3RunVacuum()` function
- The SQL statements it generates and executes
- How it handles the temp database attachment

### 3. Study Turso's Translation Layer
```bash
# See how other statements are translated
ls core/translate/
```
Look at a simple statement translation (e.g., `delete.rs` or `update.rs`) to understand the pattern.

---

## Phase 1: Parser & Translation Scaffolding

**Goal**: Get `VACUUM` to route through the translation layer properly.

### Task 1.1: Verify Parser Support

**Read**: `parser/src/parser.rs` and `parser/src/ast/mod.rs`

Check if VACUUM is already parsed. Search for:
```bash
grep -r "Vacuum" parser/src/
```

The AST should have something like:
```rust
pub enum Stmt {
    // ...
    Vacuum { schema_name: Option<String>, into: Option<Expr> },
}
```

### Task 1.2: Find Current Translation Entry Point

**Read**: `core/translate/mod.rs`

Find where VACUUM is handled. You should see something like:
```rust
Stmt::Vacuum { .. } => {
    todo!("VACUUM not yet supported")
}
```

### Task 1.3: Create vacuum.rs Module

**Create**: `core/translate/vacuum.rs`

Start with a skeleton:
```rust
// Translate VACUUM statement into VDBE program
pub fn translate_vacuum(
    schema_name: Option<&str>,
    into: Option<&Expr>,
    // ... other params from translate context
) -> Result<Program> {
    todo!()
}
```

**Reference**: Look at how `translate_delete` or `translate_update` are structured.

### Task 1.4: Wire Up the Module

**Modify**: `core/translate/mod.rs`

Add `mod vacuum;` and call your new function from the match arm.

---

## Phase 2: Core VACUUM Logic

**Goal**: Implement the main VACUUM algorithm structure.

### Task 2.1: Study SQLite's execSql Helper

**Read**: `sqlite/src/vacuum.c` lines 40-80

SQLite has a helper `execSql()` that runs SQL and handles errors. Understand how it works.

### Task 2.2: Understand the VACUUM SQL Sequence

**Read**: `sqlite/src/vacuum.c` lines 180-280

SQLite generates these SQL statements internally:
```sql
-- 1. Attach temp database
ATTACH '' AS vacuum_db;

-- 2. Set pragmas on temp database
PRAGMA vacuum_db.page_size = <same as main>;
PRAGMA vacuum_db.auto_vacuum = <same as main>;

-- 3. Begin exclusive transaction
BEGIN EXCLUSIVE;

-- 4. Copy schema
SELECT sql FROM main.sqlite_schema WHERE type='table' AND name!='sqlite_sequence';
-- Execute each CREATE TABLE in vacuum_db

-- 5. Copy data for each table
INSERT INTO vacuum_db."tablename" SELECT * FROM main."tablename";

-- 6. Copy sqlite_sequence if exists
INSERT INTO vacuum_db.sqlite_sequence SELECT * FROM main.sqlite_sequence;

-- 7. Copy other schema objects (indexes, views, triggers)
SELECT sql FROM main.sqlite_schema WHERE sql IS NOT NULL AND type!='table';
-- Execute each CREATE INDEX/VIEW/TRIGGER in vacuum_db

-- 8. Commit
COMMIT;

-- 9. Swap files (handled in C code, not SQL)
```

### Task 2.3: Implement Temp Database Creation

**Study**: How Turso handles ATTACH (if supported)

If ATTACH isn't supported, you'll need to:
1. Create a temp file path (e.g., `<dbname>-vacuum-<random>`)
2. Open a new Database connection to it
3. Track it for cleanup

**Read**: `core/lib.rs` - `Database::open_file()`

### Task 2.4: Implement Schema Copy

**Read**: `core/schema.rs` - understand how schema is structured

You need to:
1. Query `sqlite_schema` for all CREATE TABLE statements
2. Execute each in the temp database
3. Handle table ordering (for foreign keys, copy CREATE without constraints first)

**SQLite Reference**: `sqlite/src/vacuum.c` line 215-230

---

## Phase 3: Table & Index Copying

**Goal**: Copy all data from main database to temp database.

### Task 3.1: Study the xfer Optimization

**Read**: `sqlite/src/insert.c` - search for "xfer"

The xfer optimization does bulk B-tree page copying instead of row-by-row INSERT. This is much faster but complex.

**Decision Point**: Start with row-by-row copying (simpler), optimize later with xfer.

### Task 3.2: Implement Row-by-Row Copy (Simple Version)

For each table:
```
1. Prepare: SELECT * FROM main."tablename"
2. Prepare: INSERT INTO vacuum_db."tablename" VALUES (?, ?, ...)
3. For each row from SELECT, bind values and execute INSERT
```

**Read**: `core/statement.rs` - `Statement::step()`, `Statement::bind()`

### Task 3.3: Handle Special Tables

**sqlite_sequence**: Copy after all tables (for AUTOINCREMENT)

**Read**: `sqlite/src/vacuum.c` line 245-250

### Task 3.4: Copy Indexes

After data is copied, recreate indexes:
```sql
SELECT sql FROM main.sqlite_schema WHERE type='index' AND sql IS NOT NULL;
-- Execute each CREATE INDEX in vacuum_db
```

Indexes are rebuilt during creation, which is actually what we want (defragmentation).

---

## Phase 4: Finalization & Swap

**Goal**: Replace the original database with the vacuumed one.

### Task 4.1: Study SQLite's File Swap

**Read**: `sqlite/src/backup.c` - `sqlite3BtreeCopyFile()`

SQLite copies pages from temp back to main (rather than file rename) to preserve file handles and avoid issues with open connections.

### Task 4.2: Implement Page Copy-Back

**Read**: `core/storage/pager.rs`

You need to:
1. Lock both databases exclusively
2. Copy pages from temp to main
3. Update page count in main header
4. Sync to disk

**Alternative (simpler)**: If you can guarantee single connection:
1. Close main database
2. File rename temp → main
3. Reopen main

### Task 4.3: Invalidate Schema Cache

**Read**: `core/schema.rs` - schema cookie handling

After swap, increment the schema cookie to force all connections to re-read schema.

**Read**: `sqlite/src/vacuum.c` line 320-330 - cookie update

### Task 4.4: Cleanup

- Delete temp file if swap failed
- Release locks
- Return success/failure

---

## Phase 5: Edge Cases & Polish

**Goal**: Handle all the corner cases.

### Task 5.1: VACUUM INTO Support

**Read**: `sqlite/src/vacuum.c` - search for "INTO"

VACUUM INTO copies to a specified file instead of replacing the original:
```sql
VACUUM INTO 'backup.db';
```

This is simpler than regular VACUUM (no swap needed).

### Task 5.2: Empty Database Handling

What if database is empty? Handle gracefully.

### Task 5.3: Error Recovery

If VACUUM fails midway:
- Temp file should be deleted
- Main database should be unchanged
- Locks should be released

### Task 5.4: Concurrent Access

**Read**: `plan.md` - section on locking

VACUUM requires exclusive access. How to handle:
- Active readers?
- Active writers?
- WAL mode?

### Task 5.5: MVCC Interaction

**Read**: `plan.md` - MVCC section

If MVCC is active, need to checkpoint and clear the in-memory store before VACUUM.

---

## Phase 6: Testing

### Task 6.1: Basic Tests

Create `tests/integration/vacuum.rs`:

```rust
#[turso_macros::test]
fn test_vacuum_empty_db(tmp_db: TempDatabase) { ... }

#[turso_macros::test]
fn test_vacuum_single_table(tmp_db: TempDatabase) { ... }

#[turso_macros::test]
fn test_vacuum_preserves_data(tmp_db: TempDatabase) { ... }

#[turso_macros::test]
fn test_vacuum_reduces_file_size(tmp_db: TempDatabase) { ... }
```

### Task 6.2: Integrity Tests

Use dbhash (once implemented) to verify content unchanged:
```rust
let hash_before = dbhash(&db_path);
conn.execute("VACUUM")?;
let hash_after = dbhash(&db_path);
assert_eq!(hash_before, hash_after);
```

### Task 6.3: Edge Case Tests

- Table with indexes
- Table with triggers
- Table with foreign keys
- WITHOUT ROWID tables
- Tables with generated columns
- Empty tables mixed with populated tables

### Task 6.4: TCL Compatibility Tests

**Read**: `sqlite/test/vacuum.test` and `sqlite/test/vacuum2.test`

Migrate relevant tests to Turso's test format.

---

## Granular Task Checklist

### Phase 1: Scaffolding
- [ ] 1.1 Verify VACUUM is parsed correctly (check AST)
- [ ] 1.2 Find current "not supported" handling in translate/mod.rs
- [ ] 1.3 Create `core/translate/vacuum.rs` with skeleton function
- [ ] 1.4 Wire up module in `translate/mod.rs`
- [ ] 1.5 Verify VACUUM now hits your new code (add debug print)

### Phase 2: Core Logic
- [ ] 2.1 Create temp database file (unique name in same directory)
- [ ] 2.2 Open connection to temp database
- [ ] 2.3 Copy page_size pragma to temp
- [ ] 2.4 Begin exclusive transaction on main
- [ ] 2.5 Query sqlite_schema for CREATE TABLE statements
- [ ] 2.6 Execute each CREATE TABLE in temp database
- [ ] 2.7 Verify schema copied correctly

### Phase 3: Data Copy
- [ ] 3.1 Get list of all tables from schema
- [ ] 3.2 For each table, execute INSERT...SELECT to copy data
- [ ] 3.3 Handle sqlite_sequence table specially
- [ ] 3.4 Query sqlite_schema for CREATE INDEX statements
- [ ] 3.5 Execute each CREATE INDEX in temp database
- [ ] 3.6 Query for CREATE VIEW and CREATE TRIGGER
- [ ] 3.7 Execute views and triggers in temp

### Phase 4: Finalization
- [ ] 4.1 Implement page copy from temp to main
- [ ] 4.2 Update main database header (page count)
- [ ] 4.3 Increment schema cookie
- [ ] 4.4 Sync main database to disk
- [ ] 4.5 Delete temp file
- [ ] 4.6 Commit transaction

### Phase 5: Edge Cases
- [ ] 5.1 Implement VACUUM INTO variant
- [ ] 5.2 Handle empty database
- [ ] 5.3 Add error recovery (delete temp on failure)
- [ ] 5.4 Handle busy database (return SQLITE_BUSY)
- [ ] 5.5 Document MVCC interaction

### Phase 6: Testing
- [ ] 6.1 Test: empty database VACUUM
- [ ] 6.2 Test: single table with data
- [ ] 6.3 Test: multiple tables
- [ ] 6.4 Test: tables with indexes
- [ ] 6.5 Test: data integrity (compare before/after)
- [ ] 6.6 Test: file size reduction after DELETE + VACUUM
- [ ] 6.7 Test: VACUUM INTO
- [ ] 6.8 Test: error handling (disk full, locked, etc.)
- [ ] 6.9 Migrate SQLite TCL tests

---

## Quick Reference: Key SQLite Code Sections

| Task | SQLite File | Lines | What to Learn |
|------|-------------|-------|---------------|
| Main algorithm | vacuum.c | 150-350 | Overall flow |
| Attach temp DB | vacuum.c | 170-180 | Temp DB naming |
| Copy schema | vacuum.c | 200-230 | SQL generation |
| Copy data | vacuum.c | 235-250 | INSERT...SELECT |
| Copy indexes | vacuum.c | 255-270 | Index recreation |
| Page swap | backup.c | 800-900 | Page copy-back |
| Exclusive lock | btree.c | 3300-3400 | wrflag=2 handling |
| Schema cookie | btree.c | 1700-1750 | Cookie update |

---

## Quick Reference: Key Turso Code Sections

| Task | Turso File | What to Study |
|------|------------|---------------|
| Translation entry | translate/mod.rs | How statements are dispatched |
| Schema access | schema.rs | How to query sqlite_schema |
| Statement execution | statement.rs | step(), bind(), row() |
| Database open | lib.rs | Database::open_file() |
| Page operations | storage/pager.rs | Page read/write |
| File operations | storage/sqlite3_ondisk.rs | Low-level file I/O |
| Connection handling | connection.rs | Transaction management |

---

## Crash Safety Requirements

VACUUM must be **crash-safe**: the original database must never be corrupted, even if power fails mid-operation.

### The Fundamental Guarantee

```
┌─────────────────────────────────────────────────────────────────────────┐
│  VACUUM is ALL-OR-NOTHING                                               │
│                                                                         │
│  • If VACUUM completes: database is defragmented                        │
│  • If crash/failure at ANY point: database is unchanged (original state)│
│  • Never: partial/corrupted state                                       │
└─────────────────────────────────────────────────────────────────────────┘
```

### Why It's Safe: The Two-Phase Approach

```
Phase 1-3: Building temp database (SAFE)
════════════════════════════════════════════════════════════════════

  main.db  ─────────────────────────────────────────────  UNTOUCHED
                                                          (read-only)

  main.db-vacuum-XXXXX  ←── being built                   TEMP FILE
                            (CREATE TABLEs, INSERTs)

  CRASH HERE?
  └── main.db is perfectly fine
  └── temp file is orphaned garbage (deleted on next open)


Phase 4: Page copy-back (PROTECTED BY JOURNALING)
════════════════════════════════════════════════════════════════════

  Before each page overwrite:
  ┌─────────────────┐    ┌─────────────────┐
  │  main.db        │───▶│ main.db-journal │  (original page saved)
  │  page N         │    │ page N backup   │
  └─────────────────┘    └─────────────────┘
          │
          ▼
  ┌─────────────────┐
  │  main.db        │  (new page from vacuum_db)
  │  page N (new)   │
  └─────────────────┘

  CRASH HERE?
  └── On recovery, SQLite sees "hot journal"
  └── Rolls back main.db using journal
  └── Database returns to pre-VACUUM state
  └── VACUUM "never happened"


Phase 5: Commit (ATOMIC POINT)
════════════════════════════════════════════════════════════════════

  The SINGLE atomic operation that commits VACUUM:

  Rollback mode: DELETE journal file  ←── atomic (filesystem guarantees)
  WAL mode:      Write commit frame   ←── atomic (single write)

  Before this point: crash = rollback
  After this point:  VACUUM succeeded
```

### Failure Scenarios Table

| When Crash Occurs | main.db State | Recovery Action |
|-------------------|---------------|-----------------|
| During temp DB creation | Unchanged | Delete orphan temp file |
| During data copy to temp | Unchanged | Delete orphan temp file |
| During page copy-back | Unchanged | Journal rollback restores original |
| After commit, before cleanup | **Vacuumed** (success!) | Delete orphan temp file |
| Disk full during temp creation | Unchanged | Error returned, temp deleted |
| Disk full during copy-back | Unchanged | Journal rollback, temp deleted |

### Implementation Requirements

#### 1. Temp File Isolation
```
DO:    Create temp file with unique name: {db_path}-vacuum-{random}
DO:    Never write to main.db until copy-back phase
DON'T: Modify main.db during phases 1-3
```

#### 2. Copy-Back Through Pager
```
DO:    Use pager.write_page() for copy-back (this journals automatically)
DON'T: Write directly to file bypassing pager
```

#### 3. Orphan File Cleanup
```rust
// On database open, clean up any orphan vacuum temp files
fn cleanup_orphan_vacuum_files(db_path: &str) {
    let pattern = format!("{}-vacuum-*", db_path);
    for orphan in glob(&pattern) {
        std::fs::remove_file(orphan).ok();  // Best effort
    }
}
```

#### 4. Error Handling
```rust
fn vacuum(db: &Database) -> Result<()> {
    let temp_path = create_temp_path(db.path());

    // Use RAII guard for cleanup
    let _cleanup = scopeguard::guard((), |_| {
        let _ = std::fs::remove_file(&temp_path);  // Always delete temp
    });

    // Phase 1-3: Build temp (safe, main untouched)
    build_vacuum_db(&temp_path, db)?;

    // Phase 4: Copy-back (journaled)
    copy_pages_back(db, &temp_path)?;  // Goes through pager

    // Phase 5: Commit happens inside copy_pages_back
    // If we reach here, VACUUM succeeded

    Ok(())
    // _cleanup runs: deletes temp file
}
```

### WAL Mode Considerations

```
Before VACUUM in WAL mode:
1. Checkpoint WAL (flush all WAL frames to main db)
2. Switch to exclusive lock (blocks all other connections)
3. Proceed with VACUUM
4. WAL is effectively reset after VACUUM
```

**Read**: `sqlite/src/vacuum.c` lines 160-170 for WAL handling

### Testing Crash Safety

```rust
#[test]
fn test_crash_during_vacuum_leaves_db_intact() {
    let db = create_test_db_with_data();
    let original_hash = dbhash(&db);

    // Simulate crash during VACUUM (e.g., panic in copy-back)
    let result = std::panic::catch_unwind(|| {
        vacuum_with_simulated_crash(&db);
    });

    assert!(result.is_err());  // "Crashed"

    // Reopen database - should recover automatically
    let db = reopen_database(&db.path());
    let recovered_hash = dbhash(&db);

    // Original data must be intact
    assert_eq!(original_hash, recovered_hash);
}
```

### The Atomic Commit Point

This is the **most critical** concept:

```
Everything before journal delete: can be rolled back
────────────────────────────────────────────────────
                    │
                    ▼
            DELETE journal file  ◄── ATOMIC COMMIT POINT
                    │
────────────────────────────────────────────────────
Everything after: VACUUM is committed
```

The filesystem guarantees that file deletion is atomic. Once the journal is deleted, there's no going back - but at that point, all pages have been successfully copied.

---

## Recommended Implementation Order

Given the complexity, implement in this order:

### Step 1: VACUUM INTO (Simplest)
```sql
VACUUM INTO 'backup.db';
```
- No file swap needed
- No crash safety complexity
- Just: create new file, copy everything, done
- If crash: partial backup.db exists (user deletes it)

### Step 2: Regular VACUUM (Simple Version)
- Single connection only
- Close main, rename temp → main, reopen
- Works but doesn't preserve file handles

### Step 3: Regular VACUUM (Full Version)
- Page copy-back through pager
- Journal-protected
- Handles multiple connections
- Production-ready

---

## Tips

1. **Start simple**: Get basic VACUUM working with row-by-row copy first
2. **Test incrementally**: After each phase, write tests
3. **Compare with SQLite**: Use `EXPLAIN` to see what SQLite does, compare bytecode
4. **Use logging**: Add tracing to debug issues
5. **Read the tests**: SQLite's TCL tests show expected behavior
6. **Implement VACUUM INTO first**: It's 80% of the work without the hard parts

Good luck! Ask if you get stuck on any specific task.
