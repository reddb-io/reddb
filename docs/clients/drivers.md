# Client Drivers

Official drivers for talking to a RedDB server (or, where supported, embedding the engine in-process). Pick your language, install the package, point it at a connection string, run a query.

For the wire-level details every driver implements, see
[Connection Strings](./connection-strings.md), [SDK & Client Compatibility](./sdk-compatibility.md), and [ADR 0001 — RedWire](../adr/0001-redwire-tcp-protocol.md).

## Driver matrix

| Language | Package / coordinate | Embedded | RedWire (TCP / TLS / mTLS) | HTTP / HTTPS | gRPC | Status | Page |
|----------|----------------------|:--------:|:--------------------------:|:------------:|:----:|--------|------|
| JavaScript / TypeScript (Node, Deno, browser) | npm: [`@reddb-io/sdk`](https://www.npmjs.com/package/@reddb-io/sdk) | ✅ (stdio bridge) | ✅ | ✅ | ✅ | Stable | [Guide](../guides/javascript-typescript-driver.md) |
| JavaScript / TypeScript (thin remote-only) | npm: [`@reddb-io/client`](https://www.npmjs.com/package/@reddb-io/client) | ❌ | ✅ | ✅ | ✅ | Stable | [Guide](../guides/javascript-typescript-driver.md#reddb-ioclient) |
| Bun (native TCP fast-path) | npm: [`@reddb-io/client-bun`](https://www.npmjs.com/package/@reddb-io/client-bun) | ❌ | ✅ | — | — | Preview | [bun](./drivers/bun.md) |
| Rust | crates.io: [`reddb-io-client`](https://crates.io/crates/reddb-io-client) | ✅ (feature `embedded`) | ✅ | ✅ | ✅ | Stable | [rust](./drivers/rust.md) |
| Python (embedded, PyO3) | PyPI: [`reddb`](https://pypi.org/project/reddb/) | ✅ | — | — | ✅ | Preview | [python](./drivers/python.md) |
| Python (pure asyncio, remote) | PyPI: [`reddb-asyncio`](https://pypi.org/project/reddb-asyncio/) | ❌ | ✅ | ✅ | — | Preview | [python-asyncio](./drivers/python-asyncio.md) |
| Go | `github.com/reddb-io/reddb-go` | ❌ | ✅ | ✅ | — | Preview | [go](./drivers/go.md) |
| PHP | Composer: [`reddb-io/reddb`](https://packagist.org/packages/reddb-io/reddb) | ❌ | ✅ | ✅ | — | Preview | [php](./drivers/php.md) |
| Dart / Flutter | pub.dev: [`reddb`](https://pub.dev/packages/reddb) | ❌ | ✅ (VM only) | ✅ | — | Preview | [dart](./drivers/dart.md) |
| C++ (17+) | source: `drivers/cpp/` (CMake) | ❌ | ✅ | ✅ | — | Preview | [cpp](./drivers/cpp.md) |
| Zig (0.13+) | source: `drivers/zig/` | ❌ | ✅ | ✅ | — | Preview | [zig](./drivers/zig.md) |
| .NET (C# / F#) | NuGet (TBA) | — | — | — | — | Planned ([#tracking][planned]) | — |
| Java | Maven Central (TBA) | — | — | — | — | Planned ([#tracking][planned]) | — |
| Kotlin | Maven Central (TBA) | — | — | — | — | Planned ([#tracking][planned]) | — |

[planned]: https://github.com/reddb-io/reddb/issues?q=is%3Aissue+label%3Adriver

> **Status legend.** *Stable* — version-locked with the engine, covered by CI, semver-stable public API.
> *Preview* — feature-complete for the documented transports but the public surface may still shift in minor releases; pin the version in production.
> *Planned* — scaffold + tracking issue; not usable yet.

## Common connection strings

Every driver accepts the same URI shapes (where the transport applies):

```
red://host:5050                       # RedWire TCP (default port)
reds://host:5050                      # RedWire over TLS, ALPN redwire/1
http://host:8080                      # HTTP REST
https://host:8443                     # HTTPS REST
grpc://host:5055                      # gRPC
grpcs://host:5056                     # gRPC over TLS
red://user:pass@host                  # auto SCRAM (RedWire) or /auth/login (HTTP)
red://host?token=sk-...               # static bearer
reds://host?ca=…&cert=…&key=…         # mTLS
file:///abs/path/data.rdb             # embedded on-disk  (embedded drivers only)
memory://                             # embedded in-memory (embedded drivers only)
```

Full reference: [Connection Strings](./connection-strings.md).

## Pick the right driver for your shape

- **App talks to a remote RedDB server, latency-critical.** Use the RedWire-speaking driver for your language. RedWire gives sub-50µs round-trips on point reads.
- **App needs to embed the engine.** Use a driver that links the engine: `@reddb-io/sdk` (JS, via stdio bridge), `reddb-io-client` Rust (feature `embedded`), or the `reddb` Python PyO3 package.
- **No driver for your language.** Fall back to one of the standard transports — every RedDB server speaks **PostgreSQL wire v3** (use any `libpq`-compatible client), **HTTP REST**, and **gRPC** (generate stubs from [`proto/reddb.proto`](../../proto/reddb.proto)). See [SDK & Client Compatibility](./sdk-compatibility.md) for the verified-client matrix.

## Reporting driver issues

Issues for any driver belong in the central tracker:
[github.com/reddb-io/reddb/issues](https://github.com/reddb-io/reddb/issues) — tag with `driver:<lang>`.
Include the driver version, the connection string (redacted), the failing operation, and a `red doctor --json` snapshot from the server.
