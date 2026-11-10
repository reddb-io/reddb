const std = @import("std");
const Allocator = std.mem.Allocator;

// Communication authority: reddb-wire. Shared conformance fixtures pin these values.
pub const feature: u32 = 2;
pub const kind: u8 = 0x32;
pub const Endpoint = struct { addr: []const u8, region: []const u8 = "" };
pub const Replica = struct {
    addr: []const u8,
    region: []const u8 = "",
    healthy: bool,
    lag_ms: u32,
    last_applied_lsn: u64,
    rebootstrapping: bool = false,
};
pub const Topology = struct {
    arena: std.heap.ArenaAllocator,
    epoch: u64,
    primary: Endpoint,
    replicas: []Replica,
    pub fn deinit(self: *Topology) void {
        self.arena.deinit();
    }
};
const Reader = struct {
    data: []const u8,
    allocator: Allocator,
    offset: usize = 0,
    fn integer(self: *Reader, comptime T: type) !T {
        const size = @sizeOf(T);
        if (size > self.data.len - self.offset) return error.InvalidTopology;
        const value = std.mem.readInt(T, self.data[self.offset..][0..size], .little);
        self.offset += size;
        return value;
    }
    fn text(self: *Reader) ![]const u8 {
        const size = try self.integer(u32);
        if (size > 4096 or size > self.data.len - self.offset) return error.InvalidTopology;
        const value = self.data[self.offset..][0..size];
        self.offset += size;
        if (!std.unicode.utf8ValidateSlice(value)) return error.InvalidTopology;
        return self.allocator.dupe(u8, value);
    }
};
pub fn decode(allocator: Allocator, bytes: []const u8) !?Topology {
    if (bytes.len < 5) return error.InvalidTopology;
    const length = std.mem.readInt(u32, bytes[1..5], .little);
    if (length > bytes.len - 5) return error.InvalidTopology;
    if (bytes[0] != 1) return null;
    var arena = std.heap.ArenaAllocator.init(allocator);
    errdefer arena.deinit();
    var reader = Reader{ .data = bytes[5..][0..length], .allocator = arena.allocator() };
    const epoch = try reader.integer(u64);
    const primary = Endpoint{ .addr = try reader.text(), .region = try reader.text() };
    const count = try reader.integer(u32);
    if (count > 256) return error.InvalidTopology;
    const replicas = try arena.allocator().alloc(Replica, count);
    for (replicas) |*replica| {
        replica.* = .{ .addr = try reader.text(), .region = try reader.text(), .healthy = (try reader.integer(u8)) != 0, .lag_ms = try reader.integer(u32), .last_applied_lsn = try reader.integer(u64) };
    }
    if (reader.data.len - reader.offset >= count) {
        for (replicas) |*replica| replica.rebootstrapping = (try reader.integer(u8)) != 0;
    }
    return Topology{ .arena = arena, .epoch = epoch, .primary = primary, .replicas = replicas };
}
fn field(value: std.json.Value, key: []const u8) !std.json.Value {
    if (value != .object) return error.InvalidTopology;
    return value.object.get(key) orelse error.InvalidTopology;
}
fn string(value: std.json.Value) ![]const u8 {
    if (value != .string or value.string.len > 4096) return error.InvalidTopology;
    return value.string;
}
fn unsigned(value: std.json.Value) !u64 {
    return switch (value) {
        .string, .number_string => |text| std.fmt.parseInt(u64, text, 10),
        .integer => |number| std.math.cast(u64, number) orelse error.InvalidTopology,
        else => error.InvalidTopology,
    };
}
pub fn decodeJson(allocator: Allocator, bytes: []const u8) !?Topology {
    if (bytes.len > 16 * 1024 * 1024) return error.InvalidTopology;
    // Bound nesting before the standard JSON parser expands an untrusted body.
    var quoted = false;
    var escaped = false;
    var depth: usize = 0;
    for (bytes) |byte| {
        if (quoted) {
            if (escaped) {
                escaped = false;
            } else if (byte == '\\') {
                escaped = true;
            } else if (byte == '"') {
                quoted = false;
            }
        } else if (byte == '"') {
            quoted = true;
        } else if (byte == '{' or byte == '[') {
            depth += 1;
            if (depth > 64) return error.InvalidTopology;
        } else if (byte == '}' or byte == ']') {
            if (depth == 0) return error.InvalidTopology;
            depth -= 1;
        }
    }
    if (quoted or depth != 0) return error.InvalidTopology;
    var arena = std.heap.ArenaAllocator.init(allocator);
    errdefer arena.deinit();
    const value = try std.json.parseFromSliceLeaky(std.json.Value, arena.allocator(), bytes, .{ .allocate = .alloc_always });
    if ((try unsigned(try field(value, "version"))) != 1) {
        arena.deinit();
        return null;
    }
    const primary = try field(value, "primary");
    const list = try field(value, "replicas");
    if (list != .array or list.array.items.len > 256) return error.InvalidTopology;
    const replicas = try arena.allocator().alloc(Replica, list.array.items.len);
    for (list.array.items, replicas) |node, *replica| {
        const healthy = try field(node, "healthy");
        if (healthy != .bool) return error.InvalidTopology;
        const rebuilding = if (node == .object) node.object.get("rebootstrapping") else null;
        replica.* = .{ .addr = try string(try field(node, "addr")), .region = if (node.object.get("region")) |region| try string(region) else "", .healthy = healthy.bool, .lag_ms = std.math.cast(u32, try unsigned(try field(node, "lag_ms"))) orelse return error.InvalidTopology, .last_applied_lsn = try unsigned(try field(node, "last_applied_lsn")), .rebootstrapping = if (rebuilding) |flag| flag == .bool and flag.bool else false };
    }
    return Topology{ .arena = arena, .epoch = try unsigned(try field(value, "epoch")), .primary = .{ .addr = try string(try field(primary, "addr")), .region = if (primary == .object) if (primary.object.get("region")) |region| try string(region) else "" else "" }, .replicas = replicas };
}
pub fn address(allocator: Allocator, value: []const u8, protocol: []const u8) !?[]u8 {
    if (value.len == 0 or value.len > 4096 or std.mem.indexOfAny(u8, value, "@?#") != null) return null;
    const text = if (std.mem.indexOf(u8, value, "://") == null)
        try std.fmt.allocPrint(allocator, "{s}://{s}", .{ protocol, value })
    else
        try allocator.dupe(u8, value);
    errdefer allocator.free(text);
    const uri = std.Uri.parse(text) catch {
        allocator.free(text);
        return null;
    };
    if (!std.mem.eql(u8, uri.scheme, protocol) or uri.host == null) {
        allocator.free(text);
        return null;
    }
    const trimmed = std.mem.trimRight(u8, text, "/");
    const result = try allocator.dupe(u8, trimmed);
    allocator.free(text);
    return result;
}

