//! SDK Helper Spec v0.1 — rich helper surface on top of any transport
//! exposing a `Querier`. Mirrors `drivers/go/helpers.go` 1:1: same
//! `documents` / `kv` / `queue` namespaces, same envelopes, same typed
//! error set.
//!
//! All helpers build SQL strings using a small caller-owned allocator;
//! the same wire request works across RedWire and HTTP.

const std = @import("std");
const Allocator = std.mem.Allocator;
const json = std.json;

pub const HelperError = error{
    InvalidArgument,
    NotFound,
    InvalidResponse,
    OutOfMemory,
    InvalidJson,
};

// --- Querier ----------------------------------------------------------------

/// Function-pointer-based contract every transport implements. `query`
/// returns the engine's raw JSON envelope bytes (caller-owned, allocated
/// with `allocator`). `params` are positional `$N` arguments encoded as
/// caller-supplied strings — empty slice means "no params".
pub const Querier = struct {
    ptr: *anyopaque,
    queryFn: *const fn (
        ptr: *anyopaque,
        allocator: Allocator,
        sql: []const u8,
        params: []const []const u8,
    ) anyerror![]u8,

    pub fn query(
        self: Querier,
        allocator: Allocator,
        sql: []const u8,
        params: []const []const u8,
    ) ![]u8 {
        return self.queryFn(self.ptr, allocator, sql, params);
    }
};

// --- Envelopes --------------------------------------------------------------

pub const InsertResult = struct {
    affected: i64,
    rid: []u8,
    item: ?json.Value,

    pub fn deinit(self: *InsertResult, allocator: Allocator) void {
        allocator.free(self.rid);
        _ = self.item;
    }
};

pub const DeleteResult = struct { affected: i64 };
pub const ExistsResult = struct { exists: bool };
pub const ListResult = struct {
    items: []json.Value,
    next_cursor: ?[]u8 = null,

    pub fn deinit(self: *ListResult, allocator: Allocator) void {
        allocator.free(self.items);
        if (self.next_cursor) |c| allocator.free(c);
    }
};
pub const QueuePushResult = struct {
    affected: i64,
    rid: ?[]u8 = null,

    pub fn deinit(self: *QueuePushResult, allocator: Allocator) void {
        if (self.rid) |r| allocator.free(r);
    }
};

// --- Helper bundle ----------------------------------------------------------

pub const Helpers = struct {
    q: Querier,
    allocator: Allocator,

    pub fn init(allocator: Allocator, q: Querier) Helpers {
        return .{ .q = q, .allocator = allocator };
    }

    pub fn documents(self: Helpers) DocumentClient {
        return .{ .q = self.q, .allocator = self.allocator };
    }

    pub fn kv(self: Helpers, collection_opt: ?[]const u8) KvClient {
        return .{
            .q = self.q,
            .allocator = self.allocator,
            .collection = collection_opt orelse "kv_default",
        };
    }

    pub fn queue(self: Helpers) QueueClient {
        return .{ .q = self.q, .allocator = self.allocator };
    }
};

// --- Documents --------------------------------------------------------------

