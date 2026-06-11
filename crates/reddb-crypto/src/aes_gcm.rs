//! AES-256-GCM wrapper used by the page-encryption envelope.
//!
//! Thin, allocation-returning shims over the `aes-gcm` crate. The
//! envelope module ([`crate::page_envelope`]) is the only intended
//! caller; these are kept separate so the AEAD primitive can be
//! swapped (or hardware-accelerated) without touching framing logic.

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Key, Nonce,
};

/// Encrypt `plaintext` with AES-256-GCM. Returns `ciphertext ‖ tag`.
pub fn aes256_gcm_encrypt(key: &[u8; 32], iv: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(iv);
    cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("AES-256-GCM encryption failed")
}

/// Decrypt data produced by [`aes256_gcm_encrypt`] (`ciphertext ‖ tag`).
pub fn aes256_gcm_decrypt(
    key: &[u8; 32],
    iv: &[u8; 12],
    aad: &[u8],
    ciphertext_with_tag: &[u8],
) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(iv);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext_with_tag,
                aad,
            },
        )
        .map_err(|e| format!("AES-256-GCM decryption failed: {e}"))
}
