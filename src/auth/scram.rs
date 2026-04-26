//! SCRAM-SHA-256 (RFC 5802 + RFC 7677) primitives.
//!
//! Pure functions — no I/O, no state. Both server and client use
//! the same key-derivation routines; the state machine lives in
//! `wire::redwire::auth` for the server and the driver crates
//! for clients. Layout choices match what PostgreSQL ≥10 ships
//! so RedDB peers with libpq-style tooling for free.
//!
//! Verifier storage:
//!   { salt, iter, stored_key, server_key }
//!
//! `salted_password = PBKDF2-HMAC-SHA256(password, salt, iter, 32)`
//! `client_key       = HMAC-SHA256(salted_password, "Client Key")`
//! `stored_key       = SHA256(client_key)`
//! `server_key       = HMAC-SHA256(salted_password, "Server Key")`
//!
//! On login, the server never sees plaintext — only proof / signature.

use sha2::{Digest, Sha256};

use crate::storage::encryption::pbkdf2::pbkdf2_sha256;
use crate::storage::encryption::pbkdf2::Pbkdf2Params;

/// Default iteration count. RFC 7677 recommends 4096; we go higher
/// because RedDB targets 2025+ hardware. Operators can override
/// per-user when migrating from a different database.
pub const DEFAULT_ITER: u32 = 16_384;

/// Minimum iteration count we'll accept on a stored verifier.
/// Below this we treat the verifier as unsafe and force a rotation.
pub const MIN_ITER: u32 = 4096;

/// Stored verifier — what the server keeps in `AuthStore` per
/// SCRAM-enabled user. Never contains plaintext or
/// `salted_password`.
#[derive(Debug, Clone)]
pub struct ScramVerifier {
    pub salt: Vec<u8>,
    pub iter: u32,
    pub stored_key: [u8; 32],
    pub server_key: [u8; 32],
}

impl ScramVerifier {
    /// Derive a verifier from a plaintext password. Used once at
    /// account creation / password rotation.
    pub fn from_password(password: &str, salt: Vec<u8>, iter: u32) -> Self {
        let salted = salted_password(password.as_bytes(), &salt, iter);
        let client_key = hmac_sha256(&salted, b"Client Key");
        let stored_key: [u8; 32] = sha256(&client_key);
        let server_key = hmac_sha256(&salted, b"Server Key");
        Self {
            salt,
            iter,
            stored_key,
            server_key,
        }
    }
}

/// Compute `SaltedPassword`. RFC 5802 § 3 — PBKDF2 with HMAC-SHA256.
pub fn salted_password(password: &[u8], salt: &[u8], iter: u32) -> [u8; 32] {
    let params = Pbkdf2Params {
        iterations: iter,
        // pbkdf2_sha256 uses a fixed 32-byte derived length.
        ..Pbkdf2Params::default()
    };
    let v = pbkdf2_sha256(password, salt, &params);
    let mut out = [0u8; 32];
    out.copy_from_slice(&v[..32]);
    out
}

/// HMAC-SHA256(key, data) → 32 bytes. Reuses the engine's
/// internal helper at `crate::crypto::hmac_sha256`.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    crate::crypto::hmac_sha256(key, data)
}

/// SHA-256(data) → 32 bytes.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&hasher.finalize());
    out
}

/// XOR two equal-length byte slices into a fresh `Vec<u8>`.
/// Used for `ClientProof = ClientKey XOR ClientSignature`.
pub fn xor(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().zip(b.iter()).map(|(x, y)| x ^ y).collect()
}

/// Build the canonical `AuthMessage` per RFC 5802 § 3:
///     client-first-message-bare + "," + server-first-message + "," + client-final-message-without-proof
pub fn auth_message(
    client_first_bare: &str,
    server_first: &str,
    client_final_no_proof: &str,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        client_first_bare.len() + 1 + server_first.len() + 1 + client_final_no_proof.len(),
    );
    out.extend_from_slice(client_first_bare.as_bytes());
    out.push(b',');
    out.extend_from_slice(server_first.as_bytes());
    out.push(b',');
    out.extend_from_slice(client_final_no_proof.as_bytes());
    out
}

