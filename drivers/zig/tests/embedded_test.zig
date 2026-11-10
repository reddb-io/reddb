const std = @import("std");
const reddb = @import("reddb");

const t = std.testing;

test "embedded memory supports parameterized queries" {
    const conn = try reddb.connect(t.allocator, "memory://", null);
    defer destroyConn(t.allocator, conn);

    const created = try conn.query("CREATE TABLE zig_embedded (id INTEGER, name TEXT)", .{});
    defer t.allocator.free(created);
    const inserted = try conn.query(
        "INSERT INTO zig_embedded (id, name) VALUES ($1, $2)",
        .{ @as(i64, 7), "embedded" },
    );
    defer t.allocator.free(inserted);
    const selected = try conn.query(
        "SELECT id, name FROM zig_embedded WHERE id = $1",
        .{@as(i64, 7)},
    );
    defer t.allocator.free(selected);
    try t.expect(std.mem.indexOf(u8, selected, "embedded") != null);
    try conn.ping();
}

test "embedded file rejects a second writer and reopens as one artifact" {
    var tmp = t.tmpDir(.{});
    defer tmp.cleanup();
    const directory = try tmp.dir.realpathAlloc(t.allocator, ".");
    defer t.allocator.free(directory);

    const path = try std.fs.path.join(t.allocator, &.{ directory, "zig-contract.rdb" });
    defer t.allocator.free(path);
    const uri = try std.fmt.allocPrint(t.allocator, "file://{s}", .{path});
    defer t.allocator.free(uri);

    {
        const first = try reddb.connect(t.allocator, uri, null);
        defer destroyConn(t.allocator, first);
        try t.expectError(reddb.Error.EmbeddedOpen, reddb.connect(t.allocator, uri, null));

        const created = try first.query("CREATE TABLE zig_persist (id INTEGER, name TEXT)", .{});
        defer t.allocator.free(created);
        const inserted = try first.query(
            "INSERT INTO zig_persist (id, name) VALUES (9, 'survives')",
            .{},
        );
        defer t.allocator.free(inserted);
        try first.closeChecked();
    }

    {
        const reopened = try reddb.connect(t.allocator, uri, null);
        defer destroyConn(t.allocator, reopened);
        const selected = try reopened.query("SELECT name FROM zig_persist WHERE id = 9", .{});
        defer t.allocator.free(selected);
        try t.expect(std.mem.indexOf(u8, selected, "survives") != null);
        try reopened.closeChecked();
    }

    var dir = try std.fs.openDirAbsolute(directory, .{ .iterate = true });
    defer dir.close();
    var entries = dir.iterate();
    var count: usize = 0;
    var saw_database = false;
    while (try entries.next()) |entry| {
        count += 1;
        if (std.mem.eql(u8, entry.name, "zig-contract.rdb")) saw_database = true;
    }
    try t.expectEqual(@as(usize, 1), count);
    try t.expect(saw_database);
}

fn destroyConn(allocator: std.mem.Allocator, conn: *reddb.Conn) void {
    conn.deinit();
    allocator.destroy(conn);
}
