use crate::common::{do_flush, run_query, run_query_on_row, TempDatabase};
use rand::{rng, RngCore};
use std::panic;
use turso_core::{DatabaseOpts, Row};

const ENABLE_ENCRYPTION: bool = true;

// TODO: mvcc does not error here
#[turso_macros::test]
fn test_per_page_encryption(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let db_path = tmp_db.path.clone();
    let opts = tmp_db.db_opts;

    {
        let conn = tmp_db.connect_limbo();
        run_query(
            &tmp_db,
            &conn,
            "PRAGMA hexkey = 'b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327';",
        )?;
        run_query(&tmp_db, &conn, "PRAGMA cipher = 'aegis256';")?;
        run_query(
            &tmp_db,
            &conn,
            "CREATE TABLE test (id INTEGER PRIMARY KEY, value TEXT);",
        )?;
        run_query(
            &tmp_db,
            &conn,
            "INSERT INTO test (value) VALUES ('Hello, World!')",
        )?;
        let mut row_count = 0;
        run_query_on_row(&tmp_db, &conn, "SELECT * FROM test", |row: &Row| {
            assert_eq!(row.get::<i64>(0).unwrap(), 1);
            assert_eq!(row.get::<String>(1).unwrap(), "Hello, World!");
            row_count += 1;
        })?;
        assert_eq!(row_count, 1);
        do_flush(&conn, &tmp_db)?;
    }

    {
        //test connecting to the encrypted db using correct URI
        let uri = format!(
            "file:{}?cipher=aegis256&hexkey=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
            db_path.to_str().unwrap()
        );
        let (_io, conn) = turso_core::Connection::from_uri(&uri, opts)?;
        let mut row_count = 0;
        run_query_on_row(&tmp_db, &conn, "SELECT * FROM test", |row: &Row| {
            assert_eq!(row.get::<i64>(0).unwrap(), 1);
            assert_eq!(row.get::<String>(1).unwrap(), "Hello, World!");
            row_count += 1;
        })?;
        assert_eq!(row_count, 1);
    }
    {
        //Try to create a table after reopening the encrypted db.
        let uri = format!(
            "file:{}?cipher=aegis256&hexkey=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
            db_path.to_str().unwrap()
        );
        let (_io, conn) = turso_core::Connection::from_uri(&uri, opts)?;
        run_query(
            &tmp_db,
            &conn,
            "CREATE TABLE test1 (id INTEGER PRIMARY KEY, value TEXT);",
        )?;
        do_flush(&conn, &tmp_db)?;
    }
    {
        //Try to create a table after reopening the encrypted db.
        let uri = format!(
            "file:{}?cipher=aegis256&hexkey=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
            db_path.to_str().unwrap()
        );
        let (_io, conn) = turso_core::Connection::from_uri(&uri, opts)?;
        run_query(
            &tmp_db,
            &conn,
            "INSERT INTO test1 (value) VALUES ('Hello, World!')",
        )?;
        let mut row_count = 0;
        run_query_on_row(&tmp_db, &conn, "SELECT * FROM test", |row: &Row| {
            assert_eq!(row.get::<i64>(0).unwrap(), 1);
            assert_eq!(row.get::<String>(1).unwrap(), "Hello, World!");
            row_count += 1;
        })?;

        assert_eq!(row_count, 1);
        do_flush(&conn, &tmp_db)?;
    }
    {
        // test connecting to encrypted db using wrong key(key is ending with 77.The correct key is ending with 27).This should panic.
        let uri = format!(
            "file:{}?cipher=aegis256&hexkey=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76377",
            db_path.to_str().unwrap()
        );
        let (_io, conn) = turso_core::Connection::from_uri(&uri, opts)?;
        let should_panic = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            run_query_on_row(&tmp_db, &conn, "SELECT * FROM test", |_row: &Row| {}).unwrap();
        }));
        assert!(
            should_panic.is_err(),
            "should panic when accessing encrypted DB with wrong key"
        );
    }
    {
        //test connecting to encrypted db using insufficient encryption parameters in URI.This should panic.
        let uri = format!("file:{}?cipher=aegis256", db_path.to_str().unwrap());
        let should_panic = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            turso_core::Connection::from_uri(&uri, opts).unwrap();
        }));
        assert!(
            should_panic.is_err(),
            "should panic when accessing encrypted DB without passing hexkey in URI"
        );
    }
    {
        let uri = format!(
            "file:{}?hexkey=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
            db_path.to_str().unwrap()
        );
        let should_panic = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            turso_core::Connection::from_uri(&uri, opts).unwrap();
        }));
        assert!(
            should_panic.is_err(),
            "should panic when accessing encrypted DB without passing cipher in URI"
        );
    }
    {
        // Testing connecting to db without using URI.This should panic.
        let conn = tmp_db.connect_limbo();
        let should_panic = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            run_query_on_row(&tmp_db, &conn, "SELECT * FROM test", |_row: &Row| {}).unwrap();
        }));
        assert!(
            should_panic.is_err(),
            "should panic when accessing encrypted DB without using URI"
        );
    }

    Ok(())
}