pub const DocumentClient = struct {
    q: Querier,
    allocator: Allocator,

    pub const ListOptions = struct {
        limit: i32 = 0,
        order_by: ?[]const u8 = null,
        filter: ?[]const u8 = null,
    };

    pub fn insert(self: DocumentClient, collection: []const u8, document: json.Value) !InsertResult {
        try self.ensureCollection(collection);
        const lit = try jsonInlineLiteral(self.allocator, document);
        defer self.allocator.free(lit);
        const id_path = try identifierPath(self.allocator, collection);
        defer self.allocator.free(id_path);
        const sql = try std.fmt.allocPrint(self.allocator,
            "INSERT INTO {s} DOCUMENT VALUES ({s}) RETURNING *",
            .{ id_path, lit });
        defer self.allocator.free(sql);
        const body = try self.q.query(self.allocator, sql, &.{});
        defer self.allocator.free(body);
        const parsed = try firstRow(self.allocator, body);
        const row = parsed.row orelse return HelperError.InvalidResponse;
        const rid_v = row.object.get("rid") orelse return HelperError.InvalidResponse;
        const rid = try ridString(self.allocator, rid_v) orelse return HelperError.InvalidResponse;
        var affected = parsed.affected;
        if (affected == 0) affected = 1;
        return InsertResult{ .affected = affected, .rid = rid, .item = row };
    }

    pub fn get(self: DocumentClient, collection: []const u8, rid: []const u8) !json.Value {
        const id_path = try identifierPath(self.allocator, collection);
        defer self.allocator.free(id_path);
        const sql = try std.fmt.allocPrint(self.allocator,
            "SELECT * FROM {s} WHERE rid = $1 LIMIT 1", .{id_path});
        defer self.allocator.free(sql);
        const params = [_][]const u8{rid};
        const body = try self.q.query(self.allocator, sql, &params);
        defer self.allocator.free(body);
        const parsed = try firstRow(self.allocator, body);
        return parsed.row orelse HelperError.NotFound;
    }

    pub fn list(self: DocumentClient, collection: []const u8, opts: ListOptions) !ListResult {
        const limit = try normalizeLimit(opts.limit);
        const order = if (opts.order_by) |o| o else "rid ASC";
        const id_path = try identifierPath(self.allocator, collection);
        defer self.allocator.free(id_path);
        const sql = if (opts.filter) |f|
            try std.fmt.allocPrint(self.allocator,
                "SELECT * FROM {s} WHERE {s} ORDER BY {s} LIMIT {d}",
                .{ id_path, f, order, limit })
        else
            try std.fmt.allocPrint(self.allocator,
                "SELECT * FROM {s} ORDER BY {s} LIMIT {d}",
                .{ id_path, order, limit });
        defer self.allocator.free(sql);
        const body = try self.q.query(self.allocator, sql, &.{});
        defer self.allocator.free(body);
        return ListResult{ .items = try allRows(self.allocator, body) };
    }

    pub fn patch(self: DocumentClient, collection: []const u8, rid: []const u8, patch_obj: json.ObjectMap) !json.Value {
        if (patch_obj.count() == 0) return self.get(collection, rid);
        var parts = std.ArrayList(u8).init(self.allocator);
        defer parts.deinit();
        var it = patch_obj.iterator();
        var first = true;
        while (it.next()) |entry| {
            if (std.mem.indexOfScalar(u8, entry.key_ptr.*, '/') != null) {
                return HelperError.InvalidArgument;
            }
            if (!first) try parts.appendSlice(", ");
            first = false;
            const id = try identifier(self.allocator, entry.key_ptr.*);
            defer self.allocator.free(id);
            const lit = try valueLiteral(self.allocator, entry.value_ptr.*);
            defer self.allocator.free(lit);
            try parts.writer().print("{s} = {s}", .{ id, lit });
        }
        const id_path = try identifierPath(self.allocator, collection);
        defer self.allocator.free(id_path);
        const sql = try std.fmt.allocPrint(self.allocator,
            "UPDATE {s} SET {s} WHERE rid = $1 RETURNING *",
            .{ id_path, parts.items });
        defer self.allocator.free(sql);
        const params = [_][]const u8{rid};
        const body = try self.q.query(self.allocator, sql, &params);
        defer self.allocator.free(body);
        const parsed = try firstRow(self.allocator, body);
        return parsed.row orelse HelperError.NotFound;
    }

    pub fn delete(self: DocumentClient, collection: []const u8, rid: []const u8) !DeleteResult {
        const id_path = try identifierPath(self.allocator, collection);
        defer self.allocator.free(id_path);
        const sql = try std.fmt.allocPrint(self.allocator,
            "DELETE FROM {s} WHERE rid = $1", .{id_path});
        defer self.allocator.free(sql);
        const params = [_][]const u8{rid};
        const body = try self.q.query(self.allocator, sql, &params);
        defer self.allocator.free(body);
        return DeleteResult{ .affected = try affectedFromBody(self.allocator, body) };
    }

    fn ensureCollection(self: DocumentClient, collection: []const u8) !void {
        const id_path = try identifierPath(self.allocator, collection);
        defer self.allocator.free(id_path);
        const sql = try std.fmt.allocPrint(self.allocator, "CREATE DOCUMENT {s}", .{id_path});
        defer self.allocator.free(sql);
        const body = self.q.query(self.allocator, sql, &.{}) catch |err| {
            // "already exists" is the only non-fatal error; we cannot inspect
            // the error string from a plain anyerror, so callers are expected
            // to surface a typed "already exists" via the body. The Go driver
            // matches on err.Error() — Zig leaves recovery to the transport.
            // Anything else propagates.
            if (err == error.AlreadyExists) return;
            return err;
        };
        self.allocator.free(body);
    }
};

