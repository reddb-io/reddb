# Query Streaming Contract (`POST /query/stream`)

This page is the single, canonical contract for RedDB's streaming-read
surface. It consolidates the wire shape, cursor lifecycle, cancellation,
and scope rules that were delivered across the #750 split so a client can
implement against one document.

It is the umbrella deliverable of [#750](https://github.com/reddb-io/reddb/issues/750)
(query cursor cancellation and SELECT streaming) and assembles the pieces
shipped by its slices:

| Slice | Issue | What it contributed |
|:------|:------|:--------------------|
| 750a — route | [#805](https://github.com/reddb-io/reddb/issues/805) | The `POST /query/stream` NDJSON transport: descriptor-first emission, chunked encoding, the read-only gate. |
| 750b — executor channel | [#806](https://github.com/reddb-io/reddb/issues/806) | The bounded-memory `RowStream` output channel the executor produces rows through, preserving every pipeline shape's ordering and snapshot guarantees. |
| 750c — cursor contract | [#807](https://github.com/reddb-io/reddb/issues/807) | The opaque, scoped, snapshot-pinned resume cursor and its TTL/expiry semantics. |
| 750d — cancel contract | [#808](https://github.com/reddb-io/reddb/issues/808) | Explicit cancellation, client-disconnect detection, and the cursor tombstone. |

The HTTP reference's [Streaming reads section](http.md#streaming-reads-with-resumable-cursors-post-querystream)
carries the same request/response examples; this page is the normative
behavioural contract those examples illustrate.

## Descriptor-first emission

A stream always begins with a `descriptor` frame **before any row**. The
descriptor names the result columns and carries a `schema_fingerprint`, so
a client can initialise its view (column headers, typed decoders) before
the first datum arrives. The descriptor wire shape is frozen for #750 —
new metadata is added as separate control frames, never folded into the
descriptor.

## Chunk format

The response is `application/x-ndjson` over HTTP/1.1
`Transfer-Encoding: chunked`. Each frame is one newline-delimited JSON
object. Frames arrive in a fixed order:

1. `descriptor` — columns + `schema_fingerprint` (always first).
2. `cursor` — the opaque resume token control frame (see below).
3. zero or more `row` frames — one record each, in result order.
4. a terminal frame — `end` on a complete stream, or `cancelled` when the
   stream was cut short.

```json
{"descriptor":{"columns":["id","name"],"schema_fingerprint":"…"}}
{"cursor":{"token":"<48-hex>","snapshot_lsn":42,"ttl_ms":60000,"expires_at_ms":1750000060000,"resumable":true}}
{"row":{"id":1,"name":"alice"}}
{"row":{"id":2,"name":"bob"}}
{"end":{"row_count":2}}
```

The producer writes through a bounded buffer (slice 750b) and flushes
incrementally — large results are delivered in multiple wire chunks rather
than buffered whole, so memory stays bounded regardless of result size and
a slow reader exerts backpressure without losing or reordering rows.

## Ordering guarantees

Row order is exactly the order the query's pipeline produces — an
`ORDER BY` slice arrives sorted, a join stays in its join/`ORDER BY`
order, and an unfiltered scan yields every row exactly once with no drops
or duplicates across chunk boundaries. Rerouting the executor through the
streaming channel (slice 750b) does not disturb any pipeline's ordering.
The control-frame order (descriptor, then cursor, then rows, then
terminal) is itself part of the contract and is preserved on resume.

## Snapshot consistency

Every stream reads one consistent snapshot. The snapshot is pinned when
the stream opens; the pin's LSN is reported as `snapshot_lsn` in the
cursor frame. Writes that commit after the stream opens are **not** visible
to that stream — a later stream sees them, the pinned one does not. A
resume re-streams the same pinned snapshot, not a fresh read, so the
descriptor and rows are identical across the original stream and its
resume.

## Cursor lifecycle

The `cursor` control frame carries an **opaque resume token** — treat it
as bytes; it encodes nothing a client should parse. A cursor is:

- **Minted** when a fresh stream opens (a `{"query": …}` body).
- **Resumed** by POSTing the token back with no `query` field; the pinned
  query and snapshot live server-side, so a resume replays the same
  descriptor-first stream.
- **Invalidated** by expiry (TTL elapsed) or by cancellation (explicit or
  disconnect) — both make a subsequent resume fail with a distinct,
  documented status (see below).

```bash
curl -X POST http://127.0.0.1:5000/query/stream \
  -H 'content-type: application/json' \
  -H 'x-reddb-tenant: acme' \
  -H 'authorization: Bearer <principal-token>' \
  -d '{"cursor": "<token from the cursor frame>"}'
```

## Tenant scope

A cursor is bound to the **tenant** that opened it. The tenant comes from
the `x-reddb-tenant` header (or the bearer credential). A token presented
under a different tenant is refused as if it never existed — see
*Authorization scope* for the uniform masking rule.

## Authorization scope

A cursor is bound to the `(tenant, principal)` pair that opened it; the
principal comes from the bearer credential. Resume and cancel are scoped
identically:

- A token that is **unknown**, or presented by a **different tenant or
  principal**, is refused with `404 cursor_not_found`. The response is
  byte-identical across all three cases, so an unauthorized caller cannot
  distinguish a foreign cursor from one that never existed — **no
  existence leak**. The refusal never echoes the token and never says
  "forbidden".
- A foreign probe leaves the rightful owner's cursor untouched and
  resumable.

## TTL and expiry

The cursor's snapshot pin has a time-to-live. It defaults to
`stream.snapshot.ttl_ms` (60 000 ms) and is tunable at runtime via
`PUT /config/stream.snapshot.ttl_ms`. After the TTL elapses, the
**rightful owner's** resume is refused with `410 cursor_expired` — distinct
from `404` (unknown/foreign) so the owner learns the difference between
"aged out" and "never yours". Open a new stream to obtain a fresh cursor.

## Cancellation

A long-running read can be stopped two ways; both signal the executor to
stop producing rows and **tombstone** the cursor.

- **Explicit cancel** — POST the token to `POST /query/stream/cancel`:

  ```bash
  curl -X POST http://127.0.0.1:5000/query/stream/cancel \
    -H 'content-type: application/json' \
    -H 'x-reddb-tenant: acme' \
    -H 'authorization: Bearer <principal-token>' \
    -d '{"cursor": "<token from the cursor frame>"}'
  ```

  A matched cursor returns `200 {"ok":true,"status":"cancelled"}`. Cancel
  is scoped exactly like resume — an unknown or foreign token is masked as
  `404 cursor_not_found`, so cancellation cannot probe for cursors. A body
  with no cursor is `400 cursor_required`. Cancel is **idempotent**:
  cancelling an already-cancelled cursor still returns `200`.

- When a cancel is observed while a stream is still draining, the stream
  terminates with a `cancelled` terminal frame in place of `end`:

  ```json
  {"cancelled":{"row_count":<rows emitted so far>,"reason":"cancelled"}}
  ```

**Resuming a cancelled cursor** is refused to its owner with
`409 cursor_cancelled` — distinct from `410 cursor_expired` (aged out) and
`404 cursor_not_found` (unknown/foreign) — so the client learns the stream
was cancelled rather than retrying an abandoned snapshot.

## Disconnect semantics

If the client closes the TCP connection mid-stream, the server detects the
broken pipe (a `BrokenPipe` / `ConnectionReset` / `ConnectionAborted`
write failure), raises the same executor cancel signal, and tombstones the
cursor — it does not keep computing rows for a dead client. A subsequent
resume of that tombstoned cursor is refused with `409 cursor_cancelled`,
identical to an explicit cancel.

## Limits — the read-only gate

`POST /query/stream` accepts **read-only `SELECT`** statements only. A
non-read-only statement (e.g. an `INSERT`) is refused with a
non-streaming, structured `400` naming the statement kind, and the
mutation provably does not run:

```json
{"ok":false,"code":"stream_unsupported_statement","statement_kind":"mutation"}
```

Refusals are always non-streaming JSON responses (no chunked body), so a
client can cleanly distinguish "the stream was never accepted" from a
mid-stream failure.

## Status-code summary

| Status | Code | When |
|:-------|:-----|:-----|
| `200` | — | Stream accepted; descriptor-first NDJSON follows. |
| `200` | `cancelled` (cancel endpoint) | Explicit cancel matched and tombstoned the cursor. |
| `400` | `stream_unsupported_statement` | Non-read-only statement refused by the read-only gate. |
| `400` | `cursor_required` | Cancel body carried no cursor token. |
| `404` | `cursor_not_found` | Token unknown, or presented by a foreign tenant/principal (no existence leak). |
| `409` | `cursor_cancelled` | Resume of a cancelled / disconnected (tombstoned) cursor. |
| `410` | `cursor_expired` | Owner's resume after the snapshot TTL elapsed. |

## Conformance

The full behavioural matrix is exercised end-to-end over real HTTP in
`crates/reddb-server/tests/e2e_issue_809_query_stream_matrix.rs`: paging,
streaming chunk order, descriptor-first emission, cursor expiry /
invalidation, authorization scope, disconnect and cancellation, a
representative long-running read, and the read-only gate. The per-slice
suites (`e2e_issue_805_*`, `e2e_issue_806_*`, `e2e_issue_807_*`,
`e2e_issue_808_*`) remain the focused contract tests for each piece.
