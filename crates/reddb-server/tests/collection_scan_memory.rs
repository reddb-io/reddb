//! Collection pages must clone only the requested payload window, over runtime and HTTP.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use reddb_server::{EntityData, RedDBOptions, RedDBRuntime, RedDBServer, ScanCursor};
use reddb_types::Value;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::Duration;

struct TrackedAllocator;

#[global_allocator]
static ALLOCATOR: TrackedAllocator = TrackedAllocator;
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);
static MEASUREMENT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn account_allocated(bytes: usize) {
    let live = LIVE_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
}

// Test-only accounting delegates every allocation to System with its original
// layout; it never allocates bookkeeping objects or changes pointer ownership.
unsafe impl GlobalAlloc for TrackedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            account_allocated(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            account_allocated(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, layout, size) };
        if !replacement.is_null() {
            if size >= layout.size() {
                account_allocated(size - layout.size());
            } else {
                LIVE_BYTES.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        replacement
    }
}

fn get(addr: std::net::SocketAddr, path: &str) -> serde_json::Value {
    let mut stream = TcpStream::connect(addr).expect("connect to owned fixture");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").expect("HTTP framing");
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    serde_json::from_str(body).expect("collection page JSON")
}

#[test]
fn collection_pages_bound_payload_memory_and_preserve_cursor_and_row_schema() {
    let _measurement = MEASUREMENT_LOCK
        .lock()
        .expect("isolated allocation measurement");
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    runtime
        .execute_query("SET CONFIG runtime.result_cache.enabled = false")
        .unwrap();
    runtime
        .execute_query("CREATE TABLE wide_rows (id INTEGER, payload JSON)")
        .unwrap();
    let payload = format!("{{\"value\":\"{}\"}}", "x".repeat(16 * 1024)).into_bytes();
    for start in (0_i64..1024).step_by(64) {
        let mut params = Vec::with_capacity(128);
        let mut values = Vec::with_capacity(64);
        for id in start..start + 64 {
            params.push(Value::Integer(id));
            params.push(Value::Json(payload.clone()));
            values.push(format!("(${}, ${})", params.len() - 1, params.len()));
        }
        runtime
            .execute_query_with_params(
                &format!(
                    "INSERT INTO wide_rows (id, payload) VALUES {}",
                    values.join(",")
                ),
                &params,
            )
            .unwrap();
    }
    for offset in [0, 511, 1023, 1024, usize::MAX] {
        let baseline = LIVE_BYTES.load(Ordering::Relaxed);
        PEAK_BYTES.store(baseline, Ordering::Relaxed);
        let page = runtime
            .scan_collection("wide_rows", Some(ScanCursor { offset }), 1)
            .unwrap();
        let extra = PEAK_BYTES.load(Ordering::Relaxed).saturating_sub(baseline);
        eprintln!("runtime_collection_offset={offset} extra_peak_bytes={extra}");
        assert!(
            extra < 2 * 1024 * 1024,
            "one-row collection page allocated {extra} bytes"
        );
        assert_eq!(page.total, 1024);
        if offset < 1024 {
            assert_eq!(page.items.len(), 1);
            let EntityData::Row(row) = &page.items[0].data else {
                panic!("row data")
            };
            assert_eq!(row.get_field("id"), Some(&Value::Integer(offset as i64)));
            assert_eq!(
                row.get_field("payload"),
                Some(&Value::Json(payload.clone()))
            );
            assert_eq!(
                page.next.map(|c| c.offset),
                (offset < 1023).then_some(offset + 1)
            );
        } else {
            assert!(page.items.is_empty());
            assert!(page.next.is_none());
        }
    }
    let server = RedDBServer::new(runtime);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let _server = server.serve_in_background_on(listener);
    let first = get(addr, "/collections/wide_rows/scan?limit=1");
    let expected_payload = first["items"][0]["data"]["columns"][1].clone();
    drop(first);
    for offset in [511, 1023, 1024, usize::MAX] {
        let baseline = LIVE_BYTES.load(Ordering::Relaxed);
        PEAK_BYTES.store(baseline, Ordering::Relaxed);
        let page = get(
            addr,
            &format!("/collections/wide_rows/scan?limit=1&offset={offset}"),
        );
        let extra = PEAK_BYTES.load(Ordering::Relaxed).saturating_sub(baseline);
        eprintln!("http_collection_offset={offset} extra_peak_bytes={extra}");
        assert!(
            extra < 2 * 1024 * 1024,
            "one-row HTTP collection page allocated {extra} bytes"
        );
        assert_eq!(page["total"], 1024);
        assert_eq!(
            page["items"].as_array().unwrap().len(),
            usize::from(offset < 1024)
        );
        assert_eq!(
            page["next_offset"],
            if offset < 1023 {
                serde_json::json!(offset + 1)
            } else {
                serde_json::Value::Null
            }
        );
        if offset < 1024 {
            assert_eq!(page["items"][0]["data"]["columns"][0], offset);
            assert_eq!(page["items"][0]["data"]["columns"][1], expected_payload);
        }
    }
}

