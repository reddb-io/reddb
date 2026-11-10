const std = @import("std");
const reddb = @import("reddb");
const t = std.testing;
fn fixtures(name: []const u8) ![]u8 {
    const path = try std.fmt.allocPrint(t.allocator, "../../testdata/conformance/topology/{s}", .{name});
    defer t.allocator.free(path);
    return std.fs.cwd().readFileAlloc(t.allocator, path, 1024 * 1024);
}
test "shared topology fixtures and exact u64" {
    const contents = try fixtures("manifest.json");
    defer t.allocator.free(contents);
    var parsed = try std.json.parseFromSlice(std.json.Value, t.allocator, contents, .{});
    defer parsed.deinit();
    for (parsed.value.object.get("cases").?.array.items) |case| {
        const hex = case.object.get("hex").?.string;
        const bytes = try t.allocator.alloc(u8, hex.len / 2);
        defer t.allocator.free(bytes);
        _ = try std.fmt.hexToBytes(bytes, hex);
        if (case.object.get("error")) |value| {
            if (value.bool) {
                try t.expectError(error.InvalidTopology, reddb.topology.decode(t.allocator, bytes));
                continue;
            }
        }
        var topology = try reddb.topology.decode(t.allocator, bytes);
        defer if (topology) |*value| value.deinit();
        const expected = case.object.get("expected").?;
        if (expected == .null) {
            try t.expect(topology == null);
            continue;
        }
        try t.expect(topology != null);
        try t.expectEqual(try std.fmt.parseInt(u64, expected.object.get("epoch").?.string, 10), topology.?.epoch);
        try t.expectEqualStrings(expected.object.get("primary").?.object.get("addr").?.string, topology.?.primary.addr);
        const replicas = expected.object.get("replicas").?.array.items;
        try t.expectEqual(replicas.len, topology.?.replicas.len);
        for (replicas, topology.?.replicas) |want, actual| {
            try t.expectEqualStrings(want.object.get("addr").?.string, actual.addr);
            try t.expectEqual(try std.fmt.parseInt(u64, want.object.get("last_applied_lsn").?.string, 10), actual.last_applied_lsn);
            try t.expectEqual(want.object.get("rebootstrapping").?.bool, actual.rebootstrapping);
        }
    }
}
test "shared conservative SQL policy" {
    const contents = try fixtures("sql-policy.json");
    defer t.allocator.free(contents);
    var parsed = try std.json.parseFromSlice(std.json.Value, t.allocator, contents, .{});
    defer parsed.deinit();
    for (parsed.value.array.items) |case| {
        const policy = try reddb.topology.sqlPolicy(t.allocator, case.object.get("sql").?.string);
        try t.expectEqual(case.object.get("replica_read").?.bool, policy.read);
    }
}

const HttpPeer = struct {
    listener: std.net.Server,
    membership: *[]const u8,
    label: []const u8,
    stop: std.atomic.Value(bool) = std.atomic.Value(bool).init(false),
    thread: ?std.Thread = null,
    fn start(membership: *[]const u8, label: []const u8) !*HttpPeer {
        const peer = try t.allocator.create(HttpPeer);
        peer.* = .{ .listener = try (try std.net.Address.parseIp4("127.0.0.1", 0)).listen(.{}), .membership = membership, .label = label };
        peer.thread = try std.Thread.spawn(.{}, run, .{peer});
        return peer;
    }
    fn close(self: *HttpPeer) void {
        self.stop.store(true, .release);
        const wake = std.net.tcpConnectToAddress(self.listener.listen_address) catch unreachable;
        wake.close();
        self.thread.?.join();
        self.listener.deinit();
        t.allocator.destroy(self);
    }
    fn run(self: *HttpPeer) void {
        while (true) {
            const peer = self.listener.accept() catch return;
            defer peer.stream.close();
            if (self.stop.load(.acquire)) return;
            var buffer: [16384]u8 = undefined;
            var length: usize = 0;
            var header_end: usize = 0;
            while (length < buffer.len) {
                const count = peer.stream.read(buffer[length..]) catch break;
                if (count == 0) break;
                length += count;
                if (std.mem.indexOf(u8, buffer[0..length], "\r\n\r\n")) |end| {
                    header_end = end + 4;
                    break;
                }
            }
            if (header_end == 0) continue;
            var body_length: usize = 0;
            var lines = std.mem.splitSequence(u8, buffer[0..header_end], "\r\n");
            while (lines.next()) |line| {
                if (line.len > 15 and std.ascii.eqlIgnoreCase(line[0..15], "content-length:"))
                    body_length = std.fmt.parseInt(usize, std.mem.trim(u8, line[15..], " "), 10) catch 0;
            }
            while (length < header_end + body_length and length < buffer.len) {
                const count = peer.stream.read(buffer[length..]) catch break;
                if (count == 0) break;
                length += count;
            }
            const input = buffer[0..length];
            const body = if (std.mem.indexOf(u8, input, "/v1/topology?") != null) self.membership.* else if (std.mem.startsWith(u8, input, "GET")) "{}" else self.label;
            const response = std.fmt.allocPrint(std.heap.page_allocator, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {d}\r\n\r\n{s}", .{ body.len, body }) catch return;
            defer std.heap.page_allocator.free(response);
            peer.stream.writeAll(response) catch {};
        }
    }
};
test "HTTP facade discovers and pins actual connections" {
    var membership: []const u8 = "";
    const writer = try HttpPeer.start(&membership, "{\"rows\":[{\"node\":\"writer\"}]}");
    defer writer.close();
    const reader = try HttpPeer.start(&membership, "{\"rows\":[{\"node\":\"reader\"}]}");
    defer reader.close();
    const primary = try std.fmt.allocPrint(t.allocator, "http://127.0.0.1:{d}", .{writer.listener.listen_address.getPort()});
    defer t.allocator.free(primary);
    const replica = try std.fmt.allocPrint(t.allocator, "http://127.0.0.1:{d}", .{reader.listener.listen_address.getPort()});
    defer t.allocator.free(replica);
    membership = try std.json.stringifyAlloc(t.allocator, .{ .version = 1, .epoch = "1", .primary = .{ .addr = primary, .region = "" }, .replicas = .{.{ .addr = replica, .region = "", .healthy = true, .lag_ms = 10, .last_applied_lsn = "0", .rebootstrapping = false }} }, .{});
    defer t.allocator.free(membership);
    const client = try reddb.connect(t.allocator, primary, null);
    defer {
        client.deinit();
        t.allocator.destroy(client);
    }
    const generic = try client.query("SELECT 1", .{});
    defer t.allocator.free(generic);
    try t.expect(std.mem.indexOf(u8, generic, "writer") != null);
    const local = try client.queryWithOptions("SELECT 1", .{ .consistency = .local }, .{});
    defer t.allocator.free(local);
    try t.expect(std.mem.indexOf(u8, local, "reader") != null);
    const begin = try client.query("BEGIN", .{});
    defer t.allocator.free(begin);
    const pinned = try client.queryWithOptions("SELECT 1", .{ .consistency = .local }, .{});
    defer t.allocator.free(pinned);
    try t.expect(std.mem.indexOf(u8, pinned, "writer") != null);
}
