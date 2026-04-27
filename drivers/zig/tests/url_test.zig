// URL parser fixtures. Mirrors the JS parser's edge cases so the
// same connection strings work everywhere.

const std = @import("std");
const reddb = @import("reddb");
const url = reddb.url;

const t = std.testing;

test "red://host" {
    var p = try url.parse(t.allocator, "red://example.com");
    defer p.deinit(t.allocator);
    try t.expectEqual(url.Kind.redwire, p.kind);
    try t.expectEqualStrings("example.com", p.host.?);
    try t.expectEqual(@as(u16, 5050), p.port.?);
}

test "red://host:port overrides default" {
    var p = try url.parse(t.allocator, "red://example.com:9999");
    defer p.deinit(t.allocator);
    try t.expectEqual(@as(u16, 9999), p.port.?);
}

test "reds:// flips TLS and keeps default port" {
    var p = try url.parse(t.allocator, "reds://example.com");
    defer p.deinit(t.allocator);
    try t.expectEqual(url.Kind.redwire_tls, p.kind);
    try t.expectEqual(@as(u16, 5050), p.port.?);
}

test "red:// with userinfo" {
    var p = try url.parse(t.allocator, "red://alice:s3cret@example.com");
    defer p.deinit(t.allocator);
    try t.expectEqualStrings("alice", p.username.?);
    try t.expectEqualStrings("s3cret", p.password.?);
    try t.expectEqualStrings("example.com", p.host.?);
}

test "red:// with token query" {
    var p = try url.parse(t.allocator, "red://h?token=sk-abc");
    defer p.deinit(t.allocator);
    try t.expectEqualStrings("sk-abc", p.token.?);
}

test "red:// with apiKey query" {
    var p = try url.parse(t.allocator, "red://h?apiKey=ak-xyz");
    defer p.deinit(t.allocator);
    try t.expectEqualStrings("ak-xyz", p.api_key.?);
}

test "red:// with api_key snake-case" {
    var p = try url.parse(t.allocator, "red://h?api_key=ak-xyz");
    defer p.deinit(t.allocator);
    try t.expectEqualStrings("ak-xyz", p.api_key.?);
}

test "red:// with loginUrl override" {
    var p = try url.parse(t.allocator, "red://h?loginUrl=https%3A%2F%2Faux%2Fauth%2Flogin");
    defer p.deinit(t.allocator);
    try t.expectEqualStrings("https://aux/auth/login", p.login_url.?);
}

test "red:// proto=https switches kind" {
    var p = try url.parse(t.allocator, "red://h?proto=https");
    defer p.deinit(t.allocator);
    try t.expectEqual(url.Kind.https, p.kind);
}

test "red:// proto=pg picks default port 5432" {
    var p = try url.parse(t.allocator, "red://h?proto=pg");
    defer p.deinit(t.allocator);
    try t.expectEqual(url.Kind.pg, p.kind);
}

test "red:// proto=grpcs uses 5052 default" {
    var p = try url.parse(t.allocator, "red://h?proto=grpcs");
    defer p.deinit(t.allocator);
    try t.expectEqual(url.Kind.grpcs, p.kind);
}

test "red:// proto=unknown → error" {
    try t.expectError(reddb.Error.UnsupportedProto, url.parse(t.allocator, "red://h?proto=ftp"));
}

test "red:/// embedded path" {
    var p = try url.parse(t.allocator, "red:///var/lib/red.rdb");
    defer p.deinit(t.allocator);
    try t.expectEqual(url.Kind.embedded, p.kind);
    try t.expectEqualStrings("/var/lib/red.rdb", p.path.?);
}

test "red:// alone is embedded" {
    var p = try url.parse(t.allocator, "red://");
    defer p.deinit(t.allocator);
    try t.expectEqual(url.Kind.embedded, p.kind);
}

test "red://memory is embedded" {
    var p = try url.parse(t.allocator, "red://memory");
    defer p.deinit(t.allocator);
    try t.expectEqual(url.Kind.embedded, p.kind);
}

test "memory:// legacy form" {
    var p = try url.parse(t.allocator, "memory://");
    defer p.deinit(t.allocator);
    try t.expectEqual(url.Kind.embedded, p.kind);
}

test "file:// legacy with path" {
    var p = try url.parse(t.allocator, "file:///tmp/x.rdb");
    defer p.deinit(t.allocator);
    try t.expectEqual(url.Kind.embedded, p.kind);
    try t.expectEqualStrings("/tmp/x.rdb", p.path.?);
}

test "grpc:// legacy parses" {
    var p = try url.parse(t.allocator, "grpc://h:5051");
    defer p.deinit(t.allocator);
    try t.expectEqual(url.Kind.grpc, p.kind);
    try t.expectEqual(@as(u16, 5051), p.port.?);
}

test "grpcs:// legacy parses" {
    var p = try url.parse(t.allocator, "grpcs://h");
    defer p.deinit(t.allocator);
    try t.expectEqual(url.Kind.grpcs, p.kind);
    try t.expectEqual(@as(u16, 5052), p.port.?);
}

test "http:// parses host and port" {
    var p = try url.parse(t.allocator, "http://h:8080");
    defer p.deinit(t.allocator);
    try t.expectEqual(url.Kind.http, p.kind);
    try t.expectEqual(@as(u16, 8080), p.port.?);
}

test "https:// default port 8443" {
    var p = try url.parse(t.allocator, "https://h");
    defer p.deinit(t.allocator);
    try t.expectEqual(url.Kind.https, p.kind);
    try t.expectEqual(@as(u16, 8443), p.port.?);
}

test "unknown scheme errors" {
    try t.expectError(reddb.Error.UnsupportedScheme, url.parse(t.allocator, "ftp://x"));
}

test "empty uri errors" {
    try t.expectError(reddb.Error.UnparseableUri, url.parse(t.allocator, ""));
}

test "red:// percent-encoded password" {
    var p = try url.parse(t.allocator, "red://alice:p%40ss@h");
    defer p.deinit(t.allocator);
    try t.expectEqualStrings("p@ss", p.password.?);
}
