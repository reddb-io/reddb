// Handshake state-machine tests. We pair the client against a tiny
// fake server running on a real TCP socket bound to 127.0.0.1:0.
// std.posix.socketpair would have been cheaper but it produces
// AF_UNIX sockets which `std.net.Stream` doesn't accept on every
// platform; a localhost socket is portable.

const std = @import("std");
const reddb = @import("reddb");
const conn = reddb.redwire.conn;
const frame = reddb.redwire.frame;
const codec = reddb.redwire.codec;

const t = std.testing;

const FakeServer = struct {
    listener: std.net.Server,
    addr: std.net.Address,
    thread: ?std.Thread = null,
    behaviour: Behaviour,
    err: ?anyerror = null,

    pub const Behaviour = enum {
        anonymous_ok,
        bearer_ok,
        auth_fail_at_helloack,
        auth_fail_at_authok,
        bad_magic,
    };

    pub fn start(behaviour: Behaviour) !*FakeServer {
        const allocator = std.heap.page_allocator;
        const self = try allocator.create(FakeServer);
        const addr = try std.net.Address.parseIp4("127.0.0.1", 0);
        self.* = .{
            .listener = try addr.listen(.{ .reuse_address = true }),
            .addr = undefined,
            .behaviour = behaviour,
        };
        self.addr = self.listener.listen_address;
        self.thread = try std.Thread.spawn(.{}, run, .{self});
        return self;
    }

    pub fn stop(self: *FakeServer) void {
        if (self.thread) |th| th.join();
        self.listener.deinit();
        std.heap.page_allocator.destroy(self);
    }

    fn run(self: *FakeServer) void {
        self.serve() catch |e| {
            self.err = e;
        };
    }

    fn serve(self: *FakeServer) !void {
        var conn_inst = try self.listener.accept();
        defer conn_inst.stream.close();
        const allocator = std.heap.page_allocator;

        if (self.behaviour == .bad_magic) {
            // Write garbage and close — client must reject.
            _ = conn_inst.stream.writeAll(&.{ 0x00, 0x00 }) catch {};
            return;
        }

        // Read magic + version.
        var preamble: [2]u8 = undefined;
        _ = try conn_inst.stream.readAll(&preamble);
        if (preamble[0] != conn.MAGIC) return;

        // Read Hello.
        const hello = try readFrame(allocator, conn_inst.stream);
        defer allocator.free(hello.bytes);
        if (hello.frame.kind != .hello) return;

        // HelloAck or AuthFail.
        switch (self.behaviour) {
            .auth_fail_at_helloack => {
                try writeJsonFrame(conn_inst.stream, .auth_fail, 1, "{\"reason\":\"locked\"}");
                return;
            },
            .anonymous_ok, .auth_fail_at_authok => {
                try writeJsonFrame(conn_inst.stream, .hello_ack, 1, "{\"auth\":\"anonymous\"}");
            },
            .bearer_ok => {
                try writeJsonFrame(conn_inst.stream, .hello_ack, 1, "{\"auth\":\"bearer\"}");
            },
            .bad_magic => unreachable,
        }

        // Read AuthResponse.
        const resp = try readFrame(allocator, conn_inst.stream);
        defer allocator.free(resp.bytes);
        if (resp.frame.kind != .auth_response) return;

        // Final step.
        switch (self.behaviour) {
            .auth_fail_at_authok => {
                try writeJsonFrame(conn_inst.stream, .auth_fail, 2, "{\"reason\":\"bad token\"}");
            },
            .anonymous_ok, .bearer_ok => {
                try writeJsonFrame(conn_inst.stream, .auth_ok, 2, "{\"session_id\":\"sess-1\",\"features\":0}");
                // Echo a Ping → Pong, then close on Bye.
                while (true) {
                    const next_frame = readFrame(allocator, conn_inst.stream) catch return;
                    defer allocator.free(next_frame.bytes);
                    switch (next_frame.frame.kind) {
                        .ping => try writeJsonFrame(conn_inst.stream, .pong, next_frame.frame.correlation_id, ""),
                        .bye => return,
                        .query => {
                            // Echo a trivial Result envelope back.
                            try writeJsonFrame(conn_inst.stream, .result, next_frame.frame.correlation_id, "{\"ok\":true}");
                        },
                        else => return,
                    }
                }
            },
            else => {},
        }
    }

    const ReadFrame = struct {
        frame: frame.Frame,
        bytes: []u8,
    };

    fn readFrame(allocator: std.mem.Allocator, stream: std.net.Stream) !ReadFrame {
        var hdr: [frame.FRAME_HEADER_SIZE]u8 = undefined;
        var n: usize = 0;
        while (n < hdr.len) {
            const got = try stream.read(hdr[n..]);
            if (got == 0) return error.UnexpectedFrame;
            n += got;
        }
        const header = try frame.readHeader(&hdr);
        try frame.validateHeader(header);
        const buf = try allocator.alloc(u8, header.length);
        @memcpy(buf[0..frame.FRAME_HEADER_SIZE], &hdr);
        var off: usize = frame.FRAME_HEADER_SIZE;
        while (off < header.length) {
            const got = try stream.read(buf[off..]);
            if (got == 0) return error.UnexpectedFrame;
            off += got;
        }
        var decoded = try codec.decodeFrame(allocator, buf);
        // The decoded frame borrows from `buf`; copy the payload out
        // so the caller can free `buf` independently.
        const owned_payload = try allocator.dupe(u8, decoded.frame.payload);
        decoded.deinit(allocator);
        decoded.frame.payload = owned_payload;
        // Replace `buf` with one that holds the payload so freeing
        // the returned `bytes` slice frees the payload too.
        allocator.free(buf);
        return ReadFrame{ .frame = decoded.frame, .bytes = owned_payload };
    }

    fn writeJsonFrame(stream: std.net.Stream, kind: frame.MessageKind, corr: u64, body: []const u8) !void {
        const allocator = std.heap.page_allocator;
        const f = frame.Frame.init(kind, corr, body);
        const bytes = try codec.encodeFrame(allocator, f);
        defer allocator.free(bytes);
        try stream.writeAll(bytes);
    }
};

