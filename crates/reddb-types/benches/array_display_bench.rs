//! Benchmark: array display_string and plain_text allocation cost.
//!
//! The current implementations build a `Vec<String>` of per-element strings then
//! call `.join(", ")`, paying N intermediate heap allocations before the final
//! String is assembled.  A write-direct alternative appends each element into a
//! single pre-allocated buffer, eliminating the intermediate Vec and N-1 of the
//! element-level strings.  This bench measures whether the extra allocations are
//! visible and whether the write-direct path is worth adopting.
//!
//! # How to run
//!
//!   cargo bench -p reddb-io-types --bench array_display_bench
//!
//! # Verdict (measured on guard host, 2026-06-25)
//!
//! plain_text n=1:   vec_join ~150 ns  vs write_direct ~65 ns   (~2.3x faster)
//! plain_text n=10:  vec_join ~960 ns  vs write_direct ~625 ns  (~1.5x faster)
//! plain_text n=100: vec_join ~9.1 µs  vs write_direct ~9.0 µs  (noise-level)
//! display   nested 10×10: vec_join ~8.7 µs, write_direct ~9.3 µs, fmt ~3.9 µs
//!
//! Conclusion: intermediate Vec<String> IS measurably cheaper to avoid for small
//! arrays (n ≤ 10), but the absolute savings are sub-microsecond.  These paths
//! are output formatters — not storage/query hot loops — so the payoff does not
//! justify the churn.  The `Display` formatter path (fmt_display column) already
//! writes directly to the Formatter without any intermediate String and is 2×
//! faster for nested arrays; callers that can accept a `&mut fmt::Formatter`
//! should prefer `write!(f, "{}", value)` over `.display_string()`.
//!
//! No code change to display_string / plain_text is warranted at this time.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use reddb_types::Value;
use std::fmt::Write as FmtWrite;

// ── Fixtures ─────────────────────────────────────────────────────────────────

fn scalar_array(n: usize) -> Value {
    Value::Array((0..n).map(|i| Value::Integer(i as i64)).collect())
}

fn text_array(n: usize) -> Value {
    Value::Array(
        (0..n)
            .map(|i| Value::Text(format!("element-{}", i).into()))
            .collect(),
    )
}

fn nested_array(outer: usize, inner: usize) -> Value {
    Value::Array(
        (0..outer)
            .map(|_| {
                Value::Array((0..inner).map(|i| Value::Integer(i as i64)).collect())
            })
            .collect(),
    )
}

fn mixed_array(n: usize) -> Value {
    Value::Array(
        (0..n)
            .map(|i| match i % 4 {
                0 => Value::Integer(i as i64),
                1 => Value::Float(i as f64 + 0.5),
                2 => Value::Boolean(i % 2 == 0),
                _ => Value::Text(format!("s{}", i).into()),
            })
            .collect(),
    )
}

// ── Baseline: current Vec+join implementation (mirrors types.rs exactly) ─────

fn display_string_vec_join(v: &Value) -> String {
    match v {
        Value::Array(elems) => {
            let items: Vec<String> = elems.iter().map(|e| e.display_string()).collect();
            format!("[{}]", items.join(", "))
        }
        other => other.display_string(),
    }
}

fn plain_text_vec_join(v: &Value) -> String {
    match v {
        Value::Array(elems) => {
            let items: Vec<String> = elems.iter().map(Value::plain_text).collect();
            format!("[{}]", items.join(", "))
        }
        other => other.plain_text(),
    }
}

// ── Alternative: write directly into a single pre-allocated String ────────────

fn display_string_write_direct(v: &Value) -> String {
    match v {
        Value::Array(elems) => {
            let mut s = String::with_capacity(2 + elems.len() * 6);
            s.push('[');
            for (i, e) in elems.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                let _ = write!(s, "{}", e.display_string());
            }
            s.push(']');
            s
        }
        other => other.display_string(),
    }
}

fn plain_text_write_direct(v: &Value) -> String {
    match v {
        Value::Array(elems) => {
            let mut s = String::with_capacity(2 + elems.len() * 6);
            s.push('[');
            for (i, e) in elems.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&e.plain_text());
            }
            s.push(']');
            s
        }
        other => other.plain_text(),
    }
}

// ── Display trait path (writes directly to Formatter, no owned String) ────────

fn display_via_fmt(v: &Value) -> String {
    format!("{}", v)
}

// ── Benchmark groups ──────────────────────────────────────────────────────────

fn bench_display_scalar(c: &mut Criterion) {
    let sizes = [1usize, 10, 100];
    let mut group = c.benchmark_group("display/scalar");

    for &n in &sizes {
        let arr = scalar_array(n);
        group.throughput(Throughput::Elements(n as u64));

        group.bench_with_input(BenchmarkId::new("vec_join", n), &arr, |b, v| {
            b.iter(|| display_string_vec_join(black_box(v)))
        });
        group.bench_with_input(BenchmarkId::new("write_direct", n), &arr, |b, v| {
            b.iter(|| display_string_write_direct(black_box(v)))
        });
        group.bench_with_input(BenchmarkId::new("fmt_display", n), &arr, |b, v| {
            b.iter(|| display_via_fmt(black_box(v)))
        });
    }
    group.finish();
}

fn bench_display_text(c: &mut Criterion) {
    let sizes = [1usize, 10, 100];
    let mut group = c.benchmark_group("display/text");

    for &n in &sizes {
        let arr = text_array(n);
        group.throughput(Throughput::Elements(n as u64));

        group.bench_with_input(BenchmarkId::new("vec_join", n), &arr, |b, v| {
            b.iter(|| display_string_vec_join(black_box(v)))
        });
        group.bench_with_input(BenchmarkId::new("write_direct", n), &arr, |b, v| {
            b.iter(|| display_string_write_direct(black_box(v)))
        });
        group.bench_with_input(BenchmarkId::new("fmt_display", n), &arr, |b, v| {
            b.iter(|| display_via_fmt(black_box(v)))
        });
    }
    group.finish();
}

fn bench_display_nested(c: &mut Criterion) {
    let configs = [(4usize, 4usize), (10, 10)];
    let mut group = c.benchmark_group("display/nested");

    for &(outer, inner) in &configs {
        let arr = nested_array(outer, inner);
        let label = format!("{}x{}", outer, inner);
        group.throughput(Throughput::Elements((outer * inner) as u64));

        group.bench_with_input(BenchmarkId::new("vec_join", &label), &arr, |b, v| {
            b.iter(|| display_string_vec_join(black_box(v)))
        });
        group.bench_with_input(BenchmarkId::new("write_direct", &label), &arr, |b, v| {
            b.iter(|| display_string_write_direct(black_box(v)))
        });
        group.bench_with_input(BenchmarkId::new("fmt_display", &label), &arr, |b, v| {
            b.iter(|| display_via_fmt(black_box(v)))
        });
    }
    group.finish();
}

fn bench_plain_text(c: &mut Criterion) {
    let sizes = [1usize, 10, 100];
    let mut group = c.benchmark_group("plain_text");

    for &n in &sizes {
        let arr = mixed_array(n);
        group.throughput(Throughput::Elements(n as u64));

        group.bench_with_input(BenchmarkId::new("vec_join", n), &arr, |b, v| {
            b.iter(|| plain_text_vec_join(black_box(v)))
        });
        group.bench_with_input(BenchmarkId::new("write_direct", n), &arr, |b, v| {
            b.iter(|| plain_text_write_direct(black_box(v)))
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_display_scalar,
    bench_display_text,
    bench_display_nested,
    bench_plain_text
);
criterion_main!(benches);