#[test]
fn cold_vault_point_reads_do_not_retain_all_version_payloads() {
    use reddb_engine::storage::collection_read::PagedCollection;
    use reddb_engine::storage::metadata_codec::serialize_entity_record;
    use reddb_server::auth::{AuthConfig, AuthStore};
    use reddb_server::storage::{RowData, StorageDeployPreset, UnifiedEntity};
    use std::sync::Arc;

    let _measurement = MEASUREMENT_LOCK
        .lock()
        .expect("isolated allocation measurement");
    let directory = tempfile::tempdir().expect("cold Vault fixture");
    let options = RedDBOptions::persistent(directory.path().join("vault.rdb"))
        .with_memory_budget(128 * 1024 * 1024)
        .with_storage_profile(StorageDeployPreset::PrimaryReplicaDev.selection())
        .expect("profile");
    let runtime = RedDBRuntime::with_options(options).expect("runtime");
    let store = runtime.db().store();
    let pager = Arc::clone(store.pager().expect("pager"));
    runtime.set_auth_store(Arc::new(
        AuthStore::with_vault_certificate(
            AuthConfig::default(),
            Arc::clone(&pager),
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        )
        .expect("test certificate"),
    ));
    runtime
        .execute_query("CREATE VAULT secrets WITH OWN MASTER KEY")
        .expect("vault");
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut state = 0x1234_5678_u32;
    let plaintext: String = (0..256 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            alphabet[(state & 63) as usize] as char
        })
        .collect();
    runtime
        .execute_query(&format!("VAULT PUT secrets.seed = '{plaintext}'"))
        .expect("large version");
    runtime
        .execute_query("ROTATE VAULT secrets.seed = 'latest'")
        .expect("small latest version");
    let manager = store.get_collection("secrets").expect("secrets");
    let mut large = None;
    let mut small = None;
    manager.for_each_entity(|entity| {
        let row = entity.data.as_row().expect("Vault row");
        match row.get_field("version") {
            Some(Value::Integer(1)) => large = row.get_field("value").cloned(),
            Some(Value::Integer(2)) => small = row.get_field("value").cloned(),
            _ => {}
        }
        true
    });
    let large = large.expect("large encrypted value");
    let small = small.expect("small encrypted value");
    assert!(matches!(&large, Value::Secret(bytes) if bytes.len() > 128 * 1024));
    let data = Arc::new(reddb_server::storage::engine::BTree::new(Arc::clone(
        &pager,
    )));
    let keys = Arc::new(reddb_server::storage::engine::BTree::new(pager));
    let fingerprint = reddb_file::native_key_index::fingerprint("seed");
    const VERSIONS: u64 = 128;
    for version in 1..=VERSIONS {
        let id = store.next_entity_id();
        let mut entity = UnifiedEntity::table_row(id, "secrets", version, vec![]);
        entity.data = EntityData::Row(RowData::with_names(
            vec![
                Value::text("seed"),
                if version == VERSIONS {
                    small.clone()
                } else {
                    large.clone()
                },
                Value::Integer(version as i64),
                Value::Boolean(false),
            ],
            vec![
                "key".into(),
                "value".into(),
                "version".into(),
                "tombstone".into(),
            ],
        ));
        data.insert(
            &id.raw().to_be_bytes(),
            &serialize_entity_record(&entity, None, store.format_version()),
        )
        .expect("private history row");
        keys.insert(
            &reddb_file::native_key_index::encode_candidate(&fingerprint, id.raw()),
            &[],
        )
        .expect("key candidate");
    }
    let root = data.root_page_id();
    manager.install_backing(
        Arc::new(PagedCollection::with_key_index(data, keys)),
        root,
        store.format_version(),
        2 * 1024 * 1024,
        VERSIONS,
    );
    drop(large);
    drop(small);
    for query in [
        "VAULT GET secrets.seed",
        "VAULT REVEAL secrets.seed",
        "VAULT REVEAL secrets.seed VERSION 1",
    ] {
        runtime.invalidate_result_cache();
        let baseline = LIVE_BYTES.load(Ordering::Relaxed);
        PEAK_BYTES.store(baseline, Ordering::Relaxed);
        let result = runtime.execute_query(query).expect("point read");
        let extra = PEAK_BYTES.load(Ordering::Relaxed).saturating_sub(baseline);
        eprintln!("cold_vault_versions={VERSIONS} query={query} extra_peak_bytes={extra}");
        assert!(
            extra < 8 * 1024 * 1024,
            "a point read retained unrelated history payloads: {extra} bytes"
        );
        assert_eq!(result.result.records.len(), 1);
        match query {
            "VAULT GET secrets.seed" => assert_eq!(
                result.result.records[0].get("version"),
                Some(&Value::Integer(VERSIONS as i64))
            ),
            "VAULT REVEAL secrets.seed" => assert_eq!(
                result.result.records[0].get("value"),
                Some(&Value::text("latest"))
            ),
            _ => assert_eq!(
                result.result.records[0].get("value"),
                Some(&Value::text(plaintext.as_str()))
            ),
        }
    }
    assert_eq!(
        manager.count(),
        VERSIONS as usize,
        "point reads preserve every physical version"
    );
}
