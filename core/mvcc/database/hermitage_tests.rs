use crate::mvcc::database::tests::MvccTestDb;
use crate::mvcc::database::{tests, MVTableId, Row, RowID, RowKey, TxID};
use crate::types::ImmutableRecord;
use crate::LimboError;
use crate::{Value, ValueRef};

//        /\
//       /  \
//      /    \
//     /______\
//     | .  . |    Hermitage
//     |  __  |    Transaction Isolation Tests
//     |_|  |_|
//
// These tests are adapted from https://github.com/ept/hermitage
// Turso MVCC implements snapshot isolation with eager write-write conflict detection:
//
//   - Snapshot is taken at BEGIN (not at first read like FoundationDB)
//   - Write-write conflicts are detected immediately at write time (WriteWriteConflict),
//     NOT deferred to commit (like FoundationDB)
//   - Transactions never see uncommitted changes from other active transactions (no dirty reads)
//   - Isolation level: snapshot isolation (prevents G0, G1a, G1b, G1c, OTV, PMP, P4, G-single)
//   - Does NOT prevent G2-item (write skew) or G2 (anti-dependency cycles) — those require serializable
//
// Comparison with hermitage reference databases:
//   FoundationDB (serializable): writes succeed locally, conflict checked at commit time.
//   Turso: fails eagerly at write time (WriteWriteConflict), no blocking.
//   FoundationDB also prevents G2-item and G2 (serializable) → Turso does not (snapshot isolation).
//   Postgres behavior varies by isolation level — see individual test comments.
//

// All hermitage tests have a single table, so we use the fixed -1 as table ID
const SINGLE_MV_TABLE_ID: MVTableId = MVTableId(-2);

fn setup_hermitage_test() -> MvccTestDb {
    let db = MvccTestDb::new();

    // Setup: create table test (id int primary key, value int);
    // insert into test (id, value) values (1, 10), (2, 20);
    let tx_setup = db
        .mvcc_store
        .begin_tx(db.conn.pager.load().clone())
        .unwrap();
    let row1 = generate_row(1, 10);
    let row2 = generate_row(2, 20);
    db.mvcc_store.insert(tx_setup, row1).unwrap();
    db.mvcc_store.insert(tx_setup, row2).unwrap();
    tests::commit_tx(db.mvcc_store.clone(), &db.conn, tx_setup).unwrap();

    db
}

/// Verify the final committed state of rows in the database.
/// Takes a list of (row_key, expected_value) pairs.
fn verify_final_state(db: &MvccTestDb, expected: &[(i64, i64)]) {
    let conn = db.db.connect().unwrap();
    let tx = db.mvcc_store.begin_tx(conn.pager.load().clone()).unwrap();
    for &(key, expected_value) in expected {
        assert_eq!(read_value(db, tx, key), expected_value);
    }
    tests::commit_tx(db.mvcc_store.clone(), &conn, tx).unwrap();
}

fn generate_row_with_table_id(table_id: MVTableId, id: i64, value: i64) -> Row {
    let record = ImmutableRecord::from_values(&[Value::Integer(value)], 1);
    Row::new_table_row(
        RowID::new(table_id, RowKey::Int(id)),
        record.as_blob().to_vec(),
        1,
    )
}

fn generate_row(id: i64, value: i64) -> Row {
    generate_row_with_table_id(SINGLE_MV_TABLE_ID, id, value)
}

fn get_value_from_row(row: &Row) -> i64 {
    let mut record = ImmutableRecord::new(1024);
    record.start_serialization(row.data.as_ref().unwrap());
    match record.get_value(0).unwrap() {
        ValueRef::Integer(val) => val,
        _ => panic!("Expected integer value"),
    }
}

fn read_value(db: &MvccTestDb, tx: TxID, key: i64) -> i64 {
    let row = db
        .mvcc_store
        .read(tx, RowID::new(SINGLE_MV_TABLE_ID, RowKey::Int(key)))
        .unwrap()
        .unwrap();
    get_value_from_row(&row)
}

/// G0: Write Cycles prevented - https://jepsen.io/consistency/phenomena/g0
/// if two transactions try to update the same row, one should fail with a write-write conflict,
/// preventing a cycle of uncommitted updates that could lead to non-serializable behavior.
///
/// Turso: T2's write to the same row as T1 fails immediately with WriteWriteConflict.
/// Postgres read committed: T2 BLOCKS until T1 commits, then T2 overwrites T1. Both succeed.
/// FoundationDB: T2 writes locally, T2's commit is rejected.
#[test]
fn test_hermitage_g0_write_cycles_prevented() {
    let db = setup_hermitage_test();

    // T1 and T2 try to update the same row - second should fail
    let conn1 = db.db.connect().unwrap();
    let conn2 = db.db.connect().unwrap();

    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();

    // T1: update test set value = 11 where id = 1
    let row1_t1 = generate_row(1, 11);
    db.mvcc_store.update(tx1, row1_t1).unwrap();

    // T2: update test set value = 12 where id = 1 -- should fail with write-write conflict
    let row1_t2 = generate_row(1, 12);
    let result = db.mvcc_store.update(tx2, row1_t2);
    assert!(matches!(result, Err(LimboError::WriteWriteConflict)));

    // T2: rollback (failed with write-write conflict)
    db.mvcc_store
        .rollback_tx(tx2, conn2.pager.load().clone(), &conn2);

    // T1: update test set value = 21 where id = 2
    let row2_t1 = generate_row(2, 21);
    db.mvcc_store.update(tx1, row2_t1).unwrap();

    // T1: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn1, tx1).unwrap();

    {
        // T1: select * from test -- Shows 1 => 11, 2 => 21
        // Use a new connection for the read to avoid transaction conflicts
        let conn_read = db.db.connect().unwrap();
        let tx1_read = db
            .mvcc_store
            .begin_tx(conn_read.pager.load().clone())
            .unwrap();
        assert_eq!(read_value(&db, tx1_read, 1), 11);
        assert_eq!(read_value(&db, tx1_read, 2), 21);
    }
}

