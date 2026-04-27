// Public surface for the RedDB Zig driver.
//
// One entry point — `connect(allocator, uri, opts)` — picks the
// transport from the URI's scheme and returns a typed handle. The
// redwire transport offers query / ping / close; the HTTP transport
// covers the REST endpoints in `drivers/js/src/http.js`.
//
// Embedded mode is *not* implemented: there's no Zig binding to the
// RedDB engine. `connect()` returns `error.UnsupportedScheme` for
// `red:///` paths so callers can fall back to spawning the binary
// the way `drivers/js` does.

const std = @import("std");
const Allocator = std.mem.Allocator;

pub const url = @import("url.zig");
pub const errors = @import("errors.zig");
pub const Error = errors.Error;
pub const build_options = @import("build_options");

pub const redwire = struct {
    pub const frame = @import("redwire/frame.zig");
    pub const codec = @import("redwire/codec.zig");
    pub const scram = @import("redwire/scram.zig");
    pub const conn = @import("redwire/conn.zig");

    pub const Conn = conn.Conn;
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

pub const Conn = redwire.Conn;

/// Top-level connect helper. Parses the URI, picks the transport,
/// runs the handshake, and returns an owning pointer to the
/// connection. Caller drives `.deinit()` to release it.
pub fn connect(allocator: Allocator, uri: []const u8, override: ?redwire.ConnectOptions) !*redwire.Conn {
    var parsed = try url.parse(allocator, uri);
    defer parsed.deinit(allocator);

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
            return redwire.conn.connect(allocator, opts);
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
