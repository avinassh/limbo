# Turso Dart SDK - Local Encryption Example

This example demonstrates how to use local database encryption with the Turso Dart SDK.

## Features

- Create an encrypted database with a specific cipher algorithm
- Enable the encryption experimental feature
- Perform CRUD operations on encrypted data
- Verify encryption persists across database reopens
- Proper cleanup of database files

## Supported Cipher Algorithms

- `aes128gcm` - AES-128-GCM (requires 16-byte/32-hex-char key)
- `aes256gcm` - AES-256-GCM (requires 32-byte/64-hex-char key)
- `aegis256` - AEGIS-256 (requires 32-byte/64-hex-char key) **[Recommended]**
- `aegis256x2` - AEGIS-256X2 (requires 32-byte/64-hex-char key)
- `aegis256x4` - AEGIS-256X4 (requires 32-byte/64-hex-char key)
- `aegis128l` - AEGIS-128L (requires 16-byte/32-hex-char key)
- `aegis128x2` - AEGIS-128X2 (requires 16-byte/32-hex-char key)
- `aegis128x4` - AEGIS-128X4 (requires 16-byte/32-hex-char key)

## Prerequisites

- Dart SDK 3.0 or higher
- Flutter (for Flutter apps)

## Installation

Add the Turso Dart package to your `pubspec.yaml`:

```yaml
dependencies:
  turso_dart: ^latest_version
```

Then run:

```bash
dart pub get
# or for Flutter
flutter pub get
```

## Running the Example

```bash
dart run local_encryption_example.dart
```

## Usage

### Basic Example

```dart
import 'package:turso_dart/turso_dart.dart';

Future<void> main() async {
  // Create encrypted database
  final client = TursoClient('encrypted.db');

  // Connect with encryption options
  // Note: The exact API for passing encryption options may vary
  await client.connect();

  // Enable encryption experimental feature
  await client.execute("PRAGMA experimental_features = 'encryption'");

  // Use database normally
  await client.execute(
    'CREATE TABLE secrets (id INTEGER PRIMARY KEY, data TEXT)'
  );

  await client.execute(
    'INSERT INTO secrets (data) VALUES (?)',
    positional: ['secret data']
  );

  final rows = await client.query('SELECT * FROM secrets');
  print(rows);

  await client.dispose();
}
```

### Using Environment Variables

```dart
import 'dart:io';

final dbPath = Platform.environment['DB_PATH'] ?? 'encrypted.db';
final cipher = Platform.environment['TURSO_CIPHER'] ?? 'aegis256';
final encryptionKey = Platform.environment['TURSO_ENCRYPTION_KEY'];

final client = TursoClient(dbPath);
// Configure with encryption options
```

### Flutter Integration

```dart
import 'package:flutter/material.dart';
import 'package:turso_dart/turso_dart.dart';

class EncryptedDatabaseService {
  late TursoClient _client;

  Future<void> initialize() async {
    _client = TursoClient('app.db');
    await _client.connect();
    await _client.execute("PRAGMA experimental_features = 'encryption'");
  }

  Future<void> insertSecret(String name, String secret) async {
    await _client.execute(
      'INSERT INTO secrets (name, secret) VALUES (?, ?)',
      positional: [name, secret],
    );
  }

  Future<List<Map<String, dynamic>>> getSecrets() async {
    return await _client.query('SELECT * FROM secrets');
  }

  Future<void> dispose() async {
    await _client.dispose();
  }
}
```

## Generating Encryption Keys

To generate a secure encryption key:

```bash
# For 256-bit keys (aegis256, aes256gcm)
openssl rand -hex 32

# For 128-bit keys (aegis128l, aes128gcm)
openssl rand -hex 16
```

Or using Dart:

```dart
import 'dart:math';
import 'dart:convert';

String generateEncryptionKey(int bytes) {
  final random = Random.secure();
  final values = List<int>.generate(bytes, (i) => random.nextInt(256));
  return values.map((b) => b.toRadixString(16).padLeft(2, '0')).join();
}

// For 256-bit keys
final key256 = generateEncryptionKey(32);
print(key256);

// For 128-bit keys
final key128 = generateEncryptionKey(16);
print(key128);
```

