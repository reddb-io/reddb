# Columnar Batch Execution

RedDB runs queries through one of two engines:

1. **Volcano iterators** — pull-based, row-at-a-time. The original
   engine; used for complex query shapes (recursive CTEs, RLS policy
   chains, graph traversal, hybrid retrieval). Strength: handles
   anything. Weakness: each row is a dispatch + allocation.
2. **Columnar batch path** — typed [`ColumnBatch`] of up to 2048
   rows processed by tight operators the compiler auto-vectorises.
   Used for analytic workloads where the shape is simple (filter →
   project → group-by → reduce). Strength: 5–15× faster on large
   scans; SIMD-ready. Weakness: requires static schema and
   numeric-heavy work.

The planner picks which path to use at plan time based on query
shape + table size. Both read the same storage.

## `ColumnBatch`

A batch is:

```
ColumnBatch {
  schema:   Arc<Schema>,       // shared metadata
  columns:  Vec<ColumnVector>, // typed column storage
  len:      usize,             // row count (≤ BATCH_SIZE)
}
```

`ColumnVector` variants: `Int64`, `Float64`, `Bool`, `Text`, each
with an optional validity bitmap for nullable columns. `BATCH_SIZE`
is 2048 — fits f64 columns comfortably in L2 cache.

## Operators

| Operator         | API                                         | Path |
|------------------|---------------------------------------------|------|
| Filter           | `batch_filter(&batch, predicate)`           | scalar (B2 adds SIMD) |
| Project          | `batch_project(&batch, &[col_idx])`         | slice / clone |
| Aggregate        | `batch_aggregate(&batch, &group_cols, &specs)` | HashMap group-by |
| Parallel aggregate | `parallel_aggregate(&batches, ...)`       | rayon work-stealing |

Operators are plain functions rather than a trait today — keeps the
surface small until the planner wires the batch path through a
proper `BatchPlan` node.

## SIMD

Runtime CPU-feature detection picks the widest available ISA per
reducer:

* `sum_f64` / `sum_i64`
* `min_f64` / `max_f64`
* `filter_gt_f64`

AVX2 path: four f64 (or four i64) per lane, scalar tail. Falls back
to scalar on CPUs without AVX2 and on non-x86 architectures. All
reducers have a `*_scalar` sibling the tests use as ground truth.

See [`src/storage/query/batch/simd.rs`](../../../src/storage/query/batch/simd.rs).

## Codecs

Column segments can be compressed with per-column codecs matching
ClickHouse's `CODEC(...)` syntax. Supported:

* `None` — raw bytes
* `Lz4` — fast generic
* `Zstd(level)` — higher-ratio generic
* `Delta` — monotonic int streams (shares the TS Delta-of-Delta code)
* `DoubleDelta` — regular-interval timestamps
* `Dict` — low-cardinality strings

Codecs chain: `CODEC(Delta, ZSTD(3))` encodes Delta first, then
zstd. Decoding reverses the order. The header records the full
chain, so reads never need the DDL.

## Parallelism

`rayon` powers the parallel paths. `parallel_sum_f64` splits the
input per-thread, calls SIMD `sum_f64` on each chunk, then sums the
partials. `parallel_aggregate` runs `batch_aggregate` per batch on a
worker and merges partial group-by state sequentially (sort + fold
at the end).

Parallelism is gated by a length threshold (default 4096) to avoid
work-stealing overhead dominating short scans.

## When the engine uses batch vs row path

The picker, planned for the B5/B6 sprint:

| Query shape                                     | Path |
|-------------------------------------------------|------|
| Recursive CTE / RLS / graph traversal / hybrid  | Volcano |
| Simple `SELECT agg(...) FROM t WHERE ... GROUP BY ...` | Batch |
| Aggregate that can read from a matching projection    | Projection → Batch |
| Hypertable scan filtered by time column               | Pruned Batch |

## Benchmarks

Published in [`docs/perf/olap-benchmarks.md`](../perf/olap-benchmarks.md)
as each sprint lands. Rough current state (micro-benchmarks on
synthetic data):

* `sum_f64` — SIMD/scalar ratio ~4× on AVX2 hardware for 10k f64s.
* `batch_aggregate` on 1M rows grouped by a single TEXT column:
  150ms single-thread, 45ms with 4 rayon workers.

Keep in mind these are unit-level measurements; end-to-end figures
require the planner to choose the batch path — tracked in B5 + B6.
