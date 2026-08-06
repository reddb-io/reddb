//! Compatibility re-exports for the SCRAM authority in `reddb-wire`.

pub use reddb_wire::redwire::scram::{
    auth_message, client_proof, hmac_sha256, salted_password, server_signature, sha256,
    verify_client_proof, xor, ScramVerifier, DEFAULT_ITER, MIN_ITER,
};
