#[path = "../../reddb-engine/tests/support/metadata_corpus.rs"]
mod metadata_corpus;
#[path = "../../reddb-engine/tests/support/snapshot.rs"]
mod snapshot;

#[test]
fn shared_snapshot_version_conformance() {
    for (_, run) in snapshot::CASES {
        run();
    }
    let directory = tempfile::tempdir().expect("native snapshot fixture");
    let path = directory.path().join("snapshot.rdb");
    std::fs::write(
        &path,
        include_bytes!("../../reddb-engine/tests/fixtures/native-store-v12.rdst"),
    )
    .expect("native dump");
    let store = reddb_server::storage::UnifiedStore::load_from_file(&path)
        .expect("native restore of frozen snapshot");
    assert!(store.get_collection("empty").is_some());
    assert_eq!(store.aux_metadata(), b"opaque catalog");
    for entity in codec_corpus::entities() {
        let manager = store
            .get_collection(&format!("codec_{}", entity.id.raw()))
            .expect("native collection");
        assert_eq!(manager.count(), 1);
        assert_eq!(
            manager.get(entity.id).expect("native entity").kind,
            entity.kind
        );
        assert_eq!(
            manager
                .get_metadata(entity.id)
                .expect("native metadata")
                .fields,
            metadata_corpus::metadata().fields
        );
    }
    store.save_to_file(&path).expect("native re-export");
    snapshot::verify_native_snapshot(&std::fs::read(&path).expect("re-exported snapshot"));
}

#[path = "../../reddb-engine/tests/support/codec_corpus.rs"]
mod codec_corpus;

use reddb_engine::query::scalar;
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::{json, Value};

#[test]
fn native_storage_error_paths_keep_shared_type_authority() {
    use reddb_engine::storage::error::{StorageIntegrityError, StoreError};
    let detail = StorageIntegrityError::new("wal", "42", Some("rows".into()), "checksum");
    let native: reddb_server::api::StorageIntegrityError = detail.clone();
    assert_eq!(native, detail);
    let native: reddb_server::storage::StoreError = StoreError::StorageIntegrity(native);
    assert!(matches!(native, StoreError::StorageIntegrity(value) if value == detail));
}

#[test]
fn portable_scalar_corpus_matches_the_native_runtime() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory())
        .expect("native in-memory runtime opens");
    let cases: json::Value =
        json::from_str(include_str!("../../reddb-engine/tests/scalar-cases.json"))
            .expect("checked-in corpus is valid JSON");
    for case in cases.as_array().expect("corpus is an array") {
        let sql = case["sql"].as_str().expect("case has SQL");
        let portable = scalar::execute(sql).expect(sql);
        let native = runtime.execute_query(sql).expect(sql).result;
        assert_eq!(native.columns, portable.columns, "{sql}");
        let native_rows: Vec<Vec<json::Value>> = native
            .records
            .iter()
            .map(|record| {
                native
                    .columns
                    .iter()
                    .map(|column| record.get(column).unwrap_or(&Value::Null).to_json())
                    .collect()
            })
            .collect();
        let portable_rows: Vec<Vec<json::Value>> = portable
            .rows
            .iter()
            .map(|row| row.iter().map(Value::to_json).collect())
            .collect();
        assert_eq!(native_rows, portable_rows, "{sql}");
    }
}

#[test]
fn native_collection_paths_keep_the_shared_lifecycle_type() {
    use reddb_engine::storage::manager::{ManagerConfig, SegmentManager};
    use reddb_server::storage as native;

    let manager: native::SegmentManager =
        SegmentManager::with_config("shared", ManagerConfig::default());
    let _: native::ManagerStats = manager.stats();
    let store = native::UnifiedStore::new();
    store
        .create_collection("shared")
        .expect("native collection");
    let _: std::sync::Arc<SegmentManager> = store
        .get_collection("shared")
        .expect("shared native manager");
}

#[test]
fn native_public_operator_paths_keep_the_same_types() {
    use reddb_engine::query::{engine::binding, executor, filter};
    use reddb_server::storage::query as native;

    let _: native::QueryPlan = executor::QueryPlan::new("people");
    let _: native::Filter = filter::Filter::ge("age", Value::Integer(18));
    let _: native::Binding = binding::Binding::empty();
}

