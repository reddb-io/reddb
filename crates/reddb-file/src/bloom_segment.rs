//! Persisted bloom-segment frame shared by storage/index owners.

use std::fmt;

use crate::BLOOM_SEGMENT_MAGIC;

pub const BLOOM_SEGMENT_HEADER_LEN: usize = 1 + 1 + 4 + 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BloomSegmentFrame {
    pub num_hashes: u8,
    pub bit_size: u32,
    pub inserted: u32,
    pub bits: Vec<u8>,
}

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

pub fn encode_bloom_segment_frame(frame: &BloomSegmentFrame) -> Vec<u8> {
    let mut out = Vec::with_capacity(BLOOM_SEGMENT_HEADER_LEN + frame.bits.len());
    out.push(BLOOM_SEGMENT_MAGIC);
    out.push(frame.num_hashes);
    out.extend_from_slice(&frame.bit_size.to_be_bytes());
    out.extend_from_slice(&frame.inserted.to_be_bytes());
    out.extend_from_slice(&frame.bits);
    out
}

pub fn decode_bloom_segment_frame(
    bytes: &[u8],
) -> Result<(BloomSegmentFrame, usize), BloomSegmentFrameError> {
    if bytes.len() < BLOOM_SEGMENT_HEADER_LEN {
        return Err(BloomSegmentFrameError::TooShort);
    }
    if bytes[0] != BLOOM_SEGMENT_MAGIC {
        return Err(BloomSegmentFrameError::BadMagic);
    }

    let num_hashes = bytes[1];
    let bit_size = u32::from_be_bytes(bytes[2..6].try_into().expect("len checked"));
    let inserted = u32::from_be_bytes(bytes[6..10].try_into().expect("len checked"));
    let byte_len = (bit_size as usize).div_ceil(8);
    let total = BLOOM_SEGMENT_HEADER_LEN + byte_len;
    if bytes.len() < total {
        return Err(BloomSegmentFrameError::LengthMismatch);
    }

    Ok((
        BloomSegmentFrame {
            num_hashes,
            bit_size,
            inserted,
            bits: bytes[BLOOM_SEGMENT_HEADER_LEN..total].to_vec(),
        },
        total,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bloom_segment_frame_round_trips_big_endian_header() {
        let frame = BloomSegmentFrame {
            num_hashes: 7,
            bit_size: 17,
            inserted: 42,
            bits: vec![0b1010_1010, 0b0101_0101, 0xFF],
        };

        let encoded = encode_bloom_segment_frame(&frame);

        assert_eq!(encoded[0], BLOOM_SEGMENT_MAGIC);
        assert_eq!(encoded[1], 7);
        assert_eq!(&encoded[2..6], &17u32.to_be_bytes());
        assert_eq!(&encoded[6..10], &42u32.to_be_bytes());

        let (decoded, consumed) = decode_bloom_segment_frame(&encoded).expect("decode frame");
        assert_eq!(decoded, frame);
        assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn bloom_segment_frame_rejects_bad_inputs() {
        assert_eq!(
            decode_bloom_segment_frame(&[]).unwrap_err(),
            BloomSegmentFrameError::TooShort
        );

        let mut bad_magic = encode_bloom_segment_frame(&BloomSegmentFrame {
            num_hashes: 3,
            bit_size: 8,
            inserted: 1,
            bits: vec![1],
        });
        bad_magic[0] = 0;
        assert_eq!(
            decode_bloom_segment_frame(&bad_magic).unwrap_err(),
            BloomSegmentFrameError::BadMagic
        );

        let mut truncated = encode_bloom_segment_frame(&BloomSegmentFrame {
            num_hashes: 3,
            bit_size: 16,
            inserted: 1,
            bits: vec![1, 2],
        });
        truncated.pop();
        assert_eq!(
            decode_bloom_segment_frame(&truncated).unwrap_err(),
            BloomSegmentFrameError::LengthMismatch
        );
    }
}
