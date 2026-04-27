# reddb-zig

Native Zig driver for [RedDB](https://github.com/filipeforattini/reddb). Speaks
the RedWire binary TCP protocol directly and ships an HTTP fallback that
mirrors the JS driver's REST surface.

## Status

- Targets **Zig 0.13.x** (0.14-dev compatible should compile with minor tweaks).
- Wire protocol parity with the Rust / JS / Go / .NET / Java drivers — same
  framing, same handshake, same SCRAM-SHA-256 flow.
- Sync API; one connection = one socket; SCRAM, anonymous, and bearer auth.
- Optional **zstd** compression, auto-detected via `pkg-config` at build time.
- TLS via `std.crypto.tls.Client`. *ALPN advertisement is **not** wired in 0.13.*
  The server side accepts redwire connections without ALPN, so this works in
  practice; if your operator stack requires ALPN you'll need a newer Zig.

## Layout

```
drivers/zig/
  build.zig
  build.zig.zon
  README.md
  src/
    reddb.zig                public entry — `connect(allocator, uri, opts)`
    url.zig                  `red://` URI parser
    errors.zig               driver-wide Error set
    redwire/
      frame.zig              header layout + MessageKind enum
      codec.zig              encode/decode + zstd glue
      scram.zig              RFC 5802 primitives (PBKDF2 + HMAC + SHA via std.crypto)
      conn.zig               sync connection + handshake state machine
    http/
      client.zig             std.http.Client wrapper for REST endpoints
  tests/
    url_test.zig             ≥ 20 URL fixtures
    scram_test.zig           HMAC vector + PBKDF2 + proof round-trip
    frame_test.zig           codec round-trip + edge cases (incl. zstd)
    redwire_conn_test.zig    fake-server handshake state machine
```

## Quick start

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

## URI scheme

| URI                                            | Transport         |
|------------------------------------------------|-------------------|
| `red://host:5050`                              | RedWire (TCP)     |
| `reds://host:5050`                             | RedWire + TLS     |
| `red://host?proto=https`                       | HTTPS REST        |
| `red:///abs/path/data.rdb`                     | embedded (n/a)    |
| `memory://`, `file:///path`, `grpc(s)://host`  | legacy aliases    |

Default port for `red://` / `reds://` is **5050** (the redwire listener
documented in `docs/adr/0001`). Other transports keep the port defaults from the
JS driver.

## Build

```bash
cd drivers/zig
zig build              # static library at zig-out/lib/libreddb.a
zig build test         # all suites
zig build -Dzstd=true  # force-enable zstd (default: auto-detect via pkg-config)
zig build -Dzstd=false # disable zstd; compressed frames will surface
                       # error.CompressedButNoZstd on receive
```

## Limitations

- **TLS / ALPN.** `std.crypto.tls.Client` in 0.13 doesn't expose ALPN
  configuration. The driver still wraps the socket in TLS but doesn't advertise
  `redwire/1`. If your reverse proxy is strict about ALPN, use the Rust driver
  (which uses rustls) until Zig's stdlib catches up.
- **Embedded mode.** No FFI to the engine — `red:///path` returns
  `error.UnsupportedScheme`. Spawn the server binary the way `drivers/js/src/spawn.js`
  does if you need that.
- **gRPC / Postgres wire.** Not implemented; use HTTPS or RedWire instead.
- **Allocator lifetimes.** Every operation that returns `[]const u8` allocates
  through the supplied allocator. Free with the same allocator after you're
  done.

## Reference implementations

When in doubt about wire shapes, the source of truth is:

- `src/wire/redwire/{frame,codec,auth,session,listener}.rs` — server.
- `src/auth/scram.rs` — SCRAM verifier + signatures.
- `drivers/rust/src/redwire/` — companion Rust client; same byte layout.
- `drivers/js/src/redwire.js` — JS client; mirrored test corpus.

## Production deploy

When you're ready to point this driver at a production RedDB cluster:

- **Run RedDB with the encrypted vault** so auth state and
  `red.secret.*` values are protected at rest. See
  [`docs/security/vault.md`](../../docs/security/vault.md).
- **Use Docker secrets or your cloud secret manager** to inject the
  certificate — never bake it into an image. See
  [`docs/getting-started/docker.md`](../../docs/getting-started/docker.md).
- **Track every secret** the driver consumes (bearer tokens, mTLS
  cert + key, OAuth JWTs) in
  [`docs/operations/secrets.md`](../../docs/operations/secrets.md).
- **Use `reds://` (TLS)** or `red://...?tls=true` for any traffic
  crossing the network — never plain `red://` outside localhost.
- **TLS posture, mTLS, OAuth/JWT and reverse-proxy patterns** are
  covered in [`docs/security/transport-tls.md`](../../docs/security/transport-tls.md).
- See [Policies](../../docs/security/policies.md) for IAM-style authorization.
