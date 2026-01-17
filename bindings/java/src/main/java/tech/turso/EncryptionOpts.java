package tech.turso;

import tech.turso.annotations.Nullable;

/**
 * Local encryption options for Turso database.
 *
 * <p>Encryption is an experimental feature. To use it, you must enable the "encryption"
 * experimental feature when opening the database.
 *
 * <p>Supported cipher algorithms:
 *
 * <ul>
 *   <li>aes128gcm (requires 16-byte/32-hex-char key)
 *   <li>aes256gcm (requires 32-byte/64-hex-char key)
 *   <li>aegis256 (requires 32-byte/64-hex-char key)
 *   <li>aegis256x2 (requires 32-byte/64-hex-char key)
 *   <li>aegis256x4 (requires 32-byte/64-hex-char key)
 *   <li>aegis128l (requires 16-byte/32-hex-char key)
 *   <li>aegis128x2 (requires 16-byte/32-hex-char key)
 *   <li>aegis128x4 (requires 16-byte/32-hex-char key)
 * </ul>
 *
 * <p>Example usage:
 *
 * <pre>{@code
 * EncryptionOpts encryptionOpts = new EncryptionOpts(
 *     "aegis256",
 *     "b1bbfda4f589dc9daaf004fe21111e00dc00c98237102f5c7002a5669fc76327"
 * );
 * }</pre>
 */
public final class EncryptionOpts {

  private final String cipher;
  private final String hexkey;

  /**
   * Creates a new encryption configuration.
   *
   * @param cipher The encryption cipher algorithm (e.g., "aegis256", "aes256gcm")
   * @param hexkey The encryption key as a hexadecimal string
   */
  public EncryptionOpts(String cipher, String hexkey) {
    if (cipher == null || cipher.isEmpty()) {
      throw new IllegalArgumentException("cipher must not be null or empty");
    }
    if (hexkey == null || hexkey.isEmpty()) {
      throw new IllegalArgumentException("hexkey must not be null or empty");
    }
    this.cipher = cipher;
    this.hexkey = hexkey;
  }

  /**
   * Gets the cipher algorithm.
   *
   * @return The cipher algorithm
   */
  public String getCipher() {
    return cipher;
  }

  /**
   * Gets the encryption key as a hexadecimal string.
   *
   * @return The hexadecimal encryption key
   */
  public String getHexkey() {
    return hexkey;
  }

  @Override
  public String toString() {
    return "EncryptionOpts{cipher='" + cipher + "', hexkey='***'}";
  }

  @Override
  public boolean equals(Object o) {
    if (this == o) return true;
    if (o == null || getClass() != o.getClass()) return false;

    EncryptionOpts that = (EncryptionOpts) o;

    if (!cipher.equals(that.cipher)) return false;
    return hexkey.equals(that.hexkey);
  }

  @Override
  public int hashCode() {
    int result = cipher.hashCode();
    result = 31 * result + hexkey.hashCode();
    return result;
  }
}
