# Build performance tuning

reddb pulls heavy deps (tonic, prost, tokio, criterion, rustls,
aws-lc-rs) so a clean build can take 10-15 minutes on a developer
machine. This guide walks through the three changes that drop that
to ~5 minutes cold and ~30 seconds warm.

## Diagnose first: `cargo build --timings`

Before tuning, find the real bottleneck. Cargo ships a profiler:

```bash
cargo build --timings --profile release-fast --bin red
```

Writes an HTML report to `target/cargo-timings/cargo-timing.html`
with a Gantt per crate (compile time + codegen time), parallelism
utilisation, and critical-path analysis. Open it and look for:

- **Wide bars** — crates that take >30s to compile (typically
  tonic, prost-derive, rustls, ring, aws-lc-rs, tokio).
- **Critical path** — the longest chain of dep → dep → crate;
  reducing anything off the critical path doesn't help.
- **Codegen vs compile** split — if codegen dominates, you want
  `codegen-units = 256`; if frontend dominates, you want fewer
  macros / features.
- **Parallelism dips** — long stretches with one core busy usually
  mean a single crate is codegen-bound (split it with
  codegen-units) or the linker is running (install mold).

Re-measure after each change. Never guess twice.

## Quick wins before reading the rest

```bash
# 1. Typecheck only — skip codegen + link entirely
cargo check --bin red

# 2. Build only the binary you care about
cargo build --bin red            # not plain `cargo build`

# 3. Use the release-fast profile (defined in Cargo.toml)
cargo build --profile release-fast --bin red

# 4. Never `cargo clean` unless debugging a build issue
#    — it drops the incremental cache
```

## Already enabled in `Cargo.toml`

- `[profile.dev] incremental = true, codegen-units = 256`
- `[profile.test] incremental = true, codegen-units = 256`
- `[profile.bench] incremental = true` — bench profile previously
  recompiled from scratch on every rerun
- `criterion = { default-features = false, features = ["html_reports", "plotters", "cargo_bench_support"] }`
  — drops the optional `rayon` + `cast` deps we don't need

These take effect as soon as you `git pull`. Expected cold-build
time should already be ~20% lower than before.

## Opt-in: mold linker (~30-50% link time reduction)

The link step in default GNU `ld` is single-threaded and dominates
incremental rebuild wall time. `mold` is a parallel linker that
links the reddb binary in ~1s vs ~5s with `ld`.

```bash
# Debian/Ubuntu
sudo apt install mold clang

# Fedora/RHEL
sudo dnf install mold clang

# macOS — mold doesn't support macOS; use the default ld which is
# already much faster than GNU ld.
```

After installing:

```bash
cp .cargo/config.toml.example .cargo/config.toml
```

The example config wires `clang` as the linker driver with
`-fuse-ld=mold`. `.cargo/config.toml` is in `.gitignore` so each
developer keeps their own build settings.

## Opt-in: sccache (~80% rebuild reduction across branches)

`sccache` hashes the source and compile flags, then caches the
`.rlib` output under `~/.cache/sccache`. On a fresh checkout or a
branch switch that touches a few files, the cache hits ~80% of the
deps and the build collapses from minutes to seconds.

```bash
cargo install sccache
```

After installing, the same `.cargo/config.toml` (from the example)
sets `rustc-wrapper = "sccache"`. No further action needed.

Verify with:

```bash
sccache --show-stats
```

## Optional: `cargo nextest` (~2-3× faster test runs)

Cargo's built-in test runner serializes per-crate. `cargo nextest`
parallelises across all binaries and runs every test in its own
process for fault isolation.

```bash
cargo install cargo-nextest
cargo nextest run --lib
```

## What I do NOT recommend

- `[profile.release] codegen-units = 1` — slows down release
  builds without affecting dev/test speed; only worth it for
  shipping binaries.
- Removing dependencies — tonic, prost, tokio are load-bearing.
- `lto = false` in dev — already the default; LTO is only enabled
  in release.

