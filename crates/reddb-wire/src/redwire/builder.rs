//! Server-side frame construction discipline.
//!
//! Every server-emitted frame today is built by stitching together
//! [`Frame::new`] + [`Frame::with_stream`] + [`Frame::with_flags`] at
//! the call site. That spreads four invariants across every dispatch
//! path:
//!
//!   1. Correlation-id propagation — each response must echo the
//!      request frame's id (or `0` for unsolicited frames).
//!   2. `MORE_FRAMES` sequencing — only the *last* frame of a multi-
//!      frame reply may clear the flag.
//!   3. `MAX_FRAME_SIZE` enforcement — the codec checks on decode but
//!      a producer happily encodes oversized frames the peer will
//!      reject anyway.
//!   4. Compression policy — callers either opt in by setting
//!      `Flags::COMPRESSED` or do not, and the codec silently falls
//!      back to plaintext on incompressible input.
//!
//! [`FrameBuilder`] owns those invariants. The acceptance test for
//! this module is the deletion test: deleting `builder.rs` forces
//! frame-construction discipline back to inline `Frame::new` calls
//! at every dispatch site.
//!
//! ```ignore
//! use reddb_wire::redwire::{FrameBuilder, MessageKind};
//!
//! let frame = FrameBuilder::reply_to(request.correlation_id)
//!     .kind(MessageKind::Result)
//!     .payload(body)
//!     .stream_id(42)
//!     .more_frames(false)
//!     .build()?;
//! ```
//!
//! The builder is engine-free — it only depends on [`Frame`],
//! [`MessageKind`], [`Flags`], and the size constants from this
//! crate. Server dispatch (auth, session, listener) constructs
//! frames through the builder; the codec stays focused on bytes.

use super::frame::{Flags, Frame, MessageKind, FRAME_HEADER_SIZE, MAX_FRAME_SIZE};

/// Errors surfaced at `build()` time so they fail at construction
/// rather than at encode time, where the call site has already lost
/// the context to recover.
///
/// Distinct from [`crate::redwire::codec::FrameError`], which covers
/// decode-side framing errors. The split keeps producer-side
/// invariants (this) separate from consumer-side parsing (that).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// `kind()` was never called — a frame has no default kind so
    /// the builder refuses to guess.
    KindMissing,
    /// The encoded frame would exceed [`MAX_FRAME_SIZE`].
    PayloadTooLarge { encoded_len: usize, max: u32 },
    /// Catalog cross-check failed: the requested flag set isn't in
    /// `MessageKind::allowed_flags()` for the chosen kind.
    FlagsNotAllowedForKind { kind: MessageKind, flags: u8 },
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KindMissing => write!(f, "FrameBuilder: kind() must be called before build()"),
            Self::PayloadTooLarge { encoded_len, max } => write!(
                f,
                "FrameBuilder: encoded frame size {encoded_len} exceeds MAX_FRAME_SIZE ({max})"
            ),
            Self::FlagsNotAllowedForKind { kind, flags } => write!(
                f,
                "FrameBuilder: flag bits 0x{flags:02x} not allowed on kind {kind:?}"
            ),
        }
    }
}

impl std::error::Error for BuildError {}

/// Compression intent recorded by `compress(true|false)`. The
/// builder defers the actual zstd call to the codec but tracks
/// whether the caller asked for compression so it can drop the
/// `COMPRESSED` flag if the payload is provably incompressible
/// (see [`FrameBuilder::build`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compress {
    No,
    Yes,
}

/// Builder for RedWire [`Frame`]s.
///
/// Construct with [`FrameBuilder::request`] for client requests,
/// [`FrameBuilder::reply_to`] for response frames (carries the
/// request's correlation id), or [`FrameBuilder::unsolicited`] for
/// server-initiated frames (correlation id `0`). All other fields are
/// optional.
#[derive(Debug, Clone)]
pub struct FrameBuilder {
    kind: Option<MessageKind>,
    correlation_id: u64,
    stream_id: u16,
    payload: Vec<u8>,
    flags: Flags,
    compress: Compress,
    /// `true` when the caller has *not* yet declared this is the
    /// last frame of a multi-frame reply — i.e. `MORE_FRAMES` is
    /// set. Defaults to `false` (single-frame reply).
    more_frames: bool,
}

