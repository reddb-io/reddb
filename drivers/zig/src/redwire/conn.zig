// RedWire connection. Sync (blocking) API on top of std.net's
// blocking socket. Each `Conn` owns one socket; methods serialise
// through an internal mutex so callers can share the same `Conn`
// across threads as long as they don't mind the lock contention.

const std = @import("std");
const Allocator = std.mem.Allocator;

const codec = @import("codec.zig");
const frame = @import("frame.zig");
const scram = @import("scram.zig");

pub const MAGIC: u8 = 0xFE;
pub const SUPPORTED_VERSION: u8 = 0x01;
pub const ALPN_PROTO = "redwire/1";

pub const AuthKind = enum {
    anonymous,
    bearer,
    scram_sha_256,
};

pub const Auth = union(AuthKind) {
    anonymous: void,
    bearer: []const u8,
    scram_sha_256: struct {
        username: []const u8,
        password: []const u8,
    },
};

pub const ConnectOptions = struct {
    host: []const u8,
    port: u16,
    auth: Auth = .{ .anonymous = {} },
    client_name: ?[]const u8 = null,
    /// When true, wrap the TCP stream in TLS via `std.crypto.tls.Client`.
    tls: bool = false,
    /// Connect timeout in milliseconds; 0 = no timeout.
    connect_timeout_ms: u32 = 30_000,
    /// I/O timeout in milliseconds (read/write); 0 = no timeout.
    io_timeout_ms: u32 = 30_000,
};

/// Internal trait-ish wrapper so the same `Conn` code can drive a
/// plain TCP socket or a `std.crypto.tls.Client`-wrapped one.
pub const Stream = union(enum) {
    plain: std.net.Stream,
    tls: TlsStream,

    pub fn read(self: *Stream, buf: []u8) !usize {
        return switch (self.*) {
            .plain => |s| s.read(buf),
            .tls => |*t| t.read(buf),
        };
    }

    pub fn writeAll(self: *Stream, bytes: []const u8) !void {
        return switch (self.*) {
            .plain => |s| s.writeAll(bytes),
            .tls => |*t| t.writeAll(bytes),
        };
    }

    pub fn close(self: *Stream) void {
        switch (self.*) {
            .plain => |s| s.close(),
            .tls => |*t| t.close(),
        }
    }
};

pub const TlsStream = struct {
    client: std.crypto.tls.Client,
    sock: std.net.Stream,

    pub fn read(self: *TlsStream, buf: []u8) !usize {
        return self.client.read(self.sock, buf);
    }

    pub fn writeAll(self: *TlsStream, bytes: []const u8) !void {
        return self.client.writeAll(self.sock, bytes);
    }

    pub fn close(self: *TlsStream) void {
        // Best-effort close-notify; ignore failures — the socket is
        // about to disappear anyway.
        _ = self.client.writeEnd(self.sock, &.{}, true) catch {};
        self.sock.close();
    }
};

