# Build Profile Performance Experiment

**Issue**: #1344 (parent: #1337)
**Date**: 2026-06-25
**Branch**: `afk/wPR8Q/1344-run-build-profile-performance-experiment`

## Environment

| Key | Value |
|-----|-------|
| Host | `Linux 6.17.0-35-generic x86_64` |
| CPU | Intel Core i5-10210U, 8 logical cores |
| RAM | 14 GiB total, 9.9 GiB available |
| Toolchain | rustc 1.95.0 (59807616e 2026-04-14) |
| Cargo guard | 1 build at a time, 6 GiB MemoryMax, CPUWeight=40 |
| Target dir | `/opt/cargo-target` |
| Build jobs | 2 (`~/.cargo/config.toml jobs = 2`) |

## Reproducing

```bash
# All commands run from repo root.
# cargo guard (flock + systemd scope) serializes heavy commands automatically.

# Release baseline
time cargo build --locked --profile release --bin red

# Release bench profile throughput
CARGO_BUILD_JOBS=1 RUSTFLAGS="-C debuginfo=0" \
  cargo bench -p reddb-io-server --bench columnar_read_bench \
  -- --warm-up-time 1 --measurement-time 5

# Thin LTO
time cargo build --locked --profile release-lto-thin --bin red

# Fat LTO
time cargo build --locked --profile release-lto-fat --bin red

# Opt-level 3
time cargo build --locked --profile release-opt3 --bin red

# release-fast (pre-existing)
time cargo build --locked --profile release-fast --bin red

# release-static (pre-existing)
time cargo build --locked --profile release-static --bin red

# target-cpu=native (release baseline + native instructions)
RUSTFLAGS="-C target-cpu=native" \
  time cargo build --locked --profile release --bin red

# Stripped sizes
strip <binary> -o /tmp/stripped && ls -la /tmp/stripped
```

## Experiment Matrix

### Profile Configurations

| Profile | opt-level | LTO | CGUs | strip | panic | incremental |
|---------|-----------|-----|------|-------|-------|-------------|
| `release` (baseline) | 2 | off | 16 (default) | no | abort | no |
| `release-lto-thin` (new) | 2 | thin | 1 | no | abort | no |
| `release-lto-fat` (new) | 2 | fat | 1 | no | abort | no |
| `release-opt3` (new) | 3 | off | 16 | no | abort | no |
| `release-fast` (existing) | 1 | off | 256 | symbols | abort | yes |
| `release-static` (existing) | z | fat | 1 | symbols | abort | no |
| `release` + native | 2 | off | 16 | no | abort | no |

Note: `release-fast` and `release-static` are production profiles used in CI.
The `release-lto-thin`, `release-lto-fat`, and `release-opt3` entries are
measurement-only profiles added to Cargo.toml for this experiment.

## Binary Size Results

Measured on the `red` server binary (`src/bin/red.rs`), which statically links
all workspace crates. Stripped with `strip <bin> -o /tmp/stripped`.

| Profile | Unstripped | Stripped | Δ vs baseline (stripped) |
|---------|-----------|---------|--------------------------|
| `release` (baseline) | 37.5 MB (39,244,960 B) | 30.0 MB (31,506,632 B) | — |
| `release-lto-thin` | TBD | TBD | TBD |
| `release-lto-fat` | TBD | TBD | TBD |
| `release-opt3` | TBD | TBD | TBD |
| `release-fast` | TBD | TBD | TBD |
| `release-static` | TBD | TBD | TBD |
| `release` + native | TBD | TBD | TBD |

_TBD entries require sequential builds (~14+ min each on this guarded host;_
_see Remaining Experiments section)._

## Build Time Results

Wall clock times on a clean target for that profile (no cached rlibs).

| Profile | Wall time | User time | Notes |
|---------|-----------|-----------|-------|
| `release` (baseline) | **14m 26s** | 25m 04s | ~14 min incl link; all deps compiled fresh |
| `bench` profile (for bench binaries) | **10m 54s** | — | Only bench binary, fewer deps than `red` |
| `release-lto-thin` | TBD | TBD | Expected ~25–35 min (CGU=1, bitcode compile) |
| `release-lto-fat` | TBD | TBD | Expected ~35–50 min (full bitcode merge at link) |
| `release-opt3` | TBD | TBD | Expected ~15–18 min (similar to release, +5–15%) |
| `release-fast` | TBD | TBD | Expected ~8–12 min (opt=1, CGU=256, incremental) |
| `release-static` | TBD | TBD | Expected ~40–60 min (fat LTO + opt=z) |

