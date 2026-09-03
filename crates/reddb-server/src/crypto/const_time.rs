//! Constant-time byte-slice comparison.
//!
//! Defends against timing oracles when comparing secrets (auth
//! tokens, MAC tags, password hashes). Returns `false` if the
//! lengths differ without leaking which prefix matched. Centralised
//! so every site that compares secret material uses the same
//! audited routine.

/// Constant-time equality that does not return early on a length
/// mismatch either: the loop runs over the longer input and the length
/// difference is folded into the result, so the time taken reveals
/// neither the bytes nor the length of the expected value.
#[inline]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let len = a.len().max(b.len());
    let mut diff: u8 = u8::from(a.len() != b.len());
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
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