pub const Conn = struct {
    allocator: Allocator,
    stream: Stream,
    mutex: std.Thread.Mutex = .{},
    next_corr: std.atomic.Value(u64) = std.atomic.Value(u64).init(1),
    session_id: []u8 = &.{},
    server_features: u32 = 0,
    closed: bool = false,

    pub fn deinit(self: *Conn) void {
        if (!self.closed) self.close();
        if (self.session_id.len > 0) self.allocator.free(self.session_id);
    }

    pub fn close(self: *Conn) void {
        if (self.closed) return;
        self.closed = true;
        // Best-effort Bye — ignore errors, the peer might already be gone.
        const corr = self.next_corr.fetchAdd(1, .monotonic);
        const f = frame.Frame.init(.bye, corr, &.{});
        if (codec.encodeFrame(self.allocator, f)) |bytes| {
            defer self.allocator.free(bytes);
            self.stream.writeAll(bytes) catch {};
        } else |_| {}
        self.stream.close();
    }

    pub fn nextCorr(self: *Conn) u64 {
        return self.next_corr.fetchAdd(1, .monotonic);
    }

    /// Send a frame. Allocator usage is internal; this never holds
    /// memory across the lock boundary.
    pub fn sendFrame(self: *Conn, f: frame.Frame) !void {
        const bytes = try codec.encodeFrame(self.allocator, f);
        defer self.allocator.free(bytes);
        try self.stream.writeAll(bytes);
    }

    /// Read one frame. Returned `Decoded` may own a payload buffer
    /// (compressed frames) — caller must `deinit` to free it.
    pub fn recvFrame(self: *Conn) !codec.Decoded {
        var header_buf: [frame.FRAME_HEADER_SIZE]u8 = undefined;
        try readExact(&self.stream, &header_buf);
        const header = try frame.readHeader(&header_buf);
        try frame.validateHeader(header);
        const len: usize = header.length;
        const buf = try self.allocator.alloc(u8, len);
        defer self.allocator.free(buf);
        @memcpy(buf[0..frame.FRAME_HEADER_SIZE], &header_buf);
        if (len > frame.FRAME_HEADER_SIZE) {
            try readExact(&self.stream, buf[frame.FRAME_HEADER_SIZE..len]);
        }
        var decoded = try codec.decodeFrame(self.allocator, buf);
        // The decoder borrowed from `buf`; promote it to an owned
        // copy so freeing `buf` doesn't dangle.
        if (decoded.owned_payload == null) {
            const owned = try self.allocator.dupe(u8, decoded.frame.payload);
            decoded.owned_payload = owned;
            decoded.frame.payload = owned;
        }
        return decoded;
    }

    // ---- High level operations ----

    /// Run a SQL query. Returns the raw JSON envelope (allocated by
    /// `self.allocator`) — caller frees.
    pub fn query(self: *Conn, sql: []const u8) ![]const u8 {
        self.mutex.lock();
        defer self.mutex.unlock();
        const corr = self.nextCorr();
        try self.sendFrame(frame.Frame.init(.query, corr, sql));
        var resp = try self.recvFrame();
        errdefer resp.deinit(self.allocator);
        switch (resp.frame.kind) {
            .result => return takeOwned(self.allocator, &resp),
            .err => {
                resp.deinit(self.allocator);
                return error.ProtocolError;
            },
            else => {
                resp.deinit(self.allocator);
                return error.UnexpectedFrame;
            },
        }
    }

    pub fn ping(self: *Conn) !void {
        self.mutex.lock();
        defer self.mutex.unlock();
        const corr = self.nextCorr();
        try self.sendFrame(frame.Frame.init(.ping, corr, &.{}));
        var resp = try self.recvFrame();
        defer resp.deinit(self.allocator);
        if (resp.frame.kind != .pong) return error.UnexpectedFrame;
    }
};

/// Move ownership of a decoded payload to a fresh slice the caller
/// owns. Frees any temporary buffers held by the `Decoded` struct.
fn takeOwned(allocator: Allocator, d: *codec.Decoded) ![]u8 {
    if (d.owned_payload) |o| {
        d.owned_payload = null;
        // Truncate in case the payload was decompressed into a
        // capacity-padded buffer.
        if (o.len == d.frame.payload.len) return o;
        const out = try allocator.dupe(u8, d.frame.payload);
        allocator.free(o);
        return out;
    }
    return allocator.dupe(u8, d.frame.payload);
}

fn readExact(stream: *Stream, buf: []u8) !void {
    var n: usize = 0;
    while (n < buf.len) {
        const got = try stream.read(buf[n..]);
        if (got == 0) return error.UnexpectedFrame;
        n += got;
    }
}

/// Open a connection and run the handshake.
pub fn connect(allocator: Allocator, opts: ConnectOptions) !*Conn {
    const sock = try std.net.tcpConnectToHost(allocator, opts.host, opts.port);
    var stream: Stream = .{ .plain = sock };
    var stream_owned = true;
    errdefer if (stream_owned) stream.close();

    if (opts.tls) {
        // std.crypto.tls.Client in 0.13 doesn't expose ALPN
        // configuration directly; we connect with the system roots
        // and document the limitation in the README. mTLS / custom
        // CA bundles are also deferred — TLS support here is a
        // best-effort enable-flag rather than the full feature set
        // the Rust driver carries.
        var bundle: std.crypto.Certificate.Bundle = .{};
        defer bundle.deinit(allocator);
        try bundle.rescan(allocator);
        const tls_client = try std.crypto.tls.Client.init(sock, bundle, opts.host);
        stream = .{ .tls = .{ .client = tls_client, .sock = sock } };
    }

    // Magic + version preamble.
    try stream.writeAll(&.{ MAGIC, SUPPORTED_VERSION });

    var conn = try allocator.create(Conn);
    errdefer allocator.destroy(conn);
    conn.* = .{
        .allocator = allocator,
        .stream = stream,
    };
    // Once the Conn owns the stream, the conn's `close` is the only
    // path that touches the socket — don't double-close from the
    // outer errdefer.
    stream_owned = false;
    errdefer conn.close();

    try runHandshake(conn, opts);
    return conn;
}

