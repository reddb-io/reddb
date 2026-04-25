//! Encryption-at-rest framing for RedDB pages (PLAN.md Phase 6.3).
//!
//! Wraps AES-256-GCM in a stable on-disk envelope so a future
//! pager rewrite can encrypt/decrypt pages atomically without
//! reinventing the format. The envelope is self-describing: a
//! reader sees the magic+version and knows whether the page is
//! encrypted or plaintext, regardless of the runtime's current
//! configuration.
//!
//! ## On-disk frame
//!
//! ```text
//! [0..4]   magic = "RDEP" (RedDB Encrypted Page)
//! [4]      version = 0x01
//! [5..17]  nonce (12 bytes, random per page)
//! [17..]   ciphertext + 16-byte GCM tag
//! ```
//!
//! ## Properties
//!
//! - **Random nonce per page**: the pager calls `encrypt_page` once
//!   per page write; collisions across `2^96` pages are
//!   astronomically unlikely. Sequential nonce schemes are not used
//!   to keep the API stateless.
//! - **AAD = page_id**: binds the ciphertext to its page slot so a
//!   peer page swap is detected as a tag mismatch on decrypt.
//! - **Stable framing**: the magic + version let the pager detect
//!   "this DB is encrypted but the operator forgot
//!   `RED_ENCRYPTION_KEY_FILE`" cleanly, returning a typed error
//!   instead of garbage bytes.
//!
//! ## Wiring (deferred)
//!
//! This module is the foundation; no live writer uses it yet. The
//! pager hookup is gated on a format-version bump and is tracked as
//! a separate task. Encrypt/decrypt are exposed publicly so
//! one-shot tools (export, restore, snapshot rewrite) can adopt the
//! frame ahead of the pager.

use crate::crypto::aes_gcm::{aes256_gcm_decrypt, aes256_gcm_encrypt};
use crate::crypto::os_random;

/// 4-byte magic identifying an encrypted page envelope.
pub const FRAME_MAGIC: [u8; 4] = *b"RDEP";

/// Current envelope schema version.
pub const FRAME_VERSION: u8 = 0x01;

/// Fixed envelope overhead: magic (4) + version (1) + nonce (12) +
/// GCM tag (16). Plaintext expands by exactly this many bytes.
pub const FRAME_OVERHEAD: usize = 4 + 1 + 12 + 16;

/// Errors returned by the page-encryption surface. Caller (the
/// pager) maps these to its own typed error.
#[derive(Debug)]
pub enum PageEncryptionError {
    InvalidMagic,
    UnsupportedVersion(u8),
    Truncated,
    KeyMismatch(String),
    RandomFailure(String),
}

impl std::fmt::Display for PageEncryptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidMagic => f.write_str("encrypted page: bad magic — page not produced by encrypt_page"),
            Self::UnsupportedVersion(v) => write!(f, "encrypted page: unsupported version {v}"),
            Self::Truncated => f.write_str("encrypted page: truncated frame"),
            Self::KeyMismatch(detail) => write!(f, "encrypted page: key mismatch or tampering ({detail})"),
            Self::RandomFailure(detail) => {
                write!(f, "encrypted page: nonce generation failed ({detail})")
            }
        }
    }
}

impl std::error::Error for PageEncryptionError {}

