// Build script for the RedDB Zig driver.
//
// The driver is published as a single `reddb` module exposing the
// public API in `src/reddb.zig`. Tests live next to code (in-file
// `test` blocks) plus dedicated suites under `tests/` that pull the
// module in. zstd support is optional; when the host system has
// libzstd we link against it, otherwise the codec returns
// `error.CompressedButNoZstd` whenever a peer ships compressed
// frames.

const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    const enable_zstd = b.option(
        bool,
        "zstd",
        "Link libzstd for compressed redwire frames (default: auto-detect)",
    ) orelse detectZstd(b);

    const reddb_mod = b.addModule("reddb", .{
        .root_source_file = b.path("src/reddb.zig"),
        .target = target,
        .optimize = optimize,
    });

    const build_options = b.addOptions();
    build_options.addOption(bool, "enable_zstd", enable_zstd);
    reddb_mod.addOptions("build_options", build_options);

    if (enable_zstd) {
        reddb_mod.linkSystemLibrary("zstd", .{});
    }

    // Static library artifact — gives downstream consumers something
    // to link against without re-compiling the driver from source.
    const lib = b.addStaticLibrary(.{
        .name = "reddb",
        .root_source_file = b.path("src/reddb.zig"),
        .target = target,
        .optimize = optimize,
    });
    lib.root_module.addOptions("build_options", build_options);
    if (enable_zstd) {
        lib.linkSystemLibrary("zstd");
        lib.linkLibC();
    }
    b.installArtifact(lib);

    // Test step — runs the in-file test blocks plus the explicit
    // suites under `tests/`. They share the same module so test
    // helpers can `@import("reddb")` for the public surface.
    const test_step = b.step("test", "Run all driver tests");

    const test_files = [_][]const u8{
        "src/reddb.zig",
        "tests/url_test.zig",
        "tests/scram_test.zig",
        "tests/frame_test.zig",
        "tests/redwire_conn_test.zig",
    };

    for (test_files) |path| {
        const t = b.addTest(.{
            .root_source_file = b.path(path),
            .target = target,
            .optimize = optimize,
        });
        // The reddb module already carries `build_options`; tests
        // that need the flag re-export it through `reddb.build_options`.
        // Attaching it again here would trip Zig's "file exists in
        // multiple modules" guard when the test also imports reddb.
        if (std.mem.eql(u8, path, "src/reddb.zig")) {
            t.root_module.addOptions("build_options", build_options);
        } else {
            t.root_module.addImport("reddb", reddb_mod);
        }
        if (enable_zstd) {
            t.linkSystemLibrary("zstd");
            t.linkLibC();
        }
        const run_t = b.addRunArtifact(t);
        test_step.dependOn(&run_t.step);
    }
}

fn detectZstd(b: *std.Build) bool {
    // Cheap probe: ask pkg-config and trust its exit code. When
    // pkg-config itself is missing we fall back to "no zstd" so the
    // build succeeds on minimal CI images.
    _ = b;
    var child = std.process.Child.init(&.{ "pkg-config", "--exists", "libzstd" }, std.heap.page_allocator);
    child.stderr_behavior = .Ignore;
    child.stdout_behavior = .Ignore;
    const term = child.spawnAndWait() catch return false;
    return switch (term) {
        .Exited => |code| code == 0,
        else => false,
    };
}