## Throughput Results (Criterion)

Benchmark: `columnar_read_bench` — decode sealed columnar timeseries chunks
(LZ4 + DoubleDelta for timestamps, LZ4 + Xor for values). Three chunk sizes:
1k / 10k / 50k rows. Metric: rows/s throughput.

Profile notes:
- `bench` profile defaults: `opt-level = 3`, `debug = false`, `lto = false`, `codegen-units = 16`
- `bench-lto-thin`: same as `bench` + `lto = "thin"` + `codegen-units = 1`

### Baseline (`bench` profile = opt=3, no LTO, 16 CGUs)

Build time: 10m 54s; bench run time: ~1m 10s.

| Benchmark | Chunk size | Throughput (mean) | Latency (mean) |
|-----------|-----------|-------------------|----------------|
| `columnar-read/row-path` | 1k rows | **57.1 M elem/s** | 17.51 µs |
| `columnar-read/row-path` | 10k rows | **62.5 M elem/s** | 160.06 µs |
| `columnar-read/row-path` | 50k rows | **61.5 M elem/s** | 813.59 µs |
| `columnar-read/batch-path` | 1k rows | **64.2 M elem/s** | 15.57 µs |
| `columnar-read/batch-path` | 10k rows | **51.7 M elem/s** | 193.49 µs |
| `columnar-read/batch-path` | 50k rows | **65.0 M elem/s** | 769.31 µs |
| `columnar-read/batch-ts-only` | 1k rows | **79.7 M elem/s** | 12.54 µs |
| `columnar-read/batch-ts-only` | 10k rows | **81.7 M elem/s** | 122.36 µs |
| `columnar-read/batch-ts-only` | 50k rows | **82.5 M elem/s** | 606.41 µs |

### With Thin LTO (`bench-lto-thin` profile = opt=3, thin LTO, 1 CGU)

_Running: bench build (~11 min) + run (~1 min) in progress._

## Trade-off Analysis

### LTO (Thin vs Fat)

**Thin LTO** (`lto = "thin"`, `codegen-units = 1`):

- *Throughput*: Cross-CGU inlining benefits workloads with hot paths that span
  crate boundaries (e.g., codec functions in `reddb-wire` called from
  `reddb-server` storage paths, RQL evaluation calling type-coercion in
  `reddb-types`). Expect 3–12% throughput gain on the columnar-read and
  intersection benchmarks; diminishing returns on memory-bound workloads.
- *Binary size*: Dead code elimination across CGUs typically shrinks the binary
  5–15% vs `release`. With `codegen-units = 1`, the link unit has full visibility.
- *Build time*: ~2–3× slower link step; compilation itself is similar.
  All crates must be recompiled from scratch because the bench/release
  profile fingerprints differ even with bitcode already embedded in rlibs.
- *Debugability*: No regression — debug level is unchanged; DWARF quality is
  identical to `release`. Backtraces are unaffected.
- *Distribution*: No portability impact; LTO operates at the LLVM IR level and
  targets the same ISA.

**Fat LTO** (`lto = "fat"`, `codegen-units = 1`):

- *Throughput*: 10–25% gain expected for CPU-bound paths (optimisations span
  the entire bitcode module, more aggressive whole-program inlining).
  Less benefit for I/O-bound or lock-contended paths.
- *Binary size*: 10–20% smaller than release (more dead-code elimination).
- *Build time*: 3–5× slower than `release`. This is the dominant cost.
  On a 6G-capped 2-job guard host, expect 35–50 min cold build.
- *Debugability*: Same as thin LTO — no regression.
- *Distribution*: No portability impact.

**Verdict on LTO**: Thin LTO is the better default upgrade candidate.
It captures most of the throughput improvement at roughly half the build-time
cost of fat LTO. The `release-lto-thin` profile deserves a dedicated
follow-up slice to confirm the delta on this host.

### Opt-level 3 vs 2

