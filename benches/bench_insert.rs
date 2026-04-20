//! Quick Rust-native benchmark for persistent bulk insert.
//!
//! Run with: `cargo bench --bench bench_insert`

use std::path::PathBuf;
use std::time::Instant;

use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

const N: usize = 100_000;

fn main() {
    let path = PathBuf::from("/tmp/reddb_bench_insert");
    let _ = std::fs::remove_dir_all(&path);

    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
    let store = rt.db().store();
    let _ = store.create_collection("bulk");

    // Build entities
    let mut entities = Vec::with_capacity(N);
    for i in 0..N {
        let mut named = std::collections::HashMap::new();
        named.insert("name".into(), Value::text(format!("User_{i}")));
        named.insert("age".into(), Value::Integer((i % 80) as i64));
        named.insert("city".into(), Value::Text("NYC".into()));
        named.insert("score".into(), Value::Float((i as f64) / 100.0));
        entities.push(reddb::storage::UnifiedEntity::new(
            reddb::storage::EntityId::new(0),
            reddb::storage::EntityKind::TableRow {
                table: std::sync::Arc::from("bulk"),
                row_id: 0,
            },
            reddb::storage::EntityData::Row(reddb::storage::RowData {
                columns: Vec::new(),
                named: Some(named),
                schema: None,
            }),
        ));
    }

    println!("bulk_insert {N} rows (persistent, B-tree)...");
    let t0 = Instant::now();
    store.bulk_insert("bulk", entities).unwrap();
    let elapsed = t0.elapsed();
    let ops = (N as f64) / elapsed.as_secs_f64();
    println!("bulk_insert     : {:>7.0} ops/sec  ({:.2?})", ops, elapsed);

    // Disk usage (reddb creates file + -dwb + -hdr sidecars)
    let size: u64 = walkdir(&path)
        + file_size(&path)
        + file_size(&path.with_extension("-dwb"))
        + file_size(&path.with_extension("-hdr"))
        + file_size(&path.with_file_name(format!(
            "{}-dwb",
            path.file_name().unwrap().to_string_lossy()
        )))
        + file_size(&path.with_file_name(format!(
            "{}-hdr",
            path.file_name().unwrap().to_string_lossy()
        )));
    println!(
        "Disk usage      : {:.1} MB  ({} bytes/row)",
        (size as f64) / 1024.0 / 1024.0,
        size / (N as u64)
    );

    // Verify: fetch a few entities
    for id in [1u64, 100, 1_000, 50_000, N as u64] {
        let e = store.get("bulk", reddb::storage::EntityId::new(id));
        println!("  id={id:<6}: {}", if e.is_some() { "ok" } else { "MISS" });
    }
}

fn file_size(p: &std::path::Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

fn walkdir(p: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(p) {
        for e in entries.flatten() {
            let meta = match e.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                total += walkdir(&e.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}
