//! Array display / plain-text formatting allocation benchmark (#1342).
//!
//! Run with:
//!   cargo bench -p reddb-io-types --bench array_display_bench
//!
//! Goal: isolate the *output formatting* cost of `Value::display_string` and
//! `Value::plain_text` for `Value::Array` from any query-execution cost, so we
//! can decide whether the intermediate `Vec<String>` those paths build is worth
//! eliminating.
//!
//! Both paths today render an array as:
//!
//! ```ignore
//! let items: Vec<String> = elems.iter().map(|e| e.display_string()).collect();
//! format!("[{}]", items.join(", "))
//! ```
//!
//! i.e. one heap `Vec` plus one owned `String` per element, then a second
//! allocation for the joined result. The candidate optimisation writes directly
//! into a single reused `String` buffer (`write_display` / `write_plain` below),
//! avoiding the per-array `Vec<String>` and the separate join allocation.
//!
//! The benchmark measures the *baseline* (production `display_string` /
//! `plain_text`) against the *direct-write* candidate across scalar, nested, and
//! mixed arrays at two sizes. The candidate is asserted byte-for-byte identical
//! to the baseline before timing, so any reported delta is a pure formatting
//! win, never an output change.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use reddb_types::Value;

// --- Direct-write candidate (no intermediate `Vec<String>`) -----------------
//
// Mirrors the production `Array` branch exactly: `[`, elements separated by
// `, `, `]`. Non-array leaves defer to the existing `display_string` /
// `plain_text`, so output is identical by construction.

fn write_display(value: &Value, out: &mut String) {
    match value {
        Value::Array(elems) => {
            out.push('[');
            for (i, elem) in elems.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_display(elem, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.display_string()),
    }
}

fn write_plain(value: &Value, out: &mut String) {
    match value {
        Value::Array(elems) => {
            out.push('[');
            for (i, elem) in elems.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_plain(elem, out);
            }
            out.push(']');
        }
        Value::Text(text) => out.push_str(text),
        other => out.push_str(&other.display_string()),
    }
}

fn candidate_display(value: &Value) -> String {
    let mut out = String::new();
    write_display(value, &mut out);
    out
}

fn candidate_plain(value: &Value) -> String {
    let mut out = String::new();
    write_plain(value, &mut out);
    out
}

// --- Workload builders ------------------------------------------------------

/// Array of scalar values (integers, floats, booleans) — the common case.
fn scalar_array(n: usize) -> Value {
    let elems = (0..n)
        .map(|i| match i % 3 {
            0 => Value::Integer(i as i64),
            1 => Value::Float(i as f64 * 1.5),
            _ => Value::Boolean(i % 2 == 0),
        })
        .collect();
    Value::Array(elems)
}

/// Array of arrays of scalars — exercises the recursive allocation amplification.
fn nested_array(outer: usize, inner: usize) -> Value {
    let elems = (0..outer).map(|_| scalar_array(inner)).collect();
    Value::Array(elems)
}

/// Mixed array: text (distinguishes display vs plain-text), scalars, and a
/// nested array — the worst case for the intermediate-`Vec` path.
fn mixed_array(n: usize) -> Value {
    let elems = (0..n)
        .map(|i| match i % 4 {
            0 => Value::text(format!("item-{i}")),
            1 => Value::Integer(i as i64),
            2 => Value::Float(i as f64),
            _ => scalar_array(4),
        })
        .collect();
    Value::Array(elems)
}

// --- Benchmark groups -------------------------------------------------------

fn bench_display(c: &mut Criterion) {
    let cases: Vec<(&str, Value)> = vec![
        ("scalar/16", scalar_array(16)),
        ("scalar/256", scalar_array(256)),
        ("nested/16x16", nested_array(16, 16)),
        ("mixed/64", mixed_array(64)),
    ];

    let mut group = c.benchmark_group("array_display_string");
    for (name, value) in &cases {
        // Equivalence guard: the candidate must match production output exactly.
        assert_eq!(
            value.display_string(),
            candidate_display(value),
            "candidate display must equal baseline for {name}"
        );
        let elems = match value {
            Value::Array(e) => e.len() as u64,
            _ => 1,
        };
        group.throughput(Throughput::Elements(elems));
        group.bench_with_input(BenchmarkId::new("baseline", name), value, |b, v| {
            b.iter(|| black_box(black_box(v).display_string()))
        });
        group.bench_with_input(BenchmarkId::new("direct_write", name), value, |b, v| {
            b.iter(|| black_box(candidate_display(black_box(v))))
        });
    }
    group.finish();
}

fn bench_plain(c: &mut Criterion) {
    let cases: Vec<(&str, Value)> = vec![
        ("scalar/16", scalar_array(16)),
        ("scalar/256", scalar_array(256)),
        ("nested/16x16", nested_array(16, 16)),
        ("mixed/64", mixed_array(64)),
    ];

    let mut group = c.benchmark_group("array_plain_text");
    for (name, value) in &cases {
        assert_eq!(
            value.plain_text(),
            candidate_plain(value),
            "candidate plain_text must equal baseline for {name}"
        );
        let elems = match value {
            Value::Array(e) => e.len() as u64,
            _ => 1,
        };
        group.throughput(Throughput::Elements(elems));
        group.bench_with_input(BenchmarkId::new("baseline", name), value, |b, v| {
            b.iter(|| black_box(black_box(v).plain_text()))
        });
        group.bench_with_input(BenchmarkId::new("direct_write", name), value, |b, v| {
            b.iter(|| black_box(candidate_plain(black_box(v))))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_display, bench_plain);
criterion_main!(benches);
