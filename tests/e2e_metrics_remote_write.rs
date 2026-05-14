mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use prost::Message;
use reddb::server::RedDBServer;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use support::{checkpoint_and_reopen, PersistentDbPath};

#[derive(Clone, PartialEq, Message)]
struct WriteRequest {
    #[prost(message, repeated, tag = "1")]
    timeseries: Vec<TimeSeries>,
}

#[derive(Clone, PartialEq, Message)]
struct TimeSeries {
    #[prost(message, repeated, tag = "1")]
    labels: Vec<Label>,
    #[prost(message, repeated, tag = "2")]
    samples: Vec<Sample>,
}

#[derive(Clone, PartialEq, Message)]
struct Label {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(string, tag = "2")]
    value: String,
}

#[derive(Clone, PartialEq, Message)]
struct Sample {
    #[prost(double, tag = "1")]
    value: f64,
    #[prost(int64, tag = "2")]
    timestamp: i64,
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn text<'a>(row: &'a reddb::storage::query::UnifiedRecord, field: &str) -> &'a str {
    match row.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected {field} text, got {other:?} in {row:?}"),
    }
}

fn uint(row: &reddb::storage::query::UnifiedRecord, field: &str) -> u64 {
    match row.get(field) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) => *value as u64,
        other => panic!("expected {field} unsigned integer, got {other:?} in {row:?}"),
    }
}

fn json_tags<'a>(row: &'a reddb::storage::query::UnifiedRecord, field: &str) -> reddb::json::Value {
    match row.get(field) {
        Some(Value::Json(bytes)) => reddb::json::from_slice(bytes).expect("tags json should parse"),
        other => panic!("expected {field} json, got {other:?} in {row:?}"),
    }
}

fn label(name: &str, value: &str) -> Label {
    Label {
        name: name.to_string(),
        value: value.to_string(),
    }
}

fn sample(value: f64, timestamp: i64) -> Sample {
    Sample { value, timestamp }
}

fn remote_write_body(request: &WriteRequest) -> Vec<u8> {
    let mut protobuf = Vec::new();
    request
        .encode(&mut protobuf)
        .expect("remote_write protobuf should encode");
    snap::raw::Encoder::new()
        .compress_vec(&protobuf)
        .expect("remote_write body should snappy-compress")
}

fn with_one_request_server(
    rt: RedDBRuntime,
    send: impl FnOnce(&str) -> (u16, String),
) -> (u16, String) {
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let handle = thread::spawn(move || server.serve_one_on(listener));
    let response = send(&addr.to_string());
    handle
        .join()
        .expect("server thread should join")
        .expect("server should serve one request");
    response
}

fn http_post_remote_write(rt: RedDBRuntime, collection: &str, body: Vec<u8>) -> (u16, String) {
    with_one_request_server(rt, |addr| send_post_remote_write(addr, collection, body))
}

fn send_post_remote_write(addr: &str, collection: &str, body: Vec<u8>) -> (u16, String) {
    let mut request = format!(
        "POST /api/v1/write?collection={collection} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Encoding: snappy\r\n\
         Content-Type: application/x-protobuf\r\n\
         X-Prometheus-Remote-Write-Version: 0.1.0\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(&body);
    http_request(addr, request)
}

fn http_get(rt: RedDBRuntime, path: &str) -> (u16, String) {
    with_one_request_server(rt, |addr| {
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .into_bytes();
        http_request(addr, request)
    })
}

fn http_request(addr: &str, request: Vec<u8>) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");
    stream.write_all(&request).expect("write request");
    stream.flush().expect("flush request");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    let response = String::from_utf8_lossy(&response).into_owned();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|part| part.parse::<u16>().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status, body)
}

