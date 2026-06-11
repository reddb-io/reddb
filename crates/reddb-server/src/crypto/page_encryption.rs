//! Encryption-at-rest: server-side facade over the canonical
//! `reddb-io-crypto` envelope (#1053, ADR 0054).
//!
//! The per-page envelope byte-format, the mandatory encrypt
//! parameters, and key parsing now live in `reddb-io-crypto`. This
//! module previously hosted the `RDEP` self-describing frame; that
//! envelope is **retired**. Only the server-specific
//! environment-key resolution stays here, because it layers RedDB's
//! `RED_ENCRYPTION_KEY[_FILE]` file-fallback convention on top of the
//! canonical `reddb_crypto::parse_key`.
//!
//! Per ADR 0046, this is a compatibility facade: it delegates to the
//! canonical crate and carries no second frame or parameter set.

// Re-export the canonical envelope + parser so existing
// `crate::crypto::*` call paths keep resolving.
pub use reddb_crypto::{
    decrypt_page, encrypt_page, parse_key, PageEnvelopeError, PAGE_ENVELOPE_OVERHEAD,
};

/// Read the runtime encryption key from `RED_ENCRYPTION_KEY` /
/// `RED_ENCRYPTION_KEY_FILE`. Returns `None` when the operator hasn't
/// enabled at-rest encryption. Errors are surfaced as `Err` so a
/// misconfigured key (typo, wrong length) fails boot loudly instead of
/// silently leaving plaintext on disk.
pub fn key_from_env() -> Result<Option<[u8; 32]>, String> {
    match crate::utils::env_with_file_fallback("RED_ENCRYPTION_KEY") {
        Some(raw) => parse_key(&raw).map(Some),
        None => Ok(None),
    }
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
    fn facade_round_trips_through_canonical_crate() {
        let plaintext = b"facade still encrypts";
        let frame = encrypt_page(&key(), 3, plaintext).unwrap();
        assert_eq!(frame.len(), PAGE_ENVELOPE_OVERHEAD + plaintext.len());
        let recovered = decrypt_page(&key(), 3, &frame).unwrap();
        assert_eq!(recovered, plaintext);
    }
}
