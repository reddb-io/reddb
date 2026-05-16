// Mirrors `drivers/go/helpers_test.go` 1:1 via std.testing and a fake
// querier that records SQL and replays scripted JSON responses.

const std = @import("std");
const testing = std.testing;
const json = std.json;
const reddb = @import("reddb");
const helpers = reddb.helpers;

const FakeQuerier = struct {
    allocator: std.mem.Allocator,
    calls: std.ArrayList(Call),
    replies: std.ArrayList([]const u8),
    next_reply: usize = 0,
    errs: std.ArrayList(?anyerror),
    next_err: usize = 0,

    const Call = struct {
        sql: []u8,
        params: [][]u8,
    };

    pub fn init(allocator: std.mem.Allocator) FakeQuerier {
        return .{
            .allocator = allocator,
            .calls = std.ArrayList(Call).init(allocator),
            .replies = std.ArrayList([]const u8).init(allocator),
            .errs = std.ArrayList(?anyerror).init(allocator),
        };
    }

    pub fn deinit(self: *FakeQuerier) void {
        for (self.calls.items) |c| {
            self.allocator.free(c.sql);
            for (c.params) |p| self.allocator.free(p);
            self.allocator.free(c.params);
        }
        self.calls.deinit();
        self.replies.deinit();
        self.errs.deinit();
    }

    pub fn querier(self: *FakeQuerier) helpers.Querier {
        return .{
            .ptr = self,
            .queryFn = queryThunk,
        };
    }

    fn queryThunk(
        ptr: *anyopaque,
        allocator: std.mem.Allocator,
        sql: []const u8,
        params: []const []const u8,
    ) anyerror![]u8 {
        const self: *FakeQuerier = @ptrCast(@alignCast(ptr));
        const owned_sql = try self.allocator.dupe(u8, sql);
        var owned_params = try self.allocator.alloc([]u8, params.len);
        for (params, 0..) |p, i| owned_params[i] = try self.allocator.dupe(u8, p);
        try self.calls.append(.{ .sql = owned_sql, .params = owned_params });

        if (self.next_err < self.errs.items.len) {
            const e = self.errs.items[self.next_err];
            self.next_err += 1;
            if (e) |err| return err;
        }

        if (self.next_reply >= self.replies.items.len) return try allocator.alloc(u8, 0);
        const body = self.replies.items[self.next_reply];
        self.next_reply += 1;
        return try allocator.dupe(u8, body);
    }
};

// ---------------------------------------------------------------- KV path

test "kvPath quotes namespaced keys" {
    const a = testing.allocator;
    const p = try helpers.kvPath(a, "kv_default", "corpus:version");
    defer a.free(p);
    try testing.expectEqualStrings("kv_default.'corpus:version'", p);
}

test "kvPath preserves dots and slashes" {
    const a = testing.allocator;
    const p = try helpers.kvPath(a, "kv_default", "a/b.c");
    defer a.free(p);
    try testing.expectEqualStrings("kv_default.'a/b.c'", p);
}

test "kvPath rejects bad collection" {
    const a = testing.allocator;
    try testing.expectError(helpers.HelperError.InvalidArgument,
        helpers.kvPath(a, "bad-name!", "k"));
}

test "kvValueLiteral cases" {
    const a = testing.allocator;

    {
        const lit = try helpers.kvValueLiteral(a, .{ .null = {} });
        defer a.free(lit);
        try testing.expectEqualStrings("NULL", lit);
    }
    {
        const lit = try helpers.kvValueLiteral(a, .{ .bool = true });
        defer a.free(lit);
        try testing.expectEqualStrings("true", lit);
    }
    {
        const lit = try helpers.kvValueLiteral(a, .{ .bool = false });
        defer a.free(lit);
        try testing.expectEqualStrings("false", lit);
    }
    {
        const lit = try helpers.kvValueLiteral(a, .{ .integer = 42 });
        defer a.free(lit);
        try testing.expectEqualStrings("42", lit);
    }
    {
        const lit = try helpers.kvValueLiteral(a, .{ .string = "hi" });
        defer a.free(lit);
        try testing.expectEqualStrings("'hi'", lit);
    }
    {
        const lit = try helpers.kvValueLiteral(a, .{ .string = "o'reilly" });
        defer a.free(lit);
        try testing.expectEqualStrings("'o''reilly'", lit);
    }
    {
        var obj = json.ObjectMap.init(a);
        defer obj.deinit();
        try obj.put("a", .{ .integer = 1 });
        const lit = try helpers.kvValueLiteral(a, .{ .object = obj });
        defer a.free(lit);
        try testing.expectEqualStrings("'{\"a\":1}'", lit);
    }
}

