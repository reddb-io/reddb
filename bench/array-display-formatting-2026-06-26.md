# Array display / plain-text formatting allocation — benchmark & verdict (#1342)

Parent: #1337

## What this measures

`Value::display_string()` and `Value::plain_text()` render a `Value::Array` by
building an intermediate `Vec<String>` and joining it:

```rust
// crates/reddb-types/src/types.rs
Value::Array(elems) => {
    let items: Vec<String> = elems.iter().map(|e| e.display_string()).collect();
    format!("[{}]", items.join(", "))
}
```

That is one heap `Vec`, one owned `String` per element, then a second
allocation for the joined result. This slice isolates the **output-formatting**
cost of that path from query execution, and asks whether eliminating the
intermediate `Vec<String>` is justified.

The benchmark (`crates/reddb-types/benches/array_display_bench.rs`) compares the
production path against a **direct-write candidate** that writes into a single
reused `String` buffer (no per-array `Vec<String>`, no separate join
allocation). The candidate is asserted byte-for-byte identical to the baseline
for every case before timing, so any delta is a pure formatting win and never an
output change. It covers scalar arrays, nested arrays, and a mixed
text/scalar/nested array, across `display_string` and `plain_text`.

## Results

Criterion, `--measurement-time 2 --sample-size 30`, release build,
`RUSTFLAGS="-C debuginfo=0"`, 14 GB guard host. Times are per call (median).
Cross-run swing on this host is ±5–10%; only deltas well outside that are
meaningful.

### `display_string`

| case          | elements | baseline | direct-write | speedup |
|---------------|---------:|---------:|-------------:|--------:|
| scalar/16     |       16 |  1.42 µs |      1.35 µs |   ~5%   |
| scalar/256    |      256 | 26.6 µs  |     20.4 µs  |  ~23%   |
| nested/16×16  |      256 | 26.1 µs  |     18.9 µs  |  ~28%   |
| mixed/64      |       64 | 13.7 µs  |      9.1 µs  |  ~33%   |

### `plain_text`

| case          | elements | baseline | direct-write | speedup |
|---------------|---------:|---------:|-------------:|--------:|
| scalar/16     |       16 |  1.59 µs |      1.32 µs |  ~17%   |
| scalar/256    |      256 | 29.8 µs  |     24.8 µs  |  ~17%   |
| nested/16×16  |      256 | 33.3 µs  |     22.0 µs  |  ~34%   |
| mixed/64      |       64 | 14.9 µs  |      8.9 µs  |  ~40%   |

## Reading

- The direct-write approach is **consistently faster** — ~20–40% on arrays of
  64+ elements and on nested arrays, where the per-element `String` + `Vec`
  churn dominates. For tiny scalar arrays (16 elements) the win collapses into
  host noise (~5% for `display_string`).
- The win **grows with array size and nesting depth**, exactly as expected from
  removing the intermediate `Vec<String>` and the second join allocation.
- Absolute cost stays in the **single-digit-to-tens-of-microseconds** range per
  call. This is a formatting-side cost, not a per-byte storage or index cost.

## Is avoiding the intermediate `Vec<String>` justified?

**Verdict: no production change in this slice.** The optimisation is real and
provably output-preserving, but the evidence does not place array formatting on
a hot path that warrants the change now:

- The only production callers of `display_string` / `plain_text` are in SQL
  expression evaluation (`crates/reddb-server/src/runtime/expr_eval.rs`:
  `CONCAT`, `CAST … AS TEXT`, string functions, `LIKE`/`contains`) plus
  `join_filter` and `blockchain_kind` key digests. These run per row, so they
  are query-execution-adjacent rather than purely cosmetic display.
- **But** the `Vec<String>` cost only materialises when the value being coerced
  is actually a `Value::Array`. Scalars — the overwhelmingly common operand for
  these string operators — never hit the array branch, and the scalar case shows
  no meaningful win. A query that pushes large or nested arrays through
  `CONCAT`/`CAST`-to-text on a hot per-row path is an uncommon workload, and no
  current profile points to it.

Per the parent framing ("RedDB only optimizes formatting if it appears on a
relevant hot output path"), the disciplined outcome is to **land the benchmark
as the standing guard / evidence** and defer the code change until a real
workload demonstrates array-through-string-coercion is hot. If that profile ever
appears, the direct-write candidate in the bench is the drop-in replacement for
the two array branches in `types.rs`, already shown identical in output and
~20–40% cheaper.

## Reproduce

```sh
CARGO_BUILD_JOBS=1 RUSTFLAGS="-C debuginfo=0" \
  cargo bench -p reddb-io-types --bench array_display_bench
```
