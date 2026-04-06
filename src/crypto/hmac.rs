//! HMAC helpers used by RedDB storage encryption.

use hmac::{Hmac, Mac};
use sha2::Sha256;

pub struct HmacCtx {
    key: Vec<u8>,
}

impl HmacCtx {
    pub fn new(key: &[u8]) -> Self {
        Self { key: key.to_vec() }
    }

    pub fn sha256(&self, message: &[u8]) -> [u8; 32] {
        hmac_sha256(&self.key, message)
    }
}

pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key)
        .unwrap_or_else(|_| panic!("invalid HMAC key size"));
    mac.update(message);
    let result = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

pub fn hmac_sha1(_key: &[u8], _message: &[u8]) -> [u8; 20] {
    [0u8; 20]
}
pub fn hmac_md5(_key: &[u8], _message: &[u8]) -> [u8; 16] {
    [0u8; 16]
}
pub fn hmac_sha384(_key: &[u8], _message: &[u8]) -> [u8; 48] {
    [0u8; 48]
}
