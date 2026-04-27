// `red://` URI parser. Mirrors `drivers/js/src/url.js` so the same
// connection strings work across drivers — the JS test corpus is
// the de-facto fixture set.
//
// Public types:
//   - Kind: transport class (embedded / http / https / grpc / grpcs / pg)
//   - ParsedUri: normalised view; lifetime tied to the input slice
//                or to the allocator that owns the decoded copies.
//   - parse(allocator, uri) → ParsedUri (owned strings)
//
// The default port for `red://`/`reds://` (the new redwire scheme)
// is **5050**, per task brief — different from the JS reference,
// which keeps the `grpc://` legacy default of 5051. We honour the
// brief and document the divergence.

const std = @import("std");
const Allocator = std.mem.Allocator;
const Error = @import("errors.zig").Error;

pub const Kind = enum {
    embedded,
    http,
    https,
    grpc,
    grpcs,
    pg,
    redwire, // `red://` / `reds://` — TCP redwire (TLS when reds)
    redwire_tls,
};

pub const ParsedUri = struct {
    kind: Kind,
    host: ?[]const u8 = null,
    port: ?u16 = null,
    path: ?[]const u8 = null,
    username: ?[]const u8 = null,
    password: ?[]const u8 = null,
    token: ?[]const u8 = null,
    api_key: ?[]const u8 = null,
    login_url: ?[]const u8 = null,
    original_uri: []const u8,

    /// Free everything `parse` allocated. Safe to call repeatedly —
    /// fields are nulled out so a second free is a no-op.
    pub fn deinit(self: *ParsedUri, allocator: Allocator) void {
        const fields = [_]*?[]const u8{
            &self.host,
            &self.path,
            &self.username,
            &self.password,
            &self.token,
            &self.api_key,
            &self.login_url,
        };
        for (fields) |fp| {
            if (fp.*) |s| {
                allocator.free(s);
                fp.* = null;
            }
        }
    }
};

/// Default ports per transport class. `red`/`reds` use **5050** to
/// match the `redwire` listener documented in `docs/adr/0001`.
pub fn defaultPortFor(kind: Kind) u16 {
    return switch (kind) {
        .http => 8080,
        .https => 8443,
        .grpc => 5051,
        .grpcs => 5052,
        .pg => 5432,
        .redwire, .redwire_tls => 5050,
        .embedded => 0,
    };
}

/// Parse any RedDB connection URI. Allocates owned copies of every
/// returned slice so the caller can drop the original `uri` buffer
/// without dangling references.
pub fn parse(allocator: Allocator, uri: []const u8) !ParsedUri {
    if (uri.len == 0) return Error.UnparseableUri;

    if (std.mem.startsWith(u8, uri, "red://") or std.mem.eql(u8, uri, "red:") or std.mem.eql(u8, uri, "red:/")) {
        return parseRed(allocator, uri, false);
    }
    if (std.mem.startsWith(u8, uri, "reds://")) {
        return parseRed(allocator, uri, true);
    }
    if (std.mem.startsWith(u8, uri, "memory://") or std.mem.eql(u8, uri, "memory:")) {
        return ParsedUri{ .kind = .embedded, .original_uri = uri };
    }
    if (std.mem.startsWith(u8, uri, "file://")) {
        const path = uri["file://".len..];
        if (path.len == 0) return Error.UnparseableUri;
        return ParsedUri{
            .kind = .embedded,
            .path = try allocator.dupe(u8, path),
            .original_uri = uri,
        };
    }
    if (std.mem.startsWith(u8, uri, "grpc://") or std.mem.startsWith(u8, uri, "grpcs://")) {
        const tls = std.mem.startsWith(u8, uri, "grpcs://");
        const scheme_len: usize = if (tls) "grpcs://".len else "grpc://".len;
        return parseHostPort(allocator, uri, uri[scheme_len..], if (tls) .grpcs else .grpc);
    }
    if (std.mem.startsWith(u8, uri, "http://") or std.mem.startsWith(u8, uri, "https://")) {
        const tls = std.mem.startsWith(u8, uri, "https://");
        const scheme_len: usize = if (tls) "https://".len else "http://".len;
        return parseHostPort(allocator, uri, uri[scheme_len..], if (tls) .https else .http);
    }
    return Error.UnsupportedScheme;
}

