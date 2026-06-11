//! # reddb-io-crypto
//!
//! RedDB's cryptographic authority crate. It owns the **canonical
//! per-page encryption-at-rest envelope** (AES-256-GCM), the
//! **mandatory encrypt parameters**, and **key parsing** — paralleling
//! the `reddb-io-file` (on-disk artifacts) and `reddb-io-wire`
//! (protocol contracts) authority crates under ADR 0046 / 0054.
//!
//! ## Scope and boundary
//!
//! - **This crate owns** the per-page envelope byte-format
//!   ([`encrypt_page`] / [`decrypt_page`]), the fixed crypto
//!   parameters ([`params`]), and key parsing ([`key::parse_key`]).
//! - **`reddb-io-file` owns** the page-0 paged-encryption header
//!   (`PAGED_ENCRYPTION_MARKER` = `b"RDBE"` / `PagedEncryptionHeader`):
//!   the file-level marker, salt, and key-check slot. That is the
//!   self-describing "is this database encrypted, under what salt"
//!   authority and is intentionally out of this crate's scope.
//! - **`reddb-server` orchestrates**: it binds a key, decides policy
//!   (`RED_ENCRYPTION_KEY[_FILE]`), and routes pager reads/writes
//!   through this envelope. It introduces no second envelope format.
//!
//! ## History (#1053)
//!
//! Two dormant, byte-incompatible envelopes existed for the same
//! not-yet-shipped feature. This crate consolidates them: the leaner
//! magic-less frame survives as canonical (it was already embedded in
//! the page-0 `key_check` and wired into the dormant pager); the
//! self-describing `RDEP` frame is retired, with its typed errors,
//! OS-CSPRNG nonce source, and key parser carried forward here. See
//! ADR 0054 for the full rationale.

pub mod aes_gcm;
pub mod key;
pub mod os_random;
pub mod page_envelope;

/// Mandatory encrypt parameters for the canonical page envelope.
///
/// These are the fixed knobs every encrypted page agrees on. They are
/// constants, not configuration: changing any of them is an on-disk
/// format change that must go through a format-version bump in the
/// page-0 header (`reddb_file`), never a silent edit here.
pub mod params {
    /// AES-256 key length in bytes.
    pub const KEY_SIZE: usize = 32;
    /// AES-GCM nonce (IV) length in bytes.
    pub const NONCE_SIZE: usize = 12;
    /// AES-GCM authentication tag length in bytes.
    pub const TAG_SIZE: usize = 16;
    /// Fixed envelope overhead per page: nonce (12) + tag (16) = 28.
    /// Plaintext expands by exactly this many bytes. This is the value
    /// the page-0 `key_check` slot (`reddb_file`) is sized against:
    /// `PAGED_ENCRYPTION_KEY_CHECK_BLOB_SIZE` = 32 (plaintext) + 28.
    pub const PAGE_ENVELOPE_OVERHEAD: usize = NONCE_SIZE + TAG_SIZE;
    /// AEAD algorithm name, for diagnostics/logging.
    pub const AEAD_ALGORITHM: &str = "AES-256-GCM";
}

pub use key::parse_key;
pub use page_envelope::{decrypt_page, encrypt_page, PageEnvelopeError};
pub use params::{AEAD_ALGORITHM, KEY_SIZE, NONCE_SIZE, PAGE_ENVELOPE_OVERHEAD, TAG_SIZE};
