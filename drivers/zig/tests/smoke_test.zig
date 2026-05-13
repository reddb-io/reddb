const std = @import("std");
const reddb = @import("reddb");

const t = std.testing;

test "live red server parameterized query smoke" {
    var env = try std.process.getEnvMap(t.allocator);
    defer env.deinit();
    if (env.get("RED_SMOKE") == null or !std.mem.eql(u8, env.get("RED_SMOKE").?, "1")) return;
    const red_bin = env.get("RED_BIN") orelse return;

    const port: u16 = 5597;
    var tmp = t.tmpDir(.{});
    defer tmp.cleanup();

    var child = std.process.Child.init(&.{
        red_bin,
        "server",
        "--path",
        "zig-smoke.db",
        "--bind",
        "127.0.0.1:5597",
    }, t.allocator);
    child.cwd_dir = tmp.dir;
    child.stdin_behavior = .Ignore;
    child.stdout_behavior = .Ignore;
    child.stderr_behavior = .Ignore;
    try child.spawn();
    defer {
        _ = child.kill() catch {};
        _ = child.wait() catch {};
    }

    var conn = try waitForConnect(t.allocator, port);
    defer {
        conn.close();
        conn.deinit();
        t.allocator.destroy(conn);
    }

    try conn.ping();
    const select_one = try conn.query("SELECT 1", .{});
    defer t.allocator.free(select_one);
    try t.expect(std.mem.indexOf(u8, select_one, "\"ok\":true") != null);

    const create = try conn.query("CREATE TABLE zig_params (id INT, name TEXT)", .{});
    defer t.allocator.free(create);

    const inserted = try conn.query(
        "INSERT INTO zig_params (id, name) VALUES ($1, $2)",
        .{ @as(i64, 42), "alice" },
    );
    defer t.allocator.free(inserted);

    const selected = try conn.query(
        "SELECT name FROM zig_params WHERE id = $1 AND name = $2",
        .{ @as(i64, 42), "alice" },
    );
    defer t.allocator.free(selected);
    try t.expect(std.mem.indexOf(u8, selected, "alice") != null);
}

fn waitForConnect(allocator: std.mem.Allocator, port: u16) !*reddb.Conn {
    const deadline = std.time.milliTimestamp() + 60_000;
    while (std.time.milliTimestamp() < deadline) {
        const conn = reddb.connect(allocator, "red://127.0.0.1:5597", .{
            .host = "127.0.0.1",
            .port = port,
        }) catch {
            std.time.sleep(50 * std.time.ns_per_ms);
            continue;
        };
        conn.ping() catch {
            conn.close();
            conn.deinit();
            allocator.destroy(conn);
            std.time.sleep(50 * std.time.ns_per_ms);
            continue;
        };
        return conn;
    }
    return error.ServerDidNotAcceptConnections;
}
