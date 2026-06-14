const std = @import("std");
const reddb = @import("reddb");
const value_codec = reddb.redwire.value_codec;

const t = std.testing;
const empty_vector = [_]f32{};
const vector_three = [_]f32{ 1.0, 2.0, -0.5 };
const bytes_256 = makeBytes256();
const vector_128 = makeVector128();

fn makeBytes256() [256]u8 {
    var out: [256]u8 = undefined;
    for (&out, 0..) |*byte, i| {
        byte.* = @intCast(i);
    }
    return out;
}

fn makeVector128() [128]f32 {
    var out: [128]f32 = undefined;
    for (&out, 0..) |*value, i| {
        value.* = @floatFromInt(i);
    }
    return out;
}

fn readFixtureManifest(allocator: std.mem.Allocator) ![]u8 {
    const candidates = [_][]const u8{
        "../../crates/reddb-wire/tests/fixtures/params/manifest.json",
        "crates/reddb-wire/tests/fixtures/params/manifest.json",
        "../../../crates/reddb-wire/tests/fixtures/params/manifest.json",
    };

    const io = std.Options.debug_io;
    for (candidates) |path| {
        const buffer = try allocator.alloc(u8, 1024 * 1024);
        errdefer allocator.free(buffer);
        const bytes = std.Io.Dir.cwd().readFile(io, path, buffer) catch |err| switch (err) {
            error.FileNotFound => {
                allocator.free(buffer);
                continue;
            },
            else => return err,
        };
        if (bytes.len == buffer.len) return buffer;
        return try allocator.realloc(buffer, bytes.len);
    }
    return error.FixtureManifestNotFound;
}

fn manifestHex(manifest: std.json.Value, section: []const u8, name: []const u8) ![]const u8 {
    const items = manifest.object.get(section).?.array.items;
    for (items) |item| {
        const object = item.object;
        if (std.mem.eql(u8, object.get("name").?.string, name)) {
            return object.get("redwire_hex").?.string;
        }
    }
    return error.FixtureNotFound;
}

fn hexToBytes(allocator: std.mem.Allocator, hex: []const u8) ![]u8 {
    if (hex.len % 2 != 0) return error.InvalidHex;
    const out = try allocator.alloc(u8, hex.len / 2);
    errdefer allocator.free(out);
    for (out, 0..) |*byte, i| {
        byte.* = try std.fmt.parseInt(u8, hex[i * 2 .. i * 2 + 2], 16);
    }
    return out;
}

fn f64FromBits(bits: u64) f64 {
    return @bitCast(bits);
}

fn fixtureValue(name: []const u8) !value_codec.Value {
    if (std.mem.eql(u8, name, "null")) return .{ .null = {} };
    if (std.mem.eql(u8, name, "bool_true")) return .{ .bool = true };
    if (std.mem.eql(u8, name, "bool_false")) return .{ .bool = false };
    if (std.mem.eql(u8, name, "int_min")) return .{ .int = std.math.minInt(i64) };
    if (std.mem.eql(u8, name, "int_max")) return .{ .int = std.math.maxInt(i64) };
    if (std.mem.eql(u8, name, "int_42")) return .{ .int = 42 };
    if (std.mem.eql(u8, name, "float_nan")) return .{ .float = f64FromBits(0x7ff8000000000000) };
    if (std.mem.eql(u8, name, "float_pos_inf")) return .{ .float = std.math.inf(f64) };
    if (std.mem.eql(u8, name, "float_neg_inf")) return .{ .float = -std.math.inf(f64) };
    if (std.mem.eql(u8, name, "float_subnormal_min")) return .{ .float = f64FromBits(1) };
    if (std.mem.eql(u8, name, "text_unicode")) return .{ .text = "h\xc3\xa9llo" };
    if (std.mem.eql(u8, name, "text_x")) return .{ .text = "x" };
    if (std.mem.eql(u8, name, "bytes_empty")) return .{ .bytes = &.{} };
    if (std.mem.eql(u8, name, "bytes_deadbeef")) return .{ .bytes = &.{ 0xde, 0xad, 0xbe, 0xef } };
    if (std.mem.eql(u8, name, "bytes_256")) return .{ .bytes = &bytes_256 };
    if (std.mem.eql(u8, name, "json_nested")) return .{ .json = "{\"a\":null,\"z\":[1,{\"deep\":[true,false]}]}" };
    if (std.mem.eql(u8, name, "timestamp_zero")) return .{ .timestamp = 0 };
    if (std.mem.eql(u8, name, "timestamp_max")) return .{ .timestamp = std.math.maxInt(i64) };
    if (std.mem.eql(u8, name, "uuid_001122")) return try value_codec.Value.uuidFromString("00112233-4455-6677-8899-aabbccddeeff");
    if (std.mem.eql(u8, name, "vector_empty")) return .{ .vector = &empty_vector };
    if (std.mem.eql(u8, name, "vector_three")) return .{ .vector = &vector_three };
    if (std.mem.eql(u8, name, "vector_128")) return .{ .vector = &vector_128 };
    return error.UnknownFixture;
}

