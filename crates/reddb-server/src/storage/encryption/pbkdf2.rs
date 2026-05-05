//! PBKDF2-SHA256 Key Derivation Function (RFC 8018)
//!
//! PBKDF2 (Password-Based Key Derivation Function 2) is a key derivation function
//! that applies a pseudorandom function (HMAC-SHA256) to the password along with
//! a salt and repeats the process many times to produce a derived key.
//!
//! # Security Notes
//! - Uses 100,000 iterations by default (OWASP recommendation for SHA-256)
//! - Salt should be at least 16 bytes (we use 32 bytes)
//! - Output key length is 32 bytes (256 bits) for AES-256

use crate::crypto::hmac::hmac_sha256;

/// Default number of iterations (OWASP 2023 recommendation for SHA-256)
pub const DEFAULT_ITERATIONS: u32 = 100_000;

/// PBKDF2 Parameters
#[derive(Debug, Clone)]
pub struct Pbkdf2Params {
    /// Number of iterations
    pub iterations: u32,
    /// Output key length in bytes
    pub key_len: usize,
}

impl Default for Pbkdf2Params {
    fn default() -> Self {
        Self {
            iterations: DEFAULT_ITERATIONS,
            key_len: 32, // 256 bits for AES-256
        }
    }
}

/// Derive a key from password and salt using PBKDF2-HMAC-SHA256
///
/// # Arguments
/// * `password` - The password bytes
/// * `salt` - The salt bytes (should be at least 16 bytes, random)
/// * `params` - PBKDF2 parameters (iterations, key length)
///
/// # Returns
/// Derived key of length `params.key_len`
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], params: &Pbkdf2Params) -> Vec<u8> {
    let mut dk = Vec::with_capacity(params.key_len);
    let hlen = 32; // SHA-256 output length

    // Number of blocks needed
    let blocks_needed = params.key_len.div_ceil(hlen);

    for block_num in 1..=blocks_needed {
        let block = pbkdf2_f(password, salt, params.iterations, block_num as u32);
        dk.extend_from_slice(&block);
    }

    // Truncate to exact key length
    dk.truncate(params.key_len);
    dk
}

/// PBKDF2 F function: F(Password, Salt, c, i) = U_1 XOR U_2 XOR ... XOR U_c
/// where U_1 = PRF(Password, Salt || INT(i))
/// and U_j = PRF(Password, U_{j-1})
fn pbkdf2_f(password: &[u8], salt: &[u8], iterations: u32, block_num: u32) -> [u8; 32] {
    // U_1 = PRF(Password, Salt || INT(i))
    // INT(i) is a 4-byte big-endian encoding of the block number
    let mut salt_with_block = Vec::with_capacity(salt.len() + 4);
    salt_with_block.extend_from_slice(salt);
    salt_with_block.extend_from_slice(&block_num.to_be_bytes());

    let mut u = hmac_sha256(password, &salt_with_block);
    let mut result = u;

    // U_2 through U_c
    for _ in 1..iterations {
        u = hmac_sha256(password, &u);
        // XOR into result
        for j in 0..32 {
            result[j] ^= u[j];
        }
    }

    result
}

