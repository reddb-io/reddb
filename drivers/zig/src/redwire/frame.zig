// RedWire frame layout. Mirrors `src/wire/redwire/frame.rs` and
// the Rust driver's `drivers/rust/src/redwire/frame.rs`. Keep the
// byte-level constants in lockstep — the wire is a public ABI.

const std = @import("std");

pub const FRAME_HEADER_SIZE: usize = 16;
pub const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024;
pub const KNOWN_FLAGS: u8 = 0b0000_0011;

pub const Flags = struct {
    pub const COMPRESSED: u8 = 0b0000_0001;
    pub const MORE_FRAMES: u8 = 0b0000_0010;
};

pub const MessageKind = enum(u8) {
    query = 0x01,
    result = 0x02,
    err = 0x03,
    bulk_insert = 0x04,
    bulk_ok = 0x05,
    bulk_insert_binary = 0x06,
    query_binary = 0x07,
    bulk_insert_prevalidated = 0x08,
    hello = 0x10,
    hello_ack = 0x11,
    auth_request = 0x12,
    auth_response = 0x13,
    auth_ok = 0x14,
    auth_fail = 0x15,
    bye = 0x16,
    ping = 0x17,
    pong = 0x18,
    get = 0x19,
    delete = 0x1A,
    delete_ok = 0x1B,

    pub fn fromByte(b: u8) ?MessageKind {
        return switch (b) {
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08 => @enumFromInt(b),
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B => @enumFromInt(b),
            else => null,
        };
    }
};

/// Header laid out in wire order. Encoded little-endian everywhere.
/// Kept as a plain struct because Zig's `extern struct` field
/// alignment for `u64` would force a 4-byte gap between `stream_id`
/// and `correlation_id`; we encode/decode by hand instead.
pub const FrameHeader = struct {
    length: u32,
    kind: u8,
    flags: u8,
    stream_id: u16,
    correlation_id: u64,
};

pub const Frame = struct {
    kind: MessageKind,
    flags: u8 = 0,
    stream_id: u16 = 0,
    correlation_id: u64,
    payload: []const u8,

    pub fn init(kind: MessageKind, correlation_id: u64, payload: []const u8) Frame {
        return .{
            .kind = kind,
            .flags = 0,
            .stream_id = 0,
            .correlation_id = correlation_id,
            .payload = payload,
        };
    }

    pub fn encodedLen(self: Frame) u32 {
        return @intCast(FRAME_HEADER_SIZE + self.payload.len);
    }
};

/// Write a frame header into `dst`. Caller guarantees `dst.len ==
/// FRAME_HEADER_SIZE` and that `total_length` already accounts for
/// any compression done on the payload.
pub fn writeHeader(
    dst: []u8,
    total_length: u32,
    kind: MessageKind,
    flags: u8,
    stream_id: u16,
    correlation_id: u64,
) void {
    std.debug.assert(dst.len >= FRAME_HEADER_SIZE);
    std.mem.writeInt(u32, dst[0..4], total_length, .little);
    dst[4] = @intFromEnum(kind);
    dst[5] = flags;
    std.mem.writeInt(u16, dst[6..8], stream_id, .little);
    std.mem.writeInt(u64, dst[8..16], correlation_id, .little);
}

/// Decode just the header. Doesn't validate — call `validateHeader`
/// after to enforce length / flag bounds.
pub fn readHeader(src: []const u8) !FrameHeader {
    if (src.len < FRAME_HEADER_SIZE) return error.FrameTruncated;
    return FrameHeader{
        .length = std.mem.readInt(u32, src[0..4], .little),
        .kind = src[4],
        .flags = src[5],
        .stream_id = std.mem.readInt(u16, src[6..8], .little),
        .correlation_id = std.mem.readInt(u64, src[8..16], .little),
    };
}

pub fn validateHeader(h: FrameHeader) !void {
    if (h.length < FRAME_HEADER_SIZE or h.length > MAX_FRAME_SIZE) {
        return error.FrameInvalidLength;
    }
    if ((h.flags & ~KNOWN_FLAGS) != 0) {
        return error.FrameUnknownFlags;
    }
}
