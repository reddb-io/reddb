//! RedWire frame layout — 16-byte header + payload, little-endian.
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────┐
//! │ Header (16 bytes)                                         │
//! │   u32   length         total frame size, incl. header     │
//! │   u8    kind           MessageKind                         │
//! │   u8    flags          COMPRESSED | MORE_FRAMES | …        │
//! │   u16   stream_id      0 = unsolicited; otherwise multiplex│
//! │   u64   correlation_id request↔response pairing           │
//! ├──────────────────────────────────────────────────────────┤
//! │ Payload (length - 16 bytes)                               │
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! Data-plane kinds live at 0x01..0x0F; handshake / lifecycle at
//! 0x10..0x1F; control plane at 0x20..0x3F.

pub const FRAME_HEADER_SIZE: usize = 16;
pub const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: MessageKind,
    pub flags: Flags,
    pub stream_id: u16,
    pub correlation_id: u64,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(kind: MessageKind, correlation_id: u64, payload: Vec<u8>) -> Self {
        Self {
            kind,
            flags: Flags::empty(),
            stream_id: 0,
            correlation_id,
            payload,
        }
    }

    pub fn with_stream(mut self, stream_id: u16) -> Self {
        self.stream_id = stream_id;
        self
    }

    pub fn with_flags(mut self, flags: Flags) -> Self {
        self.flags = flags;
        self
    }

    pub fn encoded_len(&self) -> u32 {
        (FRAME_HEADER_SIZE + self.payload.len()) as u32
    }
}

/// Single-byte message-kind discriminator. Numeric values are
/// part of the wire spec — never repurpose a value once shipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageKind {
    // Data-plane codes.
    Query = 0x01,
    Result = 0x02,
    Error = 0x03,
    BulkInsert = 0x04,
    BulkOk = 0x05,
    BulkInsertBinary = 0x06,
    QueryBinary = 0x07,
    BulkInsertPrevalidated = 0x08,
    BulkStreamStart = 0x09,
    BulkStreamRows = 0x0A,
    BulkStreamCommit = 0x0B,
    BulkStreamAck = 0x0C,
    Prepare = 0x0D,
    PreparedOk = 0x0E,
    ExecutePrepared = 0x0F,

    // Handshake / lifecycle.
    Hello = 0x10,
    HelloAck = 0x11,
    AuthRequest = 0x12,
    AuthResponse = 0x13,
    AuthOk = 0x14,
    AuthFail = 0x15,
    Bye = 0x16,
    Ping = 0x17,
    Pong = 0x18,
    Get = 0x19,
    Delete = 0x1A,
    DeleteOk = 0x1B,

    // Control plane.
    Cancel = 0x20,
    Compress = 0x21,
    SetSession = 0x22,
    Notice = 0x23,

    // Streamed responses.
    RowDescription = 0x24,
    StreamEnd = 0x25,

    // RedDB-native data plane.
    VectorSearch = 0x26,
    GraphTraverse = 0x27,
    QueryWithParams = 0x28,
}

/// Coarse routing class for a `MessageKind`.
///
/// The numeric ranges in the wire spec (0x01..0x0F data plane,
/// 0x10..0x1F handshake/lifecycle, 0x20..0x3F control plane) are
/// turned into a typed catalog so dispatch sites can interrogate
/// a kind's role without re-implementing the comment-grouped match
/// arms. `Streamed` is split out from `DataPlane` for kinds that
/// describe an in-flight stream envelope rather than a request/reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageClass {
    DataPlane,
    Handshake,
    ControlPlane,
    Streamed,
}

/// Who is allowed to put this kind on the wire.
///
/// The handshake and lifecycle frames split cleanly between the two
/// peers (Hello is client→server, HelloAck is server→client, etc.);
/// the data-plane request/reply pairs follow the same split. `Both`
/// is reserved for symmetric frames such as `Bye` (either side may
/// initiate the disconnect).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageDirection {
    ClientToServer,
    ServerToClient,
    Both,
}

