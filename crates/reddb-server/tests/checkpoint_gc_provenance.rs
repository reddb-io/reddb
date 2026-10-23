//! Already-paged GC removals must not erase provenance for a pending durable update.
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

#[test]
fn physical_gc_pressure_preserves_unprojected_wal_updates() {
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("gc-provenance.rdb");
    let options = || {
        RedDBOptions::persistent(&path)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile")
    };
    let runtime = RedDBRuntime::with_options(options()).expect("runtime");
    for query in [
        "SET CONFIG runtime.mvcc.autovacuum.enabled = false",
        "CREATE TABLE gc_payload (id INT,n INT,payload TEXT)",
    ] {
        runtime.execute_query(query).expect("schema");
    }
    for batch in 0..10 {
        let values = (batch * 1000..(batch + 1) * 1000)
            .map(|id| format!("({id},0,'bounded checkpoint provenance')"))
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!(
                "INSERT INTO gc_payload (id,n,payload) VALUES {values}"
            ))
            .expect("seed rows");
    }
    runtime.checkpoint().expect("baseline checkpoint");
    let manager = runtime
        .db()
        .store()
        .get_collection("gc_payload")
        .expect("collection");
    let mut deleted = Vec::new();
    manager.for_each_entity(|entity| {
        let row = entity.data.as_row().expect("table row");
        let id = row.columns[0]
            .to_string()
            .parse::<u64>()
            .expect("logical ID");
        if id < 9000 {
            deleted.push(entity.id);
        }
        true
    });
    assert_eq!(deleted.len(), 9000);
    deleted.sort_unstable();
    let changed = runtime
        .execute_query("UPDATE gc_payload SET n=77 WHERE id=9999")
        .expect("durable update pending projection");
    assert_eq!(changed.affected_rows, 1);
    // This is the same bounded physical-delete path used by paced MVCC GC.
    for batch in deleted.chunks(256) {
        assert_eq!(
            runtime
                .db()
                .store()
                .delete_batch("gc_payload", batch)
                .expect("GC batch"),
            batch
        );
    }
    let root = || {
        let page = runtime
            .db()
            .store()
            .pager()
            .expect("pager")
            .read_manifest_page()
            .expect("manifest");
        let data = &page.as_bytes()[reddb_server::storage::engine::HEADER_SIZE..];
        let header = reddb_file::decode_native_paged_metadata_header(data)
            .expect("header")
            .expect("paged metadata");
        let mut position = reddb_file::METADATA_HEADER_BYTES;
        for _ in 0..header.collection_count {
            let entry = reddb_file::decode_native_paged_collection_root(data, &mut position)
                .expect("collection root");
            if entry.collection == "gc_payload" {
                return entry.root_page;
            }
        }
        panic!("missing collection root");
    };
    let projected_root = root();
    runtime.checkpoint().expect("checkpoint after GC pressure");
    assert_eq!(root(), projected_root, "already-projected physical deletions exhausted the provenance of an earlier durable update");
    drop(manager);
    drop(runtime);
    let recovered = RedDBRuntime::with_options(options()).expect("cold reopen");
    let result = recovered
        .execute_query("SELECT n FROM gc_payload WHERE id=9999")
        .expect("committed row survives");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0].get("n").expect("n").to_string(),
        "77"
    );
    for id in deleted {
        assert!(
            recovered.db().store().get("gc_payload", id).is_none(),
            "deleted physical row returned after recovery"
        );
    }
}