#[test]
fn prometheus_remote_write_ingests_counters_gauges_and_survives_reopen() {
    let path = PersistentDbPath::new("metrics_remote_write");
    let rt = path.open_runtime();
    exec(
        &rt,
        "CREATE METRICS sre RETENTION 30 d TENANT BY (tenant_id)",
    );

    let request = WriteRequest {
        timeseries: vec![
            TimeSeries {
                labels: vec![
                    label("__name__", "http_requests_total"),
                    label("job", "api"),
                    label("instance", "i1"),
                    label("service", "checkout"),
                    label("method", "GET"),
                    label("status", "200"),
                ],
                samples: vec![sample(10.0, 1_704_067_200_000)],
            },
            TimeSeries {
                labels: vec![
                    label("__name__", "process_resident_memory_bytes"),
                    label("job", "api"),
                    label("instance", "i1"),
                    label("service", "checkout"),
                ],
                samples: vec![sample(2048.0, 1_704_067_201_000)],
            },
        ],
    };

    let (status, body) = http_post_remote_write(rt.clone(), "sre", remote_write_body(&request));
    assert_eq!(status, 204, "unexpected remote_write response: {body}");
    assert!(
        body.is_empty(),
        "204 response should not carry a body: {body}"
    );

    let (status, metrics) = http_get(rt.clone(), "/metrics");
    assert_eq!(status, 200, "unexpected /metrics response: {metrics}");
    assert!(
        metrics.contains("reddb_metrics_remote_write_samples_accepted_total 2"),
        "accepted samples should be observable:\n{metrics}"
    );
    assert!(
        metrics.contains("reddb_metrics_remote_write_series_accepted_total 2"),
        "accepted series should be observable:\n{metrics}"
    );

    let reopened = checkpoint_and_reopen(&path, rt);
    let selected = exec(
        &reopened,
        "SELECT metric, value, timestamp, tags FROM sre WHERE metric = 'http_requests_total'",
    );
    assert_eq!(selected.result.records.len(), 1);
    let row = &selected.result.records[0];
    assert_eq!(text(row, "metric"), "http_requests_total");
    assert_eq!(uint(row, "timestamp"), 1_704_067_200_000_000_000);
    let tags = json_tags(row, "tags");
    assert_eq!(
        tags.get("service").and_then(reddb::json::Value::as_str),
        Some("checkout")
    );
    assert_eq!(
        tags.get("method").and_then(reddb::json::Value::as_str),
        Some("GET")
    );
    assert_eq!(
        tags.get("__reddb_kind")
            .and_then(reddb::json::Value::as_str),
        Some("counter")
    );
    assert_eq!(
        tags.get("__tenant_id").and_then(reddb::json::Value::as_str),
        Some("default")
    );
}

#[test]
fn prometheus_remote_write_rejects_invalid_payload_without_corrupting_data() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    exec(&rt, "CREATE METRICS sre RETENTION 7 d");

    let valid = WriteRequest {
        timeseries: vec![TimeSeries {
            labels: vec![label("__name__", "queue_depth"), label("queue", "billing")],
            samples: vec![sample(3.0, 1_704_067_200_000)],
        }],
    };
    let (status, body) = http_post_remote_write(rt.clone(), "sre", remote_write_body(&valid));
    assert_eq!(status, 204, "valid remote_write should succeed: {body}");

    let invalid = WriteRequest {
        timeseries: vec![TimeSeries {
            labels: vec![
                label("__name__", "queue_depth"),
                label("queue", "billing"),
                label("queue", "duplicate"),
            ],
            samples: vec![sample(4.0, 1_704_067_201_000)],
        }],
    };
    let (status, body) = http_post_remote_write(rt.clone(), "sre", remote_write_body(&invalid));
    assert_eq!(status, 400, "duplicate labels should be rejected: {body}");
    assert!(
        body.contains("duplicate label name"),
        "clear validation error expected: {body}"
    );

    let selected = exec(
        &rt,
        "SELECT metric, value, timestamp, tags FROM sre WHERE metric = 'queue_depth'",
    );
    assert_eq!(
        selected.result.records.len(),
        1,
        "invalid payload must not insert a partial sample"
    );

    let (status, metrics) = http_get(rt.clone(), "/metrics");
    assert_eq!(status, 200, "unexpected /metrics response: {metrics}");
    assert!(
        metrics.contains("reddb_metrics_remote_write_samples_accepted_total 1"),
        "accepted samples should remain observable:\n{metrics}"
    );
    assert!(
        metrics.contains("reddb_metrics_remote_write_samples_rejected_total 1"),
        "rejected samples should be observable:\n{metrics}"
    );
    assert!(
        metrics.contains("reddb_metrics_remote_write_series_rejected_total 1"),
        "rejected series should be observable:\n{metrics}"
    );
}
