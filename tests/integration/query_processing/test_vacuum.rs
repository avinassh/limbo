use crate::common::{compute_dbhash, ExecRows, TempDatabase};
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

#[turso_macros::test]
fn test_plain_vacuum_fails(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let conn = tmp_db.connect_limbo();

    // Plain VACUUM should fail
    let result = conn.execute("VACUUM");
    assert!(
        result.is_err(),
        "Plain VACUUM should fail with an error message"
    );

    Ok(())
}

#[turso_macros::test(init_sql = "CREATE TABLE t (a INTEGER);")]
fn test_vacuum_into_existing_file_fails(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let conn = tmp_db.connect_limbo();

    // Create a temp file that already exists
    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("existing.db");
    std::fs::write(&dest_path, b"existing content")?;
    let dest_path_str = dest_path.to_str().unwrap();

    // VACUUM INTO existing file should fail
    let result = conn.execute(&format!("VACUUM INTO '{dest_path_str}'"));
    assert!(
        result.is_err(),
        "VACUUM INTO existing file should fail with an error"
    );

    // Verify the error message mentions the file exists
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("already exists") || err_msg.contains("output file"),
        "Error message should mention file already exists, got: {err_msg}"
    );

    Ok(())
}

#[turso_macros::test(init_sql = "CREATE TABLE t (a INTEGER);")]
fn test_vacuum_into_within_transaction_fails(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let conn = tmp_db.connect_limbo();

    // Insert some data
    conn.execute("INSERT INTO t VALUES (1)")?;

    // Start a transaction
    conn.execute("BEGIN")?;

    // Insert more data within the transaction
    conn.execute("INSERT INTO t VALUES (2)")?;

    // Create destination path
    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    // VACUUM INTO should fail within a transaction
    let result = conn.execute(&format!("VACUUM INTO '{dest_path_str}'"));
    assert!(
        result.is_err(),
        "VACUUM INTO should fail when called within a transaction"
    );

    // Verify the error message mentions transaction
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("transaction") || err_msg.contains("VACUUM"),
        "Error message should mention transaction restriction, got: {err_msg}"
    );

    // Verify the destination file was not created
    assert!(
        !dest_path.exists(),
        "Destination file should not be created when VACUUM fails"
    );

    // Rollback and verify data is still intact
    conn.execute("ROLLBACK")?;

    // Original data should still be there
    let rows: Vec<(i64,)> = conn.exec_rows("SELECT a FROM t ORDER BY a");
    assert_eq!(
        rows,
        vec![(1,)],
        "Only committed data should remain after rollback"
    );

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

#[turso_macros::test]
fn test_vacuum_into_with_view(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let conn = tmp_db.connect_limbo();

    // Create table and view
    conn.execute("CREATE TABLE t (a INTEGER, b TEXT)")?;
    conn.execute("CREATE VIEW v AS SELECT a, b FROM t WHERE a > 1")?;

    // Insert some data
    conn.execute("INSERT INTO t VALUES (1, 'one'), (2, 'two'), (3, 'three')")?;

    // Compute hash of source database before vacuum
    let source_hash = compute_dbhash(&tmp_db);

    // Create destination
    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    // Execute VACUUM INTO
    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

    // Open destination and verify view exists and works
    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

    // Verify integrity of destination database
    let integrity_result = run_integrity_check(&dest_conn);
    assert_eq!(
        integrity_result, "ok",
        "Destination database with view should pass integrity check"
    );

    // Verify dbhash matches between source and destination
    let dest_hash = compute_dbhash(&dest_db);
    assert_eq!(
        source_hash.hash, dest_hash.hash,
        "Source and destination databases should have the same content hash"
    );

    // Check that the view exists in the schema
    let schema: Vec<(String, String)> =
        dest_conn.exec_rows("SELECT type, name FROM sqlite_schema WHERE type = 'view'");
    assert!(
        schema.iter().any(|(_, name)| name == "v"),
        "View should exist in vacuumed database"
    );

    // Query the view to verify it works
    let rows: Vec<(i64, String)> = dest_conn.exec_rows("SELECT a, b FROM v ORDER BY a");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], (2, "two".to_string()));
    assert_eq!(rows[1], (3, "three".to_string()));

    Ok(())
}

