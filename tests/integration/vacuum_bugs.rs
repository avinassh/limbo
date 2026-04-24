//! Reproducers for bugs logged in `vacuum_bugs.md`.
//!
//! These tests cover the VACUUM bugs that cannot be expressed purely in
//! the `.sqltest` DSL — either because they need a SQLite-created source
//! database (to exercise features Turso's parser rejects but can still
//! read via sqlite_master), a filesystem check, or an experimental
//! feature flag that the sqltest runner does not surface.
//!
//! Each test expresses the *correct* behaviour, so on current Turso they
//! fail; once the corresponding bug is fixed, the test passes. The bug
//! numbers match the headings in vacuum_bugs.md.

#[cfg(test)]
mod tests {
    use crate::common::{limbo_exec_rows, TempDatabase};
    use rusqlite::types::Value;
    use std::fs::{self, OpenOptions};
    use std::io::{Read, Seek, SeekFrom};
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;
    use turso_core::{Database, DatabaseOpts, OpenFlags};

    fn unique_dest_path(label: &str) -> PathBuf {
        let dir = TempDir::new().unwrap().keep();
        dir.join(format!("{label}.db"))
    }

    /// Open a fresh Turso connection for `path` with the given opts.
    fn open_turso(path: &Path, opts: DatabaseOpts) -> std::sync::Arc<turso_core::Connection> {
        let io = std::sync::Arc::new(turso_core::PlatformIO::new().unwrap());
        let db =
            Database::open_file_with_flags(io, path.to_str().unwrap(), OpenFlags::Create, opts, None)
                .unwrap();
        db.connect().unwrap()
    }

    fn read_header_bytes(path: &Path, offset: u64, len: usize) -> Vec<u8> {
        let mut f = OpenOptions::new().read(true).open(path).unwrap();
        f.seek(SeekFrom::Start(offset)).unwrap();
        let mut buf = vec![0u8; len];
        f.read_exact(&mut buf).unwrap();
        buf
    }

    // ----------------------------------------------------------------
    // Bug 1: VACUUM INTO doesn't unescape doubled single quotes in the
    // path string literal. The destination must be the SQL-unescaped
    // form (one quote) not the raw literal (two quotes).
    // ----------------------------------------------------------------
    #[test]
    fn bug1_vacuum_into_unescapes_doubled_single_quotes_in_path() {
        let db = TempDatabase::new_empty();
        let conn = db.connect_limbo();
        conn.execute("CREATE TABLE t(a)").unwrap();
        conn.execute("INSERT INTO t VALUES (1)").unwrap();

        let dir = TempDir::new().unwrap().keep();
        let expected = dir.join("foo'.db"); // one literal quote
        let escaped = format!("{}/foo''.db", dir.display()); // SQL-escaped
        conn.execute(format!("VACUUM INTO '{escaped}'")).unwrap();

        assert!(
            expected.is_file(),
            "VACUUM INTO should have unescaped the doubled quote and produced {expected:?}"
        );
        let literal_two_quotes = dir.join("foo''.db");
        assert!(
            !literal_two_quotes.is_file(),
            "VACUUM INTO should not have used the raw literal path {literal_two_quotes:?}"
        );
    }

