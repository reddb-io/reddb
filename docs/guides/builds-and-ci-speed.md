# Builds, Targets, and CI Speed

This guide explains how RedDB builds are organized, why some builds are fast and others are slow, and what is now accelerated both locally and in GitHub Actions.

## 1. Two meanings of "target"

There are two different "targets" in this repository.

Cargo targets inside the crate:

- library target
- binary targets
- test targets
- benchmark targets
- the build script target

Compilation target triples:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `armv7-unknown-linux-gnueabihf`
- `aarch64-unknown-linux-musl`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

When someone says "we have many targets", first confirm which one they mean.

To inspect the current Cargo targets in the root crate:

```bash
cargo metadata --no-deps --format-version 1
```

At the time this guide was written, the root crate exposed these Cargo target groups:

- 1 library
- 1 binary
- 19 integration tests, mostly grouped domain harnesses
- 3 benches

That is separate from the release matrix in GitHub Actions, which builds the same binary for multiple platform triples.

## 2. What each build command really does

The main commands in this repo are:

- `make build`: debug build through `scripts/cargo-fast.sh`
- `make warm`: prebuilds the common dev artifacts
- `make check`: fast compile check without full code generation
- `make test-fast`: runs the default local Rust test layer
- `make build-fast`: builds only the `red` binary with the `release-fast` profile
- `make release`: full optimized release build

The practical difference:

- `cargo build` does full codegen and linking for the selected targets
- `cargo check` stops before full codegen and linking, so it is much faster
- `cargo test` compiles test harnesses and test-only code, then runs tests
- `cargo bench` compiles benchmark targets in the bench profile
- `cargo build --profile release-fast --bin red` is the local "optimized but not fully expensive" path

## 3. Why builds get slow

The slow path is usually not parsing or type-checking. It is one of these:

- full LLVM optimization in release builds
- linker time on large binaries
- recompiling because the profile changed
- recompiling because `target/` was cleaned
- recompiling because a different `CARGO_TARGET_DIR` was used
- rebuilding benches or tests that were never prewarmed

The biggest build-speed rule in this repo is simple:

- do not destroy reuse unless you have a real reason

That means:

- avoid `cargo clean`
- avoid switching profiles unnecessarily
- avoid switching target directories unnecessarily
- keep using the same branch/session caches while you work

## 4. What is already optimized in Cargo.toml

This repository already enables incremental compilation where it matters for development:

- `profile.dev`: incremental on, high `codegen-units`
- `profile.test`: incremental on, high `codegen-units`
- `profile.release-fast`: incremental on, lower optimization cost, high `codegen-units`

`release-fast` exists for local smoke builds. It is intentionally not the shipping profile.

Use it when you want an optimized binary quickly:

```bash
make build-fast
```

Use `--release` only when you actually need the final optimized artifact:

```bash
make release
```

## 5. Local fast path

The repo now has a wrapper at [`scripts/cargo-fast.sh`](../../scripts/cargo-fast.sh).

The build/test/check/clippy commands in the Makefile and AFK validation use the same wrapper. It:

- selects the Rust version in `rust-toolchain.toml`, even when a host-wide `RUSTUP_TOOLCHAIN` points at a different version
- defaults to two Cargo jobs, adjustable through `CARGO_BUILD_JOBS` or Cargo's `--jobs`
- serializes participating build/test commands across worktrees for the same user when `flock` is available
- leaves incremental settings to the selected Cargo profile
- uses `sccache` automatically when `CARGO_INCREMENTAL=0`
- uses `mold` or `lld` automatically via `clang` if present

Use `REDDB_RUST_TOOLCHAIN=<version>` or an explicit `cargo +toolchain` argument through the wrapper for a deliberate toolchain experiment. `REDDB_CARGO_LOCK=0` disables the host lease when an external scheduler already admits builds. Long-lived `cargo run` servers receive the same jobs/toolchain/linker configuration but do not hold the build lease. On platforms without `flock`, only the job limit applies. Direct Cargo commands bypass the wrapper's toolchain selection and host lease.

The lease covers test execution as well as compilation, protecting the binaries that the test uses. Cargo's per-target lock alone does not bound independent worktree builds. The wrapper closes its lease descriptor in child processes so a subprocess cannot retain the lease after Cargo exits.

For direct `cargo` commands, you can also activate the local Cargo config:

```bash
cp .cargo/config.toml.example .cargo/config.toml
```

That file is local-only and gitignored. It gives plain `cargo` the linker setup and a two-job default. Direct Cargo still follows rustup's normal override rules and does not take the wrapper's host lease. If you customize these settings, keep them consistent with the wrapper to preserve artifact reuse.

Important:

- Rust incremental compilation and `sccache` do not compose cleanly
- local day-to-day development in this repo prefers incremental plus fast linker
- `sccache` is still useful for CI and for one-off local builds where you deliberately disable incremental

Recommended local routine:

