# ADR 0049 — UI canonical transport is RedWire-over-WebSocket

Status: accepted
Date: 2026-06-08

Part of the integration batch 0047–0051.

## Decision

red-ui's **canonical data transport is RedWire-over-binary-WebSocket**
(ADR 0036), not the HTTP-JSON `/query` + SSE `/changes/stream` adapter.

- The HTTP+SSE adapter (`packages/ui/src/lib/reddb/client.ts`,
  `cdc-stream-client.ts`) is demoted to a **bootstrap / fallback** surface.
- The `redwire-ws-spike` is promoted to the **primary** channel — it is the
  client side of ADR 0036.

API/version compatibility is the RedWire handshake's existing
`SUPPORTED_VERSION` negotiation — there is **no separate HTTP `api_version`
header**. The handshake is a *hard* negotiation: no version overlap →
`ErrorCode::Protocol`. red-ui catches that rejection and renders a friendly
"update" banner instead of a white screen. The *negotiation* is hard; only the
*presentation* of a mismatch is soft.

## Why

A feature-rich UI must reach **every** reddb feature — CDC, replication
topology with live primary/replica stats, subscriptions — and those are
streaming-native. RedWire-over-WS already carries them as one framed,
multiplexed, backpressured protocol with a browser codec (`redwire-core.js`,
ADR 0036). Folding version negotiation into the handshake reuses a mechanism
that already exists (`handshake.rs` sends `&[SUPPORTED_VERSION]`) instead of
bolting an `api_version` onto the HTTP surface, which currently advertises no
protocol version at all.

## Considered Options

- **RedWire-over-WS as canonical (chosen).** One transport reaches 100% of
  reddb; version negotiation is native to the handshake.
- **Keep HTTP+SSE as canonical (rejected as canonical, kept as fallback).**
  Two surfaces, no unified framing; CDC works but the contract is split.
- **Ship both transports indefinitely, chosen per capability (rejected).**
  Doubles the contract the bridge and server must keep in sync.

## Consequences

- The `red` bridge (ADR 0047) must serve RedWire-over-WS **over the embedded
  engine** for `file://` — not merely proxy an existing HTTP surface.
- The `redwire-ws-spike` must be hardened from spike to product.
- The WS endpoint's default-deny **Origin allowlist** (ADR 0036) governs every
  bridge and server that exposes it.
- HTTP+SSE is **not removed** — it remains a bootstrap/fallback path (e.g.
  `/health`, version advertise, environments where binary WS is blocked, the
  HTTP/2-binary fallback noted in ADR 0036).

## Related

- ADR 0036 — Unified async connection model (RedWire-over-WebSocket)
- ADR 0034 — Streaming over RedWire; HTTP stays synchronous
- ADR 0047 — `red ui` bridge: the UI is a single-transport client
