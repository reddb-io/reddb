# reddb-client-internal

Internal RedDB client used by the `red` and `red_client` binaries.
Hosts the gRPC connector + REPL that the bins drive.

## Audience

You almost certainly do **not** want to depend on this crate
directly. Two better options:

- **Application code (Rust):** depend on the published
  [`reddb-client`][drivers-rust] driver under
  `drivers/rust`. That crate exposes a stable connection-string
  API and supports embedded, gRPC, HTTP, and RedWire backends.
- **Other languages:** see [`docs/clients/sdk-compatibility.md`][sdk]
  for the full driver matrix.

This `reddb-client-internal` crate exists to keep the workspace
binaries (`red` and `red_client`) on a thin client implementation
without pulling the engine. It is `publish = false` by design.

## What's inside

- `RedDBClient` — gRPC connector with bearer-token auth, a typed
  surface for queries, scans, stats, and lifecycle operations.
- `repl` — the interactive REPL (`red>` prompt) used by both bins
  when no `-c` / `--command` is given.
- `bin/red_client.rs` — the thin client binary itself. Uses
  [`reddb-wire`](../reddb-wire) to parse connection strings and
  rejects every embedded scheme with exit code 2 + a message
  pointing the user at the full `red` binary.

## Schemes

Accepted by `red_client`:

- `red://host[:port]`   — RedWire / gRPC default port 5050
- `reds://host[:port]`  — RedWire-over-TLS
- `grpc://host[:port]`  — gRPC plain (default port 5055)
- `grpcs://host[:port]` — gRPC + TLS

Rejected (use `red`):

- `memory://`, `memory:`, `file:///path`
- `red://`, `red:///path`, `red://:memory:`

See [`docs/clients/connection-strings.md`][conn-strings] for the
full URL grammar.

## Size budget

`red_client` is guarded by `crates/reddb-client/SIZE_BUDGET`
(stripped release bytes). The CI step
`./scripts/check-red-client-size.sh` enforces it on every PR; the
budget exists to catch accidental engine re-linkage.

## References

- [Connection strings][conn-strings]
- [ADR 0001 — RedWire][adr-0001]
- [Workspace migration guide](../../docs/migration/workspace-split.md)

[adr-0001]: ../../docs/adr/0001-redwire-tcp-protocol.md
[conn-strings]: ../../docs/clients/connection-strings.md
[drivers-rust]: ../../drivers/rust
[sdk]: ../../docs/clients/sdk-compatibility.md