pub const SqlState = enum { primary, begin, end, session };
pub const SqlPolicy = struct { state: SqlState = .primary, read: bool = false };
fn has(words: []const []const u8, token: []const u8) bool {
    for (words) |word| if (std.mem.eql(u8, word, token)) return true;
    return false;
}
pub fn sqlPolicy(allocator: Allocator, sql: []const u8) !SqlPolicy {
    if (sql.len > 1048576) return .{ .state = .session };
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    var words = std.ArrayList([]const u8).init(arena.allocator());
    var index: usize = 0;
    while (index < sql.len) {
        const byte = sql[index];
        if (std.ascii.isWhitespace(byte)) {
            index += 1;
            continue;
        }
        if (std.mem.startsWith(u8, sql[index..], "--")) {
            index += 2;
            while (index < sql.len and sql[index] != '\n') index += 1;
            continue;
        }
        if (std.mem.startsWith(u8, sql[index..], "/*")) {
            var depth: usize = 1;
            index += 2;
            while (index < sql.len and depth > 0) {
                if (std.mem.startsWith(u8, sql[index..], "/*")) {
                    depth += 1;
                    index += 2;
                } else if (std.mem.startsWith(u8, sql[index..], "*/")) {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
                if (depth > 64) return .{ .state = .session };
            }
            if (depth != 0) return .{ .state = .session };
            continue;
        }
        if (byte == '\'' or byte == '"' or byte == '`') {
            index += 1;
            var closed = false;
            while (index < sql.len) {
                const current = sql[index];
                index += 1;
                if (current == '\\') return .{ .state = .session };
                if (current == byte) {
                    if (index < sql.len and sql[index] == byte) {
                        index += 1;
                        continue;
                    }
                    closed = true;
                    break;
                }
            }
            if (!closed) return .{ .state = .session };
            try words.append(if (byte == '\'') "#value" else "#identifier");
            continue;
        }
        if (byte > 127) return .{};
        const start = index;
        index += 1;
        if (std.ascii.isAlphabetic(byte) or byte == '_') {
            while (index < sql.len and (std.ascii.isAlphanumeric(sql[index]) or sql[index] == '_')) index += 1;
        }
        try words.append(try std.ascii.allocUpperString(arena.allocator(), sql[start..index]));
    }
    if (words.items.len > 0 and std.mem.eql(u8, words.items[words.items.len - 1], ";")) words.shrinkRetainingCapacity(words.items.len - 1);
    const tokens = words.items;
    if (tokens.len == 0 or has(tokens, ";")) return .{ .state = .session };
    const first = tokens[0];
    if (std.mem.eql(u8, first, "BEGIN") or (std.mem.eql(u8, first, "START") and tokens.len > 1 and std.mem.eql(u8, tokens[1], "TRANSACTION"))) return .{ .state = .begin };
    if (has(&.{ "COMMIT", "END", "ROLLBACK" }, first)) {
        if (std.mem.eql(u8, first, "ROLLBACK") and has(tokens, "TO")) return .{};
        return .{ .state = if (has(tokens, "CHAIN") and !has(tokens, "NO")) .begin else .end };
    }
    if (has(&.{ "SET", "RESET", "USE", "PREPARE", "DEALLOCATE", "DECLARE", "LISTEN" }, first)) return .{ .state = .session };
    const forbidden = [_][]const u8{ "INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP", "INTO", "FOR", "LOCK", "CALL", "EXECUTE", "SET", "QUEUE", "COPY", "[", "]" };
    const functions = [_][]const u8{ "COUNT", "SUM", "MIN", "MAX", "AVG", "LOWER", "UPPER", "LENGTH", "COALESCE", "ABS", "ROUND", "CAST", "EXISTS", "IN", "AS", "FROM", "WHERE", "AND", "OR", "NOT", "SELECT" };
    var read = std.mem.eql(u8, first, "SELECT");
    for (tokens, 0..) |token, i| {
        if (has(&forbidden, token)) read = false;
        if (i + 1 < tokens.len and std.mem.eql(u8, tokens[i + 1], "(") and
            (std.mem.eql(u8, token, "#identifier") or std.ascii.isAlphabetic(token[0]) or token[0] == '_') and !has(&functions, token)) read = false;
    }
    return .{ .read = read };
}
