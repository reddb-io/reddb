#[path = "../../support/mod.rs"]
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
                    label("service", "checkout"),
                    label("instance", "i1"),
                ],
                samples: vec![
                    sample(0.0, 1_704_067_200_000),
                    sample(10.0, 1_704_067_210_000),
                    sample(20.0, 1_704_067_220_000),
                ],
            },
            TimeSeries {
                labels: vec![
                    label("__name__", "http_requests_total"),
                    label("job", "api"),
                    label("service", "checkout"),
                    label("instance", "i2"),
                ],
                samples: vec![
                    sample(0.0, 1_704_067_200_000),
                    sample(30.0, 1_704_067_210_000),
                    sample(50.0, 1_704_067_220_000),
                ],
            },
            TimeSeries {
                labels: vec![
                    label("__name__", "http_requests_total"),
                    label("job", "api"),
                    label("service", "billing"),
                    label("instance", "i1"),
                ],
                samples: vec![
                    sample(0.0, 1_704_067_200_000),
                    sample(20.0, 1_704_067_210_000),
                    sample(40.0, 1_704_067_220_000),
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

fn result_for_service<'a>(results: &'a [JsonValue], service: &str) -> &'a JsonValue {
    results
        .iter()
        .find(|result| result["metric"]["service"] == service)
        .unwrap_or_else(|| panic!("missing service={service} in {results:?}"))
}

#[test]
fn prometheus_aggregation_grouping_and_arithmetic_work_for_instant_queries() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    ingest_fixture(&rt);

    let (status, response) = prom_query(
        &rt,
        "sum by (service) (rate(http_requests_total[10s]))",
        "1704067210",
    );
    assert_eq!(status, 200, "response={response}");
    let results = response["data"]["result"].as_array().expect("vector");
    assert_eq!(results.len(), 2);
    assert_eq!(result_for_service(results, "checkout")["value"][1], "4");
    assert_eq!(result_for_service(results, "billing")["value"][1], "2");

    let (status, response) = prom_query(
        &rt,
        "avg without (instance) (rate(http_requests_total[10s]))",
        "1704067210",
    );
    assert_eq!(status, 200, "response={response}");
    let results = response["data"]["result"].as_array().expect("vector");
    assert_eq!(result_for_service(results, "checkout")["value"][1], "2");

    let (status, response) = prom_query(
        &rt,
        "sum by (service) (rate(http_requests_total[10s])) * 60",
        "1704067210",
    );
    assert_eq!(status, 200, "response={response}");
    let results = response["data"]["result"].as_array().expect("vector");
    assert_eq!(result_for_service(results, "checkout")["value"][1], "240");

    let (status, response) = prom_query(&rt, "count(rate(http_requests_total[10s]))", "1704067210");
    assert_eq!(status, 200, "response={response}");
    assert_eq!(response["data"]["result"][0]["value"][1], "3");
}

#[test]
fn prometheus_aggregation_range_and_unsupported_vector_matching_are_clear() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    ingest_fixture(&rt);

    let (status, response) = query_range(
        &rt,
        "sum by (service) (rate(http_requests_total[10s]))",
        "1704067210",
        "1704067220",
        "10s",
    );
    assert_eq!(status, 200, "response={response}");
    assert_eq!(response["data"]["resultType"], "matrix");
    let results = response["data"]["result"].as_array().expect("matrix");
    assert_eq!(
        result_for_service(results, "checkout")["values"],
        serde_json::json!([[1704067210, "4"], [1704067220, "3"]])
    );

    let (status, response) = prom_query(
        &rt,
        "rate(http_requests_total[10s]) / rate(http_requests_total[10s])",
        "1704067210",
    );
    assert_eq!(status, 422, "response={response}");
    assert_eq!(response["status"], "error");
    assert_eq!(response["errorType"], "bad_data");
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("vector matching")),
        "clear vector matching error expected: {response}"
    );
}
