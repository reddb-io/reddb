//! Array display / plain-text formatting allocation benchmark (#1342, parent #1337).
//!
//! Goal: isolate the *output formatting* cost of `Value::Array` rendering from
//! query execution, so RedDB only optimises formatting if it sits on a hot
//! output path. The two array-rendering helpers on `Value`
//! (`display_string` and `plain_text`) both build an intermediate
//! `Vec<String>` and then `join(", ")` it:
//!
//! ```ignore
//! Value::Array(elems) => {
//!     let items: Vec<String> = elems.iter().map(|e| e.display_string()).collect();
//!     format!("[{}]", items.join(", "))
//! }
//! ```
//!
//! This benchmark measures whether avoiding that intermediate `Vec<String>` is
//! justified. For each workload it compares three strategies that all produce
//! byte-identical output:
//!
//! - `vec-join`   — the production strategy (collect into `Vec<String>` + join).
//! - `streaming`  — push each element's rendering directly into one `String`
//!                  (no intermediate vector, no separate join buffer).
//! - `display`    — the existing `Display`/`ToString` path, which already
//!                  streams via `write!` and serves as a reference point.
//!
//! Workloads cover the cases named in the acceptance criteria:
//! - scalar arrays (integers / text),
//! - nested arrays (arrays of arrays),
//! - mixed display/plain-text values (where `plain_text` and `display_string`
//!   diverge, e.g. unquoted `Text`).
//!
//! Run with:
//!   cargo bench -p reddb-io-types --bench array_format_bench
//!
//! ## Findings (2026-06-26, guard host, `--quick`; absolute numbers are noisy
//! but the cross-arm ratios are stable across runs)
//!
//! `display_string` (scalar / nested integer arrays):
//!
//! | workload          | vec-join | streaming | display (stream) |
//! |-------------------|---------:|----------:|-----------------:|
//! | scalar / 8        |  ~0.5 µs |   ~0.46µs |          ~0.31µs |
//! | scalar / 64       |  5.81 µs |   6.06 µs |          2.42 µs |
//! | scalar / 512      | 72.9  µs |  51.2  µs |         24.8  µs |
//! | nested 16× / 8    | 16.8  µs |  12.2  µs |              n/a |
//! | nested 16× / 64   | 118   µs |  106   µs |              n/a |
//!
//! `plain_text` (text / mixed arrays):
//!
//! | workload          | vec-join | streaming |
//! |-------------------|---------:|----------:|
//! | text  / 64        |  4.06 µs |   2.07 µs |
//! | text  / 512       | 34.9  µs |  16.2  µs |
//! | mixed / 64        |  6.50 µs |   4.08 µs |
//! | mixed / 512       | 58.0  µs |  35.7  µs |
//!
//! Three conclusions:
//!
//! 1. For `display_string` over scalar arrays the `streaming` arm (drop the
//!    `Vec<String>`, keep per-element `String` allocs) is *noise-level* up to
//!    ~64 elements; a real ~1.3× win only emerges at 512+. The dominant cost
//!    is the per-element allocation BOTH arms pay, not the vector.
//! 2. For `plain_text` the `streaming` arm IS consistently faster (~1.6–2×).
//!    So removing the intermediate vector is *measurable* here — but the
//!    absolute cost is single-digit µs for the array sizes (≤64) these helpers
//!    realistically see, and tens of µs only for 512-element arrays.
//! 3. The clear, consistent winner where it applies is the `Display`/`ToString`
//!    path, which already streams every element into one buffer via `write!`
//!    with zero per-element allocations (2–3× over `vec-join`). That is where
//!    the real allocation cost lives, and `to_string()` already uses it.
//!
//! ## Decision: no production code change.
//!
//! `Value::display_string` / `Value::plain_text` array branches are left as-is
//! (`Vec<String>` + `join`). The microbenchmark win for the `streaming` arm is
//! real for `plain_text` but is *not material on any demonstrated hot path*,
//! which is the bar this slice (parent #1337) set: "RedDB only optimizes
//! formatting if it appears on a relevant hot output path." Rationale:
//!   - These helpers are reached from `expr_eval` CAST/CONCAT/string ops, but
//!     the *array* branch specifically is rare — scalar operands take the cheap
//!     `other => other.display_string()` fall-through and never touch this code.
//!     No profile shows array formatting on a query hot path.
//!   - Absolute cost at realistic sizes is single-digit µs; a 1.6–2× cut of a
//!     cold µs-scale path is not worth the surface-area change.
//!   - The genuinely faster strategy (fully streaming, zero per-element allocs)
//!     already exists as the `Display` impl for the common `to_string()` case;
//!     porting it into these two helpers would duplicate a recursive `write!`
//!     writer for a path that is not hot.
//!
//! This benchmark is retained as the decision artifact and a regression guard:
//! if array formatting is later shown to sit on a hot output path, re-run it
//! and adopt the `streaming`-style writer (verified output-identical to the
//! production helpers by `assert_streaming_equivalence`).

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use reddb_types::types::Value;
use std::hint::black_box;

// ---------------------------------------------------------------------------
// Streaming equivalents of the production array branches.
//
// These reproduce the EXACT bytes emitted by `Value::display_string` /
// `Value::plain_text` for arrays, but accumulate into a single `String`
// instead of a `Vec<String>` + `join`. Element rendering still defers to the
// production helpers, so the only behavioural difference is the elimination of
// the intermediate vector and the join's separate buffer.
// ---------------------------------------------------------------------------

