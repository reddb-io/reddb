# reddb-client-connector

Deprecated compatibility shim for the RedDB gRPC connector.

Use `reddb_grpc_proto::RedDBClient` and the generated reply types from
`reddb-grpc-proto` directly. The authenticated client now lives beside the
generated tonic stubs, so client and server consumers share one reply shape.

Deprecated but working: the full pre-fold surface still compiles and behaves
exactly as before — `health()`, `create_row()`, the `String`-returning
`scan()` / `stats()` display helpers, and `BulkCreateStatus { ids }`. Each
public item carries its own `#[deprecated]` attribute (a crate-level
`#![deprecated]` or a plain `pub use` would warn nobody downstream), so
consumers get a per-call migration note and a compile that still succeeds.
The crate is removed at the next major release.

| Deprecated here | Canonical replacement |
| --- | --- |
| `RedDBClient` | `reddb_grpc_proto::RedDBClient` |
| `HealthStatus` | `reddb_grpc_proto::HealthReply` |
| `QueryResponse` | `reddb_grpc_proto::QueryReply` |
| `CreatedEntity` | `reddb_grpc_proto::EntityReply` |
| `OperationStatus` | `reddb_grpc_proto::OperationReply` |
| `BulkCreateStatus { ids }` | `reddb_grpc_proto::BulkEntityReply { items }` |

Most applications should continue to use the higher-level `reddb-client`
crate.