#[turso_macros::test(mvcc)]
fn test_non_4k_page_size_encryption(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let db_path = tmp_db.path.clone();

    {
        let conn = tmp_db.connect_limbo();
        // Set page size to 8k (8192 bytes) and test encryption. Default page size is 4k.
        run_query(&tmp_db, &conn, "PRAGMA page_size = 8192;")?;
        run_query(
            &tmp_db,
            &conn,
            "PRAGMA hexkey = 'b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327';",
        )?;
        run_query(&tmp_db, &conn, "PRAGMA cipher = 'aegis256';")?;
        run_query(
            &tmp_db,
            &conn,
            "CREATE TABLE test (id INTEGER PRIMARY KEY, value TEXT);",
        )?;
        run_query(
            &tmp_db,
            &conn,
            "INSERT INTO test (value) VALUES ('Hello, World!')",
        )?;
        let mut row_count = 0;
        run_query_on_row(&tmp_db, &conn, "SELECT * FROM test", |row: &Row| {
            assert_eq!(row.get::<i64>(0).unwrap(), 1);
            assert_eq!(row.get::<String>(1).unwrap(), "Hello, World!");
            row_count += 1;
        })?;

        assert_eq!(row_count, 1);
        do_flush(&conn, &tmp_db)?;
    }

    {
        // Reopen the existing db with 8k page size and test encryption
        let uri = format!(
            "file:{}?cipher=aegis256&hexkey=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
            db_path.to_str().unwrap()
        );
        let (_io, conn) = turso_core::Connection::from_uri(
            &uri,
            DatabaseOpts::new().with_encryption(ENABLE_ENCRYPTION),
        )?;
        run_query_on_row(&tmp_db, &conn, "SELECT * FROM test", |row: &Row| {
            assert_eq!(row.get::<i64>(0).unwrap(), 1);
            assert_eq!(row.get::<String>(1).unwrap(), "Hello, World!");
        })?;
    }

    Ok(())
}

