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

Behavioral coverage checks RLS before top-k, deny-default, collection scope,
transaction-local inserts/deletes, an older reader across a writer commit,
savepoint and transaction rollback, and clean reopen. Clean reopen is not a
process/power-loss test. These tests live in
`tests/grouped/ai_local_vector/e2e_search_vector_expansion.rs` and
`tests/grouped/ai_local_vector/e2e_vector_exact_allocations.rs`.

Exact scoring still scans candidates, retains an O(N) ID list and hydrates bounded
payload batches outside segment locks. Its own top-k payload copies and the global
text fallback are separate work. Allocation measurements do not establish
latency, throughput or performance parity with other databases.
