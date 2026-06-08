//! Large-value codec for vector/B-tree persisted payloads.
//!
//! This byte-slice codec knows nothing about pages, MVCC, or overflow chains.
//! It stores either raw bytes or an LZ4 block prefixed with the original length.

/// One-byte tag stored alongside the encoded payload.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueFlag {
    /// Payload bytes are the original input, byte-identical.
    Raw = 0,
    /// Payload bytes are an LZ4 block with a little-endian `u32` raw length.
    Lz4 = 1,
}

impl ValueFlag {
    /// Convert from the on-disk tag byte. Unknown tags are rejected.
    pub fn from_byte(b: u8) -> Result<Self, ValueCodecError> {
        match b {
            0 => Ok(ValueFlag::Raw),
            1 => Ok(ValueFlag::Lz4),
            other => Err(ValueCodecError::UnknownFlag(other)),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ValueCodecError {
    UnknownFlag(u8),
    TruncatedHeader,
    Lz4Decode(String),
}

impl std::fmt::Display for ValueCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValueCodecError::UnknownFlag(b) => write!(f, "unknown value codec flag: {}", b),
            ValueCodecError::TruncatedHeader => write!(
                f,
                "compressed payload truncated: need at least 4 bytes for length header"
            ),
            ValueCodecError::Lz4Decode(msg) => write!(f, "lz4 decode failed: {}", msg),
        }
    }
}

impl std::error::Error for ValueCodecError {}

/// Encode `input` for storage. Returns the flag and stored bytes.
///
/// When LZ4 would not shrink the input including the 4-byte raw length header,
/// the codec returns raw bytes. Equal size is treated as no win.
pub fn encode(input: &[u8]) -> (ValueFlag, Vec<u8>) {
    if input.is_empty() {
        return (ValueFlag::Raw, Vec::new());
    }

    let compressed = lz4_flex::compress(input);
    if compressed.len() + 4 < input.len() {
        let mut out = Vec::with_capacity(compressed.len() + 4);
        out.extend_from_slice(&(input.len() as u32).to_le_bytes());
        out.extend_from_slice(&compressed);
        (ValueFlag::Lz4, out)
    } else {
        (ValueFlag::Raw, input.to_vec())
    }
}

/// Return the on-disk size [`encode`] would choose without returning the bytes.
pub fn would_encode_to(input: &[u8]) -> usize {
    if input.is_empty() {
        return 0;
    }
    let compressed_len = lz4_flex::compress(input).len();
    let lz4_total = compressed_len + 4;
    if lz4_total < input.len() {
        lz4_total
    } else {
        input.len()
    }
}

/// Decode a `(flag, bytes)` pair produced by [`encode`].
pub fn decode(flag: ValueFlag, bytes: &[u8]) -> Result<Vec<u8>, ValueCodecError> {
    match flag {
        ValueFlag::Raw => Ok(bytes.to_vec()),
        ValueFlag::Lz4 => {
            if bytes.len() < 4 {
                return Err(ValueCodecError::TruncatedHeader);
            }
            let raw_len = u32::from_le_bytes(bytes[0..4].try_into().expect("len checked")) as usize;
            lz4_flex::decompress(&bytes[4..], raw_len)
                .map_err(|e| ValueCodecError::Lz4Decode(e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_compressible_text() {
        let input = "the quick brown fox jumps over the lazy dog "
            .repeat(64)
            .into_bytes();
        let (flag, bytes) = encode(&input);
        assert_eq!(flag, ValueFlag::Lz4, "highly repetitive text must compress");
        assert!(
            bytes.len() < input.len(),
            "stored size {} must be less than input {}",
            bytes.len(),
            input.len()
        );
        let decoded = decode(flag, &bytes).expect("decode");
        assert_eq!(decoded, input);
    }

    #[test]
    fn round_trip_incompressible_random() {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let input: Vec<u8> = (0..512)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let (flag, bytes) = encode(&input);
        assert_eq!(
            flag,
            ValueFlag::Raw,
            "incompressible input must fall back to raw"
        );
        assert_eq!(bytes, input, "raw bytes must be byte-identical");
        let decoded = decode(flag, &bytes).expect("decode");
        assert_eq!(decoded, input);
    }

    #[test]
    fn empty_input_round_trips_as_raw() {
        let (flag, bytes) = encode(&[]);
        assert_eq!(flag, ValueFlag::Raw);
        assert!(bytes.is_empty());
        let decoded = decode(flag, &bytes).expect("decode empty");
        assert!(decoded.is_empty());
    }

    #[test]
    fn exact_threshold_falls_back_to_raw() {
        let input = vec![0x42u8];
        let (flag, bytes) = encode(&input);
        assert_eq!(flag, ValueFlag::Raw);
        assert_eq!(bytes, input);
    }

    #[test]
    fn flag_distinguishes_compressed_and_raw() {
        let compressible = vec![b'a'; 256];
        let (flag_c, _) = encode(&compressible);
        let (flag_r, _) = encode(&[0xAB, 0xCD, 0xEF]);
        assert_eq!(flag_c, ValueFlag::Lz4);
        assert_eq!(flag_r, ValueFlag::Raw);
        assert_ne!(flag_c, flag_r);
    }

    #[test]
    fn flag_byte_round_trips() {
        assert_eq!(ValueFlag::from_byte(0).unwrap(), ValueFlag::Raw);
        assert_eq!(ValueFlag::from_byte(1).unwrap(), ValueFlag::Lz4);
        assert_eq!(
            ValueFlag::from_byte(255).unwrap_err(),
            ValueCodecError::UnknownFlag(255)
        );
    }

    #[test]
    fn would_encode_to_matches_actual_encode() {
        let compressible = vec![b'x'; 1024];
        let (_, bytes) = encode(&compressible);
        assert_eq!(would_encode_to(&compressible), bytes.len());

        let mut state: u64 = 0xDEAD_BEEF_1234_5678;
        let random: Vec<u8> = (0..256)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let (_, bytes) = encode(&random);
        assert_eq!(would_encode_to(&random), bytes.len());

        assert_eq!(would_encode_to(&[]), 0);
    }

    #[test]
    fn would_encode_to_decouples_from_spill_decision() {
        let blob = vec![b'z'; 4096];
        let projected = would_encode_to(&blob);
        let fits_in_64 = projected <= 64;

        let (flag, bytes) = encode(&blob);
        assert_eq!(bytes.len(), projected);
        assert_eq!(decode(flag, &bytes).unwrap(), blob);
        assert!(fits_in_64);
    }

    #[test]
    fn decode_rejects_unknown_flag_byte() {
        assert!(matches!(
            ValueFlag::from_byte(7),
            Err(ValueCodecError::UnknownFlag(7))
        ));
    }

    #[test]
    fn decode_rejects_truncated_lz4_header() {
        let err = decode(ValueFlag::Lz4, &[0x01, 0x02]).unwrap_err();
        assert_eq!(err, ValueCodecError::TruncatedHeader);
    }
}
