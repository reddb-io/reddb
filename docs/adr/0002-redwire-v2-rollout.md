# ADR 0002 — RedWire v2 rollout: compress, HTTP-Rust, SCRAM, OAuth, polyglot drivers

**Status:** Proposed
**Date:** 2026-04-26
**Driver:** ADR 0001 shipped the v2 framing + Hello/AuthOk handshake +
bearer/anonymous auth + JS/Rust drivers. This ADR fills the
remaining production gaps so RedWire reaches feature parity with
PostgreSQL F+B v3 (auth surface) and MongoDB OP_MSG (compression),
plus Python/Go/Java drivers.

## Phases

### Phase 1 — Frame compression (zstd)

Goal: any frame body can be zstd-compressed when both sides
flag `COMPRESS_ZSTD` in `Hello.features`. Per-frame opt-in via
`Flags::COMPRESSED` (bit 0, already reserved in `frame.rs`).

**Files touched (~120 LOC total):**
- `src/wire/redwire/codec.rs` — encode/decode honour the
  `COMPRESSED` flag: encode optionally compresses the payload
  before wrapping; decode decompresses before returning the
  `Frame.payload`. Engine already depends on the `zstd` crate.
- `src/wire/redwire/auth.rs` — `Hello.features` parses
  `COMPRESS_ZSTD` (bit 1); `HelloAck.features` advertises the
  intersection.
- `src/wire/redwire/session.rs` — track per-session
  `compress_enabled: bool`; outbound frames over a configurable
  size threshold (default 4 KiB) compress automatically.
- `drivers/rust/src/redwire/codec.rs` + `drivers/rust/src/redwire/mod.rs`
  — same flag handling client side.
- `drivers/js/src/redwire.js` — same; uses `node:zlib`'s zstd
  bindings (Node 22+) or the `@cyberweb/zstd-wasm` fallback.

**Tests:**
- Round-trip a 1 MiB payload with COMPRESSED flag — assert wire
  bytes shrink by 70%+ for repeating data.
- Mixed: client sends compressed frame, server replies plain.
  Both sides must accept either shape per-frame.
- Negotiation: client without `COMPRESS_ZSTD` cap → server never
  emits compressed frames.

**Risks:**
- Node's native zstd is gated behind Node 22+. Dual-mode (native
  → wasm fallback) keeps Node 18+ working.
- Compress level 1 is the default to avoid CPU overhead on small
  frames (skip threshold).

### Phase 2 — HTTP transport in Rust client (parity with JS)

Goal: `Reddb::connect("https://host:8443")` works in pure Rust,
same HTTP REST surface as the JS driver's `HttpRpcClient`.

**Files touched (~350 LOC total):**
- `drivers/rust/src/http.rs` — new `HttpClient` struct; uses
  `reqwest::Client` with `rustls-tls` to share TLS config.
- `drivers/rust/Cargo.toml` — new feature `http = ["dep:reqwest"]`.
- `drivers/rust/src/lib.rs` — `Reddb::Http(HttpClient)` variant
  alongside `Embedded` / `Grpc`. `connect()` routes `http(s)://`
  URIs.
- `drivers/rust/src/connect.rs` — `Target::Http { url, auth }`.
- `drivers/rust/tests/http_smoke.rs` — spin up engine HTTP
  listener, hit `/health`, `/auth/login`, `/query`,
  `/collections/.../rows`. Validate the same envelope shape JS
  consumes.

**Risks:**
- `reqwest` pulls a non-trivial dep tree. Behind an opt-in
  feature so the default `embedded`-only build stays light.

### Phase 3 — SCRAM-SHA-256 auth

Goal: client/server can negotiate SCRAM-SHA-256 in the v2
handshake. Same auth method PostgreSQL ≥10 and MongoDB ≥4.0
ship; lets RedDB host operator credentials without ever
transporting plaintext passwords.

**Files touched (~280 LOC total):**
- `src/auth/scram.rs` — new module: `ScramServerSession`,
  challenge/response state machine per RFC 5802. Uses `hmac`
  + `sha2` + `pbkdf2` (already in deps).
- `src/auth/store.rs` — store SCRAM verifier (salt + iter +
  StoredKey + ServerKey) alongside / replacing the bcrypt-style
  password hash. Migration path: on first login with the new
  scheme, derive verifier from plaintext, store, drop the old
  hash.
- `src/wire/redwire/auth.rs` — add `"scram-sha-256"` to
  `pick_auth_method` priority list (above `bearer`); 3-RTT flow
  (`AuthRequest` carries `s,salt,iter`; `AuthResponse` carries
  `c,nonce,proof`; `AuthOk` carries `v,signature`).