// --- KV ---------------------------------------------------------------------

pub const KvClient = struct {
    q: Querier,
    allocator: Allocator,
    collection: []const u8,

    pub const SetOptions = struct {
        collection: ?[]const u8 = null,
        tags: ?[]const []const u8 = null,
        expire_ms: i64 = 0,
    };

    pub const ListOpts = struct {
        collection: ?[]const u8 = null,
        limit: i32 = 0,
        prefix: ?[]const u8 = null,
    };

    pub fn set(self: KvClient, key: []const u8, value: json.Value, opts: SetOptions) !void {
        return self.put(key, value, opts);
    }

    pub fn put(self: KvClient, key: []const u8, value: json.Value, opts: SetOptions) !void {
        const coll = opts.collection orelse self.collection;
        const lit = try kvValueLiteral(self.allocator, value);
        defer self.allocator.free(lit);
        const path = try kvPath(self.allocator, coll, key);
        defer self.allocator.free(path);
        var buf = std.ArrayList(u8).init(self.allocator);
        defer buf.deinit();
        try buf.writer().print("KV PUT {s} = {s}", .{ path, lit });
        if (opts.expire_ms > 0) {
            try buf.writer().print(" EXPIRE {d} ms", .{opts.expire_ms});
        }
        if (opts.tags) |tags| {
            if (tags.len > 0) {
                try buf.appendSlice(" TAGS [");
                for (tags, 0..) |t, i| {
                    if (i > 0) try buf.appendSlice(", ");
                    const tag_lit = try kvTagLiteral(self.allocator, t);
                    defer self.allocator.free(tag_lit);
                    try buf.appendSlice(tag_lit);
                }
                try buf.append(']');
            }
        }
        const body = try self.q.query(self.allocator, buf.items, &.{});
        self.allocator.free(body);
    }

    pub fn get(self: KvClient, key: []const u8, collection_opt: ?[]const u8) !?json.Value {
        const coll = collection_opt orelse self.collection;
        const path = try kvPath(self.allocator, coll, key);
        defer self.allocator.free(path);
        const sql = try std.fmt.allocPrint(self.allocator, "KV GET {s}", .{path});
        defer self.allocator.free(sql);
        const body = try self.q.query(self.allocator, sql, &.{});
        defer self.allocator.free(body);
        const parsed = try firstRow(self.allocator, body);
        const row = parsed.row orelse return null;
        return row.object.get("value");
    }

    pub fn exists(self: KvClient, key: []const u8, collection_opt: ?[]const u8) !ExistsResult {
        const v = try self.get(key, collection_opt);
        const present = if (v) |val| val != .null else false;
        return ExistsResult{ .exists = present };
    }

    pub fn delete(self: KvClient, key: []const u8, collection_opt: ?[]const u8) !DeleteResult {
        const coll = collection_opt orelse self.collection;
        const path = try kvPath(self.allocator, coll, key);
        defer self.allocator.free(path);
        const sql = try std.fmt.allocPrint(self.allocator, "KV DELETE {s}", .{path});
        defer self.allocator.free(sql);
        const body = try self.q.query(self.allocator, sql, &.{});
        defer self.allocator.free(body);
        return DeleteResult{ .affected = try affectedFromBody(self.allocator, body) };
    }

    pub fn list(self: KvClient, opts: ListOpts) !ListResult {
        const coll = opts.collection orelse self.collection;
        const limit = try normalizeLimit(opts.limit);
        const id = try identifier(self.allocator, coll);
        defer self.allocator.free(id);
        const sql = try std.fmt.allocPrint(self.allocator,
            "SELECT key, value FROM {s} ORDER BY key ASC LIMIT {d}",
            .{ id, limit });
        defer self.allocator.free(sql);
        const body = try self.q.query(self.allocator, sql, &.{});
        defer self.allocator.free(body);
        var rows = try allRows(self.allocator, body);
        if (opts.prefix) |prefix| {
            var filtered = std.ArrayList(json.Value).init(self.allocator);
            defer filtered.deinit();
            for (rows) |row| {
                const k_opt = row.object.get("key");
                const matches = if (k_opt) |k|
                    k == .string and std.mem.startsWith(u8, k.string, prefix)
                else
                    false;
                if (matches) {
                    try filtered.append(row);
                } else {
                    freeClonedJson(self.allocator, row);
                }
            }
            self.allocator.free(rows);
            rows = try filtered.toOwnedSlice();
        }
        return ListResult{ .items = rows };
    }
};