/// G1a: Aborted Reads - https://jepsen.io/consistency/phenomena/g1a
/// if a transaction T1 updates a row but then aborts, another transaction T2 should never see
/// T1's uncommitted changes, even if T2 reads the same row before T1 aborts.
///
/// Since we don't allow reading uncommited data ever, we prevent g1a and also dirty update phenomena
/// (mentioned in the jepsen page)
#[test]
fn test_hermitage_g1a_aborted_reads() {
    // Setup: create table test (id int primary key, value int);
    // insert into test (id, value) values (1, 10), (2, 20);
    let db = setup_hermitage_test();

    let conn1 = db.db.connect().unwrap();
    let conn2 = db.db.connect().unwrap();

    // begin; set transaction isolation level read committed; -- T1
    // begin; set transaction isolation level read committed; -- T2
    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();

    // T1: update test set value = 101 where id = 1
    let row1_updated = generate_row(1, 101);
    db.mvcc_store.update(tx1, row1_updated).unwrap();

    // T2: select * from test -- Should still show 1 => 10 (original value)
    assert_eq!(read_value(&db, tx2, 1), 10); // Should see original value, not 101
    assert_eq!(read_value(&db, tx2, 2), 20);

    // T1: abort/rollback
    db.mvcc_store
        .rollback_tx(tx1, conn1.pager.load().clone(), &conn1);

    // T2: select * from test -- Should still show 1 => 10 (original value)
    assert_eq!(read_value(&db, tx2, 1), 10); // Still should see original value
    assert_eq!(read_value(&db, tx2, 2), 20);

    // T2: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn2, tx2).unwrap();

    // Verify final state - should still have original values since T1 aborted
    verify_final_state(&db, &[(1, 10), (2, 20)]);
}

/// G1b: Intermediate Reads - https://jepsen.io/consistency/phenomena/g1b
/// If a transaction T1 writes a value, then overwrites it with a second value, another committed
/// transaction T2 should never see T1's first (intermediate) write — only the final committed value
/// or the original value from before T1 (depending on when T2's snapshot is taken).
///
/// Under snapshot isolation, T2 sees the value from its snapshot, not T1's intermediate or final write.
///
/// G1b: Intermediate Update - this is not present in hermitage but Jepsen has it:
///
/// From Jepsen: "If writes are arbitrary transformations of values (rather than blind writes),
/// this definition of G1b is insufficient. Imagine an aborted transaction Ti writes an
/// intermediate version xi, and a committed transaction Tj writes xj = f(xi). Finally, a
/// committed transaction Tk reads xj. Clearly, intermediate state has leaked from Ti into Tk."
///
/// Turso never allows reading uncommitted data, so Tj would never be able to read xi from Ti, and
/// thus Tk would never see xj = f(xi) if Ti aborted. So we prevent this form of G1b as well.
#[test]
fn test_hermitage_g1b_intermediate_reads() {
    // Setup: create table test (id int primary key, value int);
    // insert into test (id, value) values (1, 10), (2, 20);
    let db = setup_hermitage_test();

    let conn1 = db.db.connect().unwrap();
    let conn2 = db.db.connect().unwrap();

    // begin; set transaction isolation level read committed; -- T1
    // begin; set transaction isolation level read committed; -- T2
    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();

    // T1: update test set value = 101 where id = 1 (intermediate value)
    let row1_intermediate = generate_row(1, 101);
    db.mvcc_store.update(tx1, row1_intermediate).unwrap();

    // T2: select * from test -- Should still show 1 => 10 (original value)
    assert_eq!(read_value(&db, tx2, 1), 10); // Should NOT see 101

    // T1: update test set value = 11 where id = 1 (final value)
    let row1_final = generate_row(1, 11);
    db.mvcc_store.update(tx1, row1_final).unwrap();

    // T1: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn1, tx1).unwrap();

    // T2: select * from test
    // Turso (snapshot isolation): T2 still sees 10 from its snapshot.
    // FoundationDB: same — T2 still sees 10.
    // Postgres read committed: T2 sees 11 (T1's committed final value).
    assert_eq!(read_value(&db, tx2, 1), 10);

    // T2: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn2, tx2).unwrap();

    // Verify final state - should show final committed value of 11
    verify_final_state(&db, &[(1, 11)]);
}

/// G1c: Circular Information Flow - https://jepsen.io/consistency/phenomena/g1c
/// if two transactions T1 and T2 both update different rows but then read each other's updated
/// rows before either commits, they should not see each other's uncommitted changes,
/// preventing a cycle of intermediate reads.
#[test]
fn test_hermitage_g1c_circular_information_flow() {
    // Setup: create table test (id int primary key, value int);
    // insert into test (id, value) values (1, 10), (2, 20);
    let db = setup_hermitage_test();

    let conn1 = db.db.connect().unwrap();
    let conn2 = db.db.connect().unwrap();

    // begin; set transaction isolation level read committed; -- T1
    // begin; set transaction isolation level read committed; -- T2
    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();

    // T1: update test set value = 11 where id = 1
    let row1_updated = generate_row(1, 11);
    db.mvcc_store.update(tx1, row1_updated).unwrap();

    // T2: update test set value = 22 where id = 2
    let row2_updated = generate_row(2, 22);
    db.mvcc_store.update(tx2, row2_updated).unwrap();

    // T1: select * from test where id = 2
    // Should still show 2 => 20 (original value), NOT T2's uncommitted 22
    assert_eq!(read_value(&db, tx1, 2), 20); // Should NOT see T2's update

    // T2: select * from test where id = 1
    // Should still show 1 => 10 (original value), NOT T1's uncommitted 11
    assert_eq!(read_value(&db, tx2, 1), 10); // Should NOT see T1's update

    // T1: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn1, tx1).unwrap();

    // T2: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn2, tx2).unwrap();

    // Verify final state - both updates should be committed
    verify_final_state(&db, &[(1, 11), (2, 22)]);
}

