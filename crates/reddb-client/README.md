# reddb-client

Official Rust client for [RedDB](https://github.com/reddb-io/reddb).
One connection-string API across embedded, gRPC, HTTP, and
RedWire transports. Also hosts the `red_client` binary and the
workspace-internal connector used by `red`'s REPL and
`reddb-server`'s rpc_stdio mode.

## Quickstart (library)

```toml
[dependencies]
reddb-client = "0.2"
```

```rust,no_run
use reddb_client::{Reddb, JsonValue};

# async fn run() -> reddb_client::Result<()> {
let db = Reddb::connect("memory://").await?;
db.insert("users", &JsonValue::object([("name", JsonValue::string("Alice"))])).await?;
let result = db.query("SELECT * FROM users").await?;
println!("{} rows", result.rows.len());
db.close().await?;
# Ok(())
# }
```

## Cargo features

- `embedded` (default) — pulls the engine in-process for
  `memory://` / `file:///` URIs.
- `grpc` — async tonic client for `grpc://` / `red://`.
- `http` — REST client over reqwest+rustls for `http://` / `https://`.
- `redwire` — native RedWire TCP client (no engine).
- `redwire-tls` — adds TLS / mTLS to the RedWire transport.

Disable defaults to drop the engine: `default-features = false`.

## `red_client` binary

The crate also hosts the `red_client` binary (built with
`cargo build -p reddb-io-client --bin red_client --no-default-features`).
It is the thin remote-only client used by ops tooling — no
engine, no embedded backend, just transports:

| Scheme              | Status     |
|---------------------|------------|
| `red://host[:port]` | gRPC, default port 5050 |
| `reds://host[:port]`| TODO (TLS not yet wired in the bin) |
| `grpc://host[:port]`| gRPC, default port 5055 |
| `http://host[:port]`| REST |
| `memory:` / `file://` | rejected (exit 2, points to `red`) |

`red_client` is guarded by [`SIZE_BUDGET`](./SIZE_BUDGET) (stripped
release bytes); CI runs `./scripts/check-red-client-size.sh` on
every PR to catch accidental engine re-linkage.

## Module layout

- `crate::Reddb` / `JsonValue` / `ClientError` — published
  high-level API.
- `crate::connect::{Target, parse}` — back-compat shim over
  [`reddb-wire`'s][rw] connection-string parser.
- `crate::connector::{RedDBClient, repl, http, redwire}` —
  workspace-internal connector consumed by the `red` REPL,
  `red_client` bin, and `reddb-server`'s rpc_stdio mode. The
  gRPC connector type itself lives in the
  [`reddb-client-connector`](../reddb-client-connector) sibling
  crate to break a path-dependency cycle.

## References

- [Connection strings][conn-strings]
- [ADR 0001 — RedWire][adr-0001]
- [Workspace migration guide](../../docs/migration/workspace-split.md)

[adr-0001]: ../../docs/adr/0001-redwire-tcp-protocol.md
[conn-strings]: ../../docs/clients/connection-strings.md
[rw]: ../reddb-wire
[sdk]: ../../docs/clients/sdk-compatibility.md
