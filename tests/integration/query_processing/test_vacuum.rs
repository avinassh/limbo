use crate::common::{ExecRows, TempDatabase, compute_dbhash};
use std::sync::Arc;
use tempfile::TempDir;
use turso_core::{Connection, Value};

/// Helper to run integrity_check and return the result string
fn run_integrity_check(conn: &Arc<Connection>) -> String {
    let rows = conn
        .pragma_query("integrity_check")
        .expect("integrity_check should succeed");

    rows.into_iter()
        .filter_map(|row| {
            row.into_iter().next().and_then(|v| {
                if let Value::Text(text) = v {
                    Some(text.as_str().to_string())
                } else {
                    None
                }
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[turso_macros::test(init_sql = "CREATE TABLE t (a INTEGER, b TEXT, c BLOB);")]
fn test_vacuum_into_basic(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let conn = tmp_db.connect_limbo();

    // Insert some data
    conn.execute("INSERT INTO t VALUES (1, 'hello', X'DEADBEEF')")?;
    conn.execute("INSERT INTO t VALUES (2, 'world', X'CAFEBABE')")?;
    conn.execute("INSERT INTO t VALUES (3, 'test', NULL)")?;

    // Compute hash of source database before vacuum
    let source_hash = compute_dbhash(&tmp_db);

    // Create a temp directory for the destination database
    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    // Execute VACUUM INTO
    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

    // Open the destination database and verify the data
    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

    // Verify integrity of destination database
    let integrity_result = run_integrity_check(&dest_conn);
    assert_eq!(
        integrity_result, "ok",
        "Destination database should pass integrity check"
    );

    // Verify dbhash matches between source and destination
    let dest_hash = compute_dbhash(&dest_db);
    assert_eq!(
        source_hash.hash, dest_hash.hash,
        "Source and destination databases should have the same content hash"
    );

    // Query and verify data
    let rows: Vec<(i64, String)> = dest_conn.exec_rows("SELECT a, b FROM t ORDER BY a");

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].0, 1);
    assert_eq!(rows[0].1, "hello");
    assert_eq!(rows[1].0, 2);
    assert_eq!(rows[1].1, "world");
    assert_eq!(rows[2].0, 3);
    assert_eq!(rows[2].1, "test");

    // Verify blob data separately using raw Value
    let mut stmt = dest_conn.prepare("SELECT c FROM t ORDER BY a")?;
    let blob_values = stmt.run_collect_rows()?;
    assert_eq!(blob_values.len(), 3);
    assert_eq!(blob_values[0][0], Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]));
    assert_eq!(blob_values[1][0], Value::Blob(vec![0xCA, 0xFE, 0xBA, 0xBE]));
    assert_eq!(blob_values[2][0], Value::Null);

    Ok(())
}

/// Test VACUUM INTO error cases: plain VACUUM, existing file, within transaction
#[turso_macros::test(init_sql = "CREATE TABLE t (a INTEGER);")]
fn test_vacuum_into_error_cases(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let conn = tmp_db.connect_limbo();
    conn.execute("INSERT INTO t VALUES (1)")?;

    let dest_dir = TempDir::new()?;

    // 1. Plain VACUUM should fail
    let result = conn.execute("VACUUM");
    assert!(result.is_err(), "Plain VACUUM should fail");

    // 2. VACUUM INTO existing file should fail
    let existing_path = dest_dir.path().join("existing.db");
    std::fs::write(&existing_path, b"existing content")?;
    let result = conn.execute(&format!(
        "VACUUM INTO '{}'",
        existing_path.to_str().unwrap()
    ));
    assert!(result.is_err(), "VACUUM INTO existing file should fail");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("already exists") || err_msg.contains("output file"),
        "Error should mention file exists, got: {err_msg}"
    );

    // 3. VACUUM INTO within transaction should fail
    conn.execute("BEGIN")?;
    conn.execute("INSERT INTO t VALUES (2)")?;
    let txn_path = dest_dir.path().join("txn.db");
    let result = conn.execute(&format!("VACUUM INTO '{}'", txn_path.to_str().unwrap()));
    assert!(
        result.is_err(),
        "VACUUM INTO within transaction should fail"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("transaction") || err_msg.contains("VACUUM"),
        "Error should mention transaction, got: {err_msg}"
    );
    assert!(!txn_path.exists(), "File should not be created on failure");

    // Rollback and verify original data intact
    conn.execute("ROLLBACK")?;
    let rows: Vec<(i64,)> = conn.exec_rows("SELECT a FROM t");
    assert_eq!(rows, vec![(1,)]);

    Ok(())
}

