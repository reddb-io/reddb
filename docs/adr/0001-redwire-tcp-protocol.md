# ADR 0001 — RedWire v2: evolving the existing TCP protocol

**Status:** Proposed
**Date:** 2026-04-26
**Decision driver:** the wire protocol that already powers RedDB's
benchmark numbers (`examples/stress_wire_client.rs` against port
5050) is missing four pieces that PostgreSQL F+B v3 and MongoDB
OP_MSG have shipped for years: auth handshake, message
multiplexing, per-frame compression, and protocol-version
negotiation. v2 closes those gaps without breaking v1 clients.

## Context

### What we already have (v1)

`src/wire/protocol.rs` defines the canonical RedDB wire protocol.
It's not a future thing — it's what bench clients have been
talking to:

```text
┌──────────────────────────────────────────────────────────┐
│ v1 Frame (5-byte header, little-endian)                  │
│   u32   length        total frame, incl. header           │
│   u8    msg_type      message kind                        │
├──────────────────────────────────────────────────────────┤
│ Payload (length - 5 bytes)                                │
└──────────────────────────────────────────────────────────┘
```

Message types in production today:

| Code | Name | Direction |
|------|------|-----------|
| 0x01 | `MSG_QUERY`                       | C→S |
| 0x02 | `MSG_RESULT`                      | S→C |
| 0x03 | `MSG_ERROR`                       | S→C |
| 0x04 | `MSG_BULK_INSERT`                 | C→S |
| 0x05 | `MSG_BULK_OK`                     | S→C |
| 0x06 | `MSG_BULK_INSERT_BINARY`          | C→S |
| 0x07 | `MSG_QUERY_BINARY`                | C→S |
| 0x08 | `MSG_BULK_INSERT_PREVALIDATED`    | C→S |
| 0x09 | `MSG_BULK_STREAM_START`           | C→S |
| 0x0A | `MSG_BULK_STREAM_ROWS`            | C→S |
| 0x0B | `MSG_BULK_STREAM_COMMIT`          | C→S |
| 0x0C | `MSG_BULK_STREAM_ACK`             | S→C |
| 0x0D+ | `MSG_PREPARE` / `EXECUTE_PREPARED` / `DEALLOCATE` | C→S |

Listener: `src/wire/listener.rs` (`start_wire_listener`,
`start_wire_tls_listener`, `start_wire_unix_listener`). Default
port 5050.

This is the protocol that gives the benchmark numbers — RedDB
beats Postgres in 7 of 10 workloads precisely because the bench
clients never speak HTTP. Don't break it.

### What v1 is missing vs PG / Mongo

| Concern | v1 today | PG F+B v3 | Mongo OP_MSG |
|---------|----------|-----------|--------------|
| Frame multiplex | one query per round-trip | extended query (yes) | `requestId`/`responseTo` (yes) |
| Auth handshake | none — relies on server-level config | StartupMessage + AuthenticationRequest | hello + saslStart/saslContinue |
| Compression | none | per-message in libpq | per-message snappy/zstd/zlib |
| Version negotiation | none | hardcoded 0x0003_0000 in StartupMessage | hello.maxWireVersion |
| Cancel in-flight | impossible without closing TCP | CancelRequest on side socket | killOp command |
| Notice / async msgs | none | NoticeResponse | change streams |

The four gaps that hurt the most in production:

1. **No auth handshake** — clients have to be on a trusted network.
   There is no "server proves identity / client presents creds /
   role established" round-trip baked into the wire. Auth is bolted
   on via HTTP (`/auth/login` mints tokens) and gRPC (Bearer
   metadata); v1 wire connections inherit nothing.
2. **No multiplexing** — a client that wants 4 concurrent queries
   opens 4 TCP connections. Postgres does the same so this isn't
   catastrophic, but Mongo has shown that single-connection
   multiplex saves real connection-pool tax in serverless.
3. **No compression** — large bulk inserts (which the protocol
   already streams via `MSG_BULK_STREAM_*`) ship raw bytes. Mongo
   ships zstd at the frame level for free.
