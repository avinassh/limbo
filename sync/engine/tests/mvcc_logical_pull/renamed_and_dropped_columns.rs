use turso_sync_engine::Result;

use crate::harness::Remote;

/// The remote drops a column and gives its name to a later column, in one
/// transaction.
#[test]
fn drop_column_and_give_its_name_to_later_column() -> Result<()> {
    let remote = Remote::new(&[
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT, b TEXT, c TEXT)",
        "INSERT INTO t VALUES (1, 'a1', 'b1', 'c1')",
    ])?;
    let replica = remote.bootstrap_replica()?;

    remote.execute_transaction(&[
        "ALTER TABLE t DROP COLUMN a",
        "ALTER TABLE t RENAME COLUMN c TO a",
    ])?;
    replica.pull()?;

    assert_eq!(replica.snapshot()?, remote.snapshot()?);
    Ok(())
}

/// The remote moves a column to the end of the table, and a generated column
/// then reads it, in one transaction.
#[test]
fn move_column_that_generated_column_then_reads() -> Result<()> {
    let remote = Remote::new(&[
        "CREATE TABLE t (id TEXT PRIMARY KEY, x TEXT, y TEXT, g TEXT AS (y))",
        "INSERT INTO t (id, x, y) VALUES ('k', 'x1', 'y1')",
    ])?;
    let replica = remote.bootstrap_replica()?;

    remote.execute_transaction(&[
        "ALTER TABLE t DROP COLUMN x",
        "ALTER TABLE t ADD COLUMN x TEXT",
        "ALTER TABLE t ALTER COLUMN g TO g TEXT AS (x)",
    ])?;
    replica.pull()?;

    assert_eq!(replica.snapshot()?, remote.snapshot()?);
    Ok(())
}
