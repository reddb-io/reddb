# Build performance tuning

reddb pulls heavy deps (tonic, prost, tokio, criterion, rustls,
aws-lc-rs) so a clean build can take 10-15 minutes on a developer
machine. This guide walks through the three changes that drop that
to ~5 minutes cold and ~30 seconds warm.

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

## Realistic delta

| Setup | Cold build | Warm rebuild |
|---|---|---|
| **Stock** (before this guide) | ~15 min | ~3 min |
| Stock + `[profile.bench] incremental` (Cargo.toml change) | ~12 min | ~2 min |
| Stock + Cargo.toml + mold | ~8 min | ~30 s |
| Stock + Cargo.toml + mold + sccache (cache hit) | ~3 min | ~10 s |

The Cargo.toml change is free. mold is one apt + one cp. sccache is
one cargo install. All three together take ~5 minutes to set up
and pay for themselves on the first rebuild.