impl FrameBuilder {
    /// Client request frame. Carries the caller's new correlation id
    /// so the server can echo it in its reply.
    pub fn request(correlation_id: u64) -> Self {
        Self::with_correlation(correlation_id)
    }

    /// Reply to a request frame. Echoes the caller's correlation id
    /// so the client can pair the response with the request.
    pub fn reply_to(correlation_id: u64) -> Self {
        Self::with_correlation(correlation_id)
    }

    /// Server-initiated frame with no request to echo (correlation
    /// id `0`). Used for notices, unsolicited Bye, etc.
    pub fn unsolicited() -> Self {
        Self::with_correlation(0)
    }

    fn with_correlation(correlation_id: u64) -> Self {
        Self {
            kind: None,
            correlation_id,
            stream_id: 0,
            payload: Vec::new(),
            flags: Flags::empty(),
            compress: Compress::No,
            more_frames: false,
        }
    }

    pub fn kind(mut self, kind: MessageKind) -> Self {
        self.kind = Some(kind);
        self
    }

    pub fn payload(mut self, payload: Vec<u8>) -> Self {
        self.payload = payload;
        self
    }

    pub fn stream_id(mut self, stream_id: u16) -> Self {
        self.stream_id = stream_id;
        self
    }

    /// Replace the flag set wholesale. Most callers should prefer
    /// [`Self::more_frames`] / [`Self::compress`] over poking flags
    /// directly — this exists for the Cancel / Compress / Notice
    /// control frames that carry caller-defined bits.
    pub fn flags(mut self, flags: Flags) -> Self {
        self.flags = flags;
        self
    }

    /// Mark this frame as part of a multi-frame reply. Pass `false`
    /// (the default) on the *last* frame of the burst — the
    /// `MORE_FRAMES` last-frame invariant is enforced at build()
    /// time by the flag bit.
    pub fn more_frames(mut self, more: bool) -> Self {
        self.more_frames = more;
        self
    }

    /// Request that the encoder zstd-compress the payload. The
    /// codec falls back to plaintext + cleared flag if the payload
    /// is incompressible (see [`Self::build`]).
    pub fn compress(mut self, yes: bool) -> Self {
        self.compress = if yes { Compress::Yes } else { Compress::No };
        self
    }

    /// Finalize the frame.
    ///
    /// Enforces:
    ///   * `kind()` was set (otherwise [`BuildError::KindMissing`]).
    ///   * Plaintext encoded size <= [`MAX_FRAME_SIZE`] (otherwise
    ///     [`BuildError::PayloadTooLarge`]) — checked against the
    ///     plaintext payload, since the wire form after compression
    ///     can only shrink.
    ///   * `MORE_FRAMES` flag mirrors the `more_frames(bool)` call.
    ///   * `COMPRESSED` flag is set only when compression was
    ///     requested *and* the payload looks compressible. A trivial
    ///     incompressibility heuristic ("the payload is empty or
    ///     too short for zstd to reduce") drops the flag here so
    ///     the encoded bytes match the flag.
    pub fn build(self) -> Result<Frame, BuildError> {
        let kind = self.kind.ok_or(BuildError::KindMissing)?;
        let encoded_len = FRAME_HEADER_SIZE + self.payload.len();
        if encoded_len > MAX_FRAME_SIZE as usize {
            return Err(BuildError::PayloadTooLarge {
                encoded_len,
                max: MAX_FRAME_SIZE,
            });
        }

        if !kind.permits_flags(self.flags) {
            return Err(BuildError::FlagsNotAllowedForKind {
                kind,
                flags: self.flags.bits(),
            });
        }

        let mut flags = self.flags;
        if self.more_frames {
            flags = flags.insert(Flags::MORE_FRAMES);
        } else {
            // Clear MORE_FRAMES on the last frame of a burst. Stays
            // a no-op when the caller never set it.
            flags = Flags::from_bits(flags.bits() & !Flags::MORE_FRAMES.bits());
        }

        let compressed = match self.compress {
            Compress::No => false,
            Compress::Yes => is_payload_compressible(&self.payload),
        };
        if compressed {
            flags = flags.insert(Flags::COMPRESSED);
        } else {
            flags = Flags::from_bits(flags.bits() & !Flags::COMPRESSED.bits());
        }

        // Catalog cross-check: the chosen kind's allowed_flags() is
        // the producer-side mirror of the codec's decode-time check.
        // Catches misframed builds (e.g. COMPRESSED on Hello) before
        // they reach the wire.
        if !kind.permits_flags(flags) {
            return Err(BuildError::FlagsNotAllowedForKind {
                kind,
                flags: flags.bits(),
            });
        }

        Ok(Frame {
            kind,
            flags,
            stream_id: self.stream_id,
            correlation_id: self.correlation_id,
            payload: self.payload,
        })
    }
}

