package tech.turso;

import static org.junit.jupiter.api.Assertions.*;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.sql.ResultSet;
import java.sql.SQLException;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import tech.turso.core.TursoDB;
import tech.turso.core.TursoConnection;
import tech.turso.core.TursoStatement;

class EncryptionTest {

  private static final String TEST_CIPHER = "aegis256";
  private static final String TEST_HEXKEY =
      "b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327";

  @Test
  void testBasicEncryption(@TempDir Path tempDir) throws SQLException, IOException {
    Path dbPath = tempDir.resolve("encrypted.db");
    String dbPathStr = dbPath.toString();

    EncryptionOpts encryptionOpts = new EncryptionOpts(TEST_CIPHER, TEST_HEXKEY);

    // Create and write to encrypted database
    try (TursoDB db = TursoDB.create("jdbc:turso:" + dbPathStr, dbPathStr, encryptionOpts)) {
      long connPointer = db.connect();
      TursoConnection conn = new TursoConnection(connPointer);

      // Enable encryption experimental feature
      try (TursoStatement stmt = conn.prepare("PRAGMA experimental_features = 'encryption'")) {
        stmt.execute();
      }

      // Create table and insert data
      try (TursoStatement stmt =
          conn.prepare("CREATE TABLE test (id INTEGER PRIMARY KEY, value TEXT)")) {
        stmt.execute();
      }

      try (TursoStatement stmt = conn.prepare("INSERT INTO test (value) VALUES ('secret data')")) {
        stmt.execute();
      }

      // Verify data can be read
      try (TursoStatement stmt = conn.prepare("SELECT * FROM test")) {
        stmt.step();
        assertEquals(1, stmt.getColumnInt(0));
        assertEquals("secret data", stmt.getColumnText(1));
      }

      // Flush to disk
      try (TursoStatement stmt = conn.prepare("PRAGMA wal_checkpoint(TRUNCATE)")) {
        stmt.step();
      }

      conn.close();
    }

    // Reopen with correct encryption key
    try (TursoDB db = TursoDB.create("jdbc:turso:" + dbPathStr, dbPathStr, encryptionOpts)) {
      long connPointer = db.connect();
      TursoConnection conn = new TursoConnection(connPointer);

      // Enable encryption experimental feature
      try (TursoStatement stmt = conn.prepare("PRAGMA experimental_features = 'encryption'")) {
        stmt.execute();
      }

      // Verify data can still be read
      try (TursoStatement stmt = conn.prepare("SELECT * FROM test")) {
        stmt.step();
        assertEquals(1, stmt.getColumnInt(0));
        assertEquals("secret data", stmt.getColumnText(1));
      }

      conn.close();
    }
  }

  @Test
  void testEncryptionOptsValidation() {
    assertThrows(IllegalArgumentException.class, () -> new EncryptionOpts(null, TEST_HEXKEY));

    assertThrows(IllegalArgumentException.class, () -> new EncryptionOpts("", TEST_HEXKEY));

    assertThrows(IllegalArgumentException.class, () -> new EncryptionOpts(TEST_CIPHER, null));

    assertThrows(IllegalArgumentException.class, () -> new EncryptionOpts(TEST_CIPHER, ""));
  }

  @Test
  void testEncryptionOptsEquality() {
    EncryptionOpts opts1 = new EncryptionOpts(TEST_CIPHER, TEST_HEXKEY);
    EncryptionOpts opts2 = new EncryptionOpts(TEST_CIPHER, TEST_HEXKEY);
    EncryptionOpts opts3 = new EncryptionOpts("aes256gcm", TEST_HEXKEY);

    assertEquals(opts1, opts2);
    assertEquals(opts1.hashCode(), opts2.hashCode());
    assertNotEquals(opts1, opts3);
  }

  @Test
  void testEncryptionOptsToString() {
    EncryptionOpts opts = new EncryptionOpts(TEST_CIPHER, TEST_HEXKEY);
    String str = opts.toString();

    assertTrue(str.contains(TEST_CIPHER));
    assertFalse(str.contains(TEST_HEXKEY)); // hexkey should be masked
    assertTrue(str.contains("***"));
  }
}
