# MVCC Encryption Support — Implementation Plan

## Problem

Encryption does not work when MVCC is enabled. The MVCC logical log (`.db-log`) writes
all committed transaction data as **plaintext** to disk. Additionally, the MVCC bootstrap
and recovery paths explicitly bypass encryption-related validation, leaving the metadata
table uncreated and the replay boundary broken.

---

## Current State — Full Detail

### The logical log stores plaintext user data on disk

When a transaction commits under MVCC, the commit state machine
(`core/mvcc/database/mod.rs`, `CommitStateMachine`) calls
`mvcc_store.storage.log_tx(log_record)`, which calls
`LogicalLog::serialize_and_pwrite_tx()` (`core/mvcc/persistent_storage/logical_log.rs:309`).

The serialization path is:
1. `serialize_and_pwrite_tx()` iterates each `RowVersion` in the `LogRecord`
2. For each row, calls `serialize_op_entry(buffer, row_version)` (line 486)
3. `serialize_op_entry` writes: `tag(1) | flags(1) | table_id(4) | payload_len(varint) | payload`
4. The `payload` contains raw user data:
   - `OP_UPSERT_TABLE`: `rowid_varint || table_record_bytes` — the actual row content from `row_version.row.payload()`
   - `OP_DELETE_TABLE`: `rowid_varint`
   - `OP_UPSERT_INDEX`: serialized index key record from `row_version.row.payload()`
   - `OP_DELETE_INDEX`: serialized index key record
5. The entire frame is written via a single `file.pwrite(self.offset, buffer, completion)`

The file handle comes from `open_mv_store()` in `core/storage/journal_mode.rs:64-80`:
```rust
pub fn open_mv_store(
    io: Arc<dyn IO>,
    db_path: impl AsRef<std::path::Path>,
    flags: OpenFlags,
) -> Result<Arc<MvStore>> {
    let file = io.open_file(string_path, flags, false)?;
    let storage = mvcc::persistent_storage::Storage::new(file, io);
    // ...
}
```
No encryption key, cipher mode, or `EncryptionContext` is passed anywhere in this chain.

### The `open_mv_store` call sites have no encryption context

There are two call sites:

1. **Database open** (`core/lib.rs:1221-1224`):
   ```rust
   if open_mv_store {
       let mv_store =
           journal_mode::open_mv_store(self.io.clone(), &self.path, self.open_flags)?;
       self.mv_store.store(Some(mv_store));
   }
   ```
   This runs inside `header_validation()`, which receives `encryption_key: Option<&EncryptionKey>`.
   The key is available but never forwarded. The cipher mode is available at `self.encryption_cipher_mode`.

2. **PRAGMA journal_mode switch** (`core/vdbe/execute.rs:11899-11903`):
   ```rust
   let mv_store = journal_mode::open_mv_store(
       pager.io.clone(),
       &db_path,
       program.connection.db.open_flags,
   )?;
   ```
   The pager has the encryption context at this point, but it is not forwarded.

### Bootstrap bypass — metadata table never created for encrypted DBs

In `MvStore::bootstrap()` (`core/mvcc/database/mod.rs:2166`), the metadata table initialization
block is gated by `if !pager.is_encryption_enabled()` at line 2182.

The full bypass block (lines 2176-2218):
```rust
if self.uses_durable_mvcc_metadata(&bootstrap_conn) {
    match self.try_read_persistent_tx_ts_max(&bootstrap_conn)? {
        Some(_) => {}
        None => {
            let pager = bootstrap_conn.pager.load().clone();
            if !pager.is_encryption_enabled() {       // <-- BYPASS
                // ... corruption checks ...
                // ... truncate torn headers ...
                // ... write fresh header, fsync ...
                self.initialize_mvcc_metadata_table(&bootstrap_conn)?;
                self.maybe_complete_interrupted_checkpoint(&bootstrap_conn)?;
            }
        }
    }
}
```

When encryption IS enabled, this entire block is skipped:
- `__turso_internal_mvcc_meta` table is never created
- No corruption checks on the logical log
- No log header written

**Why was it added?** The `initialize_mvcc_metadata_table()` executes SQL (`CREATE TABLE IF NOT EXISTS`, `INSERT OR IGNORE`) that goes through the pager. The bypass was likely added when encryption couldn't decrypt pages at bootstrap time. However, by bootstrap time in the current code, the pager's encryption context IS initialized — `_init()` (`core/lib.rs:997`) calls `pager.set_encryption_context(cipher_mode, key)` at line 1009, and `header_validation()` runs before `BootstrapMvStore` in the `OpenDbAsyncPhase` state machine.

### Recovery bypass — persistent_tx_ts_max always 0 for encrypted DBs

In `maybe_recover_logical_log()` (`core/mvcc/database/mod.rs:4181`), line 4204:
```rust
let persistent_tx_ts_max = if self.uses_durable_mvcc_metadata(&connection) {
    match self.try_read_persistent_tx_ts_max(&connection)? {
        Some(ts) => ts,
        None if pager.is_encryption_enabled() => 0,  // <-- BYPASS
        None if header.is_none() => 0,
        None => return Err(LimboError::Corrupt("Missing MVCC metadata table"...))
    }
} else { 0 };
```

