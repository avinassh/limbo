# Turso Examples

This directory contains examples for using Turso across different programming languages and platforms.

## Local Encryption Examples

Examples demonstrating local database encryption (experimental feature):

- [`java`](./java/) — Java SDK local encryption example
- [`dotnet`](./dotnet/) — .NET SDK local encryption example
- [`dart`](./dart/) — Dart SDK local encryption example
- [`javascript/encryption-node`](./javascript/encryption-node/) — JavaScript/Node.js local encryption example

### Supported Cipher Algorithms

All encryption examples support the following cipher algorithms:
- `aes128gcm` - AES-128-GCM (16-byte key)
- `aes256gcm` - AES-256-GCM (32-byte key)
- `aegis256` - AEGIS-256 (32-byte key) **[Recommended]**
- `aegis256x2`, `aegis256x4` - AEGIS-256 variants (32-byte key)
- `aegis128l`, `aegis128x2`, `aegis128x4` - AEGIS-128 variants (16-byte key)

## JavaScript Examples

- [`database-node`](./javascript/database-node/) — Node.js, local file database (no sync)
- [`database-wasm-vite`](./javascript/database-wasm-vite/) — Browser (WASM), local database in the browser
- [`sync-node`](./javascript/sync-node/) — Node.js with bidirectional sync to [Turso Cloud](https://turso.tech/)
- [`sync-wasm-vite`](./javascript/sync-wasm-vite/) — Browser (WASM) with bidirectional sync to [Turso Cloud](https://turso.tech/)
- [`encryption-node`](./javascript/encryption-node/) — Node.js with local database encryption