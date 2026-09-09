# Conservative graph segment pruning

Runtime graph materialization now skips segments that cannot contain the kind
requested by its current node or edge pass. This applies to MATCH/path runtime
materialization, including CALL and nested execution through that helper. It
does not optimize every native graph analytics API.

## Correctness boundary

Each in-memory segment carries one byte with two physical-presence bits. Single
insertion, bulk type runs, consolidation adoption and in-place replacement
observe incoming graph kinds before they can become visible. Bits only grow:
deleting or replacing the last node does not clear its bit. `get_mut` sets both
bits before returning unrestricted mutable access. Failed mutations may leave
false positives. Reconstructed/consolidated segments rebuild presence from
physical items; the mask has no persisted encoding or format version.

The mask is separate from `kind_index` and `SegmentStats`. Those structures are
not safe pruning authorities across all mutable/flat-storage paths. A zero bit
proves absence; a set bit only means that scanning may be necessary. The
summary check and scan share the same segment read guard. Explicit captured
MVCC visibility still precedes entity filtering, and historical physical
versions contribute presence regardless of their current visibility.

Ordinary reads retain the existing parallel sealed-segment scan strategy. CALL
stays sequential, counts each segment consulted (even when skipped), counts
visible candidates in unpruned segments, and counts fetched graph items. Budget
exhaustion stops the scan and returns an error. RLS remains outside segment
locks, and batch fetches still recheck the captured snapshot before RLS.

## Executable checks

- A graph-only CALL passes a 512-unit work limit. Adding 1,024 unrelated vectors
  in another segment used to exhaust that same limit. The regression covers
  growing/sealed vector segments, both within the graph collection and in a
  separate collection, and checks identical MATCH/CALL results.
- The existing mixed-segment test still rejects a CALL whose actual candidate
  scan exceeds its budget. A unit test rejects three skipped segments under a
  two-unit limit, so pruning cannot bypass segment work accounting.
- Segment tests exercise bulk ID gaps, single inserts, adoption, deletion,
  replacement, hot/forced sealed updates and mutable access, in flat and
  HashMap storage.
- A fresh-thread snapshot fixture compares pruned reads and callback results
  with the full scan, checks old/new physical IDs explicitly, counts visited
  candidates and checks early termination. Node passes inspect three visible
  candidates; edge passes inspect two, excluding the 128-vector segment.
- A persistent runtime test checks MATCH results before/after clean reopen and
  after rolling back a versioned update to a sealed graph. Versioning is enabled
  explicitly; SELECT checks read-own-write before rollback. This is a clean
  recovery regression, not a power-loss or crash-injection test.

```sh
cargo test --locked -p reddb-io-server --lib storage::unified:: -- --test-threads=1
cargo test --locked -p reddb-io-server --lib runtime::graph_tvf::pruning_tests -- --test-threads=1
cargo test --locked --test grouped_graph_topology -- --test-threads=1
```

## Cost and limits

With S segments, N physical candidates before pruning, and C candidates in
segments that may contain the requested kind, each pass costs O(S + C) instead
of O(S + N). The byte per segment adds no per-item heap allocation or storage
I/O; there are no new writes, fsyncs or network calls. Bulk observes each type
run once. The ordinary parallel path still creates its existing workers for
sealed segments, including those whose scan is skipped.

CALL work is approximately segments consulted + visible candidates inspected +
graph payloads fetched across both passes, plus pattern/statement/return work.
The counter measures plan work, not CPU instructions or RSS. ID arrays and
materialized graph memory remain unbounded by a peak-memory quota in this slice.

A segment that once held graph items, or allowed unrestricted mutation, may
remain unprunable until reconstruction. Intermixed graph/vector items still
require a candidate scan. Stable snapshot cursors, projection pushdown and
controlled optimized benchmarks remain separate work. Reduced inspected work
does not establish SQLite, PostgreSQL or SurrealDB throughput parity.

## Transaction composition gaps found during validation

The initial fixture assumed that a non-versioned graph UPDATE would be undone
by ROLLBACK. A raw storage probe showed the changed value still present: the
existing `apply_loaded_patch_entity_core`/`persist_applied_entity_mutations`
path uses in-place last-writer-wins for non-versioned graphs. With
`VcsUseCases::set_versioned("mixed_graph", true)`, rollback restores the node.
Those mutation paths are unchanged by this PR.

A second probe found a separate logical-identity gap: after a versioned node
UPDATE inside BEGIN, SELECT sees the changed name, but MATCH across its existing
edge returns no row. After ROLLBACK, MATCH returns the original path again.
Materialization still keys nodes by physical entity ID; physical versions have
new IDs while edges retain their endpoint references. This PR does not fix
logical identity resolution, change default transaction semantics or claim
MATCH read-own-write correctness across versioned graph updates. That is a
priority follow-up, including edges created before and after several versions,
commit/rollback/savepoints, RLS and historical snapshots.
