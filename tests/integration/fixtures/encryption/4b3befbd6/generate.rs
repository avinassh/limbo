use std::{path::Path, sync::Arc};
use turso_core::{
    Buffer, Completion, ConnectOpts, Database, DatabaseOpts, EncryptionKey, EncryptionOpts,
    MemoryIO, OpenFlags, OpenOptions, IO,
};
use turso_sqlite::SqliteDialect;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args().nth(1).expect("fixture output directory");
    std::fs::create_dir_all(&output)?;
    for (name, cipher, page_size, mvcc) in [
        ("aegis256-wal", "aegis256", 4096, false),
        ("aes256-wal", "aes256gcm", 8192, false),
        ("aegis256-mvcc", "aegis256", 4096, true),
        ("aes256-mvcc", "aes256gcm", 8192, true),
    ] {
        create_fixture(Path::new(&output), name, cipher, page_size, mvcc)?;
    }
    Ok(())
}

fn create_fixture(
    output: &Path,
    name: &str,
    cipher: &str,
    page_size: u32,
    mvcc: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let io = Arc::new(MemoryIO::new());
    let path = format!("fixture-source-{name}.db");
    let db = Database::open(
        io.clone(),
        &path,
        OpenOptions::new(Arc::new(SqliteDialect))
            .db_opts(DatabaseOpts::new().with_encryption(true))
            .encryption(EncryptionOpts {
                cipher: cipher.to_string(),
                hexkey: "01".repeat(32),
            }),
    )?;
    let connection =
        db.connect(ConnectOpts::new().with_encryption_key(Some(EncryptionKey::new_256([1; 32]))))?;
    connection.execute(format!("PRAGMA page_size = {page_size}"))?;
    if mvcc {
        connection.execute("PRAGMA journal_mode = 'mvcc'")?;
    }
    connection.execute("CREATE TABLE legacy_secrets(id INTEGER PRIMARY KEY, value TEXT)")?;
    connection.execute("INSERT INTO legacy_secrets VALUES (1, 'legacy-one'), (2, 'legacy-two')")?;
    let rows = connection
        .prepare("SELECT value FROM legacy_secrets ORDER BY id")?
        .run_collect_rows()?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0].to_string(), "legacy-one");
    assert_eq!(rows[1][0].to_string(), "legacy-two");

    for suffix in ["", "-wal", "-log"] {
        let file = io.open_file(&format!("{path}{suffix}"), OpenFlags::Create, false)?;
        let length = usize::try_from(file.size()?)?;
        if length == 0 {
            continue;
        }
        let buffer = Arc::new(Buffer::new_temporary(length));
        let completion = file.pread(0, Completion::new_read(buffer.clone(), Box::new(|_| None)))?;
        io.wait_for_completion(completion)?;
        std::fs::write(output.join(format!("{name}.db{suffix}")), buffer.as_slice())?;
        println!("{name}.db{suffix}: {length} bytes");
    }
    if mvcc {
        assert!(output.join(format!("{name}.db-log")).exists());
    } else {
        assert!(output.join(format!("{name}.db-wal")).exists());
    }
    Ok(())
}
