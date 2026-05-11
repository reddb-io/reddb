//! Test-fixture generator for token-shaped strings.
//!
//! Slim mirror of `reddb-server`'s generator — kept duplicated so
//! `reddb-wire`'s test tree does not depend on the server crate's
//! support modules.

#![allow(dead_code)]

const ALNUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Deterministic alphanumeric body of length `len`.
pub fn body(seed: u64, len: usize) -> String {
    let mut s = String::with_capacity(len);
    let mut x = seed.wrapping_add(1).wrapping_mul(2862933555777941757);
    for _ in 0..len {
        x = x.wrapping_mul(2862933555777941757).wrapping_add(3037000493);
        let idx = ((x >> 33) as usize) % ALNUM.len();
        s.push(ALNUM[idx] as char);
    }
    s
}

/// Build an api-key shape from atomic prefix segments.
pub fn api_key_token(prefix_parts: &[&str], body_len: usize, seed: u64) -> String {
    let body = body(seed, body_len);
    let mut s = String::new();
    for (i, p) in prefix_parts.iter().enumerate() {
        if i > 0 {
            s.push('_');
        }
        s.push_str(p);
    }
    s.push('_');
    s.push_str(&body);
    s
}

/// Build a JWT-shaped string assembled from atoms.
pub fn jwt(seed: u64) -> String {
    let header_marker: String = ['e', 'y', 'J'].iter().collect();
    let header = format!("{}{}", header_marker, body(seed, 12));
    let payload = body(seed.wrapping_add(1), 16);
    let signature = body(seed.wrapping_add(2), 20);
    format!("{}.{}.{}", header, payload, signature)
}

/// Build a `Bearer <body>` header.
pub fn bearer_header(seed: u64) -> String {
    let body = body(seed, 32);
    format!("{} {}", "Bearer", body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_is_deterministic_per_seed() {
        assert_eq!(body(1, 16), body(1, 16));
        assert_ne!(body(1, 16), body(2, 16));
        assert_eq!(body(7, 24).len(), 24);
    }

    #[test]
    fn body_is_alphanumeric() {
        let s = body(42, 64);
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn api_key_token_shape() {
        let s = api_key_token(&["sk", "live"], 24, 0xabc);
        assert!(s.starts_with("sk_live_"));
        assert_eq!(s.len(), "sk_live_".len() + 24);
    }

    #[test]
    fn jwt_has_three_segments() {
        let s = jwt(0xdead);
        assert_eq!(s.matches('.').count(), 2);
        assert!(s.starts_with("eyJ"));
    }
}
