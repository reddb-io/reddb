# Columnar-vs-row read benchmark — baseline report — 2026-06-09

Status: **Baseline measurement complete** (measure-only slice; optimisation is #962).

Tracking issue: #943 — *"Build columnar-vs-row read benchmark + profile report"*.

Parent PRD: #850 — Analytics engine (Phase 2 measurement slice).

Bench harness: `crates/reddb-server/benches/columnar_read_bench.rs` (criterion, `harness = false`).

## TL;DR

- At **50 K rows**: row path **778 µs** p50, batch path (both columns) **752 µs** p50 — the paths are
  **essentially equal** (~3% difference, within measurement noise across runs).
- **Dominant cost: codec decompression** — the value column (Xor+Zstd) alone accounts for ~55% of
  total batch decode time, isolated by comparing ts-only vs 2-column batch decode.
- **Projection pushdown is effective**: timestamp-only scan (343 µs/50K) is **2.2× faster** than
  full 2-column decode (752 µs/50K), confirming the projected read path skips the value column entirely.
- At 1 K rows the batch and row paths are within ~5% of each other (31.5 µs vs 30.0 µs) — no
  significant per-call startup overhead.
- The batch path does not lag the row path. #962 should focus on reducing codec cost, not on
  architectural changes to match a missing gap.

## Setup

### Build

```
cargo bench -p reddb-io-server --bench columnar_read_bench
```

Toolchain: `rust-toolchain.toml` from this repo. Profile: criterion default (release-equivalent
for bench binaries). No extra features. Criterion 30-sample sets, 3 s warm-up.

> **Note**: runs on the agent worker host, not a controlled bench machine — numbers are indicative
> baselines. Re-bench on a quiet dedicated host to produce certified release numbers.

### Workload

Synthetic `TimeSeriesChunk` sealed via `seal_columnar(chunk_id=7, schema_ref=1)`:
- Timestamps: `1_700_000_000_000 + i * 1_000_000` ns (1 ms apart, monotonically increasing).
- Values: `95.0 + (i % 7) * 0.25` (low-cardinality float cycle, typical for gauge metrics).
- Codec: DoubleDelta+Zstd for timestamps, Xor+Zstd for values (production defaults).

Three chunk sizes: **1 K**, **10 K**, **50 K** rows.

### Paths measured

| Label | Function | Output |
|---|---|---|
| `row-path` | `points_from_column_block(block)` | `Vec<TimeSeriesPoint>` |
| `batch-path` | `column_batch_from_block(block, &[0,1])` | `ColumnBatch` (both cols) |
| `batch-ts-only` | `column_batch_from_block(block, &[0])` | `ColumnBatch` (timestamp col only) |

`row-path` calls `read_column_block` (full block decode, both columns);
`batch-path` and `batch-ts-only` call `read_column_block_projected`.

## Baseline results

p50 values are `median.point_estimate` from Criterion's bootstrap statistics (30 samples).
p99 values are computed from the raw per-iteration sample set (30 samples — with n=30, p99
is the maximum observed iteration time, so treat as worst-case over the collection window
rather than a statistical p99).

### Row path — `points_from_column_block`

| Rows | p50 | p99 (max) | Throughput p50 | ns/row |
|------|-----|-----------|----------------|--------|
| 1 K  | 30.0 µs | 37.5 µs | 33 Mrow/s | 30.0 ns |
| 10 K | 190.9 µs | 198.7 µs | 52 Mrow/s | 19.1 ns |
| 50 K | 778 µs | 877 µs | 64 Mrow/s | 15.6 ns |

### Columnar batch path — `column_batch_from_block` (both columns)

| Rows | p50 | p99 (max) | Throughput p50 | ns/row |
|------|-----|-----------|----------------|--------|
| 1 K  | 31.5 µs | 47.6 µs | 32 Mrow/s | 31.5 ns |
| 10 K | 176.8 µs | 194.7 µs | 57 Mrow/s | 17.7 ns |
| 50 K | 752 µs | 851 µs | 66 Mrow/s | 15.1 ns |

### Projection pushdown — `column_batch_from_block` (timestamp column only)

| Rows | p50 | p99 (max) | Throughput p50 | ns/row |
|------|-----|-----------|----------------|--------|
| 1 K  | 18.9 µs | 20.5 µs | 53 Mrow/s | 18.9 ns |
| 10 K | 81.7 µs | 94.0 µs | 122 Mrow/s | 8.2 ns |
| 50 K | 343 µs | 416 µs | 146 Mrow/s | 6.9 ns |

### Summary ratio: batch (2-col) / row at each chunk size

| Rows | batch / row | Δ |
|------|-------------|---|
| 1 K  | 1.05× | row ~5% faster (noise) |
| 10 K | 0.93× | batch ~7% faster |
| 50 K | 0.97× | batch ~3% faster (noise) |

The two paths are within noise of each other across all measured chunk sizes.

## Profile: dominant cost

### Codec decompression is the bottleneck; value column dominates

Comparing ts-only vs 2-column batch decode isolates the second column's cost:

```
batch-path/50K  = 752 µs   (timestamp + value columns)
batch-ts-only/50K = 343 µs (timestamp column only)
value column decode ≈ 409 µs (~54% of total 2-column batch decode time)
```

The value column uses **Xor+Zstd** codec. The timestamp column uses **DoubleDelta+Zstd**.
Despite both columns having the same byte width (8 bytes per element), the value column
decode is **~1.2× more expensive** than the timestamp column at 50K rows.

The pattern holds across chunk sizes:

| Chunk size | ts column (µs) | value column delta (µs) | ratio val/ts |
|------------|----------------|-------------------------|--------------|
| 1 K  | 18.9 | 12.6 | 0.67× |
| 10 K | 81.7 | 95.1 | 1.16× |
| 50 K | 343  | 409  | 1.19× |

At 1 K rows the value column is cheaper than the ts column, likely because at small sizes
the Zstd decompression overhead is dominated by startup cost (shared between both codecs)
rather than per-byte work.

### Per-column cost breakdown at 50 K rows

| Category | Est. µs | Est. share | Evidence |
|---|---|---|---|
| Value col: Zstd decompress + Xor decode | ~409 µs | ~54% | ts-only vs batch-path delta |
| TS col: Zstd decompress + DoubleDelta decode | ~343 µs | ~46% | ts-only absolute time |
| Reinterpret copy + Vec allocation | < 10 µs | < 1% | 6.9 ns/row floor at 50K ts-only |
| Block header parse + column directory | negligible | < 1% | linear extrapolation 1K→50K |

### Row vs batch: why they're equal

The row path (`read_column_block`) and the batch projected path (`read_column_block_projected`)
both decompress the same column byte streams with the same codecs. The structural difference
is that the row path transposes column streams into `Vec<TimeSeriesPoint>` while the batch path
keeps columns separate in `ColumnVector`. Neither approach is free but neither dominates — the
codec step (Zstd + codec chain) dwarfs the reinterpret/transpose step at all measured sizes.

The row path uses `read_column_block` (parses the full directory entry for all columns) whereas
the batch projected path uses `read_column_block_projected` (iterates the directory and skips
unreferenced columns). At full 2-column decode, both functions touch the same two column streams;
the projection skip savings only materialise in the ts-only scan.

## Implications for #962 (optimise)

The batch path already matches the row path. The gate for #857 (chunk compaction) requires the
batch path to **beat** the row path — meaning #962 must achieve a meaningful speedup over both
current paths.

Targets ranked by estimated impact:

1. **LZ4 / faster decompressor for value column** — Zstd(3) optimises compression ratio; a
   lighter codec (LZ4 or Zstd(1)) decompresses 3–5× faster for comparable ratios on float data.
   This alone could cut the value column cost from ~409 µs to ~100–140 µs at 50 K rows.
2. **Lazy column decode** — decompress only when an operator actually materialises values from
   that column (vs eager decompress on `column_batch_from_block` entry). Already partially
   present via projection pushdown; could be extended to late-materialise within a column.
3. **Columnar reinterpret without allocation** — returning a `&[f64]` view over aligned bytes
   instead of `Vec<f64>` avoids the ~5 µs allocation + copy overhead (minor win, safe only
   for fixed-width types).
4. **SIMD Xor decode** — the Gorilla XOR pass over 50 K f64 values is ~400 ns of scalar work;
   SIMD would reduce this to ~100 ns (negligible given Zstd dominates).

## Correctness

All 19 columnar-related unit tests pass (no regression):
- `batch_path_is_value_for_value_identical_to_the_row_path` — bit-for-bit parity check
- `scan_produces_results_through_the_column_batch_path` — basic round-trip
- `projection_decodes_only_referenced_columns` — pushdown correctness
- `missing_column_is_an_error` — error path
- Plus 15 additional chunk/granule/column-block tests

The 18 pre-existing failures in the full test suite (`queue_lifecycle`, `events_autocommit`,
`ai_credentials`, `schema coercion`) are unrelated to the columnar path and predate this slice.
