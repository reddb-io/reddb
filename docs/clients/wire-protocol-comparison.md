# Wire-Protocol Comparison: Postgres vs Mongo vs RedWire

How RedDB's planned native protocol (RedWire, ADR 0001) lines up with
the two databases everyone benchmarks against.

| Dimension              | PostgreSQL F+B v3 | MongoDB OP_MSG | RedWire (planned) |
|------------------------|-------------------|----------------|-------------------|
| **Transport**          | TCP / TLS         | TCP / TLS      | TCP / TLS         |
| **Default port**       | 5432              | 27017          | 5050              |
| **Frame layout**       | `[u8 type][u32 len][body]` | `[u32 len][u32 reqId][u32 respTo][u32 opCode][body]` | `[u32 len][u8 kind][u8 flags][u16 stream][u64 corr_id][body]` |
| **Header overhead**    | 5 bytes           | 16 bytes       | 16 bytes          |
| **Body encoding**      | text + binary mix | BSON           | CBOR              |
| **Multiplexing**       | none — one query at a time | by `requestId` | by `correlation_id` |
| **Pipelining**         | yes (extended query) | yes        | yes               |
| **Cancellation**       | out-of-band TCP (CancelRequest)  | killOp command | in-band Cancel frame |
| **Auth in handshake**  | yes — SCRAM/MD5/cert | yes — SASL    | yes — SCRAM/Bearer/mTLS/OAuth |
| **TLS upgrade**        | StartTLS via SSLRequest | StartTLS via isMaster.tls | direct TLS or ALPN |
| **Streaming results**  | RowDescription + DataRow* | bulk reply | RowDescription + DataRow* |
| **Server push**        | NotificationResponse (LISTEN/NOTIFY) | change streams | Notice frame |
| **Schema-typed values**| binary protocol modes | BSON tags | CBOR + RedDB tags (Vector/NodeRef/EdgeRef/Decimal) |
| **Compression**        | none in spec; libpq has it | snappy/zlib/zstd | per-frame zstd |
| **Driver count**       | 80+                | 50+            | starts at 1 (Rust ref); Python/JS/Go/Java target |
| **Spec stability**     | frozen since 2003 | OP_MSG since 4.0 (2018) | freeze after Phase 4 |

## Auth story side by side

**Postgres SCRAM-SHA-256 handshake:**
```
C → S  StartupMessage (user, database, application_name)
S → C  AuthenticationSASL("SCRAM-SHA-256")
C → S  SASLInitialResponse(client-first-message)
S → C  AuthenticationSASLContinue(server-first-message)
C → S  SASLResponse(client-final-message)
S → C  AuthenticationSASLFinal(server-final-message)
S → C  AuthenticationOk
S → C  ParameterStatus*, BackendKeyData, ReadyForQuery
```

**MongoDB SCRAM-SHA-256 handshake:**
```
C → S  hello { saslSupportedMechs: "user@db" }
S → C  helloOk + SCRAM mechs
C → S  saslStart { mechanism: "SCRAM-SHA-256", payload: client-first }
S → C  saslContinue { conversationId, payload: server-first }
C → S  saslContinue { conversationId, payload: client-final }
S → C  saslFinal
```

**RedWire SCRAM-SHA-256 handshake (planned):**
```
C → S  Hello { versions: [1], auth_methods: ["scram-sha-256", "bearer"] }
S → C  HelloAck { version: 1, chosen_auth: "scram-sha-256", server_caps }
S → C  AuthRequest { sasl_first: client-first-message echoed back }
C → S  AuthResponse { client-final-message }
S → C  AuthOk { session_id, server_caps_final }
```

3 round-trips for SCRAM in all three. Bearer/mTLS skip the
challenge-response (1 RTT). RedWire's `Hello` carries the auth
methods inline so caller doesn't need a probe round-trip like
Mongo's `hello`.

## What RedWire takes from each

**From Postgres:**
- Streaming `RowDescription` + `DataRow*` + `Stream End` — the simplest
  shape for query results, and the one that most cleanly lets the
  client start consuming before the server finishes producing.
- Out-of-band cancellation pattern (we keep it in-band via the
  `correlation_id` because we have multiplexed streams).
- Stable spec discipline — version bump = breaking change, never
  silent.

**From Mongo:**
- Multiplexed `requestId` / `responseTo` correlation — lets one TCP
  connection carry many concurrent queries without head-of-line
  blocking. RedWire's `correlation_id` is identical in spirit.
- Per-message compression toggle.
- Modern auth (no plaintext password mode shipped).

**From neither (RedDB-specific):**
- Native `VectorSearch` and `GraphTraverse` message kinds.
- CBOR with RedDB tag extensions for `Vector`, `NodeRef`, `EdgeRef`,
  `Decimal`, `Timestamp` — typed scalars don't lose precision through
  JSON.
- Tier-0 OAuth/JWT support (Postgres has it via the `clientcert`
  hack; MongoDB has it via `MONGODB-OIDC` since 7.0). RedWire treats
  it as a first-class auth method.

## Performance equivalence claim

The targets in ADR 0001 (≤30 µs for `SELECT 1`, ≤250 ms for 10k row
insert) are within 15% of libpq + MongoDB driver numbers on the
same hardware. The wire structure is close enough to the references
that the remaining variance is in the engine, not the protocol.

The Phase 1 multiplexer (HTTP + gRPC bridge) is **not** competitive
on tight benchmarks — every HTTP request pays ~250 µs in headers
+ TLS resumption + URL parse. RedWire amortises that across the
session.

## Why a binary protocol matters even for slow queries

Even at 1 ms per query on the engine side, HTTP/REST adds 0.5–1 ms
of overhead per request. With RedWire that overhead drops to ~30 µs,
which:

- Makes batch workloads ~30× faster on the protocol axis.
- Cuts tail latency p99 because there's no GC churn on
  HTTP-header allocation.
- Frees server CPU spent on HTTP framing for actual query work.

The case where the protocol doesn't matter is dashboard queries
that humans wait for — those stay on HTTP/REST forever.

## See also

- ADR 0001 — RedWire protocol spec
- `docs/clients/sdk-compatibility.md` — driver feature matrix
- PostgreSQL Frontend/Backend Protocol — https://www.postgresql.org/docs/current/protocol.html
- MongoDB OP_MSG — https://www.mongodb.com/docs/manual/reference/mongodb-wire-protocol/
- RFC 5802 SCRAM — https://www.rfc-editor.org/rfc/rfc5802
- RFC 8949 CBOR — https://www.rfc-editor.org/rfc/rfc8949
