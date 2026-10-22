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