/// Compute the client's proof — what the client sends to prove
/// it knows the password.
pub fn client_proof(stored_key: &[u8], auth_message: &[u8], client_key: &[u8]) -> Vec<u8> {
    let signature = hmac_sha256(stored_key, auth_message);
    xor(client_key, &signature)
}

/// Verify a client proof against a stored verifier. Returns true
/// when the proof matches.
pub fn verify_client_proof(
    verifier: &ScramVerifier,
    auth_message: &[u8],
    presented_proof: &[u8],
) -> bool {
    if presented_proof.len() != 32 {
        return false;
    }
    // ClientKey = ClientProof XOR ClientSignature
    let signature = hmac_sha256(&verifier.stored_key, auth_message);
    let client_key = xor(presented_proof, &signature);
    let derived_stored: [u8; 32] = sha256(&client_key);
    crate::crypto::constant_time_eq(&derived_stored, &verifier.stored_key)
}

/// Server's signature to send back in `AuthOk` — proves to the
/// client that the server also knows the verifier.
pub fn server_signature(server_key: &[u8], auth_message: &[u8]) -> [u8; 32] {
    hmac_sha256(server_key, auth_message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity: deriving the same verifier twice from the same
    /// inputs yields identical keys.
    #[test]
    fn verifier_is_deterministic() {
        let salt = b"reddb-test-salt".to_vec();
        let v1 = ScramVerifier::from_password("hunter2", salt.clone(), 4096);
        let v2 = ScramVerifier::from_password("hunter2", salt, 4096);
        assert_eq!(v1.stored_key, v2.stored_key);
        assert_eq!(v1.server_key, v2.server_key);
    }

    /// End-to-end: derive verifier, simulate client computing the
    /// proof, server verifies. Round-trip succeeds for the right
    /// password and fails for the wrong one.
    #[test]
    fn full_round_trip() {
        let salt = b"reddb-rt-salt".to_vec();
        let iter = 4096;
        let verifier = ScramVerifier::from_password("correct horse", salt.clone(), iter);

        let client_first_bare = "n=alice,r=cnonce";
        let server_first =
            "r=cnonce+snonce,s=cmVkZGItcnQtc2FsdA==,i=4096";
        let client_final_no_proof = "c=biws,r=cnonce+snonce";
        let am = auth_message(client_first_bare, server_first, client_final_no_proof);

        // Client side computes proof from plaintext.
        let salted = salted_password(b"correct horse", &salt, iter);
        let client_key = hmac_sha256(&salted, b"Client Key");
        let proof = client_proof(&verifier.stored_key, &am, &client_key);

        // Server verifies.
        assert!(verify_client_proof(&verifier, &am, &proof));

        // Wrong password → wrong client_key → rejected.
        let salted_bad = salted_password(b"wrong password", &salt, iter);
        let client_key_bad = hmac_sha256(&salted_bad, b"Client Key");
        let proof_bad = client_proof(&verifier.stored_key, &am, &client_key_bad);
        assert!(!verify_client_proof(&verifier, &am, &proof_bad));
    }

    #[test]
    fn server_signature_round_trip() {
        let v = ScramVerifier::from_password("p", b"s".to_vec(), 4096);
        let am = b"some auth message".to_vec();
        let sig = server_signature(&v.server_key, &am);
        // Same inputs → same signature.
        let again = server_signature(&v.server_key, &am);
        assert_eq!(sig, again);
        // Different message → different signature.
        let other = server_signature(&v.server_key, b"different");
        assert_ne!(sig, other);
    }

    #[test]
    fn xor_basic() {
        assert_eq!(xor(&[0xff, 0x00, 0xaa], &[0x0f, 0xff, 0x55]), vec![0xf0, 0xff, 0xff]);
    }
}
