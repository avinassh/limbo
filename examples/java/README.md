# Turso Java SDK - Local Encryption Example

This example demonstrates how to use local database encryption with the Turso Java SDK.

## Features

- Create an encrypted database with a specific cipher algorithm
- Enable the encryption experimental feature
- Perform CRUD operations on encrypted data
- Verify encryption persists across database reopens

## Supported Cipher Algorithms

- `aes128gcm` - AES-128-GCM (requires 16-byte/32-hex-char key)
- `aes256gcm` - AES-256-GCM (requires 32-byte/64-hex-char key)
- `aegis256` - AEGIS-256 (requires 32-byte/64-hex-char key) **[Recommended]**
- `aegis256x2` - AEGIS-256X2 (requires 32-byte/64-hex-char key)
- `aegis256x4` - AEGIS-256X4 (requires 32-byte/64-hex-char key)
- `aegis128l` - AEGIS-128L (requires 16-byte/32-hex-char key)
- `aegis128x2` - AEGIS-128X2 (requires 16-byte/32-hex-char key)
- `aegis128x4` - AEGIS-128X4 (requires 16-byte/32-hex-char key)

## Building and Running

### Prerequisites

- Java 11 or higher
- Maven or Gradle

### Compile and Run

```bash
# Compile
javac -cp "path/to/turso-sdk.jar" LocalEncryptionExample.java

# Run
java -cp ".:path/to/turso-sdk.jar" LocalEncryptionExample
```

## Usage

```java
import tech.turso.EncryptionOpts;
import tech.turso.core.TursoDB;

// Create encryption options
EncryptionOpts encryptionOpts = new EncryptionOpts(
    "aegis256",  // cipher algorithm
    "b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327"  // hex key
);

// Open encrypted database
TursoDB db = TursoDB.create("jdbc:turso:encrypted.db", "encrypted.db", encryptionOpts);
TursoConnection conn = new TursoConnection(db.connect());

// Enable encryption experimental feature
conn.prepare("PRAGMA experimental_features = 'encryption'").execute();

// Use database normally
conn.prepare("CREATE TABLE secrets (id INTEGER PRIMARY KEY, data TEXT)").execute();
conn.prepare("INSERT INTO secrets (data) VALUES ('secret data')").execute();
```

## Generating Encryption Keys

To generate a secure encryption key:

```bash
# For 256-bit keys (aegis256, aes256gcm)
openssl rand -hex 32

# For 128-bit keys (aegis128l, aes128gcm)
openssl rand -hex 16
```

## Important Notes

⚠️ **Experimental Feature**: Encryption is currently an experimental feature and must be explicitly enabled with `PRAGMA experimental_features = 'encryption'`.

🔒 **Key Management**: Store your encryption keys securely! Without the correct key, encrypted data cannot be recovered.

📝 **Key Requirements**:
- Keys must be provided as hexadecimal strings
- Key length must match the cipher requirements (16 or 32 bytes)
- Invalid keys will result in errors

## Security Considerations

1. **Never hardcode encryption keys in production code**
2. **Use environment variables or secure key management systems**
3. **Rotate encryption keys periodically**
4. **Keep backups of your encryption keys in a secure location**
5. **Test your encryption setup before deploying to production**

## Output

```
=== Turso Local Encryption Example ===

Using cipher: aegis256
Encryption options: EncryptionOpts{cipher='aegis256', hexkey='***'}

Step 1: Creating encrypted database...
Step 2: Enabling encryption feature...
Step 3: Creating table...
Step 4: Inserting encrypted data...

Step 5: Reading encrypted data:
  [1] Secret 1: Password for production database
  [2] Secret 2: API key for payment processor
  [3] Secret 3: Private encryption key for backup system

Step 6: Flushing to disk...

=== Reopening Database ===

Step 7: Reopening encrypted database...
Step 8: Verifying encrypted data:
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
