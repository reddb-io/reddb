use reddb_server::storage::compact::compact_paged;
use reddb_server::storage::engine::{BTree, Page, PageType, Pager, PagerConfig, HEADER_SIZE};
use std::sync::Arc;

fn payload(id: u64) -> Vec<u8> {
    if !id.is_multiple_of(100) {
        return vec![(id % 251) as u8; 800];
    }
    let mut state = id + 1;
    (0..48_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn fixture(path: &std::path::Path) {
    let pager = Arc::new(Pager::open_default(path).expect("new pager"));
    // Reproduce abandoned allocations below the live root, not just a free tail.
    for _ in 0..512 {
        let page = pager
            .allocate_page(PageType::Overflow)
            .expect("abandoned page");
        pager
            .write_page(page.page_id(), page)
            .expect("write abandoned page");
    }
    let tree = BTree::new(Arc::clone(&pager));
    for id in 0u64..1000 {
        tree.insert(&id.to_be_bytes(), &payload(id))
            .expect("insert");
    }
    let mut metadata = Vec::new();
    reddb_file::encode_native_paged_metadata_header(
        &mut metadata,
        reddb_file::NativePagedMetadataHeader {
            format_version: 11,
            collection_count: 1,
        },
    );
    reddb_file::encode_native_paged_collection_root(&mut metadata, "rows", tree.root_page_id());
    metadata.extend_from_slice(&0u32.to_le_bytes());
    let mut page = Page::new(PageType::Header, 1);
    page.as_bytes_mut()[HEADER_SIZE..HEADER_SIZE + metadata.len()].copy_from_slice(&metadata);
    pager.write_page(1, page).expect("catalog");
    pager.sync().expect("checkpoint");
}

#[test]
fn compact_rewrites_live_records_and_preserves_source() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let before = std::fs::read(&source).expect("source image");
    let destination = directory.path().join("compacted");
    let report = compact_paged(&source, &destination).expect("compact");
    assert!(report.destination_bytes < report.source_bytes / 2);
    assert_eq!(report.records, 1000);
    assert_eq!(std::fs::read(&source).expect("preserved image"), before);
    let pager = Arc::new(
        Pager::open(
            destination.join("source.rdb"),
            PagerConfig {
                read_only: true,
                create: false,
                ..PagerConfig::default()
            },
        )
        .expect("reopen"),
    );
    let page = pager.read_page(1).expect("catalog");
    let mut offset = reddb_file::METADATA_HEADER_BYTES;
    let root =
        reddb_file::decode_native_paged_collection_root(page.content(), &mut offset).expect("root");
    let tree = BTree::with_root(pager, root.root_page);
    for id in 0u64..1000 {
        assert_eq!(
            tree.get(&id.to_be_bytes()).expect("lookup"),
            Some(payload(id))
        );
    }
}

#[test]
fn compact_refuses_busy_source_and_existing_destination() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let destination = directory.path().join("compacted");
    let pager = Pager::open_default(&source).expect("writer");
    assert!(compact_paged(&source, &destination).is_err());
    assert!(!destination.exists());
    drop(pager);
    std::fs::create_dir(&destination).expect("existing output");
    assert!(compact_paged(&source, &destination).is_err());
}

#[test]
fn compact_preserves_chained_authenticated_vault_and_native_blob() {
    use reddb_server::auth::vault::{KeyPair, Vault, VaultState};
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let pager = Pager::open_default(&source).expect("pager");
    let certificate = KeyPair::generate().certificate_hex();
    let vault = Vault::with_certificate(&pager, &certificate).expect("vault");
    let mut state = VaultState {
        bootstrapped: true,
        ..VaultState::default()
    };
    state
        .kv
        .insert("red.secret.large".into(), "private-secret".repeat(9000));
    vault.save(&pager, &state).expect("save chained vault");
    let payload = vec![0x73; 5000];
    let mut blob = pager.allocate_page(PageType::NativeMeta).expect("blob");
    let encoded = reddb_file::encode_native_blob_page(0, &payload);
    blob.content_mut()[..encoded.len()].copy_from_slice(&encoded);
    let root = blob.page_id();
    pager.write_page(root, blob).expect("blob page");
    let summary = reddb_file::NativeVectorArtifactPageSummary {
        collection: "rows".into(),
        artifact_kind: "test-opaque".into(),
        root_page: root,
        page_count: 1,
        byte_len: payload.len() as u64,
        checksum: reddb_file::native_store_page_checksum(&payload),
    };
    let encoded = reddb_file::encode_native_vector_artifact_store_page(&[summary]);
    let mut summary = pager
        .allocate_page(PageType::NativeMeta)
        .expect("artifact summary");
    summary.content_mut()[..encoded.len()].copy_from_slice(&encoded);
    let mut physical = pager.physical_header().expect("physical");
    physical.vector_artifact_page = summary.page_id();
    physical.vector_artifact_checksum = reddb_file::native_store_page_checksum(&encoded);
    pager
        .write_page(summary.page_id(), summary)
        .expect("write summary");
    pager
        .update_physical_header(physical)
        .expect("publish physical");
    pager.sync().expect("sync");
    drop(pager);
    let destination = directory.path().join("compacted");
    compact_paged(&source, &destination).expect("compact");
    let pager = Pager::open_default(destination.join("source.rdb")).expect("reopen");
    let restored = Vault::with_certificate(&pager, &certificate)
        .expect("original certificate")
        .load(&pager)
        .expect("authenticated decrypt")
        .expect("vault state");
    assert!(restored.bootstrapped);
    assert_eq!(restored.kv, state.kv);
    let physical = pager.physical_header().expect("physical");
    let page = pager
        .read_page(physical.vector_artifact_page)
        .expect("summary");
    let summary =
        reddb_file::decode_native_vector_artifact_store_page(page.content()).expect("decode");
    let blob = pager.read_page(summary[0].root_page).expect("blob");
    assert_eq!(
        reddb_file::decode_native_blob_page(blob.content())
            .expect("payload")
            .1,
        payload
    );
}

