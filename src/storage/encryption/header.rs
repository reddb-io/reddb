//! Encrypted Database Header
//!
//! Stores encryption parameters and key verification data.
//! This header is stored in the clear (in Page 0, potentially) or a separate header file?
//! The `Pager` uses Page 0 for `DatabaseHeader`.
//! Usually, encryption parameters are part of the `DatabaseHeader` or stored alongside it.
//!
//! Since `DatabaseHeader` in `pager.rs` is fixed structure (u32 fields), we might need to extend it
//! or use a separate page (e.g. Page 1?) or just reserved bytes in Page 0?
//! `HEADER_SIZE` in `page.rs` is 32 bytes.
//! A standard 4KB page has plenty of room.
//!
//! We will implement serialization for this header so it can be embedded in Page 0 after the main header.

use super::key::SecureKey;
use super::page_encryptor::{PageEncryptor, NONCE_SIZE, TAG_SIZE};
use crate::crypto::uuid::Uuid;

pub const SALT_SIZE: usize = 32;
pub const KEY_CHECK_LEN: usize = 32; // Length of known value to encrypt

/// Header containing encryption parameters
#[derive(Debug, Clone)]
pub struct EncryptionHeader {
    /// Salt used for Key Derivation (32 bytes)
    pub salt: [u8; SALT_SIZE],

    /// Key verification data
    /// Layout: [Nonce (12)] [Ciphertext (32)] [Tag (16)]
    /// Total: 12 + 32 + 16 = 60 bytes
    pub key_check: Vec<u8>,
}

impl EncryptionHeader {
    /// Create a new encryption header
    pub fn new(key: &SecureKey) -> Self {
        // Generate random salt
        let uuid = Uuid::new_v4();
        let mut salt = [0u8; SALT_SIZE];
        // Fill salt with random data (using uuid chunks for now as we don't have rand)
        let b = uuid.as_bytes();
        salt[0..16].copy_from_slice(b);
        let uuid2 = Uuid::new_v4();
        salt[16..32].copy_from_slice(uuid2.as_bytes());

        // Create key check
        // Encrypt a known value (e.g., 32 bytes of 0xAA)
        let known_value = [0xAAu8; KEY_CHECK_LEN];
        let encryptor = PageEncryptor::new(key.clone());

        // Use a dummy page ID for key check (e.g., u32::MAX)
        let check_blob = encryptor.encrypt(u32::MAX, &known_value);

        Self {
            salt,
            key_check: check_blob,
        }
    }

    /// Validate the key against this header
    pub fn validate(&self, key: &SecureKey) -> bool {
        let encryptor = PageEncryptor::new(key.clone());

        match encryptor.decrypt(u32::MAX, &self.key_check) {
            Ok(plaintext) => {
                let expected = [0xAAu8; KEY_CHECK_LEN];
                plaintext == expected
            }
            Err(_) => false,
        }
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.salt);
        // Length of key check is dynamic or fixed?
        // NONCE(12) + KEY_CHECK_LEN(32) + TAG(16) = 60 bytes.
        // It's fixed.
        buf.extend_from_slice(&self.key_check);
        buf
    }

    /// Deserialize from bytes
    pub fn from_bytes(data: &[u8]) -> Result<Self, String> {
        let check_size = NONCE_SIZE + KEY_CHECK_LEN + TAG_SIZE;
        let expected_len = SALT_SIZE + check_size;

        if data.len() < expected_len {
            return Err("Data too short for EncryptionHeader".to_string());
        }

        let mut salt = [0u8; SALT_SIZE];
        salt.copy_from_slice(&data[0..SALT_SIZE]);

        let key_check = data[SALT_SIZE..SALT_SIZE + check_size].to_vec();

        Ok(Self { salt, key_check })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_validation() {
        let key = SecureKey::new(&[0x11u8; 32]);
        let header = EncryptionHeader::new(&key);

        assert!(header.validate(&key));

        let wrong_key = SecureKey::new(&[0x22u8; 32]);
        assert!(!header.validate(&wrong_key));
    }

    #[test]
    fn test_header_serialization() {
        let key = SecureKey::new(&[0x33u8; 32]);
        let header = EncryptionHeader::new(&key);

        let bytes = header.to_bytes();
        let loaded = EncryptionHeader::from_bytes(&bytes).unwrap();

        assert_eq!(header.salt, loaded.salt);
        assert_eq!(header.key_check, loaded.key_check);
        assert!(loaded.validate(&key));
    }
}