4. **No version negotiation** — bumping `MSG_BULK_INSERT_PREVALIDATED`
   to 0x08 in 2025 was safe only because callers used
   `MSG_ERROR "unknown message type"` as an oracle. There's no
   structured way for a v2 client to ask "do you also speak
   feature X?" before issuing it.

## Decision

Add a **v2 startup handshake** that transparently extends v1 frames
on connections that opt in. v1 clients keep working unchanged on
the same listener.

### Protocol-version detection

The first byte off the wire is the discriminator:

- **`0x00..0x7F`** — first byte of a v1 length field. Length is
  almost always small (most messages are < 128 bytes), so most v1
  frames open with a low byte. Server reads the rest of the v1
  header and proceeds as today.
- **`0xFE`** — magic byte reserved for v2 startup. Followed by:
  - `u8` minor version (currently 0x01 → "v2.1")
  - `u32` features bitfield (caller capabilities, see § Features)
  - rest of the `Hello` payload framed in v2 shape

  v1 length fields can in principle reach 0xFE in their high byte,
  but that requires a frame > 4.2 GB, which is rejected by the v1
  ceiling (`MAX_INCOMING_FRAME` ≈ 256 MiB). So 0xFE as the very
  first byte is safe to claim.

This means **no changes to existing v1 clients** — they don't send
0xFE, they keep working. v2 clients announce themselves immediately
and negotiate auth / features before any data frame.

### v2 frame layout

After a successful `Hello` handshake, both sides switch to the v2
frame format (16-byte header). The v1 ↔ v2 boundary is the
`AuthOk` reply.

```text
┌──────────────────────────────────────────────────────────┐
│ v2 Frame (16-byte header, little-endian)                 │
│   u32   length         total frame, incl. header          │
│   u8    kind           MessageKind (extended over v1)     │
│   u8    flags          COMPRESSED | MORE_FRAMES | …       │
│   u16   stream_id      multiplex key (0 = unsolicited)    │
│   u64   correlation_id request↔response pairing          │
├──────────────────────────────────────────────────────────┤
│ Payload (length - 16 bytes)                               │
└──────────────────────────────────────────────────────────┘
```

The `kind` numbering is **superset** of v1: 0x01–0x0F stay
unchanged so server dispatch can share the v1 message-type table
where logic is identical. v2 adds 0x10–0x3F for handshake / control
/ explicit response framing.

### Handshake state machine

```
                   ┌──────────────┐
   Client opens TCP├──────────────┤
                   │              ▼
                   │      [first byte]
                   │       /        \
                   │   0xFE          0x00..0x7F
                   │   (v2)          (v1 — proceed as today)
                   │     │
                   │     ▼
                   │   Hello { versions, auth_methods, features }
                   │     │
                   │     ▼
                   │   HelloAck { chosen_version, chosen_auth, server_features }
                   │     │
                   │     ▼
                   │   AuthRequest { method, challenge }       (skipped for bearer)
                   │     │
                   │     ▼
                   │   AuthResponse { method, response }
                   │     │
                   │     ▼
                   │   AuthOk { session_id, role, server_caps_final }
                   │     │
                   │     ▼
                   │   [v2 data plane — Query/Insert/etc with multiplex]
                   ▼
```

`Bye` cleanly closes either path.

### v2 message kinds (additions only — v1 codes unchanged)

| Kind | Name | Purpose |
|------|------|---------|
| 0x10 | `Hello` | Client's first frame after the 0xFE discriminator. |
| 0x11 | `HelloAck` | Server's reply with chosen version + features. |
| 0x12 | `AuthRequest` | Server challenge (SCRAM salt, nonce, etc). |
| 0x13 | `AuthResponse` | Client reply. |
| 0x14 | `AuthOk` | Auth complete; session id; final caps. |
| 0x15 | `AuthFail` | Auth refused; reason; retry hint. |
| 0x16 | `Bye` | Clean close (either side). |
| 0x17 | `Ping` / 0x18 `Pong` | Keepalive. |
| 0x20 | `Cancel` | Abort the named `correlation_id`. |
| 0x21 | `Compress` | Toggle zstd for this stream. |
| 0x22 | `SetSession` | Session vars (timezone, schema, isolation). |
| 0x23 | `Notice` | Non-fatal warning. |
| 0x24 | `RowDescription` | Column types for an upcoming stream of `MSG_RESULT`-equivalent rows. |
| 0x25 | `StreamEnd` | End-of-results marker for `correlation_id`. |
| 0x26 | `VectorSearch` | RedDB-native dense kNN query. |
| 0x27 | `GraphTraverse` | RedDB-native graph walk. |

