# Context search vector expansion

Context search uses the exact, policy-before-top-k `search_similar` executor for
its optional vector expansion. A `SimilarResult` already owns the selected
physical entity, score and distance. The context result moves that entity into
its result map instead of fetching it again by ID from the global entity cache.
The enclosing statement frame supplies the snapshot and identity during scoring;
the selected payload crosses the composition boundary with that same result.

Collection scope still bounds which vector collections are searched. Already
scored physical IDs are skipped, vector discoveries retain their 0.9 score factor
and `VectorQuery` provenance, and existing per-collection top-k and final bucket
limits are unchanged. This slice changes neither the vector scoring algorithm
nor the snapshot, policy, WAL, persistence or wire contracts.

## Cost and validation

Cost sketch: for K selected payloads of B bytes each, moving the existing result
avoids K extra entity lookups and approximately K×B bytes copied on cache hits,
or 2×K×B on cold lookups that populate the cache. The context result map and
collection/provenance envelopes still allocate. No additional disk writes,
fsyncs or network round trips are introduced.

The allocation regression compares direct exact search with context expansion,
with token/global/graph/cross-reference discovery inactive. It exercises 128 and
16,384 dimensions, one and four results, growing and sealed segments, and repeated
reads. Additional context allocations must remain below 16 KiB, independent of
selected vector width. Both paths must return the same IDs, dense values and
scores (with the existing context decay).

A standalone public-API probe on local debug builds returned the same single
vector in both paths. It warmed direct exact search before measuring context
expansion, with all other discovery stages inactive:

| Dimensions | Lookup state | Parent context allocations | Reused-result allocations | Extra bytes over direct exact search, parent → reused |
|---|---|---:|---:|---:|
| 128 | Cold | 9,546 | 8,482 | 6,509 → 5,445 |
| 128 | Warm | 9,014 | 8,482 | 5,977 → 5,445 |
| 16,384 | Cold | 334,666 | 203,554 | 136,557 → 5,445 |
| 16,384 | Warm | 269,110 | 203,554 | 71,001 → 5,445 |

Units are cumulative calling-thread allocated bytes, not peak memory or timing.
Cold means the first context lookup after direct search; warm means a repeated
context call. The optimized path does not need either entity-cache lookup.

Behavioral coverage checks RLS before top-k, deny-default, collection scope,
transaction-local inserts/deletes, an older reader across a writer commit,
savepoint and transaction rollback, and clean reopen. Clean reopen is not a
process/power-loss test. These tests live in
`tests/grouped/ai_local_vector/e2e_search_vector_expansion.rs` and
`tests/grouped/ai_local_vector/e2e_vector_exact_allocations.rs`.

Exact scoring still scans candidates and hydrates bounded payload batches outside
segment locks; its [segment cursor](vector-scan-cursor.md) replaces the former O(N)
per-query ID list. Its own top-k payload copies and the global
text fallback are separate work. Allocation measurements do not establish
latency, throughput or performance parity with other databases.
