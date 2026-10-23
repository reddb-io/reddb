use reddb_engine::query::scalar;
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::{json, Value};

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