/// OTV: Observed Transaction Vanishes
/// if a transaction T1 updates a row but has not yet committed, and another transaction T2 tries
/// to update the same row and fails with a write-write conflict, then if a third transaction
/// T3 tries to read that row, it should not see T1's uncommitted changes "vanish" due to T2's
/// failed update. Instead, T3 should see the original value (since we use snapshot isolation)
///
/// IOW once a transaction's effects become visible to another transaction, they must not "vanish".
///
/// Turso-specific behavior:
///   - T2's write to the same row as T1 fails immediately with WriteWriteConflict (no blocking)
///   - T3's snapshot is taken at BEGIN (before T1 commits), so T3 sees original values throughout
///   - After T2 fails, we start a fresh T2 transaction that succeeds
///
/// Contrast with hermitage references:
///   - Postgres read committed: T2 BLOCKS until T1 commits, then succeeds. T3 sees committed values
///     change (i.e. first it sees T1's changes, then after T2 commits it sees T2's changes).
///   - FoundationDB: T2 writes locally, snapshot at first read (T3 sees T1's values), T2 commit rejected
///   - Turso: T2 fails eagerly, T3 never sees T1's changes (snapshot predates T1's commit)
#[test]
fn test_hermitage_otv_observed_transaction_vanishes() {
    // T3 should not see T1's changes "vanish"
    // Setup: create table test (id int primary key, value int);
    // insert into test (id, value) values (1, 10), (2, 20);
    let db = setup_hermitage_test();

    let conn1 = db.db.connect().unwrap();
    let conn2 = db.db.connect().unwrap();
    let conn3 = db.db.connect().unwrap();

    // begin; set transaction isolation level read committed; -- T1, T2, T3
    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();
    let tx3 = db.mvcc_store.begin_tx(conn3.pager.load().clone()).unwrap();

    // T1: update test set value = 11 where id = 1
    let row1_t1 = generate_row(1, 11);
    db.mvcc_store.update(tx1, row1_t1).unwrap();

    // T1: update test set value = 19 where id = 2
    let row2_t1 = generate_row(2, 19);
    db.mvcc_store.update(tx1, row2_t1).unwrap();

    // T2: update test set value = 12 where id = 1
    // (following blocks in Postgres)
    let row1_t2 = generate_row(1, 12);
    let result = db.mvcc_store.update(tx2, row1_t2.clone());
    assert!(matches!(result, Err(LimboError::WriteWriteConflict)));
    // Since T2 failed, rollback T2
    db.mvcc_store
        .rollback_tx(tx2, conn2.pager.load().clone(), &conn2);

    // T1: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn1, tx1).unwrap();

    // T3: select * from test where id = 1
    // With snapshot isolation, T3 sees the original value (10)
    // (with read committed, like in postgres, T3 would see T1's committed value (11))
    assert_eq!(read_value(&db, tx3, 1), 10); // Snapshot isolation

    // Start a new T2 transaction after T1 commits
    let conn2_new = db.db.connect().unwrap();
    let tx2_new = db
        .mvcc_store
        .begin_tx(conn2_new.pager.load().clone())
        .unwrap();

    // T2_new: update test set value = 12 where id = 1 (now succeeds)
    db.mvcc_store.update(tx2_new, row1_t2).unwrap();

    // T2_new: update test set value = 18 where id = 2
    let row2_t2 = generate_row(2, 18);
    db.mvcc_store.update(tx2_new, row2_t2).unwrap();

    // T3: select * from test where id = 2
    // Should still see original value with snapshot isolation
    assert_eq!(read_value(&db, tx3, 2), 20); // Snapshot isolation

    // T2_new: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn2_new, tx2_new).unwrap();

    // T3: select * from test where id = 2
    // Still sees snapshot from T3's start
    assert_eq!(read_value(&db, tx3, 2), 20); // Consistent snapshot

    // T3: select * from test where id = 1
    // Still sees snapshot from T3's start
    assert_eq!(read_value(&db, tx3, 1), 10); // Consistent snapshot

    // T3: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn3, tx3).unwrap();

    // Verify final state - T2_new's values are committed
    verify_final_state(&db, &[(1, 12), (2, 18)]);
}

