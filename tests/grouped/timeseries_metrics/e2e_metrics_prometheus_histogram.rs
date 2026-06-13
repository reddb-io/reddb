#[path = "../../support/mod.rs"]
mod support;

use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use serde_json::Value as JsonValue;
use support::prometheus::{
    encode_query_value, get, label, post_remote_write, sample, TimeSeries, WriteRequest,
};

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn histogram_series(le: &str, count: f64) -> TimeSeries {
    TimeSeries {
        labels: vec![
            label("__name__", "http_request_duration_seconds_bucket"),
            label("job", "api"),
            label("service", "checkout"),
            label("le", le),
        ],
        samples: vec![
            sample(0.0, 1_704_067_200_000),
            sample(count, 1_704_067_210_000),
        ],
    }
}

fn ingest_histogram_fixture(rt: &RedDBRuntime) {
    exec(rt, "CREATE METRICS sre RETENTION 30 d");
    let request = WriteRequest {
        timeseries: vec![
            histogram_series("0.1", 10.0),
            histogram_series("0.5", 50.0),
            histogram_series("1", 90.0),
            histogram_series("2.5", 100.0),
            histogram_series("+Inf", 100.0),
            TimeSeries {
                labels: vec![
                    label("__name__", "http_request_duration_seconds_sum"),
                    label("job", "api"),
                    label("service", "checkout"),
                ],
                samples: vec![
                    sample(0.0, 1_704_067_200_000),
                    sample(45.0, 1_704_067_210_000),
                ],
            },
            TimeSeries {
                labels: vec![
                    label("__name__", "http_request_duration_seconds_count"),
                    label("job", "api"),
                    label("service", "checkout"),
                ],
                samples: vec![
                    sample(0.0, 1_704_067_200_000),
                    sample(100.0, 1_704_067_210_000),
                ],
            },
        ],
    };
    let (status, body) = post_remote_write(rt.clone(), "sre", &request);
    assert_eq!(status, 204, "remote_write fixture should ingest: {body}");
}

fn prom_query(rt: &RedDBRuntime, promql: &str, time: &str) -> (u16, JsonValue) {
    let path = format!(
        "/api/v1/query?query={}&time={}",
        encode_query_value(promql),
        encode_query_value(time)
    );
    let (status, body) = get(rt.clone(), &path);
    let json = serde_json::from_str(&body).unwrap_or_else(|err| {
        panic!("Prometheus response should be JSON, status={status}, err={err}, body={body}")
    });
    (status, json)
}

fn query_range(rt: &RedDBRuntime, promql: &str) -> (u16, JsonValue) {
    let path = format!(
        "/api/v1/query_range?query={}&start=1704067210&end=1704067210&step=10s",
        encode_query_value(promql)
    );
    let (status, body) = get(rt.clone(), &path);
    let json = serde_json::from_str(&body).unwrap_or_else(|err| {
        panic!("Prometheus range response should be JSON, status={status}, err={err}, body={body}")
    });
    (status, json)
}

fn first_vector_value(response: &JsonValue) -> f64 {
    response["data"]["result"][0]["value"][1]
        .as_str()
        .expect("value string")
        .parse::<f64>()
        .expect("value f64")
}

#[test]
fn classic_histogram_ingests_buckets_sum_count_and_preserves_tags() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    ingest_histogram_fixture(&rt);

    let selected = exec(
        &rt,
        "SELECT metric, tags FROM sre WHERE metric = 'http_request_duration_seconds_bucket'",
    );
    assert_eq!(selected.result.records.len(), 10);
    let tags: reddb::json::Value = match selected.result.records[0].get("tags") {
        Some(Value::Json(bytes)) => reddb::json::from_slice(bytes).expect("tags json"),
        other => panic!("expected JSON tags, got {other:?}"),
    };
    assert_eq!(
        tags.get("service").and_then(reddb::json::Value::as_str),
        Some("checkout")
    );
    assert!(tags
        .get("le")
        .and_then(reddb::json::Value::as_str)
        .is_some());
    assert_eq!(
        tags.get("__tenant_id").and_then(reddb::json::Value::as_str),
        Some("default")
    );
    assert_eq!(
        tags.get("__reddb_kind")
            .and_then(reddb::json::Value::as_str),
        Some("counter")
    );
}

#[test]
fn histogram_quantile_returns_expected_p50_p95_p99_and_range_matrix() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    ingest_histogram_fixture(&rt);

    for (quantile, expected) in [("0.50", 0.5), ("0.95", 1.75), ("0.99", 2.35)] {
        let promql = format!(
            "histogram_quantile({quantile}, rate(http_request_duration_seconds_bucket[10s]))"
        );
        let (status, response) = prom_query(&rt, &promql, "1704067210");
        assert_eq!(status, 200, "response={response}");
        assert_eq!(response["data"]["resultType"], "vector");
        let actual = first_vector_value(&response);
        assert!(
            (actual - expected).abs() < 0.000_001,
            "quantile={quantile} response={response}"
        );
    }

    let (status, response) = query_range(
        &rt,
        "histogram_quantile(0.95, rate(http_request_duration_seconds_bucket[10s]))",
    );
    assert_eq!(status, 200, "response={response}");
    assert_eq!(response["data"]["resultType"], "matrix");
    assert_eq!(response["data"]["result"][0]["values"][0][1], "1.75");
}