- `drivers/rust/src/redwire/scram.rs` — client SCRAM machinery,
  `pbkdf2-hmac-sha256` from existing crate.
- `drivers/js/src/redwire-scram.js` — same in JS using
  `node:crypto`'s `pbkdf2Sync` + `createHmac` (no extra dep).

**Tests:**
- Reference vectors from RFC 5802 § 5 — assert client + server
  derive identical proofs and signatures.
- End-to-end: client + server negotiate SCRAM, exchange
  challenge/response, server stores `pwd!` once, future logins
  succeed without re-deriving.
- Wrong password path: server emits `AuthFail` with reason
  including stretchy hint text.

### Phase 4 — OAuth/JWT auth in RedWire

Goal: hook the existing `auth.oauth` (`OAuthConfig`) into the
v2 handshake. Bearer-style flow but the token is a JWT and the
server validates against a JWKS endpoint instead of the local
session table.

**Files touched (~200 LOC total):**
- `src/wire/redwire/auth.rs` — `pick_auth_method` recognises
  `"oauth-jwt"`. `validate_auth_response` for `"oauth-jwt"`
  delegates to the existing `OAuthAuthenticator` from
  `src/auth/oauth.rs`.
- Driver-side: same shape as bearer (`auth: { kind: 'jwt',
  token: '...' }`); the difference is server-side validation,
  so no new client code.

**Tests:**
- Mint a fake JWT signed with a test JWKS; assert server
  accepts it and `AuthOk` returns the role from the JWT's
  `role` claim.
- Expired JWT → server emits `AuthFail` with a stable code.
- Wrong issuer → `AuthFail`.

### Phase 5 — Polyglot drivers (Python / Go / Java)

Each driver is its own PR-stack. Same wire spec, different host
language. Conformance test suite (`tests/redwire/conformance/*.bin`,
language-agnostic) validates encode/decode parity.

**Per-driver scope (~1500 LOC each):**
- TCP transport (sync or async per language idiom)
- Frame codec (mirror Rust + JS)
- Handshake state machine
- Bearer + anonymous auth (SCRAM in a follow-up if base lands fast)
- JSON envelope parsing for Hello/HelloAck/AuthOk
- Public API: `connect`, `query`, `insert`, `bulk_insert`, `get`,
  `delete`, `ping`, `close`
- TLS / mTLS via the language's standard TLS lib

**Sequencing:**
1. **Python** — biggest user demand; uses `asyncio` + `ssl` from
   stdlib, no extra deps. Ship as `reddb-py` on PyPI.
2. **Go** — single-binary deploy targets (k8s sidecars,
   serverless functions). Uses `crypto/tls` + `encoding/binary`.
   Ship as `github.com/forattini-dev/reddb-go`.
3. **Java** — enterprise contract. Uses `javax.net.ssl` and
   classic blocking sockets first, async via Project Loom in
   Phase 5b. Ship to Maven Central as `dev.reddb:reddb-jvm`.

**Conformance tests:**
- `tests/redwire/conformance/` — golden frames in raw bytes:
  Hello, HelloAck, Query, Result, BulkInsertBinary, Bye.
- Each driver's CI loads + decodes them and asserts the parsed
  shape matches a YAML expectation file. Encoding goes the
  reverse direction. Catches divergence as soon as it lands.

## Total scope

| Phase | LOC | Sessions |
|-------|-----|----------|
| 1 zstd       | ~120  | 1 |
| 2 HTTP Rust  | ~350  | 1-2 |
| 3 SCRAM      | ~280  | 1-2 |
| 4 OAuth      | ~200  | 1 |
| 5a Python    | ~1500 | 3-4 |
| 5b Go        | ~1500 | 3-4 |
| 5c Java      | ~1500 | 4-5 |
| **Total**    | **~5450 LOC** | **~14-18 sessions** |

## Out of scope (intentional)

- Multiplexing real concurrent streams (`correlation_id` routing
  for parallel queries on one socket). v1 worked single-flight,
  v2 keeps that until backpressure design lands.
- WebSocket-tunneled RedWire for browser SDKs.
- HTTP/3 / QUIC transport.
- GSSAPI / Kerberos auth.

## Migration order

Phase 1 (zstd) lands first because it's small, validates the
flag negotiation we'll lean on for compression heuristics later,
and is a visible perf win in benchmarks. Phase 2 (HTTP-Rust)
closes the JS/Rust parity gap before users notice. Phase 3
(SCRAM) is the structural piece for production auth — every
deployment that doesn't already trust its network needs it.
Phase 4 (OAuth) is enterprise polish on top. Phase 5 grows the
language matrix.
