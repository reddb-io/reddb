const std = @import("std");

const frame = @import("frame.zig");

pub const MAX_PARAM_COUNT: usize = 65_536;
pub const MAX_VALUE_PAYLOAD_LEN: usize = @intCast(frame.MAX_FRAME_SIZE);

pub const ValueTag = enum(u8) {
    @"null" = 0x00,
    @"bool" = 0x01,
    int = 0x02,
    float = 0x03,
    text = 0x04,
    bytes = 0x05,
    vector = 0x06,
    json = 0x07,
    timestamp = 0x08,
    uuid = 0x09,
};

pub const Value = union(enum) {
    @"null": void,
    @"bool": bool,
    int: i64,
    float: f64,
    text: []const u8,
    bytes: []const u8,
    vector: []const f32,
    json: []const u8,
    timestamp: i64,
    uuid: [16]u8,

    pub fn uuidFromString(s: []const u8) !Value {
        return .{ .uuid = try parseUuid(s) };
    }
};

pub fn encodeValue(allocator: std.mem.Allocator, value: Value) ![]u8 {
    var out = std.ArrayList(u8).init(allocator);
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

    var out = std.ArrayList(u8).init(allocator);
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
    var out = std.ArrayList(u8).init(allocator);
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
    var out = std.ArrayList(u8).init(allocator);
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

fn appendValue(out: *std.ArrayList(u8), value: Value) !void {
    switch (value) {
        .@"null" => try out.append(@intFromEnum(ValueTag.@"null")),
        .@"bool" => |v| {
            try out.append(@intFromEnum(ValueTag.@"bool"));
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
    }
}

fn appendLenPrefixed(out: *std.ArrayList(u8), tag: ValueTag, bytes: []const u8) !void {
    if (bytes.len > MAX_VALUE_PAYLOAD_LEN) return error.ValuePayloadTooLarge;
    try out.append(@intFromEnum(tag));
    try appendU32(out, @intCast(bytes.len));
    try out.appendSlice(bytes);
}

fn appendHttpParamJson(out: *std.ArrayList(u8), value: Value) !void {
    switch (value) {
        .@"null" => try out.appendSlice("null"),
        .@"bool" => |v| try out.appendSlice(if (v) "true" else "false"),
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
    }
}

fn appendJsonString(out: *std.ArrayList(u8), s: []const u8) !void {
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
            0x00...0x1f => {
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

fn appendU32(out: *std.ArrayList(u8), value: u32) !void {
    var buf: [4]u8 = undefined;
    std.mem.writeInt(u32, buf[0..4], value, .little);
    try out.appendSlice(&buf);
}

fn appendU64(out: *std.ArrayList(u8), value: u64) !void {
    var buf: [8]u8 = undefined;
    std.mem.writeInt(u64, buf[0..8], value, .little);
    try out.appendSlice(&buf);
}

fn appendI64(out: *std.ArrayList(u8), value: i64) !void {
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