// ---------------------------------------------------------------- KV ops

test "kv set emits exact key path" {
    const a = testing.allocator;
    var fq = FakeQuerier.init(a);
    defer fq.deinit();
    try fq.replies.append("{}");
    const h = helpers.Helpers.init(a, fq.querier());
    try h.kv(null).set("characters:hansel", .{ .string = "ok" }, .{});
    const sql = fq.calls.items[0].sql;
    try testing.expect(std.mem.indexOf(u8, sql, "kv_default.'characters:hansel'") != null);
    try testing.expect(std.mem.indexOf(u8, sql, "= 'ok'") != null);
}

test "kv get returns value or null" {
    const a = testing.allocator;
    var fq = FakeQuerier.init(a);
    defer fq.deinit();
    try fq.replies.append(
        \\{"rows":[{"value":"v"}]}
    );
    try fq.replies.append(
        \\{"rows":[]}
    );
    const h = helpers.Helpers.init(a, fq.querier());
    var got = (try h.kv(null).get("k", null)) orelse return error.MissingValue;
    try testing.expectEqualStrings("v", got.string);
    a.free(got.string);

    const got2 = try h.kv(null).get("k2", null);
    try testing.expect(got2 == null);
}

test "kv list filters by prefix without rewriting" {
    const a = testing.allocator;
    var fq = FakeQuerier.init(a);
    defer fq.deinit();
    try fq.replies.append(
        \\{"rows":[{"key":"a:1","value":1},{"key":"b:1","value":2},{"key":"a:2","value":3}]}
    );
    const h = helpers.Helpers.init(a, fq.querier());
    var out = try h.kv(null).list(.{ .prefix = "a:" });
    defer {
        for (out.items) |row| freeCloned(a, row);
        out.deinit(a);
    }
    try testing.expectEqual(@as(usize, 2), out.items.len);
    try testing.expectEqualStrings("a:1", out.items[0].object.get("key").?.string);
    try testing.expectEqualStrings("a:2", out.items[1].object.get("key").?.string);
}

test "kv list rejects negative limit" {
    const a = testing.allocator;
    var fq = FakeQuerier.init(a);
    defer fq.deinit();
    const h = helpers.Helpers.init(a, fq.querier());
    try testing.expectError(helpers.HelperError.InvalidArgument,
        h.kv(null).list(.{ .limit = -1 }));
}

// ---------------------------------------------------------------- Queue

test "queue push emits priority and payload" {
    const a = testing.allocator;
    var fq = FakeQuerier.init(a);
    defer fq.deinit();
    try fq.replies.append(
        \\{"affected":1}
    );
    const h = helpers.Helpers.init(a, fq.querier());
    var payload = json.ObjectMap.init(a);
    defer payload.deinit();
    try payload.put("id", .{ .integer = 1 });
    var res = try h.queue().push("jobs", .{ .object = payload }, .{ .priority = 5 });
    defer res.deinit(a);
    const sql = fq.calls.items[0].sql;
    try testing.expect(std.mem.startsWith(u8, sql, "QUEUE PUSH jobs "));
    try testing.expect(std.mem.indexOf(u8, sql, "PRIORITY 5") != null);
    try testing.expect(std.mem.indexOf(u8, sql, "{\"id\":1}") != null);
}

test "queue len returns int" {
    const a = testing.allocator;
    var fq = FakeQuerier.init(a);
    defer fq.deinit();
    try fq.replies.append(
        \\{"rows":[{"len":3}]}
    );
    const h = helpers.Helpers.init(a, fq.querier());
    const n = try h.queue().len("jobs");
    try testing.expectEqual(@as(i64, 3), n);
}

test "queue pop returns payloads" {
    const a = testing.allocator;
    var fq = FakeQuerier.init(a);
    defer fq.deinit();
    try fq.replies.append(
        \\{"rows":[{"payload":"a"},{"payload":"b"}]}
    );
    const h = helpers.Helpers.init(a, fq.querier());
    const out = try h.queue().pop("jobs", 2);
    defer {
        for (out) |v| freeCloned(a, v);
        a.free(out);
    }
    try testing.expectEqual(@as(usize, 2), out.len);
    try testing.expectEqualStrings("a", out[0].string);
    try testing.expectEqualStrings("b", out[1].string);
}

