# dbhash Implementation Plan

## Overview

Implement a `dbhash` utility that computes SHA1 hash of database logical content, compatible with SQLite's `dbhash` tool.

## Decision: Location

**Chosen: Standalone tool in `tools/dbhash/`** (new directory)

Note: `tools/` directory doesn't exist yet - will create it. Alternative is `perf/dbhash/` following existing `perf/encryption/` pattern, but dbhash isn't really a perf tool.

Rationale:
- Clean separation from REPL functionality
- Can be built/distributed independently: `cargo build -p turso-dbhash`
- Simpler CLI (clap) vs integrating into dot-command parser
- Follows pattern of `perf/encryption/Cargo.toml` for standalone tools

Alternative considered: CLI dot-command (`.dbhash`) - rejected because dbhash is a utility for comparing databases, not interactive REPL operation.

## Architecture

```
tools/dbhash/
├── Cargo.toml
└── src/
    ├── lib.rs        # Public API: hash_database()
    ├── main.rs       # CLI entry point
    └── encoder.rs    # Value encoding (type prefix + big-endian)
```

## Implementation Steps

### Step 1: Create crate structure

1. Create `tools/dbhash/Cargo.toml`:
```toml
[package]
name = "turso-dbhash"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "turso-dbhash"
path = "src/main.rs"

[dependencies]
turso_core = { path = "../../core" }
sha1 = "0.10"
clap = { version = "4", features = ["derive"] }
hex = "0.4"

[dev-dependencies]
tempfile = { workspace = true }
```

2. Add to workspace in root `Cargo.toml`:
```toml
members = [
    # ... existing members
    "tools/dbhash",
]
```

### Step 2: Implement value encoder (`src/encoder.rs`)

Encode values with type prefix for hashing:
- `'0'` = NULL (no data)
- `'1'` + 8 bytes big-endian = INTEGER
- `'2'` + 8 bytes big-endian IEEE 754 bits = FLOAT
- `'3'` + raw bytes = TEXT
- `'4'` + raw bytes = BLOB

Key code (using `turso_core::Value` from `core/types.rs:273`):
```rust
use turso_core::Value;

pub fn encode_value(value: &Value, output: &mut Vec<u8>) {
    match value {
        Value::Null => output.push(b'0'),
        Value::Integer(v) => {
            output.push(b'1');
            output.extend_from_slice(&v.to_be_bytes());
        }
        Value::Float(v) => {
            output.push(b'2');
            output.extend_from_slice(&v.to_bits().to_be_bytes());
        }
        Value::Text(text) => {
            output.push(b'3');
            output.extend_from_slice(text.as_str().as_bytes());  // Text has .as_str()
        }
        Value::Blob(b) => {
            output.push(b'4');
            output.extend_from_slice(b);
        }
    }
}
```

### Step 3: Implement core hashing logic (`src/lib.rs`)

Key functions:
1. `hash_database(path, options) -> Result<DbHashResult>`
2. `get_table_names(conn, pattern) -> Result<Vec<String>>`
3. `hash_table_content(conn, table_name, hasher) -> Result<usize>`
4. `hash_schema(conn, pattern, hasher) -> Result<()>`

**Critical: Handle async I/O**

turso-core uses async I/O model. Must handle `StepResult::IO`:

```rust
fn step_to_completion(stmt: &mut Statement, pager: &Pager) -> Result<StepResult> {
    loop {
        match stmt.step()? {
            StepResult::IO => pager.io.step()?,  // Advance I/O
            other => return Ok(other),
        }
    }
}
```

**Table enumeration query:**
```sql
SELECT name FROM sqlite_schema
WHERE type = 'table'
  AND sql NOT LIKE 'CREATE VIRTUAL%'
  AND name NOT LIKE 'sqlite_%'
  AND name LIKE ?
ORDER BY name COLLATE nocase
```

**Schema hash query:**
```sql
SELECT type, name, tbl_name, sql FROM sqlite_schema
WHERE tbl_name LIKE ?
ORDER BY name COLLATE nocase
```

### Step 4: Implement CLI (`src/main.rs`)

```rust
use clap::Parser;

#[derive(Parser)]
#[command(name = "turso-dbhash")]
#[command(about = "Compute SHA1 hash of database content")]
struct Args {
    /// Database files to hash
    #[arg(required = true)]
    files: Vec<String>,

    /// Only hash tables matching LIKE pattern
    #[arg(long)]
    like: Option<String>,

    /// Only hash schema (no content)
    #[arg(long)]
    schema_only: bool,

    /// Only hash content (no schema)
    #[arg(long)]
    without_schema: bool,

    /// Debug: trace values to stderr
    #[arg(long)]
    debug: bool,
}
```

