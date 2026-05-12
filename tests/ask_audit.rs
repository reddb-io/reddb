mod support;

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};
use support::mock_ai_provider::{MockAiProvider, MockAiProviderConfig};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set(vars: &[(&'static str, String)]) -> Self {
        let mut saved = Vec::new();
        let mut dedup = BTreeMap::new();
        for (key, value) in vars {
            dedup.insert(*key, value.clone());
        }
        for (key, value) in dedup {
            saved.push((key, std::env::var(key).ok()));
            unsafe {
                std::env::set_var(key, value);
            }
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..).rev() {
            match value {
                Some(value) => unsafe {
                    std::env::set_var(key, value);
                },
                None => unsafe {
                    std::env::remove_var(key);
                },
            }
        }
    }
}

fn text(row: &UnifiedRecord, column: &str) -> String {
    match row.get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text value for {column}, got {other:?}"),
    }
}

fn integer(row: &UnifiedRecord, column: &str) -> i64 {
    match row.get(column) {
        Some(Value::Integer(value)) => *value,
        other => panic!("expected integer value for {column}, got {other:?}"),
    }
}

fn boolean(row: &UnifiedRecord, column: &str) -> bool {
    match row.get(column) {
        Some(Value::Boolean(value)) => *value,
        other => panic!("expected boolean value for {column}, got {other:?}"),
    }
}

#[test]
fn ask_writes_audit_row_before_returning_answer() {
    let _lock = env_lock().lock().expect("env lock");
    let mock =
        MockAiProvider::start(MockAiProviderConfig::default()).expect("mock provider should start");
    let _env = EnvGuard::set(&[
        ("REDDB_AI_PROVIDER", "openai".to_string()),
        ("REDDB_OPENAI_API_BASE", mock.api_base()),
        ("REDDB_OPENAI_API_KEY", "test-key".to_string()),
        ("REDDB_OPENAI_PROMPT_MODEL", "mock-chat".to_string()),
        ("REDDB_OPENAI_EMBEDDING_MODEL", "mock-embed".to_string()),
    ]);

    let rt = open_runtime();
    exec(&rt, "CREATE TABLE notes (id INT, body TEXT)");
    exec(
        &rt,
        "INSERT INTO notes (id, body) VALUES (1, 'FDD-12313 launch note')",
    );

    let answer = rt
        .execute_query("ASK 'notes FDD-12313' USING openai MODEL 'mock-chat' LIMIT 5")
        .expect("ASK should return");
    assert_eq!(answer.result.records.len(), 1);

    let audit = rt
        .execute_query("SELECT * FROM red_ask_audit")
        .expect("audit collection should be queryable");
    assert_eq!(audit.result.records.len(), 1);

    let row = &audit.result.records[0];
    assert_eq!(text(row, "question"), "notes FDD-12313");
    assert_eq!(text(row, "provider"), "openai");
    assert_eq!(text(row, "model"), "mock-chat");
    assert_eq!(integer(row, "prompt_tokens"), 1);
    assert_eq!(integer(row, "completion_tokens"), 1);
    assert!(!boolean(row, "cache_hit"));
    assert!(row.get("answer_hash").is_some());
    assert!(row.get("answer").is_none(), "answer text is opt-in only");
}

#[test]
fn ask_audit_include_answer_config_adds_answer_field() {
    let _lock = env_lock().lock().expect("env lock");
    let mock =
        MockAiProvider::start(MockAiProviderConfig::default()).expect("mock provider should start");
    let _env = EnvGuard::set(&[
        ("REDDB_AI_PROVIDER", "openai".to_string()),
        ("REDDB_OPENAI_API_BASE", mock.api_base()),
        ("REDDB_OPENAI_API_KEY", "test-key".to_string()),
        ("REDDB_OPENAI_PROMPT_MODEL", "mock-chat".to_string()),
        ("REDDB_OPENAI_EMBEDDING_MODEL", "mock-embed".to_string()),
    ]);

    let rt = open_runtime();
    exec(&rt, "CREATE TABLE notes (id INT, body TEXT)");
    exec(
        &rt,
        "INSERT INTO notes (id, body) VALUES (1, 'FDD-12313 launch note')",
    );
    exec(&rt, "SET CONFIG ask.audit.include_answer = true");

    rt.execute_query("ASK 'notes FDD-12313' USING openai MODEL 'mock-chat' LIMIT 5")
        .expect("ASK should return");

    let audit = rt
        .execute_query("SELECT * FROM red_ask_audit")
        .expect("audit collection should be queryable");
    assert_eq!(audit.result.records.len(), 1);
    let row = &audit.result.records[0];
    assert_eq!(text(row, "answer"), "mock response");
    assert!(row.get("answer_hash").is_some());
}

#[test]
fn repeated_ask_calls_append_one_audit_row_each() {
    let _lock = env_lock().lock().expect("env lock");
    let mock =
        MockAiProvider::start(MockAiProviderConfig::default()).expect("mock provider should start");
    let _env = EnvGuard::set(&[
        ("REDDB_AI_PROVIDER", "openai".to_string()),
        ("REDDB_OPENAI_API_BASE", mock.api_base()),
        ("REDDB_OPENAI_API_KEY", "test-key".to_string()),
        ("REDDB_OPENAI_PROMPT_MODEL", "mock-chat".to_string()),
        ("REDDB_OPENAI_EMBEDDING_MODEL", "mock-embed".to_string()),
    ]);

    let rt = open_runtime();
    exec(&rt, "CREATE TABLE notes (id INT, body TEXT)");
    exec(
        &rt,
        "INSERT INTO notes (id, body) VALUES (1, 'FDD-12313 launch note')",
    );

    for _ in 0..5 {
        rt.execute_query("ASK 'notes FDD-12313' USING openai MODEL 'mock-chat' LIMIT 5")
            .expect("ASK should return");
    }

    let audit = rt
        .execute_query("SELECT * FROM red_ask_audit")
        .expect("audit collection should be queryable");
    assert_eq!(audit.result.records.len(), 5);
    for row in &audit.result.records {
        assert_eq!(text(row, "question"), "notes FDD-12313");
        assert_eq!(text(row, "provider"), "openai");
        assert_eq!(text(row, "model"), "mock-chat");
    }
}