#[test]
fn shared_pages_round_trip_through_the_native_pager() {
    use reddb_engine::storage::page::{Page, PageType};
    use reddb_server::storage::engine::{pager::Pager, FreeList, PageCache};

    let _: FreeList = reddb_engine::storage::freelist::FreeList::new();
    let _: PageCache = reddb_engine::storage::page_cache::PageCache::new(16);
    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("shared-pages.rdb");
    let id = {
        let pager = Pager::open_default(&path).expect("native pager opens");
        let mut page: Page = pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate page");
        page.insert_cell(b"key", b"portable value")
            .expect("cell fits");
        let id = page.page_id();
        pager.write_page(id, page).expect("write shared page");
        pager.sync().expect("page and allocation are durable");
        id
    };
    let pager = Pager::open_default(&path).expect("native pager reopens");
    let page: Page = pager.read_page(id).expect("read persisted shared page");
    page.verify_checksum()
        .expect("checksum survives native I/O");
    assert_eq!(
        page.read_cell(0).expect("cell survives restart"),
        (b"key".to_vec(), b"portable value".to_vec())
    );
}

#[test]
fn portable_page_io_updates_a_native_pager_file() {
    use reddb_engine::storage::{page::PageType, pager_io};
    use reddb_file::BorrowedStdFile;
    use reddb_server::storage::engine::pager::Pager;

    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("portable-pager.rdb");
    let id = {
        let pager = Pager::open_default(&path).expect("native pager creates a file");
        let mut page = pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate page");
        page.insert_cell(b"key", b"before").expect("cell fits");
        let id = page.page_id();
        pager.write_page(id, page).expect("write page");
        pager.sync().expect("sync page and allocation");
        id
    };
    {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open closed native file");
        let mut file = BorrowedStdFile(&mut file);
        let mut page =
            pager_io::read_page(&mut file, id, true).expect("portable read of native page");
        assert_eq!(page.read_cell(0).expect("native cell").1, b"before");
        page = reddb_engine::storage::page::Page::new(PageType::BTreeLeaf, id);
        page.insert_cell(b"key", b"after").expect("new cell fits");
        page.update_checksum();
        pager_io::write_pages(&mut file, &[(id, page)], true).expect("portable protected write");
    }
    let pager = Pager::open_default(&path).expect("native pager reopens portable write");
    assert_eq!(
        pager
            .read_page(id)
            .expect("native page")
            .read_cell(0)
            .expect("updated cell")
            .1,
        b"after"
    );
}

#[test]
fn native_pager_recovers_the_shared_dwb_frame_after_a_torn_home_write() {
    use reddb_engine::storage::{page::PageType, page::PAGE_SIZE, pager_io};
    use reddb_file::{BorrowedStdFile, VfsFile};
    use reddb_server::storage::engine::pager::Pager;
    use std::io::SeekFrom;

    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("portable-torn-page.rdb");
    let id = {
        let pager = Pager::open_default(&path).expect("native pager creates a file");
        let mut page = pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate page");
        page.insert_cell(b"key", b"before").expect("cell fits");
        let id = page.page_id();
        pager.write_page(id, page).expect("write page");
        pager.sync().expect("sync page and allocation");
        id
    };
    let mut page = reddb_engine::storage::page::Page::new(PageType::BTreeLeaf, id);
    page.insert_cell(b"key", b"after").expect("new cell fits");
    page.update_checksum();
    {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open closed native file");
        let mut file = BorrowedStdFile(&mut file);
        let frame = reddb_file::encode_paged_dwb_frame([(id, page.as_bytes())]);
        file.seek(SeekFrom::Start(pager_io::dwb_zone_offset()))
            .expect("seek DWB");
        file.write_all(&frame).expect("stage complete DWB frame");
        file.sync_all().expect("DWB is durable before home write");
        file.seek(SeekFrom::Start(u64::from(id) * PAGE_SIZE as u64))
            .expect("seek home page");
        file.write_all(&page.as_bytes()[..PAGE_SIZE / 2])
            .expect("torn home-page prefix");
        file.sync_all().expect("fixture torn page reaches storage");
        assert!(pager_io::read_page(&mut file, id, true).is_err());
    }
    let pager = Pager::open_default(&path).expect("native open replays the shared DWB frame");
    assert_eq!(
        pager
            .read_page(id)
            .expect("recovered native page")
            .as_bytes(),
        page.as_bytes()
    );
}

