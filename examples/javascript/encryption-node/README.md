# Turso JavaScript SDK - Local Encryption Example

This example demonstrates how to use local database encryption with the Turso JavaScript SDK in Node.js.

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

- Node.js 18.0 or higher
- npm or yarn

## Installation

```bash
npm install
```

## Running the Example

```bash
npm start
```

## Usage

### Basic Example

```javascript
import { Database } from '@tursodatabase/database';

// Create encrypted database
const db = new Database('encrypted.db', {
  encryptionCipher: 'aegis256',
  encryptionHexkey: 'b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327'
});

// Enable encryption experimental feature
await db.exec("PRAGMA experimental_features = 'encryption'");

// Use database normally
await db.exec("CREATE TABLE secrets (id INTEGER PRIMARY KEY, data TEXT)");
await db.prepare("INSERT INTO secrets (data) VALUES (?)").run('secret data');

const rows = await db.prepare("SELECT * FROM secrets").all();
console.log(rows);

db.close();
```

### Using Environment Variables

```javascript
const db = new Database(process.env.DB_PATH || 'encrypted.db', {
  encryptionCipher: process.env.TURSO_CIPHER || 'aegis256',
  encryptionHexkey: process.env.TURSO_ENCRYPTION_KEY
});
```

### TypeScript Support

The Turso JavaScript SDK includes full TypeScript definitions:

```typescript
import { Database, DatabaseOpts } from '@tursodatabase/database';

const opts: DatabaseOpts = {
  encryptionCipher: 'aegis256',
  encryptionHexkey: 'b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327'
};

const db = new Database('encrypted.db', opts);
```

## Generating Encryption Keys

To generate a secure encryption key:

```bash
# For 256-bit keys (aegis256, aes256gcm)
openssl rand -hex 32

# For 128-bit keys (aegis128l, aes128gcm)
openssl rand -hex 16
```

Or using Node.js:

```javascript
import { randomBytes } from 'crypto';

// For 256-bit keys
const key256 = randomBytes(32).toString('hex');
console.log(key256);

// For 128-bit keys
const key128 = randomBytes(16).toString('hex');
console.log(key128);
```

## Important Notes

⚠️ **Experimental Feature**: Encryption is currently an experimental feature and must be explicitly enabled with `PRAGMA experimental_features = 'encryption'`.

🔒 **Key Management**: Store your encryption keys securely! Without the correct key, encrypted data cannot be recovered.

📝 **Configuration**:
```javascript
{
  encryptionCipher: 'aegis256',      // Cipher algorithm
  encryptionHexkey: '<your-key-hex>' // Hexadecimal encryption key
}
```

## Security Considerations

1. **Never hardcode encryption keys in production code**
2. **Use environment variables or secret management services**
3. **Implement proper key rotation policies**
4. **Keep backups of your encryption keys in a secure location**
5. **Use HTTPS when transmitting keys over the network**
6. **Test your encryption setup thoroughly before production deployment**

## Configuration Best Practices

### Using dotenv

```bash
npm install dotenv
```

```javascript
import 'dotenv/config';
import { Database } from '@tursodatabase/database';

const db = new Database(process.env.DB_PATH, {
  encryptionCipher: process.env.TURSO_CIPHER,
  encryptionHexkey: process.env.TURSO_ENCRYPTION_KEY
});
```

`.env` file:
```env
DB_PATH=encrypted.db
TURSO_CIPHER=aegis256
TURSO_ENCRYPTION_KEY=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327
```

### Using AWS Secrets Manager

```javascript
import { SecretsManagerClient, GetSecretValueCommand } from '@aws-sdk/client-secrets-manager';

const client = new SecretsManagerClient({ region: 'us-east-1' });

async function getEncryptionKey() {
  const response = await client.send(
    new GetSecretValueCommand({ SecretId: 'turso/encryption-key' })
  );
  return JSON.parse(response.SecretString);
}

const { cipher, hexkey } = await getEncryptionKey();
const db = new Database('encrypted.db', {
  encryptionCipher: cipher,
  encryptionHexkey: hexkey
});
```

## Output

```
=== Turso Local Encryption Example ===

Step 1: Creating encrypted database...
Using cipher: aegis256
Database path: encrypted_example.db

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

## Browser Support

For browser environments using WebAssembly, the same API applies:

```javascript
import { Database } from '@tursodatabase/database/wasm';

const db = new Database('encrypted.db', {
  encryptionCipher: 'aegis256',
  encryptionHexkey: process.env.VITE_ENCRYPTION_KEY // Using Vite env variables
});
```

## Additional Resources

- [Turso Documentation](https://docs.turso.tech)
- [Node.js Crypto Module](https://nodejs.org/api/crypto.html)
- [Environment Variables Best Practices](https://12factor.net/config)