/// PMP for read predicates:
/// Related to P3 (Phantom): https://jepsen.io/consistency/phenomena/p3
/// If a transaction T1 reads rows matching a predicate, and another transaction T2 inserts a new
/// row that matches T1's predicate and commits, then if T1 tries to read again with the same
/// predicate, it should not see the new row (phantom prevention).
#[test]
fn test_hermitage_pmp_predicate_many_preceders_read() {
    // T1 should not see T2's inserted row that matches T1's predicate
    // Setup: create table test (id int primary key, value int);
    // insert into test (id, value) values (1, 10), (2, 20);
    let db = setup_hermitage_test();

    let conn1 = db.db.connect().unwrap();
    let conn2 = db.db.connect().unwrap();

    // begin; set transaction isolation level repeatable read; -- T1
    // begin; set transaction isolation level repeatable read; -- T2
    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();

    // T1: select * from test where value = 30 -- Returns nothing
    // Since we cannot use raw SQL here, lettuce simulate predicate scan - check all rows for value = 30
    let mut found_30 = false;
    for row_id in 1..=2 {
        if let Some(row) = db
            .mvcc_store
            .read(tx1, RowID::new(SINGLE_MV_TABLE_ID, RowKey::Int(row_id)))
            .unwrap()
        {
            if get_value_from_row(&row) == 30 {
                found_30 = true;
            }
        }
    }
    assert!(!found_30); // Should find nothing

    // T2: insert into test (id, value) values(3, 30)
    let row3 = generate_row(3, 30);
    db.mvcc_store.insert(tx2, row3).unwrap();

    // T2: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn2, tx2).unwrap();

    // T1: select * from test where value % 3 = 0
    // This would match both value=30 (newly inserted) and any existing divisible by 3
    // With repeatable read/snapshot isolation, T1 should not see the new row
    let mut found_divisible_by_3 = Vec::new();
    for row_id in 1..=3 {
        if let Some(row) = db
            .mvcc_store
            .read(tx1, RowID::new(SINGLE_MV_TABLE_ID, RowKey::Int(row_id)))
            .unwrap()
        {
            let value = get_value_from_row(&row);
            if value % 3 == 0 {
                found_divisible_by_3.push(value);
            }
        }
    }
    assert!(found_divisible_by_3.is_empty()); // Should still find nothing (phantom prevention)

    // T1: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn1, tx1).unwrap();

    // Verify final state - the new row exists
    verify_final_state(&db, &[(3, 30)]);
}

/// PMP for write predicates
/// Related to P3 (Phantom): https://jepsen.io/consistency/phenomena/p3
/// if a transaction T1 updates rows matching a predicate, and another transaction T2 tries to
/// delete rows matching the same predicate, then T2 should fail with a write-write conflict,
/// even if T1 has not committed yet. This prevents lost updates and ensures that T2
/// does not delete rows that T1 is in the process of updating.
///
/// Turso: T2's delete fails immediately with WriteWriteConflict (T1 has uncommitted write on row 2).
/// Postgres repeatable read: T2's delete BLOCKS until T1 commits, then fails with serialization error.
/// FoundationDB: T2's delete succeeds locally, T2's commit is rejected.
#[test]
fn test_hermitage_pmp_predicate_many_preceders_write() {
    // T2's delete based on predicate conflicts with T1's update
    // Setup: create table test (id int primary key, value int);
    // insert into test (id, value) values (1, 10), (2, 20);
    let db = setup_hermitage_test();

    let conn1 = db.db.connect().unwrap();
    let conn2 = db.db.connect().unwrap();

    // begin; set transaction isolation level repeatable read; -- T1
    // begin; set transaction isolation level repeatable read; -- T2
    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();

    // T1: update test set value = value + 10
    // Updates: 1 => 20, 2 => 30
    let row1_updated = generate_row(1, 20);
    let row2_updated = generate_row(2, 30);
    db.mvcc_store.update(tx1, row1_updated).unwrap();
    db.mvcc_store.update(tx1, row2_updated).unwrap();

    // T2: select * from test where value = 20
    // T2 should see the original values (snapshot isolation)
    assert_eq!(read_value(&db, tx2, 2), 20); // Original value

    // T2: delete from test where value = 20
    // T2 tries to delete row 2 (which originally had value 20)
    let delete_result = db
        .mvcc_store
        .delete(tx2, RowID::new(SINGLE_MV_TABLE_ID, RowKey::Int(2)));
    assert!(matches!(delete_result, Err(LimboError::WriteWriteConflict)));

    // T1: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn1, tx1).unwrap();

    // Since T2 failed with write-write conflict, rollback T2
    db.mvcc_store
        .rollback_tx(tx2, conn2.pager.load().clone(), &conn2);

    // Verify final state - T1's updates are committed
    verify_final_state(&db, &[(1, 20), (2, 30)]);
}

/// P4: Lost Update - https://jepsen.io/consistency/phenomena/p4
/// if two transactions T1 and T2 both read the same row and then both try to update it, one of them
/// should fail with a write-write conflict, preventing the "lost update" scenario where one
/// transaction's update overwrites the other's without either being aware of the conflict.
///
/// Turso: T2's update fails immediately with WriteWriteConflict (T1 has uncommitted write).
/// Postgres repeatable read: T2's update BLOCKS until T1 commits, then fails with serialization error.
/// FoundationDB: T2's update succeeds locally, T2's commit is rejected.
#[test]
fn test_hermitage_p4_lost_update() {
    // T2's update should not be lost when T1 also updates
    // Setup: create table test (id int primary key, value int);
    // insert into test (id, value) values (1, 10), (2, 20);
    let db = setup_hermitage_test();

    let conn1 = db.db.connect().unwrap();
    let conn2 = db.db.connect().unwrap();

    // begin; set transaction isolation level repeatable read; -- T1
    // begin; set transaction isolation level repeatable read; -- T2
    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();

    // T1: select * from test where id = 1
    assert_eq!(read_value(&db, tx1, 1), 10);

    // T2: select * from test where id = 1
    assert_eq!(read_value(&db, tx2, 1), 10);

    // T1: update test set value = 11 where id = 1
    let row1_updated = generate_row(1, 11);
    db.mvcc_store.update(tx1, row1_updated.clone()).unwrap();

    // T2: update test set value = 11 where id = 1
    let result = db.mvcc_store.update(tx2, row1_updated);
    assert!(matches!(result, Err(LimboError::WriteWriteConflict)));

    // T1: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn1, tx1).unwrap();

    // T2: abort (already failed, so rollback)
    db.mvcc_store
        .rollback_tx(tx2, conn2.pager.load().clone(), &conn2);

    // Verify final state - only T1's update is committed
    verify_final_state(&db, &[(1, 11)]);
}

