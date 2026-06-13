//! Canonical per-page encryption-at-rest envelope (#1053, ADR 0054).
//!
//! This is the single byte-format for an encrypted RedDB page. It
//! consolidates two dormant, byte-incompatible predecessors:
//!
//! - **RDEP** (`reddb-server/crypto/page_encryption.rs`) — a
//!   self-describing frame carrying a `b"RDEP"` magic + version byte.
//!   *Retired.* Its genuinely-better pieces are carried forward here:
//!   the typed error enum, the OS-CSPRNG nonce source, and the
//!   hex/base64 key parser ([`crate::key`]).
//! - **PageEncryptor** (`reddb-server/storage/encryption/page_encryptor.rs`)
//!   — the leaner magic-less frame, already wired into the dormant
//!   pager and already embedded as the page-0 header's `key_check`
//!   blob. *Its frame survives as the canonical layout below.*
//!
//! ## On-disk frame
//!
//! ```text
//! [0..12]   nonce (12 bytes, random per page, OS CSPRNG)
//! [12..]    ciphertext ‖ 16-byte AES-256-GCM tag
//! ```
//!
//! Overhead is exactly [`PAGE_ENVELOPE_OVERHEAD`] = 28 bytes
//! (nonce 12 + tag 16). Plaintext expands by precisely this much, so
//! a fixed-size page slot stays fixed.
//!
//! ## Why no per-page magic/version
//!
//! Self-description for encryption-at-rest lives one level up, in the
//! page-0 paged-encryption header (`reddb_file::PAGED_ENCRYPTION_MARKER`
//! = `b"RDBE"` + `PagedEncryptionHeader`). That header is the
//! file-level authority: it records *that* the database is encrypted,
//! the salt, and a key-check blob. A database is encrypted under one
//! scheme for its whole life, so a per-page magic+version would
//! duplicate authority the page-0 header already holds — and the
//! page-0 `key_check` slot is a fixed 60 bytes (= 32-byte plaintext +
//! this 28-byte overhead), which a 33-byte RDEP frame would overflow.
//! Keeping the per-page frame lean is therefore both an authority
//! decision (ADR 0046 / 0054) and a hard layout constraint.
//!
//! ## Properties
//!
//! - **Random nonce per page** via the OS CSPRNG; collisions across
//!   `2^96` pages are astronomically unlikely. The API is stateless.
//! - **AAD = `page_id` as `u32` LE** — binds the ciphertext to its
//!   page slot, so a peer-page swap fails the GCM tag check on
//!   decrypt. `u32` matches the engine's native page-id width (the
//!   pager addresses pages with `u32`; the page-0 key-check uses the
//!   sentinel `u32::MAX`). The retired RDEP envelope used `u64`,
//!   which was speculatively wide; binding to the real identifier
//!   width is the honest choice.

use crate::aes_gcm::{aes256_gcm_decrypt, aes256_gcm_encrypt};
use crate::os_random;
use crate::params::{NONCE_SIZE, PAGE_ENVELOPE_OVERHEAD};

/// Errors returned by the page-envelope surface. The caller (the
/// pager) maps these to its own typed error.
#[derive(Debug)]
pub enum PageEnvelopeError {
    /// Frame is shorter than [`PAGE_ENVELOPE_OVERHEAD`] — cannot even
    /// contain a nonce + tag.
    Truncated,
    /// GCM tag check failed: wrong key, wrong `page_id` (AAD), or
    /// tampering. These are functionally indistinguishable and all
    /// fail closed.
    KeyMismatch(String),
    /// OS CSPRNG failed while drawing the nonce.
    RandomFailure(String),
}

impl std::fmt::Display for PageEnvelopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => f.write_str("encrypted page: truncated frame"),
            Self::KeyMismatch(detail) => {
                write!(f, "encrypted page: key mismatch or tampering ({detail})")
            }
            Self::RandomFailure(detail) => {
                write!(f, "encrypted page: nonce generation failed ({detail})")
            }
        }
    }
}

impl std::error::Error for PageEnvelopeError {}