## Important Notes

⚠️ **Experimental Feature**: Encryption is currently an experimental feature and must be explicitly enabled with `PRAGMA experimental_features = 'encryption'`.

🔒 **Key Management**: Store your encryption keys securely! Without the correct key, encrypted data cannot be recovered.

📝 **Configuration**: Encryption options are passed during the connection setup. The exact API depends on the Turso Dart SDK version.

## Security Considerations

1. **Never hardcode encryption keys in production code**
2. **Use environment variables or secure storage (like flutter_secure_storage)**
3. **Implement proper key rotation policies**
4. **Keep backups of your encryption keys in a secure location**
5. **Use HTTPS when transmitting keys over the network**
6. **Test your encryption setup thoroughly before production deployment**

## Configuration Best Practices

### Using flutter_secure_storage (Flutter)

```yaml
dependencies:
  flutter_secure_storage: ^latest_version
```

```dart
import 'package:flutter_secure_storage/flutter_secure_storage.dart';

class SecureConfig {
  final storage = const FlutterSecureStorage();

  Future<Map<String, String>> getEncryptionConfig() async {
    final cipher = await storage.read(key: 'turso_cipher') ?? 'aegis256';
    final hexkey = await storage.read(key: 'turso_encryption_key');

    if (hexkey == null) {
      throw Exception('Encryption key not found in secure storage');
    }

    return {'cipher': cipher, 'hexkey': hexkey};
  }

  Future<void> saveEncryptionConfig(String cipher, String hexkey) async {
    await storage.write(key: 'turso_cipher', value: cipher);
    await storage.write(key: 'turso_encryption_key', value: hexkey);
  }
}
```

### Using dotenv (Dart)

```yaml
dependencies:
  dotenv: ^latest_version
```

```dart
import 'package:dotenv/dotenv.dart';

void main() async {
  final env = DotEnv()..load();

  final dbPath = env['DB_PATH'] ?? 'encrypted.db';
  final cipher = env['TURSO_CIPHER'] ?? 'aegis256';
  final encryptionKey = env['TURSO_ENCRYPTION_KEY'];

  // Use the configuration
}
```

`.env` file:
```env
DB_PATH=encrypted.db
TURSO_CIPHER=aegis256
TURSO_ENCRYPTION_KEY=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327
```

## Expected Output

```
=== Turso Local Encryption Example ===

Step 1: Creating encrypted database...
Using cipher: aegis256
Database path: encrypted_example.db

Step 2: Connecting with encryption...
Step 3: Enabling encryption feature...
Step 4: Creating table...
Step 5: Inserting encrypted data...

Step 6: Reading encrypted data:
  [1] Secret 1: Password for production database
  [2] Secret 2: API key for payment processor
  [3] Secret 3: Private encryption key for backup system

Step 7: Flushing to disk...

=== Reopening Database ===

Step 8: Reopening encrypted database...
Step 9: Verifying encrypted data:
  Found 3 encrypted records
  Records:
    - Secret 1
    - Secret 2
    - Secret 3

=== Success! ===
All operations completed successfully.

Note: The database file is encrypted at rest.
Without the correct encryption key, the data cannot be accessed.
```

## Platform-Specific Considerations

### iOS
Ensure you have the proper permissions in `Info.plist` for file access.

### Android
Add appropriate permissions in `AndroidManifest.xml`:
```xml
<uses-permission android:name="android.permission.WRITE_EXTERNAL_STORAGE"/>
<uses-permission android:name="android.permission.READ_EXTERNAL_STORAGE"/>
```

### Web
Encryption works with IndexedDB backend in web environments.

## Additional Resources

- [Turso Documentation](https://docs.turso.tech)
- [Dart Security Best Practices](https://dart.dev/guides/security)
- [Flutter Secure Storage](https://pub.dev/packages/flutter_secure_storage)
- [Dart dotenv Package](https://pub.dev/packages/dotenv)
