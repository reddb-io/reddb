# ADR 0001 — RedWire: RedDB's binary TCP / TLS wire protocol

**Status:** Accepted
**Date:** 2026-04-26

## Decision

RedDB ships a single binary wire protocol called **RedWire**. It runs
over TCP, TLS, or Unix domain sockets on port 5050 by default, shares
a port with HTTP and gRPC behind the service-router, and is the only
binary protocol the server implements. Drivers in every supported
language speak it natively.

Replaces / supersedes any earlier ad-hoc framing scheme that may
appear in legacy commit history.

## Goals

1. Single round-trip auth handshake (anonymous, bearer, SCRAM-SHA-256, OAuth-JWT).
2. Frame multiplexing (`stream_id`) so one TCP connection carries N concurrent queries.
3. Per-frame compression (zstd) for bulk insert / large result sets.
4. Forward-compatible version negotiation gated on a single magic byte.
5. Zero-copy fast paths for the high-throughput data plane (`BulkInsertBinary`,
   `QueryBinary`, `BulkInsertPrevalidated`, streaming bulk).

## Frame layout

```text
┌──────────────────────────────────────────────────────────┐
│ Header (16 bytes, little-endian)                          │
│   u32   length         total frame size, incl. header     │
│   u8    kind           MessageKind                         │
│   u8    flags          COMPRESSED | MORE_FRAMES | …        │
│   u16   stream_id      0 = unsolicited; otherwise multiplex│
│   u64   correlation_id request↔response pairing           │
├──────────────────────────────────────────────────────────┤
│ Payload (length - 16 bytes)                               │
└──────────────────────────────────────────────────────────┘
```

* `length` includes the 16-byte header. Max frame size: 16 MiB.
* `kind` is the single-byte `MessageKind` discriminator (table below).
* `flags` carries `COMPRESSED` (payload is zstd-encoded) and
  `MORE_FRAMES` (logical message is split across multiple frames).
* `stream_id` lets clients multiplex requests on one socket. `0`
  means "no stream" — used for handshake / lifecycle frames.
* `correlation_id` pairs requests with responses; clients pick it,
  the server echoes it on every reply.

## Magic byte

Every RedWire connection opens with the magic byte `0xFE`, immediately
followed by a single minor-version byte and the first frame
(typically `Hello`).

The `0xFE` magic lets the service-router multiplex RedWire on the same
TCP port as HTTP/1.x and HTTP/2 (gRPC) without ambiguity:

* HTTP/1.x methods all start with ASCII letters (≤ 0x5A).
* HTTP/2 starts with the literal `PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n`.
* `0xFE` cannot appear as the first byte of either.

## MessageKind table

| Code | Kind | Direction | Notes |
|------|------|-----------|-------|
| 0x01 | `Query`                  | C→S | UTF-8 SQL payload |
| 0x02 | `Result`                 | S→C | columnar response |
| 0x03 | `Error`                  | S→C | UTF-8 error message |
| 0x04 | `BulkInsert`             | C→S | JSON bulk shape |
| 0x05 | `BulkOk`                 | S→C | `{ affected }` |
| 0x06 | `BulkInsertBinary`       | C→S | typed columnar bulk |
| 0x07 | `QueryBinary`            | C→S | direct-scan fast path |
| 0x08 | `BulkInsertPrevalidated` | C→S | skip server-side normalisation |
| 0x09 | `BulkStreamStart`        | C→S | open streaming bulk session |
| 0x0A | `BulkStreamRows`         | C→S | append rows |
| 0x0B | `BulkStreamCommit`       | C→S | finalise + flush |
| 0x0C | `BulkStreamAck`          | S→C | progress ack |
| 0x0D | `Prepare`                | C→S | parse + cache statement |
| 0x0E | `PreparedOk`             | S→C | stmt_id + param count |
| 0x0F | `ExecutePrepared`        | C→S | bind + execute by stmt_id |
| 0x10 | `Hello`                  | C→S | handshake — version + auth methods |
| 0x11 | `HelloAck`               | S→C | server picks version + method |
| 0x12 | `AuthRequest`            | S→C | challenge (SCRAM server-first) |
| 0x13 | `AuthResponse`           | C→S | bearer / SCRAM client-first/final / JWT |
| 0x14 | `AuthOk`                 | S→C | session id + role |
| 0x15 | `AuthFail`               | S→C | reason string |
| 0x16 | `Bye`                    | both | graceful close |
| 0x17 | `Ping`                   | C→S | keepalive |
| 0x18 | `Pong`                   | S→C | keepalive reply |
| 0x19 | `Get`                    | C→S | `{ collection, id }` |
| 0x1A | `Delete`                 | C→S | `{ collection, id }` |
| 0x1B | `DeleteOk`               | S→C | `{ affected }` |
| 0x20 | `Cancel`                 | C→S | cancel in-flight `correlation_id` |
| 0x21 | `Compress`               | C→S | enable/disable zstd for session |
| 0x22 | `SetSession`             | C→S | per-session knobs |
| 0x23 | `Notice`                 | S→C | async notice |
| 0x24 | `RowDescription`         | S→C | streamed schema header |
| 0x25 | `StreamEnd`              | S→C | end-of-stream marker |
| 0x26 | `VectorSearch`           | C→S | vector index probe |
| 0x27 | `GraphTraverse`          | C→S | graph walk |

