//! A bytea result whose allocation fails makes the statement fail with
//! `OutOfMemory`, without a panic.
#![cfg(nightly)]
#![feature(allocator_api)]

use std::{cell::Cell, num::NonZero, ptr::NonNull};
use turso_core::alloc::{
    current_allocation_site, set_allocator, AllocError, AllocationSite, ApiAllocator, Global,
    Layout, TursoAllocBackend, ValueBlobAllocationSite,
};
use turso_core::Value;
use turso_pg::{Connection, DatabaseOpts, LimboError, OpenFlags};

#[derive(Clone, Copy)]
enum Fault {
    None,
    /// Fail allocations of this many bytes.
    Size(usize),
    /// Fail the allocations of `Value::from_slice`.
    BlobFromSlice,
}

thread_local! {
    static FAULT: Cell<Fault> = const { Cell::new(Fault::None) };
}

struct FaultBackend;

unsafe impl TursoAllocBackend for FaultBackend {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let fail = match FAULT.get() {
            Fault::None => false,
            Fault::Size(size) => layout.size() == size,
            Fault::BlobFromSlice => {
                current_allocation_site()
                    == Some(AllocationSite::ValueBlob(
                        ValueBlobAllocationSite::FromSlice,
                    ))
            }
        };
        if fail {
            return Err(AllocError);
        }
        Global.allocate(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        unsafe { Global.deallocate(ptr, layout) }
    }
}

static BACKEND: FaultBackend = FaultBackend;

#[test]
fn bytea_allocation_failure_returns_out_of_memory() {
    unsafe { set_allocator(&BACKEND).unwrap() };
    let (_io, db) =
        turso_pg::open_database(":memory:", None, OpenFlags::default(), DatabaseOpts::new())
            .unwrap();
    let conn = Connection::connect(&db).unwrap();
    conn.execute("CREATE TABLE t (b BYTEA, v INT4, s TEXT)")
        .unwrap();
    let value = vec![7u8; 3001];
    let mut insert = conn
        .prepare("INSERT INTO t VALUES ($1, 258, '\\xdead')")
        .unwrap();
    insert
        .bind_at(NonZero::new(1).unwrap(), blob(&value))
        .unwrap();
    insert.run_ignore_rows().unwrap();

    // byteacat, for ||. The fault matches only the size of the result.
    let concat = "SELECT b || b FROM t";
    let fault = Fault::Size(2 * value.len());
    assert!(matches!(
        query(&conn, concat, fault),
        Err(LimboError::OutOfMemory)
    ));
    assert_eq!(
        query(&conn, "SELECT b FROM t", fault).unwrap(),
        [[blob(&value)]]
    );
    assert_eq!(
        query(&conn, concat, Fault::None).unwrap(),
        [[blob(&[value.as_slice(), value.as_slice()].concat())]]
    );

    // The cast from an integer and the input of bytea from text.
    for (sql, bytes) in [
        ("SELECT v::bytea FROM t", &[0, 0, 1, 2][..]),
        ("SELECT s::bytea FROM t", &[0xde, 0xad][..]),
    ] {
        let result = query(&conn, sql, Fault::BlobFromSlice);
        assert!(
            matches!(result, Err(LimboError::OutOfMemory)),
            "{sql}: {result:?}"
        );
        assert_eq!(query(&conn, sql, Fault::None).unwrap(), [[blob(bytes)]]);
    }
}

/// Runs `sql` with `fault` active and returns its rows or its error.
fn query(conn: &Connection, sql: &str, fault: Fault) -> Result<Vec<Vec<Value>>, LimboError> {
    let mut stmt = conn.prepare(sql).unwrap();
    FAULT.set(fault);
    let result = stmt.run_collect_rows();
    FAULT.set(Fault::None);
    result.map_err(|error| error.core_error().clone())
}

fn blob(bytes: &[u8]) -> Value {
    Value::from_slice(bytes).unwrap()
}
