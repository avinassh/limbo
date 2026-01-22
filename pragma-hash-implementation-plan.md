# PRAGMA hash Implementation Plan

A detailed guide for implementing `PRAGMA hash` - a built-in database content hashing utility for Turso DB.

---

## Table of Contents

1. [Overview](#overview)
2. [Architecture](#architecture)
3. [Phase 1: Parser & Registration](#phase-1-parser--registration)
4. [Phase 2: Translation Layer](#phase-2-translation-layer)
5. [Phase 3: VDBE Instruction](#phase-3-vdbe-instruction)
6. [Phase 4: Hash Computation](#phase-4-hash-computation)
7. [Phase 5: Variants & Options](#phase-5-variants--options)
8. [Phase 6: Testing](#phase-6-testing)
9. [Granular Task Checklist](#granular-task-checklist)

---

## Overview

### What PRAGMA hash Does

```sql
-- Basic usage: hash entire database content
PRAGMA hash;
--> "a3f2b8c9d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9"

-- Hash specific table
PRAGMA hash('users');
--> "b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0"

-- Hash with options
PRAGMA hash_schema;      -- Schema only
PRAGMA hash_content;     -- Content only (no schema)
```

### Why Built-in vs CLI Tool

| Feature | PRAGMA hash | CLI dbhash |
|---------|-------------|------------|
| No external tool needed | ✓ | ✗ |
| Works in SQL scripts | ✓ | ✗ |
| Usable from any binding | ✓ | ✗ |
| Can filter by table | ✓ | ✓ |
| Standalone usage | ✗ | ✓ |

### Algorithm (Same as SQLite dbhash)

```
For each table (ordered by name, case-insensitive):
    For each row (ordered by PRIMARY KEY):
        For each column:
            Hash: type_prefix + normalized_value

Then hash schema (type, name, tbl_name, sql)

Type prefixes:
  '0' = NULL
  '1' = INTEGER (8 bytes big-endian)
  '2' = FLOAT (8 bytes big-endian IEEE 754)
  '3' = TEXT (raw UTF-8 bytes)
  '4' = BLOB (raw bytes)
```

---

## Architecture

### PRAGMA Flow in Turso

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           PRAGMA hash Flow                               │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  1. Parser                                                               │
│     parser/src/ast.rs         →  PragmaName::Hash enum variant          │
│     parser/src/parser.rs      →  Already handles PRAGMA syntax          │
│                                                                          │
│  2. Metadata                                                             │
│     core/pragma.rs            →  pragma_for() returns columns & flags   │
│                                                                          │
│  3. Translation                                                          │
│     core/translate/pragma.rs  →  query_pragma() emits VDBE bytecode     │
│                                                                          │
│  4. VDBE Instruction                                                     │
│     core/vdbe/insn.rs         →  Insn::ComputeHash instruction          │
│     core/vdbe/execute.rs      →  Execute hash computation               │
│                                                                          │
│  5. Result                                                               │
│     Returns single row: { hash: "40-char-hex-sha1" }                    │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

### Files to Modify/Create

| File | Action | Purpose |
|------|--------|---------|
| `parser/src/ast.rs` | Modify | Add `Hash` to `PragmaName` enum |
| `core/pragma.rs` | Modify | Register in `pragma_for()` |
| `core/translate/pragma.rs` | Modify | Add translation case |
| `core/translate/hash.rs` | **Create** | Hash computation helper |
| `core/vdbe/insn.rs` | Modify | Add `ComputeHash` instruction |
| `core/vdbe/execute.rs` | Modify | Implement instruction execution |

---

## Phase 1: Parser & Registration

**Goal**: Make `PRAGMA hash` recognized by the system.

### Task 1.1: Add PragmaName Variant

**File**: `parser/src/ast.rs` (around line 1490)

**Read first**: Look at the existing `PragmaName` enum:
```bash
grep -A 50 "pub enum PragmaName" parser/src/ast.rs
```

**Add**:
```rust
#[derive(Clone, Debug, PartialEq, EnumIter, EnumString, strum::Display)]
#[strum(serialize_all = "snake_case")]
pub enum PragmaName {
    // ... existing variants ...

    Hash,           // PRAGMA hash - full database hash
    HashSchema,     // PRAGMA hash_schema - schema only
    HashContent,    // PRAGMA hash_content - content only
}
```

**Note**: The `#[strum(serialize_all = "snake_case")]` means:
- `Hash` → parses/displays as `"hash"`
- `HashSchema` → parses/displays as `"hash_schema"`

### Task 1.2: Register PRAGMA Metadata

**File**: `core/pragma.rs`

**Read first**: Understand the `Pragma` struct and `PragmaFlags`:
```bash
grep -B 5 -A 30 "pub fn pragma_for" core/pragma.rs
```

**Add to `pragma_for()` match**:
```rust
PragmaName::Hash => Pragma::new(
    PragmaFlags::NeedSchema | PragmaFlags::ReadOnly | PragmaFlags::Result0 | PragmaFlags::Result1,
    &["hash"],
),
PragmaName::HashSchema => Pragma::new(
    PragmaFlags::NeedSchema | PragmaFlags::ReadOnly | PragmaFlags::Result0,
    &["hash"],
),
PragmaName::HashContent => Pragma::new(
    PragmaFlags::NeedSchema | PragmaFlags::ReadOnly | PragmaFlags::Result0 | PragmaFlags::Result1,
    &["hash"],
),
```

**Flags explanation**:
- `NeedSchema`: Force schema load before running
- `ReadOnly`: Doesn't modify database
- `Result0`: Acts as query when no argument (`PRAGMA hash`)
- `Result1`: Acts as query when has one argument (`PRAGMA hash('tablename')`)

### Task 1.3: Verify Parsing Works

After adding, test that parsing recognizes it:
```rust
// In a test or debug session
let stmt = parse("PRAGMA hash");
// Should give: Stmt::Pragma { name: QualifiedName { name: "hash" }, body: None }

let stmt = parse("PRAGMA hash('users')");
// Should give: Stmt::Pragma { name: ..., body: Some(PragmaBody::Call(Expr::Literal("users"))) }
```

---

## Phase 2: Translation Layer

**Goal**: Translate `PRAGMA hash` into VDBE bytecode.

### Task 2.1: Study Existing Query Pragma Pattern

**File**: `core/translate/pragma.rs`

**Read**: Look at how `PageCount` or `IntegrityCheck` are handled:
```bash
grep -A 20 "PragmaName::PageCount =>" core/translate/pragma.rs
grep -A 20 "PragmaName::IntegrityCheck =>" core/translate/pragma.rs
```

### Task 2.2: Add Hash to Query Dispatch

**File**: `core/translate/pragma.rs`

Find the match in `translate_pragma()` that decides query vs update (around line 60-80):
```rust
Some(ast::PragmaBody::Equals(value) | ast::PragmaBody::Call(value)) => match pragma {
    PragmaName::TableInfo | PragmaName::TableXinfo |
    PragmaName::IntegrityCheck | PragmaName::QuickCheck |
    PragmaName::Hash | PragmaName::HashContent => {  // ADD THESE
        query_pragma(pragma, resolver, Some(*value), pager, connection, program)?
    }
    // ...
}
```

### Task 2.3: Implement query_pragma Case

**File**: `core/translate/pragma.rs` (in `query_pragma()` function)

**Add case**:
```rust
PragmaName::Hash => {
    let table_filter = match value {
        Some(ast::Expr::Literal(ast::Literal::String(s))) => Some(s),
        Some(ast::Expr::Name(name)) => Some(name.0),
        _ => None,
    };

    translate_hash(
        &mut program,
        resolver,
        table_filter.as_deref(),
        HashMode::Full,  // Both schema and content
    )?;

    program.add_pragma_result_column("hash".into());
    Ok((program, TransactionMode::Read))
}

PragmaName::HashSchema => {
    translate_hash(&mut program, resolver, None, HashMode::SchemaOnly)?;
    program.add_pragma_result_column("hash".into());
    Ok((program, TransactionMode::Read))
}

PragmaName::HashContent => {
    let table_filter = extract_table_filter(&value);
    translate_hash(&mut program, resolver, table_filter, HashMode::ContentOnly)?;
    program.add_pragma_result_column("hash".into());
    Ok((program, TransactionMode::Read))
}
```

### Task 2.4: Create Hash Translation Helper

**Create file**: `core/translate/hash.rs`

```rust
//! Hash computation translation for PRAGMA hash

use crate::vdbe::builder::ProgramBuilder;
use crate::translate::Resolver;

pub enum HashMode {
    Full,         // Hash both content and schema
    SchemaOnly,   // Hash schema only
    ContentOnly,  // Hash content only
}

/// Translate PRAGMA hash into VDBE bytecode
pub fn translate_hash(
    program: &mut ProgramBuilder,
    resolver: &Resolver,
    table_filter: Option<&str>,
    mode: HashMode,
) -> crate::Result<()> {
    let dest_reg = program.alloc_register();

    // Emit the ComputeHash instruction
    program.emit_insn(Insn::ComputeHash {
        db: 0,
        dest: dest_reg,
        table_filter: table_filter.map(|s| s.to_string()),
        mode,
    });

    // Emit result row with the hash
    program.emit_result_row(dest_reg, 1);

    Ok(())
}
```

### Task 2.5: Wire Up Module

**File**: `core/translate/mod.rs`

Add:
```rust
mod hash;
pub use hash::{translate_hash, HashMode};
```

---

## Phase 3: VDBE Instruction

**Goal**: Define and implement the `ComputeHash` VDBE instruction.

### Task 3.1: Define Instruction

**File**: `core/vdbe/insn.rs`

**Add to `Insn` enum**:
```rust
pub enum Insn {
    // ... existing instructions ...

    /// Compute SHA1 hash of database content
    /// Stores 40-char hex string in dest register
    ComputeHash {
        db: usize,
        dest: usize,
        table_filter: Option<String>,
        mode: HashMode,
    },
}
```

**Also add HashMode if not importing from translate**:
```rust
#[derive(Clone, Debug)]
pub enum HashMode {
    Full,
    SchemaOnly,
    ContentOnly,
}
```

### Task 3.2: Add Instruction Execution

**File**: `core/vdbe/execute.rs` (or wherever instruction execution happens)

**Find pattern**: Look for how other instructions are executed:
```bash
grep -A 30 "Insn::PageCount" core/vdbe/execute.rs
```

**Add execution**:
```rust
Insn::ComputeHash { db, dest, table_filter, mode } => {
    let hash = compute_database_hash(
        state,
        *db,
        table_filter.as_deref(),
        mode,
    )?;

    state.registers[*dest] = Value::Text(Text::new(hash));
    state.pc += 1;
}
```

---

## Phase 4: Hash Computation

**Goal**: Implement the actual hash algorithm.

### Task 4.1: Create Hash Computation Module

**Create file**: `core/vdbe/hash.rs` (or add to existing file)

```rust
//! Database content hash computation

use sha1::{Sha1, Digest};
use crate::Value;

/// Compute SHA1 hash of database content
pub fn compute_database_hash(
    state: &ExecutionState,
    db_idx: usize,
    table_filter: Option<&str>,
    mode: &HashMode,
) -> crate::Result<String> {
    let mut hasher = Sha1::new();
    let connection = &state.connection;
    let schema = connection.schema()?;

    let filter = table_filter.unwrap_or("%");

    // 1. Hash table content (unless SchemaOnly)
    if !matches!(mode, HashMode::SchemaOnly) {
        let tables = get_hashable_tables(&schema, filter)?;
        for table_name in tables {
            hash_table_content(connection, &table_name, &mut hasher)?;
        }
    }

    // 2. Hash schema (unless ContentOnly)
    if !matches!(mode, HashMode::ContentOnly) {
        hash_schema(connection, filter, &mut hasher)?;
    }

    // 3. Finalize and return hex string
    let hash_bytes = hasher.finalize();
    Ok(hex::encode(hash_bytes))
}

/// Get list of tables to hash (excludes sqlite_%, virtual tables)
fn get_hashable_tables(schema: &Schema, like_pattern: &str) -> crate::Result<Vec<String>> {
    let mut tables = Vec::new();

    for table in schema.tables() {
        let name = table.name();

        // Skip system tables
        if name.starts_with("sqlite_") {
            continue;
        }

        // Skip virtual tables
        if table.is_virtual() {
            continue;
        }

        // Apply LIKE filter
        if !sql_like_match(name, like_pattern) {
            continue;
        }

        tables.push(name.to_string());
    }

    // Sort case-insensitively for deterministic order
    tables.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));

    Ok(tables)
}

/// Hash all rows in a table
fn hash_table_content(
    connection: &Connection,
    table_name: &str,
    hasher: &mut Sha1,
) -> crate::Result<()> {
    let sql = format!("SELECT * FROM \"{}\"", escape_identifier(table_name));
    let mut stmt = connection.prepare(&sql)?;

    loop {
        match stmt.step()? {
            StepResult::Row => {
                if let Some(row) = stmt.row() {
                    for value in row.get_values() {
                        hash_value(value, hasher);
                    }
                }
            }
            StepResult::IO => connection.io().step()?,
            StepResult::Done => break,
            _ => break,
        }
    }

    Ok(())
}

/// Hash a single value with type prefix
fn hash_value(value: &Value, hasher: &mut Sha1) {
    match value {
        Value::Null => {
            hasher.update(b"0");
        }
        Value::Integer(v) => {
            hasher.update(b"1");
            hasher.update(&v.to_be_bytes());
        }
        Value::Float(v) => {
            hasher.update(b"2");
            hasher.update(&v.to_bits().to_be_bytes());
        }
        Value::Text(text) => {
            hasher.update(b"3");
            hasher.update(text.as_str().as_bytes());
        }
        Value::Blob(blob) => {
            hasher.update(b"4");
            hasher.update(blob);
        }
    }
}

/// Hash schema entries
fn hash_schema(
    connection: &Connection,
    like_pattern: &str,
    hasher: &mut Sha1,
) -> crate::Result<()> {
    let sql = format!(
        r#"SELECT type, name, tbl_name, sql FROM sqlite_schema
           WHERE tbl_name LIKE '{}'
           ORDER BY name COLLATE nocase"#,
        escape_sql_string(like_pattern)
    );

    let mut stmt = connection.prepare(&sql)?;

    loop {
        match stmt.step()? {
            StepResult::Row => {
                if let Some(row) = stmt.row() {
                    for value in row.get_values() {
                        hash_value(value, hasher);
                    }
                }
            }
            StepResult::IO => connection.io().step()?,
            StepResult::Done => break,
            _ => break,
        }
    }

    Ok(())
}

fn escape_identifier(s: &str) -> String {
    s.replace('"', "\"\"")
}

fn escape_sql_string(s: &str) -> String {
    s.replace('\'', "''")
}

fn sql_like_match(value: &str, pattern: &str) -> bool {
    // Simple LIKE matching (% = any, _ = single char)
    // For full implementation, use existing SQL LIKE logic
    if pattern == "%" {
        return true;
    }
    // TODO: implement proper LIKE matching
    value.to_lowercase().contains(&pattern.to_lowercase().replace('%', ""))
}
```

### Task 4.2: Add SHA1 Dependency

**File**: `core/Cargo.toml`

Add:
```toml
[dependencies]
sha1 = "0.10"
hex = "0.4"
```

Or use workspace dependencies if available:
```toml
[dependencies]
sha1 = { workspace = true }
hex = { workspace = true }
```

### Task 4.3: Wire Up Hash Module

**File**: `core/vdbe/mod.rs`

Add:
```rust
mod hash;
pub use hash::compute_database_hash;
```

---

## Phase 5: Variants & Options

**Goal**: Support different hash modes and options.

### Task 5.1: PRAGMA hash_schema

Already handled in Phase 2 - returns hash of schema only (useful for checking if schema changed).

### Task 5.2: PRAGMA hash_content

Already handled in Phase 2 - returns hash of data only (useful for checking if data changed).

### Task 5.3: Table Filtering

```sql
PRAGMA hash('users');           -- Hash only 'users' table
PRAGMA hash_content('user%');   -- Hash tables matching 'user%'
```

Already handled via `table_filter` parameter.

### Task 5.4: Consider Future Extensions

These could be added later:

```sql
-- Hash specific columns
PRAGMA hash('users', 'id,name');

-- Hash with custom algorithm
PRAGMA hash('users') USING md5;

-- Hash excluding certain tables
PRAGMA hash EXCEPT 'logs%';
```

---

## Phase 6: Testing

### Task 6.1: Basic Functionality Tests

```rust
#[turso_macros::test(init_sql = "CREATE TABLE t(x INT); INSERT INTO t VALUES(1);")]
fn test_pragma_hash_basic(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    let result: Vec<(String,)> = conn.exec_rows("PRAGMA hash");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0.len(), 40);  // SHA1 = 40 hex chars

    Ok(())
}
```

### Task 6.2: Determinism Tests

```rust
#[test]
fn test_pragma_hash_deterministic() -> anyhow::Result<()> {
    let db1 = TempDatabase::new("test1.db");
    let db2 = TempDatabase::new("test2.db");

    let sql = "CREATE TABLE t(a INT, b TEXT); INSERT INTO t VALUES(1, 'hello');";

    db1.connect_limbo().execute(sql)?;
    db2.connect_limbo().execute(sql)?;

    let hash1: String = db1.connect_limbo().exec_rows("PRAGMA hash")[0].0;
    let hash2: String = db2.connect_limbo().exec_rows("PRAGMA hash")[0].0;

    assert_eq!(hash1, hash2, "Same content should produce same hash");

    Ok(())
}
```

### Task 6.3: Content Change Detection

```rust
#[turso_macros::test(init_sql = "CREATE TABLE t(x INT); INSERT INTO t VALUES(1);")]
fn test_pragma_hash_detects_change(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    let hash_before: String = conn.exec_rows("PRAGMA hash")[0].0;

    conn.execute("INSERT INTO t VALUES(2)")?;

    let hash_after: String = conn.exec_rows("PRAGMA hash")[0].0;

    assert_ne!(hash_before, hash_after, "Hash should change after INSERT");

    Ok(())
}
```

### Task 6.4: Schema vs Content

```rust
#[turso_macros::test(init_sql = "CREATE TABLE t(x INT); INSERT INTO t VALUES(1);")]
fn test_pragma_hash_schema_vs_content(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    let full: String = conn.exec_rows("PRAGMA hash")[0].0;
    let schema: String = conn.exec_rows("PRAGMA hash_schema")[0].0;
    let content: String = conn.exec_rows("PRAGMA hash_content")[0].0;

    // All three should be different
    assert_ne!(full, schema);
    assert_ne!(full, content);
    assert_ne!(schema, content);

    // Modify data only
    conn.execute("INSERT INTO t VALUES(2)")?;

    let schema_after: String = conn.exec_rows("PRAGMA hash_schema")[0].0;
    let content_after: String = conn.exec_rows("PRAGMA hash_content")[0].0;

    assert_eq!(schema, schema_after, "Schema hash unchanged after INSERT");
    assert_ne!(content, content_after, "Content hash changed after INSERT");

    Ok(())
}
```

### Task 6.5: Table Filtering

```rust
#[turso_macros::test]
fn test_pragma_hash_table_filter(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    conn.execute("CREATE TABLE users(id INT)")?;
    conn.execute("CREATE TABLE orders(id INT)")?;
    conn.execute("INSERT INTO users VALUES(1)")?;
    conn.execute("INSERT INTO orders VALUES(2)")?;

    let all: String = conn.exec_rows("PRAGMA hash")[0].0;
    let users_only: String = conn.exec_rows("PRAGMA hash('users')")[0].0;
    let orders_only: String = conn.exec_rows("PRAGMA hash('orders')")[0].0;

    assert_ne!(all, users_only);
    assert_ne!(all, orders_only);
    assert_ne!(users_only, orders_only);

    Ok(())
}
```

### Task 6.6: VACUUM Invariance

```rust
#[turso_macros::test(init_sql = "CREATE TABLE t(x INT); INSERT INTO t VALUES(1),(2),(3);")]
fn test_pragma_hash_vacuum_invariant(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    let hash_before: String = conn.exec_rows("PRAGMA hash")[0].0;

    conn.execute("VACUUM")?;  // Once VACUUM is implemented

    let hash_after: String = conn.exec_rows("PRAGMA hash")[0].0;

    assert_eq!(hash_before, hash_after, "VACUUM should not change hash");

    Ok(())
}
```

### Task 6.7: Compatibility with SQLite dbhash

```rust
#[test]
#[ignore]  // Requires SQLite dbhash installed
fn test_pragma_hash_sqlite_compatible() {
    // Create database, hash with both implementations
    // Compare results
}
```

---

## Granular Task Checklist

### Phase 1: Parser & Registration
- [ ] 1.1 Add `Hash`, `HashSchema`, `HashContent` to `PragmaName` enum in `parser/src/ast.rs`
- [ ] 1.2 Add metadata in `pragma_for()` in `core/pragma.rs`
- [ ] 1.3 Verify parsing works (write quick test)

### Phase 2: Translation Layer
- [ ] 2.1 Add hash pragmas to query dispatch in `translate_pragma()`
- [ ] 2.2 Create `core/translate/hash.rs` with `translate_hash()` function
- [ ] 2.3 Define `HashMode` enum
- [ ] 2.4 Wire up module in `core/translate/mod.rs`
- [ ] 2.5 Verify translation produces bytecode (debug print)

### Phase 3: VDBE Instruction
- [ ] 3.1 Add `ComputeHash` to `Insn` enum in `core/vdbe/insn.rs`
- [ ] 3.2 Add execution case in `core/vdbe/execute.rs`
- [ ] 3.3 Verify instruction is reached (debug print)

### Phase 4: Hash Computation
- [ ] 4.1 Add `sha1` and `hex` dependencies to `core/Cargo.toml`
- [ ] 4.2 Create `core/vdbe/hash.rs` with `compute_database_hash()`
- [ ] 4.3 Implement `get_hashable_tables()` - table enumeration
- [ ] 4.4 Implement `hash_table_content()` - row iteration
- [ ] 4.5 Implement `hash_value()` - value encoding with type prefix
- [ ] 4.6 Implement `hash_schema()` - schema hashing
- [ ] 4.7 Wire up module in `core/vdbe/mod.rs`
- [ ] 4.8 Test basic hash computation

### Phase 5: Variants
- [ ] 5.1 Verify `PRAGMA hash_schema` works
- [ ] 5.2 Verify `PRAGMA hash_content` works
- [ ] 5.3 Verify table filtering works: `PRAGMA hash('tablename')`
- [ ] 5.4 Verify LIKE pattern works: `PRAGMA hash('user%')`

### Phase 6: Testing
- [ ] 6.1 Test: basic hash returns 40-char hex string
- [ ] 6.2 Test: same content = same hash (determinism)
- [ ] 6.3 Test: different content = different hash
- [ ] 6.4 Test: INSERT changes hash
- [ ] 6.5 Test: schema vs content modes
- [ ] 6.6 Test: table filtering
- [ ] 6.7 Test: system tables excluded
- [ ] 6.8 Test: VACUUM preserves hash (when implemented)
- [ ] 6.9 Test: type prefix distinguishes NULL/0/0.0/"0"/x'00'

---

## Quick Reference: Files to Modify

| File | Changes |
|------|---------|
| `parser/src/ast.rs:~1490` | Add `Hash`, `HashSchema`, `HashContent` to `PragmaName` |
| `core/pragma.rs:~50` | Add to `pragma_for()` match |
| `core/translate/pragma.rs:~70` | Add to query dispatch match |
| `core/translate/pragma.rs:~500` | Add `query_pragma` cases |
| `core/translate/hash.rs` | **Create** - translation helper |
| `core/translate/mod.rs` | Add `mod hash;` |
| `core/vdbe/insn.rs` | Add `ComputeHash` instruction |
| `core/vdbe/execute.rs` | Add execution handler |
| `core/vdbe/hash.rs` | **Create** - hash computation |
| `core/vdbe/mod.rs` | Add `mod hash;` |
| `core/Cargo.toml` | Add `sha1`, `hex` dependencies |

---

## Comparison: PRAGMA hash vs CLI dbhash

Both compute the same hash, but:

| Aspect | PRAGMA hash | CLI dbhash |
|--------|-------------|------------|
| **Location** | Inside database engine | Standalone tool |
| **Implementation** | VDBE instruction | External process |
| **Access** | SQL query | Command line |
| **Bindings** | Works from Python/JS/etc | Needs subprocess |
| **Filter** | `PRAGMA hash('table')` | `--like 'table'` |
| **Modes** | `hash_schema`, `hash_content` | `--schema-only`, `--without-schema` |

**Recommendation**: Implement PRAGMA hash first (more useful), then optionally add CLI tool that reuses the same hash computation code.

---

## Tips

1. **Start with basic PRAGMA hash** - no filtering, full database
2. **Test incrementally** - verify each phase works
3. **Reuse hash code** - same algorithm as dbhash-plan.md
4. **Match SQLite dbhash output** - for compatibility testing
5. **Consider performance** - large databases may take time; consider progress indication

Good luck!
