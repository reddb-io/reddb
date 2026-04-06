//! AES-256-GCM wrapper used by storage.

use aes_gcm::{Aes256Gcm, Nonce, Key, aead::{Aead, KeyInit, Payload}};

/// Encrypt `plaintext` with AES-256-GCM.
pub fn aes256_gcm_encrypt(key: &[u8; 32], iv: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(iv);
    cipher
        .encrypt(nonce, Payload { msg: plaintext, aad })
        .expect("AES-256-GCM encryption failed")
}

/// Decrypt data encrypted by [`aes256_gcm_encrypt`].
pub fn aes256_gcm_decrypt(
    key: &[u8; 32],
    iv: &[u8; 12],
    aad: &[u8],
    ciphertext_with_tag: &[u8],
) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(iv);
    cipher
        .decrypt(nonce, Payload { msg: ciphertext_with_tag, aad })
        .map_err(|e| format!("AES-256-GCM decryption failed: {e}"))
}

pub fn aes256_encrypt_block(_plaintext: &[u8;16], _key:&[u8;32]) -> [u8;16] {
    [0u8;16]
}

pub fn aes256_decrypt_block(_ciphertext: &[u8;16], _key:&[u8;32]) -> [u8;16] {
    [0u8;16]
}
