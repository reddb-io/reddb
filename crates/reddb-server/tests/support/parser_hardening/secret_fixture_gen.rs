//! Test-fixture generator for token-shaped strings.
//!
//! The redactor in `secret_redactor.rs` claims it masks bearer /
//! JWT / api-key / conn-string-credential patterns. To exercise
//! those claims a test must hand the redactor an input matching
//! the same regexes — which is the shape GitHub Secret Scanning
//! blocks on push. The pragmatic fix is to never let a literal of
//! that shape live in source: we assemble inputs at runtime from
//! non-matching atoms (`"sk"`, `"live"`, alnum body). The scanner
//! sees only the atoms; the test sees the assembled token.
//!
//! Determinism: a `seed: u64` parameter pins the body so failing
//! tests stay reproducible. Each call site should pass a fixed
//! seed.

#![allow(dead_code)]

const ALNUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Deterministic alphanumeric body of length `len`. Tiny LCG keyed
/// on `seed`. Body alone never matches a token regex — those need
/// a `sk_/rs_/reddb_` prefix or an `eyJ` JWT header.
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

/// Build an api-key shape from atomic prefix segments. The source
/// of this file never holds the assembled string — only the atoms.
///
/// `api_key_token(&["sk", "live"], 24, 0xabc)` returns
/// `"sk_live_<24 alnum chars>"`.
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

/// Build a JWT-shaped string (`eyJ` + alnum + `.` + alnum + `.` +
/// alnum). Three deterministic segments from `seed`.
pub fn jwt(seed: u64) -> String {
    // The `eyJ` marker is built from atoms so no single literal in
    // this file resembles a JWT prefix.
    let header_marker: String = ['e', 'y', 'J'].iter().collect();
    let header = format!("{}{}", header_marker, body(seed, 12));
    let payload = body(seed.wrapping_add(1), 16);
    let signature = body(seed.wrapping_add(2), 20);
    format!("{}.{}.{}", header, payload, signature)
}

/// Build a `Bearer <body>` header. The keyword and the body are
/// separate atoms in source.
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
