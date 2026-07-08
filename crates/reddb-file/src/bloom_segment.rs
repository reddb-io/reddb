//! Persisted bloom-segment frame shared by storage/index owners.

use std::fmt;

use crate::BLOOM_SEGMENT_V2_MAGIC;

pub const BLOOM_SEGMENT_HEADER_LEN: usize = 1 + 1 + 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BloomSegmentFrameError {
    TooShort,
    BadMagic,
    LengthMismatch,
}

impl fmt::Display for BloomSegmentFrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => write!(f, "bloom header too short"),
            Self::BadMagic => write!(f, "bloom header magic mismatch"),
            Self::LengthMismatch => write!(f, "bloom header length mismatch"),
        }
    }
}

impl std::error::Error for BloomSegmentFrameError {}

pub fn encode_bloom_segment_frame(inserted: u32, bloom_blob: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(BLOOM_SEGMENT_HEADER_LEN + bloom_blob.len());
    out.push(BLOOM_SEGMENT_V2_MAGIC);
    out.push(0);
    out.extend_from_slice(&inserted.to_be_bytes());
    out.extend_from_slice(bloom_blob);
    out
}

pub fn decode_bloom_segment_frame(
    bytes: &[u8],
) -> Result<(u32, Vec<u8>, usize), BloomSegmentFrameError> {
    if bytes.len() < BLOOM_SEGMENT_HEADER_LEN {
        return Err(BloomSegmentFrameError::TooShort);
    }
    if bytes[0] != BLOOM_SEGMENT_V2_MAGIC {
        return Err(BloomSegmentFrameError::BadMagic);
    }

    let inserted = u32::from_be_bytes(bytes[2..6].try_into().expect("len checked"));
    if bytes.len() < BLOOM_SEGMENT_HEADER_LEN + 4 {
        return Err(BloomSegmentFrameError::LengthMismatch);
    }
    let num_blocks = u32::from_le_bytes(bytes[6..10].try_into().expect("len checked")) as usize;
    if num_blocks == 0 || !num_blocks.is_power_of_two() {
        return Err(BloomSegmentFrameError::LengthMismatch);
    }
    let payload_len = 4 + num_blocks * 32;
    let total = BLOOM_SEGMENT_HEADER_LEN + payload_len;
    if bytes.len() < total {
        return Err(BloomSegmentFrameError::LengthMismatch);
    }

    Ok((
        inserted,
        bytes[BLOOM_SEGMENT_HEADER_LEN..total].to_vec(),
        total,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bloom_segment_frame_round_trips_v2_header() {
        let mut bloom_blob = Vec::new();
        bloom_blob.extend_from_slice(&1u32.to_le_bytes());
        bloom_blob.extend_from_slice(&[0xAB; 32]);

        let encoded = encode_bloom_segment_frame(42, &bloom_blob);

        assert_eq!(encoded[0], BLOOM_SEGMENT_V2_MAGIC);
        assert_eq!(encoded[1], 0);
        assert_eq!(&encoded[2..6], &42u32.to_be_bytes());

        let (inserted, decoded_blob, consumed) =
            decode_bloom_segment_frame(&encoded).expect("decode frame");
        assert_eq!(inserted, 42);
        assert_eq!(decoded_blob, bloom_blob);
        assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn bloom_segment_frame_rejects_bad_inputs() {
        assert_eq!(
            decode_bloom_segment_frame(&[]).unwrap_err(),
            BloomSegmentFrameError::TooShort
        );

        let mut bloom_blob = Vec::new();
        bloom_blob.extend_from_slice(&1u32.to_le_bytes());
        bloom_blob.extend_from_slice(&[0xAB; 32]);

        let mut bad_magic = encode_bloom_segment_frame(1, &bloom_blob);
        bad_magic[0] = 0;
        assert_eq!(
            decode_bloom_segment_frame(&bad_magic).unwrap_err(),
            BloomSegmentFrameError::BadMagic
        );

        let mut truncated = encode_bloom_segment_frame(1, &bloom_blob);
        truncated.pop();
        assert_eq!(
            decode_bloom_segment_frame(&truncated).unwrap_err(),
            BloomSegmentFrameError::LengthMismatch
        );
    }
}