/// Test VACUUM INTO with triggers (requires MVCC mode)
#[turso_macros::test(mvcc)]
fn test_vacuum_into_with_trigger(tmp_db: TempDatabase) {
    let conn = tmp_db.connect_limbo();

    // Create tables
    conn.execute("CREATE TABLE t (a INTEGER)").unwrap();
    conn.execute("CREATE TABLE log (msg TEXT)").unwrap();

    // Create a trigger
    conn.execute(
        "CREATE TRIGGER t_insert AFTER INSERT ON t BEGIN
            INSERT INTO log VALUES ('inserted ' || NEW.a);
        END",
    )
    .unwrap();

    // Insert some data (trigger will fire)
    conn.execute("INSERT INTO t VALUES (1), (2)").unwrap();

    // Create destination
    let dest_dir = TempDir::new().unwrap();
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    // Execute VACUUM INTO
    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))
        .unwrap();

    // Open destination with triggers enabled and verify trigger exists
    let dest_opts = turso_core::DatabaseOpts::new().with_triggers(true);
    let dest_db = TempDatabase::new_with_existent_with_opts(&dest_path, dest_opts);
    let dest_conn = dest_db.connect_limbo();

    // Enable MVCC on destination to use triggers
    dest_conn
        .pragma_update("journal_mode", "'experimental_mvcc'")
        .unwrap();

    // Verify integrity of destination database
    let integrity_result = run_integrity_check(&dest_conn);
    assert_eq!(
        integrity_result, "ok",
        "Destination database with trigger should pass integrity check"
    );

    // Check that the trigger exists in the schema
    let schema: Vec<(String, String)> =
        dest_conn.exec_rows("SELECT type, name FROM sqlite_schema WHERE type = 'trigger'");
    assert!(
        schema.iter().any(|(_, name)| name == "t_insert"),
        "Trigger should exist in vacuumed database"
    );

    // Verify the data was copied
    let t_rows: Vec<(i64,)> = dest_conn.exec_rows("SELECT a FROM t ORDER BY a");
    assert_eq!(t_rows, vec![(1,), (2,)]);

    // Verify log entries from source were copied correctly
    // (triggers are created after data copy, so they don't fire during VACUUM INTO)
    let log_rows: Vec<(String,)> = dest_conn.exec_rows("SELECT msg FROM log ORDER BY msg");
    assert_eq!(
        log_rows,
        vec![("inserted 1".to_string(),), ("inserted 2".to_string(),)],
        "Original log entries should be copied without duplicates"
    );

    // Verify the trigger works in the destination database for new inserts
    dest_conn.execute("INSERT INTO t VALUES (3)").unwrap();
    let new_log: Vec<(String,)> = dest_conn.exec_rows("SELECT msg FROM log ORDER BY msg");
    assert_eq!(
        new_log,
        vec![
            ("inserted 1".to_string(),),
            ("inserted 2".to_string(),),
            ("inserted 3".to_string(),)
        ],
        "Trigger should fire for new inserts in destination database"
    );
}

#[turso_macros::test(init_sql = "CREATE TABLE t (a INTEGER);")]
fn test_vacuum_into_preserves_meta_values(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let conn = tmp_db.connect_limbo();

    // Set user_version and application_id on source database
    conn.execute("PRAGMA user_version = 42")?;
    conn.execute("PRAGMA application_id = 12345")?;

    // Insert some data
    conn.execute("INSERT INTO t VALUES (1)")?;

    // Create destination
    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    // Execute VACUUM INTO
    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

    // Open destination and verify meta values are preserved
    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

    // Verify integrity of destination database
    let integrity_result = run_integrity_check(&dest_conn);
    assert_eq!(
        integrity_result, "ok",
        "Destination database should pass integrity check"
    );

    // Check user_version
    let user_version: Vec<(i64,)> = dest_conn.exec_rows("PRAGMA user_version");
    assert_eq!(
        user_version,
        vec![(42,)],
        "user_version should be preserved in destination database"
    );

    // Check application_id
    let application_id: Vec<(i64,)> = dest_conn.exec_rows("PRAGMA application_id");
    assert_eq!(
        application_id,
        vec![(12345,)],
        "application_id should be preserved in destination database"
    );

    // Verify data was also copied
    let rows: Vec<(i64,)> = dest_conn.exec_rows("SELECT a FROM t");
    assert_eq!(rows, vec![(1,)]);

    Ok(())
}