Kinds are stable: a value once shipped is never reused for a different message.

## Handshake

```
C → magic(0xFE) + minor_version(u8)
C → Hello { versions, auth_methods, features }
S → HelloAck { chosen_version, chosen_method, server_features }

# 1-RTT methods (anonymous, bearer, oauth-jwt):
C → AuthResponse { credentials }
S → AuthOk { session_id, username, role } | AuthFail { reason }

# 3-RTT method (scram-sha-256, RFC 5802 + RFC 7677):
C → AuthResponse { client-first-message }
S → AuthRequest  { server-first-message }
C → AuthResponse { client-final-message }
S → AuthOk       { v=server-signature } | AuthFail { reason }
```

Anonymous and OAuth-JWT bypass the SCRAM challenge round; SCRAM is
the only multi-RTT method. The handshake state machine lives in
`src/wire/redwire/auth.rs`; the dispatch loop lives in
`src/wire/redwire/session.rs`.

## Auth methods

| Method | RTT | Use case |
|--------|-----|----------|
| `anonymous`     | 0 | dev / unauth-allowed deployments |
| `bearer`        | 0 | static API tokens, env-injected secrets |
| `scram-sha-256` | 2 | password auth without sending the password |
| `oauth-jwt`     | 0 | OIDC / OAuth2 — server validates JWT against JWKS |

## Compression

Frames carrying `Flags::COMPRESSED` have a payload pre-compressed
with zstd. Servers and clients negotiate compression via the
`Compress` control frame (or out-of-band as a session feature). zstd
was chosen over snappy / zlib because it hits MongoDB's cited 50-70 %
ratios on JSON-heavy bulk inserts at noticeably lower CPU than zlib.

## Multiplexing

`stream_id != 0` lets clients fan out concurrent queries on a single
TCP connection. Servers handle each `stream_id` independently;
`correlation_id` still pairs every response back to its request.
Streams are virtual — there is no per-stream flow control beyond
`MORE_FRAMES`. A client wanting flow control wraps frames in its
own application-level windowing.

## Error model

Every server-emitted error is an `Error` frame (kind `0x03`) with a
UTF-8 payload describing the failure. Authentication failures use
`AuthFail` (`0x15`) instead — the distinction lets clients react
without parsing the error string.

## Listener surface

* `start_redwire_listener(config, runtime)` — plain TCP.
* `start_redwire_listener_on(listener, runtime)` — externally owned
  `TcpListener` (used by the service router).
* `start_redwire_tls_listener(addr, runtime, tls_config)` — TLS.
* `start_redwire_unix_listener(path, runtime)` — Unix domain socket.

The default port is `5050`. The service-router can multiplex RedWire
behind any port shared with HTTP and gRPC.

## Driver matrix

The official drivers all speak RedWire as their primary transport
and fall back to HTTP only when explicitly requested via URL scheme
(`http://` / `https://`). Implementations live under `drivers/`:
Rust, JavaScript / Node / Bun, Python (asyncio), Go, Java, Kotlin,
C++, .NET, PHP, Zig, Dart.

## Consequences

* RedDB ships one TCP-level binary protocol — no parallel framing
  schemes, no compatibility shims.
* The router can mux RedWire alongside HTTP/gRPC on a single port.
* Adding a new message kind is a pure add: pick the next code in the
  appropriate range, document it in this ADR, implement the
  handler. Never reuse a retired code.
