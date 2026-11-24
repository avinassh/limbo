use turso_sync_engine::Result;

use crate::harness::Remote;

/// The remote turns a generated column into a regular column. Rows then hold a
/// value for that column.
#[test]
fn turn_generated_column_into_regular_column() -> Result<()> {
    let remote = Remote::new(&[
        "CREATE TABLE t (id TEXT PRIMARY KEY, a INT, g INT AS (a + 1), b INT)",
        "INSERT INTO t (id, a, b) VALUES ('x', 10, 20)",
    ])?;
    let replica = remote.bootstrap_replica()?;

    remote.execute_transaction(&["ALTER TABLE t ALTER COLUMN g TO g INT"])?;
    remote.execute_transaction(&["INSERT INTO t VALUES ('y', 1, 2, 3)"])?;
    replica.pull()?;

    assert_eq!(replica.snapshot()?, remote.snapshot()?);
    Ok(())
}

/// The remote turns a regular column into a generated column. Rows then hold
/// no value for that column, so the values of later columns move.
#[test]
fn turn_regular_column_into_generated_column() -> Result<()> {
    let remote = Remote::new(&[
        "CREATE TABLE t (id TEXT PRIMARY KEY, a TEXT, b TEXT, c TEXT)",
        "INSERT INTO t VALUES ('x', 'alpha', 'beta', 'gamma')",
    ])?;
    let replica = remote.bootstrap_replica()?;

    remote.execute_transaction(&["ALTER TABLE t ALTER COLUMN b TO b TEXT AS (upper(a))"])?;
    remote.execute_transaction(&["INSERT INTO t (id, a, c) VALUES ('y', 'delta', 'epsilon')"])?;
    replica.pull()?;

    assert_eq!(replica.snapshot()?, remote.snapshot()?);
    Ok(())
}

/// The remote reverses the dependency between two generated columns in one
/// transaction.
#[test]
fn reverse_dependency_between_generated_columns() -> Result<()> {
    let remote = Remote::new(&[
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a INT, v INT AS (a + 1), w INT AS (v * 10))",
        "INSERT INTO t (id, a) VALUES (1, 5)",
    ])?;
    let replica = remote.bootstrap_replica()?;

    remote.execute_transaction(&[
        "ALTER TABLE t ALTER COLUMN w TO w INT AS (a * 10)",
        "ALTER TABLE t ALTER COLUMN v TO v INT AS (w + 1)",
    ])?;
    replica.pull()?;

    assert_eq!(replica.snapshot()?, remote.snapshot()?);
    Ok(())
}

/// The remote drops its last generated column and adds it again with another
/// expression. The server writes the table text again, without the quotes that
/// the table name does not need. The replica must store the same text.
#[test]
fn redefine_last_generated_column_of_table_with_quoted_name() -> Result<()> {
    let remote = Remote::new(&[
        "CREATE TABLE \"muggles\" (id TEXT PRIMARY KEY, surname VARCHAR(40), tag TEXT AS (LOWER (surname)))",
        "INSERT INTO \"muggles\" (id, surname) VALUES ('a', 'Smith')",
    ])?;
    let replica = remote.bootstrap_replica()?;

    remote.execute_transaction(&[
        "ALTER TABLE muggles DROP COLUMN tag",
        "ALTER TABLE muggles ADD COLUMN tag TEXT AS (UPPER (surname))",
    ])?;
    replica.pull()?;

    assert_eq!(replica.snapshot()?, remote.snapshot()?);
    Ok(())
}
