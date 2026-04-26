//! Encode / decode for v2 frames. Same byte layout as the
//! engine-side codec.

use super::frame::{Flags, Frame, MessageKind, FRAME_HEADER_SIZE, MAX_FRAME_SIZE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    Truncated,
    InvalidLength(u32),
    PayloadTruncated { expected: u32, available: u32 },
    UnknownKind(u8),
    UnknownFlags(u8),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "frame header truncated"),
            Self::InvalidLength(n) => write!(f, "frame length invalid: {n}"),
            Self::PayloadTruncated { expected, available } => {
                write!(f, "payload truncated: {expected} expected, {available} got")
            }
            Self::UnknownKind(b) => write!(f, "unknown message kind 0x{b:02x}"),
            Self::UnknownFlags(b) => write!(f, "unknown flag bits 0x{b:02x}"),
        }
    }
}

impl std::error::Error for FrameError {}

pub fn encode_frame(frame: &Frame) -> Vec<u8> {
    let total = frame.encoded_len() as usize;
    let mut buf = Vec::with_capacity(total);
    buf.extend_from_slice(&frame.encoded_len().to_le_bytes());
    buf.push(frame.kind as u8);
    buf.push(frame.flags.bits());
    buf.extend_from_slice(&frame.stream_id.to_le_bytes());
    buf.extend_from_slice(&frame.correlation_id.to_le_bytes());
    buf.extend_from_slice(&frame.payload);
    buf
}

pub fn decode_frame(bytes: &[u8]) -> Result<(Frame, usize), FrameError> {
    if bytes.len() < FRAME_HEADER_SIZE {
        return Err(FrameError::Truncated);
    }
    let length = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if length < FRAME_HEADER_SIZE as u32 || length > MAX_FRAME_SIZE {
        return Err(FrameError::InvalidLength(length));
    }
    if (bytes.len() as u32) < length {
        return Err(FrameError::PayloadTruncated {
            expected: length,
            available: bytes.len() as u32,
        });
    }
    let kind = MessageKind::from_u8(bytes[4]).ok_or(FrameError::UnknownKind(bytes[4]))?;
    let flag_bits = bytes[5];
    const KNOWN_FLAGS: u8 = 0b0000_0011;
    if flag_bits & !KNOWN_FLAGS != 0 {
        return Err(FrameError::UnknownFlags(flag_bits));
    }
    let flags = Flags::from_bits(flag_bits);
    let stream_id = u16::from_le_bytes([bytes[6], bytes[7]]);
    let correlation_id = u64::from_le_bytes([
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    ]);
    let payload_len = (length as usize) - FRAME_HEADER_SIZE;
    let payload = bytes[FRAME_HEADER_SIZE..FRAME_HEADER_SIZE + payload_len].to_vec();
    Ok((
        Frame {
            kind,
            flags,
            stream_id,
            correlation_id,
            payload,
        },
        length as usize,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_smoke() {
        let f = Frame::new(MessageKind::Query, 7, b"SELECT 1".to_vec());
        let bytes = encode_frame(&f);
        let (decoded, n) = decode_frame(&bytes).unwrap();
        assert_eq!(n, bytes.len());
        assert_eq!(decoded, f);
    }
}
