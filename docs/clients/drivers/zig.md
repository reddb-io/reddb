# Zig driver

Native Zig client for RedDB. Speaks the RedWire binary TCP protocol directly and ships an HTTP fallback that mirrors the JS driver's REST surface.

- **Source:** [`drivers/zig/`](https://github.com/reddb-io/reddb/tree/main/drivers/zig) (build via `zig build`; not yet on a registry)
- **Status:** Preview
- **Zig:** 0.13.x (0.14-dev compatible with minor tweaks)

## Build

```bash
cd drivers/zig
zig build              # static library at zig-out/lib/libreddb.a
zig build test         # all suites
zig build -Dzstd=true  # force-enable zstd (default: auto-detect via pkg-config)
zig build -Dzstd=false # disable zstd; compressed frames surface error.CompressedButNoZstd
```

## Quickstart

```zig
const std = @import("std");
const reddb = @import("reddb");

pub fn main() !void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    const a = gpa.allocator();

    const conn = try reddb.connect(a, "red://localhost:5050", null);
    defer {
        conn.deinit();
        a.destroy(conn);
    }

    const result = try conn.query("SELECT 1");
    defer a.free(result);
    std.debug.print("server said: {s}\n", .{result});
}
```

Every operation that returns `[]const u8` allocates through the supplied allocator. Free with the **same** allocator after you're done.

## Connection URIs

| URI                                            | Transport     |
|------------------------------------------------|---------------|
| `red://host:5050`                              | RedWire (TCP) |
| `reds://host:5050`                             | RedWire + TLS |
| `red://host?proto=https`                       | HTTPS REST    |
| `red:///abs/path/data.rdb`                     | embedded (n/a) |
| `memory://`, `file:///path`, `grpc(s)://host`  | legacy aliases |

Default port for `red://` / `reds://` is **5050** (see [ADR 0001](../../adr/0001-redwire-tcp-protocol.md)).

## Auth

Sync API; one connection = one socket. SCRAM, anonymous, and bearer auth supported via `ConnectOptions` passed to `reddb.connect`.

## Limitations

- **TLS / ALPN.** `std.crypto.tls.Client` in Zig 0.13 doesn't expose ALPN configuration. The driver still wraps the socket in TLS but doesn't advertise `redwire/1`. If your reverse proxy is strict about ALPN, use the [Rust driver](./rust.md) until Zig's stdlib catches up.
- **Embedded mode.** No FFI to the engine — `red:///path` returns `error.UnsupportedScheme`.
- **gRPC / Postgres wire.** Not implemented. Use HTTPS or RedWire instead.

## Production checklist

- Use `reds://` outside localhost.
- Run the server with the [encrypted vault](../../security/vault.md).
- See [Transport TLS](../../security/transport-tls.md) for mTLS / OAuth posture.
- Track credentials in [Secret Inventory](../../operations/secrets.md).

## Driver source

[`drivers/zig/README.md`](https://github.com/reddb-io/reddb/blob/main/drivers/zig/README.md) — layout, build flags, allocator notes.