/// G-single: Read Skew - https://jepsen.io/consistency/phenomena/g-single
/// if a transaction T1 reads a row and then another transaction T2 updates that row and commits,
/// then if T1 tries to read the same row again, it should still see the original value
/// but not an inconsistent state where T1 sees the updated value for some columns but not others.
#[test]
fn test_hermitage_g_single_read_skew() {
    // T1 should see a consistent snapshot
    // Setup: create table test (id int primary key, value int);
    // insert into test (id, value) values (1, 10), (2, 20);
    let db = setup_hermitage_test();

    let conn1 = db.db.connect().unwrap();
    let conn2 = db.db.connect().unwrap();

    // begin; set transaction isolation level repeatable read; -- T1
    // begin; set transaction isolation level repeatable read; -- T2
    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();

    // T1: select * from test where id = 1 -- Shows 1 => 10
    assert_eq!(read_value(&db, tx1, 1), 10);

    // T2: select * from test where id = 1
    assert_eq!(read_value(&db, tx2, 1), 10);

    // T2: select * from test where id = 2
    assert_eq!(read_value(&db, tx2, 2), 20);

    // T2: update test set value = 12 where id = 1
    let row1_updated = generate_row(1, 12);
    db.mvcc_store.update(tx2, row1_updated).unwrap();

    // T2: update test set value = 18 where id = 2
    let row2_updated = generate_row(2, 18);
    db.mvcc_store.update(tx2, row2_updated).unwrap();

    // T2: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn2, tx2).unwrap();

    // T1: select * from test where id = 2
    // With snapshot isolation, T1 should still see the original value (20)
    // This prevents read skew - T1 sees a consistent snapshot
    assert_eq!(read_value(&db, tx1, 2), 20); // Original value, not 18
    assert_eq!(read_value(&db, tx1, 1), 10); // Original value, not 12

    // T1: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn1, tx1).unwrap();

    // Verify final state - T2's updates are committed
    verify_final_state(&db, &[(1, 12), (2, 18)]);
}

/// G-single with predicate dependencies: https://jepsen.io/consistency/phenomena/g-single
/// if a transaction T1 reads rows matching a predicate, and another transaction T2 updates those rows
/// and commits, then if T1 tries to read again with the same predicate, it should
/// still see the original values that matched the predicate, not the updated values,
/// preventing a skew where T1 sees some updated values but not others.
#[test]
fn test_hermitage_g_single_read_skew_predicate_dependencies() {
    // T1's snapshot should not see T2's predicate-changing update
    // Setup: create table test (id int primary key, value int);
    // insert into test (id, value) values (1, 10), (2, 20);
    let db = setup_hermitage_test();

    let conn1 = db.db.connect().unwrap();
    let conn2 = db.db.connect().unwrap();

    // begin; set transaction isolation level repeatable read; -- T1
    // begin; set transaction isolation level repeatable read; -- T2
    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();

    // T1: select * from test where value % 5 = 0
    // Both 10 and 20 are divisible by 5, so returns both rows
    let mut t1_divisible_by_5 = Vec::new();
    for row_id in 1..=2 {
        if let Some(row) = db
            .mvcc_store
            .read(tx1, RowID::new(SINGLE_MV_TABLE_ID, RowKey::Int(row_id)))
            .unwrap()
        {
            let value = get_value_from_row(&row);
            if value % 5 == 0 {
                t1_divisible_by_5.push(value);
            }
        }
    }
    assert_eq!(t1_divisible_by_5, vec![10, 20]); // Both match

    // T2: update test set value = 12 where value = 10
    let row1_updated = generate_row(1, 12);
    db.mvcc_store.update(tx2, row1_updated).unwrap();

    // T2: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn2, tx2).unwrap();

    // T1: select * from test where value % 3 = 0
    // With snapshot isolation, T1 still sees (1, 10) and (2, 20) from its snapshot.
    // Neither 10 nor 20 is divisible by 3, so this should return nothing.
    // (The newly committed value 12 IS divisible by 3, but T1 must not see it.)
    let mut t1_divisible_by_3 = Vec::new();
    for row_id in 1..=2 {
        if let Some(row) = db
            .mvcc_store
            .read(tx1, RowID::new(SINGLE_MV_TABLE_ID, RowKey::Int(row_id)))
            .unwrap()
        {
            let value = get_value_from_row(&row);
            if value % 3 == 0 {
                t1_divisible_by_3.push(value);
            }
        }
    }
    assert!(t1_divisible_by_3.is_empty()); // Should find nothing (snapshot isolation)

    // T1: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn1, tx1).unwrap();

    // Verify final state - T2's update is visible
    verify_final_state(&db, &[(1, 12)]);
}

