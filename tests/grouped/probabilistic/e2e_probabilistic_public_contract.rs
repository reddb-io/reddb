#[path = "../../support/mod.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use reddb::api::DurabilityMode;
use reddb::runtime::{RedDBRuntime, RuntimeQueryResult};
use reddb::server::RedDBServer;
use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::storage::wal::{WalReader, WalRecord};
use reddb::storage::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
use reddb::{RedDBOptions, StorageDeployPreset};
use serde_json::{json, Value as JsonValue};
use support::{checkpoint_and_reopen, PersistentDbPath};

const PROB_HLL_STATE_PREFIX: &str = "red.probabilistic.hll.";
const PROB_SKETCH_STATE_PREFIX: &str = "red.probabilistic.sketch.";
const PROB_FILTER_STATE_PREFIX: &str = "red.probabilistic.filter.";
const PROB_ENCODING_MARKER_KEY: &str = "red.probabilistic.state_encoding";

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn only_record(result: &RuntimeQueryResult) -> &UnifiedRecord {
    assert_eq!(
        result.result.records.len(),
        1,
        "expected one row for query `{}`",
        result.query
    );
    &result.result.records[0]
}

fn uint_value(row: &UnifiedRecord, column: &str) -> u64 {
    match row.get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected unsigned integer column {column}, got {other:?}"),
    }
}

fn bool_value(row: &UnifiedRecord, column: &str) -> bool {
    match row.get(column) {
        Some(Value::Boolean(value)) => *value,
        other => panic!("expected bool column {column}, got {other:?}"),
    }
}

fn state_key(prefix: &str, name: &str) -> String {
    format!("{prefix}{}", hex_encode(name.as_bytes()))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn config_values(rt: &RedDBRuntime, key: &str) -> Vec<Value> {
    let Some(manager) = rt.db().store().get_collection("red_config") else {
        return Vec::new();
    };
    manager
        .query_all(|_| true)
        .into_iter()
        .filter_map(|entity| {
            let EntityData::Row(row) = entity.data else {
                return None;
            };
            let named = row.named?;
            let Some(Value::Text(candidate)) = named.get("key") else {
                return None;
            };
            if candidate.as_ref() == key {
                named.get("value").cloned()
            } else {
                None
            }
        })
        .collect()
}

fn delete_config_key(rt: &RedDBRuntime, key: &str) {
    let store = rt.db().store();
    let Some(manager) = store.get_collection("red_config") else {
        return;
    };
    let ids: Vec<EntityId> = manager
        .query_all(|_| true)
        .into_iter()
        .filter_map(|entity| {
            let EntityData::Row(row) = &entity.data else {
                return None;
            };
            let named = row.named.as_ref()?;
            let Some(Value::Text(candidate)) = named.get("key") else {
                return None;
            };
            (candidate.as_ref() == key).then_some(entity.id)
        })
        .collect();
    for id in ids {
        store
            .delete("red_config", id)
            .expect("delete seeded config row");
    }
}

fn insert_config_value(rt: &RedDBRuntime, key: &str, value: Value) {
    let store = rt.db().store();
    let _ = store.get_or_create_collection("red_config");
    let entity = UnifiedEntity::new(
        EntityId::new(0),
        EntityKind::TableRow {
            table: std::sync::Arc::from("red_config"),
            row_id: 0,
        },
        EntityData::Row(RowData {
            columns: Vec::new(),
            named: Some(
                [
                    ("key".to_string(), Value::text(key.to_string())),
                    ("value".to_string(), value),
                ]
                .into_iter()
                .collect(),
            ),
            schema: None,
        }),
    );
    store
        .insert_auto("red_config", entity)
        .expect("insert seeded config row");
}

fn open_persistent_runtime(path: &std::path::Path) -> Result<RedDBRuntime, String> {
    let path = path.to_string_lossy().to_string();
    let options = RedDBOptions::persistent(&path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())?;
    RedDBRuntime::with_options(options).map_err(|err| format!("{err:?}"))
}

fn open_wal_durable_runtime(path: &std::path::Path) -> RedDBRuntime {
    let options = RedDBOptions::persistent(path)
        .with_durability_mode(DurabilityMode::WalDurableGrouped)
        .with_storage_profile(StorageDeployPreset::PrimaryReplicaProductionHa.selection())
        .expect("primary-replica operational profile");
    RedDBRuntime::with_options(options).expect("open WAL-durable runtime")
}

fn wal_file_len(path: &std::path::Path) -> u64 {
    std::fs::metadata(reddb_file::unified_wal_path(path))
        .expect("WAL metadata")
        .len()
}

fn spawn_http_server() -> (support::TempDbFile, String) {
    let (db, rt) = support::persistent_runtime("probabilistic-http");
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    server.serve_in_background_on(listener);
    (db, addr.to_string())
}

fn post_query(addr: &str, query: &str) -> JsonValue {
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
    let parsed: JsonValue = serde_json::from_str(body).expect("json body");
    assert_eq!(parsed.get("ok").and_then(JsonValue::as_bool), Some(true));
    parsed
}

fn http_only_record(response: &JsonValue) -> &JsonValue {
    let records = response["result"]["records"]
        .as_array()
        .expect("result.records array");
    assert_eq!(records.len(), 1, "expected one record in {response}");
    &records[0]
}

fn http_value<'a>(record: &'a JsonValue, column: &str) -> &'a JsonValue {
    &record["values"][column]
}