// ---------------------------------------------------------------------------
// Handshake state machine. Hello → HelloAck → AuthResponse* → AuthOk
// ---------------------------------------------------------------------------

fn runHandshake(conn: *Conn, opts: ConnectOptions) !void {
    // 1. Build & send Hello.
    const methods: []const []const u8 = switch (opts.auth) {
        .anonymous => &.{ "anonymous", "bearer" },
        .bearer => &.{"bearer"},
        .scram_sha_256 => &.{"scram-sha-256"},
    };
    const hello_payload = try buildHelloJson(conn.allocator, methods, opts.client_name);
    defer conn.allocator.free(hello_payload);
    try conn.sendFrame(frame.Frame.init(.hello, 1, hello_payload));

    // 2. Read HelloAck.
    var ack = try conn.recvFrame();
    defer ack.deinit(conn.allocator);
    if (ack.frame.kind == .auth_fail) return error.AuthRefused;
    if (ack.frame.kind != .hello_ack) return error.UnexpectedFrame;
    const chosen = try parseHelloAckAuth(conn.allocator, ack.frame.payload);
    defer conn.allocator.free(chosen);

    // 3. Drive the chosen auth method.
    if (std.mem.eql(u8, chosen, "anonymous")) {
        try conn.sendFrame(frame.Frame.init(.auth_response, 2, &.{}));
    } else if (std.mem.eql(u8, chosen, "bearer")) {
        const token = switch (opts.auth) {
            .bearer => |t| t,
            else => return error.AuthRefused,
        };
        const resp = try buildBearerJson(conn.allocator, token);
        defer conn.allocator.free(resp);
        try conn.sendFrame(frame.Frame.init(.auth_response, 2, resp));
    } else if (std.mem.eql(u8, chosen, "scram-sha-256")) {
        try runScram(conn, opts);
    } else {
        return error.UnexpectedFrame;
    }

    // 4. Read AuthOk / AuthFail.
    var final = try conn.recvFrame();
    defer final.deinit(conn.allocator);
    switch (final.frame.kind) {
        .auth_ok => {
            const sid = try parseAuthOkSession(conn.allocator, final.frame.payload);
            conn.session_id = sid;
        },
        .auth_fail => return error.AuthRefused,
        else => return error.UnexpectedFrame,
    }
}

fn runScram(conn: *Conn, opts: ConnectOptions) !void {
    const creds = switch (opts.auth) {
        .scram_sha_256 => |c| c,
        else => return error.AuthRefused,
    };
    const cnonce = try scram.generateClientNonce(conn.allocator);
    defer conn.allocator.free(cnonce);

    const client_first_bare = try std.fmt.allocPrint(
        conn.allocator,
        "n={s},r={s}",
        .{ creds.username, cnonce },
    );
    defer conn.allocator.free(client_first_bare);

    const client_first = try std.fmt.allocPrint(conn.allocator, "n,,{s}", .{client_first_bare});
    defer conn.allocator.free(client_first);

    try conn.sendFrame(frame.Frame.init(.auth_response, 2, client_first));

    var server_first = try conn.recvFrame();
    defer server_first.deinit(conn.allocator);
    if (server_first.frame.kind != .auth_request) return error.UnexpectedFrame;

    const sf = try scram.parseServerFirst(server_first.frame.payload);
    const salt = try scram.b64DecodeAlloc(conn.allocator, sf.salt_b64);
    defer conn.allocator.free(salt);

    const client_final_no_proof = try std.fmt.allocPrint(
        conn.allocator,
        "c=biws,r={s}",
        .{sf.combined_nonce},
    );
    defer conn.allocator.free(client_final_no_proof);

    const auth_message = try std.fmt.allocPrint(
        conn.allocator,
        "{s},{s},{s}",
        .{ client_first_bare, server_first.frame.payload, client_final_no_proof },
    );
    defer conn.allocator.free(auth_message);

    const proof = try scram.clientProof(creds.password, salt, sf.iter, auth_message);
    const proof_b64 = try scram.b64Encode(conn.allocator, &proof);
    defer conn.allocator.free(proof_b64);

    const client_final = try std.fmt.allocPrint(
        conn.allocator,
        "{s},p={s}",
        .{ client_final_no_proof, proof_b64 },
    );
    defer conn.allocator.free(client_final);

    try conn.sendFrame(frame.Frame.init(.auth_response, 3, client_final));
    // The final AuthOk/AuthFail is read by the caller.
}

