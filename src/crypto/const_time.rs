//! Constant-time byte-slice comparison.
//!
//! Defends against timing oracles when comparing secrets (auth
//! tokens, MAC tags, password hashes). Returns `false` if the
//! lengths differ without leaking which prefix matched. Centralised
//! so every site that compares secret material uses the same
//! audited routine.

#[inline]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_slices_match() {
        assert!(constant_time_eq(b"hello", b"hello"));
    }

    #[test]
    fn different_content_fails() {
        assert!(!constant_time_eq(b"hello", b"world"));
    }

    #[test]
    fn different_lengths_fail_without_panic() {
        assert!(!constant_time_eq(b"x", b"xx"));
        assert!(!constant_time_eq(b"", b"x"));
    }

    #[test]
    fn empty_slices_match() {
        assert!(constant_time_eq(b"", b""));
    }
}