#[test]
fn probabilistic_sql_read_forms_have_default_columns_and_star_guidance() {
    let rt = runtime();

    exec(&rt, "CREATE HLL visitors");
    exec(&rt, "HLL ADD visitors 'alice' 'bob' 'alice'");
    let hll = exec(&rt, "SELECT CARDINALITY FROM visitors");
    assert_eq!(hll.result.columns, vec!["cardinality"]);
    assert_eq!(uint_value(only_record(&hll), "cardinality"), 2);

    exec(&rt, "CREATE SKETCH clicks");
    exec(&rt, "SKETCH ADD clicks 'signup' 5");
    let sketch = exec(&rt, "SELECT FREQ('signup') FROM clicks");
    assert_eq!(sketch.result.columns, vec!["freq"]);
    assert_eq!(uint_value(only_record(&sketch), "freq"), 5);

    exec(&rt, "CREATE FILTER sessions");
    exec(&rt, "FILTER ADD sessions 'sess:abc'");
    let filter = exec(&rt, "SELECT CONTAINS('sess:abc') FROM sessions");
    assert_eq!(filter.result.columns, vec!["contains"]);
    assert!(bool_value(only_record(&filter), "contains"));

    let star = rt
        .execute_query("SELECT * FROM visitors")
        .expect_err("SELECT * from a probabilistic collection should guide callers");
    let message = format!("{star:?}");
    assert!(
        message.contains("SELECT CARDINALITY, FREQ(...), or CONTAINS(...) read forms"),
        "unexpected error: {message}"
    );
}

#[test]
fn probabilistic_state_survives_reopen_for_commands_and_sql_read_forms() {
    let path = PersistentDbPath::new("probabilistic_public_contract");
    let rt = path.open_runtime();

    exec(&rt, "CREATE HLL visitors");
    exec(&rt, "HLL ADD visitors 'alice' 'bob' 'alice'");

    exec(&rt, "CREATE SKETCH clicks");
    exec(&rt, "SKETCH ADD clicks 'signup' 5");
    exec(&rt, "SKETCH ADD clicks 'login' 2");

    exec(&rt, "CREATE FILTER sessions");
    exec(&rt, "FILTER ADD sessions 'sess:abc'");
    exec(&rt, "FILTER ADD sessions 'sess:def'");

    let reopened = checkpoint_and_reopen(&path, rt);

    let hll_command = exec(&reopened, "HLL COUNT visitors");
    assert_eq!(uint_value(only_record(&hll_command), "count"), 2);
    let hll_sql = exec(
        &reopened,
        "SELECT CARDINALITY AS unique_count FROM visitors",
    );
    assert_eq!(hll_sql.result.columns, vec!["unique_count"]);
    assert_eq!(uint_value(only_record(&hll_sql), "unique_count"), 2);

    let sketch_command = exec(&reopened, "SKETCH COUNT clicks 'signup'");
    assert_eq!(uint_value(only_record(&sketch_command), "estimate"), 5);
    let sketch_info = exec(&reopened, "SKETCH INFO clicks");
    assert_eq!(uint_value(only_record(&sketch_info), "total"), 7);
    let sketch_sql = exec(&reopened, "SELECT FREQ('signup') AS freq FROM clicks");
    assert_eq!(sketch_sql.result.columns, vec!["freq"]);
    assert_eq!(uint_value(only_record(&sketch_sql), "freq"), 5);

    let filter_command = exec(&reopened, "FILTER CHECK sessions 'sess:abc'");
    assert!(bool_value(only_record(&filter_command), "exists"));
    let filter_info = exec(&reopened, "FILTER INFO sessions");
    assert_eq!(uint_value(only_record(&filter_info), "count"), 2);
    let filter_sql = exec(
        &reopened,
        "SELECT CONTAINS('sess:abc') AS hit FROM sessions",
    );
    assert_eq!(filter_sql.result.columns, vec!["hit"]);
    assert!(bool_value(only_record(&filter_sql), "hit"));
}

