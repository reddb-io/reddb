# Indexed graph candidate batches

Context graph expansion uses the maintained per-segment graph index through a
resumable physical-candidate cursor. It replaces the full candidate ID array and
geometric retries that repeatedly scanned an already consumed prefix. The final
adjacency is still resolved and ordered before applying `graph_max_edges`.

## Read and ownership contract

Each collection captures its growing/sealed source inventory under the topology
locks. Source handles pin retired segments across consolidation; the existing
retained-segment accounting includes them until the cursor releases them. Capture
reserves its handle-array estimate outside storage locks and retries the small
inventory array if concurrent sealing grew the inventory before capture.

Before delivering any batch, each captured source records the largest physical
ID across all requested identity/endpoint keys. A segment cursor then visits one
key at a time. Each batch seeks strictly after the last consumed ID and stops at
that source bound, including the `u64::MAX` boundary. Later appends above the
bound are excluded even for keys visited in a later batch. This is a physical traversal bound,
not an MVCC snapshot or a promise about arbitrary low-level inserts with reused
or out-of-order IDs. The enclosing statement snapshot remains authoritative.

The batch contains at most 256 IDs. Consumers and memory admission run outside
segment/topology locks. Consumers inspect/hydrate through the current manager,
so updates after consolidation cannot make a retired source's payload authoritative.
Hidden edge versions are rejected before sizing/cloning. Node/edge RLS still runs
outside storage locks. Index rebuilds remain cooperative; cancelled partial
batches and interrupted rebuilds never become successful partial results.

Visible physical edges are deduplicated across aliases before authorization and
adjacency insertion. Logical identity, collection scope, node policies, stable
endpoint/edge ordering, self-loop handling, per-source edge limits and score decay
retain their existing contracts. The consumer can stop early; errors release the
source handles and reservations.

## Cost and limits

Cost sketch: S captured source handles plus a fixed 256 × 8-byte ID buffer;
O(D + (B + K) log N) indexed work for D candidate occurrences, B batches and K key
opens across the captured segments. Alias-key duplicates remain candidate
occurrences. Each candidate occurrence is inspected once, without prefix rescans.
Cursor metadata and edge payloads spend credits from the same query-local bank
as retained adjacency. Edge payload credits accumulate only within the current
batch and return to that bank after its consumer finishes; temporary payload admission no longer scales with all D
edges over the entire query. There is no new persisted index, WAL record, fsync,
network hop or storage format.

Deduplication, identity and ordered-adjacency caches still grow with reached
identities and incident degree, under shared runtime admission. The physical
index is not ordered by authorized logical neighbor, so stopping after the first
K physical IDs would change results. Bounded top-k selection and precise
peak-memory admission across the complete multimodel pipeline remain pending.
Cold index rebuilds and policy-evaluator scratch retain their previous bounds.

## Regression evidence

The same query with 2,048 parallel edges, 4 KiB edge properties, one requested
neighbor and 4 MiB of available headroom fails on the parent implementation:
temporary payload reservations exhaust the budget before traversal. The batched
implementation must return the same seed and neighbor and return all query
credits after completion, for growing and sealed collections. This is an
admission regression, not a measured process-RSS or competitive latency result.

Small-query regressions leave 64 KiB and 130 KiB of headroom while another
operation holds the rest. Nested temporary scopes must share the same query
credits. Unwinding one temporary scope returns only its own credits, leaving
other live scopes charged.

Storage regressions compare the cursor with the complete indexed reader, exercise
missing keys, maximum IDs, appends/sealing between batches and cancellation.
A consolidation test pins the retired source, updates an unconsumed edge in the
merged segment, and checks that live hydration sees the updated payload exactly
once. Public graph-search and allocation suites retain the existing semantic
and unrelated-data regression checks.
