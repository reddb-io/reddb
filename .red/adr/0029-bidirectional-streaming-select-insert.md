# Bidirectional Streaming for SELECT and Bulk INSERT

Status: proposed

RedDB will add first-class **bidirectional streaming** to the wire protocol (RedWire) and to the HTTP surface so that result sets and bulk inputs can flow as a single piped exchange instead of being paginated by the caller. The motivating shape is `fs.createReadStream(...).pipe(connection.table.inputStream())` for ingest and `for await (const row of collection.stream(sql))` for consumption — both backed by the same framing and lifecycle on the server.

Streaming is distinct from the live-queue, ephemeral-notification, and durable-stream primitives covered by ADR 0028. Those carry **event delivery**; this ADR governs **result-set delivery** for ordinary queries and bulk writes. The two share neither lifecycle nor authorization model and must not be conflated.

## Decisions

**Shape.** Streams are bidirectional: a client opens an **output stream** to consume rows from a query, or an **input stream** to push rows into a table. Both expose object-mode at the API boundary; external formats (CSV, JSONL, Parquet) are parsed in user-space and piped into the object stream. The wire never carries opaque format-tagged blobs that the server would have to parse.

**Input stream commit model.** Input streams auto-commit per chunk; rows become visible as their chunk persists. There is no whole-stream atomic mode. Failure mid-stream leaves a recoverable prefix identified by the highest committed RID; the caller can resume from there in its own source. Atomic ingest of small payloads remains the job of `BEGIN ... COMMIT` around an in-memory batch — streaming is for payloads where holding everything in WAL until `end()` is hostile.

**Input stream feedback.** The server is silent on success and reports only errors and the terminal envelope. Transport (TCP, HTTP/1.1 chunked) provides ordered delivery; explicit ack-per-chunk would be redundant cost on the hot path. On error the server emits a single frame containing the offending chunk sequence, error reason, and the highest committed RID, so the client knows the resumable boundary without bookkeeping per chunk.

**Output stream consistency.** Output streams pin an MVCC snapshot at open and serve every chunk from that snapshot. The pin is bounded by `stream.snapshot.ttl_ms`; expired pins terminate the stream with `snapshot_expired`. This bounds MVCC garbage retention without surprising the common consumer. Resume after disconnect re-opens against the same snapshot LSN and applies `resume_after_rid` as a filter; queries without a stable total order are marked `resumable: false` at open so the client knows a disconnect means restart-from-zero.

