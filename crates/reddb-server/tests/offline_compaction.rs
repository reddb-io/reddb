use reddb_server::storage::compact::{
    compact_paged, salvage_paged_indexes, scrub_paged, scrub_paged_trees,
};
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
fn scrub_paged_trees_reports_corruption_without_changing_source() {
    use std::io::{Read, Seek, SeekFrom, Write};

    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    {
        let pager = Arc::new(Pager::open_default(&source).expect("open source"));
        let catalog = pager.read_page(1).expect("catalog");
        let mut offset = reddb_file::METADATA_HEADER_BYTES;
        let first = reddb_file::decode_native_paged_collection_root(catalog.content(), &mut offset)
            .expect("first root");
        let second = BTree::new(Arc::clone(&pager));
        for id in 0u64..3 {
            second
                .insert(&id.to_be_bytes(), b"healthy")
                .expect("insert");
        }
        let mut metadata = Vec::new();
        reddb_file::encode_native_paged_metadata_header(
            &mut metadata,
            reddb_file::NativePagedMetadataHeader {
                format_version: 11,
                collection_count: 2,
            },
        );
        reddb_file::encode_native_paged_collection_root(&mut metadata, "rows", first.root_page);
        reddb_file::encode_native_paged_collection_root(
            &mut metadata,
            "healthy",
            second.root_page_id(),
        );
        metadata.extend_from_slice(&0u32.to_le_bytes());
        let mut catalog = Page::new(PageType::Header, 1);
        catalog.content_mut()[..metadata.len()].copy_from_slice(&metadata);
        pager.write_page(1, catalog).expect("catalog");
        pager.sync().expect("checkpoint");
    }
    let before = std::fs::read(&source).expect("source image");
    let report = scrub_paged_trees(&source).expect("scrub healthy trees");
    assert_eq!(report.trees_verified, 2);
    assert_eq!(report.records_verified, 1003);
    assert!(report.findings.is_empty());
    assert_eq!(std::fs::read(&source).expect("preserved image"), before);

    let pager = Pager::open_default(&source).expect("open source");
    let catalog = pager.read_page(1).expect("catalog");
    let mut offset = reddb_file::METADATA_HEADER_BYTES;
    let root = reddb_file::decode_native_paged_collection_root(catalog.content(), &mut offset)
        .expect("root")
        .root_page;
    drop(pager);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(root) * 16384 + 100))
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    byte[0] ^= 0x80;
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&byte).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let damaged = std::fs::read(&source).expect("damaged image");
    let report = scrub_paged_trees(&source).expect("report corruption");
    assert_eq!(report.trees_verified, 1);
    assert_eq!(report.records_verified, 3);
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].collection, "rows");
    assert_eq!(report.findings[0].root_page, root);
    let destination = directory.path().join("unsafe-recovery");
    assert!(salvage_paged_indexes(&source, &destination).is_err());
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&source).expect("preserved damage"), damaged);
}

#[test]
fn destroyed_paged_superblocks_do_not_trigger_embedded_carving() {
    use std::io::{Seek, SeekFrom, Write};

    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    let destination = directory.path().join("recovered.rdb");
    fixture(&source);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .expect("open source for fault injection");
    file.write_all(&vec![0; reddb_file::PAGED_SUPERBLOCK_ZONE_SIZE])
        .expect("destroy both superblocks");
    file.seek(SeekFrom::End(0)).expect("end");
    let mut embedded_value =
        reddb_file::encode_native_store_header(reddb_file::STORE_VERSION_CURRENT);
    embedded_value.extend_from_slice(&[0; 16]);
    reddb_file::append_native_store_crc32_footer(&mut embedded_value);
    file.write_all(&embedded_value)
        .expect("embedded-looking bytes");
    file.sync_all().expect("sync fault injection");
    drop(file);
    let damaged = std::fs::read(&source).expect("damaged image");
    let error = reddb_file::salvage_embedded_store(&source, &destination)
        .expect_err("paged source must not be carved as embedded");
    assert!(error.to_string().contains("paged"), "{error}");
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&source).expect("preserved source"), damaged);
}

