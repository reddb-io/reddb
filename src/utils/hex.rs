//! Tiny hex encoder used wherever a byte slice needs to be rendered
//! as a hex string. Centralised so the same `bytes.iter().map(|b|
//! format!("{:02x}", b)).collect()` doesn't get re-typed across the
//! crate.

/// Encode `bytes` as a lowercase hex string.
#[inline]
pub fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// Encode the first `n` bytes (or fewer) as lowercase hex. Useful
/// for short identifiers like `bearer:<sha256-prefix>` labels.
#[inline]
pub fn to_hex_prefix(bytes: &[u8], n: usize) -> String {
    let mut out = String::with_capacity(n.min(bytes.len()) * 2);
    for b in bytes.iter().take(n) {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_empty_string() {
        assert_eq!(to_hex(&[]), "");
    }

    #[test]
    fn known_bytes_round_trip() {
        assert_eq!(to_hex(&[0x01, 0x02, 0xab, 0xff]), "0102abff");
    }

    #[test]
    fn prefix_caps_at_n() {
        assert_eq!(to_hex_prefix(&[0xde, 0xad, 0xbe, 0xef], 2), "dead");
        assert_eq!(to_hex_prefix(&[0xde, 0xad], 8), "dead");
    }
}
