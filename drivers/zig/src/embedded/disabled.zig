const std = @import("std");
const Error = @import("../errors.zig").Error;

pub const Conn = struct {
    allocator: std.mem.Allocator,

    pub fn deinit(_: *Conn) void {}
    pub fn close(_: *Conn) void {}
    pub fn closeChecked(_: *Conn) !void {
        return Error.EmbeddedUnavailable;
    }
    pub fn ping(_: *Conn) !void {
        return Error.EmbeddedUnavailable;
    }
    pub fn query(_: *Conn, _: []const u8, _: anytype) ![]const u8 {
        return Error.EmbeddedUnavailable;
    }
};

pub fn connect(_: std.mem.Allocator, _: ?[]const u8) !*Conn {
    return Error.EmbeddedUnavailable;
}
