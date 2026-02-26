use crate::common::{ExecRows, TempDatabase};
use std::sync::Arc;
use turso_core::StepResult;

#[turso_macros::test(mvcc)]
fn test_newrowid_mvcc_concurrent(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();

    let tmp_db = Arc::new(tmp_db);

    {
        let conn = tmp_db.connect_limbo();
        conn.execute("CREATE TABLE t_concurrent(id INTEGER PRIMARY KEY, value TEXT)")?;
    }

    let mut threads = Vec::new();
    let num_threads = 10;
    let inserts_per_thread = 10;

    for thread_id in 0..num_threads {
        let tmp_db_clone = tmp_db.clone();
        threads.push(std::thread::spawn(move || -> anyhow::Result<()> {
            let conn = tmp_db_clone.connect_limbo();

            for i in 0..inserts_per_thread {
                let mut stmt = conn.prepare(format!(
                    "INSERT INTO t_concurrent VALUES(NULL, 'thread_{thread_id}_value_{i}')",
                ))?;
                // Retry loop for handling busy conditions
                'retry: loop {
                    loop {
                        match stmt.step()? {
                            StepResult::IO => {
                                stmt._io().step()?;
                            }
                            StepResult::Done => {
                                break 'retry;
                            }
                            StepResult::Busy => {
                                // Statement is busy, re-prepare and retry
                                break;
                            }
                            StepResult::Row => {
                                anyhow::bail!("Unexpected row from INSERT");
                            }
                            StepResult::Interrupt => {
                                anyhow::bail!("Unexpected interrupt");
                            }
                        }
                    }
                }
            }
            Ok(())
        }));
    }

    for thread in threads {
        thread.join().unwrap()?;
    }

    // Verify we got the right number of rows
    let conn = tmp_db.connect_limbo();

    // Debug: check what we actually got
    let mut stmt_all = conn.prepare("SELECT id, value FROM t_concurrent ORDER BY id")?;
    let mut all_rows = Vec::new();
    stmt_all.run_with_row_callback(|row| {
        let id = row.get::<i64>(0)?;
        let value = row.get::<String>(1)?;
        all_rows.push((id, value));
        Ok(())
    })?;

    eprintln!("Total rows: {}", all_rows.len());
    eprintln!("Expected: {}", num_threads * inserts_per_thread);

    // Check for duplicates by value
    let mut value_counts = std::collections::HashMap::new();
    for (_id, value) in &all_rows {
        *value_counts.entry(value.clone()).or_insert(0) += 1;
    }

    for (value, count) in value_counts.iter() {
        if *count > 1 {
            eprintln!("Duplicate value '{value}' appears {count} times",);
        }
    }

    let mut stmt = conn.prepare("SELECT COUNT(*) FROM t_concurrent")?;
    let mut count = 0;
    stmt.run_with_row_callback(|row| {
        count = row.get::<i64>(0)?;
        Ok(())
    })?;

    // Assertion disabled - concurrent inserts without transactions cause duplicates
    assert_eq!(count, (num_threads * inserts_per_thread) as i64);
    eprintln!("Test disabled - would need BEGIN CONCURRENT for proper concurrent testing");
    Ok(())
}

// Regression test: statement-level rollback (from FK constraint violation)
// must clean up tx.write_set so that commit validation doesn't find stale
// entries pointing to pre-existing committed versions and panic with
// "there is another row insterted and not updated/deleted from before".
#[turso_macros::test]
fn test_stmt_rollback_cleans_write_set(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();

    // Enable MVCC — this test is MVCC-specific (uses BEGIN CONCURRENT)
    let conn = tmp_db.connect_limbo();
    conn.pragma_update("journal_mode", "'experimental_mvcc'")?;

    // Setup: parent/child tables with FK constraint
    conn.execute("PRAGMA foreign_keys = ON")?;
    conn.execute("CREATE TABLE parent(id INTEGER PRIMARY KEY)")?;
    conn.execute(
        "CREATE TABLE child(id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent(id))",
    )?;
    conn.execute("INSERT INTO parent VALUES (1)")?;
    conn.execute("INSERT INTO child VALUES (1, 1)")?;

    // Open a concurrent transaction on a second connection
    let conn2 = tmp_db.connect_limbo();
    conn2.execute("PRAGMA foreign_keys = ON")?;
    conn2.execute("BEGIN CONCURRENT")?;

    // DELETE from parent fails due to FK constraint. This triggers
    // statement-level rollback which undoes the MVCC version changes,
    // but before the fix, rollback_first_savepoint didn't clean up
    // tx.write_set — leaving stale entries.
    let result = conn2.execute("DELETE FROM parent WHERE id = 1");
    assert!(result.is_err(), "DELETE should fail due to FK constraint");

    // COMMIT should succeed. Before the fix this panicked at commit
    // validation because the stale write_set entry pointed to a
    // pre-existing committed version.
    conn2.execute("COMMIT")?;

    Ok(())
}

