//! AES-256-GCM wrapper used by storage.

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Key, Nonce,
};

/// Encrypt `plaintext` with AES-256-GCM.
pub fn aes256_gcm_encrypt(key: &[u8; 32], iv: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let key = Key::<Aes256Gcm>::try_from(&key[..]).expect("AES-256-GCM key is 32 bytes");
    let nonce = Nonce::try_from(&iv[..]).expect("AES-256-GCM nonce is 12 bytes");
    let cipher = Aes256Gcm::new(&key);
    cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("AES-256-GCM encryption failed")
}

/// Decrypt data encrypted by [`aes256_gcm_encrypt`].
pub fn aes256_gcm_decrypt(
    key: &[u8; 32],
    iv: &[u8; 12],
    aad: &[u8],
    ciphertext_with_tag: &[u8],
) -> Result<Vec<u8>, String> {
    let key = Key::<Aes256Gcm>::try_from(&key[..]).expect("AES-256-GCM key is 32 bytes");
    let nonce = Nonce::try_from(&iv[..]).expect("AES-256-GCM nonce is 12 bytes");
    let cipher = Aes256Gcm::new(&key);
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext_with_tag,
                aad,
            },
        )
        .map_err(|e| format!("AES-256-GCM decryption failed: {e}"))
}
