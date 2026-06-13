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

fn ingest_counter_reset_fixture(rt: &RedDBRuntime) {
    exec(rt, "CREATE METRICS sre RETENTION 30 d");
    let request = WriteRequest {
        timeseries: vec![TimeSeries {
            labels: vec![
                label("__name__", "http_requests_total"),
                label("job", "api"),
                label("instance", "i1"),
                label("service", "checkout"),
            ],
            samples: vec![
                sample(0.0, 1_704_067_200_000),
                sample(10.0, 1_704_067_210_000),
                sample(2.0, 1_704_067_220_000),
                sample(8.0, 1_704_067_230_000),
            ],
        }],
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

fn vector_value(response: &JsonValue) -> Option<&str> {
    response["data"]["result"]
        .as_array()
        .and_then(|results| results.first())
        .and_then(|result| result["value"][1].as_str())
}

#[test]
fn prometheus_counter_functions_handle_resets_and_staleness_instant_query() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    ingest_counter_reset_fixture(&rt);

    let (status, response) = prom_query(&rt, "increase(http_requests_total[30s])", "1704067230");
    assert_eq!(status, 200, "response={response}");
    assert_eq!(vector_value(&response), Some("18"));

    let (status, response) = prom_query(&rt, "rate(http_requests_total[30s])", "1704067230");
    assert_eq!(status, 200, "response={response}");
    let rate = vector_value(&response)
        .and_then(|value| value.parse::<f64>().ok())
        .expect("rate value");
    assert!((rate - 0.6).abs() < 0.000_001, "response={response}");

    let (status, response) = prom_query(&rt, "irate(http_requests_total[30s])", "1704067230");
    assert_eq!(status, 200, "response={response}");
    let irate = vector_value(&response)
        .and_then(|value| value.parse::<f64>().ok())
        .expect("irate value");
    assert!((irate - 0.6).abs() < 0.000_001, "response={response}");

    let (status, response) = prom_query(&rt, "rate(http_requests_total[5s])", "1704067230");
    assert_eq!(status, 200, "response={response}");
    assert_eq!(
        response["data"]["result"]
            .as_array()
            .expect("result array")
            .len(),
        0,
        "strict staleness window with fewer than two samples should omit series"
    );

    let (status, response) = prom_query(&rt, "rate(http_requests_total)", "1704067230");
    assert_eq!(status, 422, "response={response}");
    assert_eq!(response["status"], "error");
    assert_eq!(response["errorType"], "bad_data");
}

#[test]
fn prometheus_counter_functions_return_stable_grafana_range_matrix() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    ingest_counter_reset_fixture(&rt);

    let (status, response) = query_range(
        &rt,
        "rate(http_requests_total[20s])",
        "1704067210",
        "1704067230",
        "10s",
    );
    assert_eq!(status, 200, "response={response}");
    assert_eq!(response["status"], "success");
    assert_eq!(response["data"]["resultType"], "matrix");
    let values = &response["data"]["result"][0]["values"];
    assert_eq!(values[0], serde_json::json!([1704067210, "1"]));
    let second = values[1][1].as_str().unwrap().parse::<f64>().unwrap();
    let third = values[2][1].as_str().unwrap().parse::<f64>().unwrap();
    assert!((second - 0.6).abs() < 0.000_001, "response={response}");
    assert!((third - 0.4).abs() < 0.000_001, "response={response}");
}
