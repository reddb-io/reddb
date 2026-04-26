//! Hand-rolled binary codec for v2 frames. No serde — the on-wire
//! shape is fixed by ADR 0001, kept simple so a hex-dump is
//! readable.

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
            Self::Truncated => write!(f, "frame header truncated (< 16 bytes)"),
            Self::InvalidLength(n) => write!(f, "frame length field invalid: {n}"),
            Self::PayloadTruncated { expected, available } => write!(
                f,
                "frame payload truncated: expected {expected} bytes, got {available}"
            ),
            Self::UnknownKind(byte) => write!(f, "unknown message kind 0x{byte:02x}"),
            Self::UnknownFlags(byte) => write!(f, "unknown flag bits 0x{byte:02x}"),
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

    fn round_trip(frame: Frame) {
        let bytes = encode_frame(&frame);
        let (decoded, consumed) = decode_frame(&bytes).expect("decode");
        assert_eq!(consumed, bytes.len());
        assert_eq!(decoded, frame);
    }

    #[test]
    fn round_trip_empty_payload() {
        round_trip(Frame::new(MessageKind::Ping, 1, vec![]));
    }

    #[test]
    fn round_trip_with_payload() {
        round_trip(Frame::new(MessageKind::Query, 42, b"SELECT 1".to_vec()));
    }

    #[test]
    fn round_trip_with_stream_and_flags() {
        let frame = Frame::new(MessageKind::Result, 999, vec![0xab; 256])
            .with_stream(7)
            .with_flags(Flags::COMPRESSED | Flags::MORE_FRAMES);
        round_trip(frame);
    }

    #[test]
    fn truncated_header_rejected() {
        assert_eq!(decode_frame(&[]), Err(FrameError::Truncated));
        assert_eq!(decode_frame(&[0; 15]), Err(FrameError::Truncated));
    }

    #[test]
    fn length_below_header_rejected() {
        let mut bytes = vec![0u8; 16];
        bytes[..4].copy_from_slice(&15u32.to_le_bytes());
        assert!(matches!(
            decode_frame(&bytes),
            Err(FrameError::InvalidLength(15))
        ));
    }

    #[test]
    fn unknown_kind_rejected() {
        let mut bytes = vec![0u8; 16];
        bytes[..4].copy_from_slice(&16u32.to_le_bytes());
        bytes[4] = 0xff;
        assert_eq!(decode_frame(&bytes), Err(FrameError::UnknownKind(0xff)));
    }

    #[test]
    fn unknown_flag_bits_rejected() {
        let mut bytes = vec![0u8; 16];
        bytes[..4].copy_from_slice(&16u32.to_le_bytes());
        bytes[4] = MessageKind::Ping as u8;
        bytes[5] = 0b1000_0000;
        assert!(matches!(
            decode_frame(&bytes),
            Err(FrameError::UnknownFlags(_))
        ));
    }

    #[test]
    fn streaming_decode_two_frames_back_to_back() {
        let f1 = Frame::new(MessageKind::Query, 1, b"a".to_vec());
        let f2 = Frame::new(MessageKind::Query, 2, b"b".to_vec());
        let mut buf = encode_frame(&f1);
        buf.extend(encode_frame(&f2));
        let (got1, n1) = decode_frame(&buf).unwrap();
        let (got2, _n2) = decode_frame(&buf[n1..]).unwrap();
        assert_eq!(got1, f1);
        assert_eq!(got2, f2);
    }
}