pub fn build_reply_frame(
    correlation_id: u64,
    kind: MessageKind,
    payload: Vec<u8>,
) -> Result<Frame, BuildError> {
    FrameBuilder::reply_to(correlation_id)
        .kind(kind)
        .payload(payload)
        .build()
}

pub fn build_error_frame(correlation_id: u64, message: &str) -> Result<Frame, BuildError> {
    build_reply_frame(
        correlation_id,
        MessageKind::Error,
        message.as_bytes().to_vec(),
    )
}

pub fn build_error_frame_lossy(correlation_id: u64, message: &str) -> Frame {
    build_error_frame(correlation_id, message).unwrap_or_else(|_| {
        Frame::new(
            MessageKind::Error,
            correlation_id,
            b"redwire error frame too large".to_vec(),
        )
    })
}

pub fn build_dispatch_reply_frame(
    correlation_id: u64,
    kind: MessageKind,
    payload: Vec<u8>,
) -> Frame {
    build_reply_frame(correlation_id, kind, payload)
        .unwrap_or_else(|err| build_error_frame_lossy(correlation_id, &err.to_string()))
}

/// Adapt an older 5-byte handler envelope into a RedWire reply frame.
///
/// Some in-process fast paths return `[u32 length][u8 kind][body...]`.
/// RedWire uses the same message-kind discriminator for `kind`, but owns the
/// outer frame header. This helper keeps that compatibility bridge in the wire
/// crate instead of letting runtime code parse kind bytes directly.
pub fn rewrap_length_prefixed_handler_response(raw_bytes: &[u8], correlation_id: u64) -> Frame {
    if raw_bytes.len() < 5 {
        return build_error_frame_lossy(
            correlation_id,
            "fast-path handler returned a truncated frame",
        );
    }
    let kind = MessageKind::from_u8(raw_bytes[4]).unwrap_or(MessageKind::Error);
    build_dispatch_reply_frame(correlation_id, kind, raw_bytes[5..].to_vec())
}

pub fn build_query_frame(correlation_id: u64, sql: &str) -> Result<Frame, BuildError> {
    build_request_frame(correlation_id, MessageKind::Query, sql.as_bytes().to_vec())
}

pub fn build_query_with_params_frame(
    correlation_id: u64,
    payload: Vec<u8>,
) -> Result<Frame, BuildError> {
    build_request_frame(correlation_id, MessageKind::QueryWithParams, payload)
}

pub fn build_bulk_insert_frame(correlation_id: u64, payload: Vec<u8>) -> Result<Frame, BuildError> {
    build_request_frame(correlation_id, MessageKind::BulkInsert, payload)
}

pub fn build_get_frame(correlation_id: u64, payload: Vec<u8>) -> Result<Frame, BuildError> {
    build_request_frame(correlation_id, MessageKind::Get, payload)
}

pub fn build_delete_frame(correlation_id: u64, payload: Vec<u8>) -> Result<Frame, BuildError> {
    build_request_frame(correlation_id, MessageKind::Delete, payload)
}

pub fn build_bulk_insert_binary_frame(
    correlation_id: u64,
    payload: Vec<u8>,
) -> Result<Frame, BuildError> {
    build_request_frame(correlation_id, MessageKind::BulkInsertBinary, payload)
}

pub fn build_ping_frame(correlation_id: u64) -> Result<Frame, BuildError> {
    build_request_frame(correlation_id, MessageKind::Ping, Vec::new())
}

pub fn build_bye_frame(correlation_id: u64) -> Result<Frame, BuildError> {
    build_request_frame(correlation_id, MessageKind::Bye, Vec::new())
}

pub fn build_request_frame(
    correlation_id: u64,
    kind: MessageKind,
    payload: Vec<u8>,
) -> Result<Frame, BuildError> {
    FrameBuilder::request(correlation_id)
        .kind(kind)
        .payload(payload)
        .build()
}