#[test]
#[ignore = "Native encryption/header layout collision predates this extraction; tracked in #2453"]
fn shared_page_io_preserves_native_encryption_and_authentication() {
    use reddb_engine::storage::{page::PageType, pager_io};
    use reddb_file::BorrowedStdFile;
    use reddb_server::storage::{
        encryption::SecureKey,
        engine::pager::{Pager, PagerConfig, PagerError},
    };

    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("encrypted-pages.rdb");
    let config = || PagerConfig {
        encryption: Some(SecureKey::new(&[0x42; 32])),
        ..PagerConfig::default()
    };
    let payload = b"authenticated page I/O";
    let id = {
        let pager = Pager::open(&path, config()).expect("encrypted pager opens");
        let mut page = pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate page");
        // Keep the fixture payload within the native encrypted page's usable
        // prefix; its tail is reserved for the canonical crypto envelope.
        page.as_bytes_mut()[128..128 + payload.len()].copy_from_slice(payload);
        let id = page.page_id();
        pager
            .write_page_encrypted(id, page)
            .expect("write encrypted page");
        pager.sync().expect("sync encrypted page and header");
        id
    };
    assert!(matches!(
        Pager::open_default(&path),
        Err(PagerError::EncryptionRequired)
    ));
    {
        let pager =
            Pager::open(&path, config()).expect("encryption marker survives root publication");
        let page = pager
            .read_page_decrypted(id)
            .expect("authenticated read through shared byte I/O");
        assert_eq!(&page.as_bytes()[128..128 + payload.len()], payload);
    }
    {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open closed encrypted file");
        let mut file = BorrowedStdFile(&mut file);
        let mut cipher_page = pager_io::read_page(&mut file, id, false)
            .expect("read cipher bytes without a page CRC");
        cipher_page.as_bytes_mut()[200] ^= 1;
        pager_io::write_pages(&mut file, &[(id, cipher_page)], true)
            .expect("persist tampered ciphertext fixture");
    }
    let pager = Pager::open(&path, config()).expect("header remains valid");
    assert!(
        pager.read_page_decrypted(id).is_err(),
        "AES authentication still rejects tampering"
    );
}

