//! Portable Vault passphrase KDF. These parameters are the native v3 contract.

use zeroize::Zeroizing;

/// Derive the native Vault opening key (RFC 9106 Argon2id v1.3).
/// The caller owns the zeroizing key; passwords must never enter durable storage.
pub fn passphrase_key(passphrase: &str, salt: &[u8; 16]) -> Result<Zeroizing<[u8; 32]>, String> {
    if passphrase.is_empty() || passphrase.len() > 4096 {
        return Err("passphrase must contain 1 to 4096 bytes".into());
    }
    let parameters = argon2::Params::new(19 * 1024, 2, 1, Some(32))
        .expect("invariant: native Vault Argon2id parameters are valid");
    let hasher = argon2::Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        parameters,
    );
    let mut key = Zeroizing::new([0u8; 32]);
    hasher
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|_| "vault key derivation failed".to_string())?;
    Ok(key)
}
