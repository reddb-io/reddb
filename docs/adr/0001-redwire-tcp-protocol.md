# ADR 0001 — RedWire: native TCP protocol for RedDB clients

**Status:** Proposed
**Date:** 2026-04-26
**Decision driver:** parity with PostgreSQL (libpq/F+B v3) and MongoDB
(OP_MSG) — drivers should hold a single TCP socket, negotiate auth
in the handshake, and stream typed responses, with no HTTP/gRPC
overhead.

## Context

The JS driver currently multiplexes three transports under a unified
`red://` connection string (Phase 1, 2026-04-26):

- **embedded** — spawn the `red` binary, JSON-RPC over stdio
- **http(s)** — fetch() to REST endpoints, Bearer/Basic auth
- **grpc(s)** — spawn the binary as a stdio→gRPC bridge

This works but has structural ceilings:

1. **Per-request HTTP overhead** (headers, content-length, URL parse,
   TCP/TLS handshake amortised across far fewer queries than a
   long-lived connection).
2. **gRPC bridge requires the binary on every client host** — operators
   cannot ship a thin native client without bundling the engine.
3. **Auth model is split** — HTTP login mints tokens; gRPC accepts only
   bearer; PG-wire negotiates SCRAM/MD5; embedded has no auth at all.
   Drivers need three code paths.
4. **Streaming results back to the caller** is awkward over REST and
   tied to gRPC's message types when over gRPC.
5. **Type fidelity** — REST loses type information through JSON;
   gRPC needs proto regeneration per language.

Postgres and MongoDB solved this the same way: define one binary
TCP wire protocol, ship a thin reference client, let every language
implement framing + auth + flow control directly on TCP. Drivers stay
small (~1500 LOC each), run anywhere, and consistently outperform
HTTP-based clients by an order of magnitude on small-message
workloads.

## Decision

Define **RedWire**, a native TCP protocol that drivers in every
language speak directly. `red://` URLs without an explicit `proto=`
default to RedWire on port 5050.

### Wire layout

Length-prefixed binary frames over a single TCP (or TLS) socket.

```
┌──────────────────────────────────────────────────────────┐
│ MessageHeader                                            │
│   u32   length    (little-endian; total frame, incl. hdr) │
│   u8    kind      (see MessageKind table)                 │
│   u8    flags     (bit 0: zstd-compressed payload)        │
│   u16   stream_id (multiplexes parallel queries)          │
│   u64   correlation_id (request↔response pairing)        │
├──────────────────────────────────────────────────────────┤
│ Payload                                                   │
│   variable-length, format determined by `kind`            │
└──────────────────────────────────────────────────────────┘
```

Total header: 16 bytes. Max frame: 16 MiB by default (configurable).

### Encoding

Payloads use **CBOR** (RFC 8949) — typed, self-describing, smaller
than JSON, faster to encode/decode, and has direct mappings for
RedDB's value types (int, float, bytes, string, list, map, tag) plus
extension tags for `Vector`, `NodeRef`, `EdgeRef`, `Timestamp`,
`Decimal`. CBOR is also what TLS-EXPORTER, COSE, and CWT use, so
crypto integration (signing, attestation) gets a natural path.

Alternatives considered:
- **BSON** (Mongo) — wider language support, but larger, less
  compact for the typed scalars RedDB cares about, and heavier
  decode.