// --- Queue ------------------------------------------------------------------

pub const QueueClient = struct {
    q: Querier,
    allocator: Allocator,

    pub const PushOptions = struct { priority: ?i32 = null };

    pub fn push(self: QueueClient, queue_name: []const u8, value: json.Value, opts: PushOptions) !QueuePushResult {
        try assertIdentifier(queue_name, "queue name");
        const lit = try queueValueLiteral(self.allocator, value);
        defer self.allocator.free(lit);
        const id = try identifier(self.allocator, queue_name);
        defer self.allocator.free(id);
        const sql = if (opts.priority) |p|
            try std.fmt.allocPrint(self.allocator, "QUEUE PUSH {s} {s} PRIORITY {d}", .{ id, lit, p })
        else
            try std.fmt.allocPrint(self.allocator, "QUEUE PUSH {s} {s}", .{ id, lit });
        defer self.allocator.free(sql);
        const body = try self.q.query(self.allocator, sql, &.{});
        defer self.allocator.free(body);
        var affected = try affectedFromBody(self.allocator, body);
        if (affected == 0) affected = 1;
        var res = QueuePushResult{ .affected = affected, .rid = null };
        const parsed = try firstRow(self.allocator, body);
        if (parsed.row) |row| {
            if (row.object.get("rid")) |rid_v| {
                if (try ridString(self.allocator, rid_v)) |rid| res.rid = rid;
            }
        }
        return res;
    }

    pub fn pop(self: QueueClient, queue_name: []const u8, count: ?i32) ![]json.Value {
        return self.fetch("POP", queue_name, count);
    }

    pub fn peek(self: QueueClient, queue_name: []const u8, count: ?i32) ![]json.Value {
        return self.fetch("PEEK", queue_name, count);
    }

    fn fetch(self: QueueClient, verb: []const u8, queue_name: []const u8, count: ?i32) ![]json.Value {
        try assertIdentifier(queue_name, "queue name");
        if (count) |c| {
            if (c < 0) return HelperError.InvalidArgument;
        }
        const id = try identifier(self.allocator, queue_name);
        defer self.allocator.free(id);
        const sql = if (count) |c|
            try std.fmt.allocPrint(self.allocator, "QUEUE {s} {s} COUNT {d}", .{ verb, id, c })
        else
            try std.fmt.allocPrint(self.allocator, "QUEUE {s} {s}", .{ verb, id });
        defer self.allocator.free(sql);
        const body = try self.q.query(self.allocator, sql, &.{});
        defer self.allocator.free(body);
        const rows = try allRows(self.allocator, body);
        defer self.allocator.free(rows);
        var out = try self.allocator.alloc(json.Value, rows.len);
        for (rows, 0..) |row, i| {
            out[i] = row.object.get("payload") orelse json.Value{ .null = {} };
        }
        return out;
    }

    pub fn len(self: QueueClient, queue_name: []const u8) !i64 {
        try assertIdentifier(queue_name, "queue name");
        const id = try identifier(self.allocator, queue_name);
        defer self.allocator.free(id);
        const sql = try std.fmt.allocPrint(self.allocator, "QUEUE LEN {s}", .{id});
        defer self.allocator.free(sql);
        const body = try self.q.query(self.allocator, sql, &.{});
        defer self.allocator.free(body);
        const parsed = try firstRow(self.allocator, body);
        const row = parsed.row orelse return 0;
        const v = row.object.get("len") orelse return 0;
        return switch (v) {
            .integer => |n| n,
            .float => |f| @as(i64, @intFromFloat(f)),
            else => 0,
        };
    }

    pub fn purge(self: QueueClient, queue_name: []const u8) !DeleteResult {
        try assertIdentifier(queue_name, "queue name");
        const id = try identifier(self.allocator, queue_name);
        defer self.allocator.free(id);
        const sql = try std.fmt.allocPrint(self.allocator, "QUEUE PURGE {s}", .{id});
        defer self.allocator.free(sql);
        const body = try self.q.query(self.allocator, sql, &.{});
        defer self.allocator.free(body);
        return DeleteResult{ .affected = try affectedFromBody(self.allocator, body) };
    }

    pub const ReadWaitOptions = struct {
        group: ?[]const u8 = null,
        count: ?i32 = null,
    };

    /// Live `QUEUE READ … WAIT <ms>` helper (PRD #718 / #725). Blocks
    /// until a message is available for `consumer` on `queue_name`, the
    /// `wait_ms` budget elapses, or the server cancels. Timeout returns
    /// an empty slice — same shape as an empty `pop`. `wait_ms` is
    /// required (non-negative); there is no infinite-wait default.
    pub fn readWait(
        self: QueueClient,
        queue_name: []const u8,
        consumer: []const u8,
        wait_ms: i64,
        opts: ReadWaitOptions,
    ) ![]json.Value {
        try assertIdentifier(queue_name, "queue name");
        try assertIdentifier(consumer, "consumer name");
        if (wait_ms < 0) return HelperError.InvalidArgument;
        if (opts.count) |c| {
            if (c < 0) return HelperError.InvalidArgument;
        }
        const qid = try identifier(self.allocator, queue_name);
        defer self.allocator.free(qid);
        const cid = try identifier(self.allocator, consumer);
        defer self.allocator.free(cid);

        var group_buf: []u8 = try self.allocator.alloc(u8, 0);
        defer self.allocator.free(group_buf);
        if (opts.group) |g| {
            if (g.len > 0) {
                try assertIdentifier(g, "group name");
                const gid = try identifier(self.allocator, g);
                defer self.allocator.free(gid);
                self.allocator.free(group_buf);
                group_buf = try std.fmt.allocPrint(self.allocator, " GROUP {s}", .{gid});
            }
        }
        var count_buf: []u8 = try self.allocator.alloc(u8, 0);
        defer self.allocator.free(count_buf);
        if (opts.count) |c| {
            self.allocator.free(count_buf);
            count_buf = try std.fmt.allocPrint(self.allocator, " COUNT {d}", .{c});
        }
        const sql = try std.fmt.allocPrint(
            self.allocator,
            "QUEUE READ {s}{s} CONSUMER {s}{s} WAIT {d}ms",
            .{ qid, group_buf, cid, count_buf, wait_ms },
        );
        defer self.allocator.free(sql);
        const body = try self.q.query(self.allocator, sql, &.{});
        defer self.allocator.free(body);
        const rows = try allRows(self.allocator, body);
        defer self.allocator.free(rows);
        var out = try self.allocator.alloc(json.Value, rows.len);
        for (rows, 0..) |row, i| {
            out[i] = row.object.get("payload") orelse json.Value{ .null = {} };
        }
        return out;
    }
};

