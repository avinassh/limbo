# Encrypted files from the old base commit

These files were made by Turso commit
`4b3befbd6cf76ea81cdb15580c9ac6dd9e061c45` (`0.9.0-pre.5`), before this
branch's connection encryption changes. They were not made by the current
database code. `manifest.json` records their sizes and SHA-256 hashes.

The four cases cover AES-256-GCM and AEGIS-256 in WAL and MVCC modes, with
8192-byte and 4096-byte pages respectively. The test key is 32 bytes of `01`.
It is synthetic test data. The table is:

```sql
CREATE TABLE legacy_secrets(id INTEGER PRIMARY KEY, value TEXT);
INSERT INTO legacy_secrets VALUES (1, 'legacy-one'), (2, 'legacy-two');
```

The old connection read and checked both rows before the snapshots were
copied. It stayed open while the main file and every nonempty sidecar were
copied. WAL cases include uncheckpointed committed frames in `-wal`. MVCC
cases include the logical records in `-log`; their WAL had no remaining
bytes, so no WAL fixture is needed. Always restore each main file together
with its listed sidecars. Do not combine files from different cases.

`s18_reads_encrypted_files_from_the_old_base_commit` checks the files through
the current code. It tries correct, missing, wrong, and different-cipher
credentials in cold and cached states. It also writes a new row and checks
all three rows after every old database reference has been dropped and the
file has been opened again.

To generate new snapshots, export the old commit into a fresh temporary
directory, copy `generate.rs` to `tests/examples/encryption_fixture.rs` there,
then run:

```bash
cargo run --offline --manifest-path /path/to/old-source/Cargo.toml \
  -p core_tester --example encryption_fixture -- /path/to/fixture-output
```

The producer is only a small example added to the old source tree. It uses
the old database, pager, WAL, encryption, and MVCC code unchanged. The source
files for those writers were checked against the old commit after generation.
Encryption uses random nonces, so running the producer again gives different
bytes and hashes. Update the manifest deliberately when replacing snapshots.

These samples establish compatibility with this particular older commit and
these four cases. They do not prove compatibility with every past release.