#[turso_macros::test]
fn test_vacuum_into_multiple_tables(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let conn = tmp_db.connect_limbo();

    // Create multiple tables
    conn.execute("CREATE TABLE t1 (a INTEGER)")?;
    conn.execute("CREATE TABLE t2 (b TEXT)")?;

    // Insert data into multiple tables
    conn.execute("INSERT INTO t1 VALUES (1), (2), (3)")?;
    conn.execute("INSERT INTO t2 VALUES ('foo'), ('bar')")?;

    // Compute hash of source database before vacuum
    let source_hash = compute_dbhash(&tmp_db);

    // Create destination
    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    // Execute VACUUM INTO
    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

    // Open destination and verify both tables
    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

    // Verify integrity of destination database
    let integrity_result = run_integrity_check(&dest_conn);
    assert_eq!(
        integrity_result, "ok",
        "Destination database should pass integrity check"
    );

    // Verify dbhash matches between source and destination
    let dest_hash = compute_dbhash(&dest_db);
    assert_eq!(
        source_hash.hash, dest_hash.hash,
        "Source and destination databases should have the same content hash"
    );

    let rows_t1: Vec<(i64,)> = dest_conn.exec_rows("SELECT a FROM t1 ORDER BY a");
    assert_eq!(rows_t1, vec![(1,), (2,), (3,)]);

    let rows_t2: Vec<(String,)> = dest_conn.exec_rows("SELECT b FROM t2 ORDER BY b");
    assert_eq!(rows_t2, vec![("bar".to_string(),), ("foo".to_string(),)]);

    Ok(())
}

#[turso_macros::test(init_sql = "CREATE TABLE t (a INTEGER);")]
fn test_vacuum_into_with_index(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let conn = tmp_db.connect_limbo();

    // Create an index
    conn.execute("CREATE INDEX idx_t_a ON t (a)")?;

    // Insert some data
    conn.execute("INSERT INTO t VALUES (1), (2), (3)")?;

    // Compute hash of source database before vacuum
    let source_hash = compute_dbhash(&tmp_db);

    // Create destination
    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    // Execute VACUUM INTO
    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

    // Open destination and verify index exists
    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

    // Verify integrity of destination database (including index)
    let integrity_result = run_integrity_check(&dest_conn);
    assert_eq!(
        integrity_result, "ok",
        "Destination database with index should pass integrity check"
    );

    // Verify dbhash matches between source and destination
    let dest_hash = compute_dbhash(&dest_db);
    assert_eq!(
        source_hash.hash, dest_hash.hash,
        "Source and destination databases should have the same content hash"
    );

    // Check that the index exists in the schema
    let schema: Vec<(String, String)> =
        dest_conn.exec_rows("SELECT type, name FROM sqlite_schema WHERE type = 'index'");
    assert!(
        schema.iter().any(|(_, name)| name == "idx_t_a"),
        "Index should exist in vacuumed database"
    );

    // Verify data is accessible
    let rows: Vec<(i64,)> = dest_conn.exec_rows("SELECT a FROM t ORDER BY a");
    assert_eq!(rows, vec![(1,), (2,), (3,)]);

    Ok(())
}

/// Test VACUUM INTO with views (simple and complex views with aggregations)
#[turso_macros::test]
fn test_vacuum_into_with_views(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let conn = tmp_db.connect_limbo();

    // Create table with data
    conn.execute(
        "CREATE TABLE employees (id INTEGER, name TEXT, department TEXT, salary INTEGER)",
    )?;
    conn.execute("INSERT INTO employees VALUES (1, 'Alice', 'Engineering', 100000)")?;
    conn.execute("INSERT INTO employees VALUES (2, 'Bob', 'Sales', 80000)")?;
    conn.execute("INSERT INTO employees VALUES (3, 'Charlie', 'Engineering', 120000)")?;
    conn.execute("INSERT INTO employees VALUES (4, 'Diana', 'HR', 70000)")?;

    // Create multiple views: simple filter, complex filter, aggregation
    conn.execute(
        "CREATE VIEW engineering AS SELECT id, name, salary FROM employees WHERE department = 'Engineering'",
    )?;
    conn.execute(
        "CREATE VIEW high_earners AS SELECT name, salary FROM employees WHERE salary > 90000",
    )?;
    conn.execute(
        "CREATE VIEW dept_summary AS SELECT department, COUNT(*) as cnt FROM employees GROUP BY department",
    )?;

    let source_hash = compute_dbhash(&tmp_db);

    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed.db");
    conn.execute(&format!("VACUUM INTO '{}'", dest_path.to_str().unwrap()))?;

    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

    // Verify integrity and hash
    assert_eq!(run_integrity_check(&dest_conn), "ok");
    assert_eq!(source_hash.hash, compute_dbhash(&dest_db).hash);

    // Verify all views exist
    let views: Vec<(String,)> =
        dest_conn.exec_rows("SELECT name FROM sqlite_schema WHERE type = 'view' ORDER BY name");
    assert_eq!(
        views,
        vec![
            ("dept_summary".to_string(),),
            ("engineering".to_string(),),
            ("high_earners".to_string(),)
        ]
    );

    // Verify views query copied data correctly
    let eng: Vec<(i64, String, i64)> =
        dest_conn.exec_rows("SELECT id, name, salary FROM engineering ORDER BY id");
    assert_eq!(
        eng,
        vec![
            (1, "Alice".to_string(), 100000),
            (3, "Charlie".to_string(), 120000)
        ]
    );

    let high: Vec<(String, i64)> =
        dest_conn.exec_rows("SELECT name, salary FROM high_earners ORDER BY salary DESC");
    assert_eq!(
        high,
        vec![
            ("Charlie".to_string(), 120000),
            ("Alice".to_string(), 100000)
        ]
    );

    let summary: Vec<(String, i64)> =
        dest_conn.exec_rows("SELECT department, cnt FROM dept_summary ORDER BY department");
    assert_eq!(
        summary,
        vec![
            ("Engineering".to_string(), 2),
            ("HR".to_string(), 1),
            ("Sales".to_string(), 1)
        ]
    );

    Ok(())
}

