# dbhash: Analysis and Rust Implementation Plan

## Table of Contents

1. [Overview](#overview)
2. [What dbhash Does](#what-dbhash-does)
3. [Why Content-Based Hashing](#why-content-based-hashing)
4. [Algorithm Deep Dive](#algorithm-deep-dive)
5. [Code Walkthrough](#code-walkthrough)
6. [Rust Implementation Plan](#rust-implementation-plan)
7. [Testing Strategy](#testing-strategy)

---

## Overview

`dbhash` is a SQLite utility that computes a SHA1 hash of a database's **logical content**, independent of its physical representation. Two databases with identical data will produce the same hash even if they have different:

- Page sizes
- Text encodings (UTF-8 vs UTF-16)
- Auto-vacuum settings
- Free page counts
- Row ordering within pages

This makes dbhash useful for:
- Verifying database migrations
- Comparing databases across different platforms
- Detecting content changes after VACUUM
- Testing database replication

---

## What dbhash Does

### Input
One or more SQLite database files.

### Output
For each database, prints: `<40-char SHA1 hex> <filename>`

### Example
```bash
$ dbhash test.db
a3f2b8c9d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9 test.db

$ dbhash --schema-only test.db
b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0 test.db

$ dbhash --like 'user%' test.db
c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1 test.db
```

### Command-Line Options

| Option | Description |
|--------|-------------|
| `--help` | Show usage |
| `--debug N` | Set debug flags (bit 0 = trace hash inputs to stderr) |
| `--like PATTERN` | Only hash tables matching SQL LIKE pattern |
| `--schema-only` | Hash only the schema, not table content |
| `--without-schema` | Hash only table content, not schema |

---

## Why Content-Based Hashing

### The Problem with File Hashing

If you simply `sha1sum database.db`, you get a hash of the physical file. This hash changes when:

```
Physical changes (affect file hash, NOT content hash):
├── VACUUM (reorganizes pages)
├── Page size change
├── Encoding change (UTF-8 ↔ UTF-16)
├── Auto-vacuum mode change
├── Free pages added/removed
├── Row physical ordering within pages
└── SQLite version differences in page layout

Logical changes (affect BOTH hashes):
├── INSERT/UPDATE/DELETE rows
├── CREATE/DROP tables or indexes
├── ALTER TABLE
└── Schema modifications
```

### dbhash Solution

Hash the **logical content** by:
1. Reading data through SQL queries (abstracts physical layout)
2. Normalizing value representation (big-endian integers, etc.)
3. Ordering by table name, then by PRIMARY KEY within tables
4. Prefixing each value with its type

---

## Algorithm Deep Dive

### High-Level Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                      dbhash Algorithm                           │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  1. Initialize SHA1 context                                     │
│                                                                 │
│  2. Hash table content (unless --schema-only):                  │
│     a. Query: SELECT name FROM sqlite_schema                    │
│               WHERE type='table'                                │
│               AND sql NOT LIKE 'CREATE VIRTUAL%'                │
│               AND name NOT LIKE 'sqlite_%'                      │
│               AND name LIKE ?                                   │
│               ORDER BY name COLLATE nocase                      │
│                                                                 │
│     b. For each table:                                          │
│        Query: SELECT * FROM "tablename"                         │
│        (Rows returned in PRIMARY KEY order by default)          │
│                                                                 │
│     c. For each cell in each row:                               │
│        Hash type prefix + normalized value                      │
│                                                                 │
│  3. Hash schema (unless --without-schema):                      │
│     Query: SELECT type, name, tbl_name, sql                     │
│            FROM sqlite_schema                                   │
│            WHERE tbl_name LIKE ?                                │
│            ORDER BY name COLLATE nocase                         │
│                                                                 │
│  4. Finalize SHA1 and output                                    │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### Value Encoding

Each cell value is encoded with a **type prefix** followed by **normalized data**:

```
┌──────────────┬────────┬─────────────────────────────────────────┐
│ SQLite Type  │ Prefix │ Data Encoding                           │
├──────────────┼────────┼─────────────────────────────────────────┤
│ NULL         │ "0"    │ (nothing)                               │
├──────────────┼────────┼─────────────────────────────────────────┤
│ INTEGER      │ "1"    │ 8 bytes, big-endian                     │
├──────────────┼────────┼─────────────────────────────────────────┤
│ FLOAT        │ "2"    │ 8 bytes, big-endian (IEEE 754 bits)     │
├──────────────┼────────┼─────────────────────────────────────────┤
│ TEXT         │ "3"    │ Raw UTF-8 bytes (no length prefix)      │
├──────────────┼────────┼─────────────────────────────────────────┤
│ BLOB         │ "4"    │ Raw bytes (no length prefix)            │
└──────────────┴────────┴─────────────────────────────────────────┘
```

**Why type prefixes?**
- Distinguishes `NULL` from empty string from empty blob
- Distinguishes integer `0` from float `0.0` from text `"0"`
- Without prefixes: `hash("") == hash(empty_blob)` which is wrong

**Why big-endian for integers/floats?**
- Consistent across little-endian and big-endian machines
- Same hash regardless of host architecture

### What's Excluded from Hash

1. **System tables** (`sqlite_%`):
   - `sqlite_schema` (hashed separately via schema query)
   - `sqlite_sequence` (AUTOINCREMENT state)
   - `sqlite_stat1`, `sqlite_stat4` (ANALYZE statistics)

2. **Virtual tables**:
   - `sql LIKE 'CREATE VIRTUAL%'` filtered out
   - Virtual tables have no stored content

3. **Free space and physical layout**:
   - Page size, encoding, auto_vacuum don't affect hash
   - Row ordering within pages doesn't matter (PRIMARY KEY order used)

---

## Code Walkthrough

### Global State

```c
// dbhash.c:41-46
struct GlobalVars {
  const char *zArgv0;       // Program name for error messages
  unsigned fDebug;          // Debug flags (bit 0 = trace)
  sqlite3 *db;              // Current database connection
  SHA1Context cx;           // SHA1 hash state
} g;
```

### SHA1 Implementation

The file includes a complete SHA1 implementation (lines 30-218). Key functions:

```c
// Initialize SHA1 state with standard constants
static void hash_init(void){
  g.cx.state[0] = 0x67452301;
  g.cx.state[1] = 0xEFCDAB89;
  g.cx.state[2] = 0x98BADCFE;
  g.cx.state[3] = 0x10325476;
  g.cx.state[4] = 0xC3D2E1F0;
  g.cx.count[0] = g.cx.count[1] = 0;
}

// Add data to hash
static void hash_step(const unsigned char *data, unsigned int len);

// Finalize and print hash
static void hash_finish(const char *zName);
```

### Core Hashing Logic

```c
// dbhash.c:281-360 - Hash one query's results
static void hash_one_query(const char *zFormat, ...){
  // Prepare query
  va_start(ap, zFormat);
  pStmt = db_vprepare(zFormat, ap);
  va_end(ap);
  nCol = sqlite3_column_count(pStmt);

  // For each row
  while( SQLITE_ROW==sqlite3_step(pStmt) ){
    // For each column
    for(i=0; i<nCol; i++){
      switch( sqlite3_column_type(pStmt,i) ){

        case SQLITE_NULL: {
          hash_step((const unsigned char*)"0", 1);  // Just prefix
          break;
        }

        case SQLITE_INTEGER: {
          sqlite3_int64 v = sqlite3_column_int64(pStmt,i);
          unsigned char x[8];
          // Convert to big-endian
          for(j=7; j>=0; j--){
            x[j] = u & 0xff;
            u >>= 8;
          }
          hash_step((const unsigned char*)"1", 1);  // Prefix
          hash_step(x, 8);                          // Value
          break;
        }

        case SQLITE_FLOAT: {
          double r = sqlite3_column_double(pStmt,i);
          unsigned char x[8];
          memcpy(&u, &r, 8);  // Get IEEE 754 bits
          // Convert to big-endian
          for(j=7; j>=0; j--){
            x[j] = u & 0xff;
            u >>= 8;
          }
          hash_step((const unsigned char*)"2", 1);
          hash_step(x, 8);
          break;
        }

        case SQLITE_TEXT: {
          int n = sqlite3_column_bytes(pStmt, i);
          const unsigned char *z = sqlite3_column_text(pStmt, i);
          hash_step((const unsigned char*)"3", 1);
          hash_step(z, n);
          break;
        }

        case SQLITE_BLOB: {
          int n = sqlite3_column_bytes(pStmt, i);
          const unsigned char *z = sqlite3_column_blob(pStmt, i);
          hash_step((const unsigned char*)"4", 1);
          hash_step(z, n);
          break;
        }
      }
    }
  }
  sqlite3_finalize(pStmt);
}
```

### Main Logic

```c
// dbhash.c:432-489 - Main loop
for(i=1; i<=nFile; i++){
  zDb = argv[i];

  // Open database (read-write to allow hot journal recovery)
  rc = sqlite3_open_v2(zDb, &g.db,
      SQLITE_OPEN_READWRITE | SQLITE_OPEN_URI, 0);

  // Verify it's a valid database
  rc = sqlite3_exec(g.db, "SELECT * FROM sqlite_schema", 0, 0, &zErrMsg);

  // Initialize hash
  hash_init();

  // Hash table content (unless --schema-only)
  if( !omitContent ){
    pStmt = db_prepare(
      "SELECT name FROM sqlite_schema\n"
      " WHERE type='table' AND sql NOT LIKE 'CREATE VIRTUAL%%'\n"
      "   AND name NOT LIKE 'sqlite_%%'\n"
      "   AND name LIKE '%q'\n"
      " ORDER BY name COLLATE nocase;\n",
      zLike
    );
    while( SQLITE_ROW==sqlite3_step(pStmt) ){
      // Hash: SELECT * FROM "tablename"
      // Rows come in PRIMARY KEY order (SQLite default behavior)
      hash_one_query("SELECT * FROM \"%w\"",
                     sqlite3_column_text(pStmt,0));
    }
    sqlite3_finalize(pStmt);
  }

  // Hash schema (unless --without-schema)
  if( !omitSchema ){
    hash_one_query(
       "SELECT type, name, tbl_name, sql FROM sqlite_schema\n"
       " WHERE tbl_name LIKE '%q'\n"
       " ORDER BY name COLLATE nocase;\n",
       zLike
    );
  }

  // Output hash
  hash_finish(zDb);
  sqlite3_close(g.db);
}
```

### Table Selection Query Breakdown

```sql
SELECT name FROM sqlite_schema
WHERE type='table'                      -- Only tables (not indexes, views, triggers)
  AND sql NOT LIKE 'CREATE VIRTUAL%'    -- Exclude virtual tables
  AND name NOT LIKE 'sqlite_%'          -- Exclude system tables
  AND name LIKE ?                       -- User filter (default '%')
ORDER BY name COLLATE nocase;           -- Deterministic order
```

### Why Read-Write Open?

```c
// dbhash.c:433-436
static const int openFlags =
   SQLITE_OPEN_READWRITE |     // Read/write so hot journals can recover
   SQLITE_OPEN_URI
;
```

If the database has a hot journal (crash recovery needed), read-only open would fail. Read-write allows SQLite to automatically recover the journal.

---

## Rust Implementation Plan

### Crate Structure

```
turso-dbhash/
├── Cargo.toml
├── src/
│   ├── main.rs           # CLI entry point
│   ├── lib.rs            # Library API
│   ├── hasher.rs         # SHA1 hashing abstraction
│   └── encoder.rs        # Value encoding (type prefix + normalization)
```

### Dependencies

```toml
[dependencies]
turso-core = { path = "../core" }   # For database access
sha1 = "0.10"                       # SHA1 implementation (or use ring/sha1_smol)
clap = { version = "4", features = ["derive"] }  # CLI parsing

[dev-dependencies]
tempfile = "3"                      # For tests
```

### Core Types

```rust
// src/lib.rs

use sha1::{Sha1, Digest};
use turso_core::{Connection, Value};

/// Options for computing database hash
#[derive(Debug, Clone, Default)]
pub struct DbHashOptions {
    /// Only hash tables matching this SQL LIKE pattern
    pub table_filter: Option<String>,
    /// If true, only hash schema (no table content)
    pub schema_only: bool,
    /// If true, only hash content (no schema)
    pub without_schema: bool,
    /// If true, print each value to stderr as it's hashed
    pub debug_trace: bool,
}

/// Result of hashing a database
pub struct DbHashResult {
    /// 40-character lowercase hex SHA1
    pub hash: String,
    /// Number of tables hashed
    pub tables_hashed: usize,
    /// Number of rows hashed
    pub rows_hashed: usize,
}

/// Compute content hash of a database
pub fn hash_database(
    path: &str,
    options: &DbHashOptions,
) -> Result<DbHashResult, Error>;
```

### Value Encoder

```rust
// src/encoder.rs

use turso_core::Value;

/// Encode a value for hashing with type prefix
pub fn encode_value(value: &Value, output: &mut Vec<u8>) {
    match value {
        Value::Null => {
            output.push(b'0');
        }
        Value::Integer(v) => {
            output.push(b'1');
            output.extend_from_slice(&v.to_be_bytes());
        }
        Value::Float(v) => {
            output.push(b'2');
            output.extend_from_slice(&v.to_bits().to_be_bytes());
        }
        Value::Text(s) => {
            output.push(b'3');
            output.extend_from_slice(s.as_bytes());
        }
        Value::Blob(b) => {
            output.push(b'4');
            output.extend_from_slice(b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_null_encoding() {
        let mut buf = Vec::new();
        encode_value(&Value::Null, &mut buf);
        assert_eq!(buf, vec![b'0']);
    }

    #[test]
    fn test_integer_encoding() {
        let mut buf = Vec::new();
        encode_value(&Value::Integer(0x0102030405060708), &mut buf);
        assert_eq!(buf, vec![b'1', 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);
    }

    #[test]
    fn test_negative_integer() {
        let mut buf = Vec::new();
        encode_value(&Value::Integer(-1), &mut buf);
        // -1 as i64 = 0xFFFFFFFFFFFFFFFF
        assert_eq!(buf, vec![b'1', 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn test_float_encoding() {
        let mut buf = Vec::new();
        encode_value(&Value::Float(1.0), &mut buf);
        // 1.0 as IEEE 754 = 0x3FF0000000000000
        assert_eq!(buf[0], b'2');
        assert_eq!(buf[1..], 1.0_f64.to_bits().to_be_bytes());
    }

    #[test]
    fn test_text_encoding() {
        let mut buf = Vec::new();
        encode_value(&Value::Text("hello".into()), &mut buf);
        assert_eq!(buf, vec![b'3', b'h', b'e', b'l', b'l', b'o']);
    }

    #[test]
    fn test_blob_encoding() {
        let mut buf = Vec::new();
        encode_value(&Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]), &mut buf);
        assert_eq!(buf, vec![b'4', 0xDE, 0xAD, 0xBE, 0xEF]);
    }
}
```

### Main Hash Function

```rust
// src/lib.rs

use sha1::{Sha1, Digest};

pub fn hash_database(
    path: &str,
    options: &DbHashOptions,
) -> Result<DbHashResult, Error> {
    // Open database
    let conn = Connection::open(path)?;

    let mut hasher = Sha1::new();
    let mut tables_hashed = 0;
    let mut rows_hashed = 0;

    let table_filter = options.table_filter.as_deref().unwrap_or("%");

    // Hash table content (unless schema_only)
    if !options.schema_only {
        let table_names = get_table_names(&conn, table_filter)?;

        for table_name in &table_names {
            tables_hashed += 1;
            rows_hashed += hash_table_content(
                &conn,
                table_name,
                &mut hasher,
                options.debug_trace,
            )?;
        }
    }

    // Hash schema (unless without_schema)
    if !options.without_schema {
        hash_schema(&conn, table_filter, &mut hasher, options.debug_trace)?;
    }

    // Finalize
    let hash_bytes = hasher.finalize();
    let hash = hex::encode(hash_bytes);

    Ok(DbHashResult {
        hash,
        tables_hashed,
        rows_hashed,
    })
}

/// Get list of tables to hash (excludes system/virtual tables)
fn get_table_names(conn: &Connection, like_pattern: &str) -> Result<Vec<String>, Error> {
    let sql = r#"
        SELECT name FROM sqlite_schema
        WHERE type = 'table'
          AND sql NOT LIKE 'CREATE VIRTUAL%'
          AND name NOT LIKE 'sqlite_%'
          AND name LIKE ?
        ORDER BY name COLLATE nocase
    "#;

    let mut stmt = conn.prepare(sql)?;
    stmt.bind(1, like_pattern)?;

    let mut names = Vec::new();
    while let StepResult::Row = stmt.step()? {
        names.push(stmt.column_text(0)?.to_string());
    }

    Ok(names)
}

/// Hash all rows in a table
fn hash_table_content(
    conn: &Connection,
    table_name: &str,
    hasher: &mut Sha1,
    debug: bool,
) -> Result<usize, Error> {
    // Quote table name to handle special characters
    let sql = format!("SELECT * FROM \"{}\"", escape_identifier(table_name));
    let mut stmt = conn.prepare(&sql)?;

    let col_count = stmt.column_count();
    let mut row_count = 0;
    let mut buf = Vec::new();

    while let StepResult::Row = stmt.step()? {
        row_count += 1;

        for i in 0..col_count {
            buf.clear();
            let value = stmt.column_value(i)?;
            encode_value(&value, &mut buf);

            if debug {
                eprintln!("{:?}", value);
            }

            hasher.update(&buf);
        }
    }

    Ok(row_count)
}

/// Hash the schema entries
fn hash_schema(
    conn: &Connection,
    like_pattern: &str,
    hasher: &mut Sha1,
    debug: bool,
) -> Result<(), Error> {
    let sql = r#"
        SELECT type, name, tbl_name, sql FROM sqlite_schema
        WHERE tbl_name LIKE ?
        ORDER BY name COLLATE nocase
    "#;

    let mut stmt = conn.prepare(sql)?;
    stmt.bind(1, like_pattern)?;

    let mut buf = Vec::new();

    while let StepResult::Row = stmt.step()? {
        for i in 0..4 {
            buf.clear();
            let value = stmt.column_value(i)?;
            encode_value(&value, &mut buf);

            if debug {
                eprintln!("{:?}", value);
            }

            hasher.update(&buf);
        }
    }

    Ok(())
}

/// Escape a SQL identifier (double any quotes)
fn escape_identifier(name: &str) -> String {
    name.replace('"', "\"\"")
}
```

### CLI

```rust
// src/main.rs

use clap::Parser;
use turso_dbhash::{hash_database, DbHashOptions};

#[derive(Parser)]
#[command(name = "turso-dbhash")]
#[command(about = "Compute SHA1 hash of SQLite database content")]
struct Args {
    /// Database files to hash
    #[arg(required = true)]
    files: Vec<String>,

    /// Only hash tables matching this LIKE pattern
    #[arg(long)]
    like: Option<String>,

    /// Only hash schema (no table content)
    #[arg(long)]
    schema_only: bool,

    /// Only hash content (no schema)
    #[arg(long)]
    without_schema: bool,

    /// Debug: trace hash inputs to stderr
    #[arg(long)]
    debug: bool,
}

fn main() {
    let args = Args::parse();

    if args.schema_only && args.without_schema {
        eprintln!("Error: cannot use both --schema-only and --without-schema");
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

---

## Testing Strategy

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn create_test_db() -> (NamedTempFile, Connection) {
        let file = NamedTempFile::new().unwrap();
        let conn = Connection::open(file.path().to_str().unwrap()).unwrap();
        (file, conn)
    }

    #[test]
    fn test_empty_database() {
        let (file, conn) = create_test_db();
        drop(conn);

        let result = hash_database(
            file.path().to_str().unwrap(),
            &DbHashOptions::default(),
        ).unwrap();

        // Empty DB should have consistent hash
        assert_eq!(result.tables_hashed, 0);
        assert_eq!(result.rows_hashed, 0);
        assert_eq!(result.hash.len(), 40);
    }

    #[test]
    fn test_same_content_same_hash() {
        // Create two databases with same content
        let (file1, conn1) = create_test_db();
        let (file2, conn2) = create_test_db();

        for conn in [&conn1, &conn2] {
            conn.execute("CREATE TABLE t(x INTEGER, y TEXT)").unwrap();
            conn.execute("INSERT INTO t VALUES(1, 'hello')").unwrap();
            conn.execute("INSERT INTO t VALUES(2, 'world')").unwrap();
        }
        drop(conn1);
        drop(conn2);

        let hash1 = hash_database(file1.path().to_str().unwrap(), &Default::default()).unwrap();
        let hash2 = hash_database(file2.path().to_str().unwrap(), &Default::default()).unwrap();

        assert_eq!(hash1.hash, hash2.hash);
    }

    #[test]
    fn test_different_content_different_hash() {
        let (file1, conn1) = create_test_db();
        let (file2, conn2) = create_test_db();

        conn1.execute("CREATE TABLE t(x INTEGER)").unwrap();
        conn1.execute("INSERT INTO t VALUES(1)").unwrap();

        conn2.execute("CREATE TABLE t(x INTEGER)").unwrap();
        conn2.execute("INSERT INTO t VALUES(2)").unwrap();  // Different value

        drop(conn1);
        drop(conn2);

        let hash1 = hash_database(file1.path().to_str().unwrap(), &Default::default()).unwrap();
        let hash2 = hash_database(file2.path().to_str().unwrap(), &Default::default()).unwrap();

        assert_ne!(hash1.hash, hash2.hash);
    }

    #[test]
    fn test_vacuum_preserves_hash() {
        let (file, conn) = create_test_db();

        conn.execute("CREATE TABLE t(x INTEGER, y TEXT)").unwrap();
        conn.execute("INSERT INTO t VALUES(1, 'hello')").unwrap();
        conn.execute("INSERT INTO t VALUES(2, 'world')").unwrap();

        let hash_before = hash_database(file.path().to_str().unwrap(), &Default::default()).unwrap();

        // VACUUM changes physical layout but not content
        conn.execute("VACUUM").unwrap();
        drop(conn);

        let hash_after = hash_database(file.path().to_str().unwrap(), &Default::default()).unwrap();

        assert_eq!(hash_before.hash, hash_after.hash);
    }

    #[test]
    fn test_page_size_independent() {
        // Create same content with different page sizes
        let (file1, conn1) = create_test_db();
        let (file2, conn2) = create_test_db();

        conn1.execute("PRAGMA page_size = 1024").unwrap();
        conn2.execute("PRAGMA page_size = 4096").unwrap();

        for conn in [&conn1, &conn2] {
            conn.execute("CREATE TABLE t(x INTEGER)").unwrap();
            conn.execute("INSERT INTO t VALUES(42)").unwrap();
        }
        drop(conn1);
        drop(conn2);

        let hash1 = hash_database(file1.path().to_str().unwrap(), &Default::default()).unwrap();
        let hash2 = hash_database(file2.path().to_str().unwrap(), &Default::default()).unwrap();

        assert_eq!(hash1.hash, hash2.hash);
    }

    #[test]
    fn test_schema_only() {
        let (file, conn) = create_test_db();

        conn.execute("CREATE TABLE t(x INTEGER)").unwrap();
        conn.execute("INSERT INTO t VALUES(1)").unwrap();
        drop(conn);

        let full = hash_database(file.path().to_str().unwrap(), &Default::default()).unwrap();
        let schema_only = hash_database(
            file.path().to_str().unwrap(),
            &DbHashOptions { schema_only: true, ..Default::default() },
        ).unwrap();

        assert_ne!(full.hash, schema_only.hash);
        assert_eq!(schema_only.tables_hashed, 0);
    }

    #[test]
    fn test_without_schema() {
        let (file, conn) = create_test_db();

        conn.execute("CREATE TABLE t(x INTEGER)").unwrap();
        conn.execute("INSERT INTO t VALUES(1)").unwrap();
        drop(conn);

        let full = hash_database(file.path().to_str().unwrap(), &Default::default()).unwrap();
        let without_schema = hash_database(
            file.path().to_str().unwrap(),
            &DbHashOptions { without_schema: true, ..Default::default() },
        ).unwrap();

        assert_ne!(full.hash, without_schema.hash);
    }

    #[test]
    fn test_like_filter() {
        let (file, conn) = create_test_db();

        conn.execute("CREATE TABLE users(id INTEGER)").unwrap();
        conn.execute("CREATE TABLE orders(id INTEGER)").unwrap();
        conn.execute("INSERT INTO users VALUES(1)").unwrap();
        conn.execute("INSERT INTO orders VALUES(2)").unwrap();
        drop(conn);

        let all = hash_database(file.path().to_str().unwrap(), &Default::default()).unwrap();
        let users_only = hash_database(
            file.path().to_str().unwrap(),
            &DbHashOptions { table_filter: Some("user%".into()), ..Default::default() },
        ).unwrap();

        assert_ne!(all.hash, users_only.hash);
        assert_eq!(users_only.tables_hashed, 1);
    }

    #[test]
    fn test_excludes_sqlite_tables() {
        let (file, conn) = create_test_db();

        // Create table with AUTOINCREMENT (creates sqlite_sequence)
        conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY AUTOINCREMENT)").unwrap();
        conn.execute("INSERT INTO t(id) VALUES(NULL)").unwrap();
        drop(conn);

        let result = hash_database(file.path().to_str().unwrap(), &Default::default()).unwrap();

        // Should only hash 't', not 'sqlite_sequence'
        assert_eq!(result.tables_hashed, 1);
    }

    #[test]
    fn test_type_prefixes_matter() {
        // NULL, integer 0, float 0.0, text "0", blob x'00' should all hash differently
        let values = [
            ("NULL", "SELECT NULL"),
            ("int_zero", "SELECT 0"),
            ("float_zero", "SELECT 0.0"),
            ("text_zero", "SELECT '0'"),
            ("blob_zero", "SELECT x'00'"),
        ];

        let mut hashes = Vec::new();

        for (name, select) in values {
            let (file, conn) = create_test_db();
            conn.execute(&format!("CREATE TABLE t AS {}", select)).unwrap();
            drop(conn);

            let result = hash_database(
                file.path().to_str().unwrap(),
                &DbHashOptions { without_schema: true, ..Default::default() },
            ).unwrap();

            hashes.push((name, result.hash));
        }

        // All hashes should be unique
        for i in 0..hashes.len() {
            for j in (i+1)..hashes.len() {
                assert_ne!(
                    hashes[i].1, hashes[j].1,
                    "{} and {} should have different hashes",
                    hashes[i].0, hashes[j].0
                );
            }
        }
    }
}
```

### Compatibility Tests

```rust
#[test]
fn test_compatibility_with_sqlite_dbhash() {
    // Create a database, hash with both implementations, compare
    let (file, conn) = create_test_db();

    conn.execute("CREATE TABLE t(a INT, b TEXT, c REAL, d BLOB)").unwrap();
    conn.execute("INSERT INTO t VALUES(1, 'hello', 3.14, x'DEADBEEF')").unwrap();
    conn.execute("INSERT INTO t VALUES(NULL, NULL, NULL, NULL)").unwrap();
    drop(conn);

    // Run SQLite's dbhash
    let output = std::process::Command::new("dbhash")
        .arg(file.path())
        .output()
        .expect("sqlite dbhash not found - install sqlite3-tools");

    let sqlite_hash = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();

    // Run our implementation
    let our_result = hash_database(file.path().to_str().unwrap(), &Default::default()).unwrap();

    assert_eq!(our_result.hash, sqlite_hash);
}
```

---

## Summary

### Key Points

1. **Content-based hashing**: Hash logical content, not physical layout
2. **Type prefixes**: Distinguish NULL/int/float/text/blob
3. **Big-endian normalization**: Platform-independent integer/float encoding
4. **Deterministic ordering**: Tables by name, rows by PRIMARY KEY
5. **Exclusions**: System tables, virtual tables, statistics tables

### Rust Implementation Advantages

- Type safety for value encoding
- Better error handling with Result
- Easy SHA1 via `sha1` crate (no need to implement)
- `clap` for robust CLI parsing
- Integration with Turso's Connection type

### Estimated Effort

| Component | Complexity |
|-----------|------------|
| Value encoder | Simple |
| Table enumeration | Simple |
| Row hashing | Simple |
| CLI | Simple |
| Tests | Medium |
| SQLite compatibility verification | Medium |

Total: ~300-400 lines of Rust code, mostly tests.