1. Start with `make check`; use `make warm` only when you need to prebuild tests and benches.
2. During normal development, use `make build` and `make check`.
3. Use `make test-fast` for workspace unit/bin tests and the curated integration modules. It uses the normal target directory by default.
4. Use `make build-fast` when you want an optimized smoke binary without paying full release cost.
5. Inspect cache size before cleaning. Use `make clean-package PACKAGE=reddb-io-server` to reclaim that package's artifacts while keeping other packages' caches; a full `cargo clean` forces a cold build.

## 6. Installing the local accelerators

On Ubuntu, the straightforward setup is:

```bash
sudo apt install clang mold sccache
cp .cargo/config.toml.example .cargo/config.toml
```

If `sudo` is not available, the repo still works without these tools. The wrapper script falls back to normal `cargo`.

For local direct `cargo` usage, keep `mold` in `.cargo/config.toml` and leave `sccache` commented unless you are also running with `CARGO_INCREMENTAL=0`.

## 7. What `make warm` is for

`make warm` intentionally compiles the most common dev surfaces before you start bouncing between commands:

```bash
./scripts/cargo-fast.sh build
./scripts/cargo-fast.sh check --tests
./scripts/cargo-fast.sh check --benches
```

That prewarms:

- normal debug compilation
- test-only codepaths
- benchmark-only codepaths

Prewarming is optional: it trades work now for reuse later. Do not run it automatically after every branch switch when you only need a check.

## 8. CI/CD acceleration

The GitHub Actions workflows now use multiple layers of caching and acceleration:

- `Swatinem/rust-cache@v2` for Cargo registry/git and smart Rust artifact caching
- `mozilla-actions/sccache-action` for compiled object reuse through `sccache`
- `rui314/setup-mold@v1` on native Linux jobs to reduce link time

The current strategy is:

- `ci.yml`
  - `quality` and `tests` use `sccache`
  - both Linux jobs use `mold`
  - Rust caches are shared across the two jobs
  - the persistent test target dir is also cached
- `release.yml`
  - release builds use `sccache` across the matrix
  - native Linux x86_64 release builds use `mold`
  - Rust cache keys are separated by OS and target triple

This gives three different wins:

- faster dependency restore between workflow runs
- less recompilation between similar Rust builds
- faster linking on Linux

In CI this is safe because the workflow already runs with `CARGO_INCREMENTAL=0`, which is compatible with `sccache`.

## 9. What still stays expensive

Even after these changes, some paths will still be slow:

- first build on a new runner with a cold cache
- full release builds for non-native targets
- the musl build that runs inside Docker
- large dependency graph invalidations after major code or profile changes

So the goal is not "every build becomes instant". The goal is:

- fast normal local iteration
- faster repeated CI runs
- less time wasted on linking and recompiling identical work

## 10. Practical rule of thumb

Use this decision table:

- editing code normally: `make build` or `make check`
- opening a session or switching branch: `make warm`
- smoke testing optimized behavior: `make build-fast`
- shipping artifact: `make release`
- only checking types or borrow errors: `make check`

If builds suddenly get much slower, check these first:

- did someone run `cargo clean`?
- did the profile change?
- did the target dir change?
- is `.cargo/config.toml` active locally?
- are `mold` and `sccache` available on the machine?


## 11. Test selection and cache retention

`make test-fast` runs workspace unit/bin tests, builds the standalone `red_client` needed by cross-binary smoke tests, then selects the existing curated modules inside the grouped integration targets. A filter reduces execution, not compilation of that harness. Engine unit tests are included; cold compilation can take longer than the default 300-second step timeout. Set `REDDB_FAST_STEP_TIMEOUT=1200s` explicitly for that case.

`REDDB_FAST_TESTS` and `REDDB_FAST_EXTRA_TESTS` accept space-separated target names or `target:filter` entries. For example:

```bash
REDDB_FAST_TESTS='grouped_sql_core:e2e_ddl_drop_foundation::' make test-fast
```

`CARGO_TARGET_DIR` selects the reusable target for builds and tests. Use `REDDB_FAST_TARGET_DIR` for an explicit fast-lane target, or `REDDB_FAST_SHARED_TARGET=0` to restore the separate `test-fast` directory. Isolation duplicates caches; use it when needed, and retire inactive targets rather than retaining one forever per branch. AFK worktree target isolation remains enabled.

Each test-bearing source file belongs to one grouped harness. Cross-domain selection uses filters instead of including the same file in another binary. CI checks this and the curated target/module names with:

```bash
node --test scripts/build_workflow_contract.test.mjs
```

Cargo's automatic cache cleanup covers downloaded sources, not accumulated build artifacts. The default workflow preserves incremental reuse and does not remove caches automatically. Package-scoped cleanup is explicit:

```bash
make clean-package PACKAGE=reddb-io-server
```

That uses Cargo's own artifact ownership rather than deleting arbitrary files. It can reclaim package variants across profiles, but subsequent builds of that package lose reuse. On WSL, inspect both `df -h /` and `df -h /mnt/c`: free capacity inside a VHD does not guarantee free physical storage on its Windows volume. A tmpfs `TMPDIR` uses RAM/swap; moving compiler temporaries into `/dev/shm` does not solve memory pressure.