/// Cheap heuristic: zstd's frame header is ~12 bytes, so a payload
/// has to clear that bar to even potentially shrink. The codec also
/// catches truly pathological cases at encode time and falls back to
/// plaintext, but this lets the builder report the cleared flag
/// before encoding.
fn is_payload_compressible(payload: &[u8]) -> bool {
    payload.len() > 32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_to_propagates_correlation_id() {
        // Mirrors the dispatch site: every response frame must echo
        // the request's correlation id so the client pairs them up.
        let frame = FrameBuilder::reply_to(0xABCD)
            .kind(MessageKind::Result)
            .payload(b"ok".to_vec())
            .build()
            .expect("build");
        assert_eq!(frame.correlation_id, 0xABCD);
        assert_eq!(frame.kind, MessageKind::Result);
        assert_eq!(frame.payload, b"ok");
    }

    #[test]
    fn request_builders_choose_client_message_kinds() {
        let query = build_query_frame(1, "select 1").expect("query");
        assert_eq!(query.kind, MessageKind::Query);
        assert_eq!(query.correlation_id, 1);
        assert_eq!(query.payload, b"select 1");

        let ping = build_ping_frame(2).expect("ping");
        assert_eq!(ping.kind, MessageKind::Ping);
        assert!(ping.payload.is_empty());

        let bye = build_bye_frame(3).expect("bye");
        assert_eq!(bye.kind, MessageKind::Bye);
        assert!(bye.payload.is_empty());
    }

    #[test]
    fn unsolicited_uses_zero_correlation() {
        let frame = FrameBuilder::unsolicited()
            .kind(MessageKind::Notice)
            .payload(b"server-side notice".to_vec())
            .build()
            .expect("build");
        assert_eq!(frame.correlation_id, 0);
    }

    #[test]
    fn missing_kind_rejected() {
        let err = FrameBuilder::reply_to(1).build().unwrap_err();
        assert_eq!(err, BuildError::KindMissing);
    }

    #[test]
    fn more_frames_last_frame_clears_the_flag() {
        // The MORE_FRAMES last-frame invariant: only intermediate
        // frames in a burst carry the flag. The last frame must
        // clear it, otherwise the client keeps waiting for more.
        let middle = FrameBuilder::reply_to(7)
            .kind(MessageKind::Result)
            .payload(vec![0; 8])
            .more_frames(true)
            .build()
            .expect("build middle");
        assert!(
            middle.flags.contains(Flags::MORE_FRAMES),
            "middle frame must carry MORE_FRAMES"
        );

        let last = FrameBuilder::reply_to(7)
            .kind(MessageKind::Result)
            .payload(vec![0; 8])
            .more_frames(false)
            .build()
            .expect("build last");
        assert!(
            !last.flags.contains(Flags::MORE_FRAMES),
            "last frame must clear MORE_FRAMES"
        );
    }

    #[test]
    fn more_frames_default_is_last_frame() {
        // A single-frame reply (the common case) is implicitly the
        // last frame — callers shouldn't have to remember to clear
        // the flag.
        let frame = FrameBuilder::reply_to(1)
            .kind(MessageKind::Pong)
            .build()
            .expect("build");
        assert!(!frame.flags.contains(Flags::MORE_FRAMES));
    }

    #[test]
    fn payload_at_max_size_accepted() {
        let payload = vec![0u8; (MAX_FRAME_SIZE as usize) - FRAME_HEADER_SIZE];
        let frame = FrameBuilder::reply_to(1)
            .kind(MessageKind::Result)
            .payload(payload)
            .build()
            .expect("build at limit");
        assert_eq!(frame.encoded_len(), MAX_FRAME_SIZE);
    }

    #[test]
    fn payload_over_max_size_rejected() {
        // MAX_FRAME_SIZE is the on-wire upper bound. The builder
        // refuses oversize plaintext rather than letting the encoder
        // produce a frame the peer will reject anyway.
        let oversize = (MAX_FRAME_SIZE as usize) - FRAME_HEADER_SIZE + 1;
        let payload = vec![0u8; oversize];
        let err = FrameBuilder::reply_to(1)
            .kind(MessageKind::Result)
            .payload(payload)
            .build()
            .unwrap_err();
        match err {
            BuildError::PayloadTooLarge { encoded_len, max } => {
                assert_eq!(max, MAX_FRAME_SIZE);
                assert_eq!(encoded_len, MAX_FRAME_SIZE as usize + 1);
            }
            other => panic!("expected PayloadTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn compression_fallback_drops_flag_for_incompressible_payload() {
        // Compression was requested, but the payload is too short
        // for zstd to actually reduce. The builder drops the flag
        // so the encoded bytes match — otherwise the wire form
        // claims COMPRESSED but the body is plaintext.
        let frame = FrameBuilder::reply_to(1)
            .kind(MessageKind::Result)
            .payload(b"tiny".to_vec())
            .compress(true)
            .build()
            .expect("build");
        assert!(
            !frame.flags.contains(Flags::COMPRESSED),
            "incompressible payload must not carry COMPRESSED"
        );
    }

    #[test]
    fn compression_kept_for_compressible_payload() {
        let payload = b"abcabcabc".repeat(16);
        let frame = FrameBuilder::reply_to(1)
            .kind(MessageKind::Result)
            .payload(payload)
            .compress(true)
            .build()
            .expect("build");
        assert!(frame.flags.contains(Flags::COMPRESSED));
    }

    #[test]
    fn flags_not_allowed_for_kind_rejected_at_build() {
        // Hello is a handshake kind — the catalog forbids COMPRESSED.
        // The builder mirrors the codec's decode-time check so a
        // misframed build fails before the bytes ever hit the wire.
        let err = FrameBuilder::reply_to(1)
            .kind(MessageKind::Hello)
            .flags(Flags::COMPRESSED)
            .build()
            .unwrap_err();
        match err {
            BuildError::FlagsNotAllowedForKind { kind, flags } => {
                assert_eq!(kind, MessageKind::Hello);
                assert_eq!(flags, Flags::COMPRESSED.bits());
            }
            other => panic!("expected FlagsNotAllowedForKind, got {other:?}"),
        }
    }

    #[test]
    fn stream_id_propagates() {
        let frame = FrameBuilder::reply_to(1)
            .kind(MessageKind::Result)
            .stream_id(0xBEEF)
            .build()
            .expect("build");
        assert_eq!(frame.stream_id, 0xBEEF);
    }

    #[test]
    fn generic_reply_builders_pin_server_frame_contracts() {
        let reply = build_reply_frame(7, MessageKind::Pong, b"ok".to_vec()).expect("reply frame");
        assert_eq!(reply.correlation_id, 7);
        assert_eq!(reply.kind, MessageKind::Pong);
        assert_eq!(reply.payload, b"ok");

        let err = build_error_frame(8, "bad request").expect("error frame");
        assert_eq!(err.kind, MessageKind::Error);
        assert_eq!(err.correlation_id, 8);
        assert_eq!(err.payload, b"bad request");

        let dispatch = build_dispatch_reply_frame(9, MessageKind::Result, b"rows".to_vec());
        assert_eq!(dispatch.kind, MessageKind::Result);
        assert_eq!(dispatch.correlation_id, 9);
    }

    #[test]
    fn rewraps_length_prefixed_handler_response() {
        let raw = [4u8, 0, 0, 0, MessageKind::BulkOk as u8, b'o', b'k'];
        let frame = rewrap_length_prefixed_handler_response(&raw, 12);
        assert_eq!(frame.correlation_id, 12);
        assert_eq!(frame.kind, MessageKind::BulkOk);
        assert_eq!(frame.payload, b"ok");

        let truncated = rewrap_length_prefixed_handler_response(&raw[..4], 13);
        assert_eq!(truncated.kind, MessageKind::Error);
        assert_eq!(truncated.correlation_id, 13);
        assert_eq!(
            truncated.payload,
            b"fast-path handler returned a truncated frame"
        );
    }

    #[test]
    fn lossy_error_builder_never_panics_on_oversized_payload() {
        let too_large = "x".repeat(MAX_FRAME_SIZE as usize);
        let err = build_error_frame_lossy(11, &too_large);
        assert_eq!(err.kind, MessageKind::Error);
        assert_eq!(err.correlation_id, 11);
        assert_eq!(err.payload, b"redwire error frame too large");
    }
}