Output format: `<40-char-hex-sha1> <filename>`

### Step 5: Tests

**Unit tests in `src/encoder.rs`:**
- Test each value type encoding
- Test negative integers (two's complement)
- Test float special values (NaN, Inf)

**Integration tests in `src/lib.rs` or separate `tests/` dir:**
- Empty database has consistent hash
- Same content = same hash (across different DBs)
- Different content = different hash
- VACUUM preserves hash
- Page size doesn't affect hash
- `--schema-only` vs `--without-schema`
- `--like` filter works
- System tables (`sqlite_%`) excluded
- Virtual tables excluded

**Compatibility test (optional, requires SQLite dbhash):**
```rust
#[test]
#[ignore] // Requires sqlite dbhash installed
fn test_sqlite_compatibility() {
    // Create DB, hash with both, compare
}
```

## Files to Create/Modify

| File | Action |
|------|--------|
| `Cargo.toml` (root) | Add `tools/dbhash` to workspace members |
| `tools/dbhash/Cargo.toml` | Create new |
| `tools/dbhash/src/lib.rs` | Create - public API |
| `tools/dbhash/src/main.rs` | Create - CLI |
| `tools/dbhash/src/encoder.rs` | Create - value encoding |

## Estimated Size

- `encoder.rs`: ~50 lines
- `lib.rs`: ~150 lines
- `main.rs`: ~50 lines
- Tests: ~200 lines
- **Total: ~450 lines**

## Detailed API Design

### Opening a Database (`src/lib.rs`)

Based on `core/lib.rs:414-503`, the database opening pattern is:

```rust
use std::sync::Arc;
use turso_core::{Database, Connection, PlatformIO, OpenFlags};

pub fn open_database(path: &str) -> Result<(Arc<Database>, Arc<Connection>)> {
    let io = Arc::new(PlatformIO::new()?);

    // Use read-write to allow hot journal recovery (same as SQLite dbhash)
    let flags = OpenFlags::default();  // or SQLITE_OPEN_READWRITE
    let opts = turso_core::DatabaseOpts::default();

    let db = Database::open_file_with_flags(
        io.clone(),
        path,
        flags,
        opts,
        None,  // no encryption
    )?;

    let conn = db.connect()?;
    Ok((db, conn))
}
```

### Full lib.rs Structure

```rust
// src/lib.rs

mod encoder;

use std::sync::Arc;
use sha1::{Sha1, Digest};
use turso_core::{
    Database, Connection, Statement, Value, PlatformIO,
    OpenFlags, DatabaseOpts, LimboError, vdbe::StepResult,
};

pub use encoder::encode_value;

/// Options for database hashing
#[derive(Debug, Clone, Default)]
pub struct DbHashOptions {
    pub table_filter: Option<String>,
    pub schema_only: bool,
    pub without_schema: bool,
    pub debug_trace: bool,
}

/// Result of hashing
pub struct DbHashResult {
    pub hash: String,          // 40-char hex
    pub tables_hashed: usize,
    pub rows_hashed: usize,
}

/// Main entry point - compute hash of database content
pub fn hash_database(path: &str, options: &DbHashOptions) -> Result<DbHashResult, LimboError> {
    let io = Arc::new(PlatformIO::new()?);
    let db = Database::open_file(io.clone(), path)?;
    let conn = db.connect()?;

    let mut hasher = Sha1::new();
    let mut tables_hashed = 0;
    let mut rows_hashed = 0;

    let filter = options.table_filter.as_deref().unwrap_or("%");

    // 1. Hash table content (unless schema_only)
    if !options.schema_only {
        let tables = get_table_names(&conn, &io, filter)?;
        for table in &tables {
            tables_hashed += 1;
            rows_hashed += hash_table(&conn, &io, table, &mut hasher, options.debug_trace)?;
        }
    }

    // 2. Hash schema (unless without_schema)
    if !options.without_schema {
        hash_schema(&conn, &io, filter, &mut hasher, options.debug_trace)?;
    }

    let hash = hex::encode(hasher.finalize());

    Ok(DbHashResult { hash, tables_hashed, rows_hashed })
}

/// Get list of user tables (excludes sqlite_%, virtual tables)
fn get_table_names(
    conn: &Arc<Connection>,
    io: &Arc<dyn turso_core::IO>,
    like_pattern: &str,
) -> Result<Vec<String>, LimboError> {
    let sql = format!(
        r#"SELECT name FROM sqlite_schema
           WHERE type = 'table'
             AND sql NOT LIKE 'CREATE VIRTUAL%'
             AND name NOT LIKE 'sqlite_%'
             AND name LIKE '{}'
           ORDER BY name COLLATE nocase"#,
        escape_sql_string(like_pattern)
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut names = Vec::new();

    loop {
        match stmt.step()? {
            StepResult::Row => {
                if let Some(row) = stmt.row() {
                    if let Value::Text(t) = row.get_value(0) {
                        names.push(t.as_str().to_string());
                    }
                }
            }
            StepResult::IO => io.step()?,
            StepResult::Done => break,
            StepResult::Busy | StepResult::Interrupt => {
                return Err(LimboError::Busy);
            }
        }
    }

    Ok(names)
}

/// Hash all rows in a table
fn hash_table(
    conn: &Arc<Connection>,
    io: &Arc<dyn turso_core::IO>,
    table_name: &str,
    hasher: &mut Sha1,
    debug: bool,
) -> Result<usize, LimboError> {
    // Quote table name for safety
    let sql = format!("SELECT * FROM \"{}\"", table_name.replace('"', "\"\""));
    let mut stmt = conn.prepare(&sql)?;
    let mut row_count = 0;
    let mut buf = Vec::new();

    loop {
        match stmt.step()? {
            StepResult::Row => {
                row_count += 1;
                if let Some(row) = stmt.row() {
                    for value in row.get_values() {
                        buf.clear();
                        encode_value(value, &mut buf);
                        if debug {
                            eprintln!("{:?}", value);
                        }
                        hasher.update(&buf);
                    }
                }
            }
            StepResult::IO => io.step()?,
            StepResult::Done => break,
            StepResult::Busy | StepResult::Interrupt => {
                return Err(LimboError::Busy);
            }
        }
    }

    Ok(row_count)
}

/// Hash schema entries
fn hash_schema(
    conn: &Arc<Connection>,
    io: &Arc<dyn turso_core::IO>,
    like_pattern: &str,
    hasher: &mut Sha1,
    debug: bool,
) -> Result<(), LimboError> {
    let sql = format!(
        r#"SELECT type, name, tbl_name, sql FROM sqlite_schema
           WHERE tbl_name LIKE '{}'
           ORDER BY name COLLATE nocase"#,
        escape_sql_string(like_pattern)
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut buf = Vec::new();

    loop {
        match stmt.step()? {
            StepResult::Row => {
                if let Some(row) = stmt.row() {
                    for value in row.get_values() {
                        buf.clear();
                        encode_value(value, &mut buf);
                        if debug {
                            eprintln!("{:?}", value);
                        }
                        hasher.update(&buf);
                    }
                }
            }
            StepResult::IO => io.step()?,
            StepResult::Done => break,
            StepResult::Busy | StepResult::Interrupt => {
                return Err(LimboError::Busy);
            }
        }
    }

    Ok(())
}

fn escape_sql_string(s: &str) -> String {
    s.replace('\'', "''")
}
```

### Full main.rs Structure

```rust
// src/main.rs

use clap::Parser;
use turso_dbhash::{hash_database, DbHashOptions};

#[derive(Parser)]
#[command(name = "turso-dbhash")]
#[command(version, about = "Compute SHA1 hash of SQLite database content")]
struct Args {
    /// Database files to hash
    #[arg(required = true)]
    files: Vec<String>,

    /// Only hash tables matching SQL LIKE pattern
    #[arg(long, value_name = "PATTERN")]
    like: Option<String>,

    /// Only hash schema (no table content)
    #[arg(long)]
    schema_only: bool,

    /// Only hash content (no schema)
    #[arg(long)]
    without_schema: bool,

    /// Trace hash inputs to stderr
    #[arg(long)]
    debug: bool,
}

fn main() {
    let args = Args::parse();

    if args.schema_only && args.without_schema {
        eprintln!("Error: --schema-only and --without-schema are mutually exclusive");
        std::process::exit(1);
    }

    let options = DbHashOptions {
        table_filter: args.like,
        schema_only: args.schema_only,
        without_schema: args.without_schema,
        debug_trace: args.debug,
    };

    let mut exit_code = 0;

    for file in &args.files {
        match hash_database(file, &options) {
            Ok(result) => {
                println!("{} {}", result.hash, file);
            }
            Err(e) => {
                eprintln!("Error hashing '{}': {}", file, e);
                exit_code = 1;
            }
        }
    }

    std::process::exit(exit_code);
}
```

## Edge Cases and Gotchas

### 1. Async I/O Handling
turso-core uses async I/O internally. Every `stmt.step()` can return `StepResult::IO` which means "I/O is pending". Must call `io.step()` to advance.

### 2. Table Name Escaping
Table names can contain special characters including double quotes. Must escape with `""`:
```rust
let sql = format!("SELECT * FROM \"{}\"", name.replace('"', "\"\""));
```

### 3. LIKE Pattern Escaping
The `--like` filter is a SQL LIKE pattern. Single quotes must be escaped:
```rust
let safe = pattern.replace('\'', "''");
```

### 4. Row Ordering
SQLite returns rows in PRIMARY KEY order by default (for tables with explicit INTEGER PRIMARY KEY). This is important for deterministic hashing. The `ORDER BY name COLLATE nocase` in table enumeration ensures consistent table ordering.

### 5. Float NaN Handling
IEEE 754 NaN has multiple bit representations. Need to verify turso-core normalizes NaN values. SQLite uses a specific NaN representation for consistency.

### 6. Empty Tables
Empty tables contribute nothing to the content hash but their schema is still hashed. This is correct behavior.

### 7. WITHOUT ROWID Tables
Tables with `WITHOUT ROWID` have different internal structure but should hash identically via `SELECT *`.

### 8. Generated Columns
Generated columns are included in `SELECT *` results. This matches SQLite behavior.

### 9. Database Locking
Opening read-write allows hot journal recovery. However, if another process has an exclusive lock, the open will fail. Should handle this gracefully.

### 10. Encryption Support
If database is encrypted, would need encryption key. Current plan assumes unencrypted databases. Could add `--key` option later.

## Testing Strategy Details

### Using TempDatabase (from `tests/integration/common.rs`)

```rust
use turso_macros::test;
use tests::common::TempDatabase;

#[turso_macros::test(init_sql = "CREATE TABLE t(x INT); INSERT INTO t VALUES(1);")]
fn test_basic_hash(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let path = tmp_db.path.to_str().unwrap();

    let result = turso_dbhash::hash_database(path, &Default::default())?;

    assert_eq!(result.hash.len(), 40);
    assert_eq!(result.tables_hashed, 1);
    assert_eq!(result.rows_hashed, 1);

    Ok(())
}
```

### Determinism Test

```rust
#[test]
fn test_hash_deterministic() -> anyhow::Result<()> {
    let tmp1 = TempDatabase::new("test1.db");
    let tmp2 = TempDatabase::new("test2.db");

    let sql = "CREATE TABLE t(a INT, b TEXT); INSERT INTO t VALUES(1, 'hello');";

    for tmp in [&tmp1, &tmp2] {
        let conn = tmp.connect_limbo();
        conn.execute(sql)?;
    }

    let hash1 = hash_database(tmp1.path.to_str().unwrap(), &Default::default())?;
    let hash2 = hash_database(tmp2.path.to_str().unwrap(), &Default::default())?;

    assert_eq!(hash1.hash, hash2.hash);
    Ok(())
}
```

### VACUUM Invariance Test

```rust
#[turso_macros::test(init_sql = "CREATE TABLE t(x INT); INSERT INTO t VALUES(1),(2),(3);")]
fn test_vacuum_preserves_hash(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let path = tmp_db.path.to_str().unwrap();

    let hash_before = hash_database(path, &Default::default())?.hash;

    let conn = tmp_db.connect_limbo();
    conn.execute("VACUUM")?;  // Once VACUUM is implemented
    drop(conn);

    let hash_after = hash_database(path, &Default::default())?.hash;

    assert_eq!(hash_before, hash_after, "VACUUM should not change content hash");
    Ok(())
}
```

## Open Questions (Resolved)

1. **SHA1 crate choice**: Use `sha1` crate (simple) or `ring` (more dependencies but faster)?
   - **Decision**: `sha1` crate - simpler, sufficient for this use case

2. **Error handling**: Use `anyhow` or custom error type?
   - **Decision**: `LimboError` from turso_core for library, main.rs can use anyhow

3. **Async vs sync API**: Should the public API be async?
   - **Decision**: Sync API (handles IO internally) - matches dbhash use case

4. **Location**: `tools/dbhash/` or `perf/dbhash/`?
   - **Decision**: `tools/dbhash/` (user confirmed)

## Related Files for Reference

| File | Purpose |
|------|---------|
| `core/types.rs:273` | `Value` enum definition |
| `core/lib.rs:414` | `Database::open_file` |
| `core/connection.rs:168` | `Connection::prepare` |
| `core/statement.rs:198` | `Statement::step` |
| `core/vdbe/mod.rs:192` | `StepResult` enum |
| `tests/integration/common.rs` | `TempDatabase` test helper |
| `perf/encryption/Cargo.toml` | Example standalone tool structure |
| `sqlite/tool/dbhash.c` | Original SQLite implementation (reference) |
