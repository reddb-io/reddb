# Storage Engine Encryption

The storage engine has cryptographic building blocks for an encrypted pager, but the v1.0 file format still writes normal pager pages.

## Current v1.0 Contract

- WAL integrity uses SHA-256 and hash chaining.
- HMAC and AES-GCM primitives are available to the engine.
- Page-encryption framing exists as foundation code.
- The main pager does not yet encrypt every page before writing.
- Archived WAL segments and snapshots rely on backend-side encryption if the operator needs at-rest protection.

## The Page Envelope (`reddb-io-crypto`)

The per-page encryption-at-rest envelope is owned by the `reddb-io-crypto`
authority crate (lib `reddb_crypto`, ADR 0054), paralleling `reddb-io-wire`
(protocol contracts) and `reddb-io-file` (on-disk artifacts) under ADR 0046. The
crate owns three things and nothing else:

- the canonical envelope byte-format — `encrypt_page` / `decrypt_page`,
  AES-256-GCM;
- the fixed crypto `params` (key/nonce/tag sizes, overhead, AEAD name) as
  constants, not configuration;
- key parsing (`parse_key`, hex or base64).

### Envelope layout

The envelope is a **magic-less frame**: a `12`-byte AES-GCM nonce, the
ciphertext, and a `16`-byte GCM tag, for a fixed **`28`-byte overhead** per page
(`NONCE_SIZE + TAG_SIZE`). The AEAD additional-authenticated-data binds the page
identifier as a `u32` little-endian value — the pager's native page-address
width, with `u32::MAX` reserved for the page-0 key-check slot.

| Field | Size | Notes |
|:------|:-----|:------|
| Nonce (IV) | 12 bytes | OS CSPRNG per page |
| Ciphertext | plaintext length | AES-256-GCM |
| Tag | 16 bytes | GCM authentication tag |
| **Overhead** | **28 bytes** | `NONCE_SIZE + TAG_SIZE` |

There is deliberately **no per-page magic or version**. The page-0
encrypted-database header is the self-describing authority for *whether* a
database is encrypted and under which salt — it lives in `reddb-io-file`
(`PAGED_ENCRYPTION_MARKER` = `b"RDBE"`, `PagedEncryptionHeader`), and the
fixed-size `60`-byte key-check slot (32-byte plaintext + 28 overhead) already
embeds this exact frame. A per-page magic would duplicate authority the page-0
header already holds, which ADR 0046 forbids. Future envelope changes (a new
AEAD or nonce scheme) version through that page-0 header, never through a silent
edit to `params`.

### How this differs from vault encryption

The page envelope is **not** the same surface as the configuration/secrets
vault. The vault protects secret *values* (users, API keys, config secrets) with
AES-256-GCM authenticated encryption and Argon2id key derivation, and is active
today — see [Encryption at Rest](../security/encryption.md) and the
[Vault](../security/vault.md) page. The page envelope is a *storage-engine*
contract for whole-page data and remains dormant in the live pager (the path
hardcodes `encryption: None`), so nothing is byte-persisted through it yet.

### Server facade

`reddb-server` keeps a thin delegating facade (`crypto::page_encryption`) that
re-exports the envelope and adds the server-specific `key_from_env`
(`RED_ENCRYPTION_KEY` / `RED_ENCRYPTION_KEY_FILE`) plus a `PageEncryptor`
convenience wrapper that binds a zeroizing `SecureKey` to the canonical free
functions. Per ADR 0046 the facade carries no second envelope.

## Why Pager Encryption Is Post-v1.0

Wiring encryption into the pager is a format change, not a toggle. It needs:

1. A file-format version bump.
2. Read support for plaintext v1 files.
3. Write support for encrypted v2 files.
4. Clear wrong-key refusal.
5. Downgrade refusal when an older binary sees a newer encrypted format.
6. Backup/restore drills over encrypted files.

Until that ships, do not describe RedDB v1.0 as encrypted-at-rest by default. Use cloud disk encryption, bucket-side encryption, dm-crypt/LUKS, or equivalent infrastructure controls.
