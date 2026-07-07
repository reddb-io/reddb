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
* `Xor` — floating-point gauges (Gorilla XOR)

Codecs chain: `CODEC(Delta, ZSTD(3))` encodes Delta first, then
zstd. Decoding reverses the order. The header records the full
chain, so reads never need the DDL.

These codecs are the pipeline a sealed **columnar chunk** writes each
column with. The on-disk envelope that wraps the streams — directory,
granule index, bloom skip index, footer — is documented in
[Columnar Chunk Format (RDCC)](columnar-chunk-format.md).

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

## Columnar read decode

Reading a sealed columnar (RDCC) chunk back is now done through a typed,
zero-copy batch decode (committed in #962, lineage PRD #850). The decoder
(`column_batch_from_block`) decodes each numeric column straight into an
aligned `Vec<u64>` in one pass, then reinterprets that allocation in place as
`Vec<i64>` / `Vec<f64>` without copying. A row-shaped decode
(`points_from_column_block`) is also available for callers that want
`TimeSeriesPoint`s rather than a `ColumnBatch`.

Inside the runtime, the time-series read bridge dispatches on chunk format:
columnar-sealed chunks decode through the column-block range scan (granule
pruned), and row-sealed chunks fall back to the row materializer, so a
collection can mix both chunk formats and still read back correctly.

> **Activation status.** The typed columnar read-back is live for the
> time-series / hypertable read path on automatically projected chunks. The
> general SQL planner does **not** yet route arbitrary `SELECT` execution
> through the batch path — the batch-vs-row picker (table below) is still
> planned. Columnar storage is automatic for in-scope collections once chunks
> cross the configured size floor; use `NO COLUMNAR` to opt out. See
> [When to use columnar vs row](../data-models/timeseries.md#when-to-use-columnar-vs-row).

## Benchmarks

The committed columnar-vs-row read benchmark lives in
[`docs/perf/2026-06-03-columnar-read.md`](../perf/2026-06-03-columnar-read.md)
(harness: `crates/reddb-server/benches/columnar_read_bench.rs`). After the #962
optimization the batch decode path matches or beats the row path at every
measured chunk size (1 K / 10 K / 50 K rows): batch p50 and p99 are both ≤ the
row path, with the batch path ~2–3% faster at 10 K and 50 K rows.

> The absolute timings in that doc are only comparable **within a single bench
> run** — the batch/row *ratio* is the load-invariant signal, not the raw µs.

Other figures on this page (`sum_f64` SIMD ratios, `batch_aggregate`
throughput) are unit-level micro-benchmarks on synthetic data; treat them as
illustrative until end-to-end SQL routes through the batch path.
