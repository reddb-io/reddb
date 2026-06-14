#[path = "../../support/mod.rs"]
mod support;

use reddb::RedDBRuntime;
use serde_json::Value as JsonValue;
use support::prometheus::{
    encode_query_value, get, get_with_headers, label, post_remote_write,
    post_remote_write_with_headers, sample, TimeSeries, WriteRequest,
};

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"));
}

fn counter_series(service: &str, status: &str, values: &[(f64, i64)]) -> TimeSeries {
    TimeSeries {
        labels: vec![
            label("__name__", "http_requests_total"),
            label("job", "api"),
            label("service", service),
            label("status", status),
            label("instance", "i1"),
        ],
        samples: values
            .iter()
            .map(|(value, timestamp)| sample(*value, *timestamp))
            .collect(),
    }
}

fn gauge_series(metric: &str, service: &str, values: &[(f64, i64)]) -> TimeSeries {
    TimeSeries {
        labels: vec![
            label("__name__", metric),
            label("job", "api"),
            label("service", service),
            label("instance", "i1"),
        ],
        samples: values
            .iter()
            .map(|(value, timestamp)| sample(*value, *timestamp))
            .collect(),
    }
}

fn histogram_bucket(le: &str, count: f64) -> TimeSeries {
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

fn prom_query(rt: &RedDBRuntime, tenant: &str, promql: &str, time: &str) -> (u16, JsonValue) {
    let path = format!(
        "/api/v1/query?query={}&time={}",
        encode_query_value(promql),
        encode_query_value(time)
    );
    let (status, body) = get_with_headers(
        rt.clone(),
        &path,
        &[("X-RedDB-Tenant", tenant), ("X-RedDB-Namespace", "prod")],
    );
    let json = serde_json::from_str(&body).unwrap_or_else(|err| {
        panic!("Prometheus response should be JSON, status={status}, err={err}, body={body}")
    });
    (status, json)
}

fn query_range(
    rt: &RedDBRuntime,
    tenant: &str,
    promql: &str,
    start: &str,
    end: &str,
    step: &str,
) -> (u16, JsonValue) {
    let path = format!(
        "/api/v1/query_range?query={}&start={}&end={}&step={}",
        encode_query_value(promql),
        encode_query_value(start),
        encode_query_value(end),
        encode_query_value(step)
    );
    let (status, body) = get_with_headers(
        rt.clone(),
        &path,
        &[("X-RedDB-Tenant", tenant), ("X-RedDB-Namespace", "prod")],
    );
    let json = serde_json::from_str(&body).unwrap_or_else(|err| {
        panic!("Prometheus range response should be JSON, status={status}, err={err}, body={body}")
    });
    (status, json)
}

fn first_value(response: &JsonValue) -> &str {
    response["data"]["result"][0]["value"][1]
        .as_str()
        .expect("value string")
}

#[test]
fn grafana_prometheus_datasource_panels_render_representative_metrics() {
    let _lock = crate::timeseries_remaining_shared::metrics_env_lock();
    let rt = RedDBRuntime::in_memory().expect("runtime");
    exec(
        &rt,
        "CREATE METRICS sre RETENTION 30 d DOWNSAMPLE 60s:raw:avg",
    );

    let request = WriteRequest {
        timeseries: vec![
            counter_series(
                "checkout",
                "200",
                &[
                    (0.0, 1_704_067_200_000),
                    (100.0, 1_704_067_210_000),
                    (180.0, 1_704_067_220_000),
                ],
            ),
            counter_series(
                "billing",
                "500",
                &[
                    (0.0, 1_704_067_200_000),
                    (50.0, 1_704_067_210_000),
                    (70.0, 1_704_067_220_000),
                ],
            ),
            gauge_series(
                "process_resident_memory_bytes",
                "checkout",
                &[(2048.0, 1_704_067_220_000)],
            ),
            gauge_series(
                "long_range_temperature_celsius",
                "checkout",
                &[
                    (10.0, 1_704_067_200_000),
                    (20.0, 1_704_067_210_000),
                    (30.0, 1_704_067_220_000),
                ],
            ),
            histogram_bucket("0.1", 10.0),
            histogram_bucket("0.5", 50.0),
            histogram_bucket("1", 90.0),
            histogram_bucket("2.5", 100.0),
            histogram_bucket("+Inf", 100.0),
        ],
    };
    let (status, body) = post_remote_write_with_headers(
        rt.clone(),
        "sre",
        &request,
        &[("X-RedDB-Tenant", "acme"), ("X-RedDB-Namespace", "prod")],
    );
    assert_eq!(status, 204, "remote_write fixture should ingest: {body}");

    let globex = WriteRequest {
        timeseries: vec![gauge_series(
            "process_resident_memory_bytes",
            "checkout",
            &[(999.0, 1_704_067_220_000)],
        )],
    };
    let (status, body) = post_remote_write_with_headers(
        rt.clone(),
        "sre",
        &globex,
        &[("X-RedDB-Tenant", "globex"), ("X-RedDB-Namespace", "prod")],
    );
    assert_eq!(status, 204, "second tenant fixture should ingest: {body}");

    let (status, response) = prom_query(
        &rt,
        "acme",
        r#"process_resident_memory_bytes{service="checkout"}"#,
        "1704067220",
    );
    assert_eq!(status, 200, "selector panel failed: {response}");
    assert_eq!(first_value(&response), "2048");

    let (status, response) = query_range(
        &rt,
        "acme",
        r#"http_requests_total{service="checkout"}"#,
        "1704067200",
        "1704067220",
        "10s",
    );
    assert_eq!(status, 200, "range panel failed: {response}");
    assert_eq!(
        response["data"]["result"][0]["values"],
        serde_json::json!([[1704067200, "0"], [1704067210, "100"], [1704067220, "180"]])
    );

    let (status, response) = prom_query(
        &rt,
        "acme",
        "sum by (service) (rate(http_requests_total[20s]))",
        "1704067220",
    );
    assert_eq!(status, 200, "counter/aggregation panel failed: {response}");
    let services = response["data"]["result"]
        .as_array()
        .expect("vector result")
        .iter()
        .map(|sample| sample["metric"]["service"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(services.contains(&"billing"), "response={response}");
    assert!(services.contains(&"checkout"), "response={response}");

    let (status, response) = prom_query(
        &rt,
        "acme",
        "histogram_quantile(0.95, rate(http_request_duration_seconds_bucket[10s]))",
        "1704067210",
    );
    assert_eq!(status, 200, "histogram panel failed: {response}");
    let p95 = first_value(&response).parse::<f64>().expect("p95");
    assert!((p95 - 1.75).abs() < 0.000_001, "response={response}");

    let (status, response) = query_range(
        &rt,
        "acme",
        "long_range_temperature_celsius",
        "1704067200",
        "1704067200",
        "60s",
    );
    assert_eq!(status, 200, "rollup-backed long range failed: {response}");
    assert_eq!(response["data"]["result"][0]["values"][0][1], "20");

    let (status, response) = prom_query(
        &rt,
        "globex",
        r#"process_resident_memory_bytes{service="checkout"}"#,
        "1704067220",
    );
    assert_eq!(status, 200, "tenant panel failed: {response}");
    assert_eq!(first_value(&response), "999");

    let (status, response) = prom_query(
        &rt,
        "acme",
        "rate(http_requests_total[20s]) / rate(http_requests_total[20s])",
        "1704067220",
    );
    assert_eq!(
        status, 422,
        "unsupported vector matching should fail: {response}"
    );
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("vector matching")),
        "failure should point to the aggregation/arithmetic slice: {response}"
    );
}

#[test]
fn grafana_smoke_surfaces_cardinality_rejection_visibility() {
    let _lock = crate::timeseries_remaining_shared::metrics_env_lock();
    let _budget = crate::timeseries_remaining_shared::EnvGuard::set(
        "REDDB_METRICS_MAX_SERIES_PER_METRIC",
        "1",
    );
    let rt = RedDBRuntime::in_memory().expect("runtime");
    exec(&rt, "CREATE METRICS sre RETENTION 30 d");

    let request = WriteRequest {
        timeseries: vec![
            counter_series("checkout", "200", &[(1.0, 1_704_067_200_000)]),
            TimeSeries {
                labels: vec![
                    label("__name__", "http_requests_total"),
                    label("job", "api"),
                    label("service", "checkout"),
                    label("status", "200"),
                    label("instance", "i2"),
                ],
                samples: vec![sample(2.0, 1_704_067_200_000)],
            },
        ],
    };
    let (status, body) = post_remote_write(rt.clone(), "sre", &request);
    assert_eq!(
        status, 204,
        "partial cardinality accept should not fail batch: {body}"
    );

    let (status, metrics) = get(rt.clone(), "/metrics");
    assert_eq!(status, 200, "metrics endpoint should respond");
    assert!(
        metrics.contains(
            "reddb_metrics_remote_write_series_rejected_by_reason_total{reason=\"cardinality_budget\"} 1"
        ),
        "cardinality rejection visibility belongs to issue #490:\n{metrics}"
    );
}
