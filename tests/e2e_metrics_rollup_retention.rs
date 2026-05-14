mod support;

use reddb::catalog::CollectionModel;
use reddb::storage::EntityData;
use reddb::RedDBRuntime;
use serde_json::Value as JsonValue;
use support::prometheus::{
    encode_query_value, get, label, post_remote_write, sample, TimeSeries, WriteRequest,
};

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"));
}

fn ingest(rt: &RedDBRuntime, collection: &str, samples: Vec<(f64, i64)>) {
    let request = WriteRequest {
        timeseries: vec![TimeSeries {
            labels: vec![
                label("__name__", "http_requests_total"),
                label("job", "api"),
                label("instance", "i1"),
                label("service", "checkout"),
            ],
            samples: samples
                .into_iter()
                .map(|(value, timestamp)| sample(value, timestamp))
                .collect(),
        }],
    };
    let (status, body) = post_remote_write(rt.clone(), collection, &request);
    assert_eq!(status, 204, "remote_write should ingest: {body}");
}

fn query_range(rt: &RedDBRuntime, promql: &str, start: &str, end: &str, step: &str) -> JsonValue {
    let path = format!(
        "/api/v1/query_range?query={}&start={}&end={}&step={}",
        encode_query_value(promql),
        encode_query_value(start),
        encode_query_value(end),
        encode_query_value(step)
    );
    let (status, body) = get(rt.clone(), &path);
    assert_eq!(status, 200, "response body: {body}");
    serde_json::from_str(&body).expect("query_range response should be JSON")
}

fn only_values(response: &JsonValue) -> &JsonValue {
    assert_eq!(response["status"], "success");
    assert_eq!(response["data"]["resultType"], "matrix");
    let results = response["data"]["result"]
        .as_array()
        .expect("matrix result should be an array");
    assert_eq!(results.len(), 1, "expected one series: {response}");
    &results[0]["values"]
}

#[test]
fn metrics_collection_declares_raw_ttl_and_rollup_tier() {
    let rt = RedDBRuntime::in_memory().expect("runtime");

    exec(
        &rt,
        "CREATE METRICS sre RETENTION 1 h DOWNSAMPLE 60s:raw:avg",
    );

    let contract = rt
        .db()
        .collection_contract("sre")
        .expect("metrics contract should exist");
    assert_eq!(contract.declared_model, CollectionModel::Metrics);
    assert_eq!(contract.metrics_raw_retention_ms, Some(3_600_000));
    assert_eq!(contract.metrics_rollup_policies, vec!["60s:raw:avg"]);
}

#[test]
fn query_range_uses_raw_data_when_no_rollup_is_declared() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    exec(&rt, "CREATE METRICS sre RETENTION 30 d");
    ingest(
        &rt,
        "sre",
        vec![
            (10.0, 1_704_067_200_000),
            (20.0, 1_704_067_210_000),
            (30.0, 1_704_067_220_000),
        ],
    );

    let response = query_range(
        &rt,
        "http_requests_total",
        "1704067200",
        "1704067220",
        "10s",
    );

    assert_eq!(
        only_values(&response),
        &serde_json::json!([[1704067200, "10"], [1704067210, "20"], [1704067220, "30"]])
    );
}

#[test]
fn query_range_selects_rollup_when_step_preserves_requested_resolution() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    exec(
        &rt,
        "CREATE METRICS sre RETENTION 30 d DOWNSAMPLE 60s:raw:avg",
    );
    ingest(
        &rt,
        "sre",
        vec![
            (10.0, 1_704_067_200_000),
            (20.0, 1_704_067_210_000),
            (30.0, 1_704_067_220_000),
        ],
    );

    let response = query_range(
        &rt,
        "http_requests_total",
        "1704067200",
        "1704067200",
        "60s",
    );

    assert_eq!(
        only_values(&response),
        &serde_json::json!([[1704067200, "20"]])
    );
}

#[test]
fn expired_raw_metrics_are_removed_without_removing_rollups() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    exec(
        &rt,
        "CREATE METRICS sre RETENTION 1 s DOWNSAMPLE 60s:raw:avg",
    );

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let bucket_ms = ((now_ms / 60_000) - 2) * 60_000;
    ingest(
        &rt,
        "sre",
        vec![(100.0, bucket_ms), (200.0, bucket_ms + 10_000)],
    );

    rt.apply_retention_policy()
        .expect("metrics raw retention sweep should succeed");

    let raw = rt
        .db()
        .store()
        .get_collection("sre")
        .expect("raw metrics collection should exist")
        .query_all(|entity| matches!(entity.data, EntityData::TimeSeries(_)));
    assert!(raw.is_empty(), "raw samples should be expired");

    let rollup = rt
        .db()
        .store()
        .get_collection("red_metrics_rollup_sre_60s")
        .expect("rollup collection should exist")
        .query_all(|entity| matches!(entity.data, EntityData::TimeSeries(_)));
    assert_eq!(rollup.len(), 1, "rollup sample should survive raw TTL");

    let bucket_seconds = (bucket_ms / 1000).to_string();
    let response = query_range(
        &rt,
        "http_requests_total",
        &bucket_seconds,
        &bucket_seconds,
        "60s",
    );
    assert_eq!(
        only_values(&response),
        &serde_json::json!([[bucket_ms / 1000, "150"]])
    );
}