- **Protobuf** — requires schema regen per language; opaque on
  the wire (can't dump a frame to text without the .proto).
- **MessagePack** — close second to CBOR; CBOR wins on standardised
  tag space + RFC stability.
- **Postgres binary types** — perfect for SQL but doesn't model
  graphs, vectors, documents.

### Message kinds

Handshake / lifecycle:
| Kind | Name | Direction | Purpose |
|------|------|-----------|---------|
| 0x01 | Hello | C→S | client greets, lists supported versions + auth methods |
| 0x02 | HelloAck | S→C | server picks version, advertises auth requirement |
| 0x03 | AuthRequest | S→C | challenge for chosen method (SCRAM salt, JWT scope, etc) |
| 0x04 | AuthResponse | C→S | client reply |
| 0x05 | AuthOk | S→C | session established, returns server caps |
| 0x06 | AuthFail | S→C | reason + retry hint |
| 0x07 | Bye | both | clean close |
| 0x08 | Ping | both | heartbeat |
| 0x09 | Pong | both | heartbeat reply |

Data plane:
| Kind | Name | Direction | Purpose |
|------|------|-----------|---------|
| 0x10 | Query | C→S | SQL string + params |
| 0x11 | Prepare | C→S | prepared-statement registration |
| 0x12 | Execute | C→S | run a prepared statement |
| 0x13 | Insert | C→S | typed insert (collection + payload(s)) |
| 0x14 | Get | C→S | entity-by-id lookup |
| 0x15 | Delete | C→S | entity-by-id delete |
| 0x16 | VectorSearch | C→S | dense-vector kNN (RedDB-native, no PG analogue) |
| 0x17 | GraphTraverse | C→S | graph walk (RedDB-native) |
| 0x18 | TxBegin | C→S | open transaction |
| 0x19 | TxCommit | C→S | commit |
| 0x1A | TxRollback | C→S | rollback |
| 0x20 | RowDescription | S→C | column types for the next stream |
| 0x21 | DataRow | S→C | one row (streamed, repeats) |
| 0x22 | Affected | S→C | count for DML |
| 0x23 | Stream End | S→C | end-of-results marker for `correlation_id` |
| 0x24 | Error | S→C | error code + message + structured details |
| 0x25 | Notice | S→C | non-fatal warning (deprecated method, slow query) |

Control:
| Kind | Name | Direction | Purpose |
|------|------|-----------|---------|
| 0x30 | Cancel | C→S | abort the in-flight `correlation_id` |
| 0x31 | Compress | C→S | toggle zstd compression for this stream |
| 0x32 | SetSession | C→S | session vars (timezone, schema, isolation) |

### Auth methods (negotiated in Hello / HelloAck)

1. **`bearer`** — caller sends `Authorization: Bearer <token>`-shaped
   payload as AuthResponse. Token may be an API key or a session
   token from `POST /auth/login`. Simplest path for serverless.
2. **`scram-sha-256`** — RFC 5802 challenge/response. Server stores
   only the salted password verifier; never sees plaintext. Same as
   Postgres ≥10.
3. **`mtls`** — auth derived from the TLS client certificate (CN +
   SAN → username). Triggered when `proto=redwires://` and listener
   has `auth.cert.enabled=true`.
4. **`oauth-jwt`** — OIDC/JWT bearer validated against configured
   issuer. Reuses the existing `auth.oauth` config block.
5. **`anonymous`** — only when `auth.enabled=false` (dev mode);
   server announces `anonymous` as the *only* method in HelloAck.

The handshake never echoes the password back — SCRAM is the canonical
path when usernames+passwords are involved. Bearer + mTLS + OAuth
are stateless and don't need additional round-trips.

### Connection URL

```
red://[user[:pass]@]host[:port][/path][?param=value]
```

- Default proto = `redwire`, default port = 5050
- `red://...?proto=https` keeps the HTTPS path (Phase 1 router still works)
- `redwire://` and `red://` are aliases; `redwires://` for TLS
- Auth: `user:pass` → SCRAM if server allows; otherwise drives the
  HTTP login flow and converts to bearer (Phase 1 behaviour)
- Query params: `token=`, `apiKey=`, `sni=`, `appName=`, `compress=`,
  `timeout_ms=`

### Server-side architecture

New listener `src/wire/redwire/`:

- `listener.rs` — TCP / TLS accept, hand off to a session task per
  connection
- `frame.rs` — `Frame` codec (read/write the 16-byte header + payload)
- `session.rs` — handshake state machine, auth negotiation, command
  dispatch loop
- `auth/` — pluggable challenge handlers (bearer, scram, mtls, oauth)
- `dispatch.rs` — bridge to the runtime (`RuntimeQueryPort`,
  `RuntimeEntityPort`, etc) — same surface the gRPC + HTTP servers
  consume

The listener registers via `service_router` (existing) so a single
TCP port can multiplex RedWire / HTTP / gRPC / PG-wire by sniffing
the first bytes:

- HTTP/1.x → method prefix
- HTTP/2 / gRPC → `PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n`
- PG wire → 4-byte length + 4-byte protocol version (0x0003_0000) or `SSLRequest` / `GSSENCRequest`
- **RedWire → magic prefix `\xRD\xDB\x01` followed by the Hello frame**

The magic prefix is the dispatch signal; it's discarded after the
detector matches.

### Client-side architecture (per language)

Each driver implements roughly:

```
RedWireClient
├── transport: TCP socket (or TLS-wrapped)
├── frameCodec: read/write framed messages
├── handshake: Hello → HelloAck → Auth → AuthOk
├── inflight: Map<correlation_id, response_handler>
├── reader_task: pull frames, route by correlation_id
└── api: query / insert / get / delete / vector / graph / tx
```

Reference impl in Rust at `drivers/rust/src/redwire/` — the JS, Python,
Go, Java drivers port from there. Estimated ~1500 LOC per driver.

### Compression + chunking

- Per-frame `flags` bit 0: payload is zstd-compressed
- Negotiated default at `Hello` (client lists `zstd` cap; server
  agrees in `HelloAck`)
- `Compress` control message can flip per-stream
- Frames > 16 MiB split into chunks with `flags` bit 1 = "more frames
  follow under this correlation_id"

## Performance targets

Numbers per driver client, single connection, localhost, 1k-byte
rows:

| Workload                | Postgres (libpq) | MongoDB (OP_MSG) | Target RedWire |
|-------------------------|------------------|------------------|----------------|
| `SELECT 1` round-trip   | ~30 µs           | ~35 µs           | ≤ 30 µs        |
| Insert 10k rows         | ~250 ms          | ~280 ms          | ≤ 250 ms       |
| Stream 1M rows          | ~1.2 s           | ~1.4 s           | ≤ 1.2 s        |
| Vector kNN (k=10)       | n/a              | n/a              | ≤ 5 ms (HNSW)  |

Methodology: end-to-end through the TCP wire, no caching, fresh
connection per benchmark run.

## Migration path

**Phase 1 (done, 2026-04-26):** `red://` URL parser routes to existing
HTTP / gRPC / embedded transports. Drivers gain a unified surface
without server changes.

**Phase 2 (this ADR, ~3 weeks):**
- Implement the RedWire listener server-side.
- Reference Rust client at `drivers/rust/src/redwire/`.
- JS driver gains a RedWire transport alongside HTTP / spawn-gRPC.
- `red://` URLs default to RedWire when no `proto=` is given.

**Phase 3 (~2 weeks):** Ship Python, Go, Java drivers using the same
wire spec. Each driver is ~1500 LOC. Add fuzz harness + protocol
test vectors in `tests/redwire/`.

**Phase 4 (~1 week):** Deprecate `--connect grpc://...` from the `red`
binary. Remove the stdio→gRPC bridge in JS driver. gRPC stays
available for service-mesh integrations but stops being the canonical
remote transport.

## Out of scope

- **HTTP REST** — keeps existing role for dashboards, webhooks,
  ad-hoc curl debugging. Not deprecated.
- **PG wire compatibility** — keeps existing role for psql / JDBC /
  node-pg / asyncpg. Pgwire emulation continues. RedWire is for
  RedDB-native clients that want graph/vector/document features.
- **Web browser support** — RedWire requires raw TCP; browsers can't
  open arbitrary sockets. Browser SDKs continue using HTTP. A future
  WebSocket-tunneled RedWire might happen but isn't in this ADR.
- **GSSAPI / Kerberos auth** — not in v1; `mtls` and `oauth-jwt`
  cover most enterprise needs.

## Risks

1. **Spec churn during implementation** — mitigated by writing
   protocol test vectors first, and keeping Phase 1 transports
   working through the migration window.
2. **TLS handshake CPU on serverless** — TLS adds ~3 ms per cold
   connection. Mitigated by connection pooling + ALPN + session
   resumption (RFC 5077).
3. **Breaking change pressure** — once shipped, the protocol freezes.
   Reserve the `flags` byte (8 unused bits) and a `version` field
   in `Hello` for non-breaking extensions.
4. **Driver fragmentation** — every language reimplementing framing
   risks divergence. Mitigated by shipping a shared `redwire-conformance`
   test suite that every driver runs in its CI.

## Open questions

1. Should `Hello` carry a `tenant_id` field for multi-tenant servers,
   or rely on the auth principal? Lean toward auth principal.
2. Encrypted-at-rest interaction: does the RedWire layer carry the
   page-encryption key, or does the operator inject via env? Lean
   toward env (status quo for embedded).
3. Should we support pipelining out of the box (send N requests
   without waiting for response)? Yes — the `correlation_id` field
   exists precisely for that.
4. Cancellation: `Cancel` frame matches PG's `CancelRequest` shape
   semantically — ack is best-effort. Document this in the spec.

## Decision rationale

PostgreSQL and MongoDB both invested in custom wire protocols and
both dominated their categories in part *because* of that — the
protocol is the contract that lets a hundred drivers exist without
merge conflicts in a single repo. RedDB needs the same property to
ship in every language we care about, with consistent perf and a
consistent auth story. Phase 1's URL unification was the easy half;
this is the load-bearing half.