impl MessageKind {
    /// Routing class derived from the comment-grouped wire ranges.
    pub fn class(&self) -> MessageClass {
        match self {
            // 0x01..0x0F — data plane request/reply pairs. The
            // BulkStream* family is in this range for backward
            // compatibility but is reclassified as `Streamed` so
            // dispatch can treat it as a long-running envelope.
            Self::Query
            | Self::Result
            | Self::Error
            | Self::BulkInsert
            | Self::BulkOk
            | Self::BulkInsertBinary
            | Self::QueryBinary
            | Self::BulkInsertPrevalidated
            | Self::Prepare
            | Self::PreparedOk
            | Self::ExecutePrepared
            | Self::Get
            | Self::Delete
            | Self::DeleteOk
            | Self::VectorSearch
            | Self::GraphTraverse
            | Self::QueryWithParams => MessageClass::DataPlane,

            // BulkStream* + RowDescription/StreamEnd describe an
            // in-flight stream rather than a single round trip.
            Self::BulkStreamStart
            | Self::BulkStreamRows
            | Self::BulkStreamCommit
            | Self::BulkStreamAck
            | Self::RowDescription
            | Self::StreamEnd => MessageClass::Streamed,

            // 0x10..0x1F — handshake / lifecycle.
            Self::Hello
            | Self::HelloAck
            | Self::AuthRequest
            | Self::AuthResponse
            | Self::AuthOk
            | Self::AuthFail
            | Self::Bye
            | Self::Ping
            | Self::Pong => MessageClass::Handshake,

            // 0x20..0x3F — control plane.
            Self::Cancel | Self::Compress | Self::SetSession | Self::Notice => {
                MessageClass::ControlPlane
            }
        }
    }

    /// Bitset of `Flags` values this kind may legitimately carry.
    ///
    /// Pinned conservatively: `MORE_FRAMES` is universal (any frame
    /// may be split), but `COMPRESSED` is whitelisted only on kinds
    /// whose payloads are big enough to benefit from compression.
    /// Handshake/lifecycle payloads (Hello, AuthRequest, Ping, …)
    /// are tiny and stay uncompressed today; future contributors
    /// who want to flip that decision must update both the matrix
    /// and the unit tests that pin it.
    pub fn allowed_flags(&self) -> Flags {
        match self {
            // Handshake / lifecycle — tiny payloads, never
            // compressed today.
            Self::Hello
            | Self::HelloAck
            | Self::AuthRequest
            | Self::AuthResponse
            | Self::AuthOk
            | Self::AuthFail
            | Self::Bye
            | Self::Ping
            | Self::Pong => Flags::MORE_FRAMES,

            // Everything else may carry both documented flags.
            _ => Flags::COMPRESSED.insert(Flags::MORE_FRAMES),
        }
    }

    /// `true` when this kind belongs to the handshake/lifecycle group
    /// (Hello, AuthRequest, AuthOk, …, Bye, Ping, Pong). Equivalent to
    /// `class() == MessageClass::Handshake` and exists so dispatch sites
    /// can read the predicate without importing `MessageClass`.
    pub fn is_handshake(&self) -> bool {
        matches!(self.class(), MessageClass::Handshake)
    }

    /// `true` when every flag bit in `flags` is in `allowed_flags()`.
    /// The catalog is the single source of truth for which flag bits a
    /// kind may carry; both the codec (decode side) and the builder
    /// (encode side) consult this so a misframed frame fails at the
    /// boundary rather than reaching the dispatch arms.
    pub fn permits_flags(&self, flags: Flags) -> bool {
        let allowed = self.allowed_flags().bits();
        (flags.bits() & !allowed) == 0
    }

