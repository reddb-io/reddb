//! SCRAM-SHA-256 client primitives. Mirrors `src/auth/scram.rs`
//! in the engine — same byte layout, no engine dep so the driver
//! stays standalone.
//!
//! Server-side AuthStore migration is tracked in ADR 0002 Phase
//! 3b; until that lands, the engine returns AuthFail for SCRAM
//! attempts. This module is here so client code is ready to talk
//! once the server flips on.

use sha2::{Digest, Sha256};

const HMAC_SHA256_BLOCK: usize = 64;

/// HMAC-SHA256 implemented inline so the driver doesn't pull a
/// new crate just for this. Bit-for-bit identical to the engine
/// helper.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut padded = [0u8; HMAC_SHA256_BLOCK];
    if key.len() > HMAC_SHA256_BLOCK {
        let h = sha256(key);
        padded[..32].copy_from_slice(&h);
    } else {
        padded[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0u8; HMAC_SHA256_BLOCK];
    let mut opad = [0u8; HMAC_SHA256_BLOCK];
    for i in 0..HMAC_SHA256_BLOCK {
        ipad[i] = padded[i] ^ 0x36;
        opad[i] = padded[i] ^ 0x5c;
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer.finalize());
    out
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&h.finalize());
    out
}

/// PBKDF2-HMAC-SHA256 with a fixed 32-byte derived length.
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], iter: u32) -> [u8; 32] {
    let mut block = [0u8; 32];
    let mut salted = Vec::with_capacity(salt.len() + 4);
    salted.extend_from_slice(salt);
    salted.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha256(password, &salted);
    block.copy_from_slice(&u);
    for _ in 1..iter {
        u = hmac_sha256(password, &u);
        for i in 0..32 {
            block[i] ^= u[i];
        }
    }
    block
}

pub fn xor(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().zip(b.iter()).map(|(x, y)| x ^ y).collect()
}

/// Compute the client proof for a SCRAM exchange.
pub fn client_proof(password: &[u8], salt: &[u8], iter: u32, auth_message: &[u8]) -> Vec<u8> {
    let salted = pbkdf2_sha256(password, salt, iter);
    let client_key = hmac_sha256(&salted, b"Client Key");
    let stored_key = sha256(&client_key);
    let signature = hmac_sha256(&stored_key, auth_message);
    xor(&client_key, &signature)
}

/// Verify the server's signature on the way in (proves the server
/// also knew the verifier, prevents impersonation).
pub fn verify_server_signature(
    password: &[u8],
    salt: &[u8],
    iter: u32,
    auth_message: &[u8],
    presented_signature: &[u8],
) -> bool {
    if presented_signature.len() != 32 {
        return false;
    }
    let salted = pbkdf2_sha256(password, salt, iter);
    let server_key = hmac_sha256(&salted, b"Server Key");
    let expected = hmac_sha256(&server_key, auth_message);
    constant_time_eq(&expected, presented_signature)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PBKDF2 RFC 6070 vector (single iteration) — sanity that
    /// our hand-rolled HMAC matches the standard.
    #[test]
    fn hmac_sha256_known_vector() {
        // RFC 4231 test case 1
        let key = [0x0bu8; 20];
        let data = b"Hi There";
        let mac = hmac_sha256(&key, data);
        let expected = [
            0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53, 0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b,
            0xf1, 0x2b, 0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7, 0x26, 0xe9, 0x37, 0x6c,
            0x2e, 0x32, 0xcf, 0xf7,
        ];
        assert_eq!(mac, expected);
    }

    #[test]
    fn pbkdf2_smoke() {
        // Trivial roundtrip — same inputs produce same output.
        let a = pbkdf2_sha256(b"password", b"salt", 1024);
        let b = pbkdf2_sha256(b"password", b"salt", 1024);
        assert_eq!(a, b);
        let c = pbkdf2_sha256(b"different", b"salt", 1024);
        assert_ne!(a, c);
    }

    #[test]
    fn proof_round_trip_via_client_function() {
        let salt = b"reddb-test";
        let iter = 4096;
        let password = b"hunter2";
        let auth_message = b"client-first-bare,server-first,client-final-no-proof";

        let proof_a = client_proof(password, salt, iter, auth_message);
        let proof_b = client_proof(password, salt, iter, auth_message);
        assert_eq!(proof_a, proof_b);

        let proof_wrong = client_proof(b"wrong", salt, iter, auth_message);
        assert_ne!(proof_a, proof_wrong);
    }
}
