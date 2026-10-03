//! DML crypto / secret-payload helpers extracted from `impl_dml`.
//!
//! Behaviour-preserving move (issue #1634). Names and behaviour are unchanged
//! from `impl_dml`; `resolve_crypto_sentinel` stays `pub(crate)` (called from
//! `impl_dml` and `impl_dml_support`) and `decrypt_secret_payload` stays
//! `pub(crate)` and importable at a stable path — its single external import
//! in `impl_core` is updated to `super::impl_dml_crypto::decrypt_secret_payload`.

use super::*;

/// Sentinel prefix produced by the parser for `PASSWORD('...')` and
/// `SECRET('...')` literals. The runtime strips this marker and
/// applies the actual crypto transform during INSERT execution.
pub(crate) const PLAINTEXT_SENTINEL: &str = "@@plain@@";

impl RedDBRuntime {
    pub(crate) fn resolve_secret_column_value(
        &self,
        value: Value,
        is_secret_column: bool,
    ) -> RedDBResult<Value> {
        let value = if is_secret_column {
            match value {
                Value::Text(text) => {
                    let key = self.secret_aes_key().ok_or_else(|| {
                        RedDBError::Query("SECRET column requires an available vault key".into())
                    })?;
                    Value::Secret(encrypt_secret_payload(&key, text.as_bytes()))
                }
                Value::Null | Value::Secret(_) => value,
                _ => {
                    return Err(RedDBError::Query(
                        "SECRET columns accept text or encrypted secrets".into(),
                    ))
                }
            }
        } else {
            value
        };
        self.resolve_crypto_sentinel(value)
    }

    pub(crate) fn secret_query_input(&self, value: Value) -> Option<Value> {
        let Value::Secret(payload) = value else {
            return Some(value);
        };
        let key = self.secret_aes_key()?;
        if let Some(plain) = decrypt_secret_payload(&key, &payload) {
            return String::from_utf8(plain).ok().map(Value::text);
        }
        if payload.len() < 12 {
            return None;
        }
        let nonce: &[u8; 12] = payload[..12].try_into().ok()?;
        let plain = crate::crypto::aes_gcm::aes256_gcm_decrypt(
            &key,
            nonce,
            b"reddb.secret.value",
            &payload[12..],
        )
        .ok()?;
        let (value, consumed) = Value::from_bytes(&plain).ok()?;
        (consumed == plain.len()).then_some(value)
    }

    /// Intermediate expressions are encrypted as typed values. They can pass
    /// through CTEs and subqueries without losing their sensitivity or type.
    pub(crate) fn secret_query_output(&self, value: Value) -> RedDBResult<Value> {
        let key = self.secret_aes_key().ok_or_else(|| {
            RedDBError::Query("secret expression requires an available vault key".into())
        })?;
        let nonce_bytes = crate::auth::store::random_bytes(12);
        let nonce: &[u8; 12] = nonce_bytes
            .as_slice()
            .try_into()
            .expect("invariant: generated nonce has twelve bytes");
        let ciphertext = crate::crypto::aes_gcm::aes256_gcm_encrypt(
            &key,
            nonce,
            b"reddb.secret.value",
            &value.to_bytes(),
        );
        let mut payload = nonce_bytes;
        payload.extend_from_slice(&ciphertext);
        Ok(Value::Secret(payload))
    }
    /// Strip the plaintext sentinel from a `Value::Password` or
    /// `Value::Secret` produced by the parser and apply the real
    /// crypto transform. `Password` is always hashed with argon2id.
    /// `Secret` is encrypted with AES-256-GCM keyed by the vault
    /// when `red.config.secret.auto_encrypt = true` (default).
    pub(crate) fn resolve_crypto_sentinel(&self, value: Value) -> RedDBResult<Value> {
        match value {
            Value::Password(marked) => {
                if let Some(plain) = marked.strip_prefix(PLAINTEXT_SENTINEL) {
                    Ok(Value::Password(crate::auth::store::hash_password(plain)))
                } else {
                    Ok(Value::Password(marked))
                }
            }
            Value::Secret(bytes) => {
                if bytes.starts_with(PLAINTEXT_SENTINEL.as_bytes()) {
                    if !self.secret_auto_encrypt() {
                        return Err(RedDBError::Query(
                            "SECRET() literal rejected: red.config.secret.auto_encrypt \
                             is false. Insert pre-encrypted bytes directly instead."
                                .to_string(),
                        ));
                    }
                    let key = self.secret_aes_key().ok_or_else(|| {
                        RedDBError::Query(
                            "SECRET() column encryption requires a bootstrapped \
                             vault (red.secret.aes_key is missing). Start the server \
                             with --vault to enable."
                                .to_string(),
                        )
                    })?;
                    let plain = &bytes[PLAINTEXT_SENTINEL.len()..];
                    Ok(Value::Secret(encrypt_secret_payload(&key, plain)))
                } else {
                    Ok(Value::Secret(bytes))
                }
            }
            other => Ok(other),
        }
    }
}

/// Encode an AES-256-GCM ciphertext as `[12-byte nonce][ciphertext||tag]`.
/// This is the on-disk representation of `Value::Secret`.
fn encrypt_secret_payload(key: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
    let nonce_bytes = crate::auth::store::random_bytes(12);
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&nonce_bytes[..12]);
    let ct = crate::crypto::aes_gcm::aes256_gcm_encrypt(key, &nonce, b"reddb.secret", plaintext);
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

/// Decode a `Value::Secret` payload back to plaintext. Returns
/// `None` when the payload is too short or AES-GCM authentication
/// fails (tampered or wrong key).
pub(crate) fn decrypt_secret_payload(key: &[u8; 32], payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() < 12 {
        return None;
    }
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&payload[..12]);
    crate::crypto::aes_gcm::aes256_gcm_decrypt(key, &nonce, b"reddb.secret", &payload[12..]).ok()
}