#[test]
fn compact_rejects_pending_wal_and_does_not_publish_corruption() {
    use std::io::{Seek, SeekFrom, Write};
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let wal = reddb_file::unified_wal_path(&source);
    let mut pending = reddb_file::encode_wal_file_header().to_vec();
    pending.extend_from_slice(b"uncheckpointed");
    std::fs::write(&wal, pending).expect("pending WAL");
    let destination = directory.path().join("compacted");
    assert!(compact_paged(&source, &destination)
        .expect_err("pending WAL")
        .to_string()
        .contains("WAL"));
    assert!(!destination.exists());
    std::fs::remove_file(&wal).expect("fixture WAL");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .expect("fixture corruption");
    file.seek(SeekFrom::Start(16384 + 100)).expect("seek");
    file.write_all(&[0xFF]).expect("corrupt catalog");
    drop(file);
    assert!(compact_paged(&source, &destination).is_err());
    assert!(!destination.join("source.rdb").exists());
}

#[test]
fn compact_real_runtime_preserves_resume_indexes_and_subsequent_writes() {
    use reddb_server::{RedDBOptions, RedDBRuntime};
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    let options = RedDBOptions::persistent(&source)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    {
        let runtime = RedDBRuntime::with_options(options).expect("native runtime");
        runtime
            .execute_query("CREATE TABLE records (id INT, value TEXT)")
            .expect("table");
        runtime
            .execute_query("CREATE INDEX record_value ON records (value) USING HASH")
            .expect("index");
        for id in 0..80 {
            runtime
                .execute_query(&format!(
                    "INSERT INTO records (id, value) VALUES ({id}, 'value-{id}')"
                ))
                .expect("insert");
        }
        runtime
            .execute_query("UPDATE records SET value = 'changed' WHERE id = 10")
            .expect("update version");
        runtime
            .execute_query("DELETE FROM records WHERE id = 20")
            .expect("delete version");
        runtime.flush().expect("checkpoint");
    }
    let destination = directory.path().join("compacted");
    compact_paged(&source, &destination).expect("compact actual runtime");
    let options = RedDBOptions::persistent(destination.join("source.rdb"))
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    for round in 0..2 {
        let runtime = RedDBRuntime::with_options(options.clone()).expect("reopen runtime");
        let count = runtime
            .execute_query("SELECT COUNT(*) AS n FROM records")
            .expect("count");
        assert_eq!(
            count.result.records[0]
                .get("n")
                .expect("n")
                .display_string(),
            if round == 0 { "79" } else { "80" }
        );
        let indexed = runtime
            .execute_query("SELECT id FROM records WHERE value = 'changed'")
            .expect("indexed lookup");
        assert_eq!(indexed.result.records.len(), 1);
        assert_eq!(
            indexed.result.records[0]
                .get("id")
                .expect("id")
                .display_string(),
            "10"
        );
        if round == 0 {
            runtime
                .execute_query("INSERT INTO records (id, value) VALUES (99, 'after')")
                .expect("post-compact write");
        }
        runtime.flush().expect("post-compact checkpoint");
    }
}