// --- pure SQL helpers (unit-testable) ---------------------------------------

pub fn isIdentChar(c: u8) bool {
    return (c >= 'a' and c <= 'z') or (c >= 'A' and c <= 'Z')
        or (c >= '0' and c <= '9') or c == '_';
}

pub fn allIdentChars(s: []const u8) bool {
    if (s.len == 0) return false;
    for (s) |c| if (!isIdentChar(c)) return false;
    return true;
}

pub fn assertIdentifier(value: []const u8, label: []const u8) !void {
    _ = label;
    if (value.len == 0 or !allIdentChars(value)) return HelperError.InvalidArgument;
}

pub fn normalizeLimit(value: i32) !i32 {
    if (value == 0) return 100;
    if (value < 0) return HelperError.InvalidArgument;
    return value;
}

/// Caller owns the returned buffer.
pub fn identifier(allocator: Allocator, value: []const u8) ![]u8 {
    if (allIdentChars(value)) return try allocator.dupe(u8, value);
    return try quoteWith(allocator, value, '"');
}

pub fn identifierPath(allocator: Allocator, value: []const u8) ![]u8 {
    if (std.mem.indexOfScalar(u8, value, '.') == null) return identifier(allocator, value);
    var buf = std.ArrayList(u8).init(allocator);
    defer buf.deinit();
    var it = std.mem.splitScalar(u8, value, '.');
    var first = true;
    while (it.next()) |part| {
        if (!first) try buf.append('.');
        first = false;
        const id = try identifier(allocator, part);
        defer allocator.free(id);
        try buf.appendSlice(id);
    }
    return try buf.toOwnedSlice();
}

pub fn kvPath(allocator: Allocator, collection: []const u8, key: []const u8) ![]u8 {
    for (collection) |c| {
        if (!isIdentChar(c)) return HelperError.InvalidArgument;
    }
    const seg = try kvKeySegment(allocator, key);
    defer allocator.free(seg);
    return try std.fmt.allocPrint(allocator, "{s}.{s}", .{ collection, seg });
}