#[test]
fn compact_refuses_busy_source_and_existing_destination() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let destination = directory.path().join("compacted");
    let pager = Pager::open_default(&source).expect("writer");
    assert!(scrub_paged_trees(&source).is_err());
    assert!(salvage_paged_indexes(&source, &destination).is_err());
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
fn index_salvage_refuses_unverified_vault() {
    use reddb_server::auth::vault::{KeyPair, Vault, VaultState};
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    let destination = directory.path().join("recovered");
    fixture(&source);
    let pager = Pager::open_default(&source).expect("pager");
    let certificate = KeyPair::generate().certificate_hex();
    let vault = Vault::with_certificate(&pager, &certificate).expect("vault");
    let mut state = VaultState {
        bootstrapped: true,
        ..VaultState::default()
    };
    state.kv.insert("red.secret".into(), "secret".into());
    vault.save(&pager, &state).expect("save vault");
    pager.sync().expect("checkpoint");
    drop(pager);
    let image = std::fs::read(&source).unwrap();
    assert!(scrub_paged(&source).is_err());
    let error = salvage_paged_indexes(&source, &destination).expect_err("unverified vault");
    assert!(error.to_string().contains("vault"), "{error}");
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&source).unwrap(), image);
}

#[test]
fn physical_scrub_detects_native_metadata_damage_without_writes() {
    use std::io::{Read, Seek, SeekFrom, Write};
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let pager = Pager::open_default(&source).expect("pager");
    let data = reddb_file::encode_native_collection_roots_page(&std::collections::BTreeMap::new());
    let mut page = pager
        .allocate_page(PageType::NativeMeta)
        .expect("native metadata");
    let id = page.page_id();
    page.content_mut()[..data.len()].copy_from_slice(&data);
    pager.write_page(id, page).expect("write metadata");
    let mut physical = pager.physical_header().unwrap();
    physical.collection_roots_page = id;
    physical.collection_roots_checksum = reddb_file::native_store_page_checksum(&data);
    pager.update_physical_header(physical).unwrap();
    pager.sync().unwrap();
    drop(pager);
    let before = std::fs::read(&source).unwrap();
    let report = scrub_paged(&source).expect("scrub healthy metadata");
    assert!(report.assets_verified);
    assert!(report.trees.findings.is_empty());
    assert!(
        report.unclassified_pages >= 512,
        "abandoned allocations remain explicitly unclassified"
    );
    assert_eq!(std::fs::read(&source).unwrap(), before);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(id) * 16384 + 100))
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let damaged = std::fs::read(&source).unwrap();
    assert!(scrub_paged(&source).is_err());
    assert_eq!(std::fs::read(&source).unwrap(), damaged);
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
    assert!(scrub_paged_trees(&source)
        .expect_err("pending WAL")
        .to_string()
        .contains("WAL"));
    let destination = directory.path().join("compacted");
    assert!(salvage_paged_indexes(&source, &destination).is_err());
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
    assert!(scrub_paged_trees(&source).is_err());
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
        runtime
            .graceful_shutdown(false)
            .expect("shutdown before compaction");
    }
    let report = scrub_paged_trees(&source).expect("scrub real runtime trees");
    assert!(report.findings.is_empty(), "{:?}", report.findings);
    assert!(report.trees_verified >= 2, "collection and secondary index");
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
        runtime
            .graceful_shutdown(false)
            .expect("post-compact shutdown");
    }
}

