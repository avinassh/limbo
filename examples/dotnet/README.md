# Turso .NET SDK - Local Encryption Example

This example demonstrates how to use local database encryption with the Turso .NET SDK.

## Features

- Create an encrypted database using connection string configuration
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

- .NET 6.0 or higher
- Turso .NET SDK NuGet package

### Using .NET CLI

```bash
# Create a new console project
dotnet new console -n TursoEncryptionExample
cd TursoEncryptionExample

# Add Turso package
dotnet add package Turso

# Copy the example file
cp LocalEncryptionExample.cs Program.cs

# Run the example
dotnet run
```

### Using Visual Studio

1. Create a new Console Application
2. Install the Turso NuGet package
3. Copy the example code
4. Build and run

## Usage

### Using Connection String

```csharp
using Turso;

// Configure encryption in connection string
var connectionString =
    "Data Source=encrypted.db;" +
    "Encryption Cipher=aegis256;" +
    "Encryption Hexkey=b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327";

using var connection = new TursoConnection(connectionString);
connection.Open();

// Enable encryption experimental feature
connection.ExecuteNonQuery("PRAGMA experimental_features = 'encryption'");

// Use database normally
connection.ExecuteNonQuery("CREATE TABLE secrets (id INTEGER PRIMARY KEY, data TEXT)");
connection.ExecuteNonQuery("INSERT INTO secrets (data) VALUES ('secret data')");
```

### Using EncryptionOpts Class

```csharp
using Turso;

// Create encryption options
var encryptionOpts = new EncryptionOpts(
    cipher: "aegis256",
    hexkey: "b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327"
);

// Use with connection string builder or other configuration methods
```

## Generating Encryption Keys

To generate a secure encryption key:

```bash
# For 256-bit keys (aegis256, aes256gcm)
openssl rand -hex 32

# For 128-bit keys (aegis128l, aes128gcm)
openssl rand -hex 16
```

Or using PowerShell:

```powershell
# For 256-bit keys
[System.BitConverter]::ToString([System.Security.Cryptography.RandomNumberGenerator]::GetBytes(32)).Replace("-","").ToLower()

# For 128-bit keys
[System.BitConverter]::ToString([System.Security.Cryptography.RandomNumberGenerator]::GetBytes(16)).Replace("-","").ToLower()
```

## Important Notes

⚠️ **Experimental Feature**: Encryption is currently an experimental feature and must be explicitly enabled with `PRAGMA experimental_features = 'encryption'`.

🔒 **Key Management**: Store your encryption keys securely! Without the correct key, encrypted data cannot be recovered.

📝 **Connection String Format**:
```
Data Source=<path>;Encryption Cipher=<cipher>;Encryption Hexkey=<hexkey>
```

## Security Considerations

1. **Never hardcode encryption keys in production code**
2. **Use configuration files, environment variables, or Azure Key Vault**
3. **Implement proper key rotation policies**
4. **Keep backups of your encryption keys in a secure location**
5. **Use `SecureString` or similar for handling keys in memory**
6. **Test your encryption setup thoroughly before production deployment**

## Configuration Best Practices

### Using appsettings.json

```json
{
  "ConnectionStrings": {
    "TursoDb": "Data Source=app.db;Encryption Cipher=aegis256;Encryption Hexkey=<your-key-here>"
  }
}
```

### Using Environment Variables

```csharp
var cipher = Environment.GetEnvironmentVariable("TURSO_CIPHER") ?? "aegis256";
var hexkey = Environment.GetEnvironmentVariable("TURSO_ENCRYPTION_KEY");

var connectionString = $"Data Source=app.db;Encryption Cipher={cipher};Encryption Hexkey={hexkey}";
```

### Using User Secrets (Development)

```bash
dotnet user-secrets init
dotnet user-secrets set "Turso:EncryptionKey" "your-key-here"
```

## Output

```
=== Turso Local Encryption Example ===

Using cipher: aegis256
Database path: encrypted_example.db

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

## Additional Resources

- [Turso Documentation](https://docs.turso.tech)
- [.NET Data Protection API](https://docs.microsoft.com/en-us/aspnet/core/security/data-protection/)
- [Azure Key Vault](https://azure.microsoft.com/en-us/services/key-vault/)
