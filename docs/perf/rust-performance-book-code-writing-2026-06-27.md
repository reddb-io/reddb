# Rust Performance Book code-writing pass -- 2026-06-27

Parent: #1337

This page records the outcome of the Rust Performance Book code-writing pass:
what shipped, what measured well but did not justify a production change, what
needs a follow-up, and what should not be reopened without new evidence.

These are local, targeted measurements from repo harnesses. They are not
canonical product benchmark claims. Product-facing performance claims still go
through the scenario-specific gate in
[ADR 0009](../../.red/adr/0009-performance-gate-scope.md), `rdb-benchmark`,
and the `wins.md` / `when-not-reddb.md` methodology.

## Shipped code-writing wins

### Runtime join lookup keys (#1345)

The indexed and graph-lookup join paths no longer build formatted lookup strings
such as `n:...`, `b:...`, `t:...`, or `id:...` for every build/probe row. They
now use the typed internal `RuntimeJoinKey` enum:

- `Number(u64)`
- `Boolean(bool)`
- `Text(String)`
- `Identity(String)`

Each variant is its own namespace, preserving the old prefix-collision behavior
without numeric/boolean string formatting. `Text` and `Identity` carry
user-controlled strings, so the implementation keeps the default `std`
`HashMap` hasher. No weak hasher is used for user-controlled keys.

The shipped commit also pre-sizes join hash tables where the build-side record
count is known. The hash-join path keeps its raw `Value::to_string()` keying to
preserve the distinct null/empty-bucket behavior pinned by #1339.

### Indexed-join candidate-list borrowing (#1346)

`execute_runtime_indexed_join` no longer clones the right-side bucket
`Vec<usize>` for every left row. The probe loop only reads the bucket, so it now
borrows the candidate list as a slice and performs zero temporary allocation for
that path.

The graph-join probe still returns an owned `Vec` because it deduplicates and
sorts candidates from multiple keys through a `BTreeSet`; that ownership is
required for correctness.

### Join benchmark context

The behavior and timing guard lives in `join_filter::tests` and covers numeric,
boolean, text, identity/reference, null, missing-field, duplicate-key, outer,
cross, and mixed numeric joins. It protects observable results rather than the
private key representation.

Baseline command from #1339:

```sh
CARGO_BUILD_JOBS=1 RUSTFLAGS="-C debuginfo=0" \
  cargo nextest run -p reddb-io-server --lib \
    -- join_filter::tests::benchmark_join_build_probe_timing --nocapture
```

Baseline on the 2026-06-25 guard host, debug profile:

| n | hash join | nested loop | reading |
|---:|----------:|------------:|---------|
| 10 | 66 us | 90 us | hash already ahead, but close |
| 100 | 654 us | 4,906 us | hash join 7.5x faster |
| 1,000 | 6,385 us | 432,443 us | hash join 68x faster |

Current post-#1345/#1346 timing from this branch, run on 2026-06-27:

```sh
CARGO_BUILD_JOBS=1 RUSTFLAGS="-C debuginfo=0" \
  cargo test -p reddb-io-server --lib \
    join_filter::tests::benchmark_join_build_probe_timing -- --nocapture
```

| n | hash join | indexed join | nested loop | reading |
|---:|----------:|-------------:|------------:|---------|
| 10 | 68 us | 76 us | 90 us | indexed path remains close to hash join |
| 100 | 668 us | 756 us | 4,866 us | indexed/hash stay in the same band |
| 1,000 | 7,878 us | 9,705 us | 641,987 us | O(n) join paths still dominate nested loop |

Read these as a local guard for the join implementation, not a universal
benchmark. The shipped wins are lower allocation and less key formatting in the
indexed/graph join paths while preserving result semantics.

## Measured experiments with no production change

### Row-id intersection strategies (#1340)

Harness: `crates/reddb-server/benches/row_id_intersection_bench.rs`

Command:

```sh
cargo bench -p reddb-io-server --bench row_id_intersection_bench
```

The benchmark compared:

- current `HashSet<u64>` with SipHash
- identity/noop hasher for internal numeric ids
- sorted two-pointer merge
- galloping/binary-search intersection

Scenarios covered dense overlap, sparse overlap, no overlap, and skewed
candidate sets at n=100, 10,000, and 100,000.

Findings:

| experiment | result |
|------------|--------|
| sorted merge | 10-100x faster only when inputs are already sorted by entity id |
| sorted merge in the real path | not justified because `collect_range_limited` returns ids in B-tree key-bucket order, not entity-id order; pre-sorting costs O(n log n), roughly erasing the win |
| identity hasher | 10-25x slower at n=100,000 for consecutive ids near zero because hashbrown SwissTable tag collisions force full-key comparisons |
| current SipHash set | best justified production choice for the current unsorted input shape |

Conclusion: no change to `intersect_sorted_id_sets` shipped. A future sorted-id
emission change would need a separate storage/query issue.

### Array display/plain-text formatting (#1342)

Harness: `crates/reddb-types/benches/array_display_bench.rs`

Verdict report: `bench/array-display-formatting-2026-06-26.md`

Command:

```sh
CARGO_BUILD_JOBS=1 RUSTFLAGS="-C debuginfo=0" \
  cargo bench -p reddb-io-types --bench array_display_bench
```

The direct-write candidate avoided the intermediate `Vec<String>` used by
`Value::Array` formatting and produced identical output.

Measured local result: about 20-40% faster for arrays with 64+ elements or
nested arrays, with tiny scalar-array cases collapsing into host noise.