    /// Which peer is allowed to originate this kind.
    pub fn direction(&self) -> MessageDirection {
        match self {
            // Client-originated requests.
            Self::Hello
            | Self::AuthResponse
            | Self::Query
            | Self::QueryBinary
            | Self::BulkInsert
            | Self::BulkInsertBinary
            | Self::BulkInsertPrevalidated
            | Self::BulkStreamStart
            | Self::BulkStreamRows
            | Self::BulkStreamCommit
            | Self::Prepare
            | Self::ExecutePrepared
            | Self::Get
            | Self::Delete
            | Self::Cancel
            | Self::Compress
            | Self::SetSession
            | Self::VectorSearch
            | Self::GraphTraverse
            | Self::QueryWithParams => MessageDirection::ClientToServer,

            // Server-originated replies / push frames.
            Self::HelloAck
            | Self::AuthRequest
            | Self::AuthOk
            | Self::AuthFail
            | Self::Result
            | Self::Error
            | Self::BulkOk
            | Self::BulkStreamAck
            | Self::PreparedOk
            | Self::DeleteOk
            | Self::Notice
            | Self::RowDescription
            | Self::StreamEnd => MessageDirection::ServerToClient,

            // Symmetric — either peer may initiate.
            Self::Bye | Self::Ping | Self::Pong => MessageDirection::Both,
        }
    }

