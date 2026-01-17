import tech.turso.EncryptionOpts;
import tech.turso.core.TursoDB;
import tech.turso.core.TursoConnection;
import tech.turso.core.TursoStatement;

import java.sql.SQLException;

/**
 * Example demonstrating local database encryption with Turso.
 *
 * This example shows how to:
 * 1. Create an encrypted database with a specific cipher and encryption key
 * 2. Enable the encryption experimental feature
 * 3. Perform basic database operations on encrypted data
 * 4. Verify data persists across database reopens
 */
public class LocalEncryptionExample {

    private static final String DB_PATH = "encrypted_example.db";
    private static final String CIPHER = "aegis256";
    private static final String ENCRYPTION_KEY = "b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327";

    public static void main(String[] args) {
        System.out.println("=== Turso Local Encryption Example ===\n");

        try {
            // Create encryption options
            EncryptionOpts encryptionOpts = new EncryptionOpts(CIPHER, ENCRYPTION_KEY);
            System.out.println("Using cipher: " + CIPHER);
            System.out.println("Encryption options: " + encryptionOpts + "\n");

            // Part 1: Create encrypted database and insert data
            System.out.println("Step 1: Creating encrypted database...");
            try (TursoDB db = TursoDB.create("jdbc:turso:" + DB_PATH, DB_PATH, encryptionOpts)) {
                long connPointer = db.connect();
                TursoConnection conn = new TursoConnection(connPointer);

                // Enable encryption experimental feature
                System.out.println("Step 2: Enabling encryption feature...");
                try (TursoStatement stmt = conn.prepare("PRAGMA experimental_features = 'encryption'")) {
                    stmt.execute();
                }

                // Create table
                System.out.println("Step 3: Creating table...");
                try (TursoStatement stmt = conn.prepare(
                        "CREATE TABLE secrets (id INTEGER PRIMARY KEY, name TEXT, secret TEXT)")) {
                    stmt.execute();
                }

                // Insert data
                System.out.println("Step 4: Inserting encrypted data...");
                String[] secrets = {
                    "Password for production database",
                    "API key for payment processor",
                    "Private encryption key for backup system"
                };

                for (int i = 0; i < secrets.length; i++) {
                    try (TursoStatement stmt = conn.prepare(
                            "INSERT INTO secrets (name, secret) VALUES (?, ?)")) {
                        stmt.bindString(1, "Secret " + (i + 1));
                        stmt.bindString(2, secrets[i]);
                        stmt.execute();
                    }
                }

                // Read and display data
                System.out.println("\nStep 5: Reading encrypted data:");
                try (TursoStatement stmt = conn.prepare("SELECT * FROM secrets")) {
                    while (stmt.step()) {
                        int id = stmt.getColumnInt(0);
                        String name = stmt.getColumnText(1);
                        String secret = stmt.getColumnText(2);
                        System.out.printf("  [%d] %s: %s\n", id, name, secret);
                    }
                }

                // Checkpoint to ensure data is written to disk
                System.out.println("\nStep 6: Flushing to disk...");
                try (TursoStatement stmt = conn.prepare("PRAGMA wal_checkpoint(TRUNCATE)")) {
                    stmt.step();
                }

                conn.close();
            }

            // Part 2: Reopen database and verify data persists
            System.out.println("\n=== Reopening Database ===\n");
            System.out.println("Step 7: Reopening encrypted database...");
            try (TursoDB db = TursoDB.create("jdbc:turso:" + DB_PATH, DB_PATH, encryptionOpts)) {
                long connPointer = db.connect();
                TursoConnection conn = new TursoConnection(connPointer);

                // Enable encryption experimental feature
                try (TursoStatement stmt = conn.prepare("PRAGMA experimental_features = 'encryption'")) {
                    stmt.execute();
                }

                // Verify data is still accessible
                System.out.println("Step 8: Verifying encrypted data:");
                try (TursoStatement stmt = conn.prepare("SELECT COUNT(*) FROM secrets")) {
                    stmt.step();
                    int count = stmt.getColumnInt(0);
                    System.out.printf("  Found %d encrypted records\n", count);
                }

                try (TursoStatement stmt = conn.prepare("SELECT name FROM secrets ORDER BY id")) {
                    System.out.println("  Records:");
                    while (stmt.step()) {
                        String name = stmt.getColumnText(0);
                        System.out.printf("    - %s\n", name);
                    }
                }

                conn.close();
            }

            System.out.println("\n=== Success! ===");
            System.out.println("All operations completed successfully.");
            System.out.println("\nNote: The database file is encrypted at rest.");
            System.out.println("Without the correct encryption key, the data cannot be accessed.");

        } catch (SQLException e) {
            System.err.println("Error: " + e.getMessage());
            e.printStackTrace();
            System.exit(1);
        }
    }
}