#[test]
fn probabilistic_state_writes_raw_blobs_and_compacts_superseded_rows() {
    let rt = runtime();

    exec(&rt, "CREATE HLL visitors");
    for i in 0..8 {
        exec(&rt, &format!("HLL ADD visitors 'user-{i}'"));
    }

    exec(&rt, "CREATE SKETCH clicks WIDTH 16 DEPTH 3");
    for i in 0..8 {
        exec(&rt, &format!("SKETCH ADD clicks 'button-{i}' {}", i + 1));
    }

    exec(&rt, "CREATE FILTER sessions CAPACITY 100");
    for i in 0..8 {
        exec(&rt, &format!("FILTER ADD sessions 'sess-{i}'"));
    }

    for key in [
        state_key(PROB_HLL_STATE_PREFIX, "visitors"),
        state_key(PROB_SKETCH_STATE_PREFIX, "clicks"),
        state_key(PROB_FILTER_STATE_PREFIX, "sessions"),
    ] {
        let values = config_values(&rt, &key);
        assert_eq!(
            values.len(),
            1,
            "expected compacted single state row for {key}"
        );
        assert!(
            matches!(values.first(), Some(Value::Blob(_))),
            "expected raw Blob state for {key}, got {values:?}"
        );
    }
}

#[test]
fn hll_add_persists_delta_bytes_and_recovers_from_wal_tail() {
    let db = support::temp_db_file("probabilistic-hll-delta-wal");
    let rt = open_wal_durable_runtime(db.path());

    exec(&rt, "CREATE HLL visitors");
    let before_add_len = wal_file_len(db.path());
    exec(&rt, "HLL ADD visitors 'alice'");
    let add_bytes = wal_file_len(db.path()) - before_add_len;

    assert!(
        add_bytes < 4096,
        "single HLL ADD should persist a delta, not the full 16 KiB sketch; wrote {add_bytes} bytes"
    );

    drop(rt);
    let reopened = open_wal_durable_runtime(db.path());
    let hll_command = exec(&reopened, "HLL COUNT visitors");
    assert_eq!(uint_value(only_record(&hll_command), "count"), 1);

    let records: Vec<_> = WalReader::open(reddb_file::unified_wal_path(db.path()))
        .expect("open WAL")
        .iter()
        .map(|record| record.expect("valid WAL record").1)
        .collect();
    assert!(
        records
            .iter()
            .any(|record| matches!(record, WalRecord::ProbabilisticDelta { .. })),
        "WAL should carry a dedicated probabilistic delta record"
    );
}

#[test]
fn probabilistic_legacy_hex_state_migrates_once_to_raw_blob() {
    let path = PersistentDbPath::new("probabilistic_legacy_migration");
    let rt = path.open_runtime();

    exec(&rt, "CREATE HLL visitors");
    exec(&rt, "HLL ADD visitors 'alice' 'bob'");
    rt.checkpoint().expect("checkpoint full HLL snapshot");
    let key = state_key(PROB_HLL_STATE_PREFIX, "visitors");
    let raw = match config_values(&rt, &key).pop() {
        Some(Value::Blob(bytes)) => bytes,
        other => panic!("expected raw HLL state before legacy seed, got {other:?}"),
    };
    delete_config_key(&rt, PROB_ENCODING_MARKER_KEY);
    delete_config_key(&rt, &key);
    insert_config_value(&rt, &key, Value::text(hex_encode(&raw)));

    drop(rt);
    let reopened = path.open_runtime();
    let hll_command = exec(&reopened, "HLL COUNT visitors");
    assert_eq!(uint_value(only_record(&hll_command), "count"), 2);

    let values = config_values(&reopened, &key);
    assert_eq!(
        values.len(),
        1,
        "legacy row should be compacted during migration"
    );
    assert!(
        matches!(values.first(), Some(Value::Blob(bytes)) if bytes == &raw),
        "expected migrated raw HLL state, got {values:?}"
    );
    assert!(
        matches!(
            config_values(&reopened, PROB_ENCODING_MARKER_KEY).last(),
            Some(Value::Text(value)) if value.as_ref() == "raw-v1"
        ),
        "migration should stamp the raw encoding marker"
    );
}

#[test]
fn probabilistic_legacy_hex_state_is_rejected_after_raw_marker() {
    let dir = tempfile::Builder::new()
        .prefix("reddb-probabilistic-legacy-reject-")
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join("data.rdb");
    let rt = open_persistent_runtime(&path).expect("open runtime");

    exec(&rt, "CREATE HLL visitors");
    exec(&rt, "HLL ADD visitors 'alice'");
    rt.checkpoint().expect("checkpoint full HLL snapshot");
    let key = state_key(PROB_HLL_STATE_PREFIX, "visitors");
    let raw = match config_values(&rt, &key).pop() {
        Some(Value::Blob(bytes)) => bytes,
        other => panic!("expected raw HLL state before legacy append, got {other:?}"),
    };
    insert_config_value(&rt, &key, Value::text(hex_encode(&raw)));
    drop(rt);

    let message = match open_persistent_runtime(&path) {
        Ok(_) => panic!("raw-marked legacy state should fail"),
        Err(err) => err,
    };
    assert!(
        message.contains("legacy hex-encoded HLL state for 'visitors' is rejected"),
        "unexpected error: {message}"
    );
}

