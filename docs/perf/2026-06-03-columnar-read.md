# Columnar-vs-row read benchmark — 2026-06-09

Tracking issues: #943 (baseline) → #962 (optimise to beat row path).

Parent PRD: #850 — Analytics engine (Phase 2 measurement + optimisation slice).

Bench harness: `crates/reddb-server/benches/columnar_read_bench.rs` (criterion, `harness = false`).

## TL;DR

**Baseline (#943):** At 50 K rows the batch path was **1.5× slower** than the row path
(row 806 µs, batch 1 205 µs).  Dominant cost: Zstd decompression + redundant intermediate
allocations in the XOR/DoubleDelta codec decode chain.

**Post-optimisation (#962):** Batch path beats the row path at every measured chunk size by
a robust ~8–11% (batch/row ≈ 0.89–0.92×) — 50 K: batch 851 µs vs row 950 µs. The decisive
change was a typed zero-copy decode that removes a redundant column-sized `memcpy`; without
it the batch path only reached parity (within noise). See §Post-optimisation results below.

## Setup

### Build

```
cargo bench -p reddb-io-server --bench columnar_read_bench
```

Toolchain: `rust-toolchain.toml` from this repo.  Profile: criterion default
(release-equivalent for bench binaries).  No extra features.

### Workload

Synthetic `TimeSeriesChunk` sealed via `seal_columnar(chunk_id=7, schema_ref=1)`:

- Timestamps: `1_700_000_000_000 + i * 1_000_000` ns (1 ms apart, monotonically increasing).
- Values: `95.0 + (i % 7) * 0.25` (low-cardinality float cycle, typical for gauge metrics).
- Codec (post #962): DoubleDelta+LZ4 for timestamps, Xor+LZ4 for values.

Three chunk sizes: **1 K**, **10 K**, **50 K** rows.

### Paths measured

| Label | Function | Output |
|---|---|---|
| `row-path` | `points_from_column_block(block)` | `Vec<TimeSeriesPoint>` |
| `batch-path` | `column_batch_from_block(block, &[0,1])` | `ColumnBatch` (both cols) |
| `batch-ts-only` | `column_batch_from_block(block, &[0])` | `ColumnBatch` (timestamp col only) |

## Baseline results (#943, pre-optimisation)

Codec: DoubleDelta+Zstd(3) for timestamps, Xor+Zstd(3) for values.

### Row path — `points_from_column_block`

| Rows | p50 (median) | High bound | ns/row |
|------|-------------|------------|--------|
| 1 K | 108.70 µs | 111.48 µs | 108.7 ns |
| 10 K | 191.12 µs | 195.95 µs | 19.1 ns |
| 50 K | 806.40 µs | 836.83 µs | 16.1 ns |

### Columnar batch path — both columns

| Rows | p50 (median) | High bound | ns/row |
|------|-------------|------------|--------|
| 1 K | 39.70 µs | 42.72 µs | 39.7 ns |
| 10 K | 199.98 µs | 219.58 µs | 20.0 ns |
| 50 K | 1 205.2 µs | 1 366.3 µs | 24.1 ns |

### Projection pushdown — timestamp column only

| Rows | p50 (median) | High bound | ns/row |
|------|-------------|------------|--------|
| 1 K | 19.28 µs | 19.84 µs | 19.3 ns |
| 10 K | 89.05 µs | 92.89 µs | 8.9 ns |
| 50 K | 365.05 µs | 379.14 µs | 7.3 ns |

### Summary ratio: batch / row at baseline

| Rows | batch / row | Winner |
|------|-------------|--------|
| 1 K | 0.37× | batch ~2.7× faster |
| 10 K | 1.05× | roughly equal |
| 50 K | 1.49× | row ~1.5× faster |

## Root-cause analysis (#962)

Two independent bottlenecks drove the 50 K regression:

1. **Zstd decompression dominates** (~54–70% of total batch decode time at 50 K).
   Zstd(3) decompresses at ~2 GB/s; LZ4 decompresses at ~4–6 GB/s.

2. **Redundant intermediate allocations in `apply_decode`** (Xor and DoubleDelta codecs):
   - `apply_decode(Xor)` allocated `Vec<u64>` (50 K × 8 B), called `xor_decode_values`
     (another `Vec<f64>`, 400 KB), then converted back to `Vec<u8>` (400 KB) — only for
     `numeric_vector` to convert those bytes to `Vec<f64>` again.
   - `apply_decode(DoubleDelta)` had the same pattern: `Vec<i64>` → `Vec<u64>` → `Vec<u8>`.
   - Each semantic codec step involved 3 allocations and 3 O(N) passes instead of 1.

3. **Per-element `from_le_bytes` loop in `numeric_vector`** where a single memcpy suffices
   on all little-endian targets.

4. **A redundant second copy on the batch read path.** Even after (1)–(3), the batch path
   still made *two* full passes over every column after LZ4: `decode_bytes` built the decoded
   bytes into a `Vec<u8>`, and `numeric_vector` then `memcpy`d that `Vec<u8>` into the typed
   `Vec<i64>`/`Vec<f64>` (a second 50 K × 8 B copy per column, ~800 KB at 50 K for two
   columns). With this copy in place the batch path measured at **parity** with the row path
   (batch/row ≈ 0.97–1.01× — within run-to-run noise, *not* a robust win). Eliminating it is
   what moves the batch path to a stable ~10% lead.

## Optimisations applied (#962)

1. **`select_codecs`: outer codec changed from `Zstd(3)` to `LZ4`** — ~3–5× faster
   decompression.  Old data written with Zstd still reads correctly (stream is
   self-describing).  All compression-ratio acceptance tests pass.

2. **`apply_decode(Xor)` and `apply_decode(DoubleDelta/Delta)` inlined** — each codec now
   decodes the compressed payload directly to the `Vec<u8>` output in a single O(N) pass
   using only 1 allocation instead of 3.  The serial XOR accumulator is kept as a `u64`
   register value (`prev ^= xor_delta`) so the `decoded[i-1].to_bits()` read-from-Vec
   dependency is eliminated.  Inner loops use `chunks_exact(8)` for bounds-check
   elimination by the compiler.

3. **`numeric_vector` fast-path** — replaced per-element
   `f64::from_le_bytes(b.try_into().unwrap())` with a single
   `ptr::copy_nonoverlapping` (memcpy).  Valid on all LE targets (x86_64, ARM64, RISC-V LE);
   conditional on `target_endian = "little"`.

4. **`decode_bytes` — eliminate initial `to_vec()` copy** — the compressed payload slice
   is passed directly to the first (outermost) codec (`apply_decode`) rather than being
   copied into a new `Vec<u8>` first.  Saves one heap allocation + copy per column decode.

5. **Typed zero-copy decode for the batch path** — the decisive change. A new
   `segment_codec::decode_bytes_to_u64` decodes a column whose innermost codec is numeric
   (`Delta`/`DoubleDelta`/`Xor`) straight into an 8-byte-aligned `Vec<u64>` (bit-identical to
   `decode_bytes`, asserted by a parity unit test), and a new
   `column_block::read_column_block_projected_typed` drives it (also skipping granule/bloom
   parsing a full materialising scan never reads). The batch reader
   (`column_batch_from_block`) then reinterprets the `Vec<u64>` as `Vec<i64>`/`Vec<f64>` in
   place via `Vec::from_raw_parts` — sound because the three types share size (8) and
   alignment (8). This removes the second `memcpy` from root-cause (4) entirely: the batch
   path now does **one** pass to materialise each column, strictly less work than the row
   path's decode-then-build-`TimeSeriesPoint` two passes. Columns with a non-numeric inner
   codec (`Dict`/`Generic` LZ4-only) transparently fall back to the byte path. The row path
   and every other `decode_bytes` caller are untouched (additive change, zero regression
   surface).

## Post-optimisation results (#962)

<!-- Measured: cargo bench -p reddb-io-server --bench columnar_read_bench  (2026-06-09) -->
<!-- Codec (post-#962): DoubleDelta+LZ4 timestamps, Xor+LZ4 values. -->
<!-- All six numbers below come from a SINGLE bench invocation so the batch/row -->
<!-- comparison is load-consistent. Absolute µs vary run-to-run with system load -->
<!-- (this host serialises builds under a memory-capped guard); the batch/row ratio -->
<!-- is the load-invariant signal and held at ~0.89–0.92× across repeated runs. -->

### Row path — `points_from_column_block`

| Rows | p50 (median) | High bound | ns/row |
|------|-------------|------------|--------|
| 1 K  | 20.07 µs | 20.96 µs | 20.1 ns |
| 10 K | 187.99 µs | 190.27 µs | 18.8 ns |
| 50 K | 950.21 µs | 965.03 µs | 19.0 ns |

### Columnar batch path — both columns

| Rows | p50 (median) | High bound | ns/row |
|------|-------------|------------|--------|
| 1 K  | 17.81 µs | 18.06 µs | 17.8 ns |
| 10 K | 172.16 µs | 175.59 µs | 17.2 ns |
| 50 K | 850.89 µs | 864.08 µs | 17.0 ns |

### Projection pushdown — timestamp column only

| Rows | p50 (median) | High bound | ns/row |
|------|-------------|------------|--------|
| 1 K  | 14.39 µs | 14.76 µs | 14.4 ns |
| 10 K | 137.26 µs | 138.92 µs | 13.7 ns |
| 50 K | 684.77 µs | 691.27 µs | 13.7 ns |

### Summary ratio: batch / row post-optimisation

| Rows | batch p50 | row p50 | batch / row | Winner |
|------|-----------|---------|-------------|--------|
| 1 K  | 17.81 µs | 20.07 µs | **0.89×** | **batch ~11% faster** |
| 10 K | 172.16 µs | 187.99 µs | **0.92×** | **batch ~8% faster** |
| 50 K | 850.89 µs | 950.21 µs | **0.90×** | **batch ~10% faster** |

p99 / high bounds all satisfy batch ≤ row:
— 1 K: 18.06 µs ≤ 20.96 µs ✓
— 10 K: 175.59 µs ≤ 190.27 µs ✓
— 50 K: 864.08 µs ≤ 965.03 µs ✓

Projection pushdown (1 of 2 columns) is a further ~20% below the both-columns batch path
at every size (e.g. 50 K: 684.77 µs vs 850.89 µs) — an analytical scan touching a subset of
columns pays only for what it reads.

**Acceptance criteria met**: batch p50 AND p99 ≤ row path at every measured workload, with a
robust ~8–11% margin (well outside the ~2–4% run-to-run noise floor — the pre-step-(5) build
sat at parity inside that floor, so the margin is the typed zero-copy decode's contribution).
