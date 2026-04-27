# Wire-Protocol Comparison: Postgres vs Mongo vs RedWire

How RedDB's wire protocol (RedWire — `src/wire/`, ADR 0001) lines up
with the two databases everyone benchmarks against.

| Dimension              | PostgreSQL F+B v3 | MongoDB OP_MSG | RedWire           |
|------------------------|-------------------|----------------|--------------------|
| **Transport**          | TCP / TLS         | TCP / TLS      | TCP / TLS          |
| **Default port**       | 5432              | 27017          | 5050               |
| **Frame layout**       | `[u8 type][u32 len][body]` | `[u32 len][u32 reqId][u32 respTo][u32 opCode][body]` | `[u32 len][u8 kind][u8 flags][u16 stream][u64 corr_id][body]` |
| **Header overhead**    | 5 bytes           | 16 bytes       | 16 bytes           |
| **Body encoding**      | text + binary mix | BSON           | CBOR + RedDB tags  |
| **Multiplexing**       | extended query    | `requestId`    | `correlation_id`   |
| **Pipelining**         | yes               | yes            | yes                |
| **Cancellation**       | side-socket       | killOp         | in-band Cancel     |
| **Auth in handshake**  | SCRAM/MD5/cert    | SASL           | SCRAM/Bearer/mTLS/OAuth |
| **TLS upgrade**        | StartTLS          | StartTLS       | direct TLS / ALPN  |
| **Streaming results**  | RowDescription + DataRow* | bulk reply | RowDescription + DataRow* |
| **Server push**        | LISTEN/NOTIFY     | change streams | Notice frame       |
| **Schema-typed values**| binary modes      | BSON tags      | CBOR + tags        |
| **Compression**        | libpq-only        | snappy/zlib/zstd | per-frame zstd   |
| **Version negotiation**| hardcoded         | `hello.maxWireVersion` | `Hello.versions[]` |
| **Driver count**       | 80+               | 50+            | 2 first-party (Rust + JS) + community drivers in Go / Java / Python / .NET / C++ / Dart / Kotlin / PHP / Zig |
| **Spec stability**     | frozen 2003       | OP_MSG since 4.0 | pre-1.0 — additive bumps via `Hello.versions[]` |

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

**RedWire SCRAM-SHA-256 handshake:**
```
C → S  Hello { versions: [2], auth_methods: ["scram-sha-256", "bearer", "oauth-jwt", "mtls"] }
S → C  HelloAck { version: 2, server_caps, advertised_methods }
C → S  AuthStart { mechanism: "scram-sha-256", client-first }
S → C  AuthChallenge { server-first }
C → S  AuthResponse { client-final }
S → C  AuthOk { session_id, server_caps_final }
```

3 round-trips for SCRAM in all three. Bearer/mTLS/JWT skip the
challenge-response (1 RTT). RedWire's `Hello` carries the auth
methods inline so caller doesn't need a probe round-trip like
Mongo's `hello`. SCRAM primitives live in `src/auth/scram.rs`
(server) and `drivers/rust/src/redwire/scram.rs` (client); the
JWT validator path is shared with the HTTP and gRPC surfaces
through `AuthConfig.oauth`.

## What RedWire gives us

RedWire is what makes RedDB beat Postgres in 7/10 benchmarks. The
bench tools (`stress_wire_client`, `profile_concurrent_wire`) talk
this protocol directly:

- **Framed binary on TCP** — no HTTP parse, no gRPC trip
- **Typed value tags** — `VAL_I64`, `VAL_TEXT`, `VAL_F64`, etc
- **Bulk streaming** — `BULK_STREAM_START / ROWS / COMMIT / ACK`
  closes the gap with PG `COPY BINARY`
- **Prepared statements** — `MSG_PREPARE / EXECUTE_PREPARED /
  DEALLOCATE`
- **TLS-wrapped variant** — `start_wire_tls_listener`
- **Unix-socket variant** — `start_wire_unix_listener`
- **Operational layer** — Hello/Auth handshake, multiplexed streams,
  per-frame zstd compression, version negotiation.

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

## Performance

A one-time handshake (Hello + Auth round-trip, ~3 ms with TLS
resumption) is followed by zero per-query overhead beyond the
16-byte frame header.

Steady-state targets:

| Workload                | libpq (PG) | mongodb-driver | RedWire target |
|-------------------------|------------|----------------|-----------------|
| `SELECT 1` round-trip   | ~30 µs     | ~35 µs         | ≤ 30 µs         |
| Insert 10k rows         | ~250 ms    | ~280 ms        | ≤ 250 ms        |
| Stream 1M rows          | ~1.2 s     | ~1.4 s         | ≤ 1.2 s         |
| Vector kNN (k=10)       | n/a        | n/a            | ≤ 5 ms (HNSW)   |

The HTTP REST surface is **not** competitive on tight benchmarks —
every HTTP request pays ~250 µs in headers + TLS resumption + URL
parse. Use RedWire when the workload is latency-sensitive.

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
