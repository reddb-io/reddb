// Frame encode/decode. Compression is opt-in and driven by the
// COMPRESSED flag bit. When the build was configured without zstd
// support, encoding a compressed frame falls back to plaintext
// (matching the JS driver's behaviour) and decoding a compressed
// frame returns `error.CompressedButNoZstd`.

const std = @import("std");
const build_options = @import("build_options");

const frame = @import("frame.zig");
pub const FRAME_HEADER_SIZE = frame.FRAME_HEADER_SIZE;
pub const MAX_FRAME_SIZE = frame.MAX_FRAME_SIZE;
pub const KNOWN_FLAGS = frame.KNOWN_FLAGS;
pub const Flags = frame.Flags;
pub const MessageKind = frame.MessageKind;
pub const Frame = frame.Frame;

const c_zstd = if (build_options.enable_zstd)
    @cImport({
        @cInclude("zstd.h");
    })
else
    struct {};

/// Encode a frame into a freshly-allocated slice. Caller frees.
pub fn encodeFrame(allocator: std.mem.Allocator, f: Frame) ![]u8 {
    var on_wire: []const u8 = f.payload;
    var on_wire_owned: ?[]u8 = null;
    defer if (on_wire_owned) |o| allocator.free(o);

    var out_flags: u8 = f.flags & KNOWN_FLAGS;

    if ((out_flags & Flags.COMPRESSED) != 0) {
        if (build_options.enable_zstd) {
            const compressed = try zstdCompress(allocator, f.payload);
            on_wire_owned = compressed;
            on_wire = compressed;
        } else {
            // No zstd — peel the COMPRESSED flag back off so the
            // peer doesn't try to decompress what we ship plain.
            out_flags &= ~@as(u8, Flags.COMPRESSED);
        }
    }

    const total: usize = FRAME_HEADER_SIZE + on_wire.len;
    if (total > MAX_FRAME_SIZE) return error.FrameTooLarge;

    var buf = try allocator.alloc(u8, total);
    errdefer allocator.free(buf);
    frame.writeHeader(
        buf[0..FRAME_HEADER_SIZE],
        @intCast(total),
        f.kind,
        out_flags,
        f.stream_id,
        f.correlation_id,
    );
    @memcpy(buf[FRAME_HEADER_SIZE..total], on_wire);
    return buf;
}

pub const Decoded = struct {
    frame: Frame,
    consumed: usize,
    /// When non-null, `frame.payload` points into this allocation
    /// and the caller is expected to free it via `deinit`.
    owned_payload: ?[]u8 = null,

    pub fn deinit(self: *Decoded, allocator: std.mem.Allocator) void {
        if (self.owned_payload) |p| {
            allocator.free(p);
            self.owned_payload = null;
        }
    }
};

/// Decode a frame from `bytes`. Returns the parsed frame plus how
/// many bytes were consumed. The returned `Decoded.frame.payload`
/// either borrows from `bytes` (when not compressed) or points into
/// a fresh allocation owned by `Decoded.owned_payload`.
pub fn decodeFrame(allocator: std.mem.Allocator, bytes: []const u8) !Decoded {
    if (bytes.len < FRAME_HEADER_SIZE) return error.FrameTruncated;
    const header = try frame.readHeader(bytes[0..FRAME_HEADER_SIZE]);
    try frame.validateHeader(header);
    if (bytes.len < header.length) return error.FrameTruncated;

    const kind = MessageKind.fromByte(header.kind) orelse return error.FrameUnknownKind;
    const payload_slice = bytes[FRAME_HEADER_SIZE..header.length];

    var out_payload: []const u8 = payload_slice;
    var owned: ?[]u8 = null;
    if ((header.flags & Flags.COMPRESSED) != 0) {
        if (!build_options.enable_zstd) return error.CompressedButNoZstd;
        const plain = try zstdDecompress(allocator, payload_slice);
        owned = plain;
        out_payload = plain;
    }

    return Decoded{
        .frame = .{
            .kind = kind,
            .flags = header.flags,
            .stream_id = header.stream_id,
            .correlation_id = header.correlation_id,
            .payload = out_payload,
        },
        .consumed = header.length,
        .owned_payload = owned,
    };
}

// ---------------------------------------------------------------------------
// zstd glue. Only compiled when build_options.enable_zstd is true.
// ---------------------------------------------------------------------------

fn zstdCompress(allocator: std.mem.Allocator, src: []const u8) ![]u8 {
    if (!build_options.enable_zstd) unreachable;
    const bound = c_zstd.ZSTD_compressBound(src.len);
    const dst = try allocator.alloc(u8, bound);
    errdefer allocator.free(dst);
    const level: c_int = blk: {
        const env = std.posix.getenv("RED_REDWIRE_ZSTD_LEVEL") orelse break :blk 1;
        break :blk std.fmt.parseInt(c_int, env, 10) catch 1;
    };
    const written = c_zstd.ZSTD_compress(
        dst.ptr,
        bound,
        src.ptr,
        src.len,
        level,
    );
    if (c_zstd.ZSTD_isError(written) != 0) return error.DecompressFailed;
    return allocator.realloc(dst, written);
}

fn zstdDecompress(allocator: std.mem.Allocator, src: []const u8) ![]u8 {
    if (!build_options.enable_zstd) unreachable;
    // ZSTD_getFrameContentSize gives the exact uncompressed size or
    // a sentinel when the frame doesn't carry it; in the latter
    // case we grow incrementally via streaming. Engine ships
    // single-frame zstd so the fast path covers it.
    const announced = c_zstd.ZSTD_getFrameContentSize(src.ptr, src.len);
    const ZSTD_CONTENTSIZE_ERROR: u64 = @bitCast(@as(i64, -2));
    const ZSTD_CONTENTSIZE_UNKNOWN: u64 = @bitCast(@as(i64, -1));
    if (announced == ZSTD_CONTENTSIZE_ERROR) return error.DecompressFailed;
    if (announced == ZSTD_CONTENTSIZE_UNKNOWN) {
        // Grow buffer until decompression succeeds — capped at MAX_FRAME_SIZE.
        var cap: usize = @max(src.len * 4, 4096);
        while (cap <= MAX_FRAME_SIZE) : (cap *= 2) {
            const dst = try allocator.alloc(u8, cap);
            const wrote = c_zstd.ZSTD_decompress(dst.ptr, cap, src.ptr, src.len);
            if (c_zstd.ZSTD_isError(wrote) != 0) {
                allocator.free(dst);
                continue;
            }
            return allocator.realloc(dst, wrote);
        }
        return error.DecompressFailed;
    }
    const dst = try allocator.alloc(u8, @intCast(announced));
    errdefer allocator.free(dst);
    const wrote = c_zstd.ZSTD_decompress(dst.ptr, dst.len, src.ptr, src.len);
    if (c_zstd.ZSTD_isError(wrote) != 0) return error.DecompressFailed;
    return allocator.realloc(dst, wrote);
}
