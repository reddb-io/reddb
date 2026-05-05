# reddb-client-connector

Workspace-internal gRPC connector for [RedDB](https://github.com/reddb-io/reddb).

This crate exists to break a path-dependency cycle between
`reddb-client` (the published Rust driver, which pulls in
`reddb-server` under the `embedded` feature) and `reddb-server`
(which needs the gRPC connector for its `rpc_stdio` dispatch
mode).

It exposes [`RedDBClient`] — a thin wrapper around the
generated tonic `RedDbClient<Channel>` that adds bearer-token
auth metadata and ergonomic typed responses. No engine
dependencies: only `tonic` + `reddb-grpc-proto`.

End users typically reach this crate transitively via
`reddb-client::connector::RedDBClient` (re-exported for
back-compat with the previous `reddb-client-internal` crate).

See [`docs/migration/workspace-split.md`][migration] for the full
crate map.

[migration]: ../../docs/migration/workspace-split.md