// ---------------------------------------------------------------------------
// JSON helpers — tiny hand-rolled writers/parsers. Keeps the handshake
// payloads stable without dragging the full `std.json` API into
// build-time error sets.
// ---------------------------------------------------------------------------

fn buildHelloJson(allocator: Allocator, methods: []const []const u8, client_name: ?[]const u8) ![]u8 {
    var buf = std.ArrayList(u8).init(allocator);
    errdefer buf.deinit();
    try buf.appendSlice("{\"versions\":[1],\"auth_methods\":[");
    for (methods, 0..) |m, i| {
        if (i > 0) try buf.append(',');
        try buf.append('"');
        try buf.appendSlice(m);
        try buf.append('"');
    }
    try buf.appendSlice("],\"features\":0");
    if (client_name) |name| {
        try buf.appendSlice(",\"client_name\":\"");
        try appendJsonEscaped(&buf, name);
        try buf.append('"');
    }
    try buf.append('}');
    return buf.toOwnedSlice();
}

fn buildBearerJson(allocator: Allocator, token: []const u8) ![]u8 {
    var buf = std.ArrayList(u8).init(allocator);
    errdefer buf.deinit();
    try buf.appendSlice("{\"token\":\"");
    try appendJsonEscaped(&buf, token);
    try buf.appendSlice("\"}");
    return buf.toOwnedSlice();
}

fn appendJsonEscaped(buf: *std.ArrayList(u8), s: []const u8) !void {
    for (s) |c| {
        switch (c) {
            '"' => try buf.appendSlice("\\\""),
            '\\' => try buf.appendSlice("\\\\"),
            '\n' => try buf.appendSlice("\\n"),
            '\r' => try buf.appendSlice("\\r"),
            '\t' => try buf.appendSlice("\\t"),
            else => try buf.append(c),
        }
    }
}

fn parseHelloAckAuth(allocator: Allocator, payload: []const u8) ![]u8 {
    const found = try parseStringField(allocator, payload, "auth");
    return found orelse error.UnexpectedFrame;
}

fn parseAuthOkSession(allocator: Allocator, payload: []const u8) ![]u8 {
    const found = try parseStringField(allocator, payload, "session_id");
    return found orelse allocator.dupe(u8, "");
}

/// Find `"name":"value"` in a flat JSON object and return a duped
/// copy of the value. Skips escaping — handshake payloads come from
/// the engine, never user input, so we don't need a real parser.
fn parseStringField(allocator: Allocator, payload: []const u8, name: []const u8) !?[]u8 {
    var key_buf: [64]u8 = undefined;
    if (name.len + 2 > key_buf.len) return null;
    key_buf[0] = '"';
    @memcpy(key_buf[1 .. 1 + name.len], name);
    key_buf[1 + name.len] = '"';
    const key = key_buf[0 .. 2 + name.len];

    const start = std.mem.indexOf(u8, payload, key) orelse return null;
    var i = start + key.len;
    while (i < payload.len and (payload[i] == ' ' or payload[i] == ':')) : (i += 1) {}
    if (i >= payload.len or payload[i] != '"') return null;
    i += 1;
    const value_start = i;
    while (i < payload.len and payload[i] != '"') : (i += 1) {
        if (payload[i] == '\\' and i + 1 < payload.len) i += 1;
    }
    return try allocator.dupe(u8, payload[value_start..i]);
}