/// Test VACUUM INTO with multiple triggers across different tables
#[turso_macros::test(mvcc)]
fn test_vacuum_into_with_multiple_triggers(tmp_db: TempDatabase) {
    let conn = tmp_db.connect_limbo();

    // Create tables
    conn.execute("CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT, price INTEGER)")
        .unwrap();
    conn.execute("CREATE TABLE audit_log (action TEXT, table_name TEXT, record_id INTEGER)")
        .unwrap();
    conn.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, product_id INTEGER, quantity INTEGER)",
    )
    .unwrap();

    // Create triggers on multiple tables
    conn.execute(
        "CREATE TRIGGER log_product_insert AFTER INSERT ON products BEGIN
            INSERT INTO audit_log VALUES ('INSERT', 'products', NEW.id);
        END",
    )
    .unwrap();
    conn.execute(
        "CREATE TRIGGER log_order_insert AFTER INSERT ON orders BEGIN
            INSERT INTO audit_log VALUES ('INSERT', 'orders', NEW.id);
        END",
    )
    .unwrap();

    // Insert data (triggers will fire)
    conn.execute("INSERT INTO products VALUES (1, 'Item A', 50)")
        .unwrap();
    conn.execute("INSERT INTO products VALUES (2, 'Item B', 200)")
        .unwrap();
    conn.execute("INSERT INTO orders VALUES (1, 1, 5)").unwrap();
    conn.execute("INSERT INTO orders VALUES (2, 2, 3)").unwrap();

    // Create destination
    let dest_dir = TempDir::new().unwrap();
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    // Execute VACUUM INTO
    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))
        .unwrap();

    // Open destination with triggers enabled
    let dest_opts = turso_core::DatabaseOpts::new().with_triggers(true);
    let dest_db = TempDatabase::new_with_existent_with_opts(&dest_path, dest_opts);
    let dest_conn = dest_db.connect_limbo();

    // Enable MVCC on destination
    dest_conn
        .pragma_update("journal_mode", "'experimental_mvcc'")
        .unwrap();

    // Verify integrity
    let integrity_result = run_integrity_check(&dest_conn);
    assert_eq!(integrity_result, "ok");

    // Verify data was copied
    let products: Vec<(i64, String)> =
        dest_conn.exec_rows("SELECT id, name FROM products ORDER BY id");
    assert_eq!(
        products,
        vec![(1, "Item A".to_string()), (2, "Item B".to_string())]
    );

    let orders: Vec<(i64, i64, i64)> =
        dest_conn.exec_rows("SELECT id, product_id, quantity FROM orders ORDER BY id");
    assert_eq!(orders, vec![(1, 1, 5), (2, 2, 3)]);

    // Verify audit_log has original entries only (no duplicates from triggers firing during copy)
    let audit: Vec<(String, String, i64)> = dest_conn.exec_rows(
        "SELECT action, table_name, record_id FROM audit_log ORDER BY table_name, record_id",
    );
    assert_eq!(
        audit,
        vec![
            ("INSERT".to_string(), "orders".to_string(), 1),
            ("INSERT".to_string(), "orders".to_string(), 2),
            ("INSERT".to_string(), "products".to_string(), 1),
            ("INSERT".to_string(), "products".to_string(), 2),
        ],
        "Audit log should have original entries without duplicates"
    );

    // Verify both triggers work for new inserts
    dest_conn
        .execute("INSERT INTO products VALUES (3, 'New Item', 100)")
        .unwrap();
    dest_conn
        .execute("INSERT INTO orders VALUES (3, 3, 1)")
        .unwrap();

    let new_audit: Vec<(String, String, i64)> =
        dest_conn.exec_rows("SELECT action, table_name, record_id FROM audit_log WHERE record_id = 3 ORDER BY table_name");
    assert_eq!(
        new_audit,
        vec![
            ("INSERT".to_string(), "orders".to_string(), 3),
            ("INSERT".to_string(), "products".to_string(), 3),
        ],
        "Both triggers should fire for new inserts"
    );
}