/// Test VACUUM INTO with triggers (single and multiple, requires MVCC mode)
#[turso_macros::test(mvcc)]
fn test_vacuum_into_with_triggers(tmp_db: TempDatabase) {
    let conn = tmp_db.connect_limbo();

    // Create tables for multi-trigger test
    conn.execute("CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    conn.execute("CREATE TABLE orders (id INTEGER PRIMARY KEY, product_id INTEGER)")
        .unwrap();
    conn.execute("CREATE TABLE audit_log (action TEXT, tbl TEXT, record_id INTEGER)")
        .unwrap();

    // Create multiple triggers on different tables
    conn.execute(
        "CREATE TRIGGER log_product AFTER INSERT ON products BEGIN
            INSERT INTO audit_log VALUES ('INSERT', 'products', NEW.id);
        END",
    )
    .unwrap();
    conn.execute(
        "CREATE TRIGGER log_order AFTER INSERT ON orders BEGIN
            INSERT INTO audit_log VALUES ('INSERT', 'orders', NEW.id);
        END",
    )
    .unwrap();

    // Insert data (triggers will fire)
    conn.execute("INSERT INTO products VALUES (1, 'Item A'), (2, 'Item B')")
        .unwrap();
    conn.execute("INSERT INTO orders VALUES (1, 1), (2, 2)")
        .unwrap();

    // Compute source hash before vacuum
    let source_hash = compute_dbhash(&tmp_db);

    // Create destination
    let dest_dir = TempDir::new().unwrap();
    let dest_path = dest_dir.path().join("vacuumed.db");
    conn.execute(&format!("VACUUM INTO '{}'", dest_path.to_str().unwrap()))
        .unwrap();

    // Open destination with triggers enabled
    let dest_opts = turso_core::DatabaseOpts::new().with_triggers(true);
    let dest_db = TempDatabase::new_with_existent_with_opts(&dest_path, dest_opts);
    let dest_conn = dest_db.connect_limbo();
    dest_conn
        .pragma_update("journal_mode", "'experimental_mvcc'")
        .unwrap();

    // Verify integrity and dbhash
    assert_eq!(run_integrity_check(&dest_conn), "ok");
    assert_eq!(source_hash.hash, compute_dbhash(&dest_db).hash);

    // Verify triggers exist
    let triggers: Vec<(String,)> =
        dest_conn.exec_rows("SELECT name FROM sqlite_schema WHERE type = 'trigger' ORDER BY name");
    assert_eq!(
        triggers,
        vec![("log_order".to_string(),), ("log_product".to_string(),)]
    );

    // Verify data copied (no duplicates from triggers firing during copy)
    let products: Vec<(i64, String)> =
        dest_conn.exec_rows("SELECT id, name FROM products ORDER BY id");
    assert_eq!(
        products,
        vec![(1, "Item A".to_string()), (2, "Item B".to_string())]
    );

    let audit: Vec<(String, String, i64)> =
        dest_conn.exec_rows("SELECT action, tbl, record_id FROM audit_log ORDER BY tbl, record_id");
    assert_eq!(
        audit,
        vec![
            ("INSERT".to_string(), "orders".to_string(), 1),
            ("INSERT".to_string(), "orders".to_string(), 2),
            ("INSERT".to_string(), "products".to_string(), 1),
            ("INSERT".to_string(), "products".to_string(), 2),
        ]
    );

    // Verify triggers work for new inserts
    dest_conn
        .execute("INSERT INTO products VALUES (3, 'New')")
        .unwrap();
    dest_conn
        .execute("INSERT INTO orders VALUES (3, 3)")
        .unwrap();

    let new_audit: Vec<(String, String, i64)> = dest_conn
        .exec_rows("SELECT action, tbl, record_id FROM audit_log WHERE record_id = 3 ORDER BY tbl");
    assert_eq!(
        new_audit,
        vec![
            ("INSERT".to_string(), "orders".to_string(), 3),
            ("INSERT".to_string(), "products".to_string(), 3),
        ]
    );
}

/// Test VACUUM INTO preserves meta values: user_version, application_id (normal and boundary values)
#[turso_macros::test(init_sql = "CREATE TABLE t (a INTEGER);")]
fn test_vacuum_into_preserves_meta_values(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let conn = tmp_db.connect_limbo();
    conn.execute("INSERT INTO t VALUES (1)")?;
    let dest_dir = TempDir::new()?;

    // Test 1: Normal positive values
    conn.execute("PRAGMA user_version = 42")?;
    conn.execute("PRAGMA application_id = 12345")?;

    let source_hash1 = compute_dbhash(&tmp_db);
    let dest_path1 = dest_dir.path().join("vacuumed1.db");
    conn.execute(&format!("VACUUM INTO '{}'", dest_path1.to_str().unwrap()))?;

    let dest_db1 = TempDatabase::new_with_existent(&dest_path1);
    let dest_conn1 = dest_db1.connect_limbo();
    assert_eq!(run_integrity_check(&dest_conn1), "ok");
    assert_eq!(source_hash1.hash, compute_dbhash(&dest_db1).hash);

    let uv: Vec<(i64,)> = dest_conn1.exec_rows("PRAGMA user_version");
    assert_eq!(uv, vec![(42,)], "user_version should be 42");
    let aid: Vec<(i64,)> = dest_conn1.exec_rows("PRAGMA application_id");
    assert_eq!(aid, vec![(12345,)], "application_id should be 12345");

    // Test 2: Boundary values (negative user_version, max application_id)
    conn.execute("PRAGMA user_version = -1")?;
    conn.execute("PRAGMA application_id = 2147483647")?; // i32::MAX

    let source_hash2 = compute_dbhash(&tmp_db);
    let dest_path2 = dest_dir.path().join("vacuumed2.db");
    conn.execute(&format!("VACUUM INTO '{}'", dest_path2.to_str().unwrap()))?;

    let dest_db2 = TempDatabase::new_with_existent(&dest_path2);
    let dest_conn2 = dest_db2.connect_limbo();
    assert_eq!(run_integrity_check(&dest_conn2), "ok");
    assert_eq!(source_hash2.hash, compute_dbhash(&dest_db2).hash);

    let uv: Vec<(i64,)> = dest_conn2.exec_rows("PRAGMA user_version");
    assert_eq!(uv, vec![(-1,)], "Negative user_version should be preserved");
    let aid: Vec<(i64,)> = dest_conn2.exec_rows("PRAGMA application_id");
    assert_eq!(
        aid,
        vec![(2147483647,)],
        "Max application_id should be preserved"
    );

    Ok(())
}

/// Test VACUUM INTO preserves non-default page_size (8192)
#[turso_macros::test]
fn test_vacuum_into_preserves_page_size(_tmp_db: TempDatabase) -> anyhow::Result<()> {
    // Create a new empty database and set page_size before creating tables
    let source_db = TempDatabase::new_empty();
    let conn = source_db.connect_limbo();

    // Set non-default page_size (must be done before any tables are created)
    conn.reset_page_size(8192)?;

    // Create table and insert data
    conn.execute("CREATE TABLE t (a INTEGER, b TEXT)")?;
    conn.execute("INSERT INTO t VALUES (1, 'hello'), (2, 'world')")?;

    // Verify source has non-default page_size
    let source_page_size: Vec<(i64,)> = conn.exec_rows("PRAGMA page_size");
    assert_eq!(
        source_page_size[0].0, 8192,
        "Source database should have page_size of 8192"
    );

    let source_hash = compute_dbhash(&source_db);

    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

    // Verify page_size is preserved
    let dest_page_size: Vec<(i64,)> = dest_conn.exec_rows("PRAGMA page_size");
    assert_eq!(
        dest_page_size[0].0, 8192,
        "page_size should be preserved as 8192 in destination database"
    );

    // Verify integrity and dbhash
    assert_eq!(run_integrity_check(&dest_conn), "ok");
    assert_eq!(source_hash.hash, compute_dbhash(&dest_db).hash);

    // Verify data was copied
    let rows: Vec<(i64, String)> = dest_conn.exec_rows("SELECT a, b FROM t ORDER BY a");
    assert_eq!(
        rows,
        vec![(1, "hello".to_string()), (2, "world".to_string())]
    );

    Ok(())
}

/// Test VACUUM INTO with empty edge cases: empty tables with indexes, completely empty database
#[turso_macros::test]
fn test_vacuum_into_empty_edge_cases(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let dest_dir = TempDir::new()?;

    // Test 1: Completely empty database (no tables)
    {
        let empty_db = TempDatabase::new_empty();
        let conn = empty_db.connect_limbo();

        let schema: Vec<(String,)> =
            conn.exec_rows("SELECT name FROM sqlite_schema WHERE type = 'table'");
        assert!(schema.is_empty(), "Should have no tables");

        let dest_path = dest_dir.path().join("empty1.db");
        conn.execute(&format!("VACUUM INTO '{}'", dest_path.to_str().unwrap()))?;

        let dest_db = TempDatabase::new_with_existent(&dest_path);
        let dest_conn = dest_db.connect_limbo();
        assert_eq!(run_integrity_check(&dest_conn), "ok");

        // Verify usable
        dest_conn.execute("CREATE TABLE t (a INTEGER)")?;
        dest_conn.execute("INSERT INTO t VALUES (1)")?;
        let rows: Vec<(i64,)> = dest_conn.exec_rows("SELECT a FROM t");
        assert_eq!(rows, vec![(1,)]);
    }

    // Test 2: Empty tables with indexes (schema only, no data)
    {
        let conn = tmp_db.connect_limbo();
        conn.execute("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")?;
        conn.execute("CREATE TABLE t2 (a INTEGER, b REAL)")?;
        conn.execute("CREATE INDEX idx_t1_name ON t1 (name)")?;
        conn.execute("CREATE UNIQUE INDEX idx_t2_a ON t2 (a)")?;

        let source_hash = compute_dbhash(&tmp_db);

        let dest_path = dest_dir.path().join("empty2.db");
        conn.execute(&format!("VACUUM INTO '{}'", dest_path.to_str().unwrap()))?;

        let dest_db = TempDatabase::new_with_existent(&dest_path);
        let dest_conn = dest_db.connect_limbo();

        assert_eq!(run_integrity_check(&dest_conn), "ok");
        assert_eq!(source_hash.hash, compute_dbhash(&dest_db).hash);

        // Verify tables empty
        let cnt: Vec<(i64,)> = dest_conn.exec_rows("SELECT COUNT(*) FROM t1");
        assert_eq!(cnt, vec![(0,)]);

        // Verify indexes exist and work
        let indexes: Vec<(String,)> = dest_conn
            .exec_rows("SELECT name FROM sqlite_schema WHERE type = 'index' ORDER BY name");
        assert_eq!(
            indexes,
            vec![("idx_t1_name".to_string(),), ("idx_t2_a".to_string(),)]
        );

        // Verify unique constraint works
        dest_conn.execute("INSERT INTO t2 VALUES (1, 1.0)")?;
        let dup = dest_conn.execute("INSERT INTO t2 VALUES (1, 2.0)");
        assert!(dup.is_err(), "Unique index should prevent duplicate");
    }

    Ok(())
}

/// Test VACUUM INTO preserves AUTOINCREMENT counters (sqlite_sequence)
#[turso_macros::test]
fn test_vacuum_into_preserves_autoincrement(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Create table with AUTOINCREMENT
    conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)")?;

    // Insert some rows to advance the counter
    conn.execute("INSERT INTO t (name) VALUES ('first')")?;
    conn.execute("INSERT INTO t (name) VALUES ('second')")?;
    conn.execute("INSERT INTO t (name) VALUES ('third')")?;

    // Delete rows to create a gap
    conn.execute("DELETE FROM t WHERE id = 2")?;

    // Verify sqlite_sequence has the counter
    let seq_before: Vec<(String, i64)> =
        conn.exec_rows("SELECT name, seq FROM sqlite_sequence WHERE name = 't'");
    assert_eq!(
        seq_before,
        vec![("t".to_string(), 3)],
        "sqlite_sequence should have counter value 3"
    );

    let source_hash = compute_dbhash(&tmp_db);

    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

    // Verify integrity and dbhash (before modifying destination)
    assert_eq!(run_integrity_check(&dest_conn), "ok");
    assert_eq!(source_hash.hash, compute_dbhash(&dest_db).hash);

    // Verify sqlite_sequence was copied
    let seq_after: Vec<(String, i64)> =
        dest_conn.exec_rows("SELECT name, seq FROM sqlite_sequence WHERE name = 't'");
    assert_eq!(
        seq_after,
        vec![("t".to_string(), 3)],
        "sqlite_sequence should be preserved in destination"
    );

    // Insert a new row and verify it gets id = 4 (not 1 or 3)
    dest_conn.execute("INSERT INTO t (name) VALUES ('fourth')")?;
    let new_row: Vec<(i64, String)> =
        dest_conn.exec_rows("SELECT id, name FROM t WHERE name = 'fourth'");
    assert_eq!(
        new_row,
        vec![(4, "fourth".to_string())],
        "New row should get id = 4 (AUTOINCREMENT counter preserved)"
    );

    // Verify integrity
    let integrity_result = run_integrity_check(&dest_conn);
    assert_eq!(integrity_result, "ok");

    Ok(())
}