    // ----------------------------------------------------------------
    // Bug 5: VACUUM INTO must forward the source connection's cipher
    // key/cipher-mode to the destination. Currently Turso opens the
    // destination without encryption and writes plaintext.
    // ----------------------------------------------------------------
    #[test]
    fn bug5_vacuum_into_encrypted_source_produces_encrypted_dest() {
        let src_dir = TempDir::new().unwrap().keep();
        let src_path = src_dir.join("enc_src.db");
        let dst_path = src_dir.join("enc_dst.db");

        let opts = DatabaseOpts::new().with_encryption(true);
        {
            let conn = open_turso(&src_path, opts);
            conn.execute("PRAGMA cipher='aes256gcm'").unwrap();
            conn.execute(
                "PRAGMA hexkey='000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f'",
            )
            .unwrap();
            conn.execute("CREATE TABLE secrets(user TEXT, pw TEXT)").unwrap();
            conn.execute("INSERT INTO secrets VALUES ('u1', 'sensitive_pw_abc')").unwrap();
            conn.execute(format!("VACUUM INTO '{}'", dst_path.display())).unwrap();
        }

        // SQLite format 3 magic = "SQLite format 3\0". An encrypted Turso
        // DB has "Turso\0..." as its prefix and is unreadable without
        // the key. Check that the destination is NOT plaintext SQLite.
        let header = read_header_bytes(&dst_path, 0, 16);
        assert_ne!(
            &header[..],
            b"SQLite format 3\0",
            "VACUUM INTO must not emit a plaintext SQLite header; dest header = {header:?}"
        );
        // The destination must not be readable without the key.
        let raw_bytes = fs::read(&dst_path).unwrap();
        assert!(
            !raw_bytes.windows(16).any(|w| w == b"sensitive_pw_abc"),
            "plaintext password bytes must not appear in VACUUM INTO output of encrypted source"
        );
    }

    // ----------------------------------------------------------------
    // Bug 7: PRAGMA page_size=N; VACUUM must resize the DB to N. Turso
    // reads the pager's current page_size, ignoring the pending
    // override.
    // ----------------------------------------------------------------
    #[test]
    fn bug7_pragma_page_size_is_applied_by_vacuum() {
        let db = TempDatabase::new_empty();
        let conn = db.connect_limbo();
        conn.execute("CREATE TABLE t(a)").unwrap();
        conn.execute("INSERT INTO t VALUES (1)").unwrap();
        conn.execute("PRAGMA page_size=8192").unwrap();
        conn.execute("VACUUM").unwrap();

        // page_size is stored at header offset 16 (2 bytes, big-endian)
        let bytes = read_header_bytes(&db.path, 16, 2);
        let page_size = u16::from_be_bytes([bytes[0], bytes[1]]);
        assert_eq!(page_size, 8192, "VACUUM must adopt pending PRAGMA page_size");
    }

    // ----------------------------------------------------------------
    // Bug 9: VACUUM inserts a stray sqlite_autoindex_* row in sqlite_master
    // for __turso_internal_types whenever the DB has used CREATE TYPE.
    // The source never had that autoindex row.
    // ----------------------------------------------------------------
    #[test]
    fn bug9_vacuum_does_not_add_autoindex_for_internal_types() {
        let dir = TempDir::new().unwrap().keep();
        let path = dir.join("custom_types.db");
        let opts = DatabaseOpts::new().with_custom_types(true);

        let conn = open_turso(&path, opts);
        conn.execute("CREATE TYPE pos_int BASE INTEGER").unwrap();
        conn.execute("CREATE TABLE t (a pos_int)").unwrap();
        conn.execute("INSERT INTO t VALUES (5)").unwrap();
        let before =
            limbo_exec_rows(&conn, "SELECT name FROM sqlite_master ORDER BY name");
        conn.execute("VACUUM").unwrap();
        let after = limbo_exec_rows(&conn, "SELECT name FROM sqlite_master ORDER BY name");

        assert_eq!(
            before, after,
            "VACUUM must not alter sqlite_master row set for custom types"
        );
    }

    // ----------------------------------------------------------------
    // Bug 12: VACUUM INTO into a pre-existing zero-byte file must
    // succeed (SQLite-compatible): the placeholder is filled, not
    // rejected.
    // ----------------------------------------------------------------
    #[test]
    fn bug12_vacuum_into_empty_placeholder_file_succeeds() {
        let db = TempDatabase::new_empty();
        let conn = db.connect_limbo();
        conn.execute("CREATE TABLE t(a)").unwrap();
        conn.execute("INSERT INTO t VALUES (1)").unwrap();

        let dest = unique_dest_path("bug12_placeholder");
        // Create the zero-byte placeholder.
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&dest)
            .unwrap();
        assert_eq!(fs::metadata(&dest).unwrap().len(), 0);