// Same as test_stmt_rollback_cleans_write_set but with an index on the
// child table, exercising the index version rollback path as well.
#[turso_macros::test]
fn test_stmt_rollback_cleans_write_set_with_index(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();

    let conn = tmp_db.connect_limbo();
    conn.pragma_update("journal_mode", "'experimental_mvcc'")?;

    conn.execute("PRAGMA foreign_keys = ON")?;
    conn.execute("CREATE TABLE parent(id INTEGER PRIMARY KEY)")?;
    conn.execute(
        "CREATE TABLE child(id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent(id))",
    )?;
    conn.execute("CREATE INDEX idx_child_parent ON child(parent_id)")?;
    conn.execute("INSERT INTO parent VALUES (1)")?;
    conn.execute("INSERT INTO child VALUES (1, 1)")?;

    let conn2 = tmp_db.connect_limbo();
    conn2.execute("PRAGMA foreign_keys = ON")?;
    conn2.execute("BEGIN CONCURRENT")?;

    // DELETE from parent fails due to FK constraint. With an index on
    // child(parent_id), the rollback must also undo index version changes.
    let result = conn2.execute("DELETE FROM parent WHERE id = 1");
    assert!(result.is_err(), "DELETE should fail due to FK constraint");

    conn2.execute("COMMIT")?;
    Ok(())
}

/// Verify that regular (non-materialized) views can be created, queried,
/// dropped, and recreated in MVCC mode.
#[turso_macros::test(mvcc)]
fn test_mvcc_create_and_query_view(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();
    conn.execute("CREATE TABLE t(x INTEGER, y TEXT)")?;
    conn.execute("INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c')")?;

    // Create and query a view in MVCC mode
    conn.execute("CREATE VIEW v AS SELECT x, y FROM t WHERE x > 1")?;
    let rows: Vec<(i64, String)> = conn.exec_rows("SELECT * FROM v ORDER BY x");
    assert_eq!(rows, vec![(2, "b".to_string()), (3, "c".to_string())]);

    // Drop and recreate with a different definition
    conn.execute("DROP VIEW v")?;
    conn.execute("CREATE VIEW v AS SELECT x FROM t")?;
    let rows: Vec<(i64,)> = conn.exec_rows("SELECT * FROM v ORDER BY x");
    assert_eq!(rows, vec![(1,), (2,), (3,)]);

    Ok(())
}

/// Verify a view created on one MVCC connection is visible from another.
#[turso_macros::test(mvcc)]
fn test_mvcc_view_visible_across_connections(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn1 = tmp_db.connect_limbo();
    conn1.execute("CREATE TABLE t(x INTEGER)")?;
    conn1.execute("INSERT INTO t VALUES (1), (2), (3)")?;
    conn1.execute("CREATE VIEW v AS SELECT * FROM t WHERE x > 1")?;

    // A second connection should see the view
    let conn2 = tmp_db.connect_limbo();
    let rows: Vec<(i64,)> = conn2.exec_rows("SELECT * FROM v ORDER BY x");
    assert_eq!(rows, vec![(2,), (3,)]);

    Ok(())
}

/// Verify that a view reflects DML changes made to its underlying table,
/// even when both the view and the writes happen in MVCC mode.
#[turso_macros::test(mvcc)]
fn test_mvcc_view_reflects_table_changes(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();
    conn.execute("CREATE TABLE t(x INTEGER)")?;
    conn.execute("INSERT INTO t VALUES (1), (2)")?;
    conn.execute("CREATE VIEW v AS SELECT * FROM t")?;

    // View should reflect the initial data
    let rows: Vec<(i64,)> = conn.exec_rows("SELECT * FROM v ORDER BY x");
    assert_eq!(rows, vec![(1,), (2,)]);

    // Insert more data into the underlying table
    conn.execute("INSERT INTO t VALUES (3)")?;

    // View should now include the new row
    let rows: Vec<(i64,)> = conn.exec_rows("SELECT * FROM v ORDER BY x");
    assert_eq!(rows, vec![(1,), (2,), (3,)]);

    Ok(())
}

/// Regression test: upgrading an existing MVCC transaction from read->write must not
/// leak an extra blocking-checkpoint read lock.
///
/// Before the fix, this sequence left `blocking_checkpoint_lock` with a stale reader:
/// BEGIN (deferred) -> read statement (starts tx) -> write statement (upgrade) -> COMMIT.
/// A following checkpoint then failed with `database is locked`.
#[turso_macros::test(mvcc)]
fn test_mvcc_read_to_write_upgrade_does_not_block_checkpoint(
    tmp_db: TempDatabase,
) -> anyhow::Result<()> {
    let conn = tmp_db.connect_limbo();
    conn.execute("CREATE TABLE t(x INTEGER)")?;

    conn.execute("BEGIN")?;
    let rows: Vec<(i64,)> = conn.exec_rows("SELECT COUNT(*) FROM t");
    assert_eq!(rows, vec![(0,)]);
    conn.execute("INSERT INTO t VALUES (1)")?;
    conn.execute("COMMIT")?;

    let conn2 = tmp_db.connect_limbo();
    conn2.execute("PRAGMA wal_checkpoint(TRUNCATE)")?;

    Ok(())
}
