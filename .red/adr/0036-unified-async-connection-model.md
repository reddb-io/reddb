# Unified async connection model — RedWire-over-WebSocket for the browser

**Status:** Accepted
**Date:** 2026-06-03
**Supersedes:** the "HTTP stays thread-per-connection" stance of ADR 0034
(ADR 0034's streaming / dual-headed-registry decision still stands).

> Numbering note: PRD #930 and some in-tree comments (issue #931) name
> this record "ADR 0035". That number was already taken by
> `0035-leaderboard-rank-order-statistics-over-tables.md`, so the
> connection-model ADR is **0036**. References in connection-edge code
> (`Cargo.toml`, `server/axum_edge.rs`) point here; the ranking refs in
> `runtime/impl_core.rs` and `runtime/ranking_descriptor_catalog.rs`
> correctly mean the leaderboard ADR 0035 and are unchanged.

## Decision

RedDB serves **every** transport on one async model: no thread per
connection, anywhere. RedWire (the flagship binary protocol) is already
async on tokio — one cheap task per connection. The HTTP backend, the
lone thread-per-connection outlier (capped at `(2*num_cpus).clamp(8,256)`,
one request per connection, `Connection: close`), moves onto an
`axum`/`hyper` service on the same tokio runtime (issue #931). The
execution engine stays **100% synchronous and disk-backed**, reached from
the async edge through a shared `spawn_blocking` pool — exactly as
RedWire's `handle_session` already bridges async-transport ↔ sync-engine.

The browser reaches the server via **RedWire-over-binary-WebSocket**: the
exact RedWire frame protocol (16-byte header, `stream_id` multiplexing,
`correlation_id`) tunneled over a WSS the browser can open. This
crystallizes the core decision — **RedWire's frame protocol is decoupled
from the byte transport**. Transports become pluggable adapters: TCP /
TLS / Unix (native drivers), binary WebSocket (browser), and
WebTransport / HTTP/3 as a future adapter over the same framing.

## Why

A comparative study of high-connection-count databases (Redis/Valkey,
MongoDB, TigerBeetle) concluded a database should not open a thread per
connection. RedWire already follows that; the HTTP backend was the
outlier (ADR 0034 itself calls it "the outlier"). The concrete driver:
a **100%-in-browser SPA** must talk directly to the server with
native-driver-class throughput. A browser cannot open raw TCP, so it
cannot speak RedWire-over-TCP — yet it needs RedWire-class
throughput/latency, and the 256-thread cap plus `Connection: close` make
a high-connection-count browser front-end hostile on the direct path.

`hyper` is already in the tree via `tonic`; it provides HTTP/2
(multiplexed — closes the "browsers have no multiplexed path" gap),
mature keep-alive, chunked/SSE, and WebSocket upgrade. Hand-rolling those
is months of risky work that duplicates what hyper provides.

## Transport adapters (the seam)

| Adapter | Byte transport | Auth | Status |
|---------|----------------|------|--------|
| TCP / TLS / Unix | raw socket | anonymous / bearer / SCRAM / mTLS / OAuth-JWT | shipped (ADR 0001) |
| Binary WebSocket | WSS data channel | bearer / OAuth-JWT (no mTLS) | this work (#935/#937) |
| WebTransport / HTTP/3 | QUIC stream | — | future; same framing, no rewrite |

`handle_session<S: AsyncRead + AsyncWrite + Unpin + Send>` (issue #932)
is the seam: one protocol implementation over any async byte stream. The
WebSocket adapter (#935) exposes the binary WS data channel as an
`AsyncRead + AsyncWrite` and feeds it into the same `handle_session`,
mirroring `handle_standalone` (consume the `0xFE` magic byte, then run
the session). The browser client (#937) reuses the native RedWire codec
and swaps only the byte transport.

## Connection security (first-class, not a follow-up)

The browser endpoint is internet-exposed, so security is part of this
decision, not a later slice:

- **WSS only.** The WS upgrade is accepted only on the TLS edge; plain
  `ws://` is rejected.
- **Origin allowlist.** WebSocket is **not** covered by CORS, so the
  upgrade validates the `Origin` header against an explicit allowlist to
  prevent Cross-Site WebSocket Hijacking. The endpoint is **default-deny**:
  it is mounted only when `web.websocket_allowed_origins` is non-empty,
  and rejects any `Origin` not on the list.
- **Auth.** Browser auth is bearer / OAuth-JWT presented explicitly in
  the RedWire handshake. mTLS stays native-only (browser client certs are
  hostile UX). The hybrid token model (refresh token in an
  httpOnly+Secure+SameSite cookie, short-lived access JWT in memory) is
  Module 5 (#936) and rides the ADR 0029 stream lease for rotation.

## Considered Options

- **Unify on async; HTTP edge on axum/hyper; RedWire-over-WS for the
  browser (chosen).** One concurrency model, hyper's HTTP/2 + WS upgrade
  for free, the browser speaks the same multiplexed binary protocol as
  native drivers.
- **Keep HTTP thread-per-connection as a bounded convenience edge
  (rejected).** Incoherent two-model split; caps a direct browser
  front-end at 256 connections.
- **Hand-rolled async HTTP / a third reactor (rejected).** Duplicates
  hyper and tokio, both already proven in the tree.
- **HTTP/2-binary for the browser data plane (kept only as fallback).**
  More per-message overhead, request/response oriented; retained for
  environments where a binary WebSocket is blocked.
- **WebTransport / HTTP/3 now (deferred).** The human wanted it but
  browser/server stacks are immature; documented as a future transport
  adapter over the same framing.
- **mTLS for the browser (rejected).** Browser client certificates are
  hostile UX; bearer/JWT instead.
- **Rely on CORS for the WS upgrade (rejected).** CORS does not protect
  WebSocket; an explicit Origin check is mandatory.

## Consequences

- One connection concurrency model to reason about; idle browser
  connections are cheap tokio tasks, not scarce threads. Request admission is
  bounded independently from synchronous engine execution; per-principal
  request/rate caps and ADR 0029 stream caps still apply.
- RedWire's framing is now explicitly transport-agnostic. Adding a future
  transport (WebTransport/HTTP/3) is a new adapter, not a protocol
  rewrite.
- The engine's synchronous, disk-backed execution model is **preserved**;
  this decision is about the transport edge only and must not be cited to
  justify making engine internals async (same boundary ADR 0034 drew).
- The WS endpoint is inert until an Origin allowlist is configured —
  secure by default, but operators must opt in to enable browser
  connections.
- ADR 0034's dual-headed wake registry and "engine stays synchronous"
  decisions still hold; only its "HTTP stays thread-per-connection /
  does not grow a multiplexed event loop" stance is superseded here.

### Execution admission amendment (2026-11-04)

Async connections do not make synchronous engine work free. The HTTP edge
therefore uses two explicit bounds:

1. `max_handlers` is the admitted-request envelope. Its CPU-derived default is
   `(2*num_cpus).clamp(8,256)` and includes both running and queued requests.
2. `execution_workers` is the maximum number of buffered synchronous engine
   calls active at once. Its default is `num_cpus`; admitted excess waits on a
   bounded FIFO queue whose capacity is `max_handlers - execution_workers`.

Queue wait consumes the request's existing total deadline. Health probes keep
their separately bounded reserve and bypass engine execution admission. This
preserves the SQLite-like embedded contract (direct engine calls remain
synchronous and require no Tokio runtime) while applying the server-side lesson
shared by Cassandra-style event-loop designs: connection concurrency, queued
work and active execution are different resources and must not share one
unbounded pool.

### Runtime-wide execution amendment (2026-11-04)

The execution budget belongs to `RedDBRuntime`, not to an HTTP listener. Every
server adapter attached to that runtime shares the same active slots and bounded
FIFO queue:

- buffered HTTP and NDJSON data streams acquire before engine work, then data
  streams release before a slow network drain; ASK SSE remains bounded by the
  request/principal lane because its model callback is remote-I/O work;
- gRPC query, scan, prepare/execute and mutation fast paths acquire before
  `spawn_blocking` and report `RESOURCE_EXHAUSTED` or `DEADLINE_EXCEEDED`;
- RedWire query, bulk, get/delete and output-stream execution acquire before
  `spawn_blocking`; socket reads and response draining remain async;
- PostgreSQL Query/Parse/Describe/Execute acquire before their existing
  `block_in_place` bridge, release before the first response write, and refuse
  with SQLSTATE `53000`.

Embedded callers do not acquire this server gate: they remain direct,
synchronous SQLite-style calls. Connection/session ceilings and request-byte or
result-memory admission remain separate resources.

Back-of-envelope sizing for a 1-vCPU node: `active=1`, total request envelope
`=8`, therefore at most `1` synchronous engine call runs while `7` wait. Before
this amendment four listeners could each create their own blocking work, so the
effective active work was approximately `4 × transport concurrency`; afterward
it is `1` runtime-wide. At 1 GiB this does not claim a memory capacity number:
large request/result buffers remain governed by their byte admission.

HTTPS advertises ALPN `h2` first and `http/1.1` second. HTTP/2 multiplexing
reduces connection and head-of-line overhead but does not raise the runtime
execution budget. HTTP/3 remains a future byte-transport adapter: QUIC does not
remove storage/lock contention, and adding a second TLS/transport stack before
the HTTP/2 and execution gates are measured would increase correctness and
operational risk without addressing the observed refusal.
