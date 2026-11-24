use turso_sync_engine::Result;

use crate::harness::Remote;

/// A table that only the remote changes.
const REMOTE_TABLE: &str = "CREATE TABLE other (x)";

/// A pull without remote changes returns early, so each test pulls this change.
const REMOTE_CHANGE: &str = "INSERT INTO other VALUES (1)";

/// Local changes drop a generated column and the column that it read. Then
/// they add the generated column again with another expression.
#[test]
fn pull_with_local_redefinition_of_generated_column() -> Result<()> {
    assert_pull_keeps_local_changes(
        &[REMOTE_TABLE],
        &[
            "CREATE TABLE t (id INTEGER PRIMARY KEY, a INT, g INT AS (a + 1))",
            "INSERT INTO t (id, a) VALUES (1, 10)",
            "ALTER TABLE t DROP COLUMN g",
            "ALTER TABLE t DROP COLUMN a",
            "ALTER TABLE t ADD COLUMN g INT AS (id + 1)",
        ],
    )
}

/// A local DROP COLUMN on a table that came from the remote.
#[test]
fn pull_with_local_drop_column() -> Result<()> {
    assert_pull_keeps_local_changes(
        &[
            REMOTE_TABLE,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT, c TEXT)",
            "INSERT INTO t VALUES (1, 'a1', 'c1')",
        ],
        &["ALTER TABLE t DROP COLUMN c"],
    )
}

/// A local table gets a generated column and then a regular column. The pull
/// must keep the order of the columns.
#[test]
fn pull_with_local_generated_column_before_regular_column() -> Result<()> {
    assert_pull_keeps_local_changes(
        &[REMOTE_TABLE],
        &[
            "CREATE TABLE t (id TEXT PRIMARY KEY, a TEXT)",
            "ALTER TABLE t ADD COLUMN g TEXT AS (upper(a))",
            "ALTER TABLE t ADD COLUMN b TEXT",
            "INSERT INTO t (id, a, b) VALUES ('x', 'alpha', 'beta')",
        ],
    )
}

/// Bootstraps a replica of a remote that ran `remote_setup`, and runs `local`
/// on the replica without a push. Then the replica pulls a remote change. The
/// replica must hold the remote change and keep every local change.
fn assert_pull_keeps_local_changes(remote_setup: &[&str], local: &[&str]) -> Result<()> {
    let remote = Remote::new(remote_setup)?;
    let replica = remote.bootstrap_replica()?;
    for sql in local {
        replica.execute(sql)?;
    }

    remote.execute_transaction(&[REMOTE_CHANGE])?;
    replica.pull()?;

    let expected = remote_setup
        .iter()
        .chain([REMOTE_CHANGE].iter())
        .chain(local.iter())
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(replica.snapshot()?, Remote::new(&expected)?.snapshot()?);
    Ok(())
}