// TODO: mvcc for some reason does not error on corruption here
#[turso_macros::test]
fn test_corruption_turso_magic_bytes(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let db_path = tmp_db.path.clone();

    let opts = tmp_db.db_opts;

    {
        let conn = tmp_db.connect_limbo();
        run_query(
            &tmp_db,
            &conn,
            "PRAGMA hexkey = 'b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327';",
        )?;
        run_query(&tmp_db, &conn, "PRAGMA cipher = 'aegis256';")?;
        run_query(
            &tmp_db,
            &conn,
            "CREATE TABLE test (id INTEGER PRIMARY KEY, value TEXT);",
        )?;
        run_query(
            &tmp_db,
            &conn,
            "INSERT INTO test (value) VALUES ('Test corruption')",
        )?;
        run_query(&tmp_db, &conn, "PRAGMA wal_checkpoint(TRUNCATE);")?;
        do_flush(&conn, &tmp_db)?;
    }

    // corrupt the Turso magic bytes by changing "Turso" to "Vurso" (the db name as it was intended)
    {
        use std::fs::OpenOptions;
        use std::io::{Seek, SeekFrom, Write};

        let mut file = OpenOptions::new().write(true).open(&db_path)?;

        file.seek(SeekFrom::Start(0))?;
        file.write_all(b"V")?;
    }

    // try to connect to the corrupted database - this should fail
    {
        let uri = format!(
            "file:{}?cipher=aegis256&hexkey=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
            db_path.to_str().unwrap()
        );

        let should_panic = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let (_io, conn) = turso_core::Connection::from_uri(&uri, opts).unwrap();
            run_query_on_row(&tmp_db, &conn, "SELECT * FROM test", |_row: &Row| {}).unwrap();
        }));

        assert!(
            should_panic.is_err(),
            "should panic when accessing encrypted DB with corrupted Turso magic bytes"
        );
    }

    Ok(())
}

#[turso_macros::test(mvcc)]
fn test_corruption_associated_data_bytes(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let db_path = tmp_db.path.clone();

    {
        let conn = tmp_db.connect_limbo();
        run_query(
            &tmp_db,
            &conn,
            "PRAGMA hexkey = 'b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327';",
        )?;
        run_query(&tmp_db, &conn, "PRAGMA cipher = 'aegis256';")?;
        run_query(
            &tmp_db,
            &conn,
            "CREATE TABLE test (id INTEGER PRIMARY KEY, value TEXT);",
        )?;
        run_query(
            &tmp_db,
            &conn,
            "INSERT INTO test (value) VALUES ('Test AD corruption')",
        )?;
        run_query(&tmp_db, &conn, "PRAGMA wal_checkpoint(TRUNCATE);")?;
        do_flush(&conn, &tmp_db)?;
    }

    // test corruption at different positions in the header (the first 100 bytes)
    let corruption_positions = [3, 7, 16, 30, 50, 70, 99];

    for &corrupt_pos in &corruption_positions {
        let test_db_name = format!(
            "test-corruption-ad-pos-{}-{}.db",
            corrupt_pos,
            rng().next_u32()
        );
        let test_tmp_db = TempDatabase::new(&test_db_name);
        let test_db_path = test_tmp_db.path.clone();
        std::fs::copy(&db_path, &test_db_path)?;

        // corrupt one byte
        {
            use std::fs::OpenOptions;
            use std::io::{Read, Seek, SeekFrom, Write};

            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&test_db_path)?;

            file.seek(SeekFrom::Start(corrupt_pos as u64))?;
            let mut original_byte = [0u8; 1];
            file.read_exact(&mut original_byte)?;

            // corrupt it by flipping all bits
            let corrupted_byte = [!original_byte[0]];

            file.seek(SeekFrom::Start(corrupt_pos as u64))?;
            file.write_all(&corrupted_byte)?;
        }

        // this should fail
        {
            let uri = format!(
                "file:{}?cipher=aegis256&hexkey=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
                test_db_path.to_str().unwrap()
            );

            let should_panic = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                let (_io, conn) = turso_core::Connection::from_uri(
                    &uri,
                    DatabaseOpts::new().with_encryption(ENABLE_ENCRYPTION),
                )
                .unwrap();
                run_query_on_row(&test_tmp_db, &conn, "SELECT * FROM test", |_row: &Row| {})
                    .unwrap();
            }));

            assert!(
                should_panic.is_err(),
                "should panic when accessing encrypted DB with corrupted associated data at position {corrupt_pos}",
            );
        }
    }

    Ok(())
}

