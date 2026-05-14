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
                    label("status", "200"),
                ],
                samples: vec![
                    sample(10.0, 1_704_067_200_000),
                    sample(11.0, 1_704_067_210_000),
                ],
            },
            TimeSeries {
                labels: vec![
                    label("__name__", "http_requests_total"),
                    label("job", "api"),
                    label("instance", "i2"),
                    label("service", "billing"),
                    label("method", "POST"),
                    label("status", "500"),
                ],
                samples: vec![sample(5.0, 1_704_067_205_000)],
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
    let (status, body) = post_remote_write(rt.clone(), "sre", &request);
    assert_eq!(status, 204, "remote_write fixture should ingest: {body}");
}

fn prom_query(rt: &RedDBRuntime, promql: &str) -> (u16, JsonValue) {
    let path = format!("/api/v1/query?query={}", encode_query_value(promql));
    let (status, body) = get(rt.clone(), &path);
    let json = serde_json::from_str(&body).unwrap_or_else(|err| {
        panic!("Prometheus response should be JSON, status={status}, err={err}, body={body}")
    });
    (status, json)
}

fn vector_results(response: &JsonValue) -> &[JsonValue] {
    assert_eq!(response["status"], "success");
    assert_eq!(response["data"]["resultType"], "vector");
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
fn prometheus_query_selects_metric_and_equality_matchers_from_remote_write_data() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    ingest_fixture(&rt);

    let (status, response) = prom_query(&rt, "http_requests_total");
    assert_eq!(status, 200, "response={response}");
    let results = vector_results(&response);
    assert_eq!(
        results.len(),
        2,
        "metric selector should return both series"
    );

    let checkout = result_for_service(results, "checkout");
    assert_eq!(checkout["metric"]["__name__"], "http_requests_total");
    assert_eq!(checkout["metric"]["method"], "GET");
    assert_eq!(checkout["value"][1], "11");
    assert_eq!(checkout["value"][0].as_f64(), Some(1_704_067_210.0));

    let (status, response) = prom_query(
        &rt,
        r#"http_requests_total{service="checkout",method!="POST"}"#,
    );
    assert_eq!(status, 200, "response={response}");
    let results = vector_results(&response);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["metric"]["service"], "checkout");
    assert_eq!(results[0]["metric"]["method"], "GET");
}

#[test]
fn prometheus_query_supports_negative_matchers_and_prometheus_error_shape() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    ingest_fixture(&rt);

    let (status, response) = prom_query(&rt, r#"http_requests_total{service!="checkout"}"#);
    assert_eq!(status, 200, "response={response}");
    let results = vector_results(&response);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["metric"]["service"], "billing");
    assert_eq!(results[0]["value"][1], "5");

    let (status, response) = prom_query(&rt, "sum(http_requests_total)");
    assert_eq!(status, 422, "response={response}");
    assert_eq!(response["status"], "error");
    assert_eq!(response["errorType"], "bad_data");
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("only instant metric selectors are supported")),
        "clear unsupported PromQL error expected: {response}"
    );
}