Since the metadata table was never created (bypass #1), `try_read_persistent_tx_ts_max()` returns `None` (it catches "no such table" as `Ok(None)` at line 2130). Without bypass #2, this would hit the final `None` arm and return a corruption error.

The consequence: `persistent_tx_ts_max` is always 0, so every recovery replays the ENTIRE logical log from the beginning.

### Cascade: checkpoint never writes persistent_tx_ts_max

In `CheckpointStateMachine::new()` (`core/mvcc/database/checkpoint_state_machine.rs:188-194`):
```rust
let durable_mvcc_metadata =
    !connection.db.path.starts_with(":memory:") && mvcc_meta_table.is_some();
```

`mvcc_meta_table` is resolved by looking up `__turso_internal_mvcc_meta` in the schema.
Since it was never created, `mvcc_meta_table` is `None`, `durable_mvcc_metadata` is `false`,
and `maybe_stage_mvcc_metadata_write()` (line 689-692) returns early:
```rust
if !self.durable_mvcc_metadata {
    return Ok(());
}
```

So the checkpoint never advances the durable replay boundary.

### What currently works with encryption + MVCC

- **Checkpoint writes**: The checkpoint state machine writes rows through the pager via
  `BTreeCursor`, which encrypts pages transparently. Data that reaches the main `.db` file
  IS encrypted.
- **WAL frames**: Encrypted through the pager.
- **B-tree cursor reads** (`MvccLazyCursor`): Go through the pager, decrypted transparently.
- **In-memory version chains** (`SkipMap<RowID, ...>`): Plaintext in RAM — expected and correct.
- **Existing encryption tests**: `tests/integration/query_processing/encryption.rs` has tests
  marked `#[turso_macros::test(mvcc)]` but they don't use `BEGIN CONCURRENT` and fall back to
  regular WAL behavior, so they pass "by accident."

### Existing test TODOs

- Line 11: `// TODO: mvcc does not error here` — `test_per_page_encryption`
- Line 198: `// TODO: mvcc for some reason does not error on corruption here` — `test_corruption_turso_magic_bytes`

---

## Logical Log File Format (Current)

```
FILE HEADER (56 bytes, little-endian)
  Offset  Size  Field          Value/Description
  0       4     magic          0x4C4D4C32 ("LML2" LE)
  4       1     version        2 (LOG_VERSION constant)
  5       1     flags          bits 1..7 must be 0
  6       2     hdr_len        >= 56, LE u16
  8       8     salt           random u64, regenerated on each truncation
  16      36    reserved       must be all zeros
  52      4     hdr_crc32c     CRC32C of header with this field zeroed

TRANSACTION FRAME (variable size, repeated N times)
  TX HEADER (14 bytes, TX_HEADER_SIZE)
    0     4     frame_magic    0x5854564D ("MVTX" LE)
    4     2     op_count       LE u16
    6     8     commit_ts      LE u64

  OP ENTRIES (repeated op_count times, variable total size)
    0     1     tag            OP_UPSERT_TABLE=0, OP_DELETE_TABLE=1,
                               OP_UPSERT_INDEX=2, OP_DELETE_INDEX=3
    1     1     flags          bit 0 = OP_FLAG_BTREE_RESIDENT
    2     4     table_id       i32 LE (must be negative)
    6     1-9   payload_len    SQLite varint
    var   var   payload        payload_len bytes

  TX TRAILER (12 bytes, TX_TRAILER_SIZE)
    0     4     payload_size   total bytes of all op entries, LE u32
    4     4     crc32c         chained CRC32C
    8     4     end_magic      0x4554564D ("MVTE" LE)
```

### CRC chain mechanics

1. **Seed**: `derive_initial_crc(salt)` = `crc32c(salt.to_le_bytes())` (line 261)
2. **Per-frame**: `crc32c_append(prev_frame_crc, tx_header_14_bytes || all_op_entry_bytes)`
3. **Op entry bytes fed into CRC**: `6_fixed_bytes || varint_raw_bytes || payload_bytes` — fed incrementally during streaming read (lines 797, 818-819, 832)
4. **Trailer NOT included in CRC**
5. **Chain advances**: After trailer validation, `self.running_crc = running_crc` (line 951)

### Streaming reader mechanics

`StreamingLogicalLogReader::parse_next_transaction()` (line 754):
- Reads in 4096-byte chunks via `read_more_data()` (line 1156)
- Parses incrementally — does NOT buffer the entire frame
- For each op: reads 6 fixed bytes, reads varint, reads `payload_len` bytes, feeds each into CRC
- After all ops: reads 12-byte trailer, validates `payload_size`, `crc32c`, `end_magic`
- On any invalid frame: treated as torn tail (EOF), previous frames kept
- Tracks `last_valid_offset` for the writer to resume from

---

## Encryption Context API

```rust
// core/storage/encryption.rs

pub enum EncryptionKey { Key128([u8; 16]), Key256([u8; 32]) }  // Clone

pub enum CipherMode { // Clone, Copy, PartialEq
    None, Aes128Gcm, Aes256Gcm, Aegis256, Aegis128L,
    Aegis128X2, Aegis128X4, Aegis256X2, Aegis256X4,
}

pub struct EncryptionContext {  // Clone
    cipher_mode: CipherMode,
    cipher: Cipher,    // Clone
    page_size: usize,  // not used for raw encrypt/decrypt
}
```

**The key methods we need (currently private, line 808 and 834):**

```rust
// encrypt_raw: generates random nonce, returns (ciphertext || auth_tag, nonce)
// Output length = plaintext.len() + 16  (tag is always 16 bytes)
fn encrypt_raw(&self, plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)>

// decrypt_raw: given ciphertext_with_appended_tag and nonce, returns plaintext
fn decrypt_raw(&self, ciphertext_with_tag: &[u8], nonce: &[u8]) -> Result<Vec<u8>>
```

For AEGIS ciphers (line 196-199): `encrypt()` returns `(ciphertext, nonce)` where ciphertext
has tag appended: `let mut result = ciphertext; result.extend_from_slice(&tag);`

For AES-GCM ciphers (line 253-263): `encrypt()` returns `(ciphertext_with_tag, nonce)` via
aes_gcm's `Aead::encrypt` which appends the tag.

**`decrypt_raw` splits internally** (line 206): `let (ct, tag) = ciphertext.split_at(ciphertext.len() - TAG_SIZE);`

**Per-cipher overhead:**

| Cipher | Nonce size | Tag size | Total overhead per record |
|--------|-----------|----------|--------------------------|
| AES-128-GCM | 12 | 16 | 28 bytes |
| AES-256-GCM | 12 | 16 | 28 bytes |
| AEGIS-128L/X2/X4 | 16 | 16 | 32 bytes |
| AEGIS-256/X2/X4 | 32 | 16 | 48 bytes |

---

## Design: Per-Record Encryption

Encrypt each op's payload individually. Structural metadata (tag, flags, table_id) stays
plaintext so the streaming reader can parse frame boundaries without decryption.

### Encrypted op entry layout

```
UNENCRYPTED (same as today):
  tag(1) | flags(1) | table_id(4) | payload_len(varint)

ENCRYPTED BLOB (payload_len bytes total):
  ciphertext(plaintext_len) | auth_tag(16) | nonce(nonce_size)
```

Where:
- `payload_len` = `plaintext_len + 16 + nonce_size` (everything the reader must consume)
- `encrypt_raw(plaintext)` returns `(ciphertext || auth_tag, nonce)` — length = `plaintext_len + 16`
- Writer: writes `ciphertext_with_tag` then `nonce`
- Reader: reads `payload_len` bytes, splits off last `nonce_size` bytes as nonce, remainder is `ciphertext_with_tag`, calls `decrypt_raw(ct_with_tag, nonce)` to get plaintext

### What stays plaintext
- **File header** (56 bytes) — no user data, only structural/integrity metadata
- **TX header** (frame_magic, op_count, commit_ts) — needed for frame boundary detection and recovery filtering without decryption
- **Op structural fields** (tag, flags, table_id, payload_len) — needed for streaming parsing
- **TX trailer** (payload_size, crc32c, end_magic) — integrity validation

### CRC behavior
- CRC covers **ciphertext** (computed after encryption on write, before decryption on read)
- No change to CRC chain logic — it operates on the bytes that are on disk
- AEAD auth tag provides per-record integrity/authenticity; CRC chain provides frame-level ordering integrity

### Log header changes
- Store `cipher_id` (1 byte, from `CipherMode::cipher_id()`) at byte offset 16 in the reserved region
- Remaining reserved bytes 17-51 stay zero
- Bump `LOG_VERSION` from 2 to 3
- On read: `cipher_id` tells recovery which cipher was used → determines `nonce_size` + `tag_size`

---

## Gotchas To Keep In Mind When Implementing

### 1. `encrypt_raw` / `decrypt_raw` are private

Both methods on `EncryptionContext` are `fn` not `pub fn` (lines 808, 834 of
`core/storage/encryption.rs`). They must be made `pub` or wrapped with public methods.
This is the only change needed in encryption.rs — the methods already work on arbitrary
byte buffers and are not coupled to page sizes.

### 2. `EncryptionContext::new()` takes `page_size` but the log doesn't have pages

`EncryptionContext::new(cipher_mode, key, page_size)` requires a `page_size: usize`
(line 493). The `page_size` field is only used by `encrypt_page`/`decrypt_page` (for
assertions and reserved-byte calculations), NOT by `encrypt_raw`/`decrypt_raw`.

**Solution**: When constructing the `EncryptionContext` for the logical log, pass a dummy
page_size (e.g., 4096) or, better, clone the pager's existing `EncryptionContext` which
already has the correct cipher+key. `EncryptionContext` derives `Clone`.

Alternatively: in `header_validation()` the `EncryptionContext` doesn't exist yet as a
standalone object — it's set on the pager via `pager.set_encryption_context(cipher_mode, key)`.
You need to construct a new `EncryptionContext::new(cipher_mode, key, page_size)` locally.
The page_size at this point is known from the header or from `Pager::page_size()`.

### 3. `serialize_op_entry` is a free function — needs encryption context threaded in

Currently: `fn serialize_op_entry(buffer: &mut Vec<u8>, row_version: &RowVersion) -> Result<()>`

It's called in `serialize_and_pwrite_tx` at line 342:
```rust
for row_version in &tx.row_versions {
    serialize_op_entry(&mut self.write_buf, row_version)?;
}
```

The function writes `payload_len` varint then payload bytes directly into the buffer.
With encryption, it needs to:
1. Build the plaintext payload into a temporary buffer
2. Encrypt it
3. Write `encrypted_len` as the varint (not plaintext len)
4. Write `ciphertext_with_tag || nonce` into the main buffer

**Must not encrypt in-place in the main buffer** because the varint length changes — the
encrypted payload is larger than the plaintext by `nonce_size + tag_size` bytes.

### 4. payload_len varint encodes the encrypted size, not plaintext size

On the write side, `payload_len` must be `plaintext_len + tag_size + nonce_size`.
On the read side, the reader reads `payload_len` bytes, then splits off the crypto overhead.

This means the trailer's `payload_size` (total bytes of all op entries) and the running
`payload_bytes_read` counter also reflect encrypted sizes. Since both writer and reader use
the same convention, this is consistent. No change to trailer validation logic.

### 5. CRC must cover ciphertext, not plaintext

The CRC chain in `serialize_and_pwrite_tx` (line 350):
```rust
let crc = crc32c::crc32c_append(
    self.running_crc,
    &self.write_buf[tx_header_start..payload_end],
);
```
This already computes CRC over whatever is in `write_buf` after serialization. Since
encryption happens inside `serialize_op_entry` (before this CRC computation), the CRC
will naturally cover ciphertext. No change needed.

On the read side, `parse_next_transaction` feeds bytes into CRC as they're read from disk
(lines 797, 818-819, 832), which is ciphertext. Decryption happens AFTER CRC update. Correct.

### 6. Recovery: decryption errors on torn tails

A torn write could produce a partial encrypted payload. When `decrypt_raw` fails (AEAD
tag mismatch on corrupt data), it returns an error, NOT `ParseResult::InvalidFrame`.

The current read path treats any structural error as `InvalidFrame` (torn tail = EOF). But
a decryption error from `decrypt_raw` would be `Err(LimboError::...)`, which would propagate
up and crash recovery.

**Must catch decryption errors and treat them as InvalidFrame (torn tail):**
```rust
let payload = if let Some(enc) = &self.encryption_context {
    let nonce_size = enc.cipher_mode().nonce_size();
    if payload.len() < nonce_size + 16 {
        self.last_valid_offset = frame_start;
        return Ok(ParseResult::InvalidFrame);
    }
    let (ct_with_tag, nonce) = payload.split_at(payload.len() - nonce_size);
    match enc.decrypt_raw(ct_with_tag, nonce) {
        Ok(plaintext) => plaintext,
        Err(_) => {
            self.last_valid_offset = frame_start;
            return Ok(ParseResult::InvalidFrame);
        }
    }
} else {
    payload
};
```

### 7. Backward compatibility — reading version 2 (unencrypted) logs

After the change, the reader must handle both:
- **Version 2 logs** (`cipher_id` implicitly 0, reserved bytes all zero): no decryption
- **Version 3 logs** (`cipher_id` in reserved[0]): decrypt with matching cipher

The current `LogHeader::decode()` (line 180) rejects `version != LOG_VERSION` at line 191:
```rust
if version != LOG_VERSION {
    return Err(LimboError::Corrupt(...));
}
```

**Must accept both version 2 and version 3.** When version == 2, `cipher_id` is 0 (from the
all-zero reserved bytes). When version == 3, read `cipher_id` from `reserved[0]`.

The writer should always write version 3 when encryption is enabled, version 2 when not
(to preserve backward compat for unencrypted databases — old code can still read the log).

### 8. cipher_id mismatch detection

After reading the log header, validate:
- `cipher_id != 0` but no `EncryptionContext` → error (encrypted log, no key)
- `cipher_id == 0` but `EncryptionContext` is Some → **this is valid** — it means the log was
  written without encryption (e.g., fresh truncation before first encrypted write). Don't error.
  Or it could be a version 2 log from before encryption was enabled.
- `cipher_id != 0` and `EncryptionContext` cipher doesn't match `cipher_id` → error (wrong cipher)

### 9. Empty / small payloads still work

`OP_DELETE_TABLE` payload is just a varint rowid — could be 1 byte.
Encrypting 1 byte: 1 (ciphertext) + 16 (tag) + 12-32 (nonce) = 29-49 bytes.
This is fine — deletes are small and overhead is acceptable.

### 10. The `#[cfg(feature = "encryption")]` gating

`encrypt_raw`/`decrypt_raw` are only available when the `encryption` feature is enabled.
Without the feature, they return errors. The logical log code must handle this:
- When `encryption` feature is off, `EncryptionContext` should never be `Some` (the database
  open path won't construct one without the feature).
- The `serialize_op_entry` and reader code should compile cleanly without the feature by
  gating on `Option<EncryptionContext>` being `None`.

### 11. Bootstrap bypass may not be safely removable without testing

The bypass at line 2182 was added because something broke with encrypted databases during
bootstrap. The open flow is:
```
Init → header_validation() → ReadingHeader → LoadingSchema → BootstrapMvStore
```
In `_init()` (line 997), encryption is set up at line 1007-1009:
```rust
if let Some(key) = encryption_key {
    let cipher_mode = self.encryption_cipher_mode.get();
    pager.set_encryption_context(cipher_mode, key)?;
}
```
This runs inside `header_validation()`, which is called in `Init`. By `BootstrapMvStore`,
the pager should be able to encrypt/decrypt pages. The `initialize_mvcc_metadata_table()`
executes `CREATE TABLE` and `INSERT` through a connection that has the encryption key
(`db._connect(true, Some(pager.clone()), state.encryption_key.clone())`).

**This should work**, but must be verified with tests. If it fails, the issue is likely
that the connection's `set_encryption_context` call hasn't propagated before the SQL runs.

### 12. PRAGMA journal_mode switch has no encryption context to forward

In `core/vdbe/execute.rs:11899`, the PRAGMA handler creates a new MvStore at runtime.
At this point the pager already has the encryption context set. You can get it via
`pager.is_encryption_ctx_set()` to check, but there's no public getter to extract the
`EncryptionContext` from the pager.

**Options:**
- Add a `pub fn encryption_context(&self) -> Option<EncryptionContext>` to `Pager` that
  clones the context, OR
- Reconstruct from `db.encryption_cipher_mode` + the key (but the key is not stored on
  `Database` for security — it's only in the pager's `io_ctx`), OR
- Store the `EncryptionContext` on `Database` when it's first constructed, so both the
  pager and the MvStore can access it.

The cleanest approach: add a clone-based getter to `Pager`.

### 13. `MvStore` needs to expose encryption context for the recovery reader

`maybe_recover_logical_log()` (line 4181) creates `StreamingLogicalLogReader::new(file)`.
The reader needs the `EncryptionContext`. The context lives in `Storage` → `LogicalLog`.
Either:
- `Storage` exposes a method to get the context, OR
- `MvStore` stores a copy of the `EncryptionContext` separately, OR
- The recovery method reads it from `self.storage.logical_log.read().encryption_context`

---

## Implementation — Full Detail

### Change 1: `core/storage/encryption.rs` — Expose raw encrypt/decrypt

Make the two methods public:

```rust
// line 808: change `fn` to `pub fn`
pub fn encrypt_raw(&self, plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {

// line 834: change `fn` to `pub fn`
pub fn decrypt_raw(&self, ciphertext_with_tag: &[u8], nonce: &[u8]) -> Result<Vec<u8>> {
```

No other changes in this file.

### Change 2: `core/mvcc/persistent_storage/logical_log.rs` — LogHeader

**Add `cipher_id` field to `LogHeader` struct** (line 144):
```rust
pub(crate) struct LogHeader {
    version: u8,
    flags: u8,
    hdr_len: u16,
    pub(crate) salt: u64,
    hdr_crc32c: u32,
    cipher_id: u8,                           // NEW
    reserved: [u8; LOG_HDR_RESERVED_SIZE - 1], // shrink by 1
}
```

**Update `LogHeader::new()`** (line 154):
```rust
pub(crate) fn new(io: &Arc<dyn crate::IO>, cipher_id: u8) -> Self {
    Self {
        version: if cipher_id != 0 { 3 } else { LOG_VERSION },
        // ...
        cipher_id,
        reserved: [0; LOG_HDR_RESERVED_SIZE - 1],
    }
}
```

**Update `encode()`** (line 165):
```rust
fn encode(&self) -> [u8; LOG_HDR_SIZE] {
    // ... existing fields ...
    buf[LOG_HDR_RESERVED_START] = self.cipher_id;       // byte 16
    buf[LOG_HDR_RESERVED_START + 1..LOG_HDR_CRC_START]  // bytes 17-51
        .copy_from_slice(&self.reserved);
    // ... CRC ...
}
```

**Update `decode()`** (line 180):
```rust
fn decode(buf: &[u8]) -> Result<Self> {
    // ... magic check ...
    let version = buf[4];
    if version != 2 && version != 3 {                // accept both versions
        return Err(...);
    }
    // ... existing checks ...
    let cipher_id = buf[LOG_HDR_RESERVED_START];      // byte 16
    let mut reserved = [0u8; LOG_HDR_RESERVED_SIZE - 1];
    reserved.copy_from_slice(&buf[LOG_HDR_RESERVED_START + 1..LOG_HDR_CRC_START]);
    if reserved.iter().any(|b| *b != 0) {             // bytes 17-51 must be zero
        return Err(...);
    }
    Ok(Self { version, flags, hdr_len, salt, hdr_crc32c, cipher_id, reserved })
}
```

**Add accessor:**
```rust
pub(crate) fn cipher_id(&self) -> u8 {
    self.cipher_id
}
```

### Change 3: `core/mvcc/persistent_storage/logical_log.rs` — LogicalLog (writer)

**Add encryption field to struct** (line 265):
```rust
pub struct LogicalLog {
    pub file: Arc<dyn File>,
    io: Arc<dyn crate::IO>,
    pub offset: u64,
    write_buf: Vec<u8>,
    header: Option<LogHeader>,
    pub running_crc: u32,
    pending_running_crc: Option<u32>,
    encryption_context: Option<EncryptionContext>,  // NEW
}
```

**Update `new()`** (line 282):
```rust
pub fn new(file: Arc<dyn File>, io: Arc<dyn crate::IO>,
           encryption_context: Option<EncryptionContext>) -> Self {
    Self {
        // ... existing fields ...
        encryption_context,
    }
}
```

**Add accessor:**
```rust
pub fn encryption_context(&self) -> Option<&EncryptionContext> {
    self.encryption_context.as_ref()
}
```

**Update `serialize_and_pwrite_tx()`** (line 309):
- At line 320, when creating the header for first write:
  ```rust
  let cipher_id = self.encryption_context.as_ref()
      .map(|e| e.cipher_mode().cipher_id())
      .unwrap_or(0);
  let header = LogHeader::new(&self.io, cipher_id);
  ```
- At line 342, pass encryption context to `serialize_op_entry`:
  ```rust
  serialize_op_entry(&mut self.write_buf, row_version, self.encryption_context.as_ref())?;
  ```

**Rewrite `serialize_op_entry()`** (line 486):

Change signature:
```rust
fn serialize_op_entry(
    buffer: &mut Vec<u8>,
    row_version: &RowVersion,
    encryption: Option<&EncryptionContext>,
) -> Result<()> {
```

The core change is in how payload is written. Currently each match arm writes
`payload_len` varint then payload bytes directly. With encryption:

```rust
// For each match arm (OP_UPSERT_TABLE shown as example):
OP_UPSERT_TABLE => {
    let RowKey::Int(rowid) = row_version.row.id.row_id else {
        unreachable!("table ops must have RowKey::Int")
    };
    let record_bytes = row_version.row.payload();
    let rowid_u64 = rowid as u64;
    let rowid_len = varint_len(rowid_u64);

    // Build plaintext payload in a temp buffer
    let mut plaintext = Vec::with_capacity(rowid_len + record_bytes.len());
    write_varint_to_vec(rowid_u64, &mut plaintext);
    plaintext.extend_from_slice(record_bytes);

    if let Some(enc) = encryption {
        let (ct_with_tag, nonce) = enc.encrypt_raw(&plaintext)?;
        let encrypted_len = ct_with_tag.len() + nonce.len();
        write_varint_to_vec(encrypted_len as u64, buffer);
        buffer.extend_from_slice(&ct_with_tag);
        buffer.extend_from_slice(&nonce);
    } else {
        write_varint_to_vec(plaintext.len() as u64, buffer);
        buffer.extend_from_slice(&plaintext);
    }
}
```

Same pattern for `OP_DELETE_TABLE`, `OP_UPSERT_INDEX`, `OP_DELETE_INDEX`.

### Change 4: `core/mvcc/persistent_storage/logical_log.rs` — StreamingLogicalLogReader (reader)

**Add encryption field** (line 596):
```rust
pub struct StreamingLogicalLogReader {
    // ... existing fields ...
    encryption_context: Option<EncryptionContext>,  // NEW
}
```

**Update `new()`**:
```rust
pub fn new(file: Arc<dyn File>, encryption_context: Option<EncryptionContext>) -> Self {
    // ... pass through ...
}
```

**Update `parse_next_transaction()`** — insert decryption after line 832:

```rust
let payload = match self.try_consume_bytes(io, payload_len)? {
    Some(bytes) => bytes,
    None => return Ok(ParseResult::Eof),
};
running_crc = crc32c::crc32c_append(running_crc, &payload);

// --- NEW: decrypt payload if encrypted ---
let payload = if let Some(enc) = &self.encryption_context {
    let nonce_size = enc.cipher_mode().nonce_size();
    let min_overhead = nonce_size + 16; // nonce + tag
    if payload.len() < min_overhead {
        self.last_valid_offset = frame_start;
        return Ok(ParseResult::InvalidFrame);
    }
    let (ct_with_tag, nonce) = payload.split_at(payload.len() - nonce_size);
    match enc.decrypt_raw(ct_with_tag, nonce) {
        Ok(plaintext) => plaintext,
        Err(_) => {
            // Decryption failure = corrupted/torn frame → treat as EOF
            self.last_valid_offset = frame_start;
            return Ok(ParseResult::InvalidFrame);
        }
    }
} else {
    payload
};
// --- END NEW ---

// Existing op parsing uses decrypted `payload` — no changes below
let op_total_bytes = 6 + payload_len_bytes_len + payload_len;
```

**Note**: `op_total_bytes` and `payload_bytes_read` use `payload_len` (the on-disk encrypted
size), NOT the decrypted size. This is correct because `payload_size` in the trailer also
reflects encrypted sizes.

**Add header cipher validation** after `try_read_header()`:
```rust
// After successfully reading header:
if let Some(header) = &self.header {
    let log_cipher_id = header.cipher_id();
    match (&self.encryption_context, log_cipher_id) {
        (None, 0) => {} // unencrypted log, no key — OK
        (Some(enc), id) if id == enc.cipher_mode().cipher_id() => {} // match
        (None, _) => return Err(LimboError::Corrupt(
            "Encrypted logical log but no encryption key provided".into()
        )),
        (Some(enc), 0) => {} // unencrypted log with key — OK (pre-encryption data)
        (Some(_), _) => return Err(LimboError::Corrupt(
            "Logical log cipher mismatch".into()
        )),
    }
}
```

### Change 5: `core/mvcc/persistent_storage/mod.rs` — Storage

**Update `Storage::new()`** (line 20):
```rust
pub fn new(file: Arc<dyn File>, io: Arc<dyn crate::IO>,
           encryption_context: Option<EncryptionContext>) -> Self {
    Self {
        logical_log: RwLock::new(LogicalLog::new(file, io, encryption_context)),
        log_offset: AtomicU64::new(0),
        checkpoint_threshold: AtomicI64::new(DEFAULT_LOG_CHECKPOINT_THRESHOLD),
    }
}
```

**Add accessor:**
```rust
pub fn encryption_context(&self) -> Option<EncryptionContext> {
    self.logical_log.read().encryption_context().cloned()
}
```

### Change 6: `core/storage/journal_mode.rs` — open_mv_store

**Update signature** (line 64):
```rust
pub fn open_mv_store(
    io: Arc<dyn IO>,
    db_path: impl AsRef<std::path::Path>,
    flags: OpenFlags,
    encryption_context: Option<EncryptionContext>,  // NEW
) -> Result<Arc<MvStore>> {
    let db_path = db_path.as_ref();
    let log_path = db_path.with_extension("db-log");
    let string_path = log_path.as_os_str().to_str().expect("path should be valid string");
    let file = io.open_file(string_path, flags, false)?;
    let storage = mvcc::persistent_storage::Storage::new(file, io, encryption_context);
    let mv_store = MvStore::new(mvcc::LocalClock::new(), storage);
    let mv_store = Arc::new(mv_store);
    Ok(mv_store)
}
```

### Change 7: `core/lib.rs` — Database open path

**In `header_validation()`** (line 1221-1224):
```rust
if open_mv_store {
    let enc_ctx = encryption_key.map(|key| {
        let cipher_mode = self.encryption_cipher_mode.get();
        EncryptionContext::new(cipher_mode, key, pager.page_size())
    }).transpose()?;
    let mv_store = journal_mode::open_mv_store(
        self.io.clone(), &self.path, self.open_flags, enc_ctx)?;
    self.mv_store.store(Some(mv_store));
}
```

The `encryption_key` parameter is `Option<&EncryptionKey>`, available as the function argument.
The `cipher_mode` is on `self.encryption_cipher_mode` (an `AtomicCipherMode`).
The `page_size` is available from `pager.page_size()` (used as a dummy for encrypt_raw).

### Change 8: `core/vdbe/execute.rs` — PRAGMA journal_mode switch

**At line 11899**, need the encryption context. Add a getter to `Pager`:

In `core/storage/pager.rs`, add:
```rust
pub fn get_encryption_context(&self) -> Option<EncryptionContext> {
    self.io_ctx.read().encryption_context().cloned()
}
```

Then in execute.rs:
```rust
let enc_ctx = pager.get_encryption_context();
let mv_store = journal_mode::open_mv_store(
    pager.io.clone(),
    &db_path,
    program.connection.db.open_flags,
    enc_ctx,
)?;
```

### Change 9: `core/mvcc/database/mod.rs` — Remove bootstrap bypass

**At line 2182**, remove the `if !pager.is_encryption_enabled()` condition.
The full block should run unconditionally:

```rust
None => {
    let log_size = self.get_logical_log_file().size()?;
    let pager = bootstrap_conn.pager.load().clone();
    // REMOVED: if !pager.is_encryption_enabled() {
    if bootstrap_conn.db.is_readonly() {
        return Err(LimboError::Corrupt(
            "Missing MVCC metadata table in read-only mode".to_string(),
        ));
    }
    if log_size > LOG_HDR_SIZE as u64 {
        return Err(LimboError::Corrupt(
            "Missing MVCC metadata table while logical log state exists".to_string(),
        ));
    }
    // ... truncate torn headers, write fresh header, fsync ...
    self.initialize_mvcc_metadata_table(&bootstrap_conn)?;
    self.maybe_complete_interrupted_checkpoint(&bootstrap_conn)?;
    // REMOVED: }
}
```

### Change 10: `core/mvcc/database/mod.rs` — Remove recovery bypass

**At line 4204**, remove the `None if pager.is_encryption_enabled() => 0` arm:

```rust
let persistent_tx_ts_max = if self.uses_durable_mvcc_metadata(&connection) {
    match self.try_read_persistent_tx_ts_max(&connection)? {
        Some(ts) => ts,
        // REMOVED: None if pager.is_encryption_enabled() => 0,
        None if header.is_none() => 0,
        None => {
            return Err(LimboError::Corrupt(
                "Missing MVCC metadata table".to_string(),
            ))
        }
    }
} else { 0 };
```

### Change 11: `core/mvcc/database/mod.rs` — Recovery reader with encryption

**In `maybe_recover_logical_log()`** (line 4184):
```rust
// Current:
let mut reader = StreamingLogicalLogReader::new(file.clone());

// Change to:
let enc_ctx = self.storage.encryption_context();
let mut reader = StreamingLogicalLogReader::new(file.clone(), enc_ctx);
```

### Change 12: `core/mvcc/persistent_storage/logical_log.rs` — truncate preserves cipher_id

**In `LogicalLog::truncate()`** (line 466), when regenerating the header after truncation:
```rust
pub fn truncate(&mut self) -> Result<Completion> {
    let mut header = self.current_or_new_header()?;
    header.salt = self.io.generate_random_number() as u64;
    // cipher_id is preserved from the existing header — no change needed
    self.running_crc = derive_initial_crc(header.salt);
    // ...
}
```

The `current_or_new_header()` returns the existing header (with cipher_id) if available.
If creating a new one (only on first truncation before any write), cipher_id defaults to 0,
which is correct — the first write will set it.

Actually, `LogHeader::new()` now takes cipher_id. Need to also update
`current_or_new_header()` fallback to pass the right cipher_id:
```rust
fn current_or_new_header(&self) -> Result<LogHeader> {
    if let Some(header) = self.header.clone() {
        return Ok(header);
    }
    if self.offset == 0 {
        let cipher_id = self.encryption_context.as_ref()
            .map(|e| e.cipher_mode().cipher_id())
            .unwrap_or(0);
        return Ok(LogHeader::new(&self.io, cipher_id));
    }
    Err(LimboError::InternalError("Logical log header not initialized".to_string()))
}
```

---

## What Does NOT Need Changes

| Component | Reason |
|-----------|--------|
| TX header (14 bytes) | No user data, stays plaintext |
| TX trailer (12 bytes) | CRC covers ciphertext, no change to validation |
| CRC chain logic | Operates on disk bytes (ciphertext) — no change |
| Checkpoint state machine | Reads from in-memory store, writes through pager (already encrypted) |
| MVCC cursor (`MvccLazyCursor`) | B-tree reads go through pager; in-memory data is plaintext |
| In-memory SkipMaps | RAM data doesn't need encryption |
| `tag`, `flags`, `table_id` per-op fields | Structural metadata, stays plaintext for streaming |
| `parsed_op_to_streaming()` | Receives decrypted payload — no change |
| Recovery replay logic (lines 4341+) | Consumes `StreamingResult` from reader — no change |

---

## Testing

### New tests needed
1. **Basic MVCC + encryption round-trip**: CREATE TABLE, INSERT rows, SELECT — verify data correct
2. **Recovery with encryption**: commit rows, close DB (before checkpoint), reopen, verify rows
3. **Checkpoint + encryption**: commit, checkpoint, close, reopen — verify data in `.db` file
4. **Wrong key**: write encrypted log, reopen with different key → must error
5. **No key for encrypted log**: write encrypted log, reopen without key → must error
6. **Cipher mismatch**: write with AEGIS-256, reopen claiming AES-128-GCM → must error
7. **Unencrypted log still readable**: write with no encryption, code change deployed, reopen → works
8. **Multiple transactions**: commit several transactions, recover, verify all rows
9. **BEGIN CONCURRENT + encryption**: concurrent MVCC transactions with encryption
10. **`.db-log` is actually encrypted**: read raw `.db-log` bytes, verify they don't contain plaintext row data

### Existing tests to fix
- `tests/integration/query_processing/encryption.rs:11` — `// TODO: mvcc does not error here`
- `tests/integration/query_processing/encryption.rs:198` — `// TODO: mvcc for some reason does not error on corruption here`

Both should be resolved once the bootstrap/recovery bypasses are removed.
