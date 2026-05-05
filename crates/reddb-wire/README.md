# reddb-wire

Transport-agnostic protocol vocabulary for RedDB. This crate is the
shared layer that `reddb-server`, `reddb-client`, and the
official language drivers all depend on.

## Audience

Pick `reddb-wire` when you need to:

- Parse a RedDB connection string (`red://`, `reds://`, `grpc://`,
  `grpcs://`, `http://`, `https://`, `memory://`, `file://`) into a
  normalised `ConnectionTarget` value.
- Speak the RedWire binary frame protocol — encode/decode `Frame`,
  inspect `MessageKind`, `Flags`, framing constants.

If you only need a connector that wraps a server, use the
published [`reddb-client`](../reddb-client) driver. It depends on
`reddb-wire` for parsing and frame types.

## What's inside

- `conn_string` — the [connection-string parser][conn-strings].
  Pure function, no I/O, table-driven tests over every documented
  scheme/transport/query parameter.
- `redwire::frame` and `redwire::codec` — the RedWire frame layout
  and zstd-aware codec defined by [ADR 0001][adr-0001].
- Constants: `DEFAULT_PORT_RED`, `DEFAULT_PORT_GRPC`,
  `REDWIRE_MAGIC`, `MAX_KNOWN_MINOR_VERSION`,
  `DEFAULT_REDWIRE_PORT`.

## References

- [ADR 0001 — RedWire][adr-0001]
- [Connection strings][conn-strings]

[adr-0001]: ../../docs/adr/0001-redwire-tcp-protocol.md
[conn-strings]: ../../docs/clients/connection-strings.md