test "queue pop rejects negative count" {
    const a = testing.allocator;
    var fq = FakeQuerier.init(a);
    defer fq.deinit();
    const h = helpers.Helpers.init(a, fq.querier());
    try testing.expectError(helpers.HelperError.InvalidArgument,
        h.queue().pop("jobs", -1));
}

test "queue push rejects invalid identifier" {
    const a = testing.allocator;
    var fq = FakeQuerier.init(a);
    defer fq.deinit();
    const h = helpers.Helpers.init(a, fq.querier());
    try testing.expectError(helpers.HelperError.InvalidArgument,
        h.queue().push("bad-name!", .{ .string = "x" }, .{}));
}

// ---------------------------------------------------------------- Documents

test "documents insert returns rid envelope" {
    const a = testing.allocator;
    var fq = FakeQuerier.init(a);
    defer fq.deinit();
    try fq.replies.append(
        \\{"rows":[],"affected":0}
    );
    try fq.replies.append(
        \\{"rows":[{"rid":"doc-1","body":{"name":"alice"}}],"affected":1}
    );
    const h = helpers.Helpers.init(a, fq.querier());
    var doc = json.ObjectMap.init(a);
    defer doc.deinit();
    try doc.put("name", .{ .string = "alice" });
    var out = try h.documents().insert("people", .{ .object = doc });
    defer {
        a.free(out.rid);
        if (out.item) |it| freeCloned(a, it);
    }
    try testing.expectEqual(@as(i64, 1), out.affected);
    try testing.expectEqualStrings("doc-1", out.rid);
    try testing.expect(out.item != null);
    try testing.expectEqualStrings("doc-1", out.item.?.object.get("rid").?.string);
}

test "documents get raises not found on missing" {
    const a = testing.allocator;
    var fq = FakeQuerier.init(a);
    defer fq.deinit();
    try fq.replies.append(
        \\{"rows":[]}
    );
    const h = helpers.Helpers.init(a, fq.querier());
    try testing.expectError(helpers.HelperError.NotFound,
        h.documents().get("people", "doc-1"));
}

test "documents patch rejects json pointer paths" {
    const a = testing.allocator;
    var fq = FakeQuerier.init(a);
    defer fq.deinit();
    const h = helpers.Helpers.init(a, fq.querier());
    var patch = json.ObjectMap.init(a);
    defer patch.deinit();
    try patch.put("a/b", .{ .integer = 1 });
    try testing.expectError(helpers.HelperError.InvalidArgument,
        h.documents().patch("people", "doc-1", patch));
}

test "documents list orders by rid by default" {
    const a = testing.allocator;
    var fq = FakeQuerier.init(a);
    defer fq.deinit();
    try fq.replies.append(
        \\{"rows":[{"rid":"a"},{"rid":"b"}]}
    );
    const h = helpers.Helpers.init(a, fq.querier());
    var out = try h.documents().list("people", .{});
    defer {
        for (out.items) |row| freeCloned(a, row);
        out.deinit(a);
    }
    try testing.expectEqual(@as(usize, 2), out.items.len);
    try testing.expect(std.mem.indexOf(u8, fq.calls.items[0].sql, "ORDER BY rid ASC") != null);
}

// ---------------------------------------------------------- decode helpers

test "affectedFromBody handles nested result" {
    const a = testing.allocator;
    const n = try helpers.affectedFromBody(a,
        \\{"result":{"affected":7}}
    );
    try testing.expectEqual(@as(i64, 7), n);
}

// ---------------------------------------------------------- internal helpers

fn freeCloned(allocator: std.mem.Allocator, value: json.Value) void {
    switch (value) {
        .null, .bool, .integer, .float => {},
        .number_string, .string => |s| allocator.free(s),
        .array => |arr| {
            var a = arr;
            for (a.items) |v| freeCloned(allocator, v);
            a.deinit();
        },
        .object => |obj| {
            var o = obj;
            var it = o.iterator();
            while (it.next()) |entry| {
                allocator.free(entry.key_ptr.*);
                freeCloned(allocator, entry.value_ptr.*);
            }
            o.deinit();
        },
    }
}