#[test]
fn hll_union_preserves_non_default_precision() {
    let rt = runtime();

    exec(&rt, "CREATE HLL visitors_a PRECISION 12");
    exec(&rt, "CREATE HLL visitors_b PRECISION 12");
    exec(&rt, "HLL ADD visitors_a 'alice' 'bob'");
    exec(&rt, "HLL ADD visitors_b 'carol' 'dave'");

    let count = exec(&rt, "HLL COUNT visitors_a visitors_b");
    assert_eq!(uint_value(only_record(&count), "count"), 4);

    exec(&rt, "HLL MERGE visitors_all visitors_a visitors_b");
    let info = exec(&rt, "HLL INFO visitors_all");
    let row = only_record(&info);
    assert_eq!(uint_value(row, "precision"), 12);
    assert_eq!(uint_value(row, "count"), 4);
}

#[test]
fn hll_union_rejects_precision_mismatch() {
    let rt = runtime();

    exec(&rt, "CREATE HLL visitors_a PRECISION 12");
    exec(&rt, "CREATE HLL visitors_b PRECISION 14");

    let count_err = rt
        .execute_query("HLL COUNT visitors_a visitors_b")
        .expect_err("multi-HLL count must reject mixed precision");
    let count_message = format!("{count_err:?}");
    assert!(
        count_message.contains("HLL COUNT requires matching precision"),
        "unexpected error: {count_message}"
    );

    let merge_err = rt
        .execute_query("HLL MERGE visitors_all visitors_a visitors_b")
        .expect_err("HLL merge must reject mixed precision");
    let merge_message = format!("{merge_err:?}");
    assert!(
        merge_message.contains("HLL MERGE requires matching precision"),
        "unexpected error: {merge_message}"
    );
}

#[test]
fn hll_count_and_merge_require_operands() {
    let rt = runtime();

    let count_err = rt
        .execute_query("HLL COUNT")
        .expect_err("HLL COUNT without names should fail");
    let count_message = format!("{count_err:?}");
    assert!(
        count_message.contains("HLL COUNT requires at least one HLL name"),
        "unexpected error: {count_message}"
    );

    let merge_err = rt
        .execute_query("HLL MERGE visitors_all")
        .expect_err("HLL MERGE without sources should fail");
    let merge_message = format!("{merge_err:?}");
    assert!(
        merge_message.contains("HLL MERGE requires at least one source HLL"),
        "unexpected error: {merge_message}"
    );
}

#[test]
fn http_query_covers_probabilistic_commands_and_sql_read_forms() {
    let (_db, addr) = spawn_http_server();

    post_query(&addr, "CREATE HLL visitors");
    post_query(&addr, "HLL ADD visitors 'alice' 'bob' 'alice'");
    let hll_command = post_query(&addr, "HLL COUNT visitors");
    assert_eq!(
        http_value(http_only_record(&hll_command), "count"),
        &json!(2)
    );
    let hll_sql = post_query(&addr, "SELECT CARDINALITY AS unique_count FROM visitors");
    assert_eq!(
        http_value(http_only_record(&hll_sql), "unique_count"),
        &json!(2)
    );

    post_query(&addr, "CREATE SKETCH clicks");
    post_query(&addr, "SKETCH ADD clicks 'signup' 5");
    let sketch_command = post_query(&addr, "SKETCH COUNT clicks 'signup'");
    assert_eq!(
        http_value(http_only_record(&sketch_command), "estimate"),
        &json!(5)
    );
    let sketch_sql = post_query(&addr, "SELECT FREQ('signup') AS freq FROM clicks");
    assert_eq!(http_value(http_only_record(&sketch_sql), "freq"), &json!(5));

    post_query(&addr, "CREATE FILTER sessions");
    post_query(&addr, "FILTER ADD sessions 'sess:abc'");
    let filter_command = post_query(&addr, "FILTER CHECK sessions 'sess:abc'");
    assert_eq!(
        http_value(http_only_record(&filter_command), "exists"),
        &json!(true)
    );
    let filter_sql = post_query(&addr, "SELECT CONTAINS('sess:abc') AS hit FROM sessions");
    assert_eq!(
        http_value(http_only_record(&filter_sql), "hit"),
        &json!(true)
    );
}
