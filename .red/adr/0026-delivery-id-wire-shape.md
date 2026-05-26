# ADR 0026: `delivery_id` wire shape and deprecation-window semantics

Status: Accepted (2026-05-26)

Related: [PRD #527 (QueueLifecycle)](../../README.md), issue #627, [ADR 0015 (parameterized query contract)](0015-parameterized-query-contract.md)

## Context

PRD #527 introduces a server-issued opaque `delivery_id` as the new ACK/NACK
handle for queue deliveries, replacing the legacy
`(queue, group, message_id)` tuple. The two handles must coexist for one
minor release as a wire-compat bridge, after which the tuple path is removed.

The lifecycle Module already uses a `delivery_id` string internally
(`runtime/replica_queue_store.rs`). What is undecided is how the value travels
across the four wire transports — redwire, gRPC, Postgres-wire, HTTP — and
what the precedence and failure semantics look like at the protocol boundary.

Three open questions, settled here:

1. What concrete type does `delivery_id` take on each wire?
2. Does the deprecation warning reach the client, or stay server-side?
3. When both handles are present but `delivery_id` does not resolve, do we
   fall through to the tuple or fail?

## Decision

**Wire shape: string base32 in all four transports.** `delivery_id` is the
same ASCII-safe string in redwire, gRPC, Postgres-wire, and HTTP. No
transport-specific encoding (no `bytes` in proto). The Rust side may wrap it
in a newtype `DeliveryId(String)` for type safety; the wire payload is plain
string everywhere. Reasons: base32 is already ASCII, copy-paste-friendly for
debugging, removes the gRPC-only encode/decode step, and keeps the four wires
visually identical when a human reads a packet capture.

**Deprecation visibility: server-side log only.** When a request arrives with
the legacy tuple and no `delivery_id`, the server emits a single
deprecation log line (with rate limiting per request key — see Consequences)
and proceeds. The client receives no protocol-level signal: no HTTP
`Warning: 299` header, no gRPC trailer, no RQL warnings array. Operators see
the deprecation through their server logs and migrate; clients are not
forced to introspect a new field they did not subscribe to. Cuts protocol
noise and the four-wire surface area of the bridge.

**Invalid-resolution precedence: strict failure, no fallback.** When a
request supplies both a `delivery_id` and a legacy tuple, and the
`delivery_id` does not resolve to a live pending delivery (expired, never
existed, belongs to a different queue), the server returns an error
immediately. It does **not** silently fall through to the tuple. The
precedence rule "`delivery_id` wins when both present" is unconditional —
supplying a `delivery_id` is a commitment that it must hold. Falling through
would mask client bugs and make the migration window dishonest about which
path was actually taken.

The four wire mappings, applied uniformly:

```text
RQL:           ACK ... WITH delivery_id = '<base32>'
gRPC:          message Ack { string delivery_id = 1; ... }
Postgres-wire: SET red.delivery_id = '<base32>'; ACK <queue> ;
HTTP:          { "delivery_id": "<base32>", ... }
```

When neither handle is present, the wire returns the standard "missing
argument" error of that transport (RQL parse error, gRPC `InvalidArgument`,
PG-wire syntax error, HTTP 400). This is the same as today's behavior when a
tuple field is missing.

## Alternatives Considered

**Bytes in proto, string elsewhere.** gRPC could carry `bytes delivery_id`
to avoid the base32 encode step. Rejected — saves ~5 bytes per ACK on a
path that is not bandwidth-bound, at the cost of four-wire heterogeneity and
a transport-specific test matrix.

**Client-visible deprecation signal.** Emit a warning to the client via
HTTP `Warning: 299`, gRPC trailer metadata, or an RQL `warnings[]` field.
Rejected — every transport needs its own deprecation channel implementation
and test surface, clients then need code to surface those warnings to their
own logs, and the resulting alert volume on production loops would force
client teams to silence it rather than migrate. Server log is the right
audience: it's the operator who'd care, and the operator can already grep
their logs.

**Fallback to tuple when `delivery_id` does not resolve.** Quietly try the
tuple if the `delivery_id` is dead. Rejected — masks a client bug as
"resilience", and worse, makes the deprecation telemetry untrustworthy
(operators reading the log think "the client sent a tuple, time to nudge
them" when in fact the client sent both and the `delivery_id` failed
silently). Strict precedence makes the migration boundary observable.

## Consequences

**Positive.**

- The four wire surfaces stay structurally identical for the `delivery_id`
  field — same string everywhere, easier to document, easier to grep across
  drivers.
- Deprecation telemetry stays clean: a log line means a tuple was used as
  the *primary* handle, full stop.
- Client teams can roll forward without code changes for a release; they
  upgrade by switching to sending `delivery_id`. No new client-side code to
  handle a deprecation channel.
- Failure mode is explicit: a stale `delivery_id` returns an error instead
  of pretending to ACK a different delivery.

**Negative.**

- Server-only deprecation visibility means a client running against a remote
  service won't notice the deprecation until the next release drops the
  tuple. Drivers will need to be updated and shipped in time.
- gRPC carries ~5 bytes more per ACK than a `bytes` field would, plus the
  base32 codec on the client side. Negligible at queue throughput targets.
- The strict-failure rule on stale `delivery_id` is one more error code
  surface across four transports. Mitigated by mapping it to each
  transport's existing "not found" code (HTTP 404 / gRPC `NotFound` /
  RQL `QueueDeliveryNotFound` / PG-wire SQLSTATE 02000).

**Operational note: deprecation log rate limiting.** A naive
`log::warn!` per request will flood logs for clients still on the tuple
path. The implementation must rate-limit deprecation lines per
`(client_addr, queue)` key — one line per minute is enough to signal the
migration without drowning the log. This is a small implementation
constraint, not a separate decision.

This ADR resolves the HITL aspects of issue #627. The remaining work
(parser additions, four wire integrations, table-driven tests, CHANGELOG)
is mechanical and can proceed as AFK.