pub fn kvKeySegment(allocator: Allocator, value: []const u8) ![]u8 {
    if (allIdentChars(value)) return try allocator.dupe(u8, value);
    return try quoteWith(allocator, value, '\'');
}

pub fn kvTagLiteral(allocator: Allocator, tag: []const u8) ![]u8 {
    return try quoteWith(allocator, tag, '\'');
}

pub fn kvValueLiteral(allocator: Allocator, value: json.Value) ![]u8 {
    return switch (value) {
        .null => try allocator.dupe(u8, "NULL"),
        .bool => |b| try allocator.dupe(u8, if (b) "true" else "false"),
        .integer => |n| try std.fmt.allocPrint(allocator, "{d}", .{n}),
        .float => |f| try std.fmt.allocPrint(allocator, "{d}", .{f}),
        .number_string => |s| try allocator.dupe(u8, s),
        .string => |s| try quoteWith(allocator, s, '\''),
        .array, .object => blk: {
            const enc = try jsonEncode(allocator, value);
            defer allocator.free(enc);
            break :blk try quoteWith(allocator, enc, '\'');
        },
    };
}

pub fn queueValueLiteral(allocator: Allocator, value: json.Value) ![]u8 {
    return switch (value) {
        .null => try allocator.dupe(u8, "NULL"),
        .bool => |b| try allocator.dupe(u8, if (b) "true" else "false"),
        .integer => |n| try std.fmt.allocPrint(allocator, "{d}", .{n}),
        .float => |f| try std.fmt.allocPrint(allocator, "{d}", .{f}),
        .number_string => |s| try allocator.dupe(u8, s),
        .string => |s| try quoteWith(allocator, s, '\''),
        .array, .object => try jsonEncode(allocator, value),
    };
}

pub fn valueLiteral(allocator: Allocator, value: json.Value) ![]u8 {
    return kvValueLiteral(allocator, value);
}

// jsonInlineLiteral returns the raw JSON encoding of value with no
// surrounding quotes and no SQL escaping, for use where the RQL lexer
// parses an inline JSON literal directly (e.g. DOCUMENT VALUES (...)).
pub fn jsonInlineLiteral(allocator: Allocator, value: json.Value) ![]u8 {
    return try jsonEncode(allocator, value);
}

fn quoteWith(allocator: Allocator, value: []const u8, quote: u8) ![]u8 {
    var buf = std.ArrayList(u8).init(allocator);
    defer buf.deinit();
    try buf.append(quote);
    for (value) |c| {
        if (c == quote) try buf.append(quote);
        try buf.append(c);
    }
    try buf.append(quote);
    return try buf.toOwnedSlice();
}

fn jsonEncode(allocator: Allocator, value: json.Value) ![]u8 {
    var buf = std.ArrayList(u8).init(allocator);
    defer buf.deinit();
    try std.json.stringify(value, .{}, buf.writer());
    return try buf.toOwnedSlice();
}

// --- response parsing -------------------------------------------------------

const ParsedFirst = struct { row: ?json.Value, affected: i64 };

pub fn decodeBody(allocator: Allocator, body: []const u8) !?json.Parsed(json.Value) {
    if (body.len == 0) return null;
    return std.json.parseFromSlice(json.Value, allocator, body, .{}) catch null;
}

fn affectedFromMap(obj: json.ObjectMap) i64 {
    const v = obj.get("affected") orelse return 0;
    return switch (v) {
        .integer => |n| n,
        .float => |f| @as(i64, @intFromFloat(f)),
        else => 0,
    };
}

pub fn firstRow(allocator: Allocator, body: []const u8) !ParsedFirst {
    var parsed = (try decodeBody(allocator, body)) orelse return .{ .row = null, .affected = 0 };
    defer parsed.deinit();
    const root = parsed.value;
    if (root != .object) return .{ .row = null, .affected = 0 };
    var affected = affectedFromMap(root.object);
    var rows_opt: ?json.Array = null;
    if (root.object.get("rows")) |r| {
        if (r == .array) rows_opt = r.array;
    }
    if (rows_opt == null or rows_opt.?.items.len == 0) {
        if (root.object.get("result")) |nested| {
            if (nested == .object) {
                if (nested.object.get("rows")) |r| {
                    if (r == .array) rows_opt = r.array;
                }
                if (affected == 0) affected = affectedFromMap(nested.object);
            }
        }
    }
    if (rows_opt == null or rows_opt.?.items.len == 0) {
        return .{ .row = null, .affected = affected };
    }
    const first = rows_opt.?.items[0];
    if (first != .object) return .{ .row = null, .affected = affected };
    // Deep-clone the row so it survives `parsed.deinit()`.
    const cloned = try cloneJson(allocator, first);
    return .{ .row = cloned, .affected = affected };
}

