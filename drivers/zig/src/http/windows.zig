// WinHTTP supplies bounded HTTP(S) I/O on Windows. Zig 0.14's HTTP client
// reads sockets with synchronous ReadFile, which ignores Winsock read timeouts.
const std = @import("std");
const Handle = *anyopaque;
extern "winhttp" fn WinHttpOpen([*:0]const u16, u32, ?[*:0]const u16, ?[*:0]const u16, u32) callconv(.winapi) ?Handle;
extern "winhttp" fn WinHttpConnect(Handle, [*:0]const u16, u16, u32) callconv(.winapi) ?Handle;
extern "winhttp" fn WinHttpOpenRequest(Handle, [*:0]const u16, [*:0]const u16, ?[*:0]const u16, ?[*:0]const u16, ?*anyopaque, u32) callconv(.winapi) ?Handle;
extern "winhttp" fn WinHttpSetTimeouts(Handle, i32, i32, i32, i32) callconv(.winapi) i32;
extern "winhttp" fn WinHttpSetOption(Handle, u32, *const anyopaque, u32) callconv(.winapi) i32;
extern "winhttp" fn WinHttpSendRequest(Handle, [*:0]const u16, u32, ?*const anyopaque, u32, u32, usize) callconv(.winapi) i32;
extern "winhttp" fn WinHttpReceiveResponse(Handle, ?*anyopaque) callconv(.winapi) i32;
extern "winhttp" fn WinHttpQueryHeaders(Handle, u32, ?[*:0]const u16, *anyopaque, *u32, ?*u32) callconv(.winapi) i32;
extern "winhttp" fn WinHttpReadData(Handle, [*]u8, u32, *u32) callconv(.winapi) i32;
extern "winhttp" fn WinHttpCloseHandle(Handle) callconv(.winapi) i32;

pub fn request(allocator: std.mem.Allocator, uri: std.Uri, url: []const u8, method: std.http.Method, token: ?[]const u8, body: ?[]const u8, timeout_ms: u32) ![]u8 {
    if (body) |value| if (value.len > 16 * 1024 * 1024) return error.FrameTooLarge;
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const host = try uri.host.?.toRawMaybeAlloc(arena.allocator());
    const hostname = try std.unicode.utf8ToUtf16LeAllocZ(allocator, host);
    defer allocator.free(hostname);
    const verb = try std.unicode.utf8ToUtf16LeAllocZ(allocator, @tagName(method));
    defer allocator.free(verb);
    const start = std.mem.indexOfScalarPos(u8, url, uri.scheme.len + 3, '/') orelse url.len;
    const path = try std.unicode.utf8ToUtf16LeAllocZ(allocator, if (start == url.len) "/" else url[start..]);
    defer allocator.free(path);
    const headers = if (token) |value| try std.fmt.allocPrint(allocator, "Authorization: Bearer {s}\r\nContent-Type: application/json\r\n", .{value}) else try allocator.dupe(u8, "Content-Type: application/json\r\n");
    defer allocator.free(headers);
    if (token) |value| if (std.mem.indexOfAny(u8, value, "\r\n") != null) return error.AuthRefused;
    const wide_headers = try std.unicode.utf8ToUtf16LeAllocZ(allocator, headers);
    defer allocator.free(wide_headers);
    const agent = std.unicode.utf8ToUtf16LeStringLiteral("reddb-zig");
    const session = WinHttpOpen(agent, 1, null, null, 0) orelse return error.ConnectionRefused;
    defer _ = WinHttpCloseHandle(session);
    if (WinHttpSetTimeouts(session, 5000, 5000, @intCast(timeout_ms), @intCast(timeout_ms)) == 0) return error.ConnectionTimedOut;
    const tls = std.mem.eql(u8, uri.scheme, "https");
    const connection = WinHttpConnect(session, hostname, uri.port orelse (if (tls) @as(u16, 443) else @as(u16, 80)), 0) orelse return error.ConnectionRefused;
    defer _ = WinHttpCloseHandle(connection);
    const req = WinHttpOpenRequest(connection, verb, path, null, null, null, if (tls) 0x00800000 else 0) orelse return error.ConnectionRefused;
    defer _ = WinHttpCloseHandle(req);
    const redirect_never: u32 = 0;
    if (WinHttpSetOption(req, 88, &redirect_never, @sizeOf(u32)) == 0) return error.ConnectionRefused;
    const length: u32 = @intCast(if (body) |value| value.len else 0);
    if (WinHttpSendRequest(req, wide_headers, @intCast(wide_headers.len), if (body) |value| @ptrCast(value.ptr) else null, length, length, 0) == 0) return error.ConnectionTimedOut;
    if (WinHttpReceiveResponse(req, null) == 0) return error.ConnectionTimedOut;
    var status: u32 = 0;
    var status_size: u32 = @sizeOf(u32);
    if (WinHttpQueryHeaders(req, 19 | 0x20000000, null, &status, &status_size, null) == 0) return error.InvalidHttpResponse;
    if (status == 401 or status == 403) return error.AuthRefused;
    if (status < 200 or status >= 300) return error.HttpStatus;
    var output = std.ArrayList(u8).init(allocator);
    errdefer output.deinit();
    var buffer: [4096]u8 = undefined;
    const deadline = std.time.milliTimestamp() + timeout_ms;
    while (true) {
        if (timeout_ms != 0 and std.time.milliTimestamp() >= deadline) return error.ConnectionTimedOut;
        var count: u32 = 0;
        if (WinHttpReadData(req, &buffer, buffer.len, &count) == 0) return error.ConnectionTimedOut;
        if (count == 0) break;
        if (count > 16 * 1024 * 1024 - output.items.len) return error.FrameTooLarge;
        try output.appendSlice(buffer[0..count]);
    }
    return output.toOwnedSlice();
}