#[test]
fn salvage_rebuilds_corrupt_secondary_index_from_verified_rows() {
    use reddb_server::{RedDBOptions, RedDBRuntime};
    use std::io::{Read, Seek, SeekFrom, Write};

    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    let options = RedDBOptions::persistent(&source)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    {
        let runtime = RedDBRuntime::with_options(options).expect("runtime");
        runtime
            .execute_query("CREATE TABLE records (id INT, value TEXT)")
            .unwrap();
        runtime
            .execute_query("CREATE INDEX record_value ON records (value) USING HASH")
            .unwrap();
        runtime
            .execute_query("CREATE INDEX record_id ON records (id) USING HASH")
            .unwrap();
        for id in 0..80 {
            runtime
                .execute_query(&format!(
                    "INSERT INTO records (id, value) VALUES ({id}, 'value-{id}')"
                ))
                .unwrap();
        }
        runtime
            .execute_query("UPDATE records SET value = 'changed' WHERE id = 10")
            .unwrap();
        runtime
            .execute_query("DELETE FROM records WHERE id = 20")
            .unwrap();
        runtime.graceful_shutdown(false).expect("checkpoint");
    }
    let pager = Pager::open(
        &source,
        PagerConfig {
            read_only: true,
            create: false,
            ..PagerConfig::default()
        },
    )
    .expect("pager");
    let healthy = scrub_paged(&source).expect("physical scrub");
    assert!(healthy.assets_verified);
    assert!(healthy.trees.findings.is_empty());
    let catalog = pager.read_page(1).expect("catalog");
    let resume = reddb_file::native_resume::decode_catalog_resume(catalog.content())
        .expect("decode catalog")
        .expect("resume");
    let root = resume.collections["records"].secondary_indices["record_value"].root_page;
    assert_ne!(root, 0);
    drop(pager);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(root) * 16384 + 100))
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let damaged = std::fs::read(&source).unwrap();
    let scrub = scrub_paged(&source).expect("scrub damaged index");
    assert!(!scrub.assets_verified);
    assert_eq!(scrub.trees.findings.len(), 1);
    assert_eq!(std::fs::read(&source).unwrap(), damaged);
    let destination = directory.path().join("recovered");
    let report = salvage_paged_indexes(&source, &destination).expect("recover index");
    assert_eq!(report.indexes.len(), 1);
    assert_eq!(report.indexes[0].collection, "records");
    assert_eq!(report.indexes[0].index, "record_value");
    let saved = std::fs::read_to_string(destination.join("salvage.json")).expect("loss report");
    assert!(saved.contains("record_value"));
    assert!(saved.contains("discarded_for_rebuild"));
    assert_eq!(std::fs::read(&source).unwrap(), damaged);
    let recovered = RedDBOptions::persistent(destination.join("source.rdb"))
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    for round in 0..2 {
        let runtime = RedDBRuntime::with_options(recovered.clone()).expect("open recovered store");
        let count = runtime
            .execute_query("SELECT COUNT(*) AS n FROM records")
            .unwrap();
        assert_eq!(
            count.result.records[0].get("n").unwrap().display_string(),
            "79"
        );
        let row = runtime
            .execute_query("SELECT id FROM records WHERE value = 'changed'")
            .unwrap();
        assert_eq!(row.result.records.len(), 1);
        assert_eq!(
            row.result.records[0].get("id").unwrap().display_string(),
            "10"
        );
        let explain = runtime
            .explain_query("SELECT id FROM records WHERE value = 'changed'")
            .expect("explain recovered index");
        let mut pending = vec![&explain.logical_plan.root];
        let mut found_index = false;
        while let Some(node) = pending.pop() {
            found_index |= node.operator == "index_seek";
            pending.extend(&node.children);
        }
        assert!(
            found_index,
            "recovered query should choose an index seek: {:?}",
            explain.logical_plan.root
        );
        assert!(runtime
            .execute_query("SELECT id FROM records WHERE value = 'value-20'")
            .unwrap()
            .result
            .records
            .is_empty());
        runtime
            .graceful_shutdown(false)
            .expect("checkpoint recovered store");
        drop(runtime);
        if round == 0 {
            let pager = Pager::open(
                destination.join("source.rdb"),
                PagerConfig {
                    read_only: true,
                    create: false,
                    ..PagerConfig::default()
                },
            )
            .expect("read recovered catalog");
            let page = pager.read_page(1).expect("catalog");
            let resume = reddb_file::native_resume::decode_catalog_resume(page.content())
                .unwrap()
                .expect("resume");
            assert!(
                resume.collections["records"]
                    .secondary_indices
                    .contains_key("record_value"),
                "index should be rebuilt before publication"
            );
            assert!(resume.collections["records"]
                .secondary_indices
                .contains_key("record_id"));
        }
    }

    // A damaged operational manifest must not be copied as if it were verified.
    let manifest = source
        .with_file_name("source.rdb.ops")
        .join("manifest.json");
    let manifest_bytes = std::fs::read(&manifest).unwrap();
    let mut bad_manifest = manifest_bytes.clone();
    let checksum = bad_manifest
        .windows(b"checksum".len())
        .position(|part| part == b"checksum")
        .unwrap();
    let digit = (checksum + b"checksum".len()..bad_manifest.len())
        .find(|&i| bad_manifest[i].is_ascii_hexdigit())
        .unwrap();
    bad_manifest[digit] = if bad_manifest[digit] == b'0' {
        b'1'
    } else {
        b'0'
    };
    std::fs::write(&manifest, bad_manifest).unwrap();
    let rejected = directory.path().join("rejected-manifest");
    assert!(salvage_paged_indexes(&source, &rejected).is_err());
    assert!(!rejected.exists());
    std::fs::write(&manifest, manifest_bytes).unwrap();

    // A valid page paired with the wrong published digest is also unsafe.
    let pager = Pager::open(
        &source,
        PagerConfig {
            disk_residency: true,
            ..PagerConfig::default()
        },
    )
    .expect("open for metadata fault injection");
    let mut physical = pager.physical_header().unwrap();
    assert_ne!(physical.manifest_page, 0);
    physical.manifest_checksum ^= 1;
    pager.update_physical_header(physical).unwrap();
    pager.sync().unwrap();
    drop(pager);
    let rejected = directory.path().join("rejected-metadata");
    assert!(scrub_paged(&source).is_err());
    let err = salvage_paged_indexes(&source, &rejected).expect_err("metadata digest mismatch");
    assert!(err.to_string().contains("metadata"), "{err}");
    assert!(!rejected.exists());
    let pager = Pager::open(
        &source,
        PagerConfig {
            disk_residency: true,
            ..PagerConfig::default()
        },
    )
    .expect("restore metadata");
    physical.manifest_checksum ^= 1;
    pager.update_physical_header(physical).unwrap();
    pager.sync().unwrap();
    drop(pager);

    // An allocation with unknown ownership must not be silently discarded.
    let pager = Pager::open(
        &source,
        PagerConfig {
            disk_residency: true,
            ..PagerConfig::default()
        },
    )
    .expect("allocate unclassified page");
    let page = pager.allocate_page(PageType::Overflow).unwrap();
    pager.write_page(page.page_id(), page).unwrap();
    pager.sync().unwrap();
    drop(pager);
    let unclassified = std::fs::read(&source).unwrap();
    let rejected = directory.path().join("rejected-unclassified");
    let err = salvage_paged_indexes(&source, &rejected).expect_err("unclassified page");
    assert!(err.to_string().contains("unclassified"), "{err}");
    assert!(!rejected.join("source.rdb").exists());
    assert_eq!(std::fs::read(&source).unwrap(), unclassified);

    // A second damaged authority must not be silently repaired or omitted
    // from a report that only describes index recovery.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.seek(SeekFrom::Start(100)).unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let damaged_authority = std::fs::read(&source).unwrap();
    let rejected = directory.path().join("rejected-superblock");
    assert!(salvage_paged_indexes(&source, &rejected).is_err());
    assert!(!rejected.exists());
    assert_eq!(std::fs::read(&source).unwrap(), damaged_authority);
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

