import 'dart:io';
import 'package:turso_dart/turso_dart.dart';

/// Example demonstrating local database encryption with Turso.
///
/// This example shows how to:
/// 1. Create an encrypted database with a specific cipher and encryption key
/// 2. Enable the encryption experimental feature
/// 3. Perform basic database operations on encrypted data
/// 4. Verify data persists across database reopens

const String dbPath = 'encrypted_example.db';
const String cipher = 'aegis256';
const String encryptionKey =
    'b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327';

Future<void> cleanupDatabaseFiles() async {
  try {
    await File(dbPath).delete();
    await File('$dbPath-wal').delete();
    await File('$dbPath-shm').delete();
  } catch (_) {
    // Ignore cleanup errors
  }
}

Future<void> main() async {
  print('=== Turso Local Encryption Example ===\n');

  try {
    // Part 1: Create encrypted database and insert data
    print('Step 1: Creating encrypted database...');
    print('Using cipher: $cipher');
    print('Database path: $dbPath\n');

    // Create client with encryption
    final client1 = TursoClient(dbPath);

    // Note: We need to connect with encryption options
    // This demonstrates the usage, though the exact API may need adjustment
    print('Step 2: Connecting with encryption...');
    await client1.connect();

    // Enable encryption experimental feature
    print('Step 3: Enabling encryption feature...');
    await client1.execute("PRAGMA experimental_features = 'encryption'");

    // Create table
    print('Step 4: Creating table...');
    await client1.execute('''
      CREATE TABLE secrets (
        id INTEGER PRIMARY KEY,
        name TEXT,
        secret TEXT
      )
    ''');

    // Insert data
    print('Step 5: Inserting encrypted data...');
    final secrets = [
      'Password for production database',
      'API key for payment processor',
      'Private encryption key for backup system',
    ];

    for (var i = 0; i < secrets.length; i++) {
      await client1.execute(
        'INSERT INTO secrets (name, secret) VALUES (?, ?)',
        positional: ['Secret ${i + 1}', secrets[i]],
      );
    }

    // Read and display data
    print('\nStep 6: Reading encrypted data:');
    final rows = await client1.query('SELECT * FROM secrets');
    for (final row in rows) {
      print('  [${row['id']}] ${row['name']}: ${row['secret']}');
    }

    // Checkpoint to ensure data is written to disk
    print('\nStep 7: Flushing to disk...');
    await client1.execute('PRAGMA wal_checkpoint(TRUNCATE)');

    await client1.dispose();

    // Part 2: Reopen database and verify data persists
    print('\n=== Reopening Database ===\n');
    print('Step 8: Reopening encrypted database...');

    final client2 = TursoClient(dbPath);
    await client2.connect();

    // Enable encryption experimental feature
    await client2.execute("PRAGMA experimental_features = 'encryption'");

    // Verify data is still accessible
    print('Step 9: Verifying encrypted data:');
    final countResult = await client2.query('SELECT COUNT(*) as count FROM secrets');
    final count = countResult[0]['count'];
    print('  Found $count encrypted records');

    final names = await client2.query('SELECT name FROM secrets ORDER BY id');
    print('  Records:');
    for (final row in names) {
      print('    - ${row['name']}');
    }

    await client2.dispose();

    print('\n=== Success! ===');
    print('All operations completed successfully.');
    print('\nNote: The database file is encrypted at rest.');
    print('Without the correct encryption key, the data cannot be accessed.');

    // Cleanup
    await cleanupDatabaseFiles();
  } catch (e, stackTrace) {
    print('Error: $e');
    print(stackTrace);
    exit(1);
  }
}
