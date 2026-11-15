const std = @import("std");

const frame = @import("frame.zig");

const ByteList = std.ArrayList(u8);

pub const MAX_PARAM_COUNT: usize = 65_536;
pub const MAX_VALUE_PAYLOAD_LEN: usize = @intCast(frame.MAX_FRAME_SIZE);

pub const ValueTag = enum(u8) {
    null = 0x00,
    bool = 0x01,
    int = 0x02,
    float = 0x03,
    text = 0x04,
    bytes = 0x05,
    vector = 0x06,
    json = 0x07,
    timestamp = 0x08,
    uuid = 0x09,
    fingerprint = 0x0a,
    face = 0x0b,
    iris = 0x0c,
    feature_vector = 0x0d,
    bit_signature = 0x0e,
    landmark_set = 0x0f,
};

pub const Biometric = struct {
    format: u16,
    data: []const u8,
};

pub const FeatureVector = struct {
    space_id: [16]u8,
    values: []const f32,
};

pub const BitSignature = struct {
    space_id: [16]u8,
    bits: []const u8,
    mask: ?[]const u8 = null,
};

pub const LandmarkPair = struct {
    left: []const f32,
    right: []const f32,
};

pub const LandmarkSet = struct {
    space_id: [16]u8,
    dimensions: u8,
    pairs: []const LandmarkPair,
    midline: []const []const f32,
};

pub const Value = union(enum) {
    null: void,
    bool: bool,
    int: i64,
    float: f64,
    text: []const u8,
    bytes: []const u8,
    vector: []const f32,
    json: []const u8,
    timestamp: i64,
    uuid: [16]u8,
    fingerprint: Biometric,
    face: Biometric,
    iris: Biometric,
    feature_vector: FeatureVector,
    bit_signature: BitSignature,
    landmark_set: LandmarkSet,

    pub fn uuidFromString(s: []const u8) !Value {
        return .{ .uuid = try parseUuid(s) };
    }
};

pub fn requiresBiometricCapability(value: Value) bool {
    return switch (value) {
        .fingerprint, .face, .iris, .feature_vector, .bit_signature => true,
        else => false,
    };
}

pub fn requiresLandmarkCapability(value: Value) bool {
    return switch (value) {
        .landmark_set => true,
        else => false,
    };
}

pub fn encodeValue(allocator: std.mem.Allocator, value: Value) ![]u8 {
    var out = ByteList.init(allocator);
    errdefer out.deinit();
    try appendValue(&out, value);
    return out.toOwnedSlice();
}

/// QueryWithParams payload:
///
///   u32 sql_len LE | sql bytes | u32 param_count LE | encoded values...
pub fn encodeQueryWithParams(
    allocator: std.mem.Allocator,
    sql: []const u8,
    params: []const Value,
) ![]u8 {
    if (sql.len > MAX_VALUE_PAYLOAD_LEN) return error.ValuePayloadTooLarge;
    if (params.len > MAX_PARAM_COUNT) return error.TooManyParams;

    var out = ByteList.init(allocator);
    errdefer out.deinit();
    try appendU32(&out, @intCast(sql.len));
    try out.appendSlice(sql);
    try appendU32(&out, @intCast(params.len));
    for (params) |param| {
        try appendValue(&out, param);
    }
    return out.toOwnedSlice();
}

pub fn toHttpParamsJson(allocator: std.mem.Allocator, params: []const Value) ![]u8 {
    var out = ByteList.init(allocator);
    errdefer out.deinit();
    try out.append('[');
    for (params, 0..) |param, i| {
        if (i > 0) try out.append(',');
        try appendHttpParamJson(&out, param);
    }
    try out.append(']');
    return out.toOwnedSlice();
}

pub fn toHttpQueryBody(
    allocator: std.mem.Allocator,
    sql: []const u8,
    params: []const Value,
) ![]u8 {
    var out = ByteList.init(allocator);
    errdefer out.deinit();
    try out.appendSlice("{\"query\":");
    try appendJsonString(&out, sql);
    if (params.len > 0) {
        try out.appendSlice(",\"params\":");
        for (params, 0..) |param, i| {
            if (i == 0) try out.append('[');
            if (i > 0) try out.append(',');
            try appendHttpParamJson(&out, param);
        }
        try out.append(']');
    }
    try out.append('}');
    return out.toOwnedSlice();
}

pub fn toRpcQueryParams(
    allocator: std.mem.Allocator,
    sql: []const u8,
    params: []const Value,
) ![]u8 {
    var out = ByteList.init(allocator);
    errdefer out.deinit();
    try out.appendSlice("{\"sql\":");
    try appendJsonString(&out, sql);
    if (params.len > 0) {
        try out.appendSlice(",\"params\":");
        const encoded = try toHttpParamsJson(allocator, params);
        defer allocator.free(encoded);
        try out.appendSlice(encoded);
    }
    try out.append('}');
    return out.toOwnedSlice();
}