fn structural_leaves(pager: &Arc<Pager>, tree: &BTree) -> Vec<u32> {
    tree.retired_pages()
        .expect("structural inventory")
        .into_iter()
        .rev()
        .filter(|id| {
            pager
                .read_page(*id)
                .expect("page")
                .page_type()
                .expect("type")
                == PageType::BTreeLeaf
        })
        .collect()
}

fn assert_leaf_links(pager: &Arc<Pager>, tree: &BTree) {
    let leaves = structural_leaves(pager, tree);
    assert!(leaves.len() > 3, "exercise non-rightmost splits");
    for (index, id) in leaves.iter().enumerate() {
        let page = pager.read_page(*id).expect("leaf");
        let previous = u32::from_le_bytes(page.content()[..4].try_into().expect("previous"));
        let next = u32::from_le_bytes(page.content()[4..8].try_into().expect("next"));
        assert_eq!(
            previous,
            index.checked_sub(1).map_or(0, |i| leaves[i]),
            "previous link at {id}"
        );
        assert_eq!(
            next,
            leaves.get(index + 1).copied().unwrap_or(0),
            "next link at {id}"
        );
    }
}

#[test]
fn non_rightmost_splits_preserve_bidirectional_links_after_reopen() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    let pager = Arc::new(Pager::open_default(&source).expect("pager"));
    let tree = BTree::new(Arc::clone(&pager));
    for id in (0u64..256).rev() {
        tree.insert(
            &id.to_be_bytes(),
            &vec![u8::try_from(id).expect("fixture byte"); 800],
        )
        .expect("descending insert");
    }
    assert_leaf_links(&pager, &tree);
    for id in (0u64..256).step_by(3) {
        tree.delete(&id.to_be_bytes())
            .expect("delete and rebalance");
    }
    assert_leaf_links(&pager, &tree);
    let root = tree.root_page_id();
    pager.sync().expect("sync");
    drop(tree);
    drop(pager);
    let pager = Arc::new(Pager::open_default(&source).expect("reopen"));
    let tree = BTree::with_root(Arc::clone(&pager), root);
    assert_leaf_links(&pager, &tree);
    for id in 0u64..256 {
        assert_eq!(
            tree.get(&id.to_be_bytes()).expect("lookup"),
            (!id.is_multiple_of(3)).then(|| vec![u8::try_from(id).expect("fixture byte"); 800])
        );
    }
}

