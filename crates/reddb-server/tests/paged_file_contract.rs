use reddb_server::{api::RedDBOptions, runtime::RedDBRuntime};

#[test]
fn bounded_paged_runtime_keeps_only_database_and_wal_across_cold_boots() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("contract.rdb");
    let options = RedDBOptions::persistent(&path)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    for cycle in 0..2 {
        let runtime = RedDBRuntime::with_options(options.clone()).expect("cold boot");
        if cycle == 0 {
            runtime
                .execute_query("CREATE TABLE app.rows (id INT, value TEXT)")
                .unwrap();
            runtime
                .execute_query("INSERT INTO app.rows (id, value) VALUES (1, 'retained')")
                .unwrap();
        }
        let rows = runtime
            .execute_query("SELECT value FROM app.rows WHERE id = 1")
            .unwrap();
        assert_eq!(rows.result.records.len(), 1);
        runtime.checkpoint().unwrap();
        drop(runtime);
        let mut extra = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|entry| entry != &path && entry != &reddb_file::unified_wal_path(&path))
            .collect::<Vec<_>>();
        extra.sort();
        assert!(
            extra.is_empty(),
            "unexpected persistent artifacts: {extra:?}"
        );
    }
}

#[test]
fn complete_catalog_survives_compaction_without_metadata_sidecars() {
    use reddb_server::storage::compact::{compact_paged, scrub_paged};
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.rdb");
    let options = RedDBOptions::persistent(&source)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    {
        let runtime = RedDBRuntime::with_options(options).unwrap();
        // Exceeds the 32-collection summary; a summary alone cannot recover it.
        for index in 0..40 {
            runtime
                .execute_query(&format!(
                    "CREATE TABLE app.items_{index} (id INT, value TEXT)"
                ))
                .unwrap();
            runtime
                .execute_query(&format!(
                    "INSERT INTO app.items_{index} (id, value) VALUES (1, 'persisted')"
                ))
                .unwrap();
        }
        runtime.checkpoint().unwrap();
    }
    assert!(scrub_paged(&source).unwrap().assets_verified);
    let output = directory.path().join("compacted");
    compact_paged(&source, &output).unwrap();
    let path = output.join("source.rdb");
    let report = scrub_paged(&path).unwrap();
    assert!(report.assets_verified);
    assert_eq!(report.unclassified_pages, 0);
    assert!(!reddb_file::physical_metadata_binary_path(&path).exists());
    assert!(!reddb_file::physical_metadata_json_path(&path).exists());
    assert_only_database_and_wal(&path);
    for cycle in 0..2 {
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(&path)
                .with_memory_budget(64 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .unwrap();
        assert!(
            runtime
                .db()
                .physical_authority_status()
                .native_bootstrap_ready
        );
        assert!(
            runtime.db().readiness_for_query(),
            "{:?}",
            runtime.db().health()
        );
        for index in 0..40 {
            let name = format!("app.items_{index}");
            let contract = runtime
                .db()
                .collection_contract(&name)
                .expect("full declared contract");
            assert_eq!(contract.declared_columns.len(), 2);
            let result = runtime
                .execute_query(&format!("SELECT value FROM {name} WHERE id = 1"))
                .unwrap();
            assert_eq!(result.result.records.len(), 1);
        }
        if cycle == 0 {
            runtime
                .execute_query("CREATE TABLE app.to_drop (id INT)")
                .unwrap();
            runtime.execute_query("DROP TABLE app.to_drop").unwrap();
        } else {
            assert!(runtime.db().store().get_collection("app.to_drop").is_none());
        }
        runtime.checkpoint().unwrap();
    }
}

#[test]
fn canonical_recovery_embeds_reconciled_metadata_and_preserves_source() {
    use reddb_server::{
        api::{CatalogSnapshot, CollectionStats},
        physical::PhysicalMetadataFile,
        storage::{
            compact::{recover_paged_wal_canonical_system_config, scrub_paged},
            UnifiedStore, UnifiedStoreConfig,
        },
    };
    use std::collections::BTreeMap;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("recovery.rdb");
    {
        let store = UnifiedStore::open_with_config(
            &source,
            UnifiedStoreConfig {
                resident_memory_bytes_max: Some(64 * 1024 * 1024),
                ..Default::default()
            },
        )
        .unwrap();
        store.create_collection("red.config").unwrap();
        store.create_collection("redzconfig").unwrap();
        store.persist().unwrap();
    }
    {
        use reddb_server::storage::engine::{Pager, PagerConfig};
        let pager = Pager::open(
            &source,
            PagerConfig {
                disk_residency: true,
                ..Default::default()
            },
        )
        .unwrap();
        let mut page = pager.read_page(1).unwrap();
        let bytes = page.content_mut();
        let mut replacements = 0;
        for offset in 0..=bytes.len() - 10 {
            if &bytes[offset..offset + 10] == b"redzconfig" {
                bytes[offset..offset + 10].copy_from_slice(b"red_config");
                replacements += 1;
            }
        }
        assert!(
            replacements >= 2,
            "root and resume identities must both be legacy"
        );
        pager.write_page(1, page).unwrap();
        pager.sync().unwrap();
    }
    let mut metadata = PhysicalMetadataFile::from_state(
        RedDBOptions::persistent(&source),
        CatalogSnapshot::default(),
        BTreeMap::new(),
        Vec::new(),
        None,
    );
    for name in ["red_config", "red.config"] {
        metadata
            .catalog
            .stats_by_collection
            .insert(name.into(), CollectionStats::default());
        metadata.superblock.collection_roots.insert(name.into(), 0);
    }
    let sidecar = PhysicalMetadataFile::metadata_binary_path_for(&source);
    metadata.save_to_binary_path(&sidecar).unwrap();
    {
        use reddb_server::storage::wal::{WalRecord, WalWriter};
        let mut wal = WalWriter::open(reddb_file::unified_wal_path(&source)).unwrap();
        wal.append(&WalRecord::Begin { tx_id: 999 }).unwrap();
        wal.sync().unwrap();
    }
    let before = std::fs::read(&source).unwrap();
    let source_wal = std::fs::read(reddb_file::unified_wal_path(&source)).unwrap();
    let sidecar_before = std::fs::read(&sidecar).unwrap();
    let output = directory.path().join("recovered");
    recover_paged_wal_canonical_system_config(&source, &output).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(std::fs::read(&sidecar).unwrap(), sidecar_before);
    assert_eq!(
        std::fs::read(reddb_file::unified_wal_path(&source)).unwrap(),
        source_wal
    );
    let path = output.join("recovery.rdb");
    assert!(!PhysicalMetadataFile::metadata_binary_path_for(&path).exists());
    assert!(scrub_paged(&path).unwrap().assets_verified);
    for _ in 0..2 {
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(&path)
                .with_memory_budget(64 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .unwrap();
        let metadata = runtime.db().physical_metadata().unwrap();
        assert!(!metadata
            .catalog
            .stats_by_collection
            .contains_key("red_config"));
        assert!(runtime.db().store().get_collection("red.config").is_some());
        runtime.checkpoint().unwrap();
    }
}

fn assert_only_database_and_wal(path: &std::path::Path) {
    let extras = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|entry| entry != path && entry != &reddb_file::unified_wal_path(path))
        .collect::<Vec<_>>();
    assert!(
        extras.is_empty(),
        "unexpected persistent artifacts: {extras:?}"
    );
}

#[test]
fn corrupt_native_metadata_document_fails_closed_without_sidecar_fallback() {
    use reddb_server::storage::engine::{Pager, PagerConfig};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("corrupt.rdb");
    let options = RedDBOptions::persistent(&path)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    {
        let runtime = RedDBRuntime::with_options(options.clone()).unwrap();
        runtime
            .execute_query("CREATE TABLE app.corruption_probe (id INT)")
            .unwrap();
        runtime.checkpoint().unwrap();
    }
    {
        let pager = Pager::open(
            &path,
            PagerConfig {
                disk_residency: true,
                ..Default::default()
            },
        )
        .unwrap();
        let header = pager.physical_header().unwrap();
        let page = pager.read_page(header.metadata_state_page).unwrap();
        let reference =
            reddb_file::native_store::decode_native_metadata_document_ref(page.content())
                .unwrap()
                .unwrap();
        let mut blob = pager.read_page(reference.root_page).unwrap();
        blob.content_mut()[reddb_file::native_store::NATIVE_BLOB_PAGE_HEADER_BYTES] ^= 1;
        // The page remains valid; the full document checksum must detect this.
        pager.write_page(reference.root_page, blob).unwrap();
        pager.sync().unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    let error = RedDBRuntime::with_options(options)
        .err()
        .expect("corrupt metadata must block boot");
    assert!(
        error
            .to_string()
            .contains("metadata blob length or checksum mismatch"),
        "{error}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_only_database_and_wal(&path);
}