pub fn allRows(allocator: Allocator, body: []const u8) ![]json.Value {
    var parsed = (try decodeBody(allocator, body)) orelse return &[_]json.Value{};
    defer parsed.deinit();
    const root = parsed.value;
    if (root != .object) return &[_]json.Value{};
    var rows_opt: ?json.Array = null;
    if (root.object.get("rows")) |r| {
        if (r == .array) rows_opt = r.array;
    }
    if (rows_opt == null) {
        if (root.object.get("result")) |nested| {
            if (nested == .object) {
                if (nested.object.get("rows")) |r| {
                    if (r == .array) rows_opt = r.array;
                }
            }
        }
    }
    if (rows_opt == null) return &[_]json.Value{};
    var out = std.ArrayList(json.Value).init(allocator);
    defer out.deinit();
    for (rows_opt.?.items) |row| {
        if (row == .object) try out.append(try cloneJson(allocator, row));
    }
    return try out.toOwnedSlice();
}

pub fn affectedFromBody(allocator: Allocator, body: []const u8) !i64 {
    var parsed = (try decodeBody(allocator, body)) orelse return 0;
    defer parsed.deinit();
    const root = parsed.value;
    if (root != .object) return 0;
    const direct = affectedFromMap(root.object);
    if (direct > 0) return direct;
    if (root.object.get("result")) |nested| {
        if (nested == .object) return affectedFromMap(nested.object);
    }
    return 0;
}

pub fn ridString(allocator: Allocator, value: json.Value) !?[]u8 {
    return switch (value) {
        .string => |s| try allocator.dupe(u8, s),
        .integer => |n| try std.fmt.allocPrint(allocator, "{d}", .{n}),
        .float => |f| try std.fmt.allocPrint(allocator, "{d}", .{f}),
        .number_string => |s| try allocator.dupe(u8, s),
        else => null,
    };
}

/// Free a json.Value previously produced by `cloneJson`, releasing every
/// nested string/array/object back to the allocator that produced it.
pub fn freeClonedJson(allocator: Allocator, value: json.Value) void {
    switch (value) {
        .null, .bool, .integer, .float => {},
        .number_string, .string => |s| allocator.free(s),
        .array => |arr| {
            var a = arr;
            for (a.items) |v| freeClonedJson(allocator, v);
            a.deinit();
        },
        .object => |obj| {
            var o = obj;
            var it = o.iterator();
            while (it.next()) |entry| {
                allocator.free(entry.key_ptr.*);
                freeClonedJson(allocator, entry.value_ptr.*);
            }
            o.deinit();
        },
    }
}

/// Deep-clone a json.Value, transferring ownership of every nested string/
/// array/object to `allocator`. Needed so callers can keep rows alive after
/// the originating `Parsed(...)` is deinit'd.
fn cloneJson(allocator: Allocator, value: json.Value) anyerror!json.Value {
    return switch (value) {
        .null => .{ .null = {} },
        .bool => |b| .{ .bool = b },
        .integer => |n| .{ .integer = n },
        .float => |f| .{ .float = f },
        .number_string => |s| .{ .number_string = try allocator.dupe(u8, s) },
        .string => |s| .{ .string = try allocator.dupe(u8, s) },
        .array => |arr| blk: {
            var out = json.Array.init(allocator);
            try out.ensureTotalCapacity(arr.items.len);
            for (arr.items) |el| out.appendAssumeCapacity(try cloneJson(allocator, el));
            break :blk .{ .array = out };
        },
        .object => |obj| blk: {
            var out = json.ObjectMap.init(allocator);
            try out.ensureTotalCapacity(@intCast(obj.count()));
            var it = obj.iterator();
            while (it.next()) |entry| {
                const k = try allocator.dupe(u8, entry.key_ptr.*);
                const v = try cloneJson(allocator, entry.value_ptr.*);
                try out.put(k, v);
            }
            break :blk .{ .object = out };
        },
    };
}