#[test]
fn compact_child_process() {
    let Some(source) = std::env::var_os("REDDB_COMPACTION_TEST_SOURCE") else {
        return;
    };
    let destination = std::env::var_os("REDDB_COMPACTION_TEST_DESTINATION").expect("destination");
    compact_paged(
        std::path::Path::new(&source),
        std::path::Path::new(&destination),
    )
    .expect("child compaction");
}

#[test]
fn compact_process_loss_preserves_original_and_does_not_publish_partial_file() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let before = std::fs::read(&source).expect("source bytes");
    let destination = directory.path().join("interrupted");
    let mut child = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", "compact_child_process", "--nocapture"])
        .env("REDDB_COMPACTION_TEST_SOURCE", &source)
        .env("REDDB_COMPACTION_TEST_DESTINATION", &destination)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("child");
    let started = std::time::Instant::now();
    loop {
        if std::fs::metadata(destination.join(".compacting.rdb"))
            .is_ok_and(|metadata| metadata.len() > 1280 * 1024)
        {
            break;
        }
        assert!(
            child.try_wait().expect("status").is_none(),
            "child exited before crash injection"
        );
        if started.elapsed() > std::time::Duration::from_secs(30) {
            child.kill().expect("timeout kill");
            child.wait().expect("reap");
            panic!("child did not reach rewrite");
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    child.kill().expect("forced process loss");
    child.wait().expect("reap killed child");
    assert!(
        !destination.join("source.rdb").exists(),
        "incomplete database must not be published"
    );
    assert_eq!(std::fs::read(&source).expect("source after crash"), before);
    compact_paged(&source, &directory.path().join("retry")).expect("retry after process loss");
}

#[test]
fn compact_preserves_overflow_catalog_and_empty_collections() {
    use reddb_server::storage::UnifiedStore;
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    {
        let store = UnifiedStore::open(&source).expect("store");
        for id in 0..700 {
            store
                .create_collection(format!(
                    "collection_{id:04}_with_long_names_for_catalog_overflow"
                ))
                .expect("collection");
        }
        store.persist().expect("catalog checkpoint");
    }
    let destination = directory.path().join("compacted");
    compact_paged(&source, &destination).expect("compact overflow catalog");
    let store = UnifiedStore::open(destination.join("source.rdb")).expect("reopen catalog");
    assert_eq!(store.list_collections().len(), 700);
    store.persist().expect("checkpoint after relocation");
}

#[test]
fn compact_rejects_unknown_catalog_extension_and_preserves_bytes() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    {
        let pager = Pager::open_default(&source).expect("pager");
        let mut page = pager.read_page(1).expect("catalog");
        page.content_mut()[28..32].copy_from_slice(b"FUTR");
        pager.write_page(1, page).expect("future catalog extension");
        pager.sync().expect("sync");
    }
    let before = std::fs::read(&source).expect("source");
    let destination = directory.path().join("compacted");
    assert!(compact_paged(&source, &destination)
        .expect_err("unsupported extension")
        .to_string()
        .contains("unknown catalog extension"));
    assert_eq!(std::fs::read(&source).expect("preserved source"), before);
    assert!(!destination.exists());
}

#[test]
fn compact_repeat_has_stable_size() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let first = directory.path().join("first");
    let report = compact_paged(&source, &first).expect("first compaction");
    let second = directory.path().join("second");
    let repeated = compact_paged(&first.join("source.rdb"), &second).expect("repeated compaction");
    assert_eq!(report.destination_bytes, repeated.destination_bytes);
    assert_eq!(report.records, repeated.records);
    assert_eq!(
        report.destination_allocated_bytes,
        repeated.destination_allocated_bytes
    );
    eprintln!("compaction size evidence: {}", report.machine_json());
}

#[test]
fn compact_preserves_empty_support_scaffolding_but_refuses_hidden_wal() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let support = reddb_file::support_dir_for(&source);
    std::fs::create_dir_all(support.join("indexes")).expect("index scaffold");
    std::fs::create_dir_all(support.join("snapshots")).expect("snapshot scaffold");
    let destination = directory.path().join("compacted");
    compact_paged(&source, &destination).expect("empty support scaffold");
    let copied = reddb_file::support_dir_for(&destination.join("source.rdb"));
    assert!(copied.join("indexes").is_dir());
    assert!(copied.join("snapshots").is_dir());
    std::fs::create_dir_all(support.join("wal")).expect("tiered WAL directory");
    let wal = reddb_file::unified_wal_path_in(&support, &source);
    std::fs::write(wal, b"uncheckpointed").expect("hidden WAL");
    let rejected = directory.path().join("rejected");
    assert!(compact_paged(&source, &rejected)
        .expect_err("non-empty support layout")
        .to_string()
        .contains("non-empty support layout"));
    assert!(!rejected.exists());
}
