//! Hand-rolled binary codec for v2 frames. No serde — the on-wire
//! shape is fixed by ADR 0001, kept simple so a hex-dump is
//! readable.

use super::frame::{Flags, Frame, MessageKind, FRAME_HEADER_SIZE, MAX_FRAME_SIZE};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    Truncated,
    InvalidLength(u32),
    PayloadTruncated {
        expected: u32,
        available: u32,
    },
    UnknownKind(u8),
    UnknownFlags(u8),
    /// Catalog cross-check failed: the flag bits set on the frame are
    /// not in `MessageKind::allowed_flags()` for this kind. The wire
    /// catalog is the single source of truth for which bits a kind
    /// may carry — see `frame.rs::MessageKind::allowed_flags`.
    FlagsNotAllowedForKind {
        kind: u8,
        flags: u8,
    },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "frame header truncated (< 16 bytes)"),
            Self::InvalidLength(n) => write!(f, "frame length field invalid: {n}"),
            Self::PayloadTruncated {
                expected,
                available,
            } => write!(
                f,
                "frame payload truncated: expected {expected} bytes, got {available}"
            ),
            Self::UnknownKind(byte) => write!(f, "unknown message kind 0x{byte:02x}"),
            Self::UnknownFlags(byte) => write!(f, "unknown flag bits 0x{byte:02x}"),
            Self::FlagsNotAllowedForKind { kind, flags } => write!(
                f,
                "flag bits 0x{flags:02x} not allowed on kind 0x{kind:02x}"
            ),
        }
    }
}

impl std::error::Error for FrameError {}

pub fn frame_len_from_header(header: &[u8; FRAME_HEADER_SIZE]) -> Result<usize, FrameError> {
    let length = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    if length < FRAME_HEADER_SIZE as u32 || length > MAX_FRAME_SIZE {
        return Err(FrameError::InvalidLength(length));
    }
    Ok(length as usize)
}

