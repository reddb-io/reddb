# Encryption at Rest

RedDB v1.0 does **not** promise pager-level encryption-at-rest by itself.

What exists today:

| Component | Status |
|:----------|:-------|
| Crypto primitives | AES-256-GCM, SHA-256, HMAC-SHA256, OS entropy helpers are available in the baseline build. |
| Page-encryption foundation | Framing/key-derivation support exists for the future encrypted pager format. |
| Pager data pages | Not encrypted by RedDB v1.0. |
| Archived WAL / backups | Not encrypted by RedDB v1.0; use object-store-side encryption. |
| Vault / secret values | Protected by the vault/secret pipeline when those features are configured. |

For production v1.0 deployments that require at-rest protection, use infrastructure encryption:

- Cloud volume encryption for disks.
- S3/R2/GCS bucket-side encryption or KMS-backed object encryption.
- dm-crypt/LUKS or equivalent for bare metal.
- Secret management via `_FILE` env companions and external KMS/Vault.

Pager-level encryption is a v1.1 candidate because it requires a file-format version bump, read-old/write-new migration path, wrong-key refusal tests, and downgrade protection.
