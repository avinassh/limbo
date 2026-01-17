using Xunit;

namespace Turso.Tests;

public class EncryptionTests
{
    private const string TestCipher = "aegis256";
    private const string TestHexkey = "b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327";

    [Fact]
    public void TestBasicEncryption()
    {
        var tempFile = Path.GetTempFileName();
        try
        {
            // Create and write to encrypted database
            var connectionString =
                $"Data Source={tempFile};" +
                $"Encryption Cipher={TestCipher};" +
                $"Encryption Hexkey={TestHexkey}";

            using (var connection = new TursoConnection(connectionString))
            {
                connection.Open();

                // Enable encryption experimental feature
                connection.ExecuteNonQuery("PRAGMA experimental_features = 'encryption'");

                // Create table and insert data
                connection.ExecuteNonQuery("CREATE TABLE test (id INTEGER PRIMARY KEY, value TEXT)");
                connection.ExecuteNonQuery("INSERT INTO test (value) VALUES ('secret data')");

                // Verify data can be read
                using var command = connection.CreateCommand();
                command.CommandText = "SELECT * FROM test";
                using var reader = command.ExecuteReader();

                Assert.True(reader.Read());
                Assert.Equal(1, reader.GetInt32(0));
                Assert.Equal("secret data", reader.GetString(1));

                // Flush to disk
                connection.ExecuteNonQuery("PRAGMA wal_checkpoint(TRUNCATE)");

                connection.Close();
            }

            // Reopen with correct encryption key
            using (var connection = new TursoConnection(connectionString))
            {
                connection.Open();

                // Enable encryption experimental feature
                connection.ExecuteNonQuery("PRAGMA experimental_features = 'encryption'");

                // Verify data can still be read
                using var command = connection.CreateCommand();
                command.CommandText = "SELECT * FROM test";
                using var reader = command.ExecuteReader();

                Assert.True(reader.Read());
                Assert.Equal(1, reader.GetInt32(0));
                Assert.Equal("secret data", reader.GetString(1));

                connection.Close();
            }
        }
        finally
        {
            if (File.Exists(tempFile))
                File.Delete(tempFile);
        }
    }

    [Fact]
    public void TestEncryptionOptsValidation()
    {
        Assert.Throws<ArgumentNullException>(() => new EncryptionOpts(null!, TestHexkey));
        Assert.Throws<ArgumentException>(() => new EncryptionOpts("", TestHexkey));
        Assert.Throws<ArgumentNullException>(() => new EncryptionOpts(TestCipher, null!));
        Assert.Throws<ArgumentException>(() => new EncryptionOpts(TestCipher, ""));
    }

    [Fact]
    public void TestEncryptionOptsEquality()
    {
        var opts1 = new EncryptionOpts(TestCipher, TestHexkey);
        var opts2 = new EncryptionOpts(TestCipher, TestHexkey);
        var opts3 = new EncryptionOpts("aes256gcm", TestHexkey);

        Assert.Equal(opts1, opts2);
        Assert.Equal(opts1.GetHashCode(), opts2.GetHashCode());
        Assert.NotEqual(opts1, opts3);
    }

    [Fact]
    public void TestEncryptionOptsToString()
    {
        var opts = new EncryptionOpts(TestCipher, TestHexkey);
        var str = opts.ToString();

        Assert.Contains(TestCipher, str);
        Assert.DoesNotContain(TestHexkey, str); // hexkey should be masked
        Assert.Contains("***", str);
    }
}