#[turso_macros::test(mvcc)]
fn test_turso_header_structure(db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();

    let verify_header =
        |db_path: &str, expected_cipher_id: u8, description: &str| -> anyhow::Result<()> {
            use std::fs::File;
            use std::io::{Read, Seek, SeekFrom};

            let mut file = File::open(db_path)?;
            let mut header = [0u8; 16];
            file.seek(SeekFrom::Start(0))?;
            file.read_exact(&mut header)?;

            assert_eq!(
                &header[0..5],
                b"Turso",
                "Magic bytes should be 'Turso' for {description}"
            );
            assert_eq!(header[5], 0x00, "Version should be 0x00 for {description}");
            assert_eq!(
                header[6], expected_cipher_id,
                "Cipher ID should be {expected_cipher_id} for {description}"
            );

            // the unused bytes should be zeroed
            for (i, &byte) in header[7..16].iter().enumerate() {
                assert_eq!(
                    byte,
                    0,
                    "Unused byte at position {} should be 0 for {}",
                    i + 7,
                    description
                );
            }

            println!("Verified {} header: cipher ID = {}", description, header[6]);
            Ok(())
        };

    let test_cases = [
        (
            "aes128gcm",
            1,
            "AES-128-GCM",
            "b1bbfda4f589dc9daaf004fe21111e00",
        ),
        (
            "aes256gcm",
            2,
            "AES-256-GCM",
            "b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
        ),
        (
            "aegis256",
            3,
            "AEGIS-256",
            "b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
        ),
        (
            "aegis256x2",
            4,
            "AEGIS-256X2",
            "b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
        ),
        (
            "aegis128l",
            6,
            "AEGIS-128L",
            "b1bbfda4f589dc9daaf004fe21111e00",
        ),
        (
            "aegis128x2",
            7,
            "AEGIS-128X2",
            "b1bbfda4f589dc9daaf004fe21111e00",
        ),
        (
            "aegis128x4",
            8,
            "AEGIS-128X4",
            "b1bbfda4f589dc9daaf004fe21111e00",
        ),
    ];
    let opts = db.db_opts;
    let flags = db.db_flags;

    for (cipher_name, expected_id, description, hexkey) in test_cases {
        let tmp_db = TempDatabase::builder()
            .with_opts(opts)
            .with_flags(flags)
            .build();
        let db_path = tmp_db.path.clone();

        {
            let conn = tmp_db.connect_limbo();
            run_query(&tmp_db, &conn, &format!("PRAGMA hexkey = '{hexkey}';"))?;
            run_query(&tmp_db, &conn, &format!("PRAGMA cipher = '{cipher_name}';"))?;
            run_query(
                &tmp_db,
                &conn,
                "CREATE TABLE test (id INTEGER PRIMARY KEY, value TEXT);",
            )?;
            do_flush(&conn, &tmp_db)?;
        }

        verify_header(db_path.to_str().unwrap(), expected_id, description)?;
    }
    Ok(())
}

// ==================== MVCC Encryption Tests ====================

