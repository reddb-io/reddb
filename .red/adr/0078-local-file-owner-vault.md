# ADR 0078 — Local file-owner Vault

Status: accepted
Date: 2026-11-03

The browser and native local workspace hosts need encrypted credentials in their
private embedded `.rdb`, with ordinary metadata available before unlock. They
must not implement application ciphertext tables or reuse a remote query target.

## Decision

The shared `LocalDatabase` exposes a file-owner Vault API. `reddb-crypto` owns the
existing native passphrase KDF: Argon2id v1.3, 19 MiB, two iterations, one lane,
32-byte output. The server's v3 passphrase path delegates to it, preserving the
certificate path, auth page chain, auth fields and existing file contracts.

The embedded local Vault stores the native RDVX logical envelope in a reserved
`red.local.vault` collection with declared model Vault. Its plaintext is the
native auth KV payload; non-KV records are preserved verbatim. No ordinary SQL
mutation can reach the reserved collection and no ordinary table is substituted
for the Vault. This is a local credential provider, not server authorization,
tenant policy, versioned server Vault commands or encrypted-column support.
Server auth pages and this embedded provider have different physical placement;
the sealed payload, AES-GCM AAD and passphrase derivation are compatible.

Vault mutation produces an encrypted canonical store action. It joins the local
transaction's WAL publication or catalog snapshot, with the same interrupted-I/O
fence and rollback behavior as table writes. Initialization is explicit, outside
an active transaction. Existing records are authenticated before any write;
wrong passwords, corruption and unsupported formats never trigger replacement.

Cost sketch: a profile with 100 credentials at 1 KiB each seals about 200 KiB of
hex KV payload per edit, bounded to 1 MiB, with one canonical commit barrier;
Argon2's 19 MiB allocation occurs only on unlock, outside normal metadata commits.

The Worker session owns opaque zeroizing keys, scoped to a database name and its
random persisted Vault salt. Closing a file releases OPFS ownership but retains
its session key; explicit lock, session termination and Worker failure discard
it. Passwords and keys are never written to browser storage. Hosts still queue
cross-tab file operations using an outer Web Lock and retain the SDK owner fence.

A local backup contains encrypted credentials and unencrypted metadata and may
retain old deleted bytes. Full-file encryption, secure erasure, sync, server IAM
and hardware-backed protection are separate contracts.