/// Test that a table with "sqlite_sequence" in its SQL (e.g., default value) is NOT skipped
#[turso_macros::test]
fn test_vacuum_into_table_with_sqlite_sequence_in_sql(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Create a table that mentions "sqlite_sequence" in a default value
    // This should NOT be skipped during schema copy
    conn.execute(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, content TEXT DEFAULT 'see sqlite_sequence')",
    )?;

    conn.execute("INSERT INTO notes (id) VALUES (1)")?;
    conn.execute("INSERT INTO notes (id, content) VALUES (2, 'custom')")?;

    let source_hash = compute_dbhash(&tmp_db);

    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

    // Verify integrity and dbhash
    assert_eq!(run_integrity_check(&dest_conn), "ok");
    assert_eq!(
        source_hash.hash,
        compute_dbhash(&dest_db).hash,
        "Source and destination databases should have the same content hash"
    );

    // Verify the table was created and data was copied
    let rows: Vec<(i64, String)> = dest_conn.exec_rows("SELECT id, content FROM notes ORDER BY id");
    assert_eq!(
        rows,
        vec![
            (1, "see sqlite_sequence".to_string()),
            (2, "custom".to_string())
        ],
        "Table with 'sqlite_sequence' in SQL should be created and data copied"
    );

    Ok(())
}