/// Test basic CRUD operations with encryption in MVCC mode.
#[turso_macros::test(mvcc)]
fn test_mvcc_basic_encryption(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let db_path = tmp_db.path.clone();

    {
        let conn = tmp_db.connect_limbo();
        run_query(
            &tmp_db,
            &conn,
            "PRAGMA hexkey = 'b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327';",
        )?;
        run_query(&tmp_db, &conn, "PRAGMA cipher = 'aegis256';")?;

        // Create table and insert data
        run_query(
            &tmp_db,
            &conn,
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER);",
        )?;
        run_query(
            &tmp_db,
            &conn,
            "INSERT INTO users (id, name, age) VALUES (1, 'Alice', 30);",
        )?;
        run_query(
            &tmp_db,
            &conn,
            "INSERT INTO users (id, name, age) VALUES (2, 'Bob', 25);",
        )?;
        run_query(
            &tmp_db,
            &conn,
            "INSERT INTO users (id, name, age) VALUES (3, 'Charlie', 35);",
        )?;

        // Verify data
        let mut count = 0;
        run_query_on_row(&tmp_db, &conn, "SELECT * FROM users ORDER BY id", |row: &Row| {
            count += 1;
            match row.get::<i64>(0).unwrap() {
                1 => {
                    assert_eq!(row.get::<String>(1).unwrap(), "Alice");
                    assert_eq!(row.get::<i64>(2).unwrap(), 30);
                }
                2 => {
                    assert_eq!(row.get::<String>(1).unwrap(), "Bob");
                    assert_eq!(row.get::<i64>(2).unwrap(), 25);
                }
                3 => {
                    assert_eq!(row.get::<String>(1).unwrap(), "Charlie");
                    assert_eq!(row.get::<i64>(2).unwrap(), 35);
                }
                _ => panic!("Unexpected row"),
            }
        })?;
        assert_eq!(count, 3);
        do_flush(&conn, &tmp_db)?;
    }

    // Verify data can be read back with correct key
    {
        let uri = format!(
            "file:{}?cipher=aegis256&hexkey=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
            db_path.to_str().unwrap()
        );
        let (_io, conn) = turso_core::Connection::from_uri(
            &uri,
            DatabaseOpts::new().with_encryption(ENABLE_ENCRYPTION),
        )?;

        let mut count = 0;
        run_query_on_row(&tmp_db, &conn, "SELECT COUNT(*) FROM users", |row: &Row| {
            count = row.get::<i64>(0).unwrap();
        })?;
        assert_eq!(count, 3);
    }

    Ok(())
}

/// Test database persistence and recovery after reopen with encryption in MVCC mode.
#[turso_macros::test(mvcc)]
fn test_mvcc_encryption_reopen(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let db_path = tmp_db.path.clone();
    let opts = tmp_db.db_opts;

    // Create database and insert data
    {
        let conn = tmp_db.connect_limbo();
        run_query(
            &tmp_db,
            &conn,
            "PRAGMA hexkey = 'b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327';",
        )?;
        run_query(&tmp_db, &conn, "PRAGMA cipher = 'aegis256';")?;
        run_query(
            &tmp_db,
            &conn,
            "CREATE TABLE test (id INTEGER PRIMARY KEY, data TEXT);",
        )?;
        run_query(&tmp_db, &conn, "INSERT INTO test VALUES (1, 'first');")?;
        run_query(&tmp_db, &conn, "INSERT INTO test VALUES (2, 'second');")?;
        do_flush(&conn, &tmp_db)?;
    }

    // Reopen and add more data
    {
        let uri = format!(
            "file:{}?cipher=aegis256&hexkey=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
            db_path.to_str().unwrap()
        );
        let (_io, conn) = turso_core::Connection::from_uri(&uri, opts)?;
        run_query(&tmp_db, &conn, "INSERT INTO test VALUES (3, 'third');")?;
        do_flush(&conn, &tmp_db)?;
    }

    // Reopen again and verify all data
    {
        let uri = format!(
            "file:{}?cipher=aegis256&hexkey=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
            db_path.to_str().unwrap()
        );
        let (_io, conn) = turso_core::Connection::from_uri(&uri, opts)?;

        let mut rows = Vec::new();
        run_query_on_row(&tmp_db, &conn, "SELECT * FROM test ORDER BY id", |row: &Row| {
            rows.push((row.get::<i64>(0).unwrap(), row.get::<String>(1).unwrap()));
        })?;
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], (1, "first".to_string()));
        assert_eq!(rows[1], (2, "second".to_string()));
        assert_eq!(rows[2], (3, "third".to_string()));
    }

    Ok(())
}

