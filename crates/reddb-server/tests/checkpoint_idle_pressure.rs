//! Checkpoint work after ordinary failed statements and accumulated background writes.
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

#[test]
fn failed_unrelated_autocommit_does_not_rescan_payload_history() {
    qualify_idle_pressure(true, false, false);
}

#[test]
fn sealing_a_segment_does_not_rescan_payload_history() {
    qualify_idle_pressure(false, true, false);
}

#[test]
fn sealing_does_not_conceal_an_unlogged_manager_edit() {
    qualify_idle_pressure(false, true, true);
}

fn published_root(runtime: &RedDBRuntime) -> u32 {
    let page = runtime
        .db()
        .store()
        .pager()
        .expect("pager")
        .read_manifest_page()
        .expect("manifest");
    let data = &page.as_bytes()[reddb_server::storage::engine::HEADER_SIZE..];
    let header = reddb_file::decode_native_paged_metadata_header(data)
        .expect("metadata")
        .expect("paged metadata");
    let mut position = reddb_file::METADATA_HEADER_BYTES;
    for _ in 0..header.collection_count {
        let entry = reddb_file::decode_native_paged_collection_root(data, &mut position)
            .expect("collection root");
        if entry.collection == "idle_payload" {
            return entry.root_page;
        }
    }
    panic!("missing payload tree");
}

fn qualify_idle_pressure(reject_unrelated: bool, seal_payload: bool, unlogged_update: bool) {
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("idle.rdb");
    let options = || {
        RedDBOptions::persistent(&path)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile")
    };
    let runtime = RedDBRuntime::with_options(options()).expect("runtime");
    for query in [
        "SET CONFIG runtime.mvcc.autovacuum.enabled = false",
        "CREATE TABLE idle_payload (id INT,n INT,payload TEXT)",
        "CREATE UNIQUE INDEX idle_payload_id ON idle_payload (id)",
        "CREATE TABLE idle_noise (id INT,n INT)",
        "CREATE UNIQUE INDEX idle_noise_id ON idle_noise (id)",
        "INSERT INTO idle_noise (id,n) VALUES (1,0)",
    ] {
        runtime.execute_query(query).expect("schema");
    }
    let payload = "p".repeat(4096);
    for batch in 0..10 {
        let rows = (batch * 100..(batch + 1) * 100)
            .map(|id| format!("({id},0,'{payload}')"))
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!(
                "INSERT INTO idle_payload (id,n,payload) VALUES {rows}"
            ))
            .expect("seed history");
    }
    runtime.checkpoint().expect("baseline");
    let root_before = published_root(&runtime);
    if reject_unrelated {
        assert!(
            runtime
                .execute_query("INSERT INTO idle_noise (id,n) VALUES (1,99)")
                .is_err(),
            "duplicate must fail"
        );
    }
    if seal_payload {
        runtime
            .db()
            .store()
            .get_collection("idle_payload")
            .expect("payload collection")
            .force_seal()
            .expect("seal growing segment");
    }
    if unlogged_update {
        let manager = runtime
            .db()
            .store()
            .get_collection("idle_payload")
            .expect("collection");
        let mut edited = None;
        manager.for_each_entity(|entity| {
            if entity.data.as_row().expect("row").columns[0].to_string() == "3" {
                edited = Some(entity.clone());
                return false;
            }
            true
        });
        let mut entity = edited.expect("row to edit");
        let reddb_server::storage::EntityData::Row(row) = &mut entity.data else {
            panic!("table row expected");
        };
        row.columns[1] = reddb_types::Value::Integer(88);
        manager.update(entity).expect("unlogged manager edit");
    }
    runtime
        .execute_query("UPDATE idle_payload SET n=77 WHERE id=2")
        .expect("acknowledged update");
    let pager = runtime.db().store().pager().expect("pager").clone();
    let before = pager.cache_stats();
    runtime
        .checkpoint()
        .expect("checkpoint after rejected statement");
    let after = pager.cache_stats();
    let reads = after.hits + after.misses - before.hits - before.misses;
    eprintln!("checkpoint: rejected_unrelated={reject_unrelated}, sealed={seal_payload}, page_reads={reads}");
    if !unlogged_update {
        assert_eq!(
            published_root(&runtime),
            root_before,
            "sealing rebuilt the payload tree"
        );
        assert!(reads < 128, "checkpoint rescanned payload history: rejected_unrelated={reject_unrelated}, sealed={seal_payload}, page_reads={reads}");
    }
    drop(pager);
    drop(runtime);
    let recovered = RedDBRuntime::with_options(options()).expect("cold reopen");
    for (query, expected) in [
        ("SELECT n FROM idle_payload WHERE id=2", "77"),
        ("SELECT n FROM idle_noise WHERE id=1", "0"),
        (
            "SELECT n FROM idle_payload WHERE id=3",
            if unlogged_update { "88" } else { "0" },
        ),
    ] {
        let result = recovered.execute_query(query).expect("durable readback");
        assert_eq!(result.result.records.len(), 1);
        assert_eq!(
            result.result.records[0].get("n").expect("n").to_string(),
            expected
        );
    }
}