test "anonymous handshake succeeds" {
    var server = try FakeServer.start(.anonymous_ok);
    defer server.stop();
    const c = try conn.connect(t.allocator, .{
        .host = "127.0.0.1",
        .port = server.addr.getPort(),
    });
    defer {
        c.deinit();
        t.allocator.destroy(c);
    }
    try c.ping();
    const result = try c.query("SELECT 1");
    defer t.allocator.free(result);
    try t.expect(std.mem.indexOf(u8, result, "ok") != null);
}

test "bearer handshake succeeds" {
    var server = try FakeServer.start(.bearer_ok);
    defer server.stop();
    const c = try conn.connect(t.allocator, .{
        .host = "127.0.0.1",
        .port = server.addr.getPort(),
        .auth = .{ .bearer = "sk-test-abc" },
    });
    defer {
        c.deinit();
        t.allocator.destroy(c);
    }
    try c.ping();
}

test "AuthFail at HelloAck → AuthRefused" {
    var server = try FakeServer.start(.auth_fail_at_helloack);
    defer server.stop();
    const result = conn.connect(t.allocator, .{
        .host = "127.0.0.1",
        .port = server.addr.getPort(),
    });
    try t.expectError(error.AuthRefused, result);
}

test "AuthFail at AuthOk → AuthRefused" {
    var server = try FakeServer.start(.auth_fail_at_authok);
    defer server.stop();
    const result = conn.connect(t.allocator, .{
        .host = "127.0.0.1",
        .port = server.addr.getPort(),
    });
    try t.expectError(error.AuthRefused, result);
}

test "bad magic → connection error" {
    var server = try FakeServer.start(.bad_magic);
    defer server.stop();
    const result = conn.connect(t.allocator, .{
        .host = "127.0.0.1",
        .port = server.addr.getPort(),
    });
    // Server writes 2 bytes of garbage and closes. The client
    // either fails reading the HelloAck (UnexpectedFrame) or hits
    // a frame parse error — either is acceptable here, the point
    // is the connect call doesn't succeed.
    try t.expect(std.meta.isError(result));
    if (result) |c| {
        c.deinit();
        t.allocator.destroy(c);
    } else |_| {}
}
