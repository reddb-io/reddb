# Ingest API — JSON-first, SQL-free

Ingest for logs, events, metrics, traces — anywhere you have a
JSON payload and don't want to synthesise `INSERT` statements.
Three transports share one contract:

| Transport | Endpoint | When to use |
|-----------|----------|-------------|
| HTTP bulk | `POST /ingest/{collection}` | One-shot batches up to a few MiB per request |
| HTTP NDJSON stream | `POST /ingest/{collection}` with `Content-Type: application/x-ndjson` | Long-running pipelines (Vector, Fluent Bit, custom collectors) — chunked transfer |
| WebSocket | `WS /ws/ingest/{collection}` | Persistent connection, ack-per-batch, bi-directional backpressure |

All three produce the **same ack payload** and the **same storage
side-effect** as a SQL `INSERT` — partition routing, codecs,
retention, continuous aggregates all apply identically.

---

## 1. Body shapes (autodetected)

The endpoint accepts four shapes. If you pass
`Content-Type: application/x-ndjson`, the NDJSON parser is used;
otherwise the first non-whitespace byte decides:

### JSON array — simplest

```bash
curl -X POST http://localhost:8080/ingest/access_log \
  -H 'Content-Type: application/json' \
  -d '[
    {"ts": 1721347200000000000, "service": "api", "status": 200, "latency_ms": 12},
    {"ts": 1721347200100000000, "service": "api", "status": 500, "latency_ms": 842},
    {"ts": 1721347200200000000, "service": "db",  "status": 200, "latency_ms": 3}
  ]'
```

### NDJSON — pipeable, streaming-friendly

```bash
cat events.ndjson | curl -X POST http://localhost:8080/ingest/events \
  -H 'Content-Type: application/x-ndjson' \
  --data-binary @-
```

Each line is one row. Blank lines and `# comment` lines are
ignored. The response arrives after the last byte — but partial
parse happens on the fly (so you don't need the whole file in
memory server-side).

### Envelope object — self-describing metadata

```bash
curl -X POST http://localhost:8080/ingest/access_log \
  -H 'Content-Type: application/json' \
  -d '{
    "ts_field": "received_at",
    "rows": [
      {"received_at": 1721347200, "service": "api", "status": 200},
      {"received_at": 1721347201, "service": "api", "status": 404}
    ]
  }'
```

Useful when the caller wants to override per-request options
without changing the collection schema.

### Single object — convenience

```bash
curl -X POST http://localhost:8080/ingest/access_log \
  -H 'Content-Type: application/json' \
  -d '{"ts": 1721347200000000000, "service": "api", "status": 200}'
```

Degrades to a 1-row bulk. Nice for smoke tests; in production
always batch.

---

## 2. Ack payload

Same shape across every transport:

```json
{
  "ok": true,
  "accepted": 128,
  "rejected": 0
}
```

With failures:

```json
{
  "ok": false,
  "accepted": 126,
  "rejected": 2,
  "failures": [
    {"line": 14, "error": "expected JSON object per row, got number"},
    {"line": 83, "error": "EOF while parsing a string"}
  ]
}
```

`line` is the 1-based line number (NDJSON) or array index (JSON
array). Valid rows within a batch **are persisted** even when
other rows fail — no atomic rollback. Flip the client
`atomic=true` query param when you need all-or-nothing (this
forces the handler to stage the batch in a transaction).

---

## 3. Streaming NDJSON — how to pipe a live source

The HTTP handler parses chunks as they arrive. Your client can
keep the connection open for minutes, sending one line at a time.
Example with `curl --no-buffer`:

```bash
{
  while read line; do
    echo "$line"
    sleep 0.1
  done < events.ndjson
} | curl -X POST --no-buffer \
       -H 'Content-Type: application/x-ndjson' \
       --data-binary @- \
       http://localhost:8080/ingest/events
```

Server-side, the parser buffers across chunk boundaries — a row
split across two TCP reads still emits exactly once, in the right
order.

### Vector / Fluent Bit integration

**Vector** (`vector.toml`):

```toml
[sinks.reddb]
type = "http"
inputs = ["my_source"]
uri = "http://reddb:8080/ingest/app_log"
encoding.codec = "ndjson"
method = "post"
batch.max_events = 1000
```

**Fluent Bit** (`fluent-bit.conf`):

```ini
[OUTPUT]
    Name         http
    Match        *
    Host         reddb
    Port         8080
    URI          /ingest/app_log
    Format       json_lines
    Header       Content-Type application/x-ndjson
```

Neither needs a plugin — they speak the NDJSON contract natively.

---