/// Encrypt `plaintext` for storage. `page_id` is bound as AAD so
/// swapping two pages on disk fails the tag check on decrypt.
pub fn encrypt_page(
    key: &[u8; 32],
    page_id: u64,
    plaintext: &[u8],
) -> Result<Vec<u8>, PageEncryptionError> {
    let mut nonce = [0u8; 12];
    os_random::fill_bytes(&mut nonce).map_err(PageEncryptionError::RandomFailure)?;
    let aad = page_id.to_le_bytes();
    let ciphertext = aes256_gcm_encrypt(key, &nonce, &aad, plaintext);

    let mut out = Vec::with_capacity(FRAME_OVERHEAD + plaintext.len());
    out.extend_from_slice(&FRAME_MAGIC);
    out.push(FRAME_VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt an envelope produced by `encrypt_page`. `page_id` MUST
/// match the value passed at encrypt time — a mismatch surfaces as
/// `KeyMismatch` (GCM tag check failure) which is the correct
/// signal: an attacker swapping pages is functionally indistinguishable
/// from a wrong key.
pub fn decrypt_page(
    key: &[u8; 32],
    page_id: u64,
    frame: &[u8],
) -> Result<Vec<u8>, PageEncryptionError> {
    if frame.len() < FRAME_OVERHEAD {
        return Err(PageEncryptionError::Truncated);
    }
    if frame[0..4] != FRAME_MAGIC {
        return Err(PageEncryptionError::InvalidMagic);
    }
    let version = frame[4];
    if version != FRAME_VERSION {
        return Err(PageEncryptionError::UnsupportedVersion(version));
    }
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&frame[5..17]);
    let aad = page_id.to_le_bytes();
    aes256_gcm_decrypt(key, &nonce, &aad, &frame[17..])
        .map_err(PageEncryptionError::KeyMismatch)
}

/// Cheap sniff: does this byte slice *look* like an encrypted page?
/// Used by the pager (post-wiring) to decide whether to call
/// `decrypt_page` or treat the bytes as plaintext on a mixed
/// pre/post-encryption database.
pub fn is_encrypted_frame(bytes: &[u8]) -> bool {
    bytes.len() >= FRAME_OVERHEAD && bytes[0..4] == FRAME_MAGIC
}

/// Parse a 32-byte AES key from a string — accepts hex (64 chars)
/// or unpadded base64 (43 or 44 chars). Tolerates leading/trailing
/// whitespace including newlines from `kubectl create secret`.
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
    // 43 chars unpadded. Use a tiny inline decoder so we don't pull
    // a base64 crate just for this.
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

/// Read the runtime encryption key from `RED_ENCRYPTION_KEY` /
/// `RED_ENCRYPTION_KEY_FILE`. Returns `None` when the operator
/// hasn't enabled at-rest encryption. Errors are surfaced as `Err`
/// so a misconfigured key (typo, wrong length) fails boot loudly
/// instead of silently leaving plaintext on disk.
pub fn key_from_env() -> Result<Option<[u8; 32]>, String> {
    match crate::utils::env_with_file_fallback("RED_ENCRYPTION_KEY") {
        Some(raw) => parse_key(&raw).map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; 32] {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8;
        }
        k
    }

    #[test]
    fn round_trips_plaintext() {
        let plaintext = b"page bytes that will be encrypted";
        let frame = encrypt_page(&key(), 7, plaintext).unwrap();
        assert_eq!(frame.len(), FRAME_OVERHEAD + plaintext.len());
        assert!(is_encrypted_frame(&frame));
        let recovered = decrypt_page(&key(), 7, &frame).unwrap();
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn nonce_is_random_per_call() {
        let plaintext = b"same payload, different nonce";
        let f1 = encrypt_page(&key(), 1, plaintext).unwrap();
        let f2 = encrypt_page(&key(), 1, plaintext).unwrap();
        // Same key + same plaintext + same page id but the nonce
        // differs, so the frames must too. This guards against
        // accidental nonce reuse which would break GCM secrecy.
        assert_ne!(f1, f2);
    }

    #[test]
    fn page_id_binding_catches_swapped_pages() {
        let plaintext = b"page 1 contents";
        let frame = encrypt_page(&key(), 1, plaintext).unwrap();
        // Decrypting the same bytes against page_id=2 must fail
        // (AAD mismatch) — proves the frame is bound to its slot.
        let err = decrypt_page(&key(), 2, &frame).unwrap_err();
        assert!(matches!(err, PageEncryptionError::KeyMismatch(_)), "got {err:?}");
    }

    #[test]
    fn wrong_key_fails_closed() {
        let plaintext = b"sensitive";
        let frame = encrypt_page(&key(), 5, plaintext).unwrap();
        let mut wrong = key();
        wrong[0] ^= 0xff;
        let err = decrypt_page(&wrong, 5, &frame).unwrap_err();
        assert!(matches!(err, PageEncryptionError::KeyMismatch(_)));
    }

    #[test]
    fn bad_magic_returns_typed_error() {
        let mut frame = encrypt_page(&key(), 0, b"x").unwrap();
        frame[0] ^= 0xff;
        let err = decrypt_page(&key(), 0, &frame).unwrap_err();
        assert!(matches!(err, PageEncryptionError::InvalidMagic));
    }

    #[test]
    fn unsupported_version_is_typed() {
        let mut frame = encrypt_page(&key(), 0, b"x").unwrap();
        frame[4] = 0xFE;
        let err = decrypt_page(&key(), 0, &frame).unwrap_err();
        assert!(matches!(err, PageEncryptionError::UnsupportedVersion(0xFE)));
    }

    #[test]
    fn truncated_frame_is_typed() {
        let frame = vec![0u8; FRAME_OVERHEAD - 1];
        let err = decrypt_page(&key(), 0, &frame).unwrap_err();
        assert!(matches!(err, PageEncryptionError::Truncated));
    }

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
        // 32 bytes of 0xAB base64-encoded.
        let raw = vec![0xAB_u8; 32];
        // Manual base64 to avoid pulling a crate just for the test.
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        let mut i = 0;
        while i + 3 <= raw.len() {
            let n =
                ((raw[i] as u32) << 16) | ((raw[i + 1] as u32) << 8) | (raw[i + 2] as u32);
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
