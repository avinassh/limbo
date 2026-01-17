import { Database } from '@tursodatabase/database';
import { unlink } from 'fs/promises';

/**
 * Example demonstrating local database encryption with Turso.
 *
 * This example shows how to:
 * 1. Create an encrypted database with a specific cipher and encryption key
 * 2. Enable the encryption experimental feature
 * 3. Perform basic database operations on encrypted data
 * 4. Verify data persists across database reopens
 */

const DB_PATH = 'encrypted_example.db';
const CIPHER = 'aegis256';
const ENCRYPTION_KEY = 'b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327';

async function cleanupDatabaseFiles() {
  try {
    await unlink(DB_PATH);
    await unlink(`${DB_PATH}-wal`);
    await unlink(`${DB_PATH}-shm`);
  } catch {
    // Ignore cleanup errors
  }
}

async function main() {
  console.log('=== Turso Local Encryption Example ===\n');

  try {
    // Part 1: Create encrypted database and insert data
    console.log('Step 1: Creating encrypted database...');
    console.log(`Using cipher: ${CIPHER}`);
    console.log(`Database path: ${DB_PATH}\n`);

    const db1 = new Database(DB_PATH, {
      encryptionCipher: CIPHER,
      encryptionHexkey: ENCRYPTION_KEY
    });

    // Wait for connection (implicit)
    await db1.exec("SELECT 1");

    // Enable encryption experimental feature
    console.log('Step 2: Enabling encryption feature...');
    await db1.exec("PRAGMA experimental_features = 'encryption'");

    // Create table
    console.log('Step 3: Creating table...');
    await db1.exec(`
      CREATE TABLE secrets (
        id INTEGER PRIMARY KEY,
        name TEXT,
        secret TEXT
      )
    `);

    // Insert data
    console.log('Step 4: Inserting encrypted data...');
    const secrets = [
      'Password for production database',
      'API key for payment processor',
      'Private encryption key for backup system'
    ];

    const insertStmt = db1.prepare('INSERT INTO secrets (name, secret) VALUES (?, ?)');
    for (let i = 0; i < secrets.length; i++) {
      await insertStmt.run(`Secret ${i + 1}`, secrets[i]);
    }

    // Read and display data
    console.log('\nStep 5: Reading encrypted data:');
    const rows = await db1.prepare('SELECT * FROM secrets').all();
    for (const row of rows) {
      console.log(`  [${row.id}] ${row.name}: ${row.secret}`);
    }

    // Checkpoint to ensure data is written to disk
    console.log('\nStep 6: Flushing to disk...');
    await db1.exec('PRAGMA wal_checkpoint(TRUNCATE)');

    db1.close();

    // Part 2: Reopen database and verify data persists
    console.log('\n=== Reopening Database ===\n');
    console.log('Step 7: Reopening encrypted database...');

    const db2 = new Database(DB_PATH, {
      encryptionCipher: CIPHER,
      encryptionHexkey: ENCRYPTION_KEY
    });

    // Enable encryption experimental feature
    await db2.exec("PRAGMA experimental_features = 'encryption'");

    // Verify data is still accessible
    console.log('Step 8: Verifying encrypted data:');
    const countResult = await db2.prepare('SELECT COUNT(*) as count FROM secrets').get();
    console.log(`  Found ${countResult.count} encrypted records`);

    const names = await db2.prepare('SELECT name FROM secrets ORDER BY id').all();
    console.log('  Records:');
    for (const row of names) {
      console.log(`    - ${row.name}`);
    }

    db2.close();

    console.log('\n=== Success! ===');
    console.log('All operations completed successfully.');
    console.log('\nNote: The database file is encrypted at rest.');
    console.log('Without the correct encryption key, the data cannot be accessed.');

    // Cleanup
    await cleanupDatabaseFiles();

  } catch (error) {
    console.error('Error:', error.message);
    console.error(error.stack);
    process.exit(1);
  }
}

main();
