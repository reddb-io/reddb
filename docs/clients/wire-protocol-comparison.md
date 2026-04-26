# Wire-Protocol Comparison: Postgres vs Mongo vs RedWire

How RedDB's wire protocol — the existing v1 (`src/wire/protocol.rs`,
already powering bench numbers) and the planned v2 evolution
(ADR 0001) — lines up with the two databases everyone benchmarks
against.

> **Heads-up:** v1 is what the benchmark tools already speak.
> v2 is what extends v1 with auth handshake, multiplex,
> compression, and version negotiation. Both share port 5050;
> the first byte off the wire (`0xFE` = v2, anything else = v1)
> is the discriminator.

| Dimension              | PostgreSQL F+B v3 | MongoDB OP_MSG | RedWire v1 (today) | RedWire v2 (ADR) |
|------------------------|-------------------|----------------|--------------------|--------------------|
| **Transport**          | TCP / TLS         | TCP / TLS      | TCP / TLS          | TCP / TLS          |
| **Default port**       | 5432              | 27017          | 5050               | 5050               |
| **Frame layout**       | `[u8 type][u32 len][body]` | `[u32 len][u32 reqId][u32 respTo][u32 opCode][body]` | `[u32 len][u8 kind][body]` | `[u32 len][u8 kind][u8 flags][u16 stream][u64 corr_id][body]` |
| **Header overhead**    | 5 bytes           | 16 bytes       | 5 bytes            | 16 bytes           |
| **Body encoding**      | text + binary mix | BSON           | hand-rolled binary | CBOR + RedDB tags  |
| **Multiplexing**       | extended query    | `requestId`    | none               | `correlation_id`   |
| **Pipelining**         | yes               | yes            | one-at-a-time      | yes                |
| **Cancellation**       | side-socket       | killOp         | reconnect          | in-band Cancel     |
| **Auth in handshake**  | SCRAM/MD5/cert    | SASL           | none               | SCRAM/Bearer/mTLS/OAuth |
| **TLS upgrade**        | StartTLS          | StartTLS       | direct TLS         | direct TLS / ALPN  |
| **Streaming results**  | RowDescription + DataRow* | bulk reply | one MSG_RESULT     | RowDescription + DataRow* |
| **Server push**        | LISTEN/NOTIFY     | change streams | none               | Notice frame       |
| **Schema-typed values**| binary modes      | BSON tags      | per-msg tag bytes  | CBOR + tags        |
| **Compression**        | libpq-only        | snappy/zlib/zstd | none             | per-frame zstd     |
| **Version negotiation**| hardcoded         | `hello.maxWireVersion` | none       | `Hello.versions[]` |
| **Driver count**       | 80+               | 50+            | 1 (Rust example)   | starts at 1; JS/Py/Go/Java target |
| **Spec stability**     | frozen 2003       | OP_MSG since 4.0 | benchmark-tooling stable | freeze after Phase 4 |

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

## What v1 already gives us

Don't lose sight of this when reading v2: v1 is what makes RedDB
beat Postgres in 7/10 benchmarks. The bench tools (`stress_wire_client`,
`profile_concurrent_wire`) talk this protocol directly:

- **Framed binary on TCP** — no HTTP parse, no gRPC trip
- **Typed value tags** — `VAL_I64`, `VAL_TEXT`, `VAL_F64`, etc
- **Bulk streaming** — `BULK_STREAM_START / ROWS / COMMIT / ACK`
  closes the gap with PG `COPY BINARY`
- **Prepared statements** — `MSG_PREPARE / EXECUTE_PREPARED /
  DEALLOCATE`
- **TLS-wrapped variant** — `start_wire_tls_listener`
- **Unix-socket variant** — `start_wire_unix_listener`

What v1 doesn't have is the "operational" layer of PG/Mongo:
auth, multiplex, compression, version negotiation. v2 adds those
as a strict superset, gated on a single 0xFE startup byte.

## What v2 takes from each

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

v1 already produces the bench numbers. v2 doesn't move the
data-plane shape — it adds a one-time handshake (Hello + Auth
round-trip, ~3 ms with TLS resumption) and zero per-query
overhead beyond the extra 11 bytes in the frame header.

Steady-state targets unchanged from v1:

| Workload                | libpq (PG) | mongodb-driver | RedWire v1/v2 target |
|-------------------------|------------|----------------|----------------------|
| `SELECT 1` round-trip   | ~30 µs     | ~35 µs         | ≤ 30 µs              |
| Insert 10k rows         | ~250 ms    | ~280 ms        | ≤ 250 ms             |
| Stream 1M rows          | ~1.2 s     | ~1.4 s         | ≤ 1.2 s              |
| Vector kNN (k=10)       | n/a        | n/a            | ≤ 5 ms (HNSW)        |

The Phase 1 connection-string multiplexer (HTTP + gRPC bridge in
the JS driver) is **not** competitive on tight benchmarks —
every HTTP request pays ~250 µs in headers + TLS resumption +
URL parse. v2 brings the JS driver onto v1's perf profile by
default.

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
