// Public surface for the RedDB Zig driver.
//
// One entry point — `connect(allocator, uri, opts)` — picks the
// transport from the URI's scheme and returns a typed handle. The
// redwire transport offers query / ping / close; the HTTP transport
// covers the REST endpoints in `drivers/js/src/http.js`.
//
// Embedded mode uses the same versioned C ABI as the other native
// language drivers when the build supplies `-Dreddb-ffi-dir=...`.

const std = @import("std");
const Allocator = std.mem.Allocator;

pub const url = @import("url.zig");
pub const errors = @import("errors.zig");
pub const helpers = @import("helpers.zig");
pub const Error = errors.Error;
pub const build_options = @import("build_options");
const embedded_impl = if (build_options.enable_embedded)
    @import("embedded/conn.zig")
else
    @import("embedded/disabled.zig");

pub const redwire = struct {
    pub const frame = @import("redwire/frame.zig");
    pub const codec = @import("redwire/codec.zig");
    pub const value_codec = @import("redwire/value_codec.zig");
    pub const scram = @import("redwire/scram.zig");
    pub const conn = @import("redwire/conn.zig");

    pub const Conn = conn.Conn;
    pub const Value = value_codec.Value;
    pub const ConnectOptions = conn.ConnectOptions;
    pub const Auth = conn.Auth;
    pub const AuthKind = conn.AuthKind;
    pub const MAGIC = conn.MAGIC;
    pub const SUPPORTED_VERSION = conn.SUPPORTED_VERSION;
    pub const ALPN_PROTO = conn.ALPN_PROTO;
};

pub const http = struct {
    pub const Client = @import("http/client.zig").HttpClient;
};

pub const Conn = struct {
    allocator: Allocator,
    backend: Backend,

    const Backend = union(enum) {
        redwire: *redwire.Conn,
        embedded: *embedded_impl.Conn,
    };

    pub fn deinit(self: *Conn) void {
        switch (self.backend) {
            .redwire => |conn| {
                conn.deinit();
                self.allocator.destroy(conn);
            },
            .embedded => |conn| {
                conn.deinit();
                self.allocator.destroy(conn);
            },
        }
    }

    pub fn close(self: *Conn) void {
        self.closeChecked() catch {};
    }

    pub fn closeChecked(self: *Conn) !void {
        switch (self.backend) {
            .redwire => |conn| conn.close(),
            .embedded => |conn| try conn.closeChecked(),
        }
    }

    pub fn ping(self: *Conn) !void {
        return switch (self.backend) {
            .redwire => |conn| conn.ping(),
            .embedded => |conn| conn.ping(),
        };
    }

    pub fn query(self: *Conn, sql: []const u8, params: anytype) ![]const u8 {
        return switch (self.backend) {
            .redwire => |conn| conn.query(sql, params),
            .embedded => |conn| conn.query(sql, params),
        };
    }
};
pub const Value = redwire.Value;

/// Top-level connect helper. Parses the URI, picks the transport,
/// runs the handshake, and returns an owning pointer to the
/// connection. Caller drives `.deinit()` to release it.
pub fn connect(allocator: Allocator, uri: []const u8, override: ?redwire.ConnectOptions) !*Conn {
    var parsed = try url.parse(allocator, uri);
    defer parsed.deinit(allocator);

    const conn = try allocator.create(Conn);
    errdefer allocator.destroy(conn);
    switch (parsed.kind) {
        .redwire, .redwire_tls => {
            var opts = override orelse redwire.ConnectOptions{
                .host = parsed.host orelse return Error.MissingHost,
                .port = parsed.port orelse url.defaultPortFor(parsed.kind),
            };
            // Pull host/port from the URI when the caller didn't override.
            if (override == null) {
                opts.tls = parsed.kind == .redwire_tls;
                if (parsed.token) |t| opts.auth = .{ .bearer = t };
            }
            conn.* = .{
                .allocator = allocator,
                .backend = .{ .redwire = try redwire.conn.connect(allocator, opts) },
            };
            return conn;
        },
        .embedded => {
            if (!build_options.enable_embedded) return Error.EmbeddedUnavailable;
            conn.* = .{
                .allocator = allocator,
                .backend = .{ .embedded = try embedded_impl.connect(allocator, parsed.path) },
            };
            return conn;
        },
        else => return Error.UnsupportedScheme,
    }
}

test {
    // Pull every module into the build so `zig build test` exercises
    // the in-file test blocks across the tree.
    std.testing.refAllDecls(@This());
    std.testing.refAllDecls(redwire);
    std.testing.refAllDecls(http);
}
