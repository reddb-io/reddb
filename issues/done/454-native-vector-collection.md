# Native vector collection: inserts + brute-force `VECTOR SEARCH` [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#445

## What to build

A new collection kind backed by an on-disk vector index. The initial implementation is a brute-force scan: correct, deterministic, and adequate for the showcase's scale (<= tens of thousands of vectors). The index abstraction is designed so that an HNSW / IVF implementation can replace the scan in a separate PRD without touching the executor or the wire surface.

Scope:

- Inserts of the shape `INSERT INTO <coll> (id, embedding, metadata) VALUES (...)`, where `embedding` is a fixed-length float array of the dimension declared at CREATE time.
- Insert-time validation: wrong dimension produces a clear error.
- `VECTOR SEARCH <coll> SIMILAR TO [...] [METRIC m] [THRESHOLD t] [LIMIT k]` returns top-k matches with scores, ordered per the declared (or query-overridden) metric.
- Supported metrics: `cosine`, `l2`, `inner_product`. The order semantics are: cosine and inner_product are descending (higher = better), l2 is ascending (lower = better).
- The `METRIC` clause in `VECTOR SEARCH` overrides the collection default for that query.
- The wire format for the embedding is the existing `Value::Vector` variant.

## Acceptance criteria

- [x] `CREATE VECTOR v DIM 4 METRIC cosine` + correctly-shaped inserts succeeds.
- [x] Insert with wrong dimension produces a clear error naming the expected and actual dimensions.
- [x] `VECTOR SEARCH v SIMILAR TO [0.1, 0.2, 0.3, 0.4] LIMIT 3` returns three rows ordered by similarity.
- [x] All three metrics produce the documented ordering on a hand-checked 10-vector fixture.
- [x] `THRESHOLD t` filters results past the threshold (semantics documented for each metric).
- [x] `LIMIT k` is honored.
- [x] Golden test: cosine top-k matches the engine cosine implementation byte-for-byte on the same fixture (same input vectors, same query).
- [x] The index abstraction is a deep module with a narrow interface (`upsert`, `delete`, `search(query, k, metric, threshold)`); the brute-force scan is one implementation behind it.

## Blocked by

- #453 (CREATE VECTOR DDL)

## Completion

- Added declared-vector insert dimension validation.
- Added `embedding` as a vector insert column alias for the existing `Value::Vector` path.
- Added `runtime::vector_index::BruteForceVectorIndex` with `upsert`, `delete`, and metric-aware `search`.
- Routed runtime `VECTOR SEARCH` through exact metric-aware search using collection defaults with query metric override.
- Verified with targeted runtime vector tests, an existing vector reference-search integration test, `cargo check`, and `git diff --check`.