/// G-single with write predicate dependencies: http://jepsen.io/consistency/phenomena/g-single
/// if a transaction T1 reads rows matching a predicate, and another transaction T2 updates those rows
/// and commits, then if T1 tries to update rows matching the same predicate, it should fail with
/// a write-write conflict. This prevents T1 from updating rows based on a stale snapshot that has
/// been changed by T2, ensuring that T1 does not overwrite T2's changes without being aware of the conflict.
///
/// Turso: T1's delete fails immediately with WriteWriteConflict (T2 already committed a write to row 2).
/// Postgres repeatable read: T1's delete fails with serialization error at commit time (T1 blocks until T2 commits, then fails).
/// FoundationDB: T1's delete succeeds locally, T1's commit is rejected.
#[test]
fn test_hermitage_g_single_read_skew_write_predicate() {
    // T1's delete based on predicate should conflict
    // Setup: create table test (id int primary key, value int);
    // insert into test (id, value) values (1, 10), (2, 20);
    let db = setup_hermitage_test();

    let conn1 = db.db.connect().unwrap();
    let conn2 = db.db.connect().unwrap();

    // begin; set transaction isolation level repeatable read; -- T1
    // begin; set transaction isolation level repeatable read; -- T2
    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();

    // T1: select * from test where id = 1 -- Shows 1 => 10
    assert_eq!(read_value(&db, tx1, 1), 10);

    // T2: select * from test
    assert_eq!(read_value(&db, tx2, 1), 10);
    assert_eq!(read_value(&db, tx2, 2), 20);

    // T2: update test set value = 12 where id = 1
    let row1_updated = generate_row(1, 12);
    db.mvcc_store.update(tx2, row1_updated).unwrap();

    // T2: update test set value = 18 where id = 2
    let row2_updated = generate_row(2, 18);
    db.mvcc_store.update(tx2, row2_updated).unwrap();

    // T2: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn2, tx2).unwrap();

    // T1: delete from test where value = 20
    // T1 sees row 2 with value 20 in its snapshot and tries to delete it
    // But T2 has already updated this row, causing a write-write conflict
    let delete_result = db
        .mvcc_store
        .delete(tx1, RowID::new(SINGLE_MV_TABLE_ID, RowKey::Int(2)));
    assert!(matches!(delete_result, Err(LimboError::WriteWriteConflict)));

    // T1: abort
    db.mvcc_store
        .rollback_tx(tx1, conn1.pager.load().clone(), &conn1);

    // Verify final state - T2's updates are preserved
    verify_final_state(&db, &[(1, 12), (2, 18)]);
}

/// G-single with interleaved operations and rollback: http://jepsen.io/consistency/phenomena/g-single
/// if a transaction T1 reads rows matching a predicate, and another transaction T2 updates those
/// rows and commits, then if T1 tries to update rows matching the same predicate, it should fail with a
/// write-write conflict. If T1 then rolls back, it should not affect T2's committed changes, and
/// the final state should reflect T2's updates without any inconsistencies.
///
/// Based on FoundationDB's g-single-write-2 test scenario with Turso-specific behavior.
/// T2 updates id=1, T1 deletes id=2, then T2 tries to update id=2.
/// Turso: T2's update to id=2 fails immediately with WriteWriteConflict (T1 has uncommitted delete).
///        Both roll back. Final state: original values (10, 20).
/// FoundationDB: T2's update succeeds, T1 rolls back, T2 commits successfully.
///        Final state: T2's values (12, 18) — because conflicts are checked at commit time.
#[test]
fn test_hermitage_g_single_read_skew_write_interleaved() {
    // Interleaved operations with rollback
    // Setup: create table test (id int primary key, value int);
    // insert into test (id, value) values (1, 10), (2, 20);
    let db = setup_hermitage_test();

    let conn1 = db.db.connect().unwrap();
    let conn2 = db.db.connect().unwrap();

    // begin; set transaction isolation level repeatable read; -- T1
    // begin; set transaction isolation level repeatable read; -- T2
    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();

    // T1: select * from test where id = 1 -- Shows 1 => 10
    assert_eq!(read_value(&db, tx1, 1), 10);

    // T2: select * from test
    assert_eq!(read_value(&db, tx2, 1), 10);
    assert_eq!(read_value(&db, tx2, 2), 20);

    // T2: update test set value = 12 where id = 1
    let row1_updated = generate_row(1, 12);
    db.mvcc_store.update(tx2, row1_updated).unwrap();

    // T1: delete from test where value = 20 -- Tries to delete row 2
    let delete_result = db
        .mvcc_store
        .delete(tx1, RowID::new(SINGLE_MV_TABLE_ID, RowKey::Int(2)));
    assert!(delete_result.is_ok()); // Should succeed since T2 hasn't updated row 2 yet

    // T2: update test set value = 18 where id = 2
    // This should fail with write-write conflict since T1 already deleted row 2
    let row2_updated = generate_row(2, 18);
    let update_result = db.mvcc_store.update(tx2, row2_updated);
    assert!(matches!(update_result, Err(LimboError::WriteWriteConflict)));

    // T1: rollback
    db.mvcc_store
        .rollback_tx(tx1, conn1.pager.load().clone(), &conn1);

    // T2: rollback
    db.mvcc_store
        .rollback_tx(tx2, conn2.pager.load().clone(), &conn2);

    // Verify final state - everything remains unchanged
    verify_final_state(&db, &[(1, 10), (2, 20)]);
}

