# Encryption at Rest

RedDB supports page-level encryption using AES-256-GCM. See [Storage Engine Encryption](/engine/encryption.md) for implementation details.

## Enabling

Compile with the `encryption` feature flag:

```toml
[dependencies]
reddb = { version = "0.1", features = ["encryption"] }
```

## What's Encrypted

| Component | Encrypted |
|:----------|:----------|
| Data pages | Yes (with `encryption` feature) |
| WAL entries | Yes (with `encryption` feature) |
| Vault pages | Always (when `--vault` is used) |
| Database header | No (needed for identification) |

## Key Management

The encryption key is derived from:

1. A user-provided passphrase (for embedded use)
2. The vault seal (for server mode with `--vault`)

## Performance Impact

Encryption adds approximately 5-15% overhead to I/O operations due to AES-GCM encrypt/decrypt on every page read and write.

> [!TIP]
> Enable encryption only when regulatory or security requirements mandate data-at-rest protection. For most development and testing scenarios, it is not needed.
