//! Per-page Encryption using AES-256-GCM
//!
//! Handles encryption and decryption of individual pages.
//! Uses a unique nonce per page (stored with the page) and authenticates
//! the Page ID to prevent page swapping attacks.

use super::key::SecureKey;
use crate::crypto::aes_gcm::{aes256_gcm_decrypt, aes256_gcm_encrypt};
use crate::crypto::uuid::Uuid;

/// Size of the nonce (IV) in bytes
pub const NONCE_SIZE: usize = 12;
/// Size of the authentication tag in bytes
pub const TAG_SIZE: usize = 16;
/// Total encryption overhead per page (Nonce + Tag)
pub const OVERHEAD: usize = NONCE_SIZE + TAG_SIZE;

/// Handles page encryption/decryption
pub struct PageEncryptor {
    key: SecureKey,
}

impl PageEncryptor {
    /// Create a new page encryptor
    pub fn new(key: SecureKey) -> Self {
        Self { key }
    }

    /// Encrypt a page
    ///
    /// Layout:
    /// `[Nonce (12 bytes)] [Ciphertext (N bytes)] [Tag (16 bytes)]`
    ///
    /// The `plaintext` size N will result in an output of size N + 28 bytes.
    /// The caller is responsible for ensuring the plaintext fits within the
    /// target page size (e.g., passing 4068 bytes to get a 4096 byte page).
    pub fn encrypt(&self, page_id: u32, plaintext: &[u8]) -> Vec<u8> {
        // Generate random nonce (12 bytes)
        // We use uuid v4 (16 random bytes) and truncate to 12
        let uuid = Uuid::new_v4();
        let mut nonce = [0u8; NONCE_SIZE];
        nonce.copy_from_slice(&uuid.as_bytes()[0..NONCE_SIZE]);

        // AAD is the Page ID (prevents moving pages to different IDs)
        let aad = page_id.to_le_bytes();

        let key: &[u8; 32] = self
            .key
            .as_bytes()
            .try_into()
            .expect("Key must be 32 bytes");

        // Encrypt: returns Ciphertext || Tag
        let ciphertext_with_tag = aes256_gcm_encrypt(key, &nonce, &aad, plaintext);

        // Result: Nonce || Ciphertext || Tag
        let mut result = Vec::with_capacity(NONCE_SIZE + ciphertext_with_tag.len());
        result.extend_from_slice(&nonce);
        result.extend_from_slice(&ciphertext_with_tag);

        result
    }

    /// Decrypt a page
    pub fn decrypt(&self, page_id: u32, encrypted_data: &[u8]) -> Result<Vec<u8>, String> {
        if encrypted_data.len() < OVERHEAD {
            return Err("Encrypted data too short".to_string());
        }

        // Extract parts
        let nonce = &encrypted_data[..NONCE_SIZE];
        let ciphertext_with_tag = &encrypted_data[NONCE_SIZE..];

        let mut nonce_arr = [0u8; NONCE_SIZE];
        nonce_arr.copy_from_slice(nonce);

        // AAD must match
        let aad = page_id.to_le_bytes();

        let key: &[u8; 32] = self
            .key
            .as_bytes()
            .try_into()
            .expect("Key must be 32 bytes");

        aes256_gcm_decrypt(key, &nonce_arr, &aad, ciphertext_with_tag)
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
