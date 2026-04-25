//! HMAC-signed request authentication (PLAN.md Phase 6.1).
//!
//! Stateless service-to-service auth that doesn't require a session
//! token. The client computes
//!
//!   sig = HMAC-SHA256(secret,
//!                     METHOD ++ "\n" ++
//!                     PATH ++ "\n" ++
//!                     X-RedDB-Timestamp ++ "\n" ++
//!                     SHA256(body))
//!
//! and sends it as `X-RedDB-Signature: <hex>` plus `X-RedDB-Timestamp`
//! and `X-RedDB-Key-Id` headers. The server looks up the secret by
//! key id, recomputes the HMAC, compares constant-time, and refuses
//! requests whose timestamp falls outside `MAX_SKEW_SECS`.
//!
//! ## Why a separate scheme alongside bearer tokens
//!
//! Bearer tokens leak: they show up in logs, request mirrors, error
//! reports. HMAC-signed requests carry a derived signature that's
//! useless without the timestamped body — replays past the skew
//! window fail closed, and the secret never travels in the request.
//!
//! ## Wiring
//!
//! - `RED_HMAC_KEYS` env carries the keystore as `id1:secret1,id2:secret2,…`
//! - `RED_HMAC_KEYS_FILE` companion supports the K8s/Docker secrets pattern
//! - `MAX_SKEW_SECS` is fixed at 300s (5 min) — RFC 7235 Sec 4.4 spirit:
//!   short enough to bound replay window, long enough to absorb clock drift.
//!
//! When neither env var is set, the scheme is disabled and the gate
//! falls through to the bearer-token / public-endpoint policy.

use std::collections::BTreeMap;

const MAX_SKEW_SECS: i64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HmacOutcome {
    /// HMAC scheme is disabled (no `RED_HMAC_KEYS` configured).
    NotConfigured,
    /// Signature absent — caller may try the next auth scheme.
    NoSignaturePresented,
    /// Signature present and valid.
    Valid,
    /// Signature present but doesn't match — fail closed.
    Invalid,
    /// Timestamp outside the allowed skew window.
    SkewedTimestamp,
    /// Key id presented but not in the keystore.
    UnknownKeyId,
}

#[derive(Debug)]
pub struct HmacKeyStore {
    keys: BTreeMap<String, Vec<u8>>,
}

impl HmacKeyStore {
    pub fn from_env() -> Self {
        let raw = read_env_or_file("RED_HMAC_KEYS", "RED_HMAC_KEYS_FILE").unwrap_or_default();
        let mut keys = BTreeMap::new();
        for entry in raw.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            if let Some((id, secret)) = entry.split_once(':') {
                let id = id.trim().to_string();
                let secret = secret.trim().as_bytes().to_vec();
                if !id.is_empty() && !secret.is_empty() {
                    keys.insert(id, secret);
                }
            }
        }
        Self { keys }
    }

    pub fn is_configured(&self) -> bool {
        !self.keys.is_empty()
    }

    pub fn get(&self, key_id: &str) -> Option<&[u8]> {
        self.keys.get(key_id).map(|v| v.as_slice())
    }
}

