# ADR 0054 — `reddb-io-crypto`: per-page encryption-envelope authority

Status: accepted
Date: 2026-06-11

## Decision

The per-page encryption-at-rest envelope and its mandatory encrypt parameters
move to a new authority crate **`reddb-io-crypto`** (lib `reddb_crypto`),
paralleling `reddb-io-file` (on-disk artifacts) and `reddb-io-wire` (protocol
contracts) under ADR 0046. The crate owns:

- the canonical per-page envelope byte-format (`encrypt_page` / `decrypt_page`,
  AES-256-GCM);
- the fixed crypto parameters (`params`: key/nonce/tag sizes, overhead, AEAD
  name);
- key parsing (`parse_key`, hex or base64).

`reddb-server` keeps a thin facade (`crypto::page_encryption`) that re-exports
the envelope and adds the server-specific `key_from_env`
(`RED_ENCRYPTION_KEY[_FILE]`), and a `PageEncryptor` convenience wrapper that
binds a zeroizing `SecureKey` to the canonical free functions. Per ADR 0046,
these are delegating shims — they carry no second envelope.

## Which envelope survived, and why

Two dormant, byte-incompatible envelopes existed for the same not-yet-shipped
feature:

| | `RDEP` (`crypto/page_encryption.rs`) | `PageEncryptor` (`storage/encryption/page_encryptor.rs`) |
|---|---|---|
| framing | `b"RDEP"` + ver `0x01` + nonce(12) + ct+tag | nonce(12) + ct + tag(16), no magic/version |
| overhead | 33 | **28** |
| AAD | `page_id` as `u64` LE | `page_id` as **`u32` LE** |
| callers | none (only a re-export; `key_from_env` used by `/admin/status`) | dormant pager wiring + page-0 `key_check` blob |

**The leaner magic-less frame (`PageEncryptor`'s) wins**, with overhead 28 and
AAD = `page_id` as `u32` LE. Reasons:

1. **The page-0 header is already the self-describing authority.** `reddb_file`
   owns `PAGED_ENCRYPTION_MARKER` (`b"RDBE"`) + `PagedEncryptionHeader`, which
   record *that* a database is encrypted plus its salt and key-check. A database
   is encrypted under one scheme for life, so a per-page magic+version (RDEP's
   selling point) duplicates authority the page-0 header already holds — exactly
   the redundancy ADR 0046 forbids.
2. **Hard layout constraint.** `reddb_file::PAGED_ENCRYPTION_KEY_CHECK_BLOB_SIZE`
   is a fixed `60` (= 32-byte plaintext + 28 overhead): the page-0 `key_check`
   slot already embeds the leaner frame. RDEP's 33-byte frame would need 65 and
   would break that out-of-scope constant.
3. **Native width.** The pager addresses pages with `u32`; the key-check uses the
   sentinel `u32::MAX`. Binding AAD to the real `u32` identifier is honest;
   RDEP's `u64` was speculatively wide.
4. **Already wired** into the dormant pager and minimises usable-byte loss per
   page.

**RDEP is retired, not blind-deleted.** Its genuinely-better pieces are carried
forward into `reddb-io-crypto`: the typed error enum (`PageEnvelopeError` vs
`String`), the OS-CSPRNG nonce source (vs truncating a UUIDv4), and the
hex/base64 key parser.

## Consequences

- Nothing is byte-persisted yet (the live path hardcodes `encryption: None`), so
  this is a clean design call, not a migration. The chosen frame is
  byte-identical to the prior `PageEncryptor` output and to the page-0
  `key_check`, so the dormant wiring is unchanged.
- Future envelope changes (new AEAD, nonce scheme) version through the page-0
  header in `reddb_file`, never via a silent edit to `params`.
- The crate has no dependency on `reddb-server` (only `aes-gcm`), keeping the
  crate graph acyclic and the server shrinking toward glue.

## Related

- ADR 0046 — Wire and file crate authority boundary
- ADR 0052 — `reddb-io-types` neutral keystone crate (names this crate as planned)
- ADR 0053 — `reddb-io-rql` boundary
- Issue #1053; PRD #1050