/// Test UPDATE and DELETE operations with encryption in MVCC mode.
#[turso_macros::test(mvcc)]
fn test_mvcc_encryption_updates_deletes(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let db_path = tmp_db.path.clone();
    let opts = tmp_db.db_opts;

    {
        let conn = tmp_db.connect_limbo();
        run_query(
            &tmp_db,
            &conn,
            "PRAGMA hexkey = 'b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327';",
        )?;
        run_query(&tmp_db, &conn, "PRAGMA cipher = 'aegis256';")?;

        run_query(
            &tmp_db,
            &conn,
            "CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT, price REAL);",
        )?;
        run_query(
            &tmp_db,
            &conn,
            "INSERT INTO products VALUES (1, 'Apple', 1.50);",
        )?;
        run_query(
            &tmp_db,
            &conn,
            "INSERT INTO products VALUES (2, 'Banana', 0.75);",
        )?;
        run_query(
            &tmp_db,
            &conn,
            "INSERT INTO products VALUES (3, 'Cherry', 2.00);",
        )?;

        // Update a row
        run_query(
            &tmp_db,
            &conn,
            "UPDATE products SET price = 1.75 WHERE id = 1;",
        )?;

        // Delete a row
        run_query(&tmp_db, &conn, "DELETE FROM products WHERE id = 2;")?;

        do_flush(&conn, &tmp_db)?;
    }

    // Reopen and verify changes persisted
    {
        let uri = format!(
            "file:{}?cipher=aegis256&hexkey=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
            db_path.to_str().unwrap()
        );
        let (_io, conn) = turso_core::Connection::from_uri(&uri, opts)?;

        let mut rows = Vec::new();
        run_query_on_row(
            &tmp_db,
            &conn,
            "SELECT id, name, price FROM products ORDER BY id",
            |row: &Row| {
                rows.push((
                    row.get::<i64>(0).unwrap(),
                    row.get::<String>(1).unwrap(),
                    row.get::<f64>(2).unwrap(),
                ));
            },
        )?;

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (1, "Apple".to_string(), 1.75));
        assert_eq!(rows[1], (3, "Cherry".to_string(), 2.00));
    }

    Ok(())
}

/// Test multiple tables with JOINs in encrypted MVCC mode.
#[turso_macros::test(mvcc)]
fn test_mvcc_encryption_multiple_tables(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let db_path = tmp_db.path.clone();
    let opts = tmp_db.db_opts;

    {
        let conn = tmp_db.connect_limbo();
        run_query(
            &tmp_db,
            &conn,
            "PRAGMA hexkey = 'b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327';",
        )?;
        run_query(&tmp_db, &conn, "PRAGMA cipher = 'aegis256';")?;

        // Create multiple tables
        run_query(
            &tmp_db,
            &conn,
            "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT);",
        )?;
        run_query(
            &tmp_db,
            &conn,
            "CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT, author_id INTEGER);",
        )?;

        // Insert data
        run_query(&tmp_db, &conn, "INSERT INTO authors VALUES (1, 'Tolkien');")?;
        run_query(
            &tmp_db,
            &conn,
            "INSERT INTO authors VALUES (2, 'Rowling');",
        )?;
        run_query(
            &tmp_db,
            &conn,
            "INSERT INTO books VALUES (1, 'The Hobbit', 1);",
        )?;
        run_query(
            &tmp_db,
            &conn,
            "INSERT INTO books VALUES (2, 'Harry Potter', 2);",
        )?;
        run_query(
            &tmp_db,
            &conn,
            "INSERT INTO books VALUES (3, 'LOTR', 1);",
        )?;

        do_flush(&conn, &tmp_db)?;
    }

    // Reopen and verify with JOIN
    {
        let uri = format!(
            "file:{}?cipher=aegis256&hexkey=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
            db_path.to_str().unwrap()
        );
        let (_io, conn) = turso_core::Connection::from_uri(&uri, opts)?;

        let mut count = 0;
        run_query_on_row(
            &tmp_db,
            &conn,
            "SELECT b.title, a.name FROM books b JOIN authors a ON b.author_id = a.id WHERE a.name = 'Tolkien'",
            |_row: &Row| {
                count += 1;
            },
        )?;
        assert_eq!(count, 2); // The Hobbit and LOTR
    }

    Ok(())
}

