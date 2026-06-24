# reddb-io-client-connector

Workspace-internal gRPC connector for [RedDB](https://github.com/reddb-io/reddb).

This crate exists to break a path-dependency cycle between
`reddb-io-client` (the published Rust driver, which pulls in
`reddb-io-server` under the `embedded` feature) and `reddb-io-server`
(which needs the gRPC connector for its `rpc_stdio` dispatch
mode).

It exposes [`RedDBClient`] — a thin wrapper around the
generated tonic `RedDbClient<Channel>` that adds bearer-token
auth metadata and ergonomic typed responses. No engine
dependencies: only `tonic` + `reddb-io-grpc-proto`.

End users typically reach this crate transitively via
`reddb_client::connector::RedDBClient` (re-exported for
back-compat with the previous `reddb-client-internal` crate).

See the [monorepo structure guide][monorepo] for the full crate map.

[monorepo]: ../../docs/dev/monorepo-structure.md