/// Verify the signed-request triple `(key_id, ts, sig)` against the
/// canonical string built from method/path/timestamp/body-hash.
pub fn verify(
    store: &HmacKeyStore,
    method: &str,
    path: &str,
    body: &[u8],
    headers: &BTreeMap<String, String>,
) -> HmacOutcome {
    if !store.is_configured() {
        return HmacOutcome::NotConfigured;
    }

    let key_id = match headers.get("x-reddb-key-id") {
        Some(v) if !v.trim().is_empty() => v.trim(),
        _ => return HmacOutcome::NoSignaturePresented,
    };
    let presented_sig = match headers.get("x-reddb-signature") {
        Some(v) if !v.trim().is_empty() => v.trim(),
        _ => return HmacOutcome::NoSignaturePresented,
    };
    let ts_str = match headers.get("x-reddb-timestamp") {
        Some(v) if !v.trim().is_empty() => v.trim(),
        _ => return HmacOutcome::NoSignaturePresented,
    };
    let ts = match ts_str.parse::<i64>() {
        Ok(v) => v,
        Err(_) => return HmacOutcome::Invalid,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if (now - ts).abs() > MAX_SKEW_SECS {
        return HmacOutcome::SkewedTimestamp;
    }

    let secret = match store.get(key_id) {
        Some(s) => s,
        None => return HmacOutcome::UnknownKeyId,
    };

    let body_hash_hex = body_hash_hex(body);
    let canonical = format!("{method}\n{path}\n{ts_str}\n{body_hash_hex}");
    let expected = crate::crypto::hmac_sha256(secret, canonical.as_bytes());
    let expected_hex = crate::utils::to_hex(&expected);

    if crate::crypto::constant_time_eq(presented_sig.as_bytes(), expected_hex.as_bytes()) {
        HmacOutcome::Valid
    } else {
        HmacOutcome::Invalid
    }
}

fn body_hash_hex(body: &[u8]) -> String {
    crate::utils::to_hex(&crate::crypto::sha256(body))
}

fn read_env_or_file(env: &str, env_file: &str) -> Option<String> {
    // The env var name is the canonical form; the helper appends `_FILE`.
    if env.ends_with("_FILE") || !env_file.starts_with(env) {
        // Custom name pair — fall back to the explicit two-arg lookup.
        if let Ok(value) = std::env::var(env) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
        let path = std::env::var(env_file).ok()?;
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return None;
        }
        return std::fs::read_to_string(trimmed)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
    }
    crate::utils::env_with_file_fallback(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(id: &str, secret: &str) -> HmacKeyStore {
        let mut keys = BTreeMap::new();
        keys.insert(id.to_string(), secret.as_bytes().to_vec());
        HmacKeyStore { keys }
    }

    fn now_ts() -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string()
    }

    fn sign(store: &HmacKeyStore, key_id: &str, method: &str, path: &str, ts: &str, body: &[u8]) -> String {
        let secret = store.get(key_id).unwrap();
        let canonical = format!("{method}\n{path}\n{ts}\n{}", body_hash_hex(body));
        let sig = crate::crypto::hmac_sha256(secret, canonical.as_bytes());
        hex_encode(&sig)
    }

    #[test]
    fn unconfigured_store_returns_not_configured() {
        let store = HmacKeyStore { keys: BTreeMap::new() };
        let headers = BTreeMap::new();
        assert_eq!(
            verify(&store, "GET", "/x", b"", &headers),
            HmacOutcome::NotConfigured
        );
    }

    #[test]
    fn valid_signature_passes() {
        let store = store_with("k1", "supersecret");
        let ts = now_ts();
        let body = b"{\"foo\":1}";
        let sig = sign(&store, "k1", "POST", "/api/items", &ts, body);
        let mut headers = BTreeMap::new();
        headers.insert("x-reddb-key-id".into(), "k1".into());
        headers.insert("x-reddb-timestamp".into(), ts);
        headers.insert("x-reddb-signature".into(), sig);
        assert_eq!(
            verify(&store, "POST", "/api/items", body, &headers),
            HmacOutcome::Valid
        );
    }

    #[test]
    fn tampered_body_fails_closed() {
        let store = store_with("k1", "supersecret");
        let ts = now_ts();
        let body = b"{\"foo\":1}";
        let sig = sign(&store, "k1", "POST", "/api/items", &ts, body);
        let tampered = b"{\"foo\":2}";
        let mut headers = BTreeMap::new();
        headers.insert("x-reddb-key-id".into(), "k1".into());
        headers.insert("x-reddb-timestamp".into(), ts);
        headers.insert("x-reddb-signature".into(), sig);
        assert_eq!(
            verify(&store, "POST", "/api/items", tampered, &headers),
            HmacOutcome::Invalid
        );
    }

    #[test]
    fn wrong_method_fails_closed() {
        let store = store_with("k1", "supersecret");
        let ts = now_ts();
        let body = b"";
        let sig = sign(&store, "k1", "POST", "/x", &ts, body);
        let mut headers = BTreeMap::new();
        headers.insert("x-reddb-key-id".into(), "k1".into());
        headers.insert("x-reddb-timestamp".into(), ts);
        headers.insert("x-reddb-signature".into(), sig);
        assert_eq!(
            verify(&store, "GET", "/x", body, &headers),
            HmacOutcome::Invalid
        );
    }

    #[test]
    fn skewed_timestamp_rejected() {
        let store = store_with("k1", "supersecret");
        // 1 hour in the past — well outside MAX_SKEW_SECS=300.
        let stale_ts = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 3600)
            .to_string();
        let body = b"";
        let sig = sign(&store, "k1", "GET", "/x", &stale_ts, body);
        let mut headers = BTreeMap::new();
        headers.insert("x-reddb-key-id".into(), "k1".into());
        headers.insert("x-reddb-timestamp".into(), stale_ts);
        headers.insert("x-reddb-signature".into(), sig);
        assert_eq!(
            verify(&store, "GET", "/x", body, &headers),
            HmacOutcome::SkewedTimestamp
        );
    }

    #[test]
    fn unknown_key_id_fails_closed() {
        let store = store_with("k1", "supersecret");
        let ts = now_ts();
        let mut headers = BTreeMap::new();
        headers.insert("x-reddb-key-id".into(), "nope".into());
        headers.insert("x-reddb-timestamp".into(), ts);
        headers.insert("x-reddb-signature".into(), "deadbeef".into());
        assert_eq!(
            verify(&store, "GET", "/x", b"", &headers),
            HmacOutcome::UnknownKeyId
        );
    }

    #[test]
    fn missing_headers_treated_as_no_signature() {
        let store = store_with("k1", "supersecret");
        let headers = BTreeMap::new();
        assert_eq!(
            verify(&store, "GET", "/x", b"", &headers),
            HmacOutcome::NoSignaturePresented
        );
    }
}
