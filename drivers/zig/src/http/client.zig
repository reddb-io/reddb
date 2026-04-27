// HTTP transport. Mirrors `drivers/js/src/http.js` — same endpoint
// shapes, same Bearer-token convention. Returns the raw response
// body (allocated by the supplied allocator) so callers stay in
// control of JSON parsing.

const std = @import("std");
const Allocator = std.mem.Allocator;

pub const HttpClient = struct {
    allocator: Allocator,
    base_url: []const u8,
    token: ?[]const u8,
    client: std.http.Client,

    pub fn init(allocator: Allocator, base_url: []const u8, token: ?[]const u8) !HttpClient {
        // Strip a trailing slash so endpoint helpers can `++` paths.
        const trimmed = if (base_url.len > 0 and base_url[base_url.len - 1] == '/')
            base_url[0 .. base_url.len - 1]
        else
            base_url;
        const owned_base = try allocator.dupe(u8, trimmed);
        const owned_token = if (token) |t| try allocator.dupe(u8, t) else null;
        return .{
            .allocator = allocator,
            .base_url = owned_base,
            .token = owned_token,
            .client = .{ .allocator = allocator },
        };
    }

    pub fn deinit(self: *HttpClient) void {
        self.client.deinit();
        self.allocator.free(self.base_url);
        if (self.token) |t| self.allocator.free(t);
    }

    pub fn setToken(self: *HttpClient, token: ?[]const u8) !void {
        if (self.token) |t| self.allocator.free(t);
        self.token = if (token) |t| try self.allocator.dupe(u8, t) else null;
    }

    /// GET /admin/health → raw body.
    pub fn getHealth(self: *HttpClient) ![]u8 {
        return self.request(.GET, "/admin/health", null);
    }

    /// POST /auth/login {username,password} → response body
    /// (typically `{ "token": "..." }`).
    pub fn login(self: *HttpClient, username: []const u8, password: []const u8) ![]u8 {
        const body = try std.fmt.allocPrint(
            self.allocator,
            "{{\"username\":\"{s}\",\"password\":\"{s}\"}}",
            .{ username, password },
        );
        defer self.allocator.free(body);
        return self.request(.POST, "/auth/login", body);
    }

    pub fn query(self: *HttpClient, sql: []const u8) ![]u8 {
        const body = try std.fmt.allocPrint(
            self.allocator,
            "{{\"query\":\"{s}\"}}",
            .{sql},
        );
        defer self.allocator.free(body);
        return self.request(.POST, "/query", body);
    }

    pub fn insert(self: *HttpClient, collection: []const u8, payload_json: []const u8) ![]u8 {
        const path = try std.fmt.allocPrint(
            self.allocator,
            "/collections/{s}/rows",
            .{collection},
        );
        defer self.allocator.free(path);
        return self.request(.POST, path, payload_json);
    }

    pub fn bulkInsert(self: *HttpClient, collection: []const u8, rows_json: []const u8) ![]u8 {
        const path = try std.fmt.allocPrint(
            self.allocator,
            "/collections/{s}/bulk/rows",
            .{collection},
        );
        defer self.allocator.free(path);
        const body = try std.fmt.allocPrint(self.allocator, "{{\"rows\":{s}}}", .{rows_json});
        defer self.allocator.free(body);
        return self.request(.POST, path, body);
    }

    pub fn get(self: *HttpClient, collection: []const u8, id: []const u8) ![]u8 {
        const path = try std.fmt.allocPrint(
            self.allocator,
            "/collections/{s}/{s}",
            .{ collection, id },
        );
        defer self.allocator.free(path);
        return self.request(.GET, path, null);
    }

    pub fn delete(self: *HttpClient, collection: []const u8, id: []const u8) ![]u8 {
        const path = try std.fmt.allocPrint(
            self.allocator,
            "/collections/{s}/{s}",
            .{ collection, id },
        );
        defer self.allocator.free(path);
        return self.request(.DELETE, path, null);
    }

    /// Generic request helper. Builds a full URL, attaches the
    /// optional Bearer token, and returns the response body.
    pub fn request(
        self: *HttpClient,
        method: std.http.Method,
        path: []const u8,
        body: ?[]const u8,
    ) ![]u8 {
        const url_buf = try std.fmt.allocPrint(self.allocator, "{s}{s}", .{ self.base_url, path });
        defer self.allocator.free(url_buf);
        const uri = try std.Uri.parse(url_buf);

        var server_header_buffer: [16 * 1024]u8 = undefined;
        var auth_header_buf: [1024]u8 = undefined;
        const auth_header: ?[]const u8 = if (self.token) |t|
            std.fmt.bufPrint(&auth_header_buf, "Bearer {s}", .{t}) catch null
        else
            null;
        var extra_headers_storage: [2]std.http.Header = undefined;
        var extra_count: usize = 0;
        if (auth_header) |h| {
            extra_headers_storage[extra_count] = .{ .name = "authorization", .value = h };
            extra_count += 1;
        }
        if (body != null) {
            extra_headers_storage[extra_count] = .{ .name = "content-type", .value = "application/json" };
            extra_count += 1;
        }

        var req = try self.client.open(method, uri, .{
            .server_header_buffer = &server_header_buffer,
            .extra_headers = extra_headers_storage[0..extra_count],
        });
        defer req.deinit();

        if (body) |b| {
            req.transfer_encoding = .{ .content_length = b.len };
        }
        try req.send();
        if (body) |b| {
            try req.writeAll(b);
            try req.finish();
        }
        try req.wait();

        // Drain body into a fresh allocation owned by the caller.
        var out = std.ArrayList(u8).init(self.allocator);
        errdefer out.deinit();
        var buf: [4096]u8 = undefined;
        while (true) {
            const n = try req.read(&buf);
            if (n == 0) break;
            try out.appendSlice(buf[0..n]);
        }
        if (req.response.status.class() != .success) {
            // Surface non-2xx as an error but keep the body for the
            // caller via `error.HttpStatus` plus stderr trace.
            std.log.scoped(.reddb).warn(
                "HTTP {d} {s} {s}: {s}",
                .{ @intFromEnum(req.response.status), @tagName(method), path, out.items },
            );
            out.deinit();
            return error.HttpStatus;
        }
        return out.toOwnedSlice();
    }
};