fn appendValue(out: *ByteList, value: Value) !void {
    switch (value) {
        .null => try out.append(@intFromEnum(ValueTag.null)),
        .bool => |v| {
            try out.append(@intFromEnum(ValueTag.bool));
            try out.append(if (v) 1 else 0);
        },
        .int => |v| {
            try out.append(@intFromEnum(ValueTag.int));
            try appendI64(out, v);
        },
        .float => |v| {
            try out.append(@intFromEnum(ValueTag.float));
            const bits: u64 = @bitCast(v);
            try appendU64(out, bits);
        },
        .text => |v| try appendLenPrefixed(out, .text, v),
        .bytes => |v| try appendLenPrefixed(out, .bytes, v),
        .vector => |v| {
            const byte_len = v.len * @sizeOf(f32);
            if (byte_len > MAX_VALUE_PAYLOAD_LEN) return error.ValuePayloadTooLarge;
            try out.append(@intFromEnum(ValueTag.vector));
            try appendU32(out, @intCast(v.len));
            for (v) |f| {
                const bits: u32 = @bitCast(f);
                try appendU32(out, bits);
            }
        },
        .json => |v| try appendLenPrefixed(out, .json, v),
        .timestamp => |v| {
            try out.append(@intFromEnum(ValueTag.timestamp));
            try appendI64(out, v);
        },
        .uuid => |v| {
            try out.append(@intFromEnum(ValueTag.uuid));
            try out.appendSlice(&v);
        },
        .fingerprint => |v| try appendBiometric(out, .fingerprint, 1, v),
        .face => |v| try appendBiometric(out, .face, 2, v),
        .iris => |v| try appendBiometric(out, .iris, 3, v),
        .feature_vector => |v| {
            if (v.values.len == 0 or v.values.len > 65_536) return error.InvalidFeatureVector;
            try out.append(@intFromEnum(ValueTag.feature_vector));
            try out.appendSlice(&v.space_id);
            try appendU32(out, @intCast(v.values.len));
            var norm: f64 = 0;
            for (v.values) |coordinate| {
                if (!std.math.isFinite(coordinate)) return error.InvalidFeatureVector;
                norm += @as(f64, coordinate) * @as(f64, coordinate);
                try appendU32(out, @bitCast(coordinate));
            }
            if (norm == 0) return error.InvalidFeatureVector;
        },
        .bit_signature => |v| {
            const mask = v.mask;
            if (v.bits.len == 0 or v.bits.len > (1 << 20)) return error.InvalidBitSignature;
            if (mask) |bytes| {
                if (bytes.len != v.bits.len or !hasNonzero(bytes)) return error.InvalidBitSignature;
            }
            try out.append(@intFromEnum(ValueTag.bit_signature));
            try out.appendSlice(&v.space_id);
            try appendU32(out, @intCast(v.bits.len));
            try out.appendSlice(v.bits);
            if (mask) |bytes| {
                try out.appendSlice(bytes);
            } else {
                try out.appendNTimes(0xff, v.bits.len);
            }
        },
        .landmark_set => |v| {
            const pair_points = std.math.mul(usize, v.pairs.len, 2) catch
                return error.InvalidLandmarkSet;
            const point_count = std.math.add(usize, pair_points, v.midline.len) catch
                return error.InvalidLandmarkSet;
            if ((v.dimensions != 2 and v.dimensions != 3) or v.pairs.len < 2 or point_count > 4096)
                return error.InvalidLandmarkSet;
            try out.append(@intFromEnum(ValueTag.landmark_set));
            try out.appendSlice(&v.space_id);
            try out.append(v.dimensions);
            try appendU32(out, @intCast(v.pairs.len));
            try appendU32(out, @intCast(v.midline.len));
            for (v.pairs) |pair| {
                try appendLandmarkPoint(out, pair.left, v.dimensions);
                try appendLandmarkPoint(out, pair.right, v.dimensions);
            }
            for (v.midline) |point| try appendLandmarkPoint(out, point, v.dimensions);
        },
    }
}

fn appendLandmarkPoint(out: *ByteList, point: []const f32, dimensions: u8) !void {
    if (point.len != dimensions) return error.InvalidLandmarkSet;
    for (point) |coordinate| {
        if (!std.math.isFinite(coordinate)) return error.InvalidLandmarkSet;
        try appendU32(out, @bitCast(coordinate));
    }
}

