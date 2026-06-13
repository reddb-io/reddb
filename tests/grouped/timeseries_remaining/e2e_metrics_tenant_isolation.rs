#[path = "../../support/mod.rs"]
mod support;

use reddb::storage::EntityData;
use reddb::RedDBRuntime;
use serde_json::Value as JsonValue;
use support::prometheus::{
    encode_query_value, get, get_with_headers, label, post_remote_write_with_headers, sample,
    TimeSeries, WriteRequest,
};

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"));
}

fn write_same_label_series(rt: &RedDBRuntime, tenant: &str, namespace: &str, value: f64) {
    let request = WriteRequest {
        timeseries: vec![TimeSeries {
            labels: vec![
                label("__name__", "up"),
                label("job", "api"),
                label("instance", "shared"),
            ],
            samples: vec![sample(value, 1_704_067_200_000)],
        }],
    };
    let (status, body) = post_remote_write_with_headers(
        rt.clone(),
        "sre",
        &request,
        &[("X-RedDB-Tenant", tenant), ("X-RedDB-Namespace", namespace)],
    );
    assert_eq!(status, 204, "remote_write should ingest: {body}");
}

fn query(rt: &RedDBRuntime, tenant: &str, namespace: &str, promql: &str) -> JsonValue {
    let path = format!("/api/v1/query?query={}", encode_query_value(promql));
    let (status, body) = get_with_headers(
        rt.clone(),
        &path,
        &[("X-RedDB-Tenant", tenant), ("X-RedDB-Namespace", namespace)],
    );
    assert_eq!(status, 200, "response body: {body}");
    serde_json::from_str(&body).expect("query response should be JSON")
}

fn vector_values(response: &JsonValue) -> Vec<String> {
    assert_eq!(response["status"], "success");
    assert_eq!(response["data"]["resultType"], "vector");
    response["data"]["result"]
        .as_array()
        .expect("vector result should be an array")
        .iter()
        .map(|sample| sample["value"][1].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn metrics_ingest_and_query_are_isolated_by_tenant_and_namespace() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    exec(&rt, "CREATE METRICS sre RETENTION 30 d");

    write_same_label_series(&rt, "acme", "prod", 1.0);
    write_same_label_series(&rt, "globex", "prod", 2.0);
    write_same_label_series(&rt, "acme", "staging", 3.0);

    let raw = rt
        .db()
        .store()
        .get_collection("sre")
        .expect("metrics collection should exist")
        .query_all(|entity| matches!(entity.data, EntityData::TimeSeries(_)));
    let mut internal_scopes = raw
        .iter()
        .map(|entity| match &entity.data {
            EntityData::TimeSeries(point) => (
                point.tags.get("__tenant_id").cloned().unwrap_or_default(),
                point.tags.get("__namespace").cloned().unwrap_or_default(),
            ),
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    internal_scopes.sort();
    assert_eq!(
        internal_scopes,
        vec![
            ("acme".to_string(), "prod".to_string()),
            ("acme".to_string(), "staging".to_string()),
            ("globex".to_string(), "prod".to_string()),
        ]
    );

    assert_eq!(vector_values(&query(&rt, "acme", "prod", "up")), vec!["1"]);
    assert_eq!(
        vector_values(&query(&rt, "globex", "prod", "up")),
        vec!["2"]
    );
    assert_eq!(
        vector_values(&query(&rt, "acme", "staging", "up")),
        vec!["3"]
    );

    let guessed = query(&rt, "acme", "prod", r#"up{__tenant_id="globex"}"#);
    assert!(
        vector_values(&guessed).is_empty(),
        "tenant is not readable or selectable as a Prometheus label"
    );

    let (status, body) = get(rt.clone(), "/api/v1/query?query=up");
    assert_eq!(status, 200, "default-scope query should succeed: {body}");
    let default_response: JsonValue = serde_json::from_str(&body).expect("json");
    assert!(
        vector_values(&default_response).is_empty(),
        "unscoped default tenant must not see tenant-scoped data"
    );

    let (status, metrics) = get(rt.clone(), "/metrics");
    assert_eq!(status, 200, "metrics endpoint should respond");
    assert!(
        metrics.contains(
            "reddb_metrics_tenant_activity_total{tenant=\"acme\",namespace=\"prod\",operation=\"ingest\"} 1"
        ),
        "ingest activity should include tenant/namespace labels:\n{metrics}"
    );
    assert!(
        metrics.contains(
            "reddb_metrics_tenant_activity_total{tenant=\"acme\",namespace=\"prod\",operation=\"query\"} 2"
        ),
        "query activity should include tenant/namespace labels:\n{metrics}"
    );
}