fn parseRed(allocator: Allocator, uri: []const u8, secure: bool) !ParsedUri {
    // Embedded shortcuts — no host portion at all.
    if (std.mem.eql(u8, uri, "red:") or std.mem.eql(u8, uri, "red:/") or std.mem.eql(u8, uri, "red://")) {
        return ParsedUri{ .kind = .embedded, .original_uri = uri };
    }
    if (std.mem.eql(u8, uri, "red://memory") or std.mem.eql(u8, uri, "red://memory/") or
        std.mem.eql(u8, uri, "red://:memory") or std.mem.eql(u8, uri, "red://:memory:"))
    {
        return ParsedUri{ .kind = .embedded, .original_uri = uri };
    }
    if (std.mem.startsWith(u8, uri, "red:///")) {
        // red:///abs/path — treat as embedded with a filesystem path.
        const tail = uri["red://".len..]; // includes leading '/'
        // Strip any query string off the path.
        const q = std.mem.indexOfScalar(u8, tail, '?');
        const path_only = if (q) |idx| tail[0..idx] else tail;
        return ParsedUri{
            .kind = .embedded,
            .path = try allocator.dupe(u8, path_only),
            .original_uri = uri,
        };
    }

    const scheme_len: usize = if (secure) "reds://".len else "red://".len;
    const after = uri[scheme_len..];
    return parseAuthority(allocator, uri, after, if (secure) .redwire_tls else .redwire);
}

fn parseHostPort(allocator: Allocator, original: []const u8, after: []const u8, kind: Kind) !ParsedUri {
    return parseAuthority(allocator, original, after, kind);
}

/// Parse `[user[:pass]@]host[:port][/path][?query]`.
fn parseAuthority(
    allocator: Allocator,
    original: []const u8,
    rest: []const u8,
    initial_kind: Kind,
) !ParsedUri {
    var out = ParsedUri{ .kind = initial_kind, .original_uri = original };
    errdefer out.deinit(allocator);

    // Split off the path/query first so userinfo parsing doesn't
    // get confused by an `@` later in the URL.
    const path_start = std.mem.indexOfAny(u8, rest, "/?");
    const authority = if (path_start) |idx| rest[0..idx] else rest;
    const tail = if (path_start) |idx| rest[idx..] else "";

    // Userinfo split.
    var host_part: []const u8 = authority;
    if (std.mem.lastIndexOfScalar(u8, authority, '@')) |at| {
        const userinfo = authority[0..at];
        host_part = authority[at + 1 ..];
        if (std.mem.indexOfScalar(u8, userinfo, ':')) |colon| {
            out.username = try percentDecode(allocator, userinfo[0..colon]);
            out.password = try percentDecode(allocator, userinfo[colon + 1 ..]);
        } else {
            out.username = try percentDecode(allocator, userinfo);
        }
    }

    if (host_part.len == 0) return Error.MissingHost;

    // Host[:port]. IPv6 literal support is omitted intentionally —
    // the same JS reference doesn't handle it either; if a user
    // really needs it they can pass an `[::1]`-bracketed string.
    if (std.mem.lastIndexOfScalar(u8, host_part, ':')) |colon| {
        // Naive guard against an IPv6 literal like `[::1]:5050` —
        // only treat the last colon as a port separator when the
        // tail parses as a number.
        const port_str = host_part[colon + 1 ..];
        const port = std.fmt.parseInt(u16, port_str, 10) catch {
            // Not a port — assume IPv6 hostname.
            out.host = try allocator.dupe(u8, host_part);
            // fall through to query parsing below
            return parseQueryAndFinalise(allocator, &out, tail);
        };
        out.host = try allocator.dupe(u8, host_part[0..colon]);
        out.port = port;
    } else {
        out.host = try allocator.dupe(u8, host_part);
    }

    return parseQueryAndFinalise(allocator, &out, tail);
}

fn parseQueryAndFinalise(allocator: Allocator, out: *ParsedUri, tail: []const u8) !ParsedUri {
    if (tail.len > 0) {
        // Path comes first, then optional `?query`.
        const q = std.mem.indexOfScalar(u8, tail, '?');
        const path_part = if (q) |idx| tail[0..idx] else tail;
        const query_part = if (q) |idx| tail[idx + 1 ..] else "";
        if (path_part.len > 1) out.path = try allocator.dupe(u8, path_part);

        if (query_part.len > 0) {
            try parseQuery(allocator, out, query_part);
        }
    }

    if (out.port == null) out.port = defaultPortFor(out.kind);
    return out.*;
}

