//! Wire frame layout — mirrors the engine's
//! `reddb::wire::redwire::frame`. Kept duplicated rather than
//! shared via a crate dependency so the driver can build without
//! the engine.

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

    pub fn encoded_len(&self) -> u32 {
        (FRAME_HEADER_SIZE + self.payload.len()) as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageKind {
    Query = 0x01,
    Result = 0x02,
    Error = 0x03,
    BulkInsert = 0x04,
    BulkOk = 0x05,
    BulkInsertBinary = 0x06,
    QueryBinary = 0x07,
    BulkInsertPrevalidated = 0x08,
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
}
