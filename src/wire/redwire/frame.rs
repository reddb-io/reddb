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
}

impl MessageKind {
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