        conn.execute(format!("VACUUM INTO '{}'", dest.display())).unwrap();

        assert!(
            fs::metadata(&dest).unwrap().len() > 0,
            "VACUUM INTO must fill the pre-created empty placeholder file"
        );
    }

    // ----------------------------------------------------------------
    // Bug 13: VACUUM must work for tables with SQLITE_MAX_COLUMN columns
    // and no INTEGER PRIMARY KEY. Turso's build_copy_sql prepends a
    // rowid pseudo-column, pushing the SELECT past the column limit.
    // ----------------------------------------------------------------
    #[test]
    fn bug13_vacuum_handles_2000_column_table_without_ipk() {
        let db = TempDatabase::new_empty();
        let conn = db.connect_limbo();
        // Build a 2000-column CREATE TABLE.
        let mut cols = Vec::with_capacity(2000);
        for i in 1..=2000 {
            cols.push(format!("c{i} INTEGER DEFAULT 0"));
        }
        let create = format!("CREATE TABLE t ({})", cols.join(", "));
        conn.execute(create).unwrap();
        conn.execute("INSERT INTO t DEFAULT VALUES").unwrap();
        conn.execute("VACUUM").unwrap();
        let rows = limbo_exec_rows(&conn, "SELECT count(*) FROM t");
        assert_eq!(rows, vec![vec![Value::Integer(1)]]);
    }

    // ----------------------------------------------------------------
    // Bug 14: mid-vacuum failure must not leak a partial destination
    // file. After the CHECK-constraint failure the retry should not
    // trip the `output file already exists` preflight.
    // ----------------------------------------------------------------
    #[test]
    fn bug14_vacuum_into_cleans_up_dest_on_check_failure() {
        let db = TempDatabase::new_empty();
        let conn = db.connect_limbo();
        conn.execute("CREATE TABLE t(a INTEGER CHECK(a > 0))").unwrap();
        conn.execute("PRAGMA ignore_check_constraints=ON").unwrap();
        conn.execute("INSERT INTO t VALUES (-5)").unwrap();
        conn.execute("PRAGMA ignore_check_constraints=OFF").unwrap();

        let dest = unique_dest_path("bug14_check_leak");
        let first = conn
            .execute(format!("VACUUM INTO '{}'", dest.display()))
            .err();
        assert!(first.is_some(), "VACUUM INTO should fail on CHECK violation");

        assert!(
            !dest.exists(),
            "VACUUM INTO cleanup must unlink the partial destination after failure"
        );
        // WAL sidecar must also be gone.
        let wal = dest.with_file_name(format!(
            "{}-wal",
            dest.file_name().unwrap().to_string_lossy()
        ));
        assert!(
            !wal.exists(),
            "VACUUM INTO cleanup must unlink the partial destination WAL sidecar"
        );
    }

    // ----------------------------------------------------------------
    // Bug 17: VACUUM must not reset the database's change_counter (DB
    // header offset 24). SQLite bumps it by one, Turso hardcodes it
    // to 1 — a silent rewind visible to concurrent SQLite readers.
    // ----------------------------------------------------------------
    #[test]
    fn bug17_vacuum_preserves_or_bumps_change_counter() {
        let db = TempDatabase::new_empty();
        let conn = db.connect_limbo();
        conn.execute("CREATE TABLE t (a)").unwrap();
        for i in 0..20 {
            conn.execute(format!("INSERT INTO t VALUES ({i})")).unwrap();
        }

        let before_bytes = read_header_bytes(&db.path, 24, 4);
        let before = u32::from_be_bytes([
            before_bytes[0],
            before_bytes[1],
            before_bytes[2],
            before_bytes[3],
        ]);

        conn.execute("VACUUM").unwrap();

        let after_bytes = read_header_bytes(&db.path, 24, 4);
        let after = u32::from_be_bytes([
            after_bytes[0],
            after_bytes[1],
            after_bytes[2],
            after_bytes[3],
        ]);

        assert!(
            after > before,
            "change_counter must be monotonically non-decreasing across VACUUM (before={before}, after={after})"
        );
    }

    // ----------------------------------------------------------------
    // Bug 18: VACUUM must succeed on a SQLite-created DB that contains
    // WITHOUT ROWID tables (the common case is FTS5/RTREE backing
    // tables). Turso rejects WITHOUT ROWID at parse time during replay.
    // ----------------------------------------------------------------
    #[test]
    fn bug18_vacuum_sqlite_without_rowid_source() {
        let db = TempDatabase::new_with_rusqlite(
            "CREATE TABLE wor (id INTEGER PRIMARY KEY, v TEXT) WITHOUT ROWID; \
             INSERT INTO wor VALUES (1, 'x');",
        );
        let conn = db.connect_limbo();
        conn.execute("VACUUM").unwrap();
        let rows = limbo_exec_rows(&conn, "SELECT id, v FROM wor");
        assert_eq!(rows.len(), 1, "row must survive VACUUM on WITHOUT ROWID table");
    }

    // ----------------------------------------------------------------
    // Bug 19: VACUUM INTO must not produce a `.db-wal` sidecar next to
    // the destination. SQLite's VACUUM INTO emits only the `.db` file.
    // ----------------------------------------------------------------
    #[test]
    fn bug19_vacuum_into_does_not_create_wal_sidecar() {
        let db = TempDatabase::new_empty();
        let conn = db.connect_limbo();
        conn.execute("CREATE TABLE t(a)").unwrap();
        conn.execute("INSERT INTO t VALUES (1)").unwrap();

        let dest = unique_dest_path("bug19_wal_sidecar");
        conn.execute(format!("VACUUM INTO '{}'", dest.display())).unwrap();

        let wal = dest.with_file_name(format!(
            "{}-wal",
            dest.file_name().unwrap().to_string_lossy()
        ));
        assert!(
            !wal.exists(),
            "VACUUM INTO must not leave a .db-wal sidecar next to the destination"
        );
    }

    // ----------------------------------------------------------------
    // Bug 20: parser-time failure during VACUUM INTO must also clean up
    // the partial destination file (extension of Bug 14). Using a
    // SQLite-created WITHOUT ROWID source to trigger the parser reject.
    // ----------------------------------------------------------------
    #[test]
    fn bug20_vacuum_into_cleans_up_dest_on_parser_failure() {
        let db = TempDatabase::new_with_rusqlite(
            "CREATE TABLE wor (id INTEGER PRIMARY KEY) WITHOUT ROWID; \
             INSERT INTO wor VALUES (1);",
        );
        let conn = db.connect_limbo();

        let dest = unique_dest_path("bug20_parser_leak");
        let _ = conn.execute(format!("VACUUM INTO '{}'", dest.display()));

        assert!(
            !dest.exists(),
            "VACUUM INTO cleanup must unlink the partial destination after a parse-time failure"
        );
    }

    // ----------------------------------------------------------------
    // Bug 21: the journal_mode write/read file-format bytes (header
    // offsets 18 & 19) must reflect the *source's* mode, not Turso's
    // hard-coded WAL. SQLite's VACUUM INTO always writes (1,1)
    // regardless of source.
    // ----------------------------------------------------------------
    #[test]
    fn bug21_vacuum_into_matches_source_journal_mode_header_bytes() {
        let db = TempDatabase::new_with_rusqlite(
            "CREATE TABLE t(a); INSERT INTO t VALUES (1);",
        );
        let conn = db.connect_limbo();
        let dest = unique_dest_path("bug21_journal_bytes");
        conn.execute(format!("VACUUM INTO '{}'", dest.display())).unwrap();

        let bytes = read_header_bytes(&dest, 18, 2);
        assert_eq!(
            bytes,
            vec![0x01, 0x01],
            "VACUUM INTO output must use rollback-journal format-version bytes (1,1), got ({}, {})",
            bytes[0],
            bytes[1]
        );
    }

    // ----------------------------------------------------------------
    // Bug 22: PRAGMA auto_vacuum=FULL; VACUUM must actually change the
    // on-disk auto_vacuum mode. Turso silently ignores the pending
    // override and reads the pager's current mode.
    // ----------------------------------------------------------------
    #[test]
    fn bug22_pragma_auto_vacuum_full_is_applied_by_vacuum() {
        let dir = TempDir::new().unwrap().keep();
        let path = dir.join("auto_vacuum.db");
        let opts = DatabaseOpts::new().with_autovacuum(true);
        let conn = open_turso(&path, opts);
        conn.execute("CREATE TABLE t(a)").unwrap();
        conn.execute("INSERT INTO t VALUES (1), (2)").unwrap();
        conn.execute("PRAGMA auto_vacuum=FULL").unwrap();
        conn.execute("VACUUM").unwrap();

        // Header offset 52 is largest_root_btree_page; auto_vacuum is
        // considered enabled when this is non-zero.
        let bytes = read_header_bytes(&path, 52, 4);
        let val = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert!(
            val > 0,
            "VACUUM must enable auto_vacuum when PRAGMA auto_vacuum=FULL is pending"
        );
    }

    // ----------------------------------------------------------------
    // Bug 25: VACUUM on a SQLite-created DB with INSTEAD OF triggers on
    // views must succeed. Turso's trigger resolver rejects the stored
    // CREATE TRIGGER at replay time.
    // ----------------------------------------------------------------
    #[test]
    fn bug25_vacuum_sqlite_instead_of_trigger_source() {
        let db = TempDatabase::new_with_rusqlite(
            "CREATE TABLE t(a); \
             CREATE VIEW v AS SELECT a FROM t; \
             CREATE TRIGGER trg INSTEAD OF INSERT ON v BEGIN INSERT INTO t VALUES (NEW.a); END; \
             INSERT INTO v VALUES (1);",
        );
        let opts = DatabaseOpts::new().with_views(true);
        let conn = open_turso(&db.path, opts);
        conn.execute("VACUUM").unwrap();
        let rows = limbo_exec_rows(&conn, "SELECT a FROM t ORDER BY a");
        assert_eq!(rows, vec![vec![Value::Integer(1)]]);
    }

    // ----------------------------------------------------------------
    // Bug 26: VACUUM on a SQLite-created DB with FTS4 backing tables
    // must not return "Corrupt database". The backing tables are plain
    // rowid tables and must survive VACUUM.
    // ----------------------------------------------------------------
    #[test]
    fn bug26_vacuum_sqlite_fts4_backing_tables_source() {
        let db = TempDatabase::new_with_rusqlite(
            "CREATE VIRTUAL TABLE ft USING fts4(c); \
             INSERT INTO ft VALUES ('hello world');",
        );
        let conn = db.connect_limbo();
        conn.execute("VACUUM").unwrap();
    }

    // ----------------------------------------------------------------
    // Bug 27: VACUUM must preserve the quotes around reserved-keyword
    // column names in CREATE VIEW column lists. The post-VACUUM schema
    // SQL becomes malformed otherwise — the view is unqueryable by both
    // Turso and SQLite.
    // ----------------------------------------------------------------
    #[test]
    fn bug27_vacuum_preserves_quoted_reserved_keyword_view_columns() {
        let db = TempDatabase::new_with_rusqlite(
            "CREATE TABLE t(a); \
             INSERT INTO t VALUES (42); \
             CREATE VIEW v(\"order\") AS SELECT a FROM t;",
        );
        let opts = DatabaseOpts::new().with_views(true);
        let conn = open_turso(&db.path, opts);
        conn.execute("VACUUM").unwrap();

        let after = limbo_exec_rows(
            &conn,
            "SELECT sql FROM sqlite_master WHERE name = 'v'",
        );
        let sql_after = match &after[0][0] {
            Value::Text(s) => s.clone(),
            other => panic!("expected TEXT sql, got {other:?}"),
        };
        assert!(
            sql_after.contains("\"order\""),
            "CREATE VIEW column list must keep quotes around reserved keyword \"order\"; got {sql_after:?}"
        );

        // The view must still be queryable after VACUUM.
        let rows = limbo_exec_rows(&conn, "SELECT * FROM v");
        assert_eq!(rows, vec![vec![Value::Integer(42)]]);
    }

    // ----------------------------------------------------------------
    // Bug 28: VACUUM on a SQLite-created DB with a partial index whose
    // WHERE clause uses a non-deterministic function must succeed (the
    // source DB is already in this shape; Turso cannot refuse to VACUUM
    // it).
    // ----------------------------------------------------------------
    #[test]
    fn bug28_vacuum_sqlite_partial_index_with_nondeterministic_where() {
        let db = TempDatabase::new_with_rusqlite(
            "CREATE TABLE t(a INTEGER); \
             CREATE INDEX ix ON t(a) WHERE datetime('now') > '2024-01-01'; \
             INSERT INTO t VALUES (1);",
        );
        let conn = db.connect_limbo();
        conn.execute("VACUUM").unwrap();
    }

    // ----------------------------------------------------------------
    // Bug 30: VACUUM must not duplicate sqlite_sequence rows when the
    // source has a row with rowid != 1. SQLite collapses to a single
    // row at the new rowid; Turso ends up with the auto-generated row
    // PLUS the source's row preserved at its original rowid.
    // ----------------------------------------------------------------
    #[test]
    fn bug30_vacuum_does_not_duplicate_sqlite_sequence_rows() {
        let db = TempDatabase::new_with_rusqlite(
            "CREATE TABLE t(id INTEGER PRIMARY KEY AUTOINCREMENT); \
             INSERT INTO t (id) VALUES (10); \
             DELETE FROM sqlite_sequence; \
             INSERT INTO sqlite_sequence (rowid, name, seq) VALUES (5, 't', 100);",
        );
        let conn = db.connect_limbo();

        conn.execute("VACUUM").unwrap();

        let rows = limbo_exec_rows(
            &conn,
            "SELECT count(*) FROM sqlite_sequence WHERE name = 't'",
        );
        assert_eq!(
            rows,
            vec![vec![Value::Integer(1)]],
            "VACUUM must not add an extra sqlite_sequence row for an AUTOINCREMENT table already tracked in the source"
        );

        // Value must be preserved (Bug 6 coverage on the same path).
        let rows = limbo_exec_rows(
            &conn,
            "SELECT seq FROM sqlite_sequence WHERE name = 't'",
        );
        assert_eq!(
            rows,
            vec![vec![Value::Integer(100)]],
            "VACUUM must preserve the source sqlite_sequence.seq value"
        );
    }

    // ----------------------------------------------------------------
    // Bug 34: VACUUM must succeed on a SQLite-created DB whose CREATE
    // TABLE declares a CHECK constraint with a double-quoted string
    // literal. SQLite treats `"banned"` as a string literal in DDL
    // (its "double-quoted string literals" backwards-compat mode);
    // Turso's strict CREATE TABLE replay resolves it as a column
    // reference and bails with `no such column: banned`.
    // ----------------------------------------------------------------
    #[test]
    fn bug34_vacuum_sqlite_check_with_double_quoted_string_literal() {
        let db = TempDatabase::new_with_rusqlite(
            "CREATE TABLE t (a TEXT CHECK(a <> \"banned\")); \
             INSERT INTO t VALUES ('ok');",
        );
        let conn = db.connect_limbo();
        // Sanity: Turso can OPEN and SELECT from this DB.
        let rows = limbo_exec_rows(&conn, "SELECT a FROM t");
        assert_eq!(rows, vec![vec![Value::Text("ok".into())]]);
        // VACUUM must succeed — this is the bug.
        conn.execute("VACUUM").unwrap();
        let rows = limbo_exec_rows(&conn, "SELECT a FROM t");
        assert_eq!(rows, vec![vec![Value::Text("ok".into())]]);
    }

    // Bug 34 extension: same issue for SQLite-created GENERATED VIRTUAL
    // columns whose expression uses double-quoted string literals.
    #[test]
    fn bug34_vacuum_sqlite_generated_virtual_with_double_quoted_string() {
        let db = TempDatabase::new_with_rusqlite(
            "CREATE TABLE t (\
               a INTEGER, \
               b TEXT AS (CASE WHEN a > 0 THEN \"positive\" ELSE \"non-positive\" END) VIRTUAL\
             ); \
             INSERT INTO t(a) VALUES (1), (-1);",
        );
        let opts = DatabaseOpts::new().with_generated_columns(true);
        let conn = open_turso(&db.path, opts);
        // VACUUM must succeed — this is the bug.
        conn.execute("VACUUM").unwrap();
        let rows = limbo_exec_rows(&conn, "SELECT a FROM t ORDER BY a");
        assert_eq!(
            rows,
            vec![
                vec![Value::Integer(-1)],
                vec![Value::Integer(1)],
            ]
        );
    }

    // ----------------------------------------------------------------
    // Bug 35: VACUUM must succeed on a SQLite-created DB with a partial
    // index whose WHERE clause includes a `COLLATE` suffix (BINARY,
    // NOCASE, RTRIM, ...). Turso's CREATE INDEX parser currently
    // rejects COLLATE in WHERE with a misleading "aggregate, window
    // functions or reference other tables" error.
    // ----------------------------------------------------------------
    #[test]
    fn bug35_vacuum_sqlite_partial_index_where_collate_nocase() {
        let db = TempDatabase::new_with_rusqlite(
            "CREATE TABLE t(a); \
             CREATE INDEX ix ON t(a) WHERE a COLLATE NOCASE > 'm'; \
             INSERT INTO t VALUES ('apple'), ('Zebra');",
        );
        let conn = db.connect_limbo();
        // Sanity: Turso can OPEN and SELECT from this DB.
        let rows = limbo_exec_rows(&conn, "SELECT count(*) FROM t");
        assert_eq!(rows, vec![vec![Value::Integer(2)]]);
        // VACUUM must succeed — this is the bug.
        conn.execute("VACUUM").unwrap();
        let rows = limbo_exec_rows(&conn, "SELECT count(*) FROM t");
        assert_eq!(rows, vec![vec![Value::Integer(2)]]);
    }

    #[test]
    fn bug35_vacuum_sqlite_partial_index_where_collate_binary() {
        let db = TempDatabase::new_with_rusqlite(
            "CREATE TABLE t(a); \
             CREATE INDEX ix ON t(a) WHERE a COLLATE BINARY > 'm'; \
             INSERT INTO t VALUES ('apple'), ('Zebra');",
        );
        let conn = db.connect_limbo();
        // VACUUM must succeed — this is the bug.
        conn.execute("VACUUM").unwrap();
    }

    // ----------------------------------------------------------------
    // Regression note: writing the empty dest-file placeholder above
    // relies on the rusqlite helper writing a valid SQLite header into
    // the source. For completeness, assert a fresh Turso DB matches
    // the source format after one VACUUM (sanity check shared by all
    // tests above that round-trip through sqlite3).
    // ----------------------------------------------------------------
    #[test]
    fn sanity_vacuum_roundtrip_preserves_row_contents() {
        let db = TempDatabase::new_with_rusqlite(
            "CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT); \
             INSERT INTO t VALUES (1, 'one'), (2, 'two');",
        );
        let conn = db.connect_limbo();
        conn.execute("VACUUM").unwrap();
        let rows = limbo_exec_rows(&conn, "SELECT a, b FROM t ORDER BY a");
        assert_eq!(
            rows,
            vec![
                vec![Value::Integer(1), Value::Text("one".into())],
                vec![Value::Integer(2), Value::Text("two".into())],
            ]
        );
    }
}