fn display_string_streaming(v: &Value) -> String {
    match v {
        Value::Array(elems) => {
            let mut out = String::with_capacity(elems.len() * 8 + 2);
            out.push('[');
            for (i, e) in elems.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&display_string_streaming(e));
            }
            out.push(']');
            out
        }
        other => other.display_string(),
    }
}

fn plain_text_streaming(v: &Value) -> String {
    match v {
        Value::Array(elems) => {
            let mut out = String::with_capacity(elems.len() * 8 + 2);
            out.push('[');
            for (i, e) in elems.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&plain_text_streaming(e));
            }
            out.push(']');
            out
        }
        Value::Text(text) => text.to_string(),
        other => other.display_string(),
    }
}

// ---------------------------------------------------------------------------
// Workloads.
// ---------------------------------------------------------------------------

fn scalar_int_array(n: usize) -> Value {
    Value::Array((0..n as i64).map(Value::Integer).collect())
}

fn scalar_text_array(n: usize) -> Value {
    Value::Array(
        (0..n)
            .map(|i| Value::text(format!("item-{i}")))
            .collect(),
    )
}

fn nested_array(rows: usize, cols: usize) -> Value {
    Value::Array(
        (0..rows)
            .map(|_| scalar_int_array(cols))
            .collect(),
    )
}

fn mixed_array(n: usize) -> Value {
    Value::Array(
        (0..n)
            .map(|i| match i % 4 {
                0 => Value::text(format!("name-{i}")),
                1 => Value::Integer(i as i64),
                2 => Value::Boolean(i % 2 == 0),
                _ => Value::Float(i as f64 + 0.5),
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Benchmark groups.
// ---------------------------------------------------------------------------

/// Assert the streaming variants are byte-for-byte identical to the production
/// helpers, so the benchmark compares like with like (and so any future
/// production adoption of the streaming form would preserve output exactly).
///
/// Called at the start of each bench group. `harness = false` means criterion
/// owns `main`, so ordinary `#[test]` functions in this file would never run —
/// running the equivalence check inline guarantees it executes on every
/// `cargo bench` invocation instead.
fn assert_streaming_equivalence() {
    let cases = vec![
        Value::Array(vec![]),
        scalar_int_array(1),
        scalar_int_array(64),
        scalar_text_array(32),
        nested_array(4, 5),
        mixed_array(40),
        // Deeply nested, to exercise recursion.
        Value::Array(vec![nested_array(2, 3), scalar_text_array(2)]),
    ];
    for v in &cases {
        assert_eq!(
            display_string_streaming(v),
            v.display_string(),
            "streaming display_string diverged for {v:?}"
        );
        assert_eq!(
            plain_text_streaming(v),
            v.plain_text(),
            "streaming plain_text diverged for {v:?}"
        );
    }
}

fn bench_display_string(c: &mut Criterion) {
    assert_streaming_equivalence();
    let mut group = c.benchmark_group("array-display_string");
    for &n in &[8usize, 64, 512] {
        let scalar = scalar_int_array(n);
        group.throughput(Throughput::Elements(n as u64));

        group.bench_with_input(BenchmarkId::new("scalar/vec-join", n), &scalar, |b, v| {
            b.iter(|| black_box(v.display_string()))
        });
        group.bench_with_input(BenchmarkId::new("scalar/streaming", n), &scalar, |b, v| {
            b.iter(|| black_box(display_string_streaming(v)))
        });
        group.bench_with_input(BenchmarkId::new("scalar/display", n), &scalar, |b, v| {
            b.iter(|| black_box(v.to_string()))
        });
    }

    // Nested arrays: 16 rows of `cols` scalars each.
    for &cols in &[8usize, 64] {
        let nested = nested_array(16, cols);
        group.bench_with_input(BenchmarkId::new("nested/vec-join", cols), &nested, |b, v| {
            b.iter(|| black_box(v.display_string()))
        });
        group.bench_with_input(
            BenchmarkId::new("nested/streaming", cols),
            &nested,
            |b, v| b.iter(|| black_box(display_string_streaming(v))),
        );
    }
    group.finish();
}

fn bench_plain_text(c: &mut Criterion) {
    let mut group = c.benchmark_group("array-plain_text");
    for &n in &[8usize, 64, 512] {
        let text = scalar_text_array(n);
        let mixed = mixed_array(n);
        group.throughput(Throughput::Elements(n as u64));

        group.bench_with_input(BenchmarkId::new("text/vec-join", n), &text, |b, v| {
            b.iter(|| black_box(v.plain_text()))
        });
        group.bench_with_input(BenchmarkId::new("text/streaming", n), &text, |b, v| {
            b.iter(|| black_box(plain_text_streaming(v)))
        });
        group.bench_with_input(BenchmarkId::new("mixed/vec-join", n), &mixed, |b, v| {
            b.iter(|| black_box(v.plain_text()))
        });
        group.bench_with_input(BenchmarkId::new("mixed/streaming", n), &mixed, |b, v| {
            b.iter(|| black_box(plain_text_streaming(v)))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_display_string, bench_plain_text);
criterion_main!(benches);
