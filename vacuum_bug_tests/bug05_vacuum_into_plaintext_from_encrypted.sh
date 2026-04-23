#!/usr/bin/env bash
# Bug 5: VACUUM INTO on encrypted source produces plaintext output.
# VACUUM-specific: core/vdbe/execute.rs::VacuumIntoOpPhase::Init opens the
# output database without forwarding encryption opts (unlike in-place VACUUM
# which uses open_vacuum_temp_db + connect_with_encryption correctly).
set -u
D=$(mktemp -d); trap "rm -rf $D" EXIT
TURSO="cargo run --manifest-path /home/ubuntu/limbo/Cargo.toml --bin tursodb -q -- --experimental-encryption -m list -q"

$TURSO "$D/enc.db" 2>/dev/null <<'SQL'
PRAGMA cipher='aes256gcm';
PRAGMA hexkey='000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f';
CREATE TABLE secrets(u, pw);
INSERT INTO secrets VALUES ('user1', 'sensitive_password_abc');
VACUUM INTO '/tmp/enc_out.db';
SQL

echo '---Source header (first 16 bytes)---'
head -c 16 "$D/enc.db"; echo
echo '---Dest header (should be encrypted too, but it is plaintext)---'
head -c 16 /tmp/enc_out.db; echo
echo '---Strings: sensitive data visible in plaintext dest---'
strings /tmp/enc_out.db | grep -E 'user1|sensitive' | head
echo '---Dest readable without key (confirming plaintext)---'
/home/ubuntu/sqlite/sqlite3 /tmp/enc_out.db 'SELECT * FROM secrets;' 2>&1
rm -f /tmp/enc_out.db /tmp/enc_out.db-wal

# Bug manifests: dest starts with "SQLite format 3" (plaintext) and sensitive
# data is grep-able / readable with unencrypted sqlite3.
