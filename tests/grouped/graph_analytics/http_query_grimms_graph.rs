#[path = "../../support/mod.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use reddb::server::RedDBServer;
use serde_json::{json, Value};

fn spawn_http_server() -> (support::TempDbFile, String) {
    let (db, rt) = support::persistent_runtime("grimms-graph-http");
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    server.serve_in_background_on(listener);
    (db, addr.to_string())
}

fn post_query(addr: &str, query: &str) -> Value {
    let body = json!({ "query": query }).to_string();
    let request = format!(
        "POST /query HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );

    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush request");

    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "expected 200 for query {query:?}, got:\n{response}"
    );
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("HTTP response has body");
    let parsed: Value = serde_json::from_str(body).expect("json body");
    assert_eq!(parsed.get("ok").and_then(Value::as_bool), Some(true));
    parsed
}

fn records(response: &Value) -> &[Value] {
    response["result"]["records"]
        .as_array()
        .expect("result.records array")
}

fn value<'a>(record: &'a Value, column: &str) -> &'a Value {
    &record["values"][column]
}

fn first_value<'a>(record: &'a Value, columns: &[&str]) -> &'a Value {
    columns
        .iter()
        .map(|column| value(record, column))
        .find(|value| !value.is_null())
        .unwrap_or_else(|| panic!("missing any of columns {columns:?} in {record}"))
}

fn number(record: &Value, column: &str) -> f64 {
    value(record, column)
        .as_f64()
        .unwrap_or_else(|| panic!("expected numeric column {column} in {record}"))
}

fn only_record(response: &Value) -> &Value {
    let rows = records(response);
    assert_eq!(rows.len(), 1, "expected one record in {response}");
    &rows[0]
}

#[test]
fn http_query_grimms_graph_showcase_queries_match_embedded_contract() {
    let (_db, addr) = spawn_http_server();

    post_query(
        &addr,
        "INSERT INTO tales NODE (label, name) VALUES ('hansel', 'Hansel')",
    );
    post_query(
        &addr,
        "INSERT INTO tales NODE (label, name) VALUES ('gretel', 'Gretel')",
    );
    post_query(
        &addr,
        "INSERT INTO tales EDGE (label, from, to, evidence) VALUES \
         ('HAS_TRAIT', 'hansel', 'gretel', 'siblings in the forest')",
    );

    let path = post_query(
        &addr,
        "GRAPH SHORTEST_PATH 'hansel' TO 'gretel' ALGORITHM dijkstra",
    );
    assert_eq!(records(&path).len(), 1);
    assert_eq!(value(&records(&path)[0], "hop_count"), &json!(1));

    let props = post_query(&addr, "GRAPH PROPERTIES 'hansel'");
    assert_eq!(records(&props).len(), 1);
    assert_eq!(value(&records(&props)[0], "name"), &json!("Hansel"));

    let centrality = post_query(&addr, "GRAPH CENTRALITY LIMIT 2");
    assert_eq!(records(&centrality).len(), 2);

    let matched = post_query(
        &addr,
        "MATCH (a)-[r:HAS_TRAIT]->(b) \
         WHERE a.label = 'hansel' \
         RETURN a.name, b.name, r.evidence",
    );
    assert_eq!(records(&matched).len(), 1);
    let row = &records(&matched)[0];
    assert_eq!(value(row, "a.name"), &json!("Hansel"));
    assert_eq!(value(row, "b.name"), &json!("Gretel"));
    assert_eq!(value(row, "r.evidence"), &json!("siblings in the forest"));
}

#[test]
fn http_query_mini_grimms_multimodel_showcase_smoke() {
    let (_db, addr) = spawn_http_server();

    post_query(
        &addr,
        "CREATE TABLE tale_words (tale_slug TEXT, word TEXT, count INTEGER)",
    );
    post_query(
        &addr,
        "INSERT INTO tale_words (tale_slug, word, count) VALUES \
         ('hansel-gretel', 'forest', 3), \
         ('hansel-gretel', 'witch', 2), \
         ('cinderella', 'forest', 1)",
    );
    let word_counts = post_query(
        &addr,
        "SELECT word, SUM(count) FROM tale_words WHERE word = 'forest' GROUP BY word",
    );
    let word_row = only_record(&word_counts);
    assert_eq!(value(word_row, "word"), &json!("forest"));
    assert_eq!(
        first_value(word_row, &["SUM(count)", "sum(count)"]),
        &json!(4)
    );

    post_query(
        &addr,
        "INSERT INTO settings KV (key, value) VALUES ('featured_tale', 'hansel-gretel')",
    );
    let kv = post_query(
        &addr,
        "SELECT key, value FROM settings WHERE key = 'featured_tale'",
    );
    let kv_row = only_record(&kv);
    assert_eq!(value(kv_row, "key"), &json!("featured_tale"));
    assert_eq!(value(kv_row, "value"), &json!("hansel-gretel"));

    post_query(&addr, "CREATE TIMESERIES tale_metrics RETENTION 7 d");
    post_query(
        &addr,
        "INSERT INTO tale_metrics (metric, value, tags, timestamp) VALUES \
         ('tale.reads', 2.0, {tale: 'hansel-gretel'}, 1704067200000000000)",
    );
    let metrics = post_query(
        &addr,
        "SELECT metric, value FROM tale_metrics WHERE metric = 'tale.reads'",
    );
    let metric_row = only_record(&metrics);
    assert_eq!(value(metric_row, "metric"), &json!("tale.reads"));
    assert_eq!(number(metric_row, "value"), 2.0);

    post_query(&addr, "CREATE SKETCH tale_terms");
    post_query(&addr, "SKETCH ADD tale_terms 'forest' 3");
    let sketch = post_query(&addr, "SKETCH COUNT tale_terms 'forest'");
    assert_eq!(value(only_record(&sketch), "estimate"), &json!(3));

    post_query(&addr, "CREATE FILTER seen_tales");
    post_query(&addr, "FILTER ADD seen_tales 'hansel-gretel'");
    let filter = post_query(&addr, "FILTER CHECK seen_tales 'hansel-gretel'");
    assert_eq!(value(only_record(&filter), "exists"), &json!(true));

    post_query(&addr, "CREATE VECTOR tale_embeddings DIM 2 METRIC cosine");
    post_query(
        &addr,
        "INSERT INTO tale_embeddings VECTOR (dense, content) VALUES \
         ([1.0, 0.0], 'Hansel and Gretel'), \
         ([0.0, 1.0], 'Cinderella')",
    );
    let vector = post_query(
        &addr,
        "VECTOR SEARCH tale_embeddings SIMILAR TO [1.0, 0.0] LIMIT 1",
    );
    assert_eq!(
        value(only_record(&vector), "content"),
        &json!("Hansel and Gretel")
    );

    post_query(&addr, "CREATE QUEUE insight_jobs");
    post_query(&addr, "QUEUE PUSH insight_jobs 'rank:hansel-gretel'");
    let job = post_query(&addr, "QUEUE READ insight_jobs CONSUMER worker1 COUNT 1");
    assert_eq!(
        value(only_record(&job), "payload"),
        &json!("rank:hansel-gretel")
    );
}