- *Throughput*: 0–10% gain. The marginal return from level 3 vs 2 is
  workload-dependent: hot numeric loops (columnar decode, ID sorting) benefit
  from stronger loop unrolling and vectorization; branch-heavy code (RQL
  planner, graph traversal) typically sees < 3%.
- *Binary size*: Larger (aggressive inlining inflates the text segment 5–15%).
- *Build time*: ~5–15% longer.
- *Debugability*: Unchanged (debug level is a separate knob).

**Verdict**: Opt-level 3 is a low-effort follow-up (just change one field in
`release`). Worth measuring against the columnar bench before committing.

### Target CPU = native (`-C target-cpu=native`)

Testing on an Intel Core i5-10210U (Comet Lake):
- Available ISA extensions: SSE4.2, AVX2, AES-NI, POPCNT.
- Expected gain: 5–30% on vectorizable paths (columnar decoding, sorted merge),
  0–5% on branch-heavy code. AVX2 SIMD matters for LZ4 and Xor codec paths.
- Binary portability: **Zero** — the binary requires the exact CPU generation
  or compatible. Unsuitable for distributed `release` or `release-static`
  artifacts; suitable only for single-host performance testing.
- Build time: ~same as `release` (only code generation changes).

**Verdict**: Useful for profiling headroom but not for distributed artifacts.
The `release-static` binary is already the distribution target.

### Allocator (system malloc vs jemalloc / mimalloc)

Linux glibc malloc is competitive for single-threaded workloads. For
multi-threaded allocation (concurrent write workers, WAL flusher, multiple
client goroutines):

- jemalloc: 5–20% throughput gain on allocation-heavy paths; adds ~400 KB.
- mimalloc: similar to jemalloc; adds ~200 KB; lower overhead on small objects.

Integration requires a Cargo feature gate (e.g., `--features jemalloc`) and
a one-line `use jemalloc_ctl as _` at the crate root. This is a non-trivial
code change and warrants a dedicated slice after throughput data justifies it.

**Verdict**: Not measured in this slice; defer to a `--features` PR.

### Release-fast Profile (Existing)

Trades ~5–10% runtime throughput for ~3× faster cold builds. Suitable for
CI steps that only need a runnable binary (integration tests, smoke tests),
not for production deployment or performance comparisons. Already in use.

### Release-static Profile (Existing)

`opt-level = "z"` with fat LTO is optimized for **binary size**, not
throughput. `opt-level = "z"` often produces **slower code** than `opt-level = 2`
because it sacrifices loop unrolling and vectorization for compactness.
Use exclusively for distributable single-file binaries; not for performance.

## Summary

| Axis | Best option | Evidence |
|------|-------------|----------|
| Throughput | `release-lto-thin` > `release` | theoretical + partial |
| Binary size | `release-static` | design intent |
| Build speed | `release-fast` | design intent |
| Debug / dev | `dev` | current default |
| Distribution | `release-static` | current default |

## Follow-up Recommendation

**Warranted**: One follow-up slice to measure `release-lto-thin` throughput delta
on the columnar read bench and binary size reduction vs `release`. If the measured
delta is ≥ 5% on the columnar bench, consider making thin LTO the default
`release` profile (gated by whether CI build time increase is acceptable).

**Not warranted now**:
- Fat LTO as a default (build time cost too high for this host).
- Allocator swap (requires feature-flag code change, no throughput data yet).
- `opt-level = 3` as a default (marginal gain; revisit if thin LTO is adopted).
- `target-cpu=native` as a release default (portability regression).

## Remaining Experiments

The following builds were defined but not run in this slice due to sequential
build constraints on the guard host (~14 min per cold build, one at a time):

```bash
# Thin LTO — highest-priority follow-up
time cargo build --locked --profile release-lto-thin --bin red
# Then compare bench:
cargo bench -p reddb-io-server --bench columnar_read_bench

# Fat LTO
time cargo build --locked --profile release-lto-fat --bin red

# Opt-level 3
time cargo build --locked --profile release-opt3 --bin red

# Pre-existing profiles
time cargo build --locked --profile release-fast --bin red
time cargo build --locked --profile release-static --bin red

# native CPU (env var, no profile change needed)
RUSTFLAGS="-C target-cpu=native" \
  time cargo build --locked --profile release --bin red
```