test "value tag table is pinned" {
    try t.expectEqual(@as(u8, 0x00), @intFromEnum(value_codec.ValueTag.null));
    try t.expectEqual(@as(u8, 0x01), @intFromEnum(value_codec.ValueTag.bool));
    try t.expectEqual(@as(u8, 0x02), @intFromEnum(value_codec.ValueTag.int));
    try t.expectEqual(@as(u8, 0x03), @intFromEnum(value_codec.ValueTag.float));
    try t.expectEqual(@as(u8, 0x04), @intFromEnum(value_codec.ValueTag.text));
    try t.expectEqual(@as(u8, 0x05), @intFromEnum(value_codec.ValueTag.bytes));
    try t.expectEqual(@as(u8, 0x06), @intFromEnum(value_codec.ValueTag.vector));
    try t.expectEqual(@as(u8, 0x07), @intFromEnum(value_codec.ValueTag.json));
    try t.expectEqual(@as(u8, 0x08), @intFromEnum(value_codec.ValueTag.timestamp));
    try t.expectEqual(@as(u8, 0x09), @intFromEnum(value_codec.ValueTag.uuid));
}

test "encodes scalar values" {
    const null_value = try value_codec.encodeValue(t.allocator, .{ .null = {} });
    defer t.allocator.free(null_value);
    try t.expectEqualSlices(u8, &.{0x00}, null_value);

    const true_value = try value_codec.encodeValue(t.allocator, .{ .bool = true });
    defer t.allocator.free(true_value);
    try t.expectEqualSlices(u8, &.{ 0x01, 0x01 }, true_value);

    const int_value = try value_codec.encodeValue(t.allocator, .{ .int = -1 });
    defer t.allocator.free(int_value);
    try t.expectEqualSlices(u8, &.{
        0x02, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    }, int_value);

    const text_value = try value_codec.encodeValue(t.allocator, .{ .text = "x" });
    defer t.allocator.free(text_value);
    try t.expectEqualSlices(u8, &.{ 0x04, 1, 0, 0, 0, 'x' }, text_value);
}

test "encodes bytes timestamp uuid json and vector" {
    const bytes_value = try value_codec.encodeValue(t.allocator, .{ .bytes = &.{ 0xde, 0xad, 0xbe, 0xef } });
    defer t.allocator.free(bytes_value);
    try t.expectEqualSlices(u8, &.{ 0x05, 4, 0, 0, 0, 0xde, 0xad, 0xbe, 0xef }, bytes_value);

    const timestamp_value = try value_codec.encodeValue(t.allocator, .{ .timestamp = 1_700_000_000 });
    defer t.allocator.free(timestamp_value);
    try t.expectEqualSlices(u8, &.{ 0x08, 0x00, 0xf1, 0x53, 0x65, 0x00, 0x00, 0x00, 0x00 }, timestamp_value);

    const uuid_value = try value_codec.encodeValue(
        t.allocator,
        try value_codec.Value.uuidFromString("00112233-4455-6677-8899-aabbccddeeff"),
    );
    defer t.allocator.free(uuid_value);
    try t.expectEqualSlices(u8, &.{
        0x09, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
        0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
    }, uuid_value);

    const json_value = try value_codec.encodeValue(t.allocator, .{ .json = "{\"a\":1,\"b\":2}" });
    defer t.allocator.free(json_value);
    try t.expectEqualSlices(u8, &.{
        0x07, 13, 0, 0, 0, '{', '"', 'a', '"', ':', '1', ',', '"', 'b', '"', ':', '2', '}',
    }, json_value);

    const vector = [_]f32{ 1.0, 2.0, -0.5 };
    const vector_value = try value_codec.encodeValue(t.allocator, .{ .vector = &vector });
    defer t.allocator.free(vector_value);
    try t.expectEqualSlices(u8, &.{
        0x06, 3,    0,    0,    0,
        0x00, 0x00, 0x80, 0x3f, 0x00,
        0x00, 0x00, 0x40, 0x00, 0x00,
        0x00, 0xbf,
    }, vector_value);
}

