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
const topology_contract = @import("topology.zig");
pub const topology = topology_contract;
pub const routing = @import("routing.zig");
pub const ReadOptions = routing.ReadOptions;
pub const ReadConsistency = routing.Consistency;
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
        http: *HttpConnection,
        routed: *routing.Router,
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
            .http => |conn| {
                conn.close();
                self.allocator.destroy(conn);
            },
            .routed => |conn| {
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
            .http => |conn| conn.close(),
            .routed => |conn| conn.close(),
        }
    }

    pub fn ping(self: *Conn) anyerror!void {
        return switch (self.backend) {
            .redwire => |conn| conn.ping(),
            .embedded => |conn| conn.ping(),
            .http => |conn| conn.ping(),
            .routed => |conn| conn.ping(),
        };
    }

    pub fn query(self: *Conn, sql: []const u8, params: anytype) anyerror![]const u8 {
        return switch (self.backend) {
            .redwire => |conn| conn.query(sql, params),
            .embedded => |conn| conn.query(sql, params),
            .http => |conn| conn.query(sql, params),
            .routed => |conn| conn.query(sql, params, null),
        };
    }
    pub fn queryWithOptions(self: *Conn, sql: []const u8, options: ReadOptions, params: anytype) anyerror![]const u8 {
        return switch (self.backend) {
            .routed => |conn| conn.query(sql, params, options),
            else => error.ConsistencyUnsupported,
        };
    }
    pub fn topology(self: *Conn, protocol: []const u8) !?topology_contract.Topology {
        return switch (self.backend) {
            .redwire => |conn| blk: {
                const bytes = (try conn.topology(protocol)) orelse break :blk null;
                defer self.allocator.free(bytes);
                break :blk try topology_contract.decode(self.allocator, bytes);
            },
            .http => |conn| blk: {
                const prior_timeout = conn.client.timeout_ms;
                conn.client.timeout_ms = 5000;
                defer conn.client.timeout_ms = prior_timeout;
                const path = try std.fmt.allocPrint(self.allocator, "/v1/topology?transport={s}", .{protocol});
                defer self.allocator.free(path);
                const bytes = try conn.client.request(.GET, path, null);
                defer self.allocator.free(bytes);
                break :blk try topology_contract.decodeJson(self.allocator, bytes);
            },
            else => null,
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
    if (parsed.kind == .embedded) return connectDirect(allocator, uri, override);
    const protocol: []const u8 = switch (parsed.kind) {
        .redwire => if (override != null and override.?.tls) "reds" else "red",
        .redwire_tls => "reds",
        .http => "http",
        .https => "https",
        else => return Error.UnsupportedScheme,
    };
    const host = if (override) |options| options.host else parsed.host orelse return Error.MissingHost;
    const authority = if (std.mem.indexOfScalar(u8, host, ':') != null and !std.mem.startsWith(u8, host, "["))
        try std.fmt.allocPrint(allocator, "[{s}]", .{host})
    else
        try allocator.dupe(u8, host);
    defer allocator.free(authority);
    const seed = try std.fmt.allocPrint(allocator, "{s}://{s}:{d}{s}", .{ protocol, authority, if (override) |options| options.port else parsed.port orelse url.defaultPortFor(parsed.kind), parsed.path orelse "" });
    defer allocator.free(seed);
    const initial = try connectDirect(allocator, uri, override);
    const router = try routing.Router.create(allocator, uri, seed, protocol, initial, override);
    errdefer {
        router.deinit();
        allocator.destroy(router);
    }
    const result = try allocator.create(Conn);
    result.* = .{ .allocator = allocator, .backend = .{ .routed = router } };
    return result;
}

/// Internal transport factory used by the routing pool; bypasses recursive discovery.
pub fn connectDirect(allocator: Allocator, uri: []const u8, override: ?redwire.ConnectOptions) !*Conn {
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
            opts.tls = opts.tls or parsed.kind == .redwire_tls;
            // Pull host/port from the URI when the caller didn't override.
            if (override == null) {
                opts.tls = parsed.kind == .redwire_tls;
                if (parsed.token orelse parsed.api_key) |t| {
                    opts.auth = .{ .bearer = t };
                } else if (parsed.username) |username| {
                    if (parsed.password) |password| opts.auth = .{ .scram_sha_256 = .{ .username = username, .password = password } };
                }
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
        .http, .https => {
            const scheme: []const u8 = if (parsed.kind == .https) "https" else "http";
            const host = parsed.host orelse return Error.MissingHost;
            const base = try std.fmt.allocPrint(allocator, "{s}://{s}:{d}{s}", .{ scheme, host, parsed.port orelse url.defaultPortFor(parsed.kind), parsed.path orelse "" });
            defer allocator.free(base);
            var token = parsed.token orelse parsed.api_key;
            if (override) |options| switch (options.auth) {
                .bearer => |value| token = value,
                else => {},
            };
            const client = try allocator.create(HttpConnection);
            errdefer allocator.destroy(client);
            client.* = .{ .allocator = allocator, .client = try http.Client.init(allocator, base, token) };
            errdefer client.close();
            if (token == null) {
                var user = parsed.username;
                var pass = parsed.password;
                if (override) |options| switch (options.auth) {
                    .scram_sha_256 => |credentials| {
                        user = credentials.username;
                        pass = credentials.password;
                    },
                    else => {},
                };
                if (user != null and pass != null) {
                    const body = try client.client.login(user.?, pass.?);
                    defer allocator.free(body);
                    const json = try std.json.parseFromSlice(std.json.Value, allocator, body, .{});
                    defer json.deinit();
                    if (json.value != .object) return Error.AuthRefused;
                    const value = json.value.object.get("token") orelse return Error.AuthRefused;
                    if (value != .string) return Error.AuthRefused;
                    try client.client.setToken(value.string);
                }
            }
            conn.* = .{ .allocator = allocator, .backend = .{ .http = client } };
            return conn;
        },
        else => return Error.UnsupportedScheme,
    }
}

const HttpConnection = struct {
    allocator: Allocator,
    client: http.Client,
    closed: bool = false,
    fn close(self: *HttpConnection) void {
        if (!self.closed) {
            self.closed = true;
            self.client.deinit();
        }
    }
    fn ping(self: *HttpConnection) !void {
        if (self.closed) return Error.ConnectionClosed;
        const prior_timeout = self.client.timeout_ms;
        self.client.timeout_ms = 5000;
        defer self.client.timeout_ms = prior_timeout;
        const result = try self.client.getHealth();
        self.allocator.free(result);
    }
    fn query(self: *HttpConnection, sql: []const u8, params: anytype) ![]const u8 {
        if (self.closed) return Error.ConnectionClosed;
        const T = @TypeOf(params);
        switch (@typeInfo(T)) {
            .@"struct" => |info| {
                if (!info.is_tuple) @compileError("params must be a tuple");
                var values: [info.fields.len]Value = undefined;
                inline for (info.fields, 0..) |field, index| values[index] = try redwire.conn.nativeParamToValue(@field(params, field.name));
                return self.client.queryWithParams(sql, &values);
            },
            .pointer => |pointer| switch (pointer.size) {
                .slice => return self.client.queryWithParams(sql, params),
                .one => return self.client.queryWithParams(sql, params[0..]),
                else => @compileError("params must be values or a tuple"),
            },
            else => @compileError("params must be values or a tuple"),
        }
    }
};

test {
    // Pull every module into the build so `zig build test` exercises
    // the in-file test blocks across the tree.
    std.testing.refAllDecls(@This());
    std.testing.refAllDecls(redwire);
    std.testing.refAllDecls(http);
}