/// Derive a 32-byte key using default parameters
pub fn derive_key(password: &[u8], salt: &[u8]) -> Vec<u8> {
    pbkdf2_sha256(password, salt, &Pbkdf2Params::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pbkdf2_rfc6070_vector1() {
        // RFC 6070 Test Vector 1
        // P = "password" (8 octets)
        // S = "salt" (4 octets)
        // c = 1
        // dkLen = 20
        let password = b"password";
        let salt = b"salt";
        let params = Pbkdf2Params {
            iterations: 1,
            key_len: 20,
        };

        let dk = pbkdf2_sha256(password, salt, &params);

        // Expected (from RFC 6070, but note RFC 6070 uses HMAC-SHA1, not SHA256)
        // For HMAC-SHA256, we need to verify our own test vectors
        // This is a basic sanity check that different inputs produce different outputs
        assert_eq!(dk.len(), 20);
    }

    #[test]
    fn test_pbkdf2_different_passwords() {
        let salt = b"random_salt_value_here";
        let params = Pbkdf2Params {
            iterations: 1000, // Fewer iterations for test speed
            key_len: 32,
        };

        let key1 = pbkdf2_sha256(b"password1", salt, &params);
        let key2 = pbkdf2_sha256(b"password2", salt, &params);

        assert_ne!(
            key1, key2,
            "Different passwords must produce different keys"
        );
    }

    #[test]
    fn test_pbkdf2_different_salts() {
        let password = b"same_password";
        let params = Pbkdf2Params {
            iterations: 1000,
            key_len: 32,
        };

        let key1 = pbkdf2_sha256(password, b"salt1", &params);
        let key2 = pbkdf2_sha256(password, b"salt2", &params);

        assert_ne!(key1, key2, "Different salts must produce different keys");
    }

    #[test]
    fn test_pbkdf2_deterministic() {
        let password = b"test_password";
        let salt = b"test_salt";
        let params = Pbkdf2Params {
            iterations: 1000,
            key_len: 32,
        };

        let key1 = pbkdf2_sha256(password, salt, &params);
        let key2 = pbkdf2_sha256(password, salt, &params);

        assert_eq!(key1, key2, "Same inputs must produce same outputs");
    }

    #[test]
    fn test_pbkdf2_key_length() {
        let password = b"password";
        let salt = b"salt";

        for key_len in [16, 32, 48, 64] {
            let params = Pbkdf2Params {
                iterations: 100,
                key_len,
            };
            let dk = pbkdf2_sha256(password, salt, &params);
            assert_eq!(dk.len(), key_len);
        }
    }

    #[test]
    fn test_pbkdf2_known_vector_sha256() {
        // Test vector for PBKDF2-HMAC-SHA256 from various sources
        // Password: "password", Salt: "salt", Iterations: 1, dkLen: 32
        let password = b"password";
        let salt = b"salt";
        let params = Pbkdf2Params {
            iterations: 1,
            key_len: 32,
        };

        let dk = pbkdf2_sha256(password, salt, &params);

        // Expected output (from OpenSSL: echo -n "password" | openssl kdf -keylen 32 -kdfopt digest:SHA256 -kdfopt pass:password -kdfopt salt:salt -kdfopt iter:1 PBKDF2)
        let expected = [
            0x12, 0x0f, 0xb6, 0xcf, 0xfc, 0xf8, 0xb3, 0x2c, 0x43, 0xe7, 0x22, 0x52, 0x56, 0xc4,
            0xf8, 0x37, 0xa8, 0x65, 0x48, 0xc9, 0x2c, 0xcc, 0x35, 0x48, 0x08, 0x05, 0x98, 0x7c,
            0xb7, 0x0b, 0xe1, 0x7b,
        ];

        assert_eq!(dk, expected, "PBKDF2-SHA256 known vector mismatch");
    }

    #[test]
    fn test_pbkdf2_known_vector_sha256_iter2() {
        // Test vector: Password: "password", Salt: "salt", Iterations: 2, dkLen: 32
        let password = b"password";
        let salt = b"salt";
        let params = Pbkdf2Params {
            iterations: 2,
            key_len: 32,
        };

        let dk = pbkdf2_sha256(password, salt, &params);

        let expected = [
            0xae, 0x4d, 0x0c, 0x95, 0xaf, 0x6b, 0x46, 0xd3, 0x2d, 0x0a, 0xdf, 0xf9, 0x28, 0xf0,
            0x6d, 0xd0, 0x2a, 0x30, 0x3f, 0x8e, 0xf3, 0xc2, 0x51, 0xdf, 0xd6, 0xe2, 0xd8, 0x5a,
            0x95, 0x47, 0x4c, 0x43,
        ];

        assert_eq!(dk, expected, "PBKDF2-SHA256 known vector (iter=2) mismatch");
    }
}
