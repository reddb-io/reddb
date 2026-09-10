# Context graph expansion memory admission

`graph_max_edges` bounds the ordered candidate selection and admitted adjacent
edges during traversal. It does not bound the degree that must be inspected or
the identity caches required by those probes. Previously, that preparation could allocate candidates, identity
caches and payload copies without consulting the runtime's shared memory budget.

## Admission and ownership

Context graph expansion now reserves estimated query-owned memory through the
same runtime reservation seam as admitted writes. Runtime clones share the
reservation pool. Candidate arrays, alias lists, identity/adjacency caches,
hydrated graph payloads, traversal state and added result copies consume credits
before their allocation. Container estimates include growth/occupancy slack;
payload estimates charge shared values conservatively per query owner.

Reservations grow in 64 KiB quanta; when slack does not fit, only the required
bytes are requested. The caller holds the guards through context result assembly,
including subsequent vector expansion. Temporary cursor and batch scopes spend
credits from this same query-local bank and refund their usage to it on drop.
They cannot strand separate 64 KiB reservations and reject an otherwise small
query. Runtime reservations remain held at the query's conservative high-water
mark until completion; errors and unwinding release the guards.
The existing sampler reconciles completed reservations before admitting the next
operation. This introduces no independent per-query budget setting.

[Indexed graph cursors](search-graph-candidate-cursor.md) merge edge streams in
physical-ID/collection order using 32 IDs per active source/key stream and a
256-entry output buffer. Each stream seeks after its last consumed physical ID.
Source handles, streams and heap growth are admitted before allocation; edge
payload credits are released after each consumed output batch.
An interrupted partial batch never reaches the consumer.
Payload reads inspect the size under the owning segment lock, reserve outside
storage, then recheck before cloning. A larger concurrent replacement requires
additional admission. RLS evaluation remains outside segment locks.

Insufficient headroom returns the existing memory-budget error. It does not
truncate neighbors, weaken RLS, change snapshot selection or relax durability.
Logical identity, deterministic endpoint/edge ordering, edge allowances, depth
limits, score decay and strongest-source deduplication retain their contracts.

## Cost and remaining boundaries

Cost sketch: each physical index entry is examined once per requested key and
captured segment. Each batch resumes with an indexed seek. Source capture uses
O(S) handles, including scoped sources without graph matches, plus O(32R) stream
buffers/heap metadata for R active source/key streams. The 256-entry output
buffer costs 4 KiB on 64-bit platforms. Heap work is O(D log(R + 1)) for D raw
occurrences; smaller buffers require more indexed seeks. There are no degree-sized
ID arrays or geometric prefix rescans. See the cursor contract for snapshot and
consolidation details. There are no new WAL records, formats, fsyncs or network
round trips.

Identity, selected adjacency, hydrated payload and result credits remain
conservative over the query lifetime. Physical-edge deduplication retains one last-visible ID across output batches. Ordered
selection retains at most `graph_max_edges` authorized candidates plus one
replacement. Eviction refunds candidate strings and displaced policy payloads;
container capacity remains charged until selection drops. Selected policy
payloads transfer their credits into query-lifetime ownership without duplicate
admission. Later expansions reuse the discarded candidates' credits. Temporary edge payloads are charged per batch, then released;
within a batch, their credits still accumulate conservatively. This is not a
precise peak-memory allocator or a hard process RSS cap. Degree still determines preparation work; source/alias topology determines cursor metadata.
Candidate ordering and retained adjacency are bounded by the edge allowance per
expanded node; identity/hydration cache bounds remain separate performance work.
RLS may inspect additional competitive node payloads before the final selection;
unrestricted losing payloads remain unhydrated.

This slice does not admit earlier context/global search buffers, policy-evaluator
scratch, collection catalog enumeration, subsequent vector-search allocations,
serialization, or results retained by the caller after return. Existing input
payloads remain owned by the earlier stages. Index reconstruction after structural
invalidation and other maintenance allocations retain their existing accounting
boundaries. Completing admission across these stages remains required for a
shared multimodel peak-memory guarantee. No competitive performance lead is
inferred from this change.

## Verification

Runtime regressions exercise high degree with a one-edge allowance, large reached
payloads, growing/sealed segments, concurrent reservations, query completion,
failure and panic cleanup. Existing public graph-search tests cover RLS-denied
bridges, collection scope, MVCC/savepoints, reopen and deterministic limits.
Allocation regressions retain the checks against copying unrelated graph/vector
payloads or building a whole-graph candidate map.

The ordered-merge regression returns the same two nodes for 32,768 parallel
edges and a one-edge allowance with 512 KiB of available query headroom, in
both growing and sealed storage. The parent physical-ID deduplication set fails
admission. Completion returns all query credits. This does not measure latency,
process RSS or a competitive performance lead.
