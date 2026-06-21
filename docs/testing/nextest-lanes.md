# Test lanes under cargo-nextest

RedDB runs its Rust test suite through [`cargo-nextest`](https://nexte.st). nextest
gives us two properties the built-in `cargo test` harness does not:

- **Per-test process isolation.** Each test runs in its own process, so a crash
  or a leaked global in one test cannot corrupt another.
- **A hard per-test timeout.** A test that hangs is killed and reported as a
  timeout failure instead of stalling the whole run — the same stall class that
  used to wedge AFK workers.

The timeout lives in [`.config/nextest.toml`](../../.config/nextest.toml):

```toml
[profile.default]
slow-timeout = { period = "60s", terminate-after = 2 }
```

A test exceeding `period` (60s) is flagged slow; after `terminate-after` such
periods (2 × 60s = 120s) nextest terminates the process and records a timeout.

## Install

```sh
cargo install cargo-nextest --locked
```

## Lanes

The suite is split into two lanes that run independently.

### Lib lane — fast unit tests

In-crate `--lib` unit tests only. Fast, no external state, runs on every change.

```sh
cargo nextest run --workspace --lib
# or:
make test-nextest-lib
```

### e2e lane — integration-test binaries

The top-level integration-test binaries (nextest filterset `kind(test)`): the
`tests/*.rs` targets across the workspace, including the grouped harnesses.

```sh
cargo nextest run --workspace -E 'kind(test)'
# or:
make test-nextest-e2e
```

## Sharding the e2e lane

The e2e lane is the slow lane, so it is shardable across runners. nextest's
`--partition count:<index>/<total>` deterministically splits the selected tests
into `total` disjoint buckets; the union of all shards is the whole lane.

```sh
# four runners, each executing a disjoint quarter of the e2e lane:
scripts/nextest-e2e-shard.sh 1 4
scripts/nextest-e2e-shard.sh 2 4
scripts/nextest-e2e-shard.sh 3 4
scripts/nextest-e2e-shard.sh 4 4
```

Each invocation runs `cargo nextest run --workspace -E 'kind(test)' --partition
count:<index>/<total>` under the `ci` profile (single retry + JUnit report).
