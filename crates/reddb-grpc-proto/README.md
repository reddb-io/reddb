# reddb-grpc-proto

Generated gRPC protobuf types and tonic client/server stubs for
RedDB. The `.proto` source lives in this crate's `proto/` directory
and is compiled by `tonic-prost-build` at build time.

## Audience

This crate is consumed by:

- `reddb-server` — server-side dispatch handlers.
- `reddb-client` — gRPC connector used by the `red` and
  `red_client` binaries (via the workspace-internal
  `reddb-client-connector` sibling).

You usually want one of those higher-level crates instead of
depending on `reddb-grpc-proto` directly. The crate exists so the
two sides can share generated types without one depending on the
other (which would form a dependency cycle).

## What's inside

- `proto/reddb.proto` — single source of truth for the RedDB gRPC
  surface (RedDb service, all request/reply messages).
- `src/lib.rs` — `tonic::include_proto!("reddb.v1")` re-exports
  every generated type at the crate root.

## References

- [Connection strings][conn-strings] — gRPC vs RedWire vs HTTP transports
- [ADR 0001 — RedWire][adr-0001] — companion binary protocol

[adr-0001]: ../../docs/adr/0001-redwire-tcp-protocol.md
[conn-strings]: ../../docs/clients/connection-strings.md
