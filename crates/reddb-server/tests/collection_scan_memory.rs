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
