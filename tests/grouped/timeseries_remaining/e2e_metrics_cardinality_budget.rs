#[path = "../../support/mod.rs"]
mod support;

use reddb::RedDBRuntime;
use serde_json::Value as JsonValue;
use support::prometheus::{
    encode_query_value, get, label, post_remote_write, sample, TimeSeries, WriteRequest,
};
use support::{checkpoint_and_reopen, PersistentDbPath};

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"));
}

fn series(instance: &str, value: f64) -> TimeSeries {
    TimeSeries {
        labels: vec![
            label("__name__", "http_requests_total"),
            label("job", "api"),
            label("service", "checkout"),
            label("instance", instance),
        ],
        samples: vec![sample(value, 1_704_067_200_000)],
    }
}

fn write_request(instances: &[(&str, f64)]) -> WriteRequest {
    WriteRequest {
        timeseries: instances
            .iter()
            .map(|(instance, value)| series(instance, *value))
            .collect(),
    }
}

fn prom_query(rt: &RedDBRuntime, promql: &str) -> JsonValue {
    let path = format!("/api/v1/query?query={}", encode_query_value(promql));
    let (status, body) = get(rt.clone(), &path);
    assert_eq!(status, 200, "response={body}");
    serde_json::from_str(&body).expect("json")
}

#[test]
fn cardinality_budget_partially_accepts_and_persists_series_registry() {
    let _lock = crate::timeseries_remaining_shared::metrics_env_lock();
    let _budget = crate::timeseries_remaining_shared::EnvGuard::set(
        "REDDB_METRICS_MAX_SERIES_PER_METRIC",
        "2",
    );
    let path = PersistentDbPath::new("metrics_cardinality_budget");
    let rt = path.open_runtime();
    exec(&rt, "CREATE METRICS sre RETENTION 30 d");

    let request = write_request(&[("i1", 1.0), ("i2", 2.0), ("i3", 3.0)]);
    let (status, body) = post_remote_write(rt.clone(), "sre", &request);
    assert_eq!(
        status, 204,
        "partial budget rejection should still accept valid series: {body}"
    );

    let response = prom_query(&rt, "http_requests_total");
    let results = response["data"]["result"].as_array().expect("vector");
    assert_eq!(results.len(), 2, "only two series should be admitted");
    let instances = results
        .iter()
        .map(|item| item["metric"]["instance"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(instances.contains(&"i1"));
    assert!(instances.contains(&"i2"));
    assert!(!instances.contains(&"i3"));

    let (status, metrics) = get(rt.clone(), "/metrics");
    assert_eq!(status, 200, "metrics={metrics}");
    assert!(
        metrics.contains("reddb_metrics_remote_write_series_rejected_total 1"),
        "total rejection counter should include over-budget series:\n{metrics}"
    );
    assert!(
        metrics.contains(
            "reddb_metrics_remote_write_series_rejected_by_reason_total{reason=\"cardinality_budget\"} 1"
        ),
        "reason-labelled rejection counter should be observable:\n{metrics}"
    );

    let reopened = checkpoint_and_reopen(&path, rt);
    let request = write_request(&[("i4", 4.0)]);
    let (status, body) = post_remote_write(reopened.clone(), "sre", &request);
    assert_eq!(
        status, 204,
        "over-budget reopen request should be rejected without failing batch: {body}"
    );
    let response = prom_query(&reopened, "http_requests_total");
    assert_eq!(
        response["data"]["result"].as_array().expect("vector").len(),
        2,
        "reopened runtime must count existing series against budget"
    );
}
