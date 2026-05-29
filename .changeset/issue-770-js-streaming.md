---
"@reddb-io/client": minor
---

feat(js-client): Node-native streaming surface (#770 / PRD #759 S11)

Expose the streaming wire from the JS driver:

- `db.collection(name).stream(sql)` / `db.stream(sql)` — a Node `Readable`
  in object mode that also conforms to `AsyncIterable<Row>`. Backpressure
  flows via `read()` / `pause()` / `resume()`; errors surface as `'error'`
  events and rejected iterations.
- `db.collection(name).inputStream()` / `db.inputStream(target)` — a Node
  `Writable` in object mode. Backpressure via `write()` + `'drain'`; the
  server's terminal envelope resolves a `.completion()` promise.
- `.cancel(reason?)` on both — `StreamCancel` over RedWire,
  `AbortController.abort()` over HTTP NDJSON.
- `splitNdjson()` transform for piping NDJSON files into `inputStream()`.

Transport is chosen by the connection (RedWire when available, HTTP NDJSON
otherwise) with an identical caller-facing surface. `db.query()` stays a
one-shot Promise — no streaming-surface leakage.
