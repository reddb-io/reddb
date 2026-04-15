# Running Benchmarks

This guide explains which build to use, which binary matters in each case, and how to run RedDB benchmarks without mixing up benchmark modes.

## 1. First rule: not every benchmark uses the `red` binary

There are three distinct benchmark paths in this repository.

Criterion/Cargo benchmarks:

- run with `cargo bench --bench ...`
- do **not** use `target/release/red`
- Cargo builds and runs a dedicated benchmark executable for each bench target

Manual or end-to-end server benchmarking:

- uses the `red` binary
- usually compare HTTP, gRPC, or CLI query latency/throughput
- this is where `target/release/red` matters

Ignored test-style benchmark harnesses:

- run with `cargo test ... -- --ignored --nocapture`
- useful for some algorithm and stress scenarios
- these also do **not** use the `red` CLI binary directly

If you pick the wrong path, the numbers can still "look valid" while measuring the wrong thing.

## 2. Benchmark targets in this repo

The current Cargo bench targets are:

- `bench_embedded`
- `bench_insert`
- `perf_sweep`

Run them like this:

```bash
cargo bench --bench bench_embedded
cargo bench --bench bench_insert
cargo bench --bench perf_sweep
```

What each one is for:

- `bench_embedded`: macro-style embedded runtime benchmarks across rows, docs, graph, vector, KV, and query paths
- `bench_insert`: persistent bulk insert benchmark
- `perf_sweep`: microbenchmarks for hot-path primitives such as filter execution and WAL behavior

Criterion outputs usually land under:

```bash
target/criterion/
```

## 3. Which binary to use

Use this rule:

- for `cargo bench`: do not choose a binary manually; Cargo runs the bench target executable
- for quick local smoke perf: use `target/release-fast/red`
- for numbers you want to trust and compare over time: use `target/release/red`

So the answer to "which bin do I use for benchmark?" is:

- Criterion benches: not `red`
- end-to-end benchmark with the server/CLI: `red`
- serious end-to-end benchmark: `target/release/red`

## 4. Which build to use

There are two relevant optimized builds for benchmark-related work.

Fast local optimized build:

```bash
make build-fast
```

This produces:

```bash
target/release-fast/red
```

Use it for:

- local smoke runs
- quick throughput sanity checks
- validating that a perf-sensitive path is not catastrophically worse

Do **not** use it for:

- final published benchmark numbers
- comparisons against previous releases
- claims about absolute engine performance

Final optimized build:

```bash
make release
```

This produces:

```bash
target/release/red
```

Use it for:

- benchmark reports
- before/after comparisons you want to keep
- CI performance jobs
- external comparisons against other databases

## 5. Recommended workflow by benchmark type

### Embedded Rust benches

Use this for engine-in-process benchmarking.

```bash
make warm
cargo bench --bench bench_embedded
```

Or for a focused run:

```bash
cargo bench --bench perf_sweep
```

Use this when you want:

- engine hot-path measurements
- lower noise than client/server round-trips
- direct comparison of internal primitives

### Persistent insert benchmark

```bash
cargo bench --bench bench_insert
```

This runs the dedicated persistent bulk insert harness. It does not use `red`.

### Manual server benchmark

Build the binary first:

```bash
make release
```

Then run the server:

```bash
./target/release/red server --path ./tmp/bench.rdb --http-bind 127.0.0.1:8080
```

Now point your HTTP/gRPC benchmark tool at that process.

Examples of when this is the right mode:

- HTTP latency benchmarks
- gRPC throughput benchmarks
- connection-handling benchmarks
- end-to-end query benchmarks including protocol overhead

## 6. Algorithm benchmark harnesses

Some benchmark-like routines are implemented as ignored tests.

Example pattern:

```bash
cargo test --release bench_graph_creation_1m_edges -- --ignored --nocapture
```

Use this model for the graph algorithm harnesses under:

[`src/storage/engine/algorithms/mod.rs`](/home/cyber/Work/FF/reddb/src/storage/engine/algorithms/mod.rs)

This path is useful when the benchmark is easier to express as a test harness than as a Criterion bench.

## 7. Practical decision table

Use this rule of thumb:

- "I want to benchmark engine internals": `cargo bench --bench ...`
- "I want to benchmark the real server process": `make release` then `./target/release/red ...`
- "I only want a quick optimized smoke run": `make build-fast` then `./target/release-fast/red ...`
- "I want algorithm stress output already in tests": `cargo test --release ... -- --ignored --nocapture`

## 8. Important caveats

For trustworthy benchmark numbers:

- prefer `target/release/red`, not `release-fast`
- avoid running heavy parallel builds during measurement
- keep the same machine, CPU governor, and dataset shape when comparing runs
- do a warm-up run before recording numbers
- do not compare `cargo bench` numbers directly to HTTP benchmark numbers as if they were the same class of result

The big distinction is:

- `cargo bench` mostly measures engine code
- `red server` benchmarks measure engine + protocol + serialization + scheduler + networking