fn appendBiometric(out: *ByteList, tag: ValueTag, modality: u16, value: Biometric) !void {
    if (value.format >> 8 != modality or value.data.len == 0 or value.data.len > MAX_VALUE_PAYLOAD_LEN) {
        return error.InvalidBiometric;
    }
    try out.append(@intFromEnum(tag));
    try appendU16(out, value.format);
    try appendU32(out, @intCast(value.data.len));
    try out.appendSlice(value.data);
}

fn hasNonzero(bytes: []const u8) bool {
    for (bytes) |byte| if (byte != 0) return true;
    return false;
}

fn appendLenPrefixed(out: *ByteList, tag: ValueTag, bytes: []const u8) !void {
    if (bytes.len > MAX_VALUE_PAYLOAD_LEN) return error.ValuePayloadTooLarge;
    try out.append(@intFromEnum(tag));
    try appendU32(out, @intCast(bytes.len));
    try out.appendSlice(bytes);
}

fn appendHttpParamJson(out: *ByteList, value: Value) !void {
    switch (value) {
        .null => try out.appendSlice("null"),
        .bool => |v| try out.appendSlice(if (v) "true" else "false"),
        .int => |v| try out.writer().print("{d}", .{v}),
        .float => |v| try out.writer().print("{d}", .{v}),
        .text => |v| try appendJsonString(out, v),
        .bytes => |v| {
            const encoded_len = std.base64.standard.Encoder.calcSize(v.len);
            const encoded = try out.allocator.alloc(u8, encoded_len);
            defer out.allocator.free(encoded);
            _ = std.base64.standard.Encoder.encode(encoded, v);
            try out.appendSlice("{\"$bytes\":");
            try appendJsonString(out, encoded);
            try out.append('}');
        },
        .vector => |v| {
            try out.append('[');
            for (v, 0..) |f, i| {
                if (i > 0) try out.append(',');
                try out.writer().print("{d}", .{f});
            }
            try out.append(']');
        },
        .json => |v| try out.appendSlice(v),
        .timestamp => |v| try out.writer().print("{{\"$ts\":{d}}}", .{v}),
        .uuid => |v| {
            try out.appendSlice("{\"$uuid\":");
            var buf: [36]u8 = undefined;
            const formatted = formatUuid(&buf, v);
            try appendJsonString(out, formatted);
            try out.append('}');
        },
        .fingerprint => |v| try appendBiometricJson(out, "fingerprint", v),
        .face => |v| try appendBiometricJson(out, "face", v),
        .iris => |v| try appendBiometricJson(out, "iris", v),
        .feature_vector => |v| {
            try out.appendSlice("{\"$feature_vector\":{\"space_id\":");
            try appendHexJsonString(out, &v.space_id);
            try out.appendSlice(",\"values\":[");
            for (v.values, 0..) |coordinate, index| {
                if (index > 0) try out.append(',');
                try out.writer().print("{d}", .{coordinate});
            }
            try out.appendSlice("]}}");
        },
        .bit_signature => |v| {
            try out.appendSlice("{\"$bit_signature\":{\"space_id\":");
            try appendHexJsonString(out, &v.space_id);
            try out.appendSlice(",\"bits\":");
            try appendHexJsonString(out, v.bits);
            try out.appendSlice(",\"mask\":");
            if (v.mask) |mask| {
                try appendHexJsonString(out, mask);
            } else {
                const mask = try out.allocator.alloc(u8, v.bits.len);
                defer out.allocator.free(mask);
                @memset(mask, 0xff);
                try appendHexJsonString(out, mask);
            }
            try out.appendSlice("}}");
        },
        .landmark_set => |v| {
            try out.appendSlice("{\"$landmark_set\":{\"space_id\":");
            try appendHexJsonString(out, &v.space_id);
            try out.writer().print(",\"dimensions\":{d},\"pairs\":[", .{v.dimensions});
            for (v.pairs, 0..) |pair, index| {
                if (index > 0) try out.append(',');
                try out.appendSlice("{\"left\":");
                try appendLandmarkPointJson(out, pair.left);
                try out.appendSlice(",\"right\":");
                try appendLandmarkPointJson(out, pair.right);
                try out.append('}');
            }
            try out.appendSlice("],\"midline\":[");
            for (v.midline, 0..) |point, index| {
                if (index > 0) try out.append(',');
                try appendLandmarkPointJson(out, point);
            }
            try out.appendSlice("]}}");
        },
    }
}

fn appendLandmarkPointJson(out: *ByteList, point: []const f32) !void {
    try out.append('[');
    for (point, 0..) |coordinate, index| {
        if (index > 0) try out.append(',');
        try out.writer().print("{d}", .{coordinate});
    }
    try out.append(']');
}

