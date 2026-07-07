//! Per-page encryption adapter binding a [`SecureKey`] to the
//! canonical `reddb-io-crypto` envelope (#1053, ADR 0054).
//!
//! This type used to carry its own AES-256-GCM framing (a magic-less
//! `nonce ‖ ct ‖ tag` layout with a UUIDv4-truncated nonce). That
//! framing is **retired**: the byte-format and parameters now live in
//! `reddb-io-crypto`, and the rival self-describing `RDEP` envelope is
//! retired too. `PageEncryptor` survives only as a server-local
//! convenience wrapper that holds a key (with secure zeroing on drop)
//! and delegates to the canonical free functions. The on-disk frame is
//! byte-identical to the previous `PageEncryptor` output, so the
//! dormant pager wiring and the page-0 `key_check` blob are unchanged.

use super::key::SecureKey;

/// Size of the nonce (IV) in bytes.
pub const NONCE_SIZE: usize = reddb_crypto::NONCE_SIZE;
/// Size of the authentication tag in bytes.
pub const TAG_SIZE: usize = reddb_crypto::TAG_SIZE;
/// Total encryption overhead per page (nonce + tag = 28).
pub const OVERHEAD: usize = reddb_crypto::PAGE_ENVELOPE_OVERHEAD;

/// Binds a [`SecureKey`] to the canonical per-page envelope.
pub struct PageEncryptor {
    key: SecureKey,
}

impl PageEncryptor {
    /// Create a new page encryptor.
    pub fn new(key: SecureKey) -> Self {
        Self { key }
    }

    /// Encrypt a page through the canonical envelope.
    ///
    /// Layout: `[nonce (12)] [ciphertext (N)] [tag (16)]`; the
    /// `plaintext` of size N yields N + [`OVERHEAD`] bytes. The caller
    /// ensures the plaintext fits the target page size (e.g. 4068
    /// bytes → a 16 KiB page). `page_id` is bound as AAD.
    pub fn encrypt(&self, page_id: u32, plaintext: &[u8]) -> Vec<u8> {
        reddb_crypto::encrypt_page(self.key_bytes(), page_id, plaintext)
            .expect("page envelope encryption failed (CSPRNG)")
    }

    /// Decrypt a page produced by [`Self::encrypt`].
    pub fn decrypt(&self, page_id: u32, encrypted_data: &[u8]) -> Result<Vec<u8>, String> {
        reddb_crypto::decrypt_page(self.key_bytes(), page_id, encrypted_data)
            .map_err(|e| e.to_string())
    }

    fn key_bytes(&self) -> &[u8; 32] {
        self.key
            .as_bytes()
            .try_into()
            .expect("Key must be 32 bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_encryption_roundtrip() {
        let key = SecureKey::new(&[0x42u8; 32]);
        let encryptor = PageEncryptor::new(key);

        let page_id = 123;
        let plaintext = b"This is a secret page content.";

        let encrypted = encryptor.encrypt(page_id, plaintext);

        // Size check
        assert_eq!(encrypted.len(), plaintext.len() + OVERHEAD);

        // Decrypt
        let decrypted = encryptor.decrypt(page_id, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_page_encryption_bad_page_id() {
        let key = SecureKey::new(&[0x42u8; 32]);
        let encryptor = PageEncryptor::new(key);

        let plaintext = b"content";
        let encrypted = encryptor.encrypt(100, plaintext);

        // Try decrypting with wrong page ID
        let result = encryptor.decrypt(101, &encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn test_page_encryption_tampering() {
        let key = SecureKey::new(&[0x42u8; 32]);
        let encryptor = PageEncryptor::new(key);

        let plaintext = b"content";
        let mut encrypted = encryptor.encrypt(100, plaintext);

        // Tamper with the last byte (tag)
        let last = encrypted.len() - 1;
        encrypted[last] ^= 1;

        let result = encryptor.decrypt(100, &encrypted);
        assert!(result.is_err());
    }
}