Conclusion: no production change shipped. The win is real but currently tied to
array-through-string-coercion workloads, and no profile shows that path hot.
Keep the benchmark as the guard and only reopen if a real workload puts array
formatting on a hot query/output path.

### Build/profile matrix (#1344)

Report: `bench/build-profile-experiment-2026-06-25.md`

Representative commands:

```sh
time cargo build --locked --profile release --bin red

CARGO_BUILD_JOBS=1 RUSTFLAGS="-C debuginfo=0" \
  cargo bench -p reddb-io-server --bench columnar_read_bench \
  -- --warm-up-time 1 --measurement-time 5

time cargo build --locked --profile release-lto-thin --bin red
time cargo build --locked --profile release-lto-fat --bin red
time cargo build --locked --profile release-opt3 --bin red
RUSTFLAGS="-C target-cpu=native" \
  time cargo build --locked --profile release --bin red
```

Measured data points:

| item | result |
|------|--------|
| baseline `release` binary | 37.5 MB unstripped, 30.0 MB stripped |
| baseline cold `release` build | 14m 26s on the guarded host |
| default `bench` profile build | 10m 54s for the bench binary, reusing most release rlibs |
| columnar decode throughput | 57-83 M rows/s depending on decode path and chunk size |
| thin LTO first build | expected/measured as 5-9x longer than no-LTO because all deps need bitcode-compatible recompilation |

Conclusion: no default profile change shipped. Thin LTO remains a possible
release/nightly follow-up only after a complete throughput delta, because the
cold-build penalty is too high for PR validation. Fat LTO, allocator swaps,
`opt-level = 3`, `target-cpu=native`, and PGO were not adopted as defaults.

## Measured follow-up candidates

### `Value` layout (#1341)

Harnesses:

- `crates/reddb-types/src/bin/measure_value_layout.rs`
- `crates/reddb-types/tests/value_layout_report.rs`

Commands:

```sh
cargo run -p reddb-io-types --bin measure_value_layout

CARGO_BUILD_JOBS=1 RUSTFLAGS="-C debuginfo=0" \
  cargo nextest run -p reddb-io-types --test value_layout_report --no-capture
```

Measured x86-64 findings:

| item | result |
|------|--------|
| `size_of::<Value>()` | 48 bytes |
| variants measured | 52 |
| compact scalar variants | 31 word-sized scalar variants in the regression test; the earlier measurement script reported 32 compact scalar variants |
| layout drivers | `KeyRef` 48 B, `Money` 40 B, `VectorRef`/`RowRef`/`DocRef` 32 B |
| common scalar overhead | about 40 B above an 8-byte scalar payload |

Conclusion: a follow-up representation slice is justified, but no semantics,
serialization, query behavior, or layout rewrite shipped in this pass. A future
change should confirm the wide variants are cold before boxing or splitting
them.

### BufferRing contention (#1343)

Harness: `crates/reddb-server/benches/cache_ring_contention_bench.rs`

Command:

```sh
cargo bench -p reddb-io-server --bench cache_ring_contention_bench
```

The benchmark drove one shared `BufferRing` with 1, 2, 4, and 8 threads across
read-heavy, mixed, and write-heavy workloads at capacities 16 and 32.

Measured finding: aggregate throughput had negative scaling at 8 threads,
about 0.05-0.22x of the single-thread rate depending on workload and capacity.

Conclusion: contention is material in the shared-ring benchmark, so a
synchronization follow-up is justified. No cache logic or eviction semantics
changed here. The important caveat is production sharing: each
`BufferAccessStrategy` ring is usually per scan/cursor, so a rewrite should
first confirm that shared rings are active on a real hot path.

## Rejected ideas

Do not reopen these without fresh measurements that contradict this pass:

- No blanket weak-hasher replacement. The identity-hasher row-id experiment was
  slower at large sizes, and user-controlled strings must keep SipHash/default
  hashing.
- No assertion removal. Release-live assertions stayed intact; this pass did
  not trade correctness checks for microbench noise.
- No non-Rust driver scope. This was a Rust implementation/code-writing pass,
  not a driver rewrite pass.
- No unmeasured profile changes. LTO, allocator swaps, `opt-level = 3`,
  `target-cpu=native`, and PGO remain experiment/follow-up material until a
  completed workload-specific measurement justifies adoption.
- No universal performance claim. The evidence here is local to the named
  harnesses and must not be translated into a repo-wide percentage claim.

## Already healthy areas

- The runtime hash join already beats nested loop decisively at n=100 and above;
  the pass kept that O(n) behavior and added indexed-join coverage.
- The current row-id intersection path is defensible for unsorted entity-id
  candidates; the tempting sorted and weak-hasher alternatives did not survive
  measurement.
- The existing `release-fast` and `release-static` profiles already have clear
  roles: build speed and distribution size respectively. They are not
  throughput defaults.
- The performance roadmap already uses a scenario-specific gate, so this pass
  only adds targeted code-path evidence instead of changing RedDB's public
  benchmark posture.

## Verification

Docs-only change for #1347. The added page contains shell commands and source
references but no doctests or code-adjacent examples, so `cargo check` is not
required by the issue acceptance criteria.

Additional confidence run performed while preparing this page:

```sh
CARGO_BUILD_JOBS=1 RUSTFLAGS="-C debuginfo=0" \
  cargo test -p reddb-io-server --lib \
    join_filter::tests::benchmark_join_build_probe_timing -- --nocapture
```

Result: passed, 1 test, 0 failures, 4,952 filtered out.