#[test]
fn shared_page_io_preserves_opaque_crypto_envelopes_and_authentication() {
    use reddb_engine::storage::{page::Page, page::PAGE_SIZE, pager_io};
    use reddb_file::BorrowedStdFile;
    use reddb_server::storage::encryption::{page_encryptor::OVERHEAD, PageEncryptor, SecureKey};

    let directory = tempfile::tempdir().expect("temporary page I/O directory");
    let path = directory.path().join("cipher-pages.rdb");
    let id = pager_io::FIRST_ALLOCATABLE_PAGE;
    let encryptor = PageEncryptor::new(SecureKey::new(&[0x42; 32]));
    let plaintext = vec![0x5a; PAGE_SIZE - OVERHEAD];
    let ciphertext = encryptor.encrypt(id, &plaintext);
    let page = Page::from_slice(&ciphertext).expect("canonical envelope fills one page");
    {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("new page I/O fixture");
        pager_io::write_pages(&mut BorrowedStdFile(&mut file), &[(id, page)], true)
            .expect("protected opaque page write");
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("reopen persisted page I/O fixture");
    let mut file = BorrowedStdFile(&mut file);
    let mut page =
        pager_io::read_page(&mut file, id, false).expect("read envelope without a page CRC");
    assert_eq!(page.as_bytes().as_slice(), ciphertext);
    assert_eq!(
        encryptor
            .decrypt(id, page.as_bytes())
            .expect("authenticate persisted envelope"),
        plaintext
    );
    page.as_bytes_mut()[200] ^= 1;
    pager_io::write_pages(&mut file, &[(id, page)], true)
        .expect("persist tampered envelope fixture");
    let page = pager_io::read_page(&mut file, id, false).expect("read opaque bytes");
    assert!(
        encryptor.decrypt(id, page.as_bytes()).is_err(),
        "authentication still rejects tampering"
    );
}

#[test]
fn native_and_portable_pagers_share_allocation_and_checkpoint_state() {
    use reddb_engine::storage::{
        page::PageType,
        pager::{NoWal, Pager as PortablePager, PagerConfig as PortableConfig},
    };
    use reddb_file::{OpenMode, StdVfs, Vfs};
    use reddb_server::storage::engine::pager::Pager;

    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("shared-orchestration.rdb");
    let id = {
        let pager = Pager::open_default(&path).expect("native create");
        let mut page = pager.allocate_page(PageType::BTreeLeaf).expect("allocate");
        let id = page.page_id();
        page.insert_cell(b"native", b"before").expect("cell fits");
        pager.write_page(id, page).expect("write");
        pager.sync().expect("native data and allocation durable");
        pager
            .set_checkpoint_in_progress(true, 42)
            .expect("checkpoint start");
        id
    };
    let portable_id = {
        let file = StdVfs
            .open(&path, OpenMode::create_keep())
            .expect("owned file after native close");
        let pager =
            PortablePager::open_owned(file, path.clone(), PortableConfig::default(), NoWal, false)
                .expect("portable open");
        assert!(pager.header().expect("header").checkpoint_in_progress);
        assert_eq!(
            pager
                .read_page(id)
                .expect("native page")
                .read_cell(0)
                .expect("native cell")
                .1,
            b"before"
        );
        pager.enable_deferred_reclamation().expect("defer frees");
        pager.free_page(id).expect("defer native page");
        pager
            .complete_deferred_publication(&[], &[id])
            .expect("publish removal");
        pager.sync().expect("persist portable freelist");
        let mut page = pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("portable allocation");
        let portable_id = page.page_id();
        page.insert_cell(b"portable", b"after").expect("cell fits");
        pager.write_page(portable_id, page).expect("portable write");
        pager
            .sync()
            .expect("data and both allocation roots durable");
        pager.complete_checkpoint(42).expect("checkpoint complete");
        portable_id
    };
    let pager = Pager::open_default(&path).expect("native reopen");
    assert_eq!(
        pager
            .read_page(portable_id)
            .expect("portable page")
            .read_cell(0)
            .expect("portable cell")
            .1,
        b"after"
    );
    assert_eq!(pager.header().expect("header").checkpoint_lsn, 42);
    assert!(!pager.header().expect("header").checkpoint_in_progress);
    assert_ne!(
        pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("next native allocation")
            .page_id(),
        portable_id
    );
}

#[path = "../../reddb-engine/tests/support/btree_restart.rs"]
mod btree_fixture;

#[test]
fn native_and_portable_btrees_share_records_splits_overflow_and_reclamation() {
    use reddb_file::{OpenMode, StdVfs, Vfs};
    use reddb_server::storage::engine::{BTree, Pager, PagerConfig};
    use std::sync::Arc;

    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("portable-tree.rdb");
    {
        let pager = Arc::new(
            Pager::open(
                &path,
                PagerConfig {
                    cache_size: 2,
                    ..PagerConfig::default()
                },
            )
            .expect("native pager"),
        );
        let tree = BTree::new(Arc::clone(&pager));
        let _: &reddb_engine::storage::btree::BTree<Pager> = &tree;
        btree_fixture::seed(&tree).expect("native tree with inline/compressed/overflow values");
        btree_fixture::verify(&tree, false).expect("native records match fixture");
        pager.sync().expect("persist children before root");
        btree_fixture::publish_root(pager.as_ref(), tree.root_page_id()).expect("root");
        pager.sync().expect("native publication");
    }
    let file = StdVfs
        .open(&path, OpenMode::create_keep())
        .expect("closed native file");
    btree_fixture::run(file, 1).expect("portable tree updates native records");
    let pager = Arc::new(Pager::open_default(&path).expect("native reopen"));
    let root = btree_fixture::read_root(pager.as_ref()).expect("portable root");
    let tree = BTree::with_root(pager, root);
    btree_fixture::verify(&tree, true)
        .expect("native cursor and point reads see portable mutations");
}

#[test]
fn native_mvcc_paths_use_the_shared_manager_and_entity_identity() {
    use reddb_engine::transaction::{state::TransactionState, IsolationLevel, SnapshotManager};
    use reddb_server::storage::{transaction as native, EntityId};

    let id: EntityId = reddb_engine::storage::entity_id::EntityId::new(1024);
    assert_eq!(id.raw(), 1024);
    assert_eq!(id.to_string(), "e1024");
    let _: native::SnapshotManager = SnapshotManager::new();
    let isolation: native::IsolationLevel = IsolationLevel::Serializable;
    let state = TransactionState::new();
    let writer: native::Xid = state.begin(1, isolation);
    let manager: std::sync::Arc<native::SnapshotManager> = state.snapshot_manager();
    manager.record_serializable_read(writer, "records", id);
    let captured: native::Snapshot = manager.fresh_read_snapshot();
    assert!(!native::is_visible(
        writer,
        0,
        captured.xid,
        &captured.in_progress,
        &std::collections::HashSet::new()
    ));
    state
        .commit(1, |_| Ok::<_, ()>(()))
        .expect("commit prepares");
    let committed = manager.fresh_read_snapshot();
    assert!(native::is_visible(
        writer,
        0,
        committed.xid,
        &committed.in_progress,
        &std::collections::HashSet::new()
    ));
    assert!(
        !captured.sees(writer, 0),
        "earlier snapshot keeps its exclusions"
    );
}

#[test]
fn shared_entity_reads_use_native_persisted_rows_and_timestamps() {
    use reddb_engine::storage::entity::UnifiedEntity;
    use reddb_engine::transaction::read::{resolve_logical_id, visibility_check, SnapshotContext};
    use reddb_server::storage::StorageDeployPreset;
    use std::collections::HashSet;

    for paged in [false, true] {
        let directory = tempfile::tempdir().expect("temporary entity database");
        let mut options = RedDBOptions::persistent(directory.path().join("entities.rdb"));
        if paged {
            options = options
                .with_storage_profile(StorageDeployPreset::Serverless.selection())
                .expect("paged fixture profile");
        }
        let (logical_id, physical_id, created_at, updated_at) = {
            let runtime =
                RedDBRuntime::with_options(options.clone()).expect("native runtime opens");
            for sql in [
                "CREATE TABLE entity_rows (id INTEGER, n INTEGER)",
                "INSERT INTO entity_rows (id,n) VALUES (1,7)",
                "BEGIN",
                "UPDATE entity_rows SET n = 8 WHERE id = 1",
                "COMMIT",
            ] {
                runtime
                    .execute_query(sql)
                    .expect("native fixture statement");
            }
            runtime.checkpoint().expect("committed row checkpoint");
            let rows = runtime
                .db()
                .store()
                .get_collection("entity_rows")
                .expect("collection exists")
                .query_all(|entity| entity.xmax == 0);
            let entity: &UnifiedEntity = rows.first().expect("one committed row");
            assert_eq!(rows.len(), 1);
            assert!(entity.has_explicit_logical_id());
            (
                entity.logical_id(),
                entity.id,
                entity.created_at,
                entity.updated_at,
            )
        };
        let runtime = RedDBRuntime::with_options(options).expect("native database reopens");
        let context: reddb_server::runtime::mvcc::SnapshotContext = SnapshotContext {
            snapshot: runtime.current_snapshot(),
            manager: runtime.snapshot_manager(),
            own_xids: HashSet::new(),
            requires_index_fallback: false,
            serializable_reader: None,
        };
        let store = runtime.db().store();
        for history in [false, true] {
            let entity: reddb_server::storage::UnifiedEntity = resolve_logical_id(
                store.as_ref(),
                "entity_rows",
                logical_id,
                history,
                |entity| visibility_check(&context, entity.xmin, entity.xmax),
            )
            .expect("shared selector resolves native persisted row");
            assert_eq!(entity.id, physical_id);
            assert_eq!(entity.created_at, created_at);
            assert_eq!(entity.updated_at, updated_at);
            assert_eq!(
                entity.data.as_row().expect("table row").get_field("n"),
                Some(&Value::Integer(8))
            );
            assert!(reddb_server::runtime::mvcc::entity_visible_with_context(
                Some(&context),
                &entity
            ));
        }
        let result = runtime
            .execute_query("SELECT n FROM entity_rows WHERE id = 1")
            .expect("native SQL reads same row");
        assert_eq!(result.result.records[0].get("n"), Some(&Value::Integer(8)));
    }
}

#[test]
fn native_moderation_gate_stays_ahead_of_shared_mvcc_reads() {
    use reddb_engine::storage::entity::{EntityData, EntityId, RowData, UnifiedEntity};
    use reddb_engine::transaction::read::{visibility_check, SnapshotContext};
    use std::collections::HashSet;

    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("native runtime");
    let context = SnapshotContext {
        snapshot: runtime.current_snapshot(),
        manager: runtime.snapshot_manager(),
        own_xids: HashSet::new(),
        requires_index_fallback: false,
        serializable_reader: None,
    };
    for status in ["pending", "rejected", ""] {
        let mut row = UnifiedEntity::table_row(EntityId::new(1024), "moderated", 1, vec![]);
        row.data = EntityData::Row(RowData::with_names(
            vec![Value::text(status)],
            vec!["__moderation_status".into()],
        ));
        assert!(visibility_check(&context, row.xmin, row.xmax));
        assert_eq!(
            reddb_server::runtime::mvcc::entity_visible_with_context(Some(&context), &row),
            status.is_empty()
        );
        assert_eq!(
            reddb_server::runtime::mvcc::entity_visible_with_context(None, &row),
            status.is_empty()
        );
    }
}

#[test]
fn shared_entity_codec_preserves_native_dump_payloads_and_metadata() {
    use reddb_engine::storage::entity_codec::write_entity_binary;
    use reddb_server::storage::UnifiedStore;

    let directory = tempfile::tempdir().expect("native dump fixture");
    let path = directory.path().join("codec.rdb");
    let store = UnifiedStore::new();
    let metadata = metadata_corpus::metadata();
    store
        .create_collection("empty")
        .expect("empty collection registry");
    store.set_aux_metadata(b"opaque catalog".to_vec());
    let mut expected = vec![];
    for entity in codec_corpus::entities() {
        let collection = format!("codec_{}", entity.id.raw());
        let id = store
            .insert_auto(&collection, entity)
            .expect("native insertion");
        let manager = store.get_collection(&collection).expect("collection");
        manager
            .set_metadata(id, metadata.clone())
            .expect("native metadata");
        let stored = manager.get(id).expect("native entity");
        let mut encoded = vec![];
        write_entity_binary(&mut encoded, &stored, reddb_file::STORE_VERSION_CURRENT);
        expected.push((collection, id, encoded));
    }
    store.save_to_file(&path).expect("native dump saved");
    drop(store);
    let mut restored = snapshot::Restored::default();
    let dump = std::fs::read(&path).expect("native snapshot bytes");
    assert_eq!(
        reddb_engine::storage::snapshot::visit_snapshot(&dump, &mut restored)
            .expect("shared snapshot reader"),
        reddb_file::STORE_VERSION_CURRENT,
    );
    assert!(restored.collections["empty"].is_empty());
    assert_eq!(restored.auxiliary, b"opaque catalog");
    let reopened = UnifiedStore::load_from_file(&path).expect("native dump reopens");
    assert!(reopened.get_collection("empty").is_some());
    assert_eq!(reopened.aux_metadata(), b"opaque catalog");
    for (collection, id, bytes) in expected {
        let (shared_entity, shared_metadata) = &restored.collections[&collection][0];
        let mut encoded = vec![];
        write_entity_binary(
            &mut encoded,
            shared_entity,
            reddb_file::STORE_VERSION_CURRENT,
        );
        assert_eq!(encoded, bytes, "shared snapshot for {collection}");
        assert_eq!(
            shared_metadata.as_ref().expect("shared metadata").fields,
            metadata.fields
        );
        let manager = reopened
            .get_collection(&collection)
            .expect("restored collection");
        let entity = manager.get(id).expect("restored entity");
        let mut encoded = vec![];
        write_entity_binary(&mut encoded, &entity, reddb_file::STORE_VERSION_CURRENT);
        assert_eq!(encoded, bytes, "payload for {collection}");
        assert_eq!(
            manager.get_metadata(id).expect("metadata").fields,
            metadata.fields
        );
    }
}

#[test]
fn shared_records_materialize_native_persisted_query_rows() {
    use reddb_engine::query::{
        records::runtime_table_record_from_entity_ref_with_schema, unified::UnifiedResult,
    };
    use reddb_server::storage::StorageDeployPreset;

    for paged in [false, true] {
        let directory = tempfile::tempdir().expect("query record database");
        let mut options = RedDBOptions::persistent(directory.path().join("records.rdb"));
        if paged {
            options = options
                .with_storage_profile(StorageDeployPreset::Serverless.selection())
                .expect("paged profile");
        }
        {
            let runtime = RedDBRuntime::with_options(options.clone()).expect("native open");
            runtime
                .execute_query("CREATE TABLE portable_records (id INTEGER, answer INTEGER)")
                .expect("DDL");
            runtime
                .execute_query("INSERT INTO portable_records (id,answer) VALUES (1,42)")
                .expect("native insert");
            runtime.checkpoint().expect("native checkpoint");
        }
        let runtime = RedDBRuntime::with_options(options).expect("native reopen");
        let result: UnifiedResult = runtime
            .execute_query("SELECT * FROM portable_records WHERE id = 1")
            .expect("native stored-data query")
            .result;
        assert_eq!(result.records.len(), 1);
        let manager = runtime
            .db()
            .store()
            .get_collection("portable_records")
            .expect("native collection");
        let schema = manager.column_schema();
        let rows = manager.query_all(|entity| entity.xmax == 0);
        assert_eq!(rows.len(), 1);
        let record = runtime_table_record_from_entity_ref_with_schema(&rows[0], schema.as_ref())
            .expect("shared native record");
        for column in ["id", "answer", "rid"] {
            assert_eq!(
                record.get(column),
                result.records[0].get(column),
                "{column}, paged={paged}"
            );
        }
    }
}

#[test]
fn native_store_read_indexes_keep_the_shared_types() {
    use reddb_engine::storage::{
        context_index::ContextIndex,
        entity::{EntityData, EntityId, RowData, UnifiedEntity},
        entity_cache::EntityCacheStats,
    };
    let store = reddb_server::storage::UnifiedStore::new();
    store
        .create_collection("documents")
        .expect("native collection");
    let index: &ContextIndex = store.context_index();
    index.set_collection_enabled("documents", true);
    let mut bytes = Vec::new();
    reddb_types::document_body_codec::encode(&[("name", &Value::text("Alice"))], &mut bytes)
        .expect("binary document body");
    let mut entity = UnifiedEntity::table_row(EntityId::new(1024), "documents", 1, vec![]);
    entity.data = EntityData::Row(RowData::with_names(
        vec![Value::Json(bytes)],
        vec!["body".into()],
    ));
    store
        .insert_auto("documents", entity)
        .expect("native insertion indexes body");
    assert_eq!(index.search("alice", 10, None)[0].entity_id.raw(), 1024);
    assert!(store.get_any(EntityId::new(1024)).is_some());
    let cached: EntityCacheStats = store.entity_cache_stats();
    assert_eq!(cached.entries, 1);
    store
        .drop_collection("documents")
        .expect("native collection drop");
    assert!(index.search("alice", 10, None).is_empty());
    assert_eq!(store.entity_cache_stats().entries, 0);
}

#[test]
fn native_table_planning_keeps_catalog_estimates_and_shared_type_authority() {
    use reddb_engine::query::logical_plan::{CanonicalLogicalPlan, CardinalityEstimate};
    use reddb_server::storage::query::planner::build_canonical_plan;
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("native runtime");
    for sql in [
        "CREATE TABLE shared_plan (id INT, total INT)",
        "INSERT INTO shared_plan (id, total) VALUES (1, 10), (2, 20), (3, 30)",
    ] {
        runtime.execute_query(sql).expect(sql);
    }
    let expression = reddb_rql::parse_frontend(
        "SELECT total FROM shared_plan WHERE total >= 10 ORDER BY total DESC LIMIT 1 OFFSET 1",
    )
    .expect("canonical frontend")
    .into_query_expr();
    let plan: CanonicalLogicalPlan = build_canonical_plan(&runtime.db(), &expression);
    let mut node = &plan.root;
    let mut operators = Vec::new();
    loop {
        operators.push(node.operator.as_str());
        let Some(child) = node.children.first() else {
            break;
        };
        node = child;
    }
    assert_eq!(
        operators,
        [
            "projection",
            "limit",
            "offset",
            "sort",
            "filter",
            "table_scan"
        ]
    );
    assert_eq!(
        node.estimated_rows, 3.0,
        "native host reads the live collection count"
    );
    assert_eq!(node.estimated_confidence, 0.85);
    assert_eq!(node.source.as_deref(), Some("shared_plan"));
    assert_eq!(node.details["access_path"], "table_scan");
    assert_eq!(node.details["universal"], "false");
    let native: reddb_server::storage::query::planner::CardinalityEstimate =
        CardinalityEstimate::full_scan(3.0);
    assert_eq!(native.rows, 3.0);
    // The generated plan also executes through the production native adapter.
    let result = runtime
        .execute_query(
            "SELECT total FROM shared_plan WHERE total >= 10 ORDER BY CASE WHEN total >= 10 THEN -CAST(total AS BIGINT) ELSE 0 END ASC LIMIT 1 OFFSET 1",
        )
        .expect("native shared plan and runtime expression execution");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0].get("total"),
        Some(&Value::Integer(20))
    );
}
