const std = @import("std");
const windows = @import("builtin").os.tag == .windows;

// Winsock recv/send honor SO_RCVTIMEO; std.net.Stream uses ReadFile on Windows.
pub const Socket = struct {
    pub const ReadError = std.posix.RecvFromError || error{ ConnectionTimedOut, SocketNotConnected, ConnectionResetByPeer };
    pub const WriteError = std.posix.SendError || error{ ConnectionTimedOut, SocketNotConnected, BrokenPipe };
    handle: std.posix.socket_t,
    timeout_ms: u32 = 30000,
    fn wait(self: Socket, events: i16) !void {
        if (windows) {
            var descriptor = std.os.windows.ws2_32.pollfd{ .fd = self.handle, .events = events, .revents = 0 };
            const result = std.os.windows.ws2_32.WSAPoll(@ptrCast(&descriptor), 1, if (self.timeout_ms == 0) -1 else @intCast(self.timeout_ms));
            if (result == 0) return error.ConnectionTimedOut;
            if (result < 0) return error.SocketNotConnected;
        }
    }
    pub fn read(self: Socket, bytes: []u8) !usize {
        if (windows) {
            try self.wait(0x0100);
            const count = std.os.windows.ws2_32.recv(self.handle, bytes.ptr, @intCast(@min(bytes.len, std.math.maxInt(i32))), 0);
            if (count >= 0) return @intCast(count);
            return switch (std.os.windows.ws2_32.WSAGetLastError()) {
                .WSAETIMEDOUT, .WSAEWOULDBLOCK => error.ConnectionTimedOut,
                .WSAECONNRESET, .WSAECONNABORTED => error.ConnectionResetByPeer,
                else => error.SocketNotConnected,
            };
        }
        return std.posix.recv(self.handle, bytes, 0) catch |err| return if (err == error.WouldBlock) error.ConnectionTimedOut else err;
    }
    pub fn readAtLeast(self: Socket, bytes: []u8, minimum: usize) !usize {
        var offset: usize = 0;
        while (offset < minimum) {
            const count = try self.read(bytes[offset..]);
            if (count == 0) break;
            offset += count;
        }
        return offset;
    }
    pub fn write(self: Socket, bytes: []const u8) !usize {
        if (windows) {
            try self.wait(0x0010);
            const count = std.os.windows.ws2_32.send(self.handle, bytes.ptr, @intCast(@min(bytes.len, std.math.maxInt(i32))), 0);
            if (count >= 0) return @intCast(count);
            return error.BrokenPipe;
        }
        return std.posix.send(self.handle, bytes, 0) catch |err| return if (err == error.WouldBlock) error.ConnectionTimedOut else err;
    }
    pub fn readv(self: Socket, vectors: []const std.posix.iovec) !usize {
        for (vectors) |vector| if (vector.len > 0) return self.read(vector.base[0..vector.len]);
        return 0;
    }
    pub fn writev(self: Socket, vectors: []const std.posix.iovec_const) !usize {
        for (vectors) |vector| if (vector.len > 0) return self.write(vector.base[0..vector.len]);
        return 0;
    }
    pub fn writeAll(self: Socket, bytes: []const u8) !void {
        var offset: usize = 0;
        while (offset < bytes.len) {
            const count = try self.write(bytes[offset..]);
            if (count == 0) return error.BrokenPipe;
            offset += count;
        }
    }
    pub fn writevAll(self: Socket, vectors: []const std.posix.iovec_const) !void {
        for (vectors) |vector| try self.writeAll(vector.base[0..vector.len]);
    }
    pub fn close(self: Socket) void {
        (std.net.Stream{ .handle = self.handle }).close();
    }
};