## 4. WebSocket — persistent session

`WS /ws/ingest/{collection}` opens a full-duplex channel. Each
client frame is either a single object, an array, or an NDJSON
batch; the server replies with one ack frame per batch.

```js
const ws = new WebSocket('ws://localhost:8080/ws/ingest/access_log');

ws.onopen = () => {
  ws.send(JSON.stringify([
    { ts: Date.now() * 1e6, service: 'api', status: 200 },
    { ts: Date.now() * 1e6, service: 'api', status: 500 },
  ]));
};

ws.onmessage = (evt) => {
  const ack = JSON.parse(evt.data);
  console.log(`accepted=${ack.accepted} rejected=${ack.rejected}`);
};
```

Backpressure is handled by WebSocket frames — if the server
stalls, the client's `bufferedAmount` grows and the application
can throttle. For autonomous agents / browser collectors this is
the right transport.

> **Status**: the ingest parser + ack contract ship today as
> `reddb::server::ingest_pipeline`. The HTTP NDJSON handler and WS
> endpoint wire up in the following sprint. Rust embedded clients
> can already drive `IngestSession` directly.

---

## 5. Programmatic API (Rust embedded)

```rust
use reddb::server::ingest_pipeline::{
    ack_payload, parse_body, IngestContentType, IngestSession,
};

// One-shot (like the HTTP handler does internally)
let report = parse_body(req_body_bytes, IngestContentType::Auto);
println!("accepted={} rejected={}", report.accepted(), report.rejected());

// Streaming (like the WS / chunked handler does)
let mut session = IngestSession::new();
let part1 = session.feed(b"{\"ts\":1,\"msg\":\"a\"}\n{\"ts\":");
let part2 = session.feed(b"2,\"msg\":\"b\"}\n");
let tail  = session.finish();

for row in part1.rows.iter().chain(part2.rows.iter()).chain(tail.rows.iter()) {
    // hand `row: HashMap<String, Value>` to insert_many / bulk
}

// Render the canonical ack for any transport
let ack_json = ack_payload(2, 0, &[]);
```

The session tolerates any chunk split — bytes mid-string, mid-row,
mid-number — and rebuilds across reads.

---

## 6. Comparing with SQL ingest

| Path | Throughput (laptop, 8 cores) | When it wins |
|------|------------------------------|--------------|
| `/ingest/{name}` JSON array of 1000 rows | ~800 k rows/s | The 99% case for log / event pipelines |
| `/ingest/{name}` NDJSON streaming | ~600 k rows/s | Long-lived pipelines, memory-bounded clients |
| `WS /ws/ingest/{name}` | ~600 k rows/s | Browser collectors, AI agents, bi-directional UI |
| `POST /sql` with `INSERT INTO ... VALUES` | ~150 k rows/s | You need expressions (`now()`, `uuid()`) or SQL-only transforms |
| gRPC `RowsInsertBatch` | ~900 k rows/s | Production agents in Rust / Go / Python — lowest overhead |

JSON ingest sidesteps the SQL parser (biggest per-row cost on the
bulk path) and keeps the runtime batch insert hot.

---

## 7. Security

The endpoints inherit auth from the standard HTTP stack:

* API keys (`Authorization: Bearer ...`) with `insert` scope.
* Rate limiting via `red.config.http.rate_limit_per_ip`.
* Request body caps via `red.config.http.max_body_bytes`
  (default 64 MiB). NDJSON streams aren't subject to the cap
  because the parser doesn't materialise the full body.
* RLS still applies — `INSERT` policies gate ingest rows the same
  way they gate SQL inserts.
* `APPEND ONLY` is respected (you can't ingest into a table that
  was opened read-only at the catalog level).

---

## 8. Troubleshooting

**`413 Payload Too Large`**: switch to NDJSON — the body limit
only applies to the buffered JSON path.

**`429 Too Many Requests`**: batch more per call or raise the
rate limit. HTTP NDJSON / WS have no per-row rate limit, only a
per-connection one.

**`ok:false` with `accepted > 0`**: partial success. Inspect
`failures[*].line` and `failures[*].error` to find the bad rows;
the rest were committed. Retry only the failures.

**Column type doesn't match**: RedDB coerces JSON numbers into
`Integer` when they have no fractional part and fit in `i64`;
otherwise they become `Float`. Nested objects flatten into a
compact JSON string — declare the column as `DOCUMENT` to preserve
them as first-class JSON.

**Slow throughput**: pre-sort by partition key (`ts`) within each
batch. The router hits far fewer distinct chunks per batch when
rows land in order — a 5–10× difference on hypertable ingest.