/// G2: Write Skew -
/// if two transactions T1 and T2 both read the same rows and then both try to update different
/// rows based on that same snapshot, both should be able to commit successfully,
/// even though this can lead to an inconsistent state (write skew).
/// This is allowed under snapshot isolation but would not be allowed under serializable.
///
/// Turso (and also Postgres): Both commit successfully — no write-write conflict (different rows). Write skew ALLOWED.
/// FoundationDB: T2's commit is REJECTED — serializable prevents write skew via read-conflict tracking.
#[test]
fn test_hermitage_g2_item_write_skew() {
    // This test explicitly verifies that write skew DOES occur with snapshot isolation
    // This is the expected behavior unless serializable isolation is implemented
    let db = setup_hermitage_test();

    let conn1 = db.db.connect().unwrap();
    let conn2 = db.db.connect().unwrap();

    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();

    // Both transactions read both rows
    assert_eq!(read_value(&db, tx1, 1), 10);
    assert_eq!(read_value(&db, tx1, 2), 20);
    assert_eq!(read_value(&db, tx2, 1), 10);
    assert_eq!(read_value(&db, tx2, 2), 20);

    // Each updates a different row (no write-write conflict)
    let row1_updated = generate_row(1, 11);
    let row2_updated = generate_row(2, 21);
    db.mvcc_store.update(tx1, row1_updated).unwrap();
    db.mvcc_store.update(tx2, row2_updated).unwrap();

    // Both should successfully commit with snapshot isolation
    // (This demonstrates write skew is allowed)
    tests::commit_tx(db.mvcc_store.clone(), &conn1, tx1).unwrap();
    tests::commit_tx(db.mvcc_store.clone(), &conn2, tx2).unwrap();

    // Verify both updates succeeded (write skew occurred)
    verify_final_state(&db, &[(1, 11), (2, 21)]);
}

/// G2: Anti-Dependency Cycles (Fekete et al's two-edge example) — https://jepsen.io/consistency/phenomena/g2
/// If three transactions T1, T2, and T3 all read the same rows, and then T2 updates one of those
/// rows and commits, and then T3 reads (seeing T2's update) and commits, then if T1 tries to update
/// a different row based on its original snapshot, it should succeed under snapshot isolation (since there
/// are no write-write conflicts), even though this creates a dependency cycle
/// (T1 --rw--> T2 --wr--> T3 --rw--> T1) that would not be allowed under serializable.
///
/// T1 reads both rows, T2 updates row 2 and commits, T3 reads (sees T2's update) and commits,
/// then T1 updates row 1.
/// Turso (and Postgres repeated read): T1's update succeeds and commits — no write-write conflict (disjoint rows). G2 ALLOWED.
/// Postgres serializable: T1's update fails
/// FoundationDB: T1's commit is REJECTED (serializable, detects read-write conflict on row 2).
#[test]
fn test_hermitage_g2_two_edges_fekete() {
    // G2-two-edges: Fekete et al's example with two anti-dependency edges
    // Setup: create table test (id int primary key, value int);
    // insert into test (id, value) values (1, 10), (2, 20);
    let db = setup_hermitage_test();

    let conn1 = db.db.connect().unwrap();
    let conn2 = db.db.connect().unwrap();
    let conn3 = db.db.connect().unwrap();

    // T1: begin (snapshot isolation)
    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();

    // T1: select * from test -- Shows 1 => 10, 2 => 20
    // T1 reads row 2 (sees 20) — forms edge T1 --rw--> T2 (T2 will write row 2 later)
    assert_eq!(read_value(&db, tx1, 1), 10);
    assert_eq!(read_value(&db, tx1, 2), 20);

    // T2: begin
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();

    // T2: update test set value = value + 5 where id = 2
    // T2 writes row 2 — completes edge T1 --rw--> T2
    let row2_updated = generate_row(2, 25);
    db.mvcc_store.update(tx2, row2_updated).unwrap();

    // T2: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn2, tx2).unwrap();

    // T3: begin (after T2 committed)
    let tx3 = db.mvcc_store.begin_tx(conn3.pager.load().clone()).unwrap();

    // T3: select * from test -- sees T2's committed changes
    // T3 reads row 2 (sees 25, T2's write) — forms edge T2 --wr--> T3
    // T3 reads row 1 (sees 10) — forms edge T3 --rw--> T1 (T1 will write row 1 later)
    assert_eq!(read_value(&db, tx3, 1), 10); // unchanged
    assert_eq!(read_value(&db, tx3, 2), 25); // T2's update

    // T3: commit
    tests::commit_tx(db.mvcc_store.clone(), &conn3, tx3).unwrap();

    // T1: update test set value = 0 where id = 1
    // T1 writes row 1 — completes edge T3 --rw--> T1
    // Cycle formed: T1 --rw--> T2 --wr--> T3 --rw--> T1
    // Snapshot isolation: succeeds (no write-write conflict, T1 writes row 1, T2 wrote row 2)
    // Serializable: would fail (cycle not allowed)
    let row1_updated = generate_row(1, 0);
    db.mvcc_store.update(tx1, row1_updated).unwrap();

    // T1: commit — succeeds under snapshot isolation
    tests::commit_tx(db.mvcc_store.clone(), &conn1, tx1).unwrap();

    // Verify final state - T1's update to row 1, T2's update to row 2
    verify_final_state(&db, &[(1, 0), (2, 25)]);
}