pub fn setTimeout(stream: Socket, milliseconds: u32) !void {
    if (windows) {
        try std.posix.setsockopt(stream.handle, std.posix.SOL.SOCKET, std.posix.SO.RCVTIMEO, std.mem.asBytes(&milliseconds));
        try std.posix.setsockopt(stream.handle, std.posix.SOL.SOCKET, std.posix.SO.SNDTIMEO, std.mem.asBytes(&milliseconds));
    } else {
        const timeout = std.posix.timeval{ .sec = @intCast(milliseconds / 1000), .usec = @intCast((milliseconds % 1000) * 1000) };
        try std.posix.setsockopt(stream.handle, std.posix.SOL.SOCKET, std.posix.SO.RCVTIMEO, std.mem.asBytes(&timeout));
        try std.posix.setsockopt(stream.handle, std.posix.SOL.SOCKET, std.posix.SO.SNDTIMEO, std.mem.asBytes(&timeout));
    }
}
fn blocking(socket: std.posix.socket_t, enabled: bool) !void {
    if (windows) {
        var mode: u32 = if (enabled) 0 else 1;
        if (std.os.windows.ws2_32.ioctlsocket(socket, std.os.windows.ws2_32.FIONBIO, &mode) != 0) return error.Unexpected;
    } else {
        const flags = try std.posix.fcntl(socket, std.posix.F.GETFL, 0);
        _ = try std.posix.fcntl(socket, std.posix.F.SETFL, if (enabled) flags & ~@as(usize, std.posix.SOCK.NONBLOCK) else flags | std.posix.SOCK.NONBLOCK);
    }
}
pub fn connect(allocator: std.mem.Allocator, host: []const u8, port: u16, milliseconds: u32) !Socket {
    const addresses = try std.net.getAddressList(allocator, host, port);
    defer addresses.deinit();
    const deadline = std.time.milliTimestamp() + milliseconds;
    for (addresses.addrs) |address| {
        const socket = try std.posix.socket(address.any.family, std.posix.SOCK.STREAM, std.posix.IPPROTO.TCP);
        const stream = Socket{ .handle = socket, .timeout_ms = milliseconds };
        var owned = true;
        defer if (owned) stream.close();
        try blocking(socket, false);
        std.posix.connect(socket, &address.any, address.getOsSockLen()) catch |err| {
            if (err != error.WouldBlock and err != error.ConnectionPending) continue;
            const remaining = deadline - std.time.milliTimestamp();
            if (remaining <= 0) return error.ConnectionTimedOut;
            if (windows) {
                var descriptor = std.os.windows.ws2_32.pollfd{ .fd = socket, .events = 0x0010, .revents = 0 };
                if (std.os.windows.ws2_32.WSAPoll(@ptrCast(&descriptor), 1, @intCast(remaining)) <= 0) return error.ConnectionTimedOut;
                var failure: i32 = 0;
                var length: i32 = @sizeOf(i32);
                if (std.os.windows.ws2_32.getsockopt(socket, std.posix.SOL.SOCKET, std.posix.SO.ERROR, @ptrCast(&failure), &length) != 0) return error.Unexpected;
                if (failure != 0) continue;
            } else {
                var descriptor = [_]std.posix.pollfd{.{ .fd = socket, .events = std.posix.POLL.OUT, .revents = 0 }};
                if (try std.posix.poll(&descriptor, @intCast(remaining)) == 0) return error.ConnectionTimedOut;
                std.posix.getsockoptError(socket) catch continue;
            }
        };
        if (!windows) try blocking(socket, true);
        try setTimeout(stream, milliseconds);
        owned = false;
        return stream;
    }
    return error.ConnectionRefused;
}

pub fn httpConnection(client: *std.http.Client, host: []const u8, port: u16, tls: bool, timeout_ms: u32) !*std.http.Client.Connection {
    const node = try client.allocator.create(std.http.Client.ConnectionPool.Node);
    errdefer client.allocator.destroy(node);
    const stream = try connect(client.allocator, host, port, 5000);
    errdefer stream.close();
    const owned_host = try client.allocator.dupe(u8, host);
    errdefer client.allocator.free(owned_host);
    node.* = .{ .data = .{ .stream = .{ .handle = stream.handle }, .host = owned_host, .port = port, .protocol = if (tls) .tls else .plain, .tls_client = undefined, .closing = true } };
    if (tls) {
        try client.ca_bundle.rescan(client.allocator);
        node.data.tls_client = try client.allocator.create(std.crypto.tls.Client);
        errdefer client.allocator.destroy(node.data.tls_client);
        node.data.tls_client.* = try std.crypto.tls.Client.init(stream, .{ .host = .{ .explicit = host }, .ca = .{ .bundle = client.ca_bundle } });
        node.data.tls_client.allow_truncation_attacks = true;
    }
    try setTimeout(stream, timeout_ms);
    client.connection_pool.addUsed(node);
    return &node.data;
}

test "silent socket reads have a deadline" {
    var listener = try (try std.net.Address.parseIp4("127.0.0.1", 0)).listen(.{});
    defer listener.deinit();
    const Server = struct {
        fn run(server: *std.net.Server) void {
            const peer = server.accept() catch return;
            defer peer.stream.close();
            var byte: [1]u8 = undefined;
            _ = peer.stream.read(&byte) catch {};
        }
    };
    const thread = try std.Thread.spawn(.{}, Server.run, .{&listener});
    const stream = try connect(std.testing.allocator, "127.0.0.1", listener.listen_address.getPort(), 100);
    defer thread.join();
    defer stream.close();
    var byte: [1]u8 = undefined;
    const started = std.time.milliTimestamp();
    if (stream.read(&byte)) |count| try std.testing.expectEqual(@as(usize, 0), count) else |_| {}
    try std.testing.expect(std.time.milliTimestamp() - started >= 50);
    try std.testing.expect(std.time.milliTimestamp() - started < 2000);
}
