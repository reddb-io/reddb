//! Compatibility exports for SCRAM-SHA-256 client primitives.
//!
//! The protocol authority and handshake drivers live in `reddb-wire`.

pub use reddb_wire::redwire::scram::{hmac_sha256, sha256, xor};

/// PBKDF2-HMAC-SHA256 with a fixed 32-byte derived length.
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], iter: u32) -> [u8; 32] {
    reddb_wire::redwire::scram::salted_password(password, salt, iter)
}

/// Compute the client proof for a SCRAM exchange.
pub fn client_proof(password: &[u8], salt: &[u8], iter: u32, auth_message: &[u8]) -> Vec<u8> {
    let salted = pbkdf2_sha256(password, salt, iter);
    let client_key = hmac_sha256(&salted, b"Client Key");
    let stored_key = sha256(&client_key);
    reddb_wire::redwire::scram::client_proof(&stored_key, auth_message, &client_key)
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
    reddb_wire::redwire::scram::verify_server_signature(
        &server_key,
        auth_message,
        presented_signature,
    )
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
