# Storage Engine Encryption

The storage engine has cryptographic building blocks for an encrypted pager, but the v1.0 file format still writes normal pager pages.

## Current v1.0 Contract

- WAL integrity uses SHA-256 and hash chaining.
- HMAC and AES-GCM primitives are available to the engine.
- Page-encryption framing exists as foundation code.
- The main pager does not yet encrypt every page before writing.
- Archived WAL segments and snapshots rely on backend-side encryption if the operator needs at-rest protection.

## Why Pager Encryption Is Post-v1.0

Wiring encryption into the pager is a format change, not a toggle. It needs:

1. A file-format version bump.
2. Read support for plaintext v1 files.
3. Write support for encrypted v2 files.
4. Clear wrong-key refusal.
5. Downgrade refusal when an older binary sees a newer encrypted format.
6. Backup/restore drills over encrypted files.

Until that ships, do not describe RedDB v1.0 as encrypted-at-rest by default. Use cloud disk encryption, bucket-side encryption, dm-crypt/LUKS, or equivalent infrastructure controls.