test "encodes query with params payload" {
    const params = [_]value_codec.Value{
        .{ .int = 42 },
        .{ .text = "x" },
        .{ .null = {} },
    };
    const encoded = try value_codec.encodeQueryWithParams(t.allocator, "Q", &params);
    defer t.allocator.free(encoded);
    try t.expectEqualSlices(u8, &.{
        1,  0, 0, 0,    'Q',
        3,  0, 0, 0,    0x02,
        42, 0, 0, 0,    0,
        0,  0, 0, 0x04, 1,
        0,  0, 0, 'x',  0x00,
    }, encoded);
}

test "http query body adds typed params only when non-empty" {
    const empty = try value_codec.toHttpQueryBody(t.allocator, "SELECT \"x\"", &.{});
    defer t.allocator.free(empty);
    try t.expectEqualStrings("{\"query\":\"SELECT \\\"x\\\"\"}", empty);

    const bytes = [_]u8{ 'h', 'i' };
    const vector = [_]f32{ 1.0, 2.0 };
    const params = [_]value_codec.Value{
        .{ .null = {} },
        .{ .bool = true },
        .{ .int = 42 },
        .{ .float = 1.5 },
        .{ .text = "txt" },
        .{ .bytes = &bytes },
        .{ .vector = &vector },
        .{ .json = "{\"a\":1,\"b\":2}" },
        .{ .timestamp = 1_700_000_000 },
        try value_codec.Value.uuidFromString("00112233-4455-6677-8899-aabbccddeeff"),
    };
    const body = try value_codec.toHttpQueryBody(t.allocator, "SELECT $1", &params);
    defer t.allocator.free(body);
    try t.expect(std.mem.indexOf(u8, body, "\"params\"") != null);
    try t.expect(std.mem.indexOf(u8, body, "\"$bytes\":\"aGk=\"") != null);
    try t.expect(std.mem.indexOf(u8, body, "\"$uuid\":\"00112233-4455-6677-8899-aabbccddeeff\"") != null);
}

test "shared parameter fixtures match manifest" {
    const manifest_bytes = try readFixtureManifest(t.allocator);
    defer t.allocator.free(manifest_bytes);

    var parsed = try std.json.parseFromSlice(std.json.Value, t.allocator, manifest_bytes, .{});
    defer parsed.deinit();

    const names = [_][]const u8{
        "null",
        "bool_true",
        "bool_false",
        "int_min",
        "int_max",
        "int_42",
        "float_nan",
        "float_pos_inf",
        "float_neg_inf",
        "float_subnormal_min",
        "text_unicode",
        "text_x",
        "bytes_empty",
        "bytes_deadbeef",
        "bytes_256",
        "json_nested",
        "timestamp_zero",
        "timestamp_max",
        "uuid_001122",
        "vector_empty",
        "vector_three",
        "vector_128",
    };

    for (names) |name| {
        const encoded = try value_codec.encodeValue(t.allocator, try fixtureValue(name));
        defer t.allocator.free(encoded);

        const expected = try hexToBytes(t.allocator, try manifestHex(parsed.value, "values", name));
        defer t.allocator.free(expected);

        try t.expectEqualSlices(u8, expected, encoded);
    }

    const query = parsed.value.object.get("queries").?.array.items[0].object;
    const param_names = query.get("params").?.array.items;
    const params = try t.allocator.alloc(value_codec.Value, param_names.len);
    defer t.allocator.free(params);
    for (param_names, 0..) |param, i| {
        params[i] = try fixtureValue(param.string);
    }

    const encoded_query = try value_codec.encodeQueryWithParams(
        t.allocator,
        query.get("sql").?.string,
        params,
    );
    defer t.allocator.free(encoded_query);

    const expected_query = try hexToBytes(
        t.allocator,
        try manifestHex(parsed.value, "queries", query.get("name").?.string),
    );
    defer t.allocator.free(expected_query);

    try t.expectEqualSlices(u8, expected_query, encoded_query);
}
