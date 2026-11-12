//! A valid CRC must not authorize a misdirected, impossible or stale page image.
use super::*;
use crate::storage::engine::{Page, PageType, Pager, PagerConfig, PAGE_SIZE};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

fn open(path: &Path) -> RedDBResult<RedDBRuntime> {
    let runtime = crate::storage::RedDB::with_disk_residency_for_qualification(|| {
        RedDBRuntime::with_options(
            RedDBOptions::persistent(path)
                .with_memory_budget(64 * 1024 * 1024)
                .with_storage_profile(
                    crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
                )
                .expect("profile"),
        )
    })?;
    runtime
        .background_tasks
        .as_ref()
        .expect("workers")
        .shutdown();
    Ok(runtime)
}

fn seed(path: &Path) {
    let runtime = open(path).expect("healthy fixture");
    for sql in [
        "SET CONFIG runtime.mvcc.autovacuum.enabled = false",
        "CREATE TABLE records (id INT, payload TEXT)",
        "CREATE TABLE unrelated (id INT, payload TEXT)",
        "CREATE UNIQUE INDEX records_id ON records (id) USING HASH",
        "CREATE INDEX records_payload ON records (payload) USING BTREE",
        "INSERT INTO records (id,payload) VALUES (1,'original'),(2,'keep')",
        "INSERT INTO unrelated (id,payload) VALUES (1,'foreign-a'),(2,'foreign-b')",
    ] {
        runtime.execute_query(sql).expect(sql);
    }
    runtime
        .graceful_shutdown(false)
        .expect("durable checkpoint");
}

fn collection_root(path: &Path, collection: &str) -> (u32, Page) {
    let pager = Pager::open(
        path,
        PagerConfig {
            read_only: true,
            create: false,
            disk_residency: true,
            ..PagerConfig::default()
        },
    )
    .expect("read fixture roots");
    let catalog = pager.read_page(1).expect("catalog");
    let resume = reddb_file::native_resume::decode_catalog_resume(catalog.content())
        .expect("catalog trailer")
        .expect("resume roots");
    let root = resume.collections[collection].root_page;
    let page = pager.read_page(root).expect("healthy collection leaf");
    assert_eq!(page.page_type().expect("page type"), PageType::BTreeLeaf);
    page.verify_checksum().expect("fixture checksum");
    (root, page)
}

fn replace_page(path: &Path, destination: u32, image: &Page) {
    image
        .verify_checksum()
        .expect("injected image must have a valid checksum");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("fault target");
    file.seek(SeekFrom::Start(u64::from(destination) * PAGE_SIZE as u64))
        .expect("page offset");
    file.write_all(image.as_bytes())
        .expect("replace persisted page");
    file.sync_all().expect("persist fault");
}

fn assert_authoritative_read_rejected(path: &Path, reason: &str) {
    let before = std::fs::read(path).expect("preserve source evidence");
    let wal_path = reddb_file::layout::unified_wal_path(path);
    let wal_before = std::fs::read(&wal_path).expect("preserve WAL evidence");
    let error = match open(path) {
        Err(error) => error,
        Ok(runtime) => match runtime.execute_query("SELECT id, payload FROM records ORDER BY id") {
            Err(error) => error,
            Ok(result) => panic!(
                "accepted {reason} page as authoritative rows: {:?}",
                result.result.records
            ),
        },
    };
    assert!(error.to_string().contains(reason), "{reason}: {error}");
    assert_eq!(
        std::fs::read(path).expect("unchanged corrupt source"),
        before
    );
    assert_eq!(std::fs::read(wal_path).expect("unchanged WAL"), wal_before);
}