/// Encrypt `plaintext` for storage as page `page_id`. `page_id` is
/// bound as AAD (`u32` LE), so swapping two pages on disk fails the
/// tag check on decrypt.
///
/// Output layout: `nonce(12) ‖ ciphertext ‖ tag(16)`; length is
/// `plaintext.len() + PAGE_ENVELOPE_OVERHEAD`.
pub fn encrypt_page(
    key: &[u8; 32],
    page_id: u32,
    plaintext: &[u8],
) -> Result<Vec<u8>, PageEnvelopeError> {
    let mut nonce = [0u8; NONCE_SIZE];
    os_random::fill_bytes(&mut nonce).map_err(PageEnvelopeError::RandomFailure)?;
    let aad = page_id.to_le_bytes();
    let ciphertext = aes256_gcm_encrypt(key, &nonce, &aad, plaintext);

    let mut out = Vec::with_capacity(PAGE_ENVELOPE_OVERHEAD + plaintext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt an envelope produced by [`encrypt_page`]. `page_id` MUST
/// match the value passed at encrypt time — a mismatch surfaces as
/// [`PageEnvelopeError::KeyMismatch`] (the GCM tag check failing),
/// which is the correct signal: an attacker swapping pages is
/// functionally indistinguishable from a wrong key.
pub fn decrypt_page(
    key: &[u8; 32],
    page_id: u32,
    frame: &[u8],
) -> Result<Vec<u8>, PageEnvelopeError> {
    if frame.len() < PAGE_ENVELOPE_OVERHEAD {
        return Err(PageEnvelopeError::Truncated);
    }
    let mut nonce = [0u8; NONCE_SIZE];
    nonce.copy_from_slice(&frame[..NONCE_SIZE]);
    let aad = page_id.to_le_bytes();
    aes256_gcm_decrypt(key, &nonce, &aad, &frame[NONCE_SIZE..])
        .map_err(PageEnvelopeError::KeyMismatch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8;
        }
        k
    }

    #[test]
    fn round_trips_plaintext() {
        let plaintext = b"page bytes that will be encrypted";
        let frame = encrypt_page(&key(), 7, plaintext).unwrap();
        assert_eq!(frame.len(), PAGE_ENVELOPE_OVERHEAD + plaintext.len());
        let recovered = decrypt_page(&key(), 7, &frame).unwrap();
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn nonce_is_random_per_call() {
        let plaintext = b"same payload, different nonce";
        let f1 = encrypt_page(&key(), 1, plaintext).unwrap();
        let f2 = encrypt_page(&key(), 1, plaintext).unwrap();
        assert_ne!(f1, f2);
    }

    #[test]
    fn page_id_binding_catches_swapped_pages() {
        let plaintext = b"page 1 contents";
        let frame = encrypt_page(&key(), 1, plaintext).unwrap();
        let err = decrypt_page(&key(), 2, &frame).unwrap_err();
        assert!(
            matches!(err, PageEnvelopeError::KeyMismatch(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn wrong_key_fails_closed() {
        let plaintext = b"sensitive";
        let frame = encrypt_page(&key(), 5, plaintext).unwrap();
        let mut wrong = key();
        wrong[0] ^= 0xff;
        let err = decrypt_page(&wrong, 5, &frame).unwrap_err();
        assert!(matches!(err, PageEnvelopeError::KeyMismatch(_)));
    }

    #[test]
    fn truncated_frame_is_typed() {
        let frame = vec![0u8; PAGE_ENVELOPE_OVERHEAD - 1];
        let err = decrypt_page(&key(), 0, &frame).unwrap_err();
        assert!(matches!(err, PageEnvelopeError::Truncated));
    }

    #[test]
    fn tampered_tag_fails() {
        let frame = encrypt_page(&key(), 9, b"abc").unwrap();
        let mut bad = frame.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(decrypt_page(&key(), 9, &bad).is_err());
    }

    #[test]
    fn error_display_is_specific_to_failure_class() {
        assert_eq!(
            PageEnvelopeError::Truncated.to_string(),
            "encrypted page: truncated frame"
        );
        assert_eq!(
            PageEnvelopeError::KeyMismatch("bad tag".to_string()).to_string(),
            "encrypted page: key mismatch or tampering (bad tag)"
        );
        assert_eq!(
            PageEnvelopeError::RandomFailure("no entropy".to_string()).to_string(),
            "encrypted page: nonce generation failed (no entropy)"
        );
    }
}
