//! Reports the in-memory layout of [`Value`] and identifies which variants
//! drive the enum size.
//!
//! Run with:
//! ```text
//! cargo run -p reddb-io-types --bin measure_value_layout
//! ```
//!
//! This binary is a pure measurement harness — no I/O, no side effects, no
//! changes to value semantics.

use std::mem::{align_of, size_of};
use std::net::IpAddr;
use std::sync::Arc;

use reddb_types::Value;

// Mirror of the Money struct variant fields — used only to measure layout.
// Must stay in sync with `Value::Money { asset_code, minor_units, scale }`.
#[allow(dead_code)]
struct MoneyLayout {
    asset_code: String,
    minor_units: i64,
    scale: u8,
}

fn main() {
    print_report();
}

fn print_report() {
    let enum_size = size_of::<Value>();
    let enum_align = align_of::<Value>();

    // Each entry: (name, category, inner_payload_bytes)
    //   inner_payload_bytes = size of the variant's data fields in isolation,
    //   NOT including the enum discriminant or any enum-level padding.
    //   The true enum size = discriminant_slot + max(inner_payload_bytes),
    //   all rounded up to enum_align.
    let variants: &[(&str, &str, usize)] = &[
        // unit / zero
        ("Null", "zero", 0),
        // compact scalars — ≤ 1 byte payload
        ("Boolean", "scalar", size_of::<bool>()),
        ("EnumValue", "scalar", size_of::<u8>()),
        // compact scalars — 2 bytes
        ("Country2", "scalar", size_of::<[u8; 2]>()),
        ("Lang2", "scalar", size_of::<[u8; 2]>()),
        ("Port", "scalar", size_of::<u16>()),
        // compact scalars — 3 bytes
        ("Color", "scalar", size_of::<[u8; 3]>()),
        ("Country3", "scalar", size_of::<[u8; 3]>()),
        ("Currency", "scalar", size_of::<[u8; 3]>()),
        // compact scalars — 4 bytes
        ("ColorAlpha", "scalar", size_of::<[u8; 4]>()),
        ("Date", "scalar", size_of::<i32>()),
        ("Ipv4", "scalar", size_of::<u32>()),
        ("Latitude", "scalar", size_of::<i32>()),
        ("Longitude", "scalar", size_of::<i32>()),
        ("PageRef", "scalar", size_of::<u32>()),
        ("Semver", "scalar", size_of::<u32>()),
        ("Time", "scalar", size_of::<u32>()),
        // compact scalars — 5 bytes
        ("Lang5", "scalar", size_of::<[u8; 5]>()),
        // compact scalars — 6 bytes
        ("MacAddr", "scalar", size_of::<[u8; 6]>()),
        // compact scalars — 8 bytes (word-size)
        ("BigInt", "scalar", size_of::<i64>()),
        ("Cidr", "scalar", size_of::<(u32, u8)>()),
        ("Decimal", "scalar", size_of::<i64>()),
        ("Duration", "scalar", size_of::<i64>()),
        ("Float", "scalar", size_of::<f64>()),
        ("GeoPoint", "scalar", size_of::<(i32, i32)>()),
        ("Integer", "scalar", size_of::<i64>()),
        ("Phone", "scalar", size_of::<u64>()),
        ("Subnet", "scalar", size_of::<(u32, u32)>()),
        ("Timestamp", "scalar", size_of::<i64>()),
        ("TimestampMs", "scalar", size_of::<i64>()),
        ("UnsignedInt", "scalar", size_of::<u64>()),
        // 16-byte scalars (inline arrays / arc-ptr)
        ("IpAddr", "scalar", size_of::<IpAddr>()),
        ("Ipv6", "scalar", size_of::<[u8; 16]>()),
        ("Text", "arc-ptr", size_of::<Arc<str>>()),
        ("Uuid", "scalar", size_of::<[u8; 16]>()),
        // heap-allocating variants — single String (24 bytes on x86-64)
        ("AssetCode", "heap", size_of::<String>()),
        ("EdgeRef", "heap", size_of::<String>()),
        ("Email", "heap", size_of::<String>()),
        ("NodeRef", "heap", size_of::<String>()),
        ("Password", "heap", size_of::<String>()),
        ("TableRef", "heap", size_of::<String>()),
        ("Url", "heap", size_of::<String>()),
        // heap-allocating variants — Vec (24 bytes header on x86-64)
        ("Array", "heap", size_of::<Vec<Value>>()),
        ("Blob", "heap", size_of::<Vec<u8>>()),
        ("Json", "heap", size_of::<Vec<u8>>()),
        ("Secret", "heap", size_of::<Vec<u8>>()),
        ("Vector", "heap", size_of::<Vec<f32>>()),
        // heap-allocating variants — String + scalar (32 bytes on x86-64)
        ("DocRef", "heap", size_of::<String>() + size_of::<u64>()),
        ("RowRef", "heap", size_of::<String>() + size_of::<u64>()),
        ("VectorRef", "heap", size_of::<String>() + size_of::<u64>()),
        // fat variants — struct or two Strings
        ("Money", "heap", size_of::<MoneyLayout>()),
        ("KeyRef", "heap", size_of::<(String, String)>()),
    ];

    let max_payload = variants.iter().map(|e| e.2).max().unwrap_or(0);

    println!();
    println!("┌──────────────────────────────────────────────────────────────┐");
    println!("│              RedDB  Value  Layout  Measurement               │");
    println!("└──────────────────────────────────────────────────────────────┘");
    println!();
    println!("  arch            : {}", std::env::consts::ARCH);
    println!("  size_of<Value>  = {} bytes", enum_size);
    println!("  align_of<Value> = {} bytes", enum_align);
    println!(
        "  largest payload = {} bytes  ← sets the enum floor",
        max_payload
    );

    let overhead = enum_size.saturating_sub(max_payload);
    println!(
        "  discriminant slot = {} bytes  (enum_size − max_payload)",
        overhead
    );

    let scalar_payload = size_of::<i64>(); // Integer / Float — most common value
    let waste_per_scalar = enum_size.saturating_sub(scalar_payload);
    println!(
        "  waste/scalar    = {} bytes  (enum_size − {}B Integer/Float payload)",
        waste_per_scalar, scalar_payload
    );
    println!();

    // Sort descending by payload size for easy scanning
    let mut sorted = variants.to_vec();
    sorted.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(b.0)));

    const BAR_WIDTH: usize = 20;
    println!(
        "  {:<14}  {:>8}  {:<8}  Bar (relative)",
        "Variant", "Payload", "Category"
    );
    println!(
        "  {}  {}  {}  {}",
        "─".repeat(14),
        "─".repeat(8),
        "─".repeat(8),
        "─".repeat(BAR_WIDTH)
    );

    for (name, cat, size) in &sorted {
        let bar_len = (size * BAR_WIDTH).checked_div(max_payload).unwrap_or(0);
        let bar: String = "█".repeat(bar_len);
        let note = if *size == max_payload {
            " ← drives enum size"
        } else {
            ""
        };
        println!(
            "  {:<14}  {:>5} B   {:<8}  {}{}",
            name, size, cat, bar, note
        );
    }

    println!();
    println!("──────────────────────────────────────────────────────────────────");
    println!("Verdict");
    println!("──────────────────────────────────────────────────────────────────");

    // The threshold for justifying a boxing/splitting follow-up:
    // waste ≥ 24 bytes per common scalar means every Integer/Float carries
    // an extra cache line of dead space.
    let justified = waste_per_scalar >= 24;

    if justified {
        println!();
        println!("  FOLLOW-UP SLICE JUSTIFIED");
        println!();
        println!(
            "  Value is {} B, but common scalars (Integer, Float, Timestamp) carry",
            enum_size
        );
        println!(
            "  only {} B of payload — {} B wasted per value.",
            scalar_payload, waste_per_scalar
        );
        println!();
        println!("  Fat variants driving the size:");
        for (name, _cat, size) in &sorted {
            if *size >= 32 {
                println!("    {:<14}  {} B", name, size);
            }
        }
        println!();
        println!("  Recommendation: box fat variants (KeyRef, Money, Vector,");
        println!(
            "  Array, Secret) behind a pointer.  Expected: {} → ≤24 B",
            enum_size
        );
        println!("  per common scalar, removing the excess cache-line pressure.");
    } else {
        println!();
        println!("  FOLLOW-UP SLICE NOT JUSTIFIED");
        println!();
        println!(
            "  The {} B enum wastes {} B per scalar — within one pointer width.",
            enum_size, waste_per_scalar
        );
        println!("  Revisit if profiling reveals Value as a hot allocation path.");
    }

    println!();
}