fn assert_healthy_rows(runtime: &RedDBRuntime) {
    let by_id = runtime
        .execute_query("SELECT payload FROM records WHERE id = 1")
        .expect("cold HASH read");
    assert_eq!(by_id.result.records.len(), 1);
    assert_eq!(
        by_id.result.records[0].get("payload"),
        Some(&Value::text("original"))
    );
    let by_payload = runtime
        .execute_query(
            "SELECT id, payload FROM records WHERE payload >= 'a' AND payload < 'z' ORDER BY id",
        )
        .expect("cold BTREE read");
    let scan = runtime
        .execute_query("SELECT id, payload FROM records ORDER BY id")
        .expect("scan");
    for result in [&by_payload, &scan] {
        assert_eq!(result.result.records.len(), 2);
        for (record, (id, payload)) in result
            .result
            .records
            .iter()
            .zip([(1, "original"), (2, "keep")])
        {
            assert_eq!(record.get("id"), Some(&Value::Integer(id)));
            assert_eq!(record.get("payload"), Some(&Value::text(payload)));
        }
    }
    assert!(runtime
        .verify_persisted_indexes("records")
        .expect("index integrity")
        .is_empty());
}

#[test]
fn complete_dwb_repairs_misdirected_and_future_lsn_home_pages() {
    for fault in ["identity", "LSN"] {
        let directory = tempfile::tempdir().expect("isolated recovery fixture");
        let path = directory.path().join("protected.rdb");
        seed(&path);
        let (root, original) = collection_root(&path, "records");
        let frame = reddb_file::encode_paged_dwb_frame([(root, original.as_bytes())]);
        {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .expect("DWB fixture");
            file.seek(SeekFrom::Start(
                reddb_engine::storage::pager_io::dwb_zone_offset(),
            ))
            .expect("DWB offset");
            file.write_all(&frame).expect("stage trusted current image");
            file.sync_all().expect("durable DWB frame");
        }
        let bad = if fault == "identity" {
            collection_root(&path, "unrelated").1
        } else {
            let mut bad = original.clone();
            bad.set_lsn(u64::MAX);
            bad.update_checksum();
            bad
        };
        replace_page(&path, root, &bad);
        for _ in 0..2 {
            let runtime = open(&path).expect("recover only from complete DWB evidence");
            assert_healthy_rows(&runtime);
            runtime
                .graceful_shutdown(false)
                .expect("checkpoint healthy recovery");
        }
    }
}

#[test]
fn checksum_valid_misdirected_collection_page_is_rejected() {
    let directory = tempfile::tempdir().expect("isolated database");
    let path = directory.path().join("misdirected.rdb");
    seed(&path);
    let (root, _) = collection_root(&path, "records");
    let (other, image) = collection_root(&path, "unrelated");
    assert_ne!(root, other);
    replace_page(&path, root, &image);
    assert_authoritative_read_rejected(&path, "identity");
}

#[test]
fn checksum_valid_future_page_lsn_is_rejected() {
    let directory = tempfile::tempdir().expect("isolated database");
    let path = directory.path().join("future-lsn.rdb");
    seed(&path);
    let (root, mut image) = collection_root(&path, "records");
    image.set_lsn(u64::MAX);
    image.update_checksum();
    replace_page(&path, root, &image);
    assert_authoritative_read_rejected(&path, "LSN");
}

#[test]
fn checksum_valid_old_image_of_reused_collection_page_is_rejected() {
    let directory = tempfile::tempdir().expect("isolated database");
    let path = directory.path().join("stale.rdb");
    seed(&path);
    let (old_root, old_image) = collection_root(&path, "records");
    let mut previous = std::collections::BTreeMap::from([(old_root, old_image)]);
    for generation in 1..=16 {
        let runtime = open(&path).expect("healthy writer");
        runtime
            .execute_query(&format!(
                "UPDATE records SET payload = 'committed-{generation}' WHERE id = 1"
            ))
            .expect("committed MVCC update");
        runtime
            .graceful_shutdown(false)
            .expect("publish newer generation");
        drop(runtime);
        let (root, current) = collection_root(&path, "records");
        if let Some(old_image) = previous.insert(root, current.clone()) {
            assert_ne!(
                current.as_bytes(),
                old_image.as_bytes(),
                "fixture must change the page"
            );
            replace_page(&path, root, &old_image);
            assert_authoritative_read_rejected(&path, "generation");
            return;
        }
    }
    panic!("fixture must reuse a collection page within sixteen checkpoints");
}
