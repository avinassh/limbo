using System;
using System.IO;
using Turso;

/// <summary>
/// Example demonstrating local database encryption with Turso.
///
/// This example shows how to:
/// 1. Create an encrypted database with a specific cipher and encryption key
/// 2. Enable the encryption experimental feature
/// 3. Perform basic database operations on encrypted data
/// 4. Verify data persists across database reopens
/// </summary>
class LocalEncryptionExample
{
    private const string DbPath = "encrypted_example.db";
    private const string Cipher = "aegis256";
    private const string EncryptionKey = "b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327";

    static void Main(string[] args)
    {
        Console.WriteLine("=== Turso Local Encryption Example ===\n");

        try
        {
            // Create connection string with encryption
            var connectionString =
                $"Data Source={DbPath};" +
                $"Encryption Cipher={Cipher};" +
                $"Encryption Hexkey={EncryptionKey}";

            Console.WriteLine($"Using cipher: {Cipher}");
            Console.WriteLine($"Database path: {DbPath}\n");

            // Part 1: Create encrypted database and insert data
            Console.WriteLine("Step 1: Creating encrypted database...");
            using (var connection = new TursoConnection(connectionString))
            {
                connection.Open();

                // Enable encryption experimental feature
                Console.WriteLine("Step 2: Enabling encryption feature...");
                connection.ExecuteNonQuery("PRAGMA experimental_features = 'encryption'");

                // Create table
                Console.WriteLine("Step 3: Creating table...");
                connection.ExecuteNonQuery(@"
                    CREATE TABLE secrets (
                        id INTEGER PRIMARY KEY,
                        name TEXT,
                        secret TEXT
                    )
                ");

                // Insert data
                Console.WriteLine("Step 4: Inserting encrypted data...");
                var secrets = new[]
                {
                    "Password for production database",
                    "API key for payment processor",
                    "Private encryption key for backup system"
                };

                using (var command = connection.CreateCommand())
                {
                    command.CommandText = "INSERT INTO secrets (name, secret) VALUES (@name, @secret)";

                    for (int i = 0; i < secrets.Length; i++)
                    {
                        command.Parameters.Clear();
                        command.Parameters.AddWithValue("@name", $"Secret {i + 1}");
                        command.Parameters.AddWithValue("@secret", secrets[i]);
                        command.ExecuteNonQuery();
                    }
                }

                // Read and display data
                Console.WriteLine("\nStep 5: Reading encrypted data:");
                using (var command = connection.CreateCommand())
                {
                    command.CommandText = "SELECT * FROM secrets";
                    using (var reader = command.ExecuteReader())
                    {
                        while (reader.Read())
                        {
                            int id = reader.GetInt32(0);
                            string name = reader.GetString(1);
                            string secret = reader.GetString(2);
                            Console.WriteLine($"  [{id}] {name}: {secret}");
                        }
                    }
                }

                // Checkpoint to ensure data is written to disk
                Console.WriteLine("\nStep 6: Flushing to disk...");
                connection.ExecuteNonQuery("PRAGMA wal_checkpoint(TRUNCATE)");

                connection.Close();
            }

            // Part 2: Reopen database and verify data persists
            Console.WriteLine("\n=== Reopening Database ===\n");
            Console.WriteLine("Step 7: Reopening encrypted database...");
            using (var connection = new TursoConnection(connectionString))
            {
                connection.Open();

                // Enable encryption experimental feature
                connection.ExecuteNonQuery("PRAGMA experimental_features = 'encryption'");

                // Verify data is still accessible
                Console.WriteLine("Step 8: Verifying encrypted data:");
                using (var command = connection.CreateCommand())
                {
                    command.CommandText = "SELECT COUNT(*) FROM secrets";
                    int count = Convert.ToInt32(command.ExecuteScalar());
                    Console.WriteLine($"  Found {count} encrypted records");
                }

                using (var command = connection.CreateCommand())
                {
                    command.CommandText = "SELECT name FROM secrets ORDER BY id";
                    using (var reader = command.ExecuteReader())
                    {
                        Console.WriteLine("  Records:");
                        while (reader.Read())
                        {
                            string name = reader.GetString(0);
                            Console.WriteLine($"    - {name}");
                        }
                    }
                }

                connection.Close();
            }

            Console.WriteLine("\n=== Success! ===");
            Console.WriteLine("All operations completed successfully.");
            Console.WriteLine("\nNote: The database file is encrypted at rest.");
            Console.WriteLine("Without the correct encryption key, the data cannot be accessed.");

            // Cleanup
            CleanupDatabaseFiles();
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Error: {ex.Message}");
            Console.Error.WriteLine(ex.StackTrace);
            Environment.Exit(1);
        }
    }

    static void CleanupDatabaseFiles()
    {
        try
        {
            if (File.Exists(DbPath))
                File.Delete(DbPath);
            if (File.Exists($"{DbPath}-wal"))
                File.Delete($"{DbPath}-wal");
            if (File.Exists($"{DbPath}-shm"))
                File.Delete($"{DbPath}-shm");
        }
        catch
        {
            // Ignore cleanup errors
        }
    }
}