#[test]
fn compact_repairs_historical_backlinks_but_refuses_forward_omissions() {
    for forward in [false, true] {
        let directory = tempfile::tempdir().expect("directory");
        let source = directory.path().join("source.rdb");
        fixture(&source);
        {
            let pager = Arc::new(Pager::open_default(&source).expect("pager"));
            let page = pager.read_page(1).expect("catalog");
            let mut offset = reddb_file::METADATA_HEADER_BYTES;
            let root = reddb_file::decode_native_paged_collection_root(page.content(), &mut offset)
                .expect("root");
            let tree = BTree::with_root(Arc::clone(&pager), root.root_page);
            let leaves = structural_leaves(&pager, &tree);
            assert!(leaves.len() > 3);
            // Historical split: A -> B -> C is intact, but C still points to A.
            // A forward skip must instead fail, even if the leaf checksums pass.
            let (id, offset, link) = if forward {
                (leaves[0], 4, leaves[2])
            } else {
                (leaves[2], 0, leaves[0])
            };
            let mut page = pager.read_page(id).expect("leaf");
            page.content_mut()[offset..offset + 4].copy_from_slice(&link.to_le_bytes());
            pager.write_page(id, page).expect("historical link");
            pager.sync().expect("sync");
        }
        let before = std::fs::read(&source).expect("source bytes");
        let destination = directory.path().join("compacted");
        if forward {
            let error = compact_paged(&source, &destination).expect_err("forward omission");
            assert!(error.to_string().contains("leaf chain"));
            assert!(!destination.join("source.rdb").exists());
        } else {
            let report = compact_paged(&source, &destination)
                .expect("repair independently verified backlink");
            assert_eq!(report.records, 1000);
            assert!(report
                .machine_json()
                .contains("\"repaired_leaf_backlinks\":1"));
            let pager =
                Arc::new(Pager::open_default(destination.join("source.rdb")).expect("reopen"));
            let page = pager.read_page(1).expect("catalog");
            let mut offset = reddb_file::METADATA_HEADER_BYTES;
            let root = reddb_file::decode_native_paged_collection_root(page.content(), &mut offset)
                .expect("root");
            let tree = BTree::with_root(Arc::clone(&pager), root.root_page);
            assert_leaf_links(&pager, &tree);
            for id in 0u64..1000 {
                assert_eq!(
                    tree.get(&id.to_be_bytes()).expect("lookup"),
                    Some(payload(id))
                );
            }
        }
        assert_eq!(std::fs::read(&source).expect("unchanged source"), before);
    }
}