Existing v1 codes (0x01–0x0F) keep their semantics, just framed
in 16-byte headers when the connection is v2.

### Features bitfield (Hello.features)

| Bit | Name | Meaning |
|-----|------|---------|
| 0   | `MULTIPLEX` | Client supports `correlation_id` routing |
| 1   | `COMPRESS_ZSTD` | Client/server has zstd available |
| 2   | `CANCEL` | Client may issue 0x20 Cancel frames |
| 3   | `ROW_STREAMING` | Client wants RowDescription/DataRow streaming instead of one-shot Result |
| 4   | `VECTOR_NATIVE` | Client speaks 0x26 VectorSearch frames |
| 5   | `GRAPH_NATIVE` | Client speaks 0x27 GraphTraverse frames |

Server intersects with its own capabilities and returns the
agreed set in `HelloAck.features`. Only flagged features may be
used for the rest of the session.

### Auth methods

Negotiated via `Hello.auth_methods` (CBOR list of strings) →
`HelloAck.chosen_auth`:

1. **`bearer`** — token in `AuthResponse`; server validates
   against `AuthStore`. Maps cleanly onto today's HTTP/gRPC
   bearer flow.
2. **`scram-sha-256`** — RFC 5802. Server stores only the salted
   verifier; never sees plaintext. Same as Postgres ≥ 10 and
   MongoDB ≥ 4.0.
3. **`mtls`** — auth derived from the TLS client certificate;
   triggers when the listener has client-cert auth on. No extra
   round-trip after the TLS handshake.
4. **`oauth-jwt`** — JWT bearer validated against the configured
   `auth.oauth.issuer`. Reuses the `OAuthConfig` block.
5. **`anonymous`** — only when `auth.enabled = false`; server
   advertises `anonymous` as the *only* method in `HelloAck`.

Multi-method servers list every method they accept; client picks
the strongest available (mtls > scram > oauth > bearer > anon).

### Encoding

v2 payloads use **CBOR** (RFC 8949) — typed, self-describing,
smaller than JSON, has tag space for `Vector`, `NodeRef`,
`EdgeRef`, `Decimal`, `Timestamp`. v1 payloads stay in their
hand-rolled binary format until the kind moves into v2-only
territory.

The codec uses `ciborium` (already used by parts of the Rust
ecosystem we depend on; small dep).

### Compression

When both sides flag `COMPRESS_ZSTD`, frames may set
`Flags::COMPRESSED` (bit 0). Compressor: zstd level 1 (we already
depend on the `zstd` crate).

### Multiplexing

When both sides flag `MULTIPLEX`, server dispatches each frame
keyed by `correlation_id`. Per-stream backpressure is the v1
TCP backpressure (no new flow-control credit system) — clients
that issue 1000 concurrent queries on one socket are rate-limited
by socket-buffer and server-side worker pool the same way as
gRPC.

### Service-router integration

`src/service_router/detector.rs` gains a `RedWireDetector`:

- `detect(peek)` returns `Match(RedWire)` if `peek[0] == 0xFE` and
  `peek[1] <= MAX_KNOWN_MINOR_VERSION`.
- Registered after `H2Detector` and `HttpDetector` so HTTP/2 and
  HTTP/1 still win on their own bytes.
- v1 clients keep working through the existing wire listener — they
  never set 0xFE so the detector returns `NoMatch` and the router
  falls back to Wire.

This means a single port (5050) can serve v1, v2, HTTP, and gRPC —
just like the existing service-router multiplex but with one more
detector.

### Driver story

Reference Rust client lives at `drivers/rust/src/redwire/`:

- `transport.rs` — TCP socket + TLS (rustls)
- `frame.rs` — encode / decode v2 frames
- `handshake.rs` — Hello → HelloAck → Auth*
- `session.rs` — multiplex + cancellation
- `api.rs` — query / insert / get / delete / vector / graph / tx

JS / Python / Go / Java drivers port from this. ~1500 LOC each.

## Migration plan

**Phase 0 (today, done):** v1 protocol exists and powers
benchmarks.

**Phase 1 (today, done):** `red://` URL multiplexer in JS driver
routes among HTTP / gRPC / embedded. v1 wire stays accessible via
`red://host:5050` (default proto, but currently routed through
the gRPC bridge — this is the asymmetry to fix).

**Phase 2 (this ADR, ~3 weeks):**
1. Codec for v2 frames (`src/wire/redwire/frame.rs` +
   `codec.rs`). Reuses v1 codes for kinds 0x01–0x0F.
2. Handshake state machine (`src/wire/redwire/handshake.rs`).
3. Auth methods plugged into the existing `AuthStore`.
4. Service-router detector for the `0xFE` magic.
5. Reference Rust client at `drivers/rust/src/redwire/`.
6. JS driver gains a v2 transport (replaces the gRPC bridge for
   `red://host:5050`).

**Phase 3 (~2 weeks):** Python, Go, Java drivers. Conformance test
suite.

**Phase 4 (~1 week):** Deprecate the spawn-binary gRPC bridge in
the JS driver. v2 wire becomes the canonical remote transport.
gRPC stays available for service-mesh integrations and v1 wire
stays accessible on the same port for legacy benchmark tooling.

## Out of scope (intentional)

- **Replacing the v1 protocol** — v1 stays. Bench tools that talk
  to it directly are not retired. v2 is additive.
- **PG-wire compatibility** — keeps existing role for psql / JDBC.
  The pgwire emulator continues. v2 RedWire is for RedDB-native
  clients that want graph/vector/document features.
- **HTTP REST** — keeps existing role for dashboards, webhooks,
  ad-hoc curl debugging.
- **GSSAPI / Kerberos auth** — not in v2; mtls + oauth-jwt cover
  most enterprise needs.

## Risks

1. **Mid-handshake server crash** — partial state. Mitigated by
   only spawning per-session resources after `AuthOk`.
2. **TLS handshake CPU on serverless cold start** — ~3 ms per
   connection. Mitigated by ALPN + session resumption (RFC 5077).
3. **0xFE collision** — only matters if v1 ever needs > 4 GiB
   frames. Today's v1 ceiling is 256 MiB. Reserve 0xFE explicitly
   in v1 config so a future v1 bump won't cross it.
4. **Spec churn during implementation** — mitigated by writing
   protocol test vectors first (`tests/redwire/conformance/*.bin`).
5. **Driver fragmentation** — mitigated by a shared
   `redwire-conformance` test suite every driver runs in CI.

## Performance targets

Numbers per driver client, single connection, localhost,
1k-byte rows:

| Workload                | libpq (PG) | mongodb-driver | RedWire v2 target |
|-------------------------|------------|----------------|-------------------|
| `SELECT 1` round-trip   | ~30 µs     | ~35 µs         | ≤ 30 µs           |
| Insert 10k rows         | ~250 ms    | ~280 ms        | ≤ 250 ms          |
| Stream 1M rows          | ~1.2 s     | ~1.4 s         | ≤ 1.2 s           |
| Vector kNN (k=10)       | n/a        | n/a            | ≤ 5 ms (HNSW)     |

These targets match v1's existing numbers for everything except
the auth-handshake amortisation. Connection setup is one-time;
steady-state perf doesn't regress.

## Decision rationale

v1 already gives us the binary-protocol numbers. What it doesn't
give is the rest of what production clusters need: structured
auth, multiplex, compression, version negotiation. v2 ships those
without touching the data-plane format, on the same port, behind
a single first-byte discriminator. Drivers in every language get
a clean handshake to target, and the bench tools we already
trust keep talking the v1 protocol they were written against.
