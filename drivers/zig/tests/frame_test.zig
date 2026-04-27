// Frame codec round-trip + edge cases. Mirrors the Rust driver's
// `codec::tests` so any wire change has to update both sides.

const std = @import("std");
const reddb = @import("reddb");
const codec = reddb.redwire.codec;
const frame = reddb.redwire.frame;
const build_options = reddb.build_options;

const t = std.testing;

test "round-trip Query frame" {
    const f = frame.Frame.init(.query, 7, "SELECT 1");
    const bytes = try codec.encodeFrame(t.allocator, f);
    defer t.allocator.free(bytes);

    var decoded = try codec.decodeFrame(t.allocator, bytes);
    defer decoded.deinit(t.allocator);

    try t.expectEqual(frame.MessageKind.query, decoded.frame.kind);
    try t.expectEqual(@as(u64, 7), decoded.frame.correlation_id);
    try t.expectEqualStrings("SELECT 1", decoded.frame.payload);
    try t.expectEqual(bytes.len, decoded.consumed);
}

test "round-trip empty payload (Ping)" {
    const f = frame.Frame.init(.ping, 42, &.{});
    const bytes = try codec.encodeFrame(t.allocator, f);
    defer t.allocator.free(bytes);
    try t.expectEqual(@as(usize, frame.FRAME_HEADER_SIZE), bytes.len);

    var decoded = try codec.decodeFrame(t.allocator, bytes);
    defer decoded.deinit(t.allocator);
    try t.expectEqual(frame.MessageKind.ping, decoded.frame.kind);
    try t.expectEqual(@as(usize, 0), decoded.frame.payload.len);
}

test "reject too-large length" {
    var hdr: [frame.FRAME_HEADER_SIZE]u8 = undefined;
    std.mem.writeInt(u32, hdr[0..4], frame.MAX_FRAME_SIZE + 1, .little);
    hdr[4] = @intFromEnum(frame.MessageKind.query);
    hdr[5] = 0;
    std.mem.writeInt(u16, hdr[6..8], 0, .little);
    std.mem.writeInt(u64, hdr[8..16], 0, .little);
    try t.expectError(error.FrameInvalidLength, codec.decodeFrame(t.allocator, &hdr));
}

test "reject unknown flag bits" {
    var hdr: [frame.FRAME_HEADER_SIZE]u8 = undefined;
    std.mem.writeInt(u32, hdr[0..4], frame.FRAME_HEADER_SIZE, .little);
    hdr[4] = @intFromEnum(frame.MessageKind.query);
    hdr[5] = 0b1000_0000; // bit not in KNOWN_FLAGS
    std.mem.writeInt(u16, hdr[6..8], 0, .little);
    std.mem.writeInt(u64, hdr[8..16], 0, .little);
    try t.expectError(error.FrameUnknownFlags, codec.decodeFrame(t.allocator, &hdr));
}

test "reject unknown message kind" {
    var hdr: [frame.FRAME_HEADER_SIZE]u8 = undefined;
    std.mem.writeInt(u32, hdr[0..4], frame.FRAME_HEADER_SIZE, .little);
    hdr[4] = 0xEE;
    hdr[5] = 0;
    std.mem.writeInt(u16, hdr[6..8], 0, .little);
    std.mem.writeInt(u64, hdr[8..16], 0, .little);
    try t.expectError(error.FrameUnknownKind, codec.decodeFrame(t.allocator, &hdr));
}

test "truncated header" {
    const short = [_]u8{ 0, 0, 0 };
    try t.expectError(error.FrameTruncated, codec.decodeFrame(t.allocator, &short));
}

test "compressed payload round-trip (zstd or skipped)" {
    if (!build_options.enable_zstd) {
        // No zstd in this build: encoding falls back to plaintext
        // (flag stripped) and decoding compressed bytes errors out.
        return error.SkipZigTest;
    }
    var f = frame.Frame.init(.result, 1, "abcabc" ** 50);
    f.flags = frame.Flags.COMPRESSED;
    const bytes = try codec.encodeFrame(t.allocator, f);
    defer t.allocator.free(bytes);
    // Compressed body should be smaller than the raw payload.
    try t.expect(bytes.len < frame.FRAME_HEADER_SIZE + f.payload.len);

    var decoded = try codec.decodeFrame(t.allocator, bytes);
    defer decoded.deinit(t.allocator);
    try t.expectEqualStrings(f.payload, decoded.frame.payload);
    try t.expectEqual(frame.Flags.COMPRESSED, decoded.frame.flags & frame.Flags.COMPRESSED);
}

test "compressed flag without zstd reports error" {
    if (build_options.enable_zstd) return error.SkipZigTest;
    // Hand-craft a frame with the COMPRESSED bit set. Decoder
    // should reject it with the canonical error.
    var hdr_and_body: [frame.FRAME_HEADER_SIZE + 4]u8 = undefined;
    std.mem.writeInt(u32, hdr_and_body[0..4], frame.FRAME_HEADER_SIZE + 4, .little);
    hdr_and_body[4] = @intFromEnum(frame.MessageKind.result);
    hdr_and_body[5] = frame.Flags.COMPRESSED;
    std.mem.writeInt(u16, hdr_and_body[6..8], 0, .little);
    std.mem.writeInt(u64, hdr_and_body[8..16], 0, .little);
    @memcpy(hdr_and_body[frame.FRAME_HEADER_SIZE..], &[_]u8{ 0xff, 0xff, 0xff, 0xff });
    try t.expectError(error.CompressedButNoZstd, codec.decodeFrame(t.allocator, &hdr_and_body));
}

test "header has correct little-endian layout" {
    const f = frame.Frame.init(.query, 0xCAFE_BABE_DEAD_BEEF, "hi");
    const bytes = try codec.encodeFrame(t.allocator, f);
    defer t.allocator.free(bytes);
    try t.expectEqual(@as(u32, frame.FRAME_HEADER_SIZE + 2), std.mem.readInt(u32, bytes[0..4], .little));
    try t.expectEqual(@as(u8, 0x01), bytes[4]);
    try t.expectEqual(@as(u8, 0), bytes[5]);
    try t.expectEqual(@as(u16, 0), std.mem.readInt(u16, bytes[6..8], .little));
    try t.expectEqual(@as(u64, 0xCAFE_BABE_DEAD_BEEF), std.mem.readInt(u64, bytes[8..16], .little));
}
