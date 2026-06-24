# reddb-io-crypto

The cryptographic authority crate for RedDB's page-level storage contracts.
It owns the canonical per-page encryption-at-rest envelope, fixed AES-GCM
parameters, and key parsing. Runtime policy stays in `reddb-io-server`; file
headers and encrypted-database markers stay in `reddb-io-file`.

## When to use it

Use this crate when you need to work with the exact encrypted-page envelope
RedDB storage uses or validates:

- encrypting or decrypting one database page with `encrypt_page` /
  `decrypt_page`;
- parsing a 32-byte AES-256 key from hex or base64 with `parse_key`;
- reading the fixed envelope constants: `KEY_SIZE`, `NONCE_SIZE`, `TAG_SIZE`,
  `PAGE_ENVELOPE_OVERHEAD`, and `AEAD_ALGORITHM`.

Most application code should not depend on this crate directly. The live engine
reaches it through the `reddb-io-server` facade when page encryption is wired
into a storage profile.

## Install

```toml
[dependencies]
reddb-io-crypto = "1.13"
```

The Rust import name is `reddb_crypto`:

```rust
use reddb_crypto::{decrypt_page, encrypt_page, parse_key, PAGE_ENVELOPE_OVERHEAD};

# fn run() -> Result<(), Box<dyn std::error::Error>> {
let key = parse_key("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
    .expect("valid 32-byte hex key");

let page_id = 7;
let plaintext = b"fixed-size page bytes";
let frame = encrypt_page(&key, page_id, plaintext)?;

assert_eq!(frame.len(), plaintext.len() + PAGE_ENVELOPE_OVERHEAD);
assert_eq!(decrypt_page(&key, page_id, &frame)?, plaintext);
# Ok(())
# }
```

## Envelope contract

The page frame is deliberately small and magic-less:

```text
[0..12]  random AES-GCM nonce
[12..]   ciphertext plus 16-byte authentication tag
```

The fixed overhead is 28 bytes. Additional authenticated data is the `page_id`
encoded as little-endian `u32`, so swapping encrypted pages between slots fails
closed during decryption.

Self-description lives one level up: `reddb-io-file` owns the page-0
`PAGED_ENCRYPTION_MARKER` (`b"RDBE"`), salt, and key-check slot. Future envelope
changes must version through that file-level header, not through silent changes
to this crate's constants.

## Boundary

- `reddb-io-crypto` owns: per-page frame bytes, AES-256-GCM parameters, nonce
  generation, key parsing, and `PageEnvelopeError`.
- `reddb-io-file` owns: disk markers, page-0 encrypted-database header, salts,
  and persisted file layout.
- `reddb-io-server` owns: configuration, environment variable lookup,
  zeroizing key storage, pager orchestration, and user-facing policy.

## Verification

```sh
cargo test -p reddb-io-crypto
cargo check -p reddb-io-crypto
```

## References

- [Encryption engine notes](../../docs/engine/encryption.md)
- [ADR 0046 - wire and file crate authority](../../.red/adr/0046-wire-file-crate-authority-boundary.md)
- [ADR 0054 - crypto page-envelope authority](../../.red/adr/0054-reddb-io-crypto-page-envelope-authority.md)
