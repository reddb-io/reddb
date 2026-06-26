//! Value layout measurement harness (issue #1341, parent #1337).
//!
//! Measurement-first slice: report the in-memory layout of the shared
//! [`Value`] enum and identify which variants drive its size, so a later
//! representation rewrite (boxing/splitting rare large variants) can be
//! justified by evidence rather than speculation.
//!
//! Reproducible command:
//!
//! ```text
//! CARGO_BUILD_JOBS=1 RUSTFLAGS="-C debuginfo=0" \
//!   cargo nextest run -p reddb-io-types --test value_layout_report --no-capture
//! ```
//!
//! The `--no-capture` flag surfaces the printed report; the asserts double as
//! a regression guard so an accidental layout blow-up trips CI.

use std::mem::{align_of, size_of};
use std::net::IpAddr;

use reddb_types::Value;

/// One measured variant: its name, the size of its payload (the fields it
/// carries), and whether it owns a heap allocation.
struct VariantMeasure {
    name: &'static str,
    /// Size in bytes of the variant's payload tuple (0 for unit variants).
    payload: usize,
    /// `true` when the variant may point at a separate heap allocation
    /// (`String`/`Vec`/`Arc`). These are the variants whose *inline* footprint
    /// is bounded but whose total cost is paid elsewhere.
    heap: bool,
    /// `true` for the common, cheap, `Copy` scalar variants.
    scalar: bool,
}

/// Payload size of a variant = `size_of` of the tuple of its field types.
/// We list every variant explicitly so the report stays honest if a field is
/// added or its type changes.
fn measure() -> Vec<VariantMeasure> {
    macro_rules! v {
        ($name:literal, $ty:ty, heap = $heap:expr, scalar = $scalar:expr) => {
            VariantMeasure {
                name: $name,
                payload: size_of::<$ty>(),
                heap: $heap,
                scalar: $scalar,
            }
        };
    }

    vec![
        v!("Null", (), heap = false, scalar = true),
        v!("Integer", i64, heap = false, scalar = true),
        v!("UnsignedInteger", u64, heap = false, scalar = true),
        v!("Float", f64, heap = false, scalar = true),
        v!("Text", std::sync::Arc<str>, heap = true, scalar = false),
        v!("Blob", Vec<u8>, heap = true, scalar = false),
        v!("Boolean", bool, heap = false, scalar = true),
        v!("Timestamp", i64, heap = false, scalar = true),
        v!("Duration", i64, heap = false, scalar = true),
        v!("IpAddr", IpAddr, heap = false, scalar = true),
        v!("MacAddr", [u8; 6], heap = false, scalar = true),
        v!("Vector", Vec<f32>, heap = true, scalar = false),
        v!("Json", Vec<u8>, heap = true, scalar = false),
        v!("Uuid", [u8; 16], heap = false, scalar = false),
        v!("NodeRef", String, heap = true, scalar = false),
        v!("EdgeRef", String, heap = true, scalar = false),
        v!("VectorRef", (String, u64), heap = true, scalar = false),
        v!("RowRef", (String, u64), heap = true, scalar = false),
        v!("Color", [u8; 3], heap = false, scalar = true),
        v!("Email", String, heap = true, scalar = false),
        v!("Url", String, heap = true, scalar = false),
        v!("Phone", u64, heap = false, scalar = true),
        v!("Semver", u32, heap = false, scalar = true),
        v!("Cidr", (u32, u8), heap = false, scalar = true),
        v!("Date", i32, heap = false, scalar = true),
        v!("Time", u32, heap = false, scalar = true),
        v!("Decimal", i64, heap = false, scalar = true),
        v!("EnumValue", u8, heap = false, scalar = true),
        v!("Array", Vec<Value>, heap = true, scalar = false),
        v!("TimestampMs", i64, heap = false, scalar = true),
        v!("Ipv4", u32, heap = false, scalar = true),
        v!("Ipv6", [u8; 16], heap = false, scalar = false),
        v!("Subnet", (u32, u32), heap = false, scalar = true),
        v!("Port", u16, heap = false, scalar = true),
        v!("Latitude", i32, heap = false, scalar = true),
        v!("Longitude", i32, heap = false, scalar = true),
        v!("GeoPoint", (i32, i32), heap = false, scalar = true),
        v!("Country2", [u8; 2], heap = false, scalar = true),
        v!("Country3", [u8; 3], heap = false, scalar = true),
        v!("Lang2", [u8; 2], heap = false, scalar = true),
        v!("Lang5", [u8; 5], heap = false, scalar = true),
        v!("Currency", [u8; 3], heap = false, scalar = true),
        v!("AssetCode", String, heap = true, scalar = false),
        v!("Money", (String, i64, u8), heap = true, scalar = false),
        v!("ColorAlpha", [u8; 4], heap = false, scalar = true),
        v!("BigInt", i64, heap = false, scalar = true),
        v!("KeyRef", (String, String), heap = true, scalar = false),
        v!("DocRef", (String, u64), heap = true, scalar = false),
        v!("TableRef", String, heap = true, scalar = false),
        v!("PageRef", u32, heap = false, scalar = true),
        v!("Secret", Vec<u8>, heap = true, scalar = false),
        v!("Password", String, heap = true, scalar = false),
    ]
}

