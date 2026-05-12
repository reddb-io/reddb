const std = @import("std");
const reddb = @import("reddb");
const value_codec = reddb.redwire.value_codec;

const t = std.testing;

test "value tag table is pinned" {
    try t.expectEqual(@as(u8, 0x00), @intFromEnum(value_codec.ValueTag.@"null"));
    try t.expectEqual(@as(u8, 0x01), @intFromEnum(value_codec.ValueTag.@"bool"));
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
    const null_value = try value_codec.encodeValue(t.allocator, .{ .@"null" = {} });
    defer t.allocator.free(null_value);
    try t.expectEqualSlices(u8, &.{0x00}, null_value);

    const true_value = try value_codec.encodeValue(t.allocator, .{ .@"bool" = true });
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
        0x06, 3, 0, 0, 0,
        0x00, 0x00, 0x80, 0x3f,
        0x00, 0x00, 0x00, 0x40,
        0x00, 0x00, 0x00, 0xbf,
    }, vector_value);
}

test "encodes query with params payload" {
    const params = [_]value_codec.Value{
        .{ .int = 42 },
        .{ .text = "x" },
        .{ .@"null" = {} },
    };
    const encoded = try value_codec.encodeQueryWithParams(t.allocator, "Q", &params);
    defer t.allocator.free(encoded);
    try t.expectEqualSlices(u8, &.{
        1, 0, 0, 0, 'Q',
        3, 0, 0, 0,
        0x02, 42, 0, 0, 0, 0, 0, 0, 0,
        0x04, 1, 0, 0, 0, 'x',
        0x00,
    }, encoded);
}

test "http query body adds typed params only when non-empty" {
    const empty = try value_codec.toHttpQueryBody(t.allocator, "SELECT \"x\"", &.{});
    defer t.allocator.free(empty);
    try t.expectEqualStrings("{\"query\":\"SELECT \\\"x\\\"\"}", empty);

    const bytes = [_]u8{ 'h', 'i' };
    const vector = [_]f32{ 1.0, 2.0 };
    const params = [_]value_codec.Value{
        .{ .@"null" = {} },
        .{ .@"bool" = true },
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
