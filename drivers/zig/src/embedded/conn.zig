const std = @import("std");
const Error = @import("../errors.zig").Error;
const redwire_conn = @import("../redwire/conn.zig");
const value_codec = @import("../redwire/value_codec.zig");

const Handle = opaque {};
const REDDB_OK: c_int = 0;
const REDDB_ABI_VERSION: u32 = 1;

extern fn reddb_abi_version() u32;
extern fn reddb_open(
    path: ?[*:0]const u8,
    out_handle: *?*Handle,
    out_error: *?[*:0]u8,
) c_int;
extern fn reddb_invoke(
    handle: *Handle,
    method: [*:0]const u8,
    params_json: [*:0]const u8,
    out_result: *?[*:0]u8,
    out_error: *?[*:0]u8,
) c_int;
extern fn reddb_close(handle: *Handle, out_error: *?[*:0]u8) c_int;
extern fn reddb_string_free(value: ?[*:0]u8) void;

pub const Conn = struct {
    allocator: std.mem.Allocator,
    handle: ?*Handle,
    mutex: std.Thread.Mutex = .{},

    pub fn deinit(self: *Conn) void {
        self.close();
    }

    pub fn close(self: *Conn) void {
        self.closeChecked() catch {};
    }

    pub fn closeChecked(self: *Conn) !void {
        self.mutex.lock();
        defer self.mutex.unlock();
        const handle = self.handle orelse return;
        self.handle = null;
        var error_message: ?[*:0]u8 = null;
        const status = reddb_close(handle, &error_message);
        freeFfiString(error_message);
        if (status != REDDB_OK) return Error.EmbeddedCall;
    }

    pub fn ping(self: *Conn) !void {
        const result = try self.invoke("health", "{}");
        self.allocator.free(result);
    }

    pub fn query(self: *Conn, sql: []const u8, params: anytype) ![]const u8 {
        const ParamType = @TypeOf(params);
        switch (@typeInfo(ParamType)) {
            .@"struct" => |info| {
                if (!info.is_tuple) @compileError("query params must be a tuple or []const Value");
                const values = try self.allocator.alloc(value_codec.Value, info.fields.len);
                defer self.allocator.free(values);
                inline for (info.fields, 0..) |field, i| {
                    values[i] = try redwire_conn.nativeParamToValue(@field(params, field.name));
                }
                return self.queryValues(sql, values);
            },
            .pointer => |ptr| switch (ptr.size) {
                .slice => {
                    if (ptr.child != value_codec.Value) {
                        @compileError("query params must be []const Value");
                    }
                    return self.queryValues(sql, params);
                },
                .one => switch (@typeInfo(ptr.child)) {
                    .array => |array| {
                        if (array.child != value_codec.Value) {
                            @compileError("query params must be a Value array pointer");
                        }
                        return self.queryValues(sql, params[0..]);
                    },
                    else => @compileError("query params must be a tuple or []const Value"),
                },
                else => @compileError("query params must be a tuple or []const Value"),
            },
            else => @compileError("query params must be a tuple or []const Value"),
        }
    }

    fn queryValues(self: *Conn, sql: []const u8, params: []const value_codec.Value) ![]const u8 {
        const json = try value_codec.toRpcQueryParams(self.allocator, sql, params);
        defer self.allocator.free(json);
        return self.invoke("query", json);
    }

    fn invoke(self: *Conn, method: [*:0]const u8, params: []const u8) ![]const u8 {
        self.mutex.lock();
        defer self.mutex.unlock();
        const handle = self.handle orelse return Error.ConnectionClosed;
        const params_z = try self.allocator.dupeZ(u8, params);
        defer self.allocator.free(params_z);

        var result: ?[*:0]u8 = null;
        var error_message: ?[*:0]u8 = null;
        const status = reddb_invoke(handle, method, params_z.ptr, &result, &error_message);
        freeFfiString(error_message);
        if (status != REDDB_OK) {
            freeFfiString(result);
            return Error.EmbeddedCall;
        }
        const result_ptr = result orelse return Error.EmbeddedCall;
        defer reddb_string_free(result_ptr);
        return self.allocator.dupe(u8, std.mem.span(result_ptr));
    }
};

pub fn connect(allocator: std.mem.Allocator, path: ?[]const u8) !*Conn {
    if (reddb_abi_version() != REDDB_ABI_VERSION) return Error.EmbeddedAbiMismatch;
    const path_z = if (path) |value| try allocator.dupeZ(u8, value) else null;
    defer if (path_z) |value| allocator.free(value);

    var handle: ?*Handle = null;
    var error_message: ?[*:0]u8 = null;
    const status = reddb_open(
        if (path_z) |value| value.ptr else null,
        &handle,
        &error_message,
    );
    freeFfiString(error_message);
    if (status != REDDB_OK or handle == null) return Error.EmbeddedOpen;
    errdefer {
        var close_error: ?[*:0]u8 = null;
        _ = reddb_close(handle.?, &close_error);
        freeFfiString(close_error);
    }

    const conn = try allocator.create(Conn);
    conn.* = .{ .allocator = allocator, .handle = handle };
    return conn;
}

fn freeFfiString(value: ?[*:0]u8) void {
    if (value != null) reddb_string_free(value);
}
