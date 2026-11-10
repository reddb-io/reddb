# Topology discovery and replica routing

`@reddb-io/client` (Node and browser entrypoints) and the Rust
`reddb_client::Reddb::connect` API share the following remote connection
contract. It applies to `red://`, `reds://`, `grpc://`, `grpcs://`, `http://`,
`https://`, `ws://`, and `wss://`. A browser uses HTTP(S) or WS(S); it cannot
open native TCP or gRPC connections. Rust WebSocket support requires the
`redwire-websocket` feature; HTTP and gRPC require their existing features.
Node runtimes without a global `WebSocket` constructor can supply
`options.WebSocketImpl`.

The connection facade in each official language driver uses this lifecycle on
its implemented remote transports. Low-level transport constructors remain
explicit single-node handles. Transport availability is separate from topology
routing; this change does not add every protocol to every language:

| Facade | Remote transports with topology routing |
| --- | --- |
| JavaScript / TypeScript remote client, Rust, native Python | RedWire TCP/TLS, gRPC(S), HTTP(S), WS(S) |
| Go | RedWire TCP/TLS, gRPC(S), HTTP(S) |
| Python asyncio, .NET, Java, Kotlin, PHP, Dart, C++, Zig | RedWire TCP/TLS, HTTP(S) |
| Bun native TCP client | RedWire TCP/TLS |

All these facades provide explicit strong, local and bounded-staleness reads.
Only Rust currently exposes causal reads. The embedded-only JS SDK retains its
existing package boundary; the JS remote facade is `@reddb-io/client`.

## Server addresses

Each primary and replica advertises a reachable address for every protocol it
serves. Bind addresses are not discovery addresses: a listener may bind to
`0.0.0.0` while clients reach it through a service, load balancer, or TLS edge.
Configure each node through the normal configuration overlay, for example:

```text
REDDB_ROUTING_RED_ADVERTISE_ADDR=red://primary.internal:5050
REDDB_ROUTING_GRPC_ADVERTISE_ADDR=grpc://primary.internal:55055
REDDB_ROUTING_HTTP_ADVERTISE_ADDR=http://primary.internal:5000
REDDB_ROUTING_WSS_ADVERTISE_ADDR=wss://primary.example.com/redwire
```

The corresponding config keys are `routing.<scheme>.advertise_addr`, with one
key for each of the eight schemes. Replica nodes use their own addresses.
Replicas register these endpoints on their normal WAL pulls; the primary
combines them with the live replica registry's health and applied LSN. The WAL
reply supplies the primary's endpoint map so a client can also bootstrap from
a replica. Existing replication authentication and authorization still apply.

No port or transport is inferred from another listener. A missing address
excludes that node for that protocol. TLS endpoints retain their TLS scheme;
an HTTPS connection never discovers an HTTP replica as a usable alternative.
Advertisements containing credentials, query strings, or fragments are ignored.

## Discovery lifecycle

The client fetches topology at connection time, then refreshes before the next
operation after 30 seconds or a connection failure. A replica seed can advertise
the primary, which the client then asks for the full membership. Discovery uses
separate connections so it cannot consume query or stream responses.

The same canonical, versioned topology bytes travel in the gRPC `Topology` RPC
and the RedWire `Topology` request (`0x32`, also available over WebSocket).
RedWire advertises `FEATURE_TOPOLOGY` (`0x00000002`) after authentication; clients
do not send the new request to older peers without that feature.
HTTP exposes the equivalent JSON at `GET /v1/topology?transport=<scheme>`.
The gRPC transport selects the projection with `x-reddb-topology-transport`.

ADR 0008's disclosure rule applies on every transport: authenticated callers
receive eligible replica addresses; anonymous callers receive primary-only
topology. A server without discovery remains usable through its seed endpoint.

An advertised primary change updates the target of subsequent operations.
Discovery follows server-published membership; it does not elect or promote a
replica and does not replace the server's write fencing or promotion policy.

## Request routing and consistency

- Generic SQL `query`/`execute` calls use the primary. Explicit read APIs and
  read helpers can use healthy replicas. Replicas undergoing rebootstrap are excluded.
  Rust additionally uses its existing RTT and circuit-breaker policy; JS uses
  round-robin selection among eligible replicas and falls back after read errors.
- SQL mutations, explicit insert/delete helpers, locking reads, unknown
  functions, CTEs, and unclassified statements use the primary. Classification
  does not depend on whether the caller used a method named `query` or `execute`.
- Transactions remain on the same primary endpoint through commit or rollback.
  Session commands and multi-statement SQL keep the connection on that endpoint.
- Streams remain on the primary. A cursor or an in-progress stream is never
  moved to another node.
  In JS, interrupting a binary stream inside a transaction or pinned session
  closes that session; reconnect explicitly before further operations.
- Explicit local replica reads are eventually consistent. Append `?route=primary`
  to keep all reads on the primary. Rust `query_with_options` supports
  local, strong, bounded-staleness, and causal reads. JS `queryWithOptions`
  supports `{ consistency: 'local' }`, `{ consistency: 'strong' }`, and
  `{ consistency: 'bounded_staleness', maxLagMs: 50 }`. Unsupported consistency
  requests fail with `CONSISTENCY_UNSUPPORTED`. A Rust causal read never
  selects a replica below the required applied LSN.

For example, opt a SQL read into replica routing explicitly:

```js
const result = await db.queryWithOptions('SELECT * FROM users', { consistency: 'local' })
```

```rust
let result = db.query_with_options("SELECT * FROM users", QueryOptions::local()).await?;
```

Connection failures invalidate discovery for the next operation. Proven reads
can fall back to the primary. Writes are never automatically replayed: a lost
response can mean that the write committed. An active transaction is not moved
or silently recreated after an error.

This is primary/replica client routing. It does not claim complete distributed
SQL, cross-range transactions, or transport integration for the cluster sharding
model described in [Cluster Sharding](../architecture/cluster-sharding.md).

## Language read APIs

Generic SQL remains on the primary. Use the language's explicit read entrypoint
to allow a conservatively classified read to use an eligible replica:

| Driver | Local read example |
| --- | --- |
| Bun | `db.queryWithOptions(sql, { consistency: 'local' })` |
| Python native | `db.query_with_options(sql, "local")` |
| Python asyncio | `await db.query_with_options(sql, {"consistency": "local"})` |
| Go | `reddb.QueryWithOptions(ctx, db, sql, reddb.ReadOptions{Consistency: reddb.ReadLocal})` |
| .NET | `await db.QueryWithOptionsAsync(sql, new(ReadConsistency.Local))` |
| Java | `db.queryWithOptions(sql, ReadOptions.local())` |
| Kotlin | `db.queryWithOptions(sql, ReadOptions(ReadConsistency.LOCAL))` |
| PHP | `$db->queryWithOptions($sql, new ReadOptions('local'))` |
| Dart | `await db.queryWithOptions(sql, const ReadOptions(ReadConsistency.local))` |
| C++ | `db->query_with_options(sql, {ReadConsistency::Local})` |
| Zig | `try db.queryWithOptions(sql, .{ .consistency = .local }, .{})` |

The non-Rust ports use round-robin selection and preserve the seed's credentials,
TLS settings and HTTP proxy prefix when connecting advertised peers. Bun accepts
`connect(address, { token })` or `connectTls(address, { token, ...tlsOptions })`;
authenticated topology can include replicas. The native Python facade delegates
to the Rust router, preserving nested JSON and integer values in result envelopes.
Shared binary and SQL-classification cases live in `testdata/conformance/topology`.
