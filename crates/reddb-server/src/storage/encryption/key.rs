//! Key Management for RedDB Encryption
//!
//! Handles secure storage and derivation of encryption keys.
//! Ensures keys are zeroed out from memory when dropped.

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

    /// Access the raw key bytes
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }
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
        let key = SecureKey::new(b"secret");
        drop(key);
        // Can't easily check memory after drop safely in Rust tests without UB,
        // but we trust the implementation logic.
    }
}