/// Test VACUUM INTO preserves boundary/negative meta values
#[turso_macros::test(init_sql = "CREATE TABLE t (a INTEGER);")]
fn test_vacuum_into_preserves_boundary_meta_values(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Set negative user_version and max application_id
    conn.execute("PRAGMA user_version = -1")?;
    conn.execute("PRAGMA application_id = 2147483647")?; // i32::MAX

    conn.execute("INSERT INTO t VALUES (1)")?;

    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

    // Verify boundary values are preserved
    let user_version: Vec<(i64,)> = dest_conn.exec_rows("PRAGMA user_version");
    assert_eq!(
        user_version,
        vec![(-1,)],
        "Negative user_version should be preserved"
    );

    let application_id: Vec<(i64,)> = dest_conn.exec_rows("PRAGMA application_id");
    assert_eq!(
        application_id,
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

    // Verify integrity
    let integrity_result = run_integrity_check(&dest_conn);
    assert_eq!(integrity_result, "ok");

    // Verify data was copied
    let rows: Vec<(i64, String)> = dest_conn.exec_rows("SELECT a, b FROM t ORDER BY a");
    assert_eq!(rows, vec![(1, "hello".to_string()), (2, "world".to_string())]);

    Ok(())
}

/// Test VACUUM INTO with empty tables (schema only, no data)
#[turso_macros::test]
fn test_vacuum_into_empty_tables(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Create multiple empty tables with various features
    conn.execute("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")?;
    conn.execute("CREATE TABLE t2 (a INTEGER, b REAL, c BLOB)")?;
    conn.execute("CREATE INDEX idx_t1_name ON t1 (name)")?;

    let source_hash = compute_dbhash(&tmp_db);

    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

    // Verify integrity
    let integrity_result = run_integrity_check(&dest_conn);
    assert_eq!(integrity_result, "ok");

    // Verify hash matches
    let dest_hash = compute_dbhash(&dest_db);
    assert_eq!(source_hash.hash, dest_hash.hash);

    // Verify tables exist and are empty
    let t1_rows: Vec<(i64,)> = dest_conn.exec_rows("SELECT COUNT(*) FROM t1");
    assert_eq!(t1_rows, vec![(0,)]);

    let t2_rows: Vec<(i64,)> = dest_conn.exec_rows("SELECT COUNT(*) FROM t2");
    assert_eq!(t2_rows, vec![(0,)]);

    // Verify index exists
    let indexes: Vec<(String,)> = dest_conn
        .exec_rows("SELECT name FROM sqlite_schema WHERE type = 'index' AND name = 'idx_t1_name'");
    assert_eq!(indexes, vec![("idx_t1_name".to_string(),)]);

    // Verify we can insert into the empty tables
    dest_conn.execute("INSERT INTO t1 (name) VALUES ('test')")?;
    let inserted: Vec<(i64, String)> = dest_conn.exec_rows("SELECT id, name FROM t1");
    assert_eq!(inserted, vec![(1, "test".to_string())]);

    Ok(())
}

/// Test that views correctly query copied data
#[turso_macros::test]
fn test_vacuum_into_view_queries_copied_data(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();

    // Create table with data
    conn.execute(
        "CREATE TABLE employees (id INTEGER, name TEXT, department TEXT, salary INTEGER)",
    )?;
    conn.execute("INSERT INTO employees VALUES (1, 'Alice', 'Engineering', 100000)")?;
    conn.execute("INSERT INTO employees VALUES (2, 'Bob', 'Sales', 80000)")?;
    conn.execute("INSERT INTO employees VALUES (3, 'Charlie', 'Engineering', 120000)")?;
    conn.execute("INSERT INTO employees VALUES (4, 'Diana', 'HR', 70000)")?;

    // Create multiple views with different complexities
    conn.execute(
        "CREATE VIEW engineering_team AS SELECT id, name, salary FROM employees WHERE department = 'Engineering'",
    )?;
    conn.execute(
        "CREATE VIEW high_earners AS SELECT name, salary FROM employees WHERE salary > 90000",
    )?;
    conn.execute(
        "CREATE VIEW department_summary AS SELECT department, COUNT(*) as count FROM employees GROUP BY department",
    )?;

    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

    // Verify views return correct data from copied tables
    let engineering: Vec<(i64, String, i64)> =
        dest_conn.exec_rows("SELECT id, name, salary FROM engineering_team ORDER BY id");
    assert_eq!(
        engineering,
        vec![
            (1, "Alice".to_string(), 100000),
            (3, "Charlie".to_string(), 120000)
        ]
    );

    let high_earners: Vec<(String, i64)> =
        dest_conn.exec_rows("SELECT name, salary FROM high_earners ORDER BY salary DESC");
    assert_eq!(
        high_earners,
        vec![
            ("Charlie".to_string(), 120000),
            ("Alice".to_string(), 100000)
        ]
    );

    let summary: Vec<(String, i64)> =
        dest_conn.exec_rows("SELECT department, count FROM department_summary ORDER BY department");
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

    let dest_dir = TempDir::new()?;
    let dest_path = dest_dir.path().join("vacuumed.db");
    let dest_path_str = dest_path.to_str().unwrap();

    conn.execute(&format!("VACUUM INTO '{dest_path_str}'"))?;

    let dest_db = TempDatabase::new_with_existent(&dest_path);
    let dest_conn = dest_db.connect_limbo();

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