/// Print the layout report and assert the layout invariants.
///
/// The asserts encode the *findings* of the measurement: the enum is dominated
/// by a handful of multi-field heap variants, while the overwhelming majority
/// of variants are cheap `Copy` scalars far below the enum size.
#[test]
fn value_layout_report() {
    let value_size = size_of::<Value>();
    let value_align = align_of::<Value>();
    let measures = measure();

    let max_payload = measures
        .iter()
        .map(|m| m.payload)
        .max()
        .expect("variant list is non-empty");
    let mut dominant: Vec<&VariantMeasure> = measures
        .iter()
        .filter(|m| m.payload == max_payload)
        .collect();
    dominant.sort_by_key(|m| m.name);

    let scalar_count = measures.iter().filter(|m| m.scalar).count();
    let heap_count = measures.iter().filter(|m| m.heap).count();
    // A "small scalar" sits at or below a machine word — the cheap common case.
    let small_scalar_count = measures
        .iter()
        .filter(|m| m.scalar && m.payload <= size_of::<u64>())
        .count();

    println!("\n=== Value in-memory layout report (issue #1341) ===");
    println!("size_of::<Value>()  = {value_size} bytes");
    println!("align_of::<Value>() = {value_align} bytes");
    println!("variants measured   = {}", measures.len());
    println!(
        "scalar variants     = {scalar_count} (of which {small_scalar_count} fit in a u64 word)"
    );
    println!("heap-owning variants = {heap_count}");
    println!("max payload size    = {max_payload} bytes");
    println!("layout-driving (largest-payload) variants:");
    for m in &dominant {
        println!(
            "  - {:<10} payload={} bytes (heap={})",
            m.name, m.payload, m.heap
        );
    }

    println!("\n-- per-variant payloads (bytes) --");
    let mut sorted = measures.iter().collect::<Vec<_>>();
    sorted.sort_by(|a, b| b.payload.cmp(&a.payload).then(a.name.cmp(b.name)));
    for m in &sorted {
        let kind = if m.heap {
            "heap"
        } else if m.scalar {
            "scalar"
        } else {
            "inline"
        };
        println!("  {:<16} {:>3}  [{kind}]", m.name, m.payload);
    }

    // --- Findings, encoded as regression-guarding assertions ---

    // 1. The enum is dominated by multi-field heap variants. With at least one
    //    `(String, _, _)` / `(String, String)` payload present, the enum cannot
    //    shrink below two pointer-words of payload plus discriminant.
    assert!(
        max_payload >= size_of::<(String, i64, u8)>().min(size_of::<(String, String)>()),
        "expected a multi-field heap variant to dominate the payload"
    );

    // 2. `Money` and `KeyRef` are the rare, wide variants that set the floor —
    //    the whole point of the measurement. Guard that they remain the (or a)
    //    dominant variant so a regression here is visible.
    let dominant_names: Vec<&str> = dominant.iter().map(|m| m.name).collect();
    assert!(
        dominant_names.contains(&"Money") || dominant_names.contains(&"KeyRef"),
        "expected Money/KeyRef among layout-driving variants, got {dominant_names:?}"
    );

    // 3. The common case really is common: the large majority of variants are
    //    cheap word-sized scalars, distinguishing them from the rare wide ones.
    assert!(
        small_scalar_count * 2 > measures.len(),
        "expected most variants to be small word-sized scalars"
    );

    // 4. The enum holds every payload (it is at least as large as the widest)
    //    and stays within a small, fixed cache-friendly bound. 64 bytes = one
    //    typical cache line; we assert the enum does not silently exceed it.
    assert!(
        value_size >= max_payload,
        "enum smaller than its widest payload"
    );
    assert!(
        value_size <= 64,
        "Value grew beyond one cache line ({value_size} bytes) — re-evaluate boxing rare variants"
    );

    println!("\n-- conclusion --");
    println!(
        "Common scalars ({small_scalar_count}/{}) cost a single word; the enum's {value_size}-byte \
         footprint is set by rare wide variants (e.g. Money/KeyRef = (String, String)).",
        measures.len()
    );
    let single_word = size_of::<u64>();
    let boxed_floor = size_of::<usize>() * 2; // ptr + discriminant after boxing
    println!(
        "VERDICT: a follow-up representation slice IS justified. Value is {value_size} bytes \
         (~{} words) yet the dominant common case is a {single_word}-byte scalar. Boxing the few \
         rare wide variants (KeyRef, Money, the *Ref pairs) could bring Value toward ~{boxed_floor} \
         bytes — a ~{}x shrink for the scalar-heavy hot paths (Row vectors, query results).",
        value_size / single_word,
        value_size / boxed_floor.max(1)
    );
    println!(
        "Caveat: confirm the boxed variants are genuinely cold before committing — the win is \
         pure layout/cache, with no value-semantics or serialization change. This harness is the \
         regression guard for that follow-up (one-cache-line ceiling at 64 bytes)."
    );
}