fn appendBiometricJson(out: *ByteList, modality: []const u8, value: Biometric) !void {
    try out.appendSlice("{\"$biometric\":{\"modality\":");
    try appendJsonString(out, modality);
    try out.appendSlice(",\"format\":");
    try appendJsonString(out, try biometricFormatName(value.format));
    try out.appendSlice(",\"data\":");
    try appendHexJsonString(out, value.data);
    try out.appendSlice("}}");
}

fn appendHexJsonString(out: *ByteList, bytes: []const u8) !void {
    const digits = "0123456789abcdef";
    try out.append('"');
    for (bytes) |byte| {
        try out.append(digits[byte >> 4]);
        try out.append(digits[byte & 0x0f]);
    }
    try out.append('"');
}

fn biometricFormatName(format: u16) ![]const u8 {
    return switch (format) {
        0x0101 => "ISO/IEC 39794-2",
        0x0102 => "ISO/IEC 39794-4",
        0x0103 => "ISO/IEC 19794-2",
        0x0104 => "ISO/IEC 19794-4",
        0x0105 => "ANSI INCITS 378",
        0x0106 => "ANSI INCITS 381",
        0x0107 => "ANSI/NIST-ITL Type-4",
        0x0108 => "ANSI/NIST-ITL Type-9",
        0x0109 => "CBEFF fingerprint",
        0x0201 => "ISO/IEC 39794-5",
        0x0202 => "ISO/IEC 19794-5",
        0x0203 => "ANSI/NIST-ITL Type-10",
        0x0204 => "CBEFF face",
        0x0301 => "ISO/IEC 39794-6",
        0x0302 => "ISO/IEC 19794-6",
        0x0303 => "ANSI/NIST-ITL Type-17",
        0x0304 => "CBEFF iris",
        else => error.InvalidBiometric,
    };
}

fn appendJsonString(out: *ByteList, s: []const u8) !void {
    try out.append('"');
    for (s) |c| {
        switch (c) {
            '"' => try out.appendSlice("\\\""),
            '\\' => try out.appendSlice("\\\\"),
            '\n' => try out.appendSlice("\\n"),
            '\r' => try out.appendSlice("\\r"),
            '\t' => try out.appendSlice("\\t"),
            0x08 => try out.appendSlice("\\b"),
            0x0c => try out.appendSlice("\\f"),
            0x00...0x07, 0x0b, 0x0e...0x1f => {
                const hex = "0123456789abcdef";
                try out.appendSlice("\\u00");
                try out.append(hex[@intCast(c >> 4)]);
                try out.append(hex[@intCast(c & 0x0f)]);
            },
            else => try out.append(c),
        }
    }
    try out.append('"');
}

fn appendU32(out: *ByteList, value: u32) !void {
    var buf: [4]u8 = undefined;
    std.mem.writeInt(u32, buf[0..4], value, .little);
    try out.appendSlice(&buf);
}

fn appendU16(out: *ByteList, value: u16) !void {
    var buf: [2]u8 = undefined;
    std.mem.writeInt(u16, buf[0..2], value, .little);
    try out.appendSlice(&buf);
}

fn appendU64(out: *ByteList, value: u64) !void {
    var buf: [8]u8 = undefined;
    std.mem.writeInt(u64, buf[0..8], value, .little);
    try out.appendSlice(&buf);
}

fn appendI64(out: *ByteList, value: i64) !void {
    try appendU64(out, @bitCast(value));
}

fn parseUuid(s: []const u8) ![16]u8 {
    var compact: [32]u8 = undefined;
    var compact_len: usize = 0;
    for (s) |c| {
        if (c == '-') continue;
        if (compact_len >= compact.len) return error.InvalidUuid;
        compact[compact_len] = c;
        compact_len += 1;
    }
    if (compact_len != compact.len) return error.InvalidUuid;

    var out: [16]u8 = undefined;
    for (&out, 0..) |*b, i| {
        const hi = try hexNibble(compact[i * 2]);
        const lo = try hexNibble(compact[i * 2 + 1]);
        b.* = (hi << 4) | lo;
    }
    return out;
}

fn hexNibble(c: u8) !u8 {
    return switch (c) {
        '0'...'9' => c - '0',
        'a'...'f' => c - 'a' + 10,
        'A'...'F' => c - 'A' + 10,
        else => error.InvalidUuid,
    };
}

fn formatUuid(buf: *[36]u8, uuid: [16]u8) []const u8 {
    const hex = "0123456789abcdef";
    var j: usize = 0;
    for (uuid, 0..) |byte, i| {
        if (i == 4 or i == 6 or i == 8 or i == 10) {
            buf[j] = '-';
            j += 1;
        }
        buf[j] = hex[@intCast(byte >> 4)];
        buf[j + 1] = hex[@intCast(byte & 0x0f)];
        j += 2;
    }
    return buf[0..];
}