    pub fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            0x01 => Some(Self::Query),
            0x02 => Some(Self::Result),
            0x03 => Some(Self::Error),
            0x04 => Some(Self::BulkInsert),
            0x05 => Some(Self::BulkOk),
            0x06 => Some(Self::BulkInsertBinary),
            0x07 => Some(Self::QueryBinary),
            0x08 => Some(Self::BulkInsertPrevalidated),
            0x09 => Some(Self::BulkStreamStart),
            0x0A => Some(Self::BulkStreamRows),
            0x0B => Some(Self::BulkStreamCommit),
            0x0C => Some(Self::BulkStreamAck),
            0x0D => Some(Self::Prepare),
            0x0E => Some(Self::PreparedOk),
            0x0F => Some(Self::ExecutePrepared),
            0x10 => Some(Self::Hello),
            0x11 => Some(Self::HelloAck),
            0x12 => Some(Self::AuthRequest),
            0x13 => Some(Self::AuthResponse),
            0x14 => Some(Self::AuthOk),
            0x15 => Some(Self::AuthFail),
            0x16 => Some(Self::Bye),
            0x17 => Some(Self::Ping),
            0x18 => Some(Self::Pong),
            0x19 => Some(Self::Get),
            0x1A => Some(Self::Delete),
            0x1B => Some(Self::DeleteOk),
            0x20 => Some(Self::Cancel),
            0x21 => Some(Self::Compress),
            0x22 => Some(Self::SetSession),
            0x23 => Some(Self::Notice),
            0x24 => Some(Self::RowDescription),
            0x25 => Some(Self::StreamEnd),
            0x26 => Some(Self::VectorSearch),
            0x27 => Some(Self::GraphTraverse),
            0x28 => Some(Self::QueryWithParams),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags(u8);

impl Flags {
    pub const COMPRESSED: Self = Self(0b0000_0001);
    pub const MORE_FRAMES: Self = Self(0b0000_0010);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn insert(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl std::ops::BitOr for Flags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.insert(rhs)
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    /// Every kind known to the wire spec — kept in sync with the
    /// `from_u8` table. New entries must be added here so the
    /// matrix tests below cover them.
    const ALL_KINDS: &[MessageKind] = &[
        MessageKind::Query,
        MessageKind::Result,
        MessageKind::Error,
        MessageKind::BulkInsert,
        MessageKind::BulkOk,
        MessageKind::BulkInsertBinary,
        MessageKind::QueryBinary,
        MessageKind::BulkInsertPrevalidated,
        MessageKind::BulkStreamStart,
        MessageKind::BulkStreamRows,
        MessageKind::BulkStreamCommit,
        MessageKind::BulkStreamAck,
        MessageKind::Prepare,
        MessageKind::PreparedOk,
        MessageKind::ExecutePrepared,
        MessageKind::Hello,
        MessageKind::HelloAck,
        MessageKind::AuthRequest,
        MessageKind::AuthResponse,
        MessageKind::AuthOk,
        MessageKind::AuthFail,
        MessageKind::Bye,
        MessageKind::Ping,
        MessageKind::Pong,
        MessageKind::Get,
        MessageKind::Delete,
        MessageKind::DeleteOk,
        MessageKind::Cancel,
        MessageKind::Compress,
        MessageKind::SetSession,
        MessageKind::Notice,
        MessageKind::RowDescription,
        MessageKind::StreamEnd,
        MessageKind::VectorSearch,
        MessageKind::GraphTraverse,
        MessageKind::QueryWithParams,
    ];

    #[test]
    fn class_matrix_is_pinned() {
        // Handshake / lifecycle (0x10..0x1F minus Get/Delete/DeleteOk
        // which are data plane despite the historic numbering).
        assert_eq!(MessageKind::Hello.class(), MessageClass::Handshake);
        assert_eq!(MessageKind::HelloAck.class(), MessageClass::Handshake);
        assert_eq!(MessageKind::AuthRequest.class(), MessageClass::Handshake);
        assert_eq!(MessageKind::AuthResponse.class(), MessageClass::Handshake);
        assert_eq!(MessageKind::AuthOk.class(), MessageClass::Handshake);
        assert_eq!(MessageKind::AuthFail.class(), MessageClass::Handshake);
        assert_eq!(MessageKind::Bye.class(), MessageClass::Handshake);
        assert_eq!(MessageKind::Ping.class(), MessageClass::Handshake);
        assert_eq!(MessageKind::Pong.class(), MessageClass::Handshake);

        // Data plane.
        assert_eq!(MessageKind::Query.class(), MessageClass::DataPlane);
        assert_eq!(MessageKind::Result.class(), MessageClass::DataPlane);
        assert_eq!(MessageKind::BulkInsert.class(), MessageClass::DataPlane);
        assert_eq!(MessageKind::Get.class(), MessageClass::DataPlane);
        assert_eq!(MessageKind::Delete.class(), MessageClass::DataPlane);
        assert_eq!(MessageKind::DeleteOk.class(), MessageClass::DataPlane);
        assert_eq!(MessageKind::VectorSearch.class(), MessageClass::DataPlane);
        assert_eq!(MessageKind::GraphTraverse.class(), MessageClass::DataPlane);
        assert_eq!(
            MessageKind::QueryWithParams.class(),
            MessageClass::DataPlane
        );

        // Streamed envelopes.
        assert_eq!(MessageKind::BulkStreamStart.class(), MessageClass::Streamed);
        assert_eq!(MessageKind::BulkStreamRows.class(), MessageClass::Streamed);
        assert_eq!(
            MessageKind::BulkStreamCommit.class(),
            MessageClass::Streamed
        );
        assert_eq!(MessageKind::BulkStreamAck.class(), MessageClass::Streamed);
        assert_eq!(MessageKind::RowDescription.class(), MessageClass::Streamed);
        assert_eq!(MessageKind::StreamEnd.class(), MessageClass::Streamed);

        // Control plane.
        assert_eq!(MessageKind::Cancel.class(), MessageClass::ControlPlane);
        assert_eq!(MessageKind::Compress.class(), MessageClass::ControlPlane);
        assert_eq!(MessageKind::SetSession.class(), MessageClass::ControlPlane);
        assert_eq!(MessageKind::Notice.class(), MessageClass::ControlPlane);

        // Coverage check — every catalogued kind has a class.
        for k in ALL_KINDS {
            let _ = k.class();
        }
    }

    #[test]
    fn allowed_flags_matrix_is_pinned() {
        // Handshake / lifecycle: MORE_FRAMES only — no COMPRESSED on
        // tiny control-frame payloads. Flipping this requires updating
        // the matrix below in lockstep.
        let handshake = [
            MessageKind::Hello,
            MessageKind::HelloAck,
            MessageKind::AuthRequest,
            MessageKind::AuthResponse,
            MessageKind::AuthOk,
            MessageKind::AuthFail,
            MessageKind::Bye,
            MessageKind::Ping,
            MessageKind::Pong,
        ];
        for k in handshake {
            let f = k.allowed_flags();
            assert!(
                f.contains(Flags::MORE_FRAMES),
                "{k:?} must allow MORE_FRAMES"
            );
            assert!(
                !f.contains(Flags::COMPRESSED),
                "{k:?} must NOT allow COMPRESSED today"
            );
        }

        // Everything else: both documented flags allowed.
        for k in ALL_KINDS {
            if handshake.contains(k) {
                continue;
            }
            let f = k.allowed_flags();
            assert!(
                f.contains(Flags::MORE_FRAMES),
                "{k:?} must allow MORE_FRAMES"
            );
            assert!(f.contains(Flags::COMPRESSED), "{k:?} must allow COMPRESSED");
        }
    }

    #[test]
    fn every_kind_has_unique_byte_value() {
        // The byte value is the wire spec — two kinds sharing a value
        // would silently corrupt dispatch. The catalog must reject it.
        let mut seen = std::collections::HashSet::new();
        for k in ALL_KINDS {
            let byte = *k as u8;
            assert!(
                seen.insert(byte),
                "byte 0x{byte:02x} reused by {k:?}; catalog has a duplicate"
            );
        }
    }

    #[test]
    fn from_u8_round_trips_for_every_kind() {
        for k in ALL_KINDS {
            let byte = *k as u8;
            let decoded = MessageKind::from_u8(byte).unwrap_or_else(|| {
                panic!("from_u8 returned None for catalog entry {k:?} (0x{byte:02x})")
            });
            assert_eq!(
                decoded, *k,
                "from_u8(0x{byte:02x}) must round-trip back to {k:?}"
            );
        }
    }

    #[test]
    fn permits_flags_matches_allowed_flags() {
        // Handshake kinds reject COMPRESSED, accept MORE_FRAMES.
        assert!(MessageKind::Ping.permits_flags(Flags::MORE_FRAMES));
        assert!(MessageKind::Ping.permits_flags(Flags::empty()));
        assert!(!MessageKind::Ping.permits_flags(Flags::COMPRESSED));
        assert!(!MessageKind::Ping.permits_flags(Flags::COMPRESSED | Flags::MORE_FRAMES));

        // Streamed kinds accept both documented bits — the
        // MORE_FRAMES invariant for in-flight stream envelopes is
        // declared here through `allowed_flags`.
        assert!(MessageKind::BulkStreamRows.permits_flags(Flags::MORE_FRAMES));
        assert!(MessageKind::BulkStreamRows.permits_flags(Flags::COMPRESSED));
        assert!(MessageKind::RowDescription.permits_flags(Flags::MORE_FRAMES));
        assert!(MessageKind::StreamEnd.permits_flags(Flags::MORE_FRAMES));
    }

    #[test]
    fn direction_matrix_is_pinned() {
        // Client → Server.
        for k in [
            MessageKind::Hello,
            MessageKind::AuthResponse,
            MessageKind::Query,
            MessageKind::QueryBinary,
            MessageKind::BulkInsert,
            MessageKind::BulkInsertBinary,
            MessageKind::BulkInsertPrevalidated,
            MessageKind::BulkStreamStart,
            MessageKind::BulkStreamRows,
            MessageKind::BulkStreamCommit,
            MessageKind::Prepare,
            MessageKind::ExecutePrepared,
            MessageKind::Get,
            MessageKind::Delete,
            MessageKind::Cancel,
            MessageKind::Compress,
            MessageKind::SetSession,
            MessageKind::VectorSearch,
            MessageKind::GraphTraverse,
            MessageKind::QueryWithParams,
        ] {
            assert_eq!(
                k.direction(),
                MessageDirection::ClientToServer,
                "{k:?} should be client-originated"
            );
        }

        // Server → Client.
        for k in [
            MessageKind::HelloAck,
            MessageKind::AuthRequest,
            MessageKind::AuthOk,
            MessageKind::AuthFail,
            MessageKind::Result,
            MessageKind::Error,
            MessageKind::BulkOk,
            MessageKind::BulkStreamAck,
            MessageKind::PreparedOk,
            MessageKind::DeleteOk,
            MessageKind::Notice,
            MessageKind::RowDescription,
            MessageKind::StreamEnd,
        ] {
            assert_eq!(
                k.direction(),
                MessageDirection::ServerToClient,
                "{k:?} should be server-originated"
            );
        }

        // Symmetric.
        for k in [MessageKind::Bye, MessageKind::Ping, MessageKind::Pong] {
            assert_eq!(
                k.direction(),
                MessageDirection::Both,
                "{k:?} should be symmetric"
            );
        }
    }
}
