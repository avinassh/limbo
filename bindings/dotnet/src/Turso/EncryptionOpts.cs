namespace Turso;

/// <summary>
/// Local encryption options for Turso database.
/// </summary>
/// <remarks>
/// Encryption is an experimental feature. To use it, you must enable the "encryption"
/// experimental feature when opening the database.
///
/// Supported cipher algorithms:
/// - aes128gcm (requires 16-byte/32-hex-char key)
/// - aes256gcm (requires 32-byte/64-hex-char key)
/// - aegis256 (requires 32-byte/64-hex-char key)
/// - aegis256x2 (requires 32-byte/64-hex-char key)
/// - aegis256x4 (requires 32-byte/64-hex-char key)
/// - aegis128l (requires 16-byte/32-hex-char key)
/// - aegis128x2 (requires 16-byte/32-hex-char key)
/// - aegis128x4 (requires 16-byte/32-hex-char key)
/// </remarks>
/// <example>
/// <code>
/// var encryptionOpts = new EncryptionOpts(
///     "aegis256",
///     "b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327"
/// );
/// </code>
/// </example>
public sealed class EncryptionOpts
{
    /// <summary>
    /// Gets the encryption cipher algorithm.
    /// </summary>
    public string Cipher { get; }

    /// <summary>
    /// Gets the encryption key as a hexadecimal string.
    /// </summary>
    public string Hexkey { get; }

    /// <summary>
    /// Creates a new encryption configuration.
    /// </summary>
    /// <param name="cipher">The encryption cipher algorithm (e.g., "aegis256", "aes256gcm")</param>
    /// <param name="hexkey">The encryption key as a hexadecimal string</param>
    /// <exception cref="ArgumentNullException">Thrown when cipher or hexkey is null</exception>
    /// <exception cref="ArgumentException">Thrown when cipher or hexkey is empty</exception>
    public EncryptionOpts(string cipher, string hexkey)
    {
        if (cipher == null)
            throw new ArgumentNullException(nameof(cipher));
        if (string.IsNullOrWhiteSpace(cipher))
            throw new ArgumentException("Cipher must not be empty", nameof(cipher));

        if (hexkey == null)
            throw new ArgumentNullException(nameof(hexkey));
        if (string.IsNullOrWhiteSpace(hexkey))
            throw new ArgumentException("Hexkey must not be empty", nameof(hexkey));

        Cipher = cipher;
        Hexkey = hexkey;
    }

    /// <summary>
    /// Returns a string representation of the encryption options with the key masked.
    /// </summary>
    public override string ToString()
    {
        return $"EncryptionOpts {{ Cipher = '{Cipher}', Hexkey = '***' }}";
    }

    public override bool Equals(object? obj)
    {
        if (obj is not EncryptionOpts other)
            return false;

        return Cipher == other.Cipher && Hexkey == other.Hexkey;
    }

    public override int GetHashCode()
    {
        return HashCode.Combine(Cipher, Hexkey);
    }
}