**Chunk sizing.** Chunks are produced in page-aligned reads (N × 16 KiB, where 16 KiB is the engine's `PAGE_SIZE`). Defaults: 4 pages (64 KiB), with hard floor 1 page and hard ceiling 64 pages. The server flushes a chunk when the first of three caps fires: byte cap, row cap, or latency cap. Clients may hint per-stream values; the server rounds to the nearest legal page count and clamps to the hard ceilings. Page-alignment governs the **production buffer** on the server, not the wire encoding — wire frames carry row-encoded data so drivers stay thin and storage layout stays free to evolve.

**Transport.** RedWire reuses the existing `stream_id` and `MORE_FRAMES` multiplexing primitives; one TCP connection multiplexes multiple concurrent streams. HTTP carries NDJSON (`application/x-ndjson`) under `Transfer-Encoding: chunked`. Request body for input streams uses the same NDJSON framing. The choice of streaming versus non-streaming is content-negotiated on HTTP (via `Accept`) and exposed as a separate driver method (`collection.stream(sql)` vs `collection.query(sql)`); SQL grammar is unchanged.

**Cancellation.** RedWire requires an explicit `StreamCancel` frame so canceling one stream does not tear down a multiplexed connection. HTTP relies on connection abort (no multiplex). Drivers expose a uniform `stream.cancel()` that maps to the appropriate primitive. A server-side read-side idle cap forces cleanup if the client stops draining without canceling.

**Authorization.** Token expiry must not terminate a stream that the server already accepted. At `OpenStream` the server issues an internal, unforwarded **stream lease** bound to the snapshot pin and bounded by the snapshot TTL. The lease is the credential consulted for subsequent chunks; the bearer token authenticates only the open. Policy revocation mid-stream does not interrupt the stream, consistent with snapshot-pinned reads. This decouples credential rotation cadence from result-set delivery time.

**Integrity.** Input streams support opt-in end-to-end SHA-256: the client streams the rolling hash and emits the digest in the terminal frame; on mismatch the server marks the stream's committed RIDs with an integrity tombstone (rollback is not possible under auto-commit). Resume of an output stream **always** carries a prefix hash of the previously delivered rows so the resumed prefix can be detected if substituted. Per-frame HMAC was rejected: with TLS in place and an unguessable lease, it adds cost without closing a real gap.

**Transaction interaction.** Opening a stream while a `BEGIN` is active on the session is rejected at `OpenStream` with `stream_in_transaction_unsupported`. Stream lifetime and explicit-transaction lifetime are incompatible scopes; mixing them produces either read-your-writes surprises or interactive COMMIT semantics, both worse than an explicit refusal. Callers split bulk-read and transactional-write across sessions.

**Capacity.** Two caps apply: `stream.max_global` (defends the process from snapshot-pin and buffer pressure) and `stream.max_per_principal` (fairness and abuse defense). Both are enforced at `OpenStream`; refusal returns a structured error so clients can backoff. There is no implicit snapshot eviction under pressure — eviction of a stream that has already delivered gigabytes is hostile UX. Caps fail closed and fast.

**Engine integration.** The current executor materializes via `UnifiedResult { records: Vec<UnifiedRecord> }`. The first slice is a **wrapping shim**: the transport layer chunks an already-materialized `UnifiedResult` over the wire, giving incremental delivery to the client without engine refactor. Server memory does not improve in this slice. Subsequent slices add a pull-based path for scan executors (`parallel_scan`, `bitmap_scan`) where materializing 10⁷ rows on the server is the actual pain. Executors that must observe the full input by design (aggregation, sort, window) remain materializing; a `max_materialized_rows` ceiling protects them. A big-bang refactor of every executor is explicitly rejected — too much surface, too much risk, too little incremental value.

## Configuration

All runtime tunables live in the `red.config` KV under the `stream.*` namespace, dot-notation per ADR 0027 conventions:

| Key | Default | Meaning |
|---|---|---|
| `stream.max_global` | 256 | Hard cap on concurrent streams in the process |
| `stream.max_per_principal` | 32 | Per-principal concurrent stream cap |
| `stream.snapshot.ttl_ms` | 60000 | MVCC pin lifetime ceiling |
| `stream.chunk.default_pages` | 4 | Default chunk size (× 16 KiB) |
| `stream.chunk.min_pages` | 1 | Hard floor for client hints |
| `stream.chunk.max_pages` | 64 | Hard ceiling for client hints |
| `stream.chunk.max_rows` | 1000 | Row-count flush trigger |
| `stream.chunk.max_latency_ms` | 50 | Time-based flush trigger |
| `stream.integrity.default_verify` | `"none"` | `"none"` or `"sha256"` |
| `stream.integrity.resume_prefix_hash` | `true` | Always require prefix hash on resume |
| `stream.input.autocommit` | `true` | Locked; explicit for inspection |
| `stream.input.feedback_mode` | `"errors_only"` | Server feedback policy |
| `stream.lease.ttl_inherits_snapshot` | `true` | Lease TTL ≤ snapshot TTL |

Values captured at `OpenStream` are immutable on the lease; hot updates to the KV apply only to subsequent streams. Environment variables (`RED_STREAM_*`) only seed the KV on first boot.

## Rejected alternatives

- **Server-side parsing of external formats (CSV/JSONL on the wire).** Inflates the server's attack surface with format parsers, breaks the thin-driver posture of ADR 0007, and provides no benefit over piping a userland transform into an object-mode stream.
- **Whole-stream atomic input.** Either requires WAL/staging large enough for the full payload (hostile for the 10 GB case the feature exists for) or table-level locking. Auto-commit + recoverable prefix wins.
- **Server-side cursor for resume.** Requires per-stream execution state with spill management and crash cleanup. Re-execution against a pinned snapshot with an RID filter reuses primitives already present.
- **Ship raw page bytes on the wire.** Tightly couples drivers to internal storage layout, breaks under any predicate or projection (the server still has to decode), and is meaningless for JOIN/aggregation/sort. Page-alignment is preserved on the **production buffer**, not the wire.
- **Per-chunk acknowledgement on input.** Redundant with transport ordering and the terminal-envelope error report. Hot-path cost without recovery benefit.
- **SQL grammar extension (`SELECT ... AS STREAM`).** Couples delivery to query language; forces gramatical change across every transport and driver. Method/header surface is the right knob.
- **Server-side stream eviction under MVCC pressure.** Surprising termination of a stream that has already delivered gigabytes is worse than failing the next `OpenStream` with a structured capacity error.
- **Big-bang pull-based executor refactor.** Touches 12+ executors and blocks the rest of the roadmap to deliver value that a wrapping shim already produces for the common case.

## Threat model summary

- **In-flight tampering.** Mitigated by TLS at transport. Lease unguessability prevents stream hijack on a multiplexed RedWire connection. Per-frame HMAC adds no real gap given those.
- **Disk/middleware corruption of bulk ingest.** Mitigated by opt-in SHA-256 end-to-end on input streams; integrity tombstones mark inconsistent commits.
- **Resumption substitution.** Mitigated by always-on prefix hash on output-stream resume.
- **Token-lifetime DoS on long streams.** Mitigated by lease decoupling — credential rotation does not terminate accepted work.
- **Capacity exhaustion.** Mitigated by global + per-principal caps with structured refusal, not silent backpressure or surprise eviction.
- **Compromised client.** Out of scope. Payload-level signing belongs to the producer, not the wire.

## Phasing

1. **Shim slice.** Wrap existing `UnifiedResult` for incremental wire delivery. NDJSON over HTTP, RedWire chunked frames. Snapshot pin, lease, caps, cancellation, integrity hooks. Server memory does not improve.
2. **Input streams.** Auto-commit per chunk, error-only feedback, opt-in SHA-256. Driver `inputStream()` exposing Node `Writable`-compatible surface.
3. **Pull-based scans.** Convert `parallel_scan`, `bitmap_scan` to a pull iterator so server memory tracks the chunk buffer rather than the full result set. Aggregating executors remain materializing.
4. **Resume.** Output-stream resume with RID filter and prefix hash. `resumable: false` annotation for orderless queries.

Each slice is independently shippable; the shim slice alone delivers the graph-rendering use case without engine refactor.
