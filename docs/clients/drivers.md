# Client Drivers

Official drivers for talking to a RedDB server (or, where supported, embedding the engine in-process). Pick your language, install the package, point it at a connection string, run a query.

For the wire-level details every driver implements, see
[Connection Strings](./connection-strings.md), [SDK & Client Compatibility](./sdk-compatibility.md), and [ADR 0001 — RedWire](../../.red/adr/0001-redwire-tcp-protocol.md).

The JS/TS remote client and Rust `Reddb::connect` support
[topology discovery and replica routing](./topology-routing.md) across their
remote transports. The guide describes the server advertisements, refresh,
transaction pinning, consistency options, and the scope of driver support.

## Driver matrix

| Language | Package / coordinate | Embedded | RedWire (TCP / TLS / mTLS) | HTTP / HTTPS | gRPC | Status | Page |
|----------|----------------------|:--------:|:--------------------------:|:------------:|:----:|--------|------|
| JavaScript / TypeScript (Node) | npm: [`@reddb-io/sdk`](https://www.npmjs.com/package/@reddb-io/sdk) | ✅ (native addon) | — | — | — | Stable | [Guide](../guides/javascript-typescript-driver.md) |
| JavaScript / TypeScript (thin remote-only) | npm: [`@reddb-io/client`](https://www.npmjs.com/package/@reddb-io/client) | ❌ | ✅ | ✅ | ✅ | Stable | [Guide](../guides/javascript-typescript-driver.md#reddb-ioclient) |
| Bun (native TCP fast-path) | npm: [`@reddb-io/client-bun`](https://www.npmjs.com/package/@reddb-io/client-bun) | ❌ | ✅ | — | — | Preview | [bun](./drivers/bun.md) |
| Rust | crates.io: [`reddb-io-client`](https://crates.io/crates/reddb-io-client) | ✅ (feature `embedded`) | ✅ | ✅ | ✅ | Stable | [rust](./drivers/rust.md) |
| Python (embedded, PyO3) | PyPI: [`reddb`](https://pypi.org/project/reddb/) | ✅ | — | — | ✅ | Preview | [python](./drivers/python.md) |
| Python (pure asyncio, remote) | PyPI: [`reddb-asyncio`](https://pypi.org/project/reddb-asyncio/) | ❌ | ✅ | ✅ | — | Preview | [python-asyncio](./drivers/python-asyncio.md) |
| Go | `github.com/reddb-io/reddb-go` | ✅ (shared ABI) | ✅ | ✅ | — | Preview | [go](./drivers/go.md) |
| PHP | Composer: [`reddb-io/reddb`](https://packagist.org/packages/reddb-io/reddb) | ✅ (FFI + shared ABI) | ✅ | ✅ | — | Preview | [php](./drivers/php.md) |
| Dart / Flutter | pub.dev: [`reddb`](https://pub.dev/packages/reddb) | ✅ (native VM + shared ABI) | ✅ (VM only) | ✅ | — | Preview | [dart](./drivers/dart.md) |
| C++ (17+) | source: `drivers/cpp/` (CMake) | ✅ (shared ABI) | ✅ | ✅ | — | Preview | [cpp](./drivers/cpp.md) |
| Zig (0.14.1+) | source: `drivers/zig/` | ✅ (shared ABI) | ✅ | ✅ | — | Preview | [zig](./drivers/zig.md) |
| .NET (C# / F#) | NuGet: `Reddb` | ✅ (shared ABI) | ✅ | ✅ | — | Preview | [dotnet](../../drivers/dotnet/README.md) |
| Java | Maven: `dev.reddb:reddb-jvm` | ✅ (JNA + shared ABI) | ✅ | ✅ | — | Preview | [java](../../drivers/java/README.md) |
| Kotlin | Maven: `dev.reddb:reddb-kotlin` | ✅ (JNA + shared ABI) | ✅ | ✅ | — | Preview | [kotlin](../../drivers/kotlin/README.md) |

> **Status legend.** *Stable* — version-locked with the engine, covered by CI, semver-stable public API.
> *Preview* — feature-complete for the documented transports but the public surface may still shift in minor releases; pin the version in production.
> *Planned* — scaffold + tracking issue; not usable yet.

### Deployment modes are a separate contract

The transport table above does **not** establish support for the four RedDB
deployment modes. A remote driver that dials one address is not thereby capable
of replica promotion or cluster ownership routing, and separate embedded and
remote packages do not make one language facade complete.

The release-gated source of truth is the
[`deployment-mode-matrix.json`](../conformance/deployment-mode-matrix.json),
defined by [ADR 0081](../../.red/adr/0081-four-deployment-mode-driver-contract.md).
It accounts for **serverless**, **embedded single-file**, **server with read
replicas**, and **cluster** for every official language facade. At the time of
this matrix, no facade is four-mode ready. Every listed facade now has a
qualified embedded journey; the remaining serverless, replica, and cluster
cells retain explicit gaps. Native-language drivers use the version-matched
shared ABI from stable release assets. A mode becomes qualified
only through its executable integration journey.

## Common connection strings

Every driver accepts the same URI shapes (where the transport applies):

```
red://host:5050                       # RedWire TCP (default port)
reds://host:5050                      # RedWire over TLS, ALPN redwire/1
wss://host                            # RedWire over WebSocket for browsers
ws://localhost                        # RedWire over WebSocket for local browser dev
http://host:5000                      # HTTP REST
https://host:55555                     # HTTPS REST
grpc://host:55055                      # gRPC
grpcs://host:55555                     # gRPC over TLS
red://user:pass@host                  # auto SCRAM (RedWire) or /auth/login (HTTP)
red://host?token=sk-...               # static bearer
reds://host?ca=…&cert=…&key=…         # mTLS
file:///abs/path/data.rdb             # embedded on-disk  (embedded drivers only)
memory://                             # embedded in-memory (embedded drivers only)
```

Full reference: [Connection Strings](./connection-strings.md).

## Pick the right driver for your shape

- **App talks to a remote RedDB server, latency-critical.** Use the RedWire-speaking driver for your language. RedWire gives sub-50µs round-trips on point reads.
- **App needs to embed the engine.** Use `memory://` or `file://` with the public facade and install the version-matched native component documented for that driver.
- **No driver for your language.** Fall back to one of the standard transports — every RedDB server speaks **PostgreSQL wire v3** (use any `libpq`-compatible client), **HTTP REST**, and **gRPC** (generate stubs from [`proto/reddb.proto`](../../proto/reddb.proto)). See [SDK & Client Compatibility](./sdk-compatibility.md) for the verified-client matrix.

## Reporting driver issues

Issues for any driver belong in the central tracker:
[github.com/reddb-io/reddb/issues](https://github.com/reddb-io/reddb/issues) — tag with `driver:<lang>`.
Include the driver version, the connection string (redacted), the failing operation, and a `red doctor --json` snapshot from the server.

<!-- contract-matrix:begin -->
## Public-surface support

> Generated from [`docs/conformance/public-surface-contract-matrix.json`](/docs/conformance/public-surface-contract-matrix.json) by `scripts/gen-docs-from-matrix.mjs`. Do not edit between the markers by hand — run `node scripts/gen-docs-from-matrix.mjs --write`. The matrix is the source of truth; this block can never claim more than it, and CI (`docs-matrix`) fails on drift.
>
> The public promises this document makes, and the status of each surface.

| Promise | sql | http | redwire | grpc | driver_helpers |
| --- | --- | --- | --- | --- | --- |
| **PSC-013** — Official drivers implement the SDK Helper Spec v1.0 conformance suite (all 22 §12 case IDs). | ❌ unsupported | ❌ unsupported | ❌ unsupported | ✅ supported | ✅ supported |

_Status legend: ✅ supported · ⚠️ partial (known gaps) · ❌ unsupported._
<!-- contract-matrix:end -->
