//! Encryption-key parsing — a mandatory encrypt parameter homed here
//! per #1053 / ADR 0054 (carried forward from the retired RDEP
//! envelope).
//!
//! Accepts a 32-byte AES-256 key as either 64 hex chars or
//! (un)padded standard base64, tolerating surrounding whitespace
//! (e.g. the trailing newline `kubectl create secret` leaves on a
//! key file). Reading the key from the environment stays in
//! `reddb-server` because it layers a server-specific
//! file-fallback convention on top of this parser.

/// Parse a 32-byte AES key from a string — accepts hex (64 chars) or
/// unpadded/padded standard base64 (43 or 44 chars). Tolerates
/// leading/trailing whitespace including newlines.
pub fn parse_key(raw: &str) -> Result<[u8; 32], String> {
    let trimmed = raw.trim();
    // Hex: exactly 64 hex digits.
    if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&trimmed[i * 2..i * 2 + 2], 16)
                .map_err(|err| format!("invalid hex key byte {i}: {err}"))?;
        }
        return Ok(out);
    }
    // Base64: standard alphabet, 32 raw bytes → 44 chars padded or
    // 43 unpadded. A tiny inline decoder avoids pulling a base64
    // crate just for this.
    let decoded = decode_base64(trimmed)
        .map_err(|err| format!("key is neither 64-hex nor base64 (decode error: {err})"))?;
    if decoded.len() != 32 {
        return Err(format!(
            "decoded key is {} bytes; AES-256-GCM requires exactly 32",
            decoded.len()
        ));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&decoded);
    Ok(out)
}

fn decode_base64(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = s
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut i = 0;
    while i + 3 < bytes.len() {
        let a = val(bytes[i]).ok_or_else(|| format!("invalid base64 char at {i}"))?;
        let b = val(bytes[i + 1]).ok_or_else(|| format!("invalid base64 char at {}", i + 1))?;
        let c = val(bytes[i + 2]).ok_or_else(|| format!("invalid base64 char at {}", i + 2))?;
        let d = val(bytes[i + 3]).ok_or_else(|| format!("invalid base64 char at {}", i + 3))?;
        out.push((a << 2) | (b >> 4));
        out.push(((b & 0x0F) << 4) | (c >> 2));
        out.push(((c & 0x03) << 6) | d);
        i += 4;
    }
    let rem = bytes.len() - i;
    match rem {
        0 => {}
        2 => {
            let a = val(bytes[i]).ok_or_else(|| format!("invalid base64 char at {i}"))?;
            let b = val(bytes[i + 1]).ok_or_else(|| format!("invalid base64 char at {}", i + 1))?;
            out.push((a << 2) | (b >> 4));
        }
        3 => {
            let a = val(bytes[i]).ok_or_else(|| format!("invalid base64 char at {i}"))?;
            let b = val(bytes[i + 1]).ok_or_else(|| format!("invalid base64 char at {}", i + 1))?;
            let c = val(bytes[i + 2]).ok_or_else(|| format!("invalid base64 char at {}", i + 2))?;
            out.push((a << 2) | (b >> 4));
            out.push(((b & 0x0F) << 4) | (c >> 2));
        }
        _ => return Err(format!("invalid base64 length remainder {rem}")),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_accepts_hex() {
        let hex = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let key = parse_key(hex).unwrap();
        assert_eq!(key[0], 0x01);
        assert_eq!(key[31], 0x20);
    }

    #[test]
    fn parse_key_accepts_hex_with_whitespace() {
        let hex = "  0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20\n";
        assert!(parse_key(hex).is_ok());
    }

    #[test]
    fn parse_key_rejects_wrong_length() {
        assert!(parse_key("ab").is_err());
        assert!(parse_key("zz".repeat(32).as_str()).is_err()); // 64 chars but not hex
    }

    #[test]
    fn parse_key_accepts_base64() {
        // 32 bytes of 0xAB base64-encoded, encoded inline to avoid a crate.
        let raw = vec![0xAB_u8; 32];
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        let mut i = 0;
        while i + 3 <= raw.len() {
            let n = ((raw[i] as u32) << 16) | ((raw[i + 1] as u32) << 8) | (raw[i + 2] as u32);
            out.push(alphabet[((n >> 18) & 0x3F) as usize] as char);
            out.push(alphabet[((n >> 12) & 0x3F) as usize] as char);
            out.push(alphabet[((n >> 6) & 0x3F) as usize] as char);
            out.push(alphabet[(n & 0x3F) as usize] as char);
            i += 3;
        }
        if i < raw.len() {
            let rem = raw.len() - i;
            let n = if rem == 1 {
                (raw[i] as u32) << 16
            } else {
                ((raw[i] as u32) << 16) | ((raw[i + 1] as u32) << 8)
            };
            out.push(alphabet[((n >> 18) & 0x3F) as usize] as char);
            out.push(alphabet[((n >> 12) & 0x3F) as usize] as char);
            if rem == 2 {
                out.push(alphabet[((n >> 6) & 0x3F) as usize] as char);
            }
        }
        let key = parse_key(&out).unwrap();
        assert_eq!(key, [0xABu8; 32]);
    }
}
