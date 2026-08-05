# reddb-client-connector

Deprecated compatibility shim for the RedDB gRPC connector.

Use `reddb_grpc_proto::RedDBClient` and the generated reply types from
`reddb-grpc-proto` directly. The authenticated client now lives beside the
generated tonic stubs, so client and server consumers share one reply shape.

This crate only re-exports that canonical client and compatibility aliases for
the former response names. It is deprecated and will be removed in the next
major release.

Most applications should continue to use the higher-level `reddb-client`
crate.