/// G2: Anti-Dependency Cycles — https://jepsen.io/consistency/phenomena/g2
/// If two transactions T1 and T2 both read using the same predicate and find no matching rows,
/// and then each inserts a new row that would match the other's predicate,
/// both should be able to commit successfully under snapshot isolation (no write-write conflict
/// since they insert different rows), even though this creates a dependency cycle
/// (T1 --rw--> T2 --rw--> T1) that would not be allowed under serializable isolation.
///
/// Both T1 and T2 read using a predicate (value % 3 = 0) and find no matching rows. Then each
/// inserts a new row that WOULD match the other's predicate (T1 inserts 30, T2 inserts 42).
/// Turso: Both commit successfully — inserts to different row IDs have no write-write conflict. G2 ALLOWED.
/// Postgres repeatable read: Both commit successfully — same behavior (snapshot isolation allows G2).
/// FoundationDB: T2's commit is REJECTED (serializable, detects predicate read-write conflict).
#[test]
fn test_hermitage_g2_anti_dependency_cycles() {
    // This test verifies that G2 anomalies DO occur with snapshot isolation
    let db = setup_hermitage_test();

    let conn1 = db.db.connect().unwrap();
    let conn2 = db.db.connect().unwrap();

    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();

    // Both read and find no rows with value % 3 = 0
    // T1's read misses T2's future insert — forms edge T1 --rw--> T2
    // T2's read misses T1's future insert — forms edge T2 --rw--> T1
    assert_eq!(read_value(&db, tx1, 1), 10);
    assert_eq!(read_value(&db, tx1, 2), 20);
    assert_eq!(read_value(&db, tx2, 1), 10);
    assert_eq!(read_value(&db, tx2, 2), 20);

    // T1 inserts row 3 (value 30, divisible by 3) — completes edge T2 --rw--> T1
    // T2 inserts row 4 (value 42, divisible by 3) — completes edge T1 --rw--> T2
    // Cycle formed: T1 --rw--> T2 --rw--> T1
    // Snapshot isolation: succeeds (different row IDs, no write-write conflict)
    // Serializable: would fail (cycle not allowed)
    let row3 = generate_row(3, 30);
    let row4 = generate_row(4, 42);
    db.mvcc_store.insert(tx1, row3).unwrap();
    db.mvcc_store.insert(tx2, row4).unwrap();

    tests::commit_tx(db.mvcc_store.clone(), &conn1, tx1).unwrap();
    tests::commit_tx(db.mvcc_store.clone(), &conn2, tx2).unwrap();

    // Verify both inserts succeeded — cycle occurred
    verify_final_state(&db, &[(3, 30), (4, 42)]);
}

/// this is not a standard hermitage test, simpler version of G1a. Here the txn rollbacks, after that
/// the second read happens and in G1a test, the txn reads the row before the rollback
///
/// we will test that:
///    - a transaction that updates a row but then aborts does not make its changes visible to other transactions.
#[test]
fn test_hermitage_aborted_transaction_not_visible() {
    let db = MvccTestDb::new();

    // T1 inserts and commits
    let tx1 = db
        .mvcc_store
        .begin_tx(db.conn.pager.load().clone())
        .unwrap();
    let row1 = tests::generate_simple_string_row((-2).into(), 1, "committed_data");
    db.mvcc_store.insert(tx1, row1.clone()).unwrap();
    tests::commit_tx(db.mvcc_store.clone(), &db.conn, tx1).unwrap();

    // T2 updates but aborts
    let conn2 = db.db.connect().unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();
    let row2 = tests::generate_simple_string_row((-2).into(), 1, "aborted_data");
    db.mvcc_store.update(tx2, row2).unwrap();
    db.mvcc_store
        .rollback_tx(tx2, conn2.pager.load().clone(), conn2.as_ref());

    // T3 should see original committed data, not aborted changes
    let conn3 = db.db.connect().unwrap();
    let tx3 = db.mvcc_store.begin_tx(conn3.pager.load().clone()).unwrap();
    let visible_row = db
        .mvcc_store
        .read(tx3, RowID::new((-2).into(), RowKey::Int(1)))
        .unwrap()
        .unwrap();
    // should see T1's data, not T2's
    assert_eq!(visible_row, row1);
}

/// this is not a standard hermitage test, but a simpler version of G0
///
/// we will test that:
///     - if two transactions try to update the same row, the second one should fail with a write-write conflict,
///       even if the first transaction has not committed yet.
#[test]
fn test_hermitage_write_write_conflict() {
    let db = MvccTestDb::new();

    // setup initial data x=1
    let tx_setup = db
        .mvcc_store
        .begin_tx(db.conn.pager.load().clone())
        .unwrap();
    let initial_row = tests::generate_simple_string_row((-2).into(), 1, "x=1");
    db.mvcc_store.insert(tx_setup, initial_row).unwrap();
    tests::commit_tx(db.mvcc_store.clone(), &db.conn, tx_setup).unwrap();

    // conn1: start transaction and update x=2 (but don't commit yet)
    let conn1 = db.db.connect().unwrap();
    let tx1 = db.mvcc_store.begin_tx(conn1.pager.load().clone()).unwrap();
    let row_x2 = tests::generate_simple_string_row((-2).into(), 1, "x=2");
    db.mvcc_store.update(tx1, row_x2.clone()).unwrap();

    // conn2: start transaction and try to update x=3 - should fail with write-write conflict
    let conn2 = db.db.connect().unwrap();
    let tx2 = db.mvcc_store.begin_tx(conn2.pager.load().clone()).unwrap();
    let row_x3 = tests::generate_simple_string_row((-2).into(), 1, "x=3");
    let update_result = db.mvcc_store.update(tx2, row_x3);
    assert!(matches!(update_result, Err(LimboError::WriteWriteConflict)));

    // conn1 should be able to commit successfully since it was first
    tests::commit_tx(db.mvcc_store.clone(), &conn1, tx1).unwrap();

    // verify final state - should see conn1's update (x=2)
    let conn3 = db.db.connect().unwrap();
    let tx3 = db.mvcc_store.begin_tx(conn3.pager.load().clone()).unwrap();
    let final_row = db
        .mvcc_store
        .read(tx3, RowID::new((-2).into(), RowKey::Int(1)))
        .unwrap()
        .unwrap();
    assert_eq!(final_row, row_x2);
}
