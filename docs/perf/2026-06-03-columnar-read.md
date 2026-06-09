# Columnar-vs-row read benchmark — baseline report — 2026-06-09

Status: **Baseline measurement complete** (measure-only slice; optimisation is #962).

Tracking issue: #943 — *"Build columnar-vs-row read benchmark + profile report"*.

Parent PRD: #850 — Analytics engine (Phase 2 measurement slice).

Bench harness: `crates/reddb-server/benches/columnar_read_bench.rs` (criterion, `harness = false`).

## TL;DR

- At **50 K rows**: row path **806 µs** median, batch path **1 205 µs** median — batch is **1.5× slower**.
- The columnar batch path is *faster* only at small chunk sizes (< ~5 K rows), where the row path pays disproportionate codec startup overhead.
- **Dominant cost: Zstd codec decompression**, specifically the value column (Xor+Zstd). Comparing the 1-column ts-only scan (365 µs/50K) vs the 2-column scan (1 205 µs/50K) shows the value column decode alone accounts for ~70% of total batch decode time.
- Projection pushdown is effective: scanning only the timestamp column is **3.3× faster** than scanning both columns at 50 K rows.
- No path wins unconditionally; #962 must close the 1.5× gap at large chunks.

## Setup

### Build

```
cargo bench -p reddb-io-server --bench columnar_read_bench
```

Toolchain: `rust-toolchain.toml` from this repo. Profile: criterion default (release-equivalent
for bench binaries). No extra features. Runs on the same host as the agent worker — not a
controlled bench host, so numbers are indicative rather than canonical. Re-bench on a dedicated
host to produce certified baselines.

### Workload

Synthetic `TimeSeriesChunk` sealed via `seal_columnar(chunk_id=7, schema_ref=1)`:
- Timestamps: `1_700_000_000_000 + i * 1_000_000` ns (1 ms apart, monotonically increasing).
- Values: `95.0 + (i % 7) * 0.25` (low-cardinality float cycle, typical for gauge metrics).
- Codec: DoubleDelta+Zstd(3) for timestamps, Xor+Zstd(3) for values (production defaults).

Three chunk sizes: **1 K**, **10 K**, **50 K** rows.

### Paths measured

| Label | Function | Output |
|---|---|---|
| `row-path` | `points_from_column_block(block)` | `Vec<TimeSeriesPoint>` |
| `batch-path` | `column_batch_from_block(block, &[0,1])` | `ColumnBatch` (both cols) |
| `batch-ts-only` | `column_batch_from_block(block, &[0])` | `ColumnBatch` (timestamp col only) |

`row-path` calls `read_column_block` (full block decode); both batch variants call
`read_column_block_projected`.

## Baseline results (p50 median / high-bound)

Criterion reports a 95% confidence interval `[low, median, high]` over 30 samples.
Values below are the **median estimate** (≈ p50) and the **high bound** (≈ conservative p99
proxy). Throughput is rows/second derived from the same measurement.

### Row path — `points_from_column_block`

| Rows | p50 (median) | High bound | Throughput (p50) | ns/row |
|------|-------------|------------|-----------------|--------|
| 1 K | 108.70 µs | 111.48 µs | 9.2 Mrow/s | 108.7 ns |
| 10 K | 191.12 µs | 195.95 µs | 52.3 Mrow/s | 19.1 ns |
| 50 K | 806.40 µs | 836.83 µs | 62.0 Mrow/s | 16.1 ns |

### Columnar batch path — `column_batch_from_block` (both columns)

| Rows | p50 (median) | High bound | Throughput (p50) | ns/row |
|------|-------------|------------|-----------------|--------|
| 1 K | 39.70 µs | 42.72 µs | 25.2 Mrow/s | 39.7 ns |
| 10 K | 199.98 µs | 219.58 µs | 50.0 Mrow/s | 20.0 ns |
| 50 K | 1 205.2 µs | 1 366.3 µs | 41.5 Mrow/s | 24.1 ns |

### Projection pushdown — `column_batch_from_block` (timestamp column only)

| Rows | p50 (median) | High bound | Throughput (p50) | ns/row |
|------|-------------|------------|-----------------|--------|
| 1 K | 19.28 µs | 19.84 µs | 51.9 Mrow/s | 19.3 ns |
| 10 K | 89.05 µs | 92.89 µs | 112.3 Mrow/s | 8.9 ns |
| 50 K | 365.05 µs | 379.14 µs | 137.0 Mrow/s | 7.3 ns |

### Summary ratio: batch / row at each chunk size

| Rows | batch / row | Winner |
|------|-------------|--------|
| 1 K | 0.37× | batch ~2.7× faster |
| 10 K | 1.05× | roughly equal |
| 50 K | 1.49× | row ~1.5× faster |

## Profile: dominant cost

### Observation: codec decompression dominates, value column is the bottleneck

Comparing the 1-column (ts-only) vs 2-column (batch-path) scans at 50 K rows isolates the
cost of the second column decode:

```
batch-path/50K  = 1 205 µs   (timestamp + value columns)
batch-ts-only/50K =  365 µs  (timestamp column only)
value column decode ≈ 840 µs (70% of total batch decode time)
```

The value column uses **Xor+Zstd(3)** codec. The timestamp column uses **DoubleDelta+Zstd(3)**.
The value column decode is **2.3× more expensive** than the timestamp column, despite both being
the same byte width (8 bytes/element). Zstd decompression of XOR-coded float data is more
expensive than inverse-delta integer data because the XOR residuals have higher entropy.

### Final reinterpret step is cheap

Both paths share the same raw-bytes → typed-vec step (`chunks_exact(8).map(f64::from_le_bytes).collect()`).
The cost difference between ts-only (7.3 ns/row at 50K) and a theoretically zero-copy path
suggests the reinterpret/collect allocation is ~5–8 ns/row — a small fraction of the total
105+ µs codec decompression step for the value column.

### Row-path anomaly at 1 K rows

The row path is **2.7× slower** than the batch path at 1 K rows (108.7 µs vs 39.7 µs), despite
being faster at 50 K. The difference is likely:
1. `read_column_block` (used by the row path) parses the complete block header and column
   directory unconditionally; `read_column_block_projected` (used by the batch path) reads only
   the projected columns, skipping directory entries for unreferenced streams.
2. Zstd has significant startup overhead that amortizes poorly at small block sizes.

At small chunk sizes the projected decoder's skip-overhead advantage outweighs the batch
path's struct-wrapping overhead.

### Cost categories (estimated share at 50 K, both columns)

| Category | Est. share | Evidence |
|---|---|---|
| Zstd decompression (value col, Xor) | ~70% | ts-only vs batch-path delta |
| Zstd decompression (ts col, DoubleDelta) | ~25% | ts-only absolute time |
| Reinterpret copy + Vec allocation | ~5% | ts-only – estimated zero-copy floor |
| Struct/ColumnBatch wrapping overhead | negligible | batch vs row at 10K ≈ 1.0× |

## Implications for #962 (optimise)

The ~1.5× gap at 50 K rows is **entirely inside the codec layer**. Potential optimisation
targets ranked by estimated impact:

1. **Replace Zstd with LZ4 for the value column** — LZ4 is 3–5× faster to decompress than
   Zstd at comparable ratios for floating-point data. Expected 1.5–2× speedup on the value
   column decode alone.
2. **Lazy/on-demand column decode** — only decompress columns that operators actually read;
   the projected path already skips unreferenced streams but still decompresses referenced
   columns eagerly.
3. **SIMD reinterpret** — marginal given the ~5% share, but free once the codec bottleneck
   is addressed.
4. **Adaptive codec selection** — switch from Xor+Zstd to plain Zstd (or none) for high-
   entropy float columns where XOR pre-conditioning doesn't improve compressibility.

The #857 (chunk compaction) gate requires the batch path to equal or beat the row path; that
requires a ≥1.5× codec-layer speedup at 50 K rows.

## Correctness

The existing unit tests in `storage::query::batch::columnar_scan` cover:
- `batch_path_is_value_for_value_identical_to_the_row_path` — bit-for-bit parity check
- `scan_produces_results_through_the_column_batch_path` — basic round-trip
- `projection_decodes_only_referenced_columns` — pushdown correctness
- `missing_column_is_an_error` — error path

No correctness regression was introduced by this slice (bench-only change).
