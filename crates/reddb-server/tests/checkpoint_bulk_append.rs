//! A small ingest batch beside a pending writer must not scan old payloads.
use reddb_server::runtime::mvcc::set_current_connection_id;
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

#[test]
fn small_bulk_append_with_pending_writer_keeps_checkpoint_bounded() {
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("bulk-append.rdb");
    let options = || {
        RedDBOptions::persistent(&path)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile")
    };
    let runtime = RedDBRuntime::with_options(options()).expect("runtime");
    for query in [
        "SET CONFIG runtime.mvcc.autovacuum.enabled = false",
        "CREATE TABLE ingest_payload (id INT, n INT, payload TEXT)",
    ] {
        runtime.execute_query(query).expect("schema");
    }
    let mut seed = 0x7b3d_5961_a902_4c85_u64;
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789.-";
    for batch in 0..20 {
        let rows = (batch * 100..(batch + 1) * 100)
            .map(|id| {
                let payload: String = (0..4096)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        char::from(alphabet[(seed & 63) as usize])
                    })
                    .collect();
                format!("({id},0,'{payload}')")
            })
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!(
                "INSERT INTO ingest_payload (id,n,payload) VALUES {rows}"
            ))
            .expect("payload history");
    }
    runtime.checkpoint().expect("baseline");
    // Retirement retains the preceding published generation for one more
    // checkpoint. Drain fixture construction before measuring the new batch.
    runtime.checkpoint().expect("retire baseline generation");
    set_current_connection_id(9364);
    runtime.execute_query("BEGIN").expect("pending writer");
    runtime
        .execute_query("UPDATE ingest_payload SET n=999 WHERE id=1999")
        .expect("pending update");
    set_current_connection_id(9365);
    runtime
        .execute_query(
            "INSERT INTO ingest_payload (id,n,payload) VALUES (2000,7,'new'),(2001,8,'new')",
        )
        .expect("committed micro batch");
    let pager = runtime.db().store().pager().expect("pager").clone();
    let before = pager.cache_stats();
    // Measure the paged checkpoint itself. Runtime metadata publication has
    // additional catalog work independent of sparse row projection.
    runtime
        .db()
        .store()
        .persist()
        .expect("checkpoint beside writer");
    let after = pager.cache_stats();
    let reads = after.hits + after.misses - before.hits - before.misses;
    eprintln!("small bulk append checkpoint page reads: {reads}");
    runtime.checkpoint().expect("publish runtime metadata");
    set_current_connection_id(9364);
    runtime.execute_query("ROLLBACK").expect("rollback");
    set_current_connection_id(0);
    drop(pager);
    drop(runtime);
    let recovered = RedDBRuntime::with_options(options()).expect("cold reopen");
    for (id, expected) in [(1999, "0"), (2000, "7"), (2001, "8")] {
        let result = recovered
            .execute_query(&format!("SELECT n FROM ingest_payload WHERE id={id}"))
            .expect("read recovered row");
        assert_eq!(result.result.records.len(), 1);
        assert_eq!(
            result.result.records[0].get("n").expect("n").to_string(),
            expected
        );
    }
    assert!(reads < 512, "small append scanned payload history: {reads}");
}