fn parseQuery(allocator: Allocator, out: *ParsedUri, query: []const u8) !void {
    var it = std.mem.splitScalar(u8, query, '&');
    while (it.next()) |pair| {
        if (pair.len == 0) continue;
        const eq = std.mem.indexOfScalar(u8, pair, '=') orelse continue;
        const key = pair[0..eq];
        const val = pair[eq + 1 ..];
        if (std.mem.eql(u8, key, "proto")) {
            const k = try resolveProto(val);
            out.kind = switch (k) {
                .pg => .pg,
                .http => .http,
                .https => .https,
                .grpc => .grpc,
                .grpcs => .grpcs,
                .redwire => .redwire,
                .redwire_tls => .redwire_tls,
                .embedded => .embedded,
            };
            out.port = out.port orelse defaultPortFor(out.kind);
        } else if (std.mem.eql(u8, key, "token")) {
            out.token = try percentDecode(allocator, val);
        } else if (std.mem.eql(u8, key, "apiKey") or std.mem.eql(u8, key, "api_key")) {
            out.api_key = try percentDecode(allocator, val);
        } else if (std.mem.eql(u8, key, "loginUrl") or std.mem.eql(u8, key, "login_url")) {
            out.login_url = try percentDecode(allocator, val);
        }
    }
}

fn resolveProto(proto: []const u8) !Kind {
    if (proto.len == 0) return .grpc;
    if (std.ascii.eqlIgnoreCase(proto, "grpc")) return .grpc;
    if (std.ascii.eqlIgnoreCase(proto, "grpcs")) return .grpcs;
    if (std.ascii.eqlIgnoreCase(proto, "red")) return .redwire;
    if (std.ascii.eqlIgnoreCase(proto, "reds")) return .redwire_tls;
    if (std.ascii.eqlIgnoreCase(proto, "http")) return .http;
    if (std.ascii.eqlIgnoreCase(proto, "https")) return .https;
    if (std.ascii.eqlIgnoreCase(proto, "pg") or
        std.ascii.eqlIgnoreCase(proto, "postgres") or
        std.ascii.eqlIgnoreCase(proto, "postgresql"))
    {
        return .pg;
    }
    return Error.UnsupportedProto;
}

fn percentDecode(allocator: Allocator, s: []const u8) ![]u8 {
    var out = try std.ArrayList(u8).initCapacity(allocator, s.len);
    errdefer out.deinit();
    var i: usize = 0;
    while (i < s.len) : (i += 1) {
        if (s[i] == '%' and i + 2 < s.len) {
            const hi = std.fmt.charToDigit(s[i + 1], 16) catch {
                try out.append(s[i]);
                continue;
            };
            const lo = std.fmt.charToDigit(s[i + 2], 16) catch {
                try out.append(s[i]);
                continue;
            };
            try out.append((hi << 4) | lo);
            i += 2;
        } else if (s[i] == '+') {
            try out.append(' ');
        } else {
            try out.append(s[i]);
        }
    }
    return out.toOwnedSlice();
}

test "parse red:// host:port" {
    var p = try parse(std.testing.allocator, "red://localhost:5050");
    defer p.deinit(std.testing.allocator);
    try std.testing.expectEqual(Kind.redwire, p.kind);
    try std.testing.expectEqualStrings("localhost", p.host.?);
    try std.testing.expectEqual(@as(u16, 5050), p.port.?);
}

test "parse red:// default port is 5050" {
    var p = try parse(std.testing.allocator, "red://localhost");
    defer p.deinit(std.testing.allocator);
    try std.testing.expectEqual(@as(u16, 5050), p.port.?);
}

test "parse reds:// flips to TLS" {
    var p = try parse(std.testing.allocator, "reds://example.com");
    defer p.deinit(std.testing.allocator);
    try std.testing.expectEqual(Kind.redwire_tls, p.kind);
    try std.testing.expectEqual(@as(u16, 5050), p.port.?);
}
