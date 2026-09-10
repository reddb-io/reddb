# Context graph expansion memory admission

`graph_max_edges` limits admitted adjacent edges during traversal. It does not
limit the degree of the node whose adjacency must first be resolved, authorized
and ordered. Previously, that preparation could allocate candidates, identity
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
including subsequent vector expansion. Errors and unwinding release the guards.
The existing sampler reconciles completed reservations before admitting the next
operation. This introduces no independent per-query budget setting.

Candidate probes allocate a reserved fixed-capacity array before entering segment
locks. On overflow, the array is discarded and the probe restarts with twice the
admitted capacity. An incomplete probe is never used as a successful result.
Payload reads inspect the size under the owning segment lock, reserve outside
storage, then recheck before cloning. A larger concurrent replacement requires
additional admission. RLS evaluation remains outside segment locks.

Insufficient headroom returns the existing memory-budget error. It does not
truncate neighbors, weaken RLS, change snapshot selection or relax durability.
Logical identity, deterministic endpoint/edge ordering, edge allowances, depth
limits, score decay and strongest-source deduplication retain their contracts.

## Cost and remaining boundaries

Cost sketch: for D physical incident candidates, geometric retries inspect O(D)
candidates on a stable graph, plus O(log D) indexed restarts; admission samples
resident inventories once per credit refill, amortized across 64 KiB. Each graph
payload adds a size inspection before its admitted clone. There are no new WAL
records, disk formats, fsyncs or network round trips. Sorting uses an in-place
unstable sort with the same explicit ordering keys, avoiding sort scratch space.

Credits are conservative over the query lifetime: temporary allocations are not
individually refunded. A long expansion can therefore exhaust its allowance
before its actual peak resident memory would exhaust the budget. This is a first
admission boundary, not a precise peak-memory allocator or a hard process RSS cap.
Degree still determines preparation work; a resumable bounded adjacency cursor
and earlier top-k selection remain separate performance work.

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