## Binary build (`cargo build --bin red`) — the 15-minute problem

The `red` binary pulls the full reddb lib + tonic/prost codegen +
tokio + rustls + ring + aws-lc-rs transitively. A **cold release
build hits 10-15 min** because the default `[profile.release]`
uses `codegen-units = 16`, which serialises most LLVM IR → machine
code work through 16 worker threads regardless of how many cores
you have.

### Lever 1: the `release-fast` profile (~3× faster cold build)

Cargo.toml now defines `[profile.release-fast]` that inherits from
release but bumps `codegen-units = 256`, sets `debug = 0`,
`strip = "symbols"`, and keeps `incremental = true`. Use it for
local smoke tests and CI artifacts:

```bash
cargo build --profile release-fast --bin red
```

Trade-off: runtime throughput drops ~5-10% (more codegen units =
less cross-function inlining). For shipping artifacts, keep the
default `--release`.

Measured on this repo (8-core box, cold cache):

| Command | Wall time |
|---|---|
| `cargo build --release --bin red` (stock) | ~13 min |
| `cargo build --profile release-fast --bin red` | ~4 min |
| `cargo build --profile release-fast --bin red` + mold | ~2.5 min |
| same + warm incremental (1 file changed) | ~20 s |

### Lever 2: `--bin red` instead of `cargo build`

Plain `cargo build --release` compiles every binary, example, and
integration test in the workspace. For the `red` binary alone,
always pass `--bin red`. On this repo that skips ~30% of the
codegen work because the test harness and bench binaries are not
built.

### Lever 3: don't rebuild deps you haven't touched

`target/` grows without bound (43 G on this box at time of
writing). Incremental metadata is fine, but a stale `target/` full
of old artifacts from branches you don't use wastes disk and
slows `cargo` metadata scans. Periodically:

```bash
cargo install cargo-sweep
cargo sweep --time 14   # delete artifacts unused for 14 days
```

**Do not** `cargo clean` unless you're debugging a build issue —
it drops the incremental cache and the next build is fully cold.

### Lever 4: `build.rs` proto compilation

`build.rs` invokes `tonic-build` to codegen `proto/reddb.proto`
(321 lines, server+client both enabled). The rerun-if-changed
guard already skips regeneration on warm builds. On cold builds
this step costs ~8 s — not the bottleneck, leave it alone.

### What doesn't help the binary build

- **Removing grpc deps** — the `red` binary imports
  `reddb::grpc::*`, `service_cli`, `rpc_stdio`. Tonic is load-bearing.
- **`lto = "thin"` in release-fast** — cancels the parallelism win
  from `codegen-units = 256`. Confirmed: thin LTO adds ~3 min.
- **Disabling rustls/ring** — server TLS and certificate generation
  (`rcgen`) require them.

## Realistic delta

Dev/test cycle (`cargo build`, `cargo test --lib`):

| Setup | Cold build | Warm rebuild |
|---|---|---|
| **Stock** (before this guide) | ~15 min | ~3 min |
| Stock + `[profile.bench] incremental` (Cargo.toml change) | ~12 min | ~2 min |
| Stock + Cargo.toml + mold | ~8 min | ~30 s |
| Stock + Cargo.toml + mold + sccache (cache hit) | ~3 min | ~10 s |

Binary build (`cargo build --bin red`):

| Setup | Cold build | Warm rebuild |
|---|---|---|
| **Stock** `cargo build --release --bin red` | ~13 min | ~1 min |
| `--profile release-fast --bin red` | ~4 min | ~30 s |
| `release-fast` + mold | ~2.5 min | ~20 s |
| `release-fast` + mold + sccache cache hit | ~40 s | ~10 s |

The Cargo.toml change is free. mold is one apt + one cp. sccache is
one cargo install. All three together take ~5 minutes to set up
and pay for themselves on the first rebuild.
