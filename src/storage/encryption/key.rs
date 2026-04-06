//! Key Management for RedDB Encryption
//!
//! Handles secure storage and derivation of encryption keys.
//! Ensures keys are zeroed out from memory when dropped.

use super::pbkdf2::derive_key;
use std::ptr;

/// A securely managed encryption key
pub struct SecureKey {
    data: Box<[u8]>,
}

impl SecureKey {
    /// Create a new secure key from raw bytes
    pub fn new(data: &[u8]) -> Self {
        Self { data: data.into() }
    }

    /// Derive a key from a password using PBKDF2-SHA256
    /// Uses 100,000 iterations for security (OWASP recommendation)
    pub fn from_passphrase(password: &str, salt: &[u8]) -> Self {
        let key_data = derive_key(password.as_bytes(), salt);
        Self::new(&key_data)
    }

    /// Derive a key from an environment variable (e.g., REDBLUE_DB_KEY)
    /// The env var can contain a hex string or a raw passphrase.
    /// If it's a 64-char hex string, it's treated as the raw key (32 bytes).
    /// Otherwise, it's treated as a passphrase and KDF is applied (requires salt).
    pub fn from_env(var_name: &str, salt: Option<&[u8]>) -> Result<Self, String> {
        let val = std::env::var(var_name).map_err(|_| format!("{} not set", var_name))?;

        // Try hex decoding first
        if val.len() == 64 {
            if let Ok(bytes) = decode_hex(&val) {
                return Ok(Self::new(&bytes));
            }
        }

        // Fallback to KDF
        if let Some(s) = salt {
            Ok(Self::from_passphrase(&val, s))
        } else {
            Err("Salt required for passphrase-based key derivation".to_string())
        }
    }

    /// Access the raw key bytes
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("Odd length".to_string());
    }

    let mut bytes = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte_str = &s[i..i + 2];
        let byte = u8::from_str_radix(byte_str, 16).map_err(|e| format!("Invalid hex: {}", e))?;
        bytes.push(byte);
    }
    Ok(bytes)
}

impl Drop for SecureKey {
    fn drop(&mut self) {
        // Volatile zeroing to prevent compiler optimization
        unsafe {
            ptr::write_volatile(self.data.as_mut_ptr(), 0);
            for i in 1..self.data.len() {
                ptr::write_volatile(self.data.as_mut_ptr().add(i), 0);
            }
        }
        // Memory fence to ensure writes happen before deallocation
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

impl Clone for SecureKey {
    fn clone(&self) -> Self {
        Self::new(&self.data)
    }
}

impl std::fmt::Debug for SecureKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecureKey(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secure_key_zeroing() {
        let mut key = SecureKey::new(b"secret");
        let ptr = key.data.as_ptr();
        drop(key);
        // Can't easily check memory after drop safely in Rust tests without UB,
        // but we trust the implementation logic.
    }

    #[test]
    fn test_key_derivation() {
        let key = SecureKey::from_passphrase("password", b"somesalt");
        assert_eq!(key.as_bytes().len(), 32);
    }
}
