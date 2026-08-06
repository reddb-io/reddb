use reddb_server::runtime::query_request::{
    ParamValue, PreparedRegistry, QueryRequest, QueryRequestExecutor,
};
use reddb_server::{json, replication::CommitPolicy, RedDBRuntime};
use reddb_types::Value;
use std::sync::{Mutex, OnceLock};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: this integration-test binary serializes all environment
        // mutation through `env_lock`, and restores each value on drop.
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: see `EnvGuard::set`; the same lock is held until this guard drops.
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

#[test]
fn request_decodes_every_adr_0015_parameter_variant() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let prepared = PreparedRegistry::new();
    let executor = QueryRequestExecutor::new(&runtime, &prepared);
    let uuid = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    let cases = vec![
        (ParamValue::Null, Value::Null),
        (ParamValue::Bool(true), Value::Boolean(true)),
        (ParamValue::Int64(-42), Value::Integer(-42)),
        // The wire taxonomy's unsigned type, over the full u64 range: it
        // must stay unsigned and must not narrow through i64.
        (
            ParamValue::UInt64(u64::MAX),
            Value::UnsignedInteger(u64::MAX),
        ),
        (ParamValue::Float64(1.5), Value::Float(1.5)),
        (ParamValue::Text("hello".to_string()), Value::text("hello")),
        (
            ParamValue::Bytes(vec![0xde, 0xad, 0xbe, 0xef]),
            Value::Blob(vec![0xde, 0xad, 0xbe, 0xef]),
        ),
        (
            ParamValue::Vector(vec![1.0, 2.0, -0.5]),
            Value::Vector(vec![1.0, 2.0, -0.5]),
        ),
        (
            ParamValue::Json(br#"{"nested":[true,null]}"#.to_vec()),
            Value::Json(br#"{"nested":[true,null]}"#.to_vec()),
        ),
        (
            ParamValue::Timestamp(1_700_000_000),
            Value::Timestamp(1_700_000_000),
        ),
        (ParamValue::Uuid(uuid), Value::Uuid(uuid)),
    ];

    for (param, expected) in cases {
        let result = executor
            .execute(QueryRequest::sql("SELECT $1 AS value", vec![param]))
            .expect("parameterized request");
        assert_eq!(result.result.records[0].get("value"), Some(&expected));
    }
}

#[test]
fn json_param_decode_preserves_typed_envelopes() {
    let cases = [
        ("null", ParamValue::Null),
        ("true", ParamValue::Bool(true)),
        ("-42", ParamValue::Int64(-42)),
        ("1.5", ParamValue::Float64(1.5)),
        (r#""hello""#, ParamValue::Text("hello".to_string())),
        (r#"{"$bytes":"AAECAw=="}"#, ParamValue::Bytes(vec![0, 1, 2, 3])),
        (
            r#"{"$ts":"9223372036854775807"}"#,
            ParamValue::Timestamp(i64::MAX),
        ),
        (
            r#"{"$uuid":"00112233-4455-6677-8899-aabbccddeeff"}"#,
            ParamValue::Uuid([
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
                0xcc, 0xdd, 0xee, 0xff,
            ]),
        ),
        (
            r#"{"$int":"9007199254740993"}"#,
            ParamValue::Int64(9_007_199_254_740_993),
        ),
        (
            r#"{"$uint":"9223372036854775808"}"#,
            ParamValue::UInt64(i64::MAX as u64 + 1),
        ),
        (
            r#"{"$decimal":"3.14159265358979323846"}"#,
            ParamValue::DecimalText("3.14159265358979323846".to_string()),
        ),
        ("[1,2.5]", ParamValue::Vector(vec![1.0, 2.5])),
        (
            r#"{"nested":[true,null]}"#,
            ParamValue::Json(br#"{"nested":[true,null]}"#.to_vec()),
        ),
    ];

    for (encoded, expected) in cases {
        let value = json::from_str(encoded).expect("JSON fixture");
        assert_eq!(ParamValue::decode_json(&value).expect("decode param"), expected);
    }

    let superseded = json::from_str(r#"{"$number":"1"}"#).expect("JSON fixture");
    assert!(ParamValue::decode_json(&superseded)
        .expect_err("superseded envelope must be rejected")
        .contains("superseded"));
}

#[test]
fn prepared_id_is_scoped_to_its_connection_registry() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let first_connection = PreparedRegistry::new();
    let second_connection = PreparedRegistry::new();
    let prepared = first_connection
        .prepare(&runtime, "SELECT $1 AS value")
        .expect("prepare query");
    assert_eq!(prepared.parameter_count, 1);

    let result = QueryRequestExecutor::new(&runtime, &first_connection)
        .execute(QueryRequest::prepared(
            prepared.id,
            vec![ParamValue::Int64(42)],
        ))
        .expect("execute on owning connection");
    assert_eq!(result.query, "SELECT $1 AS value");
    assert_eq!(
        result.result.records[0].get("value"),
        Some(&Value::Integer(42))
    );

    let error = QueryRequestExecutor::new(&runtime, &second_connection)
        .execute(QueryRequest::prepared(
            prepared.id,
            vec![ParamValue::Int64(42)],
        ))
        .expect_err("a second connection must not resolve the prepared id");
    assert!(error.to_string().contains("not found or expired"));
}

#[test]
fn ddl_epoch_change_rejects_prepared_execution() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let registry = PreparedRegistry::new();
    let prepared = registry
        .prepare(&runtime, "SELECT $1 AS value")
        .expect("prepare query");

    runtime
        .execute_query("CREATE TABLE request_epoch_guard (id INTEGER)")
        .expect("execute DDL");

    let error = QueryRequestExecutor::new(&runtime, &registry)
        .execute(QueryRequest::prepared(
            prepared.id,
            vec![ParamValue::Int64(42)],
        ))
        .expect_err("DDL must invalidate the prepared shape");
    assert!(error.to_string().contains("prepared_needs_replan"));
}

#[test]
fn prepared_registry_kill_switch_rejects_prepare_and_execute() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let registry = PreparedRegistry::new();
    let prepared = registry
        .prepare(&runtime, "SELECT $1 AS value")
        .expect("prepare before kill switch");

    registry.disable();

    let prepare_error = registry
        .prepare(&runtime, "SELECT 1")
        .expect_err("kill switch must reject prepare");
    assert!(prepare_error
        .to_string()
        .contains("prepared statements disabled"));

    let execute_error = QueryRequestExecutor::new(&runtime, &registry)
        .execute(QueryRequest::prepared(
            prepared.id,
            vec![ParamValue::Int64(42)],
        ))
        .expect_err("kill switch must reject execute");
    assert!(execute_error
        .to_string()
        .contains("prepared statements disabled"));
}

#[test]
fn prepared_registry_evicts_the_oldest_entry_at_capacity() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let registry = PreparedRegistry::with_capacity(1).expect("bounded registry");
    let oldest = registry
        .prepare(&runtime, "SELECT 1 AS value")
        .expect("prepare oldest");
    let newest = registry
        .prepare(&runtime, "SELECT 2 AS value")
        .expect("prepare newest");
    let executor = QueryRequestExecutor::new(&runtime, &registry);

    let error = executor
        .execute(QueryRequest::prepared(oldest.id, Vec::new()))
        .expect_err("oldest entry must be evicted");
    assert!(error.to_string().contains("not found or expired"));

    let result = executor
        .execute(QueryRequest::prepared(newest.id, Vec::new()))
        .expect("newest entry remains available");
    assert_eq!(
        result.result.records[0].get("value"),
        Some(&Value::Integer(2))
    );
}

#[test]
fn request_commit_policy_uses_global_default_and_honors_override() {
    let _env = env_lock().lock().expect("environment lock");
    let _global_policy = EnvGuard::set("RED_PRIMARY_COMMIT_POLICY", "ack_n=1");
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    runtime
        .execute_query("CREATE TABLE request_commit_policy (id INTEGER)")
        .expect("create table");
    let registry = PreparedRegistry::new();
    let executor = QueryRequestExecutor::new(&runtime, &registry);

    executor
        .execute(QueryRequest::sql(
            "INSERT INTO request_commit_policy (id) VALUES ($1)",
            vec![ParamValue::Int64(1)],
        ))
        .expect("omitted policy uses the server default");

    let error = executor
        .execute(
            QueryRequest::sql(
                "INSERT INTO request_commit_policy (id) VALUES ($1)",
                vec![ParamValue::Int64(2)],
            )
            .with_commit_policy(CommitPolicy::Local),
        )
        .expect_err("a weaker per-request override must be rejected");
    assert!(error.to_string().contains("weaker than resolved floor"));

    let rows = runtime
        .execute_query("SELECT id FROM request_commit_policy WHERE id = 2")
        .expect("read committed rows");
    assert!(rows.result.records.is_empty());
}
