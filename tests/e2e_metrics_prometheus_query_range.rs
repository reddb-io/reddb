mod support;

use reddb::RedDBRuntime;
use serde_json::Value as JsonValue;
use support::prometheus::{
    encode_query_value, get, label, post_remote_write, sample, TimeSeries, WriteRequest,
};

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"));
}

fn ingest_fixture(rt: &RedDBRuntime) {
    exec(rt, "CREATE METRICS sre RETENTION 30 d");
    let request = WriteRequest {
        timeseries: vec![
            TimeSeries {
                labels: vec![
                    label("__name__", "http_requests_total"),
                    label("job", "api"),
                    label("instance", "i1"),
                    label("service", "checkout"),
                    label("method", "GET"),
                ],
                samples: vec![
                    sample(10.0, 1_704_067_200_000),
                    sample(11.0, 1_704_067_205_000),
                    sample(12.0, 1_704_067_210_000),
                    sample(20.0, 1_704_067_220_000),
                ],
            },
            TimeSeries {
                labels: vec![
                    label("__name__", "http_requests_total"),
                    label("job", "api"),
                    label("instance", "i2"),
                    label("service", "billing"),
                    label("method", "POST"),
                ],
                samples: vec![
                    sample(5.0, 1_704_067_200_000),
                    sample(7.0, 1_704_067_210_000),
                ],
            },
        ],
    };
    let (status, body) = post_remote_write(rt.clone(), "sre", &request);
    assert_eq!(status, 204, "remote_write fixture should ingest: {body}");
}

fn query_range(
    rt: &RedDBRuntime,
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
    let (status, body) = get(rt.clone(), &path);
    let json = serde_json::from_str(&body).unwrap_or_else(|err| {
        panic!("Prometheus range response should be JSON, status={status}, err={err}, body={body}")
    });
    (status, json)
}

fn matrix_results(response: &JsonValue) -> &[JsonValue] {
    assert_eq!(response["status"], "success");
    assert_eq!(response["data"]["resultType"], "matrix");
    response["data"]["result"]
        .as_array()
        .expect("result should be an array")
}

fn result_for_service<'a>(results: &'a [JsonValue], service: &str) -> &'a JsonValue {
    results
        .iter()
        .find(|result| result["metric"]["service"] == service)
        .unwrap_or_else(|| panic!("missing service={service} in {results:?}"))
}

#[test]
fn prometheus_query_range_returns_step_aligned_matrix_for_grafana_panel_fixture() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    ingest_fixture(&rt);

    let (status, response) = query_range(
        &rt,
        "http_requests_total",
        "1704067200",
        "1704067220",
        "10s",
    );
    assert_eq!(status, 200, "response={response}");
    let results = matrix_results(&response);
    assert_eq!(results.len(), 2, "Grafana panel should see both series");

    let checkout = result_for_service(results, "checkout");
    assert_eq!(checkout["metric"]["__name__"], "http_requests_total");
    assert_eq!(
        checkout["values"],
        serde_json::json!([[1704067200, "10"], [1704067210, "12"], [1704067220, "20"]])
    );

    let billing = result_for_service(results, "billing");
    assert_eq!(
        billing["values"],
        serde_json::json!([[1704067200, "5"], [1704067210, "7"], [1704067220, "7"]])
    );
}

#[test]
fn prometheus_query_range_filters_labels_and_rejects_bad_ranges() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    ingest_fixture(&rt);

    let (status, response) = query_range(
        &rt,
        r#"http_requests_total{service="checkout",method!="POST"}"#,
        "1704067200",
        "1704067220",
        "10",
    );
    assert_eq!(status, 200, "response={response}");
    let results = matrix_results(&response);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["metric"]["service"], "checkout");

    let (status, response) =
        query_range(&rt, "http_requests_total", "1704067220", "1704067200", "10");
    assert_eq!(status, 400, "response={response}");
    assert_eq!(response["status"], "error");
    assert_eq!(response["errorType"], "bad_data");
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("end must be greater")),
        "clear invalid range error expected: {response}"
    );

    let (status, response) = query_range(
        &rt,
        "histogram_quantile(0.99, http_requests_total)",
        "1704067200",
        "1704067220",
        "10",
    );
    assert_eq!(status, 200, "response={response}");
    let results = matrix_results(&response);
    assert!(results.is_empty(), "non-bucket input has no quantiles");
}