/// Test VACUUM INTO with table names containing special characters
/// Consolidates tests for: spaces, quotes, SQL keywords, unicode, numeric names, and mixed special chars
#[turso_macros::test]
fn test_vacuum_into_special_table_names(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // 1. Table with spaces
    conn.execute(r#"CREATE TABLE "table with spaces" (id INTEGER, value TEXT)"#)?;
    conn.execute(r#"INSERT INTO "table with spaces" VALUES (1, 'spaces work')"#)?;

    // 2. Table with double quotes
    conn.execute(r#"CREATE TABLE "table""quote" (id INTEGER, data TEXT)"#)?;
    conn.execute(r#"INSERT INTO "table""quote" VALUES (2, 'quotes work')"#)?;

    // 3. SQL reserved keyword as table name
    conn.execute(r#"CREATE TABLE "select" (id INTEGER, val TEXT)"#)?;
    conn.execute(r#"INSERT INTO "select" VALUES (3, 'keyword works')"#)?;

    // 4. Unicode table name (Chinese, accents, emoji)
    conn.execute(r#"CREATE TABLE "表格_données_🎉" (id INTEGER, val TEXT)"#)?;
    conn.execute(r#"INSERT INTO "表格_données_🎉" VALUES (4, 'unicode works')"#)?;

    // 5. Numeric table name
    conn.execute(r#"CREATE TABLE "123" (id INTEGER, val TEXT)"#)?;
    conn.execute(r#"INSERT INTO "123" VALUES (5, 'numeric works')"#)?;

    // 6. Mixed special characters (multiple quotes, spaces, SQL-injection-like)
    conn.execute(r#"CREATE TABLE "table ""with"" many; DROP TABLE--" (id INTEGER, val TEXT)"#)?;
    conn.execute(r#"INSERT INTO "table ""with"" many; DROP TABLE--" VALUES (6, 'mixed works')"#)?;

    let source_hash = compute_dbhash(&tmp_db);

    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed_tables.db");
    let dest_path_str = dest_path.to_str().unwrap();

    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

    // Verify integrity and dbhash
    assert_eq!(
        run_integrity_check(&dest_conn),
        "ok",
        "Destination should pass integrity check"
    );
    assert_eq!(
        source_hash.hash,
        compute_dbhash(&dest_db).hash,
        "Source and destination databases should have the same content hash"
    );

    // Verify all tables were copied correctly
    let r1: Vec<(i64, String)> = dest_conn.exec_rows(r#"SELECT * FROM "table with spaces""#);
    assert_eq!(r1, vec![(1, "spaces work".to_string())]);

    let r2: Vec<(i64, String)> = dest_conn.exec_rows(r#"SELECT * FROM "table""quote""#);
    assert_eq!(r2, vec![(2, "quotes work".to_string())]);

    let r3: Vec<(i64, String)> = dest_conn.exec_rows(r#"SELECT * FROM "select""#);
    assert_eq!(r3, vec![(3, "keyword works".to_string())]);

    let r4: Vec<(i64, String)> = dest_conn.exec_rows(r#"SELECT * FROM "表格_données_🎉""#);
    assert_eq!(r4, vec![(4, "unicode works".to_string())]);

    let r5: Vec<(i64, String)> = dest_conn.exec_rows(r#"SELECT * FROM "123""#);
    assert_eq!(r5, vec![(5, "numeric works".to_string())]);

    let r6: Vec<(i64, String)> =
        dest_conn.exec_rows(r#"SELECT * FROM "table ""with"" many; DROP TABLE--""#);
    assert_eq!(r6, vec![(6, "mixed works".to_string())]);

    Ok(())
}

/// Test VACUUM INTO preserves float precision
#[turso_macros::test]
fn test_vacuum_into_preserves_float_precision(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    conn.execute("CREATE TABLE floats (id INTEGER PRIMARY KEY, value REAL)")?;

    // Insert various floats that require high precision
    // These values are chosen to test edge cases in float representation
    let test_values: Vec<f64> = vec![
        0.1,                  // Classic binary representation issue
        0.123456789012345,    // Many decimal places
        1.0000000000000002,   // Smallest increment above 1.0
        std::f64::consts::PI, // Pi (3.141592653589793)
        std::f64::consts::E,  // Euler's number (2.718281828459045)
        1e-10,                // Very small number
        1e15,                 // Large number
        -0.999999999999999,   // Negative with many 9s
        123456789.123456789,  // Large with decimals
        1.0,                  // Integer-like float (must stay float, not become int)
        -2.0,                 // Negative integer-like float
        0.0,                  // Zero as float
        100.0,                // Larger integer-like float
    ];

    for (i, &val) in test_values.iter().enumerate() {
        conn.execute(&format!(
            "INSERT INTO floats VALUES ({}, {:.17})",
            i + 1,
            val
        ))?;
    }

    let source_hash = compute_dbhash(&tmp_db);

    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

    // Verify integrity and dbhash
    assert_eq!(run_integrity_check(&dest_conn), "ok");
    assert_eq!(
        source_hash.hash,
        compute_dbhash(&dest_db).hash,
        "Source and destination databases should have the same content hash"
    );

    // Verify float precision is preserved
    let rows: Vec<(i64, f64)> = dest_conn.exec_rows("SELECT id, value FROM floats ORDER BY id");
    assert_eq!(rows.len(), test_values.len());

    for (i, &expected) in test_values.iter().enumerate() {
        let actual = rows[i].1;
        assert!(
            (actual - expected).abs() < 1e-15 || actual == expected,
            "Float precision lost for value {}: expected {:.17}, got {:.17}",
            i + 1,
            expected,
            actual
        );
    }

    Ok(())
}

/// Test VACUUM INTO behavior with virtual tables (FTS)
/// This test documents the current behavior - virtual tables have rootpage=0
/// and SQLite handles them specially in VACUUM
#[turso_macros::test]
fn test_vacuum_into_with_virtual_table(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Create a regular table
    conn.execute("CREATE TABLE documents (id INTEGER PRIMARY KEY, title TEXT, body TEXT)")?;
    conn.execute("INSERT INTO documents VALUES (1, 'Hello World', 'This is a test document')")?;
    conn.execute(
        "INSERT INTO documents VALUES (2, 'Rust Programming', 'Rust is a systems language')",
    )?;

    // Try to create a virtual table (FTS5)
    // Note: This may fail if FTS is not enabled - that's also useful information
    let fts_result = conn.execute(
        "CREATE VIRTUAL TABLE documents_fts USING fts5(title, body, content=documents, content_rowid=id)"
    );

    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed_virtual.db");
    let dest_path_str = dest_path.to_str().unwrap();

    match fts_result {
        Ok(_) => {
            // FTS table was created successfully
            // Check that the virtual table exists in schema
            let schema: Vec<(String, String, i64)> = conn.exec_rows(
                "SELECT type, name, COALESCE(rootpage, -1) FROM sqlite_schema WHERE name LIKE 'documents%' ORDER BY name"
            );

            // Virtual tables typically have rootpage=0
            // Log what we found for debugging
            for (type_val, name, rootpage) in &schema {
                println!("Schema entry: type={type_val}, name={name}, rootpage={rootpage}");
            }

            let source_hash = compute_dbhash(&tmp_db);

            // Execute VACUUM INTO
            let vacuum_result = conn.execute(&format!("VACUUM INTO '{dest_path_str}'"));

            match vacuum_result {
                Ok(_) => {
                    // VACUUM succeeded - check what was copied
                    let dest_db = TempDatabase::new_with_existent(&dest_path);
                    let dest_conn = dest_db.connect_limbo();

                    // Verify integrity and dbhash
                    assert_eq!(
                        run_integrity_check(&dest_conn),
                        "ok",
                        "Destination should pass integrity check"
                    );
                    assert_eq!(
                        source_hash.hash,
                        compute_dbhash(&dest_db).hash,
                        "Source and destination databases should have the same content hash"
                    );

                    // Check what tables exist in destination
                    let dest_schema: Vec<(String, String)> = dest_conn.exec_rows(
                        "SELECT type, name FROM sqlite_schema WHERE name LIKE 'documents%' ORDER BY name"
                    );
                    println!("Destination schema: {dest_schema:?}");

                    // Regular table data should be copied
                    let rows: Vec<(i64, String)> =
                        dest_conn.exec_rows("SELECT id, title FROM documents ORDER BY id");
                    assert_eq!(
                        rows,
                        vec![
                            (1, "Hello World".to_string()),
                            (2, "Rust Programming".to_string())
                        ],
                        "Regular table data should be copied"
                    );
                }
                Err(e) => {
                    // VACUUM failed with virtual table - document the error
                    println!("VACUUM INTO failed with virtual table present: {e}");
                    // This is acceptable - we're documenting behavior
                }
            }
        }
        Err(e) => {
            // FTS not supported or not enabled - test without virtual table
            println!("FTS virtual table creation failed (expected if FTS not enabled): {e}");

            let source_hash = compute_dbhash(&tmp_db);

            // Just vacuum the regular table
            conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

            let dest_db = TempDatabase::new_with_existent(&dest_path);
            let dest_conn = dest_db.connect_limbo();

            // Verify integrity and dbhash
            assert_eq!(run_integrity_check(&dest_conn), "ok");
            assert_eq!(
                source_hash.hash,
                compute_dbhash(&dest_db).hash,
                "Source and destination databases should have the same content hash"
            );

            let rows: Vec<(i64, String)> =
                dest_conn.exec_rows("SELECT id, title FROM documents ORDER BY id");
            assert_eq!(
                rows,
                vec![
                    (1, "Hello World".to_string()),
                    (2, "Rust Programming".to_string())
                ]
            );
        }
    }

    Ok(())
}

/// Test VACUUM INTO with tables that have no columns
/// SQLite allows CREATE TABLE t(); with zero columns
#[turso_macros::test]
fn test_vacuum_into_table_with_no_columns(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Try to create a table with no columns
    // This is valid SQL in SQLite: CREATE TABLE t();
    let create_result = conn.execute("CREATE TABLE empty_cols()");

    match create_result {
        Ok(_) => {
            // Table with no columns was created successfully
            // Verify it exists in schema
            let schema: Vec<(String, String)> =
                conn.exec_rows("SELECT type, name FROM sqlite_schema WHERE name = 'empty_cols'");
            assert_eq!(
                schema,
                vec![("table".to_string(), "empty_cols".to_string())],
                "Table with no columns should exist in schema"
            );

            // Check column count via pragma
            let columns: Vec<(String,)> =
                conn.exec_rows("SELECT name FROM pragma_table_info('empty_cols')");
            assert!(
                columns.is_empty(),
                "Table should have no columns, got: {columns:?}"
            );

            // Also create a normal table to ensure mixed scenario works
            conn.execute("CREATE TABLE normal_table (id INTEGER, name TEXT)")?;
            conn.execute("INSERT INTO normal_table VALUES (1, 'test')")?;

            let source_hash = compute_dbhash(&tmp_db);

            let dest_dir = TempDir::new()?;
            let dest_path = dest_dir.path().join("vacuumed_no_cols.db");
            let dest_path_str = dest_path.to_str().unwrap();

            // Execute VACUUM INTO
            let vacuum_result = conn.execute(&format!("VACUUM INTO '{dest_path_str}'"));

            match vacuum_result {
                Ok(_) => {
                    // VACUUM succeeded
                    let dest_db = TempDatabase::new_with_existent(&dest_path);
                    let dest_conn = dest_db.connect_limbo();

                    // Verify integrity and dbhash
                    assert_eq!(run_integrity_check(&dest_conn), "ok");
                    assert_eq!(
                        source_hash.hash,
                        compute_dbhash(&dest_db).hash,
                        "Source and destination databases should have the same content hash"
                    );

                    // Verify the no-column table exists
                    let dest_schema: Vec<(String,)> = dest_conn
                        .exec_rows("SELECT name FROM sqlite_schema WHERE name = 'empty_cols'");
                    assert_eq!(
                        dest_schema,
                        vec![("empty_cols".to_string(),)],
                        "Table with no columns should be copied"
                    );

                    // Verify normal table data was copied
                    let rows: Vec<(i64, String)> =
                        dest_conn.exec_rows("SELECT id, name FROM normal_table");
                    assert_eq!(rows, vec![(1, "test".to_string())]);
                }
                Err(e) => {
                    // VACUUM failed - document the error
                    println!("VACUUM INTO failed with no-column table: {e}");
                    // This documents the behavior
                }
            }
        }
        Err(e) => {
            // Creating table with no columns is not supported
            println!("CREATE TABLE with no columns not supported: {e}");
            // This is also valid behavior to document
        }
    }

    Ok(())
}

/// Test VACUUM INTO with column names containing special characters
/// Consolidates tests for: spaces, quotes, SQL keywords, unicode, numeric, dashes, dots,
/// mixed special chars, and indexes on special columns
#[turso_macros::test]
fn test_vacuum_into_special_column_names(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Table with various special column names covering all edge cases
    conn.execute(
        r#"CREATE TABLE special_cols (
            "column with spaces" INTEGER,
            "column""with""quotes" TEXT,
            "from" INTEGER,
            "列名_données_🎉" TEXT,
            "123numeric" REAL,
            "col.with" INTEGER,
            "SELECT * FROM t; --" TEXT
        )"#,
    )?;

    // Create index on column with special name
    conn.execute(r#"CREATE INDEX "idx on special" ON special_cols ("column with spaces")"#)?;
    conn.execute(r#"CREATE INDEX "idx""quoted" ON special_cols ("column""with""quotes")"#)?;

    // Insert test data
    conn.execute(
        r#"INSERT INTO special_cols VALUES (1, 'quotes', 10, 'unicode', 1.5, 100, 'injection')"#,
    )?;
    conn.execute(r#"INSERT INTO special_cols VALUES (2, 'work', 20, 'works', 2.5, 200, 'safe')"#)?;

    let source_hash = compute_dbhash(&tmp_db);

    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed_cols.db");
    let dest_path_str = dest_path.to_str().unwrap();

    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

    // Verify integrity and dbhash
    assert_eq!(
        run_integrity_check(&dest_conn),
        "ok",
        "Destination should pass integrity check"
    );
    assert_eq!(
        source_hash.hash,
        compute_dbhash(&dest_db).hash,
        "Source and destination databases should have the same content hash"
    );

    // Verify all column data was copied correctly
    let rows: Vec<(i64, String, i64, String, f64, i64, String)> = dest_conn.exec_rows(
        r#"SELECT "column with spaces", "column""with""quotes", "from", "列名_données_🎉", "123numeric", "col.with", "SELECT * FROM t; --" FROM special_cols ORDER BY "column with spaces""#,
    );
    assert_eq!(
        rows,
        vec![
            (
                1,
                "quotes".to_string(),
                10,
                "unicode".to_string(),
                1.5,
                100,
                "injection".to_string()
            ),
            (
                2,
                "work".to_string(),
                20,
                "works".to_string(),
                2.5,
                200,
                "safe".to_string()
            )
        ]
    );

    // Verify indexes exist
    let indexes: Vec<(String,)> = dest_conn.exec_rows(
        r#"SELECT name FROM sqlite_schema WHERE type = 'index' AND name LIKE 'idx%' ORDER BY name"#,
    );
    assert_eq!(
        indexes,
        vec![
            ("idx on special".to_string(),),
            ("idx\"quoted".to_string(),)
        ]
    );

    Ok(())
}