pub fn encode_frame(frame: &Frame) -> Vec<u8> {
    // The frame's `payload` is always the plaintext form. If the
    // COMPRESSED flag is set we compress on the wire and rewrite
    // the length header to match the compressed size — the
    // receiver inflates before delivering to the dispatch loop.
    if frame.flags.contains(Flags::COMPRESSED) {
        return encode_compressed(frame);
    }
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

fn encode_compressed(frame: &Frame) -> Vec<u8> {
    // zstd level 1 — keeps CPU low while still cutting JSON +
    // BulkInsertBinary by 60-80%. Operators that want max ratio
    // can flip to level 3+ via `RED_REDWIRE_ZSTD_LEVEL` env.
    let level = std::env::var("RED_REDWIRE_ZSTD_LEVEL")
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(1);
    let compressed = match zstd::stream::encode_all(frame.payload.as_slice(), level) {
        Ok(buf) => buf,
        Err(_) => {
            // Fallback: drop the COMPRESSED flag and ship plaintext.
            // Compression failures are rare (level 1 effectively
            // never fails on bytes), but the fallback is safer
            // than panicking inside the framing layer.
            let mut clone = frame.clone();
            clone.flags = Flags::from_bits(clone.flags.bits() & !Flags::COMPRESSED.bits());
            return encode_frame(&clone);
        }
    };
    let total = (FRAME_HEADER_SIZE + compressed.len()) as u32;
    let mut buf = Vec::with_capacity(total as usize);
    buf.extend_from_slice(&total.to_le_bytes());
    buf.push(frame.kind as u8);
    buf.push(frame.flags.bits());
    buf.extend_from_slice(&frame.stream_id.to_le_bytes());
    buf.extend_from_slice(&frame.correlation_id.to_le_bytes());
    buf.extend_from_slice(&compressed);
    buf
}

pub fn decode_frame(bytes: &[u8]) -> Result<(Frame, usize), FrameError> {
    if bytes.len() < FRAME_HEADER_SIZE {
        return Err(FrameError::Truncated);
    }
    let mut header = [0u8; FRAME_HEADER_SIZE];
    header.copy_from_slice(&bytes[..FRAME_HEADER_SIZE]);
    let length = frame_len_from_header(&header)? as u32;
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
    // Catalog cross-check: the kind's `allowed_flags()` is the single
    // source of truth. Reject combinations the catalog forbids
    // (e.g. COMPRESSED on tiny handshake payloads) so misframed
    // frames fail at the boundary instead of reaching dispatch.
    if !kind.permits_flags(flags) {
        return Err(FrameError::FlagsNotAllowedForKind {
            kind: bytes[4],
            flags: flag_bits,
        });
    }
    let stream_id = u16::from_le_bytes([bytes[6], bytes[7]]);
    let correlation_id = u64::from_le_bytes([
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    ]);
    let payload_len = (length as usize) - FRAME_HEADER_SIZE;
    let on_wire = &bytes[FRAME_HEADER_SIZE..FRAME_HEADER_SIZE + payload_len];
    let payload = if flags.contains(Flags::COMPRESSED) {
        // Decompress on read so the rest of the dispatch loop
        // sees plaintext bytes regardless of how they arrived.
        match zstd::stream::decode_all(on_wire) {
            Ok(plain) => plain,
            Err(e) => {
                return Err(FrameError::PayloadTruncated {
                    // Reuse PayloadTruncated for "decompression
                    // failed" rather than introduce a new variant
                    // — the wire-layer outcome is the same: the
                    // body is unparseable, drop the connection.
                    expected: payload_len as u32,
                    available: e.to_string().len() as u32,
                });
            }
        }
    } else {
        on_wire.to_vec()
    };
    Ok((
        Frame {
            kind,
            // The flag stays on the decoded frame so dispatch can
            // see it was compressed if it cares (audit, metrics).
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
    fn frame_len_from_header_validates_bounds() {
        let mut header = [0u8; FRAME_HEADER_SIZE];
        header[..4].copy_from_slice(&(FRAME_HEADER_SIZE as u32).to_le_bytes());
        assert_eq!(frame_len_from_header(&header).unwrap(), FRAME_HEADER_SIZE);

        header[..4].copy_from_slice(&15u32.to_le_bytes());
        assert_eq!(
            frame_len_from_header(&header),
            Err(FrameError::InvalidLength(15))
        );

        header[..4].copy_from_slice(&(MAX_FRAME_SIZE + 1).to_le_bytes());
        assert_eq!(
            frame_len_from_header(&header),
            Err(FrameError::InvalidLength(MAX_FRAME_SIZE + 1))
        );
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
    fn flags_not_allowed_for_kind_rejected() {
        // Ping is a handshake kind — the catalog forbids COMPRESSED
        // on tiny handshake payloads. A frame with kind=Ping and the
        // COMPRESSED bit set must be rejected at the boundary.
        let mut bytes = vec![0u8; 16];
        bytes[..4].copy_from_slice(&16u32.to_le_bytes());
        bytes[4] = MessageKind::Ping as u8;
        bytes[5] = Flags::COMPRESSED.bits();
        match decode_frame(&bytes) {
            Err(FrameError::FlagsNotAllowedForKind { kind, flags }) => {
                assert_eq!(kind, MessageKind::Ping as u8);
                assert_eq!(flags, Flags::COMPRESSED.bits());
            }
            other => panic!("expected FlagsNotAllowedForKind, got {other:?}"),
        }
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

    #[test]
    fn compressed_round_trip_recovers_plaintext() {
        // A compressible payload — a kilobyte of repeating text.
        let payload = b"abcabcabcabc".repeat(100);
        let frame =
            Frame::new(MessageKind::Result, 7, payload.clone()).with_flags(Flags::COMPRESSED);
        let bytes = encode_frame(&frame);
        // Wire form should be smaller than the plaintext frame.
        assert!(
            bytes.len() < FRAME_HEADER_SIZE + payload.len(),
            "compressed frame ({}) must be smaller than plaintext payload ({})",
            bytes.len(),
            payload.len(),
        );
        let (decoded, _) = decode_frame(&bytes).expect("decode compressed");
        assert_eq!(decoded.payload, payload);
        assert!(decoded.flags.contains(Flags::COMPRESSED));
    }

    #[test]
    fn output_stream_lifecycle_envelopes_round_trip() {
        // Golden encode/decode for every new variant added in
        // issue #762 / PRD #759 S3. Pins the exact byte values
        // and confirms `stream_id` multiplex routing survives the
        // round trip (it is what `StreamCancel`'s per-stream
        // targeting relies on).
        let open = Frame::new(
            MessageKind::OpenStream,
            10,
            br#"{"sql":"SELECT 1","opts":{}}"#.to_vec(),
        )
        .with_stream(7);
        round_trip(open.clone());
        assert_eq!(encode_frame(&open)[4], 0x29);

        let ack = Frame::new(
            MessageKind::OpenAck,
            10,
            br#"{"lease_handle":"42","resumable":false,"snapshot_lsn":1234}"#.to_vec(),
        )
        .with_stream(7);
        round_trip(ack.clone());
        assert_eq!(encode_frame(&ack)[4], 0x2A);

        let chunk = Frame::new(
            MessageKind::StreamChunk,
            10,
            br#"{"seq":0,"rows":[{"a":1}],"terminal":false}"#.to_vec(),
        )
        .with_stream(7);
        round_trip(chunk.clone());
        assert_eq!(encode_frame(&chunk)[4], 0x2B);

        let serr = Frame::new(
            MessageKind::StreamError,
            10,
            br#"{"code":"unknown_stream","message":"x"}"#.to_vec(),
        )
        .with_stream(7);
        round_trip(serr.clone());
        assert_eq!(encode_frame(&serr)[4], 0x2C);

        let end = Frame::new(
            MessageKind::StreamEnd,
            10,
            br#"{"stats":{"row_count":1}}"#.to_vec(),
        )
        .with_stream(7);
        round_trip(end.clone());
        assert_eq!(encode_frame(&end)[4], 0x25);

        let cancel = Frame::new(
            MessageKind::StreamCancel,
            10,
            br#"{"reason":"client-abort"}"#.to_vec(),
        )
        .with_stream(7);
        round_trip(cancel.clone());
        assert_eq!(encode_frame(&cancel)[4], 0x2D);
    }

    #[test]
    fn input_stream_envelopes_round_trip() {
        // Golden encode/decode for the input-direction envelopes
        // added in issue #764 / PRD #759 S5. The envelope *vocabulary*
        // is reused from S3 — only the payload shapes and the
        // direction of `StreamChunk` differ — so the byte values are
        // pinned to the same kinds (no new MessageKind bytes).

        // OpenStream with `direction:"in"` + target/columns instead of
        // a `sql` field. Still kind 0x29, still multiplex via stream_id.
        let open_in = Frame::new(
            MessageKind::OpenStream,
            20,
            br#"{"direction":"in","target":"t","columns":["id","name"]}"#.to_vec(),
        )
        .with_stream(5);
        round_trip(open_in.clone());
        assert_eq!(encode_frame(&open_in)[4], 0x29);

        // Client-originated chunk of rows on the input stream. Same
        // 0x2B kind the server uses on output streams; the rows are
        // JSON objects keyed by column.
        let chunk_in = Frame::new(
            MessageKind::StreamChunk,
            20,
            br#"{"seq":0,"rows":[{"id":1,"name":"a"}],"terminal":false}"#.to_vec(),
        )
        .with_stream(5);
        round_trip(chunk_in.clone());
        assert_eq!(encode_frame(&chunk_in)[4], 0x2B);

        // Terminal chunk closes the input stream.
        let chunk_terminal = Frame::new(
            MessageKind::StreamChunk,
            20,
            br#"{"seq":2,"rows":[],"terminal":true}"#.to_vec(),
        )
        .with_stream(5);
        round_trip(chunk_terminal.clone());
        assert_eq!(encode_frame(&chunk_terminal)[4], 0x2B);

        // Server StreamEnd carries the committed RID range + stats.
        let end = Frame::new(
            MessageKind::StreamEnd,
            20,
            br#"{"stats":{"row_count":3,"chunk_count":2,"committed_rid":42,"snapshot_lsn":40,"cancelled":false}}"#.to_vec(),
        )
        .with_stream(5);
        round_trip(end.clone());
        assert_eq!(encode_frame(&end)[4], 0x25);

        // Server StreamError carries the recoverable_rid prefix.
        let serr = Frame::new(
            MessageKind::StreamError,
            20,
            br#"{"code":"invalid_row","message":"x","chunk_seq":1,"recoverable_rid":41}"#.to_vec(),
        )
        .with_stream(5);
        round_trip(serr.clone());
        assert_eq!(encode_frame(&serr)[4], 0x2C);
    }

    #[test]
    fn queue_wait_envelopes_round_trip() {
        // Golden encode/decode for the live queue-wait envelopes added
        // in issue #917 / PRD #915. Pins the new byte values and
        // confirms the request/push pair round-trips equal through the
        // codec, multiplexed over `stream_id` like the other streamed
        // envelopes.
        let open = Frame::new(
            MessageKind::QueueWaitOpen,
            10,
            br#"{"queue":"jobs","consumer":"w1","count":1,"wait_ms":5000}"#.to_vec(),
        )
        .with_stream(3);
        round_trip(open.clone());
        assert_eq!(encode_frame(&open)[4], 0x2E);

        let push = Frame::new(
            MessageKind::QueueEventPush,
            10,
            br#"{"message_id":"42","payload":{"hello":"world"},"consumer":"w1","delivery_count":1}"#
                .to_vec(),
        )
        .with_stream(3);
        round_trip(push.clone());
        assert_eq!(encode_frame(&push)[4], 0x2F);
    }

    #[test]
    fn uncompressed_frame_decodes_unchanged_when_flag_unset() {
        let payload = b"hello world".to_vec();
        let frame = Frame::new(MessageKind::Result, 1, payload.clone());
        let bytes = encode_frame(&frame);
        let (decoded, _) = decode_frame(&bytes).unwrap();
        assert_eq!(decoded.payload, payload);
        assert!(!decoded.flags.contains(Flags::COMPRESSED));
    }
}