/// Test transaction support with encryption in MVCC mode.
#[turso_macros::test(mvcc)]
fn test_mvcc_encryption_transactions(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let db_path = tmp_db.path.clone();
    let opts = tmp_db.db_opts;

    {
        let conn = tmp_db.connect_limbo();
        run_query(
            &tmp_db,
            &conn,
            "PRAGMA hexkey = 'b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327';",
        )?;
        run_query(&tmp_db, &conn, "PRAGMA cipher = 'aegis256';")?;

        run_query(
            &tmp_db,
            &conn,
            "CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance REAL);",
        )?;
        run_query(&tmp_db, &conn, "INSERT INTO accounts VALUES (1, 100.0);")?;
        run_query(&tmp_db, &conn, "INSERT INTO accounts VALUES (2, 200.0);")?;

        // Transaction with multiple operations
        run_query(&tmp_db, &conn, "BEGIN;")?;
        run_query(
            &tmp_db,
            &conn,
            "UPDATE accounts SET balance = balance - 50 WHERE id = 1;",
        )?;
        run_query(
            &tmp_db,
            &conn,
            "UPDATE accounts SET balance = balance + 50 WHERE id = 2;",
        )?;
        run_query(&tmp_db, &conn, "COMMIT;")?;

        do_flush(&conn, &tmp_db)?;
    }

    // Verify transaction was committed
    {
        let uri = format!(
            "file:{}?cipher=aegis256&hexkey=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327",
            db_path.to_str().unwrap()
        );
        let (_io, conn) = turso_core::Connection::from_uri(&uri, opts)?;

        let mut balances = Vec::new();
        run_query_on_row(
            &tmp_db,
            &conn,
            "SELECT id, balance FROM accounts ORDER BY id",
            |row: &Row| {
                balances.push((row.get::<i64>(0).unwrap(), row.get::<f64>(1).unwrap()));
            },
        )?;

        assert_eq!(balances.len(), 2);
        assert_eq!(balances[0], (1, 50.0));
        assert_eq!(balances[1], (2, 250.0));
    }

    Ok(())
}

/// Test different cipher modes with encryption in MVCC mode.
#[turso_macros::test(mvcc)]
fn test_mvcc_encryption_cipher_modes(tmp_db: TempDatabase) -> anyhow::Result<()> {
    let _ = env_logger::try_init();
    let opts = tmp_db.db_opts;
    let flags = tmp_db.db_flags;

    let test_cases = [
        ("aes256gcm", "b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327"),
        ("aegis128l", "b1bbfda4f589dc9daaf004fe21111e00"),
    ];

    for (cipher_name, hexkey) in test_cases {
        let test_db = TempDatabase::builder()
            .with_opts(opts)
            .with_flags(flags)
            .build();
        let db_path = test_db.path.clone();

        // Create and populate database
        {
            let conn = test_db.connect_limbo();
            run_query(&test_db, &conn, &format!("PRAGMA hexkey = '{hexkey}';"))?;
            run_query(&test_db, &conn, &format!("PRAGMA cipher = '{cipher_name}';"))?;
            run_query(
                &test_db,
                &conn,
                "CREATE TABLE cipher_test (id INTEGER PRIMARY KEY, data TEXT);",
            )?;
            run_query(
                &test_db,
                &conn,
                &format!("INSERT INTO cipher_test VALUES (1, 'Encrypted with {cipher_name}');"),
            )?;
            do_flush(&conn, &test_db)?;
        }

        // Reopen and verify
        {
            let uri = format!(
                "file:{}?cipher={cipher_name}&hexkey={hexkey}",
                db_path.to_str().unwrap()
            );
            let (_io, conn) = turso_core::Connection::from_uri(&uri, opts)?;

            let mut result = String::new();
            run_query_on_row(&test_db, &conn, "SELECT data FROM cipher_test", |row: &Row| {
                result = row.get::<String>(0).unwrap();
            })?;
            assert_eq!(result, format!("Encrypted with {cipher_name}"));
        }
    }

    Ok(())
}
