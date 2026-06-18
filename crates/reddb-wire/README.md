# reddb-wire

Transport-agnostic protocol vocabulary for RedDB. This crate is the
shared layer that `reddb-server`, `reddb-client`, and the
official language drivers all depend on.

`reddb-wire` is the canonical protocol crate. RedProtocol is the
logical contract; RedWire is the binary frame format inside that
contract. Server runtime, storage, auth validation, sockets, and task
scheduling stay outside this crate.

## Audience

Pick `reddb-wire` when you need to:

- Parse a RedDB connection string (`red://`, `reds://`, `grpc://`,
  `grpcs://`, `http://`, `https://`, `memory://`, `file://`) into a
  normalised `ConnectionTarget` value.
- Speak the RedWire binary frame protocol — encode/decode `Frame`,
  inspect `MessageKind`, `Flags`, framing constants.
- Build or parse protocol payloads that must be shared by server and
  drivers: legacy binary values, RedWire Hello/HelloAck/Auth payloads,
  stream envelopes, queue-wait envelopes, and topology advertisements.

If you only need a connector that wraps a server, use the
published [`reddb-client`](../reddb-client) driver. It depends on
`reddb-wire` for parsing and frame types.

## What's inside

- `conn_string` — the [connection-string parser][conn-strings].
  Pure function, no I/O, table-driven tests over every documented
  scheme/transport/query parameter.
- `legacy` — the pre-RedWire binary protocol constants, frame header,
  column-name codec, and `WireValue` value codec.
- `redwire::frame` and `redwire::codec` — the RedWire frame layout
  and zstd-aware codec defined by [ADR 0001][adr-0001].
- `redwire::handshake` — Hello, HelloAck, AuthOk, and AuthFail payload
  contracts. Credential validation remains in `reddb-server`.
- `redwire::stream` and `redwire::queue` — JSON payload contracts for
  multiplexed streams and queue wait. Stream registries, leases,
  runtime execution, and cancellation tasks remain in `reddb-server`.
- `replication` — transport-agnostic payload contracts for WAL pull,
  replica ACK, basebackup chunks, catchup mode, and timeline fork
  notices. Applying WAL, writing snapshots, and failover policy remain
  in `reddb-server` / `reddb-file`.
- `topology` — the binary topology advertisement shared by RedWire and
  gRPC.
- Constants: `DEFAULT_PORT_RED`, `DEFAULT_PORT_GRPC`,
  `REDWIRE_MAGIC`, `MAX_KNOWN_MINOR_VERSION`,
  `DEFAULT_REDWIRE_PORT`.

## Shared fixtures

Cross-driver protocol fixtures live under
`../../testdata/conformance/redwire/`. Keep fixtures there when non-Rust
adapters consume them; keep crate-private parser and snapshot fixtures under
`tests/`.

The parameter manifest at
`../../testdata/conformance/redwire/params/manifest.json` is consumed by this
crate, the Rust client gRPC tests, and the official language drivers.

## References

- [ADR 0001 — RedWire][adr-0001]
- [Connection strings][conn-strings]
- [Monorepo structure][monorepo-structure]

[adr-0001]: ../../.red/adr/0001-redwire-tcp-protocol.md
[conn-strings]: ../../docs/clients/connection-strings.md
[monorepo-structure]: ../../docs/dev/monorepo-structure.md
