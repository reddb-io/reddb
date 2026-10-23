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
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open closed native file");
    let mut file = BorrowedStdFile(&mut file);
    let mut page = pager_io::read_page(&mut file, id, true).expect("portable read of native page");
    assert_eq!(page.read_cell(0).expect("native cell").1, b"before");
    page = reddb_engine::storage::page::Page::new(PageType::BTreeLeaf, id);
    page.insert_cell(b"key", b"after").expect("new cell fits");
    page.update_checksum();
    pager_io::write_pages(&mut file, &[(id, page)], true).expect("portable protected write");
    drop(file);
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
    let (id, mut page) = {
        let pager = Pager::open_default(&path).expect("native pager creates a file");
        let mut page = pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate page");
        page.insert_cell(b"key", b"before").expect("cell fits");
        let id = page.page_id();
        pager.write_page(id, page.clone()).expect("write page");
        pager.sync().expect("sync page and allocation");
        (id, page)
    };
    page = reddb_engine::storage::page::Page::new(PageType::BTreeLeaf, id);
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
