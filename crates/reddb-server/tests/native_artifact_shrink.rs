//! Ordinary table churn must publish exact native index blobs after VACUUM.
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

fn fulltext_artifact(runtime: &RedDBRuntime) -> (u32, Vec<u8>) {
    let store = runtime.db().store();
    let header = store.physical_file_header().expect("native header");
    let (summary, bytes) = store
        .read_native_vector_artifact_blob(
            header.vector_artifact_page,
            "artifact_rows",
            Some("text.fulltext"),
        )
        .expect("native artifact read")
        .expect("full-text artifact");
    assert_eq!(summary.byte_len as usize, bytes.len(), "stale blob tail");
    assert_eq!(summary.checksum, crc32fast::hash(&bytes) as u64);
    (summary.page_count, bytes)
}

#[test]
fn vacuum_shrinks_native_fulltext_artifact_and_cold_reopens() {
    qualify_shrink(false);
}

#[test]
fn reopened_paged_default_profile_publishes_shrunk_native_artifacts() {
    qualify_shrink(true);
}

fn qualify_shrink(reopen_with_default_profile: bool) {
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("native-shrink.rdb");
    let options = || {
        RedDBOptions::persistent(&path)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile")
    };
    let runtime = RedDBRuntime::with_options(options()).expect("runtime");
    runtime
        .execute_query("SET CONFIG runtime.mvcc.autovacuum.enabled = false")
        .expect("deterministic explicit vacuum");
    runtime
        .execute_query("CREATE TABLE artifact_rows (id INT, payload TEXT)")
        .expect("table");
    let payload = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
    // Compressed postings need a larger corpus to keep exercising physical
    // tail retirement, rather than shrinking entirely within one page.
    for batch in 0..64 {
        let rows = (batch * 128..(batch + 1) * 128)
            .map(|id| format!("({id},'{payload}')"))
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!(
                "INSERT INTO artifact_rows (id,payload) VALUES {rows}"
            ))
            .expect("seed indexed rows");
    }
    runtime.checkpoint().expect("large native artifact");
    let (pages_before, _) = fulltext_artifact(&runtime);
    assert!(pages_before > 1, "fixture must span multiple native pages");

    let runtime = if reopen_with_default_profile {
        drop(runtime);
        RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("existing paged file under the default embedded options")
    } else {
        runtime
    };

    runtime
        .execute_query("DELETE FROM artifact_rows WHERE id >= 8")
        .expect("remove indexed rows");
    runtime
        .execute_query("VACUUM artifact_rows")
        .expect("reclaim physical history");
    runtime.checkpoint().expect("smaller native artifact");
    let (pages_after, expected_artifact) = fulltext_artifact(&runtime);
    assert!(pages_after < pages_before);
    drop(runtime);

    let recovered = RedDBRuntime::with_options(if reopen_with_default_profile {
        RedDBOptions::persistent(&path)
    } else {
        options()
    })
    .expect("cold reopen");
    assert_eq!(fulltext_artifact(&recovered).1, expected_artifact);
    let rows = recovered
        .execute_query("SELECT id,payload FROM artifact_rows ORDER BY id")
        .expect("current rows after reopen")
        .result
        .records;
    assert_eq!(rows.len(), 8);
    for (id, row) in rows.iter().enumerate() {
        assert_eq!(row.get("id").expect("id").to_string(), id.to_string());
        assert_eq!(
            row.get("payload").expect("payload").as_text(),
            Some(payload)
        );
    }
}
