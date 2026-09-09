# Graph materialization: filter kinds before copying

Local diagnostic, 2026-09-08. Before: `db9905fe2eb4cf996edce5c6be941a902197a0b8`.
Engine change: `e39684c6e86aad7bb9504d302c647beb55a78226` (stacked on PR #2300).

A two-node, one-edge MATCH over a mixed collection copied every visible entity
in both materialization passes, including vectors that never participated in the
graph. Filtering actual entity kinds before cloning removes those copies. CALL
also collects only matching IDs before its batch fetch; every visible candidate
inspected still consumes scan work, including rejected kinds. RLS evaluation
stays outside segment locks and batch fetches recheck the captured MVCC snapshot.

## Results

These are **requested allocation bytes**, summed across alloc/alloc_zeroed/realloc
calls, not peak live heap, process RSS or a memory cap. The fixture with 64 vectors
allocates about **98.4% fewer bytes for MATCH** and **98.3% fewer for CALL**, with
identical query results. The largest individual allocation drops from 65,536 to
16,384 bytes in those cases.

Timing columns are the median of seven per-run means (100 queries per run), not
individual-query p50/p95/p99. Engine libraries and probes are **unoptimized**,
allocation tracking is enabled, and the host is shared. Timings are diagnostic;
they do not establish production throughput or satisfy competitive acceptance.

| Vectors | Segment | Query | Bytes/query before | Bytes/query after | Mean ms/query before (median run) | Mean ms/query after (median run) |
|---|---|---|---:|---:|---:|---:|
| 0 | growing | MATCH | 353,580 | 143,400 | 2.27 | 1.79 |
| 0 | growing | CALL | 287,412 | 145,472 | 3.86 | 3.06 |
| 0 | sealed | MATCH | 353,580 | 143,400 | 2.41 | 2.04 |
| 0 | sealed | CALL | 287,412 | 145,472 | 3.85 | 3.05 |
| 64 | growing | MATCH | 8,874,540 | 143,400 | 6.73 | 2.55 |
| 64 | growing | CALL | 8,716,212 | 145,472 | 9.61 | 3.81 |
| 64 | sealed | MATCH | 8,874,540 | 143,400 | 8.67 | 2.94 |
| 64 | sealed | CALL | 8,716,212 | 145,472 | 10.59 | 4.49 |

The paired before/after timing ratios for the mixed fixture ranged from 1.73 to
3.91. One graph-only control pair was slower after the change (ratio 0.82); no
samples were removed. Host load averages were approximately 6.8 at the start and
6.9 at the end on eight logical CPUs. The allocation regression is the stable
acceptance signal; optimized, controlled-load timing remains pending.

## Method and reproduction

- One in-memory runtime per repetition: two named nodes and a directed edge in
  `mixed_graph`, with either zero or 64 unrelated vectors in the same collection.
  Each vector has 16,384 f32 elements (64 KiB); vector ingestion uses real IDs in
  the unified store as fixture setup. Public MATCH/CALL reads are the measured API.
- Growing and single-sealed-segment cases. At most one sealed segment ensures
  the entire scan is observed by the calling thread's allocation counter; this
  diagnostic does not measure worker-thread allocations from parallel scans.
- `runtime.result_cache.enabled = false`; ten warmup queries precede each timed
  run. The query returns only `Bob`; every query asserts that result.
- Seven paired runs for each of eight cases, alternating executable order.
  Each run executes 100 queries. All 112 runs / 11,200 measured queries succeeded.
  Setup, warmup and process startup are outside the timer. No concurrent build
  or local test campaign was started during the measurements.
- [Raw samples](graph-materialization-filter-samples.jsonl) retain elapsed time,
  allocation requests, order/repetition, workload and host load for every run.
  The recorded commands name the preserved local probe binaries. The probe uses
  the same fixture and `measured_query` helper as the regression below and times
  a loop of 100 calls after warmup; it is not a release benchmark harness.

The checked-in [regression test](../../tests/grouped/graph_analytics/e2e_graph_materialization_allocations.rs)
asserts results and rejects any copied 64-KiB vector payload, with a 512-KiB bound
on total requests for each fixture query. It failed on the parent for growing
and sealed MATCH/CALL and passed after the change. A second test checks that CALL
still fails on a tight budget for non-graph candidates, then ordinary MATCH works.

```sh
cargo test --locked --test grouped_graph_topology e2e_graph_materialization_allocations -- --test-threads=1 --nocapture
```

To reproduce the parent failure, apply only the regression test and its harness
registration to an isolated checkout of the before revision, then run the same
command filtered to `graph_reads_do_not_copy_vector_payloads_from_mixed_collections`.

## Cost and remaining work

No network round trip, write, fsync or storage-format change is added. With N
visible entities and G graph entities, both O(N) passes remain, but only G
payloads are copied instead of 2N. In CALL, temporary IDs are O(graph candidates)
instead of O(all candidates), and accounting is approximately 2N + G units before
pattern expansion. Ordinary queries retain the existing parallel scan path.

This covers runtime graph-pattern materialization, including nested execution
and CALL. It does not claim optimization of every native graph analytics API.
Collection/segment pruning, projection pushdown, a stable snapshot cursor and
peak-memory enforcement remain separate work. The comparison against SQLite,
PostgreSQL and SurrealDB still requires the program's pinned, equivalent workloads.
