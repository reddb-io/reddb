//! Native reference for issue #2270. Run with an optimized build, outside other benchmarks.
//! cargo run --release --example competitive_insert -- /tmp/audit.rdb 200 1
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Barrier};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .ok_or("usage: competitive_insert PATH ITEMS WRITERS")?;
    if std::path::Path::new(path).exists() {
        return Err("database path must be fresh".into());
    }
    let items: usize = args.get(2).map(String::as_str).unwrap_or("200").parse()?;
    let writers: usize = args.get(3).map(String::as_str).unwrap_or("1").parse()?;
    if items == 0 || items > 1_000_000 || writers == 0 || writers > 16 {
        return Err("items must be 1..=1000000 and writers 1..=16".into());
    }
    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::persistent(path))?);
    runtime.execute_query("CREATE TABLE t (id TEXT PRIMARY KEY, payload TEXT)")?;
    let payload = format!(r#"{{"type":"text","text":"{}"}}"#, "x".repeat(400));
    let barrier = Arc::new(Barrier::new(writers + 1));
    let mut handles = Vec::new();
    for writer in 0..writers {
        let runtime = Arc::clone(&runtime);
        let barrier = Arc::clone(&barrier);
        let payload = payload.clone();
        handles.push(std::thread::spawn(move || -> Result<Vec<u128>, String> {
            reddb::runtime::mvcc::set_current_connection_id(2_270_000 + writer as u64);
            let mut samples = Vec::new();
            barrier.wait();
            for index in (writer..items).step_by(writers) {
                let params = [
                    Value::text(format!("k{index}")),
                    Value::text(payload.clone()),
                ];
                let start = Instant::now();
                runtime
                    .execute_query_with_params(
                        "INSERT INTO t (id, payload) VALUES ($1, $2)",
                        &params,
                    )
                    .map_err(|error| error.to_string())?;
                samples.push(start.elapsed().as_nanos());
            }
            reddb::runtime::mvcc::clear_current_connection_id();
            Ok(samples)
        }));
    }
    let start = Instant::now();
    barrier.wait();
    let mut samples = Vec::new();
    for handle in handles {
        samples.extend(handle.join().map_err(|_| "writer panicked")??);
    }
    let elapsed_ns = start.elapsed().as_nanos();
    let verify = |runtime: &RedDBRuntime| -> Result<(), Box<dyn std::error::Error>> {
        let result = runtime.execute_query("SELECT id, payload FROM t")?;
        let mut actual = BTreeMap::new();
        for row in &result.result.records {
            let Some(Value::Text(id)) = row.get("id") else {
                return Err("missing id".into());
            };
            let Some(Value::Text(value)) = row.get("payload") else {
                return Err("missing payload".into());
            };
            if actual.insert(id.to_string(), value.to_string()).is_some() {
                return Err("duplicate id".into());
            }
        }
        if actual.len() != items {
            return Err("wrong row count".into());
        }
        for index in 0..items {
            if actual.get(&format!("k{index}")) != Some(&payload) {
                return Err("readback mismatch".into());
            }
        }
        Ok(())
    };
    verify(&runtime)?;
    runtime.checkpoint()?;
    drop(runtime);
    let reopened = RedDBRuntime::with_options(RedDBOptions::persistent(path))?;
    verify(&reopened)?;
    println!(
        "{}",
        serde_json::json!({
            "schema": "competitive-native-v1", "engine": "reddb", "execution": "rust-in-process",
            "version": env!("CARGO_PKG_VERSION"), "items": items, "writers": writers,
            "elapsed_ns": elapsed_ns, "latency_ns": samples, "valid": true,
            "readback": "all ids and payloads", "reopen": "all ids and payloads after checkpoint"
        })
    );
    Ok(())
}
