//! Spill file frame contract.
//!
//! Runtime cache policy lives in `reddb-server`; the durable spill frame lives
//! here so the server does not define persisted bytes directly.

use std::fmt;

pub const SPILL_FILE_MAGIC: [u8; 4] = *b"SPIL";
pub const SPILL_FILE_VERSION_V1: u8 = 1;
pub const SPILL_FILE_VERSION_V2: u8 = 2;
pub const SPILL_FILE_HEADER_LEN: usize = 4 + 1 + 4 + 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpillFileFrameError {
    BadMagic,
    UnsupportedVersion(u8),
    ChecksumMismatch { expected: u32, actual: u32 },
    Truncated,
    SizeOverflow,
}

impl fmt::Display for SpillFileFrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => write!(f, "bad spill file magic"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported spill file version {version}")
            }
            Self::ChecksumMismatch { expected, actual } => write!(
                f,
                "spill file checksum mismatch: expected {expected:#010x}, got {actual:#010x}"
            ),
            Self::Truncated => write!(f, "truncated spill file frame"),
            Self::SizeOverflow => write!(f, "spill file payload size overflows this platform"),
        }
    }
}

impl std::error::Error for SpillFileFrameError {}

pub fn spill_file_name(segment: &str, pid: u32) -> String {
    format!("{segment}-{pid}.spill")
}

pub fn encode_spill_file_frame(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(SPILL_FILE_HEADER_LEN + data.len());
    out.extend_from_slice(&SPILL_FILE_MAGIC);
    out.push(SPILL_FILE_VERSION_V2);
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    out.extend_from_slice(data);
    out
}

pub fn decode_spill_file_frame(bytes: &[u8]) -> Result<Vec<u8>, SpillFileFrameError> {
    if bytes.len() < SPILL_FILE_HEADER_LEN {
        return Err(SpillFileFrameError::Truncated);
    }
    if bytes[..4] != SPILL_FILE_MAGIC {
        return Err(SpillFileFrameError::BadMagic);
    }

    let version = bytes[4];
    let expected_checksum = u32::from_le_bytes(bytes[5..9].try_into().expect("checksum slice"));
    let payload_len_u64 = u64::from_le_bytes(bytes[9..17].try_into().expect("size slice"));
    let payload_len: usize = payload_len_u64
        .try_into()
        .map_err(|_| SpillFileFrameError::SizeOverflow)?;

    let payload_end = SPILL_FILE_HEADER_LEN
        .checked_add(payload_len)
        .ok_or(SpillFileFrameError::SizeOverflow)?;
    if bytes.len() < payload_end {
        return Err(SpillFileFrameError::Truncated);
    }

    let payload = &bytes[SPILL_FILE_HEADER_LEN..payload_end];
    let actual_checksum = match version {
        SPILL_FILE_VERSION_V1 => legacy_v1_checksum(payload),
        SPILL_FILE_VERSION_V2 => crc32(payload),
        other => return Err(SpillFileFrameError::UnsupportedVersion(other)),
    };
    if actual_checksum != expected_checksum {
        return Err(SpillFileFrameError::ChecksumMismatch {
            expected: expected_checksum,
            actual: actual_checksum,
        });
    }

    Ok(payload.to_vec())
}

fn legacy_v1_checksum(data: &[u8]) -> u32 {
    data.iter()
        .fold(0u32, |acc, &byte| acc.wrapping_add(byte as u32))
}

fn crc32(data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(data);
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_round_trip() {
        let data: Vec<u8> = (0u8..=127).collect();
        let frame = encode_spill_file_frame(&data);

        assert_eq!(frame.len(), SPILL_FILE_HEADER_LEN + data.len());
        assert_eq!(decode_spill_file_frame(&frame).unwrap(), data);
    }

    #[test]
    fn reads_legacy_v1_checksum() {
        let data = b"legacy spill";
        let mut frame = Vec::new();
        frame.extend_from_slice(&SPILL_FILE_MAGIC);
        frame.push(SPILL_FILE_VERSION_V1);
        frame.extend_from_slice(&legacy_v1_checksum(data).to_le_bytes());
        frame.extend_from_slice(&(data.len() as u64).to_le_bytes());
        frame.extend_from_slice(data);

        assert_eq!(decode_spill_file_frame(&frame).unwrap(), data);
    }

    #[test]
    fn rejects_single_byte_mutation() {
        let data = b"hello world mutation test data";
        let mut frame = encode_spill_file_frame(data);
        frame[SPILL_FILE_HEADER_LEN] ^= 0xff;

        assert!(matches!(
            decode_spill_file_frame(&frame),
            Err(SpillFileFrameError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn rejects_byte_permutation() {
        let data = b"abcdefghij";
        let mut frame = encode_spill_file_frame(data);
        frame.swap(SPILL_FILE_HEADER_LEN, SPILL_FILE_HEADER_LEN + 1);

        assert!(matches!(
            decode_spill_file_frame(&frame),
            Err(SpillFileFrameError::ChecksumMismatch { .. })
        ));
    }
}
