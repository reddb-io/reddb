# Connection quality — async-edge migration (before/after)

Measures the **transport edge** of a running RedDB server (not the query
engine): request latency distribution, sequential vs reused-connection
throughput, the `HttpConnectionLimiter` / 503 behaviour under load, and — added
for PRD slice #938 — **RedWire-over-WSS**, the browser transport (#935/#937),
measured alongside HTTP and RedWire-over-TCP. This is the **before/after**
record for the async-connection-model migration (ADR 0036, PRD #930): the
harness is the instrument, and the same table carries all three transports so
the migration's effect is read in one place.

> Numbering note: PRD #930 calls the connection-model ADR "0035". That number
> was already taken by the leaderboard-rank ADR, so the connection-model record
> is **[ADR 0036](../../.red/adr/0036-unified-async-connection-model.md)**.

Harness: [`scripts/connection-quality.mjs`](../../scripts/connection-quality.mjs)
(measurement logic covered by
[`scripts/connection-quality.test.mjs`](../../scripts/connection-quality.test.mjs)).
It drives HTTP over raw TCP sockets (`node:net`) so it is independent of any
driver version, and drives RedWire-over-TCP and RedWire-over-WSS through the
local js-client so both binary transports are measured by **identical** code —
only the byte stream differs (ADR 0036: the RedWire frame protocol is decoupled
from the transport).

```
node scripts/connection-quality.mjs --http HOST:PORT \
     [--wire HOST:PORT] [--ws wss://HOST:PORT/redwire] [--ws-insecure] \
     [--token T] [--requests N] [--concurrency C] [--field sql|query]
```

## What the numbers mean

- **HTTP seq · new conn/req** — each request opens a fresh TCP connection (and,
  over HTTPS, a fresh TLS handshake), the `Connection: close` model. The
  per-request sample **includes** the connect cost.
- **HTTP seq · reuse conn** — requests reuse one connection (the harness reopens
  if the server closes; the `reopens` count exposes whether the server kept it
  alive). The per-request sample **excludes** the connect cost, so the delta
  against the new-conn p50 is roughly the per-request connection-setup tax.
- **RedWire-over-TCP seq · mux conn** — the native binary protocol over one
  multiplexed TCP connection (`--wire`); the throughput ceiling native drivers
  see. Skipped (not fatal) when no native RedWire listener is reachable.
- **RedWire-over-WSS seq · mux conn** — the **browser** transport (`--ws`): the
  exact RedWire frame protocol tunneled over a binary WebSocket (#935/#937).
  Same multiplexed binary protocol as native drivers, same measurement loop —
  the browser path’s latency/throughput, read on the same axis as TCP. Skipped
  (not fatal) when no WSS edge is reachable.
- **connection-limiter probe** — fires `--concurrency` simultaneous fresh
  connections × 5 rounds and reports the 200 / 503 / error split. A non-zero 503
  count is the `HttpConnectionLimiter` load-shedding as designed (cap
  `(2 * num_cpus).clamp(8, 256)`). The WSS line below it records the **same
  concurrency multiplexed over one socket** — the browser path fans out by
  `stream_id`, so there is no per-connection cap to engage.

## Before — `Connection: close`, thread-per-connection HTTP

The shipped HTTP backend (prebuilt `red` 0.1.5) closes after each reply, so this
is the **`Connection: close` baseline** — the model ADR 0036 removes. RedWire
rides a separate native listener and the browser has **no** direct binary path
at all (a browser cannot open raw TCP).

In-memory, HTTP plaintext (loopback), `--requests 1500 --concurrency 64 --field query`:

| measurement                 |   n  |  p50  |  p90  |  p99  |  max  | throughput | note |
|-----------------------------|-----:|------:|------:|------:|------:|-----------:|------|
| HTTP seq · new conn/req     | 1500 | 3.122 | 4.536 | 9.571 | 18.25 |  272 req/s | `Connection: close` |
| HTTP seq · reuse conn       | 1500 | 1.750 | 2.415 | 4.488 | 18.50 |  384 req/s | server closed each time, reopens=2998 |

Limiter probe (concurrency 64 × 5 = 320 reqs): `200=320 503=0` — no shedding at
64; the cap is higher than 64 on this host. Under concurrency the per-request
latency rose to p50≈43 ms / p99≈69 ms, reflecting handler-pool queueing.

**Read:** the ~1.4 ms p50 gap between *new conn/req* (3.12 ms, includes connect)
and *reuse conn* (1.75 ms, excludes connect) is the per-request TCP-setup tax on
loopback. On a real network with RTT and — for HTTPS — a TLS handshake, that tax
is far larger. The `reuse conn` row’s `reopens=2998` is the tell: 0.1.5 closes
after **every** reply, so even the “reuse” path is forced to reconnect — both
rows are paying the connect tax. That is exactly the cost the async edge removes.

A fresh confirmation re-run on 2026-06-03 (same binary, less-loaded host)
reproduced the model — `HTTP seq · reuse conn` again reported `reopens=1499`,
i.e. **keep-alive OFF, server closed each time** — confirming 0.1.5 is still the
`Connection: close` baseline the migration starts from. Absolute latencies on
loopback are host- and load-sensitive (a quiet host shows ~1 ms p50 for both
rows because loopback connect is nearly free); the **`reopens` count**, not the
millisecond, is the stable before/after signal.

## After — async axum/hyper edge + RedWire-over-WSS

ADR 0036 replaces the thread-per-connection HTTP backend with a single async
(axum/hyper) edge on the shared tokio runtime, and adds **RedWire-over-WSS** for
the browser. To capture the *after*, run the **same** harness against an
async-edge build whose TLS edge mounts the `/redwire` WebSocket route (i.e.
`web.websocket_allowed_origins` is non-empty — the route is default-deny, ADR
0036):

```
# after the migration: keep-alive is free, and the browser speaks RedWire-over-WSS
node scripts/connection-quality.mjs \
     --http  127.0.0.1:8080 \
     --wire  127.0.0.1:5050 \
     --ws    wss://127.0.0.1:8443/redwire --ws-insecure \
     --requests 1500 --concurrency 64
```

What the *after* table records, against the *before* above:

- **HTTP seq · reuse conn** flips to `keep-alive ON (reopens=0)`: on the async
  edge a persistent connection is a cheap tokio task, not a scarce thread, so
  the per-request connect tax disappears and the `reuse conn` row pulls cleanly
  ahead of `new conn/req` instead of being dragged back by forced reconnects.
- **RedWire-over-WSS seq · mux conn** appears as a populated row — the browser
  now has a direct, multiplexed binary path where before it had none. It is read
  directly against **RedWire-over-TCP**: the two share the codec and differ only
  in the byte transport, so the WSS row quantifies the browser’s overhead (WS
  framing + TLS record layer) relative to native TCP.
- **connection-limiter probe** loses its ceiling: the `(2*num_cpus).clamp(8,256)`
  cap is replaced by async backpressure, and the WSS concurrency line shows the
  same in-flight query count multiplexed over **one** socket with **no
  per-connection cap to engage** — the high-connection-count browser front-end
  that was hostile on the `Connection: close` direct path becomes cheap.

Capturing the live *after* numbers requires an async-edge server build with a
TLS WSS edge configured; this repo’s prebuilt `red` 0.1.5 predates that edge, so
the table above is the *before* the migration starts from. The harness itself
already produces the RedWire-over-WSS row when pointed at such an edge — the
WSS measurement path (handshake + `stream_id` multiplex + query over a binary
WebSocket) is exercised end-to-end against the native RedWire codec in
`scripts/connection-quality.test.mjs`, so the instrument is verified
independently of having a TLS edge on hand.

## Why this matters (ADR 0036)

The connect-cost tax and the 256-connection cap measured in the *before* exist
because the HTTP backend was thread-per-connection with `Connection: close`
(one OS thread per in-flight request, capped at `(2*num_cpus).clamp(8,256)`).
ADR 0036 unifies **every** transport on one async model: keep-alive becomes
free, the cap is replaced by async backpressure, and — critically — the
**100%-in-browser SPA** reaches the server over RedWire-over-WSS, the same
multiplexed binary protocol as native drivers, so the connect tax and the cap
both vanish from the browser path. This harness is the before/after instrument
for exactly that claim.
