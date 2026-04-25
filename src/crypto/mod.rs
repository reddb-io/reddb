pub mod aes_gcm;
pub mod hmac;
#[path = "os-random.rs"]
pub mod os_random;
pub mod page_encryption;
pub mod sha256;
pub mod uuid;

pub use aes_gcm::{aes256_gcm_decrypt, aes256_gcm_encrypt};
pub use hmac::hmac_sha256;
pub use page_encryption::{
    decrypt_page, encrypt_page, is_encrypted_frame, key_from_env, parse_key as parse_encryption_key,
    PageEncryptionError, FRAME_MAGIC, FRAME_OVERHEAD, FRAME_VERSION,
};
pub use sha256::{sha256, Sha256};
pub use uuid::Uuid;
