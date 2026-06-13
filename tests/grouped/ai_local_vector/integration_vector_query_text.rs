use reddb::application::{CreateVectorInput, EntityUseCases, ExecuteQueryInput, QueryUseCases};
use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Mutex, OnceLock};
use std::thread;

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("failed to create in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput {
            query: sql.to_string(),
        })
        .unwrap_or_else(|err| panic!("query should succeed: {sql}\nerror: {err:?}"))
}

fn text(record: &UnifiedRecord, column: &str) -> String {
    match record.get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text value for {column}, got {other:?}"),
    }
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

fn spawn_mock_embedding_server(embedding: &[f32]) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("mock listener should bind");
    let addr = listener
        .local_addr()
        .expect("mock listener should expose a local addr");
    let embedding_json = embedding
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let body = format!(
        "{{\"object\":\"list\",\"data\":[{{\"object\":\"embedding\",\"index\":0,\"embedding\":[{embedding_json}]}}],\"model\":\"mock-embed\",\"usage\":{{\"prompt_tokens\":1,\"total_tokens\":1}}}}"
    );

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("mock server should accept");
        let mut buffer = [0u8; 4096];
        let _ = stream.read(&mut buffer);
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("mock response should write");
        stream.flush().expect("mock response should flush");
    });

    (format!("http://{addr}/v1"), handle)
}

#[test]
fn test_vector_search_with_text_source_uses_runtime_embeddings() {
    let _env_lock = env_lock().lock().expect("env lock should be available");
    let (api_base, server) = spawn_mock_embedding_server(&[1.0, 0.0]);
    let _env = EnvGuard::set(&[
        ("REDDB_AI_PROVIDER", "openai".to_string()),
        ("REDDB_OPENAI_API_BASE", api_base),
        ("REDDB_OPENAI_API_KEY", "test-key".to_string()),
        (
            "REDDB_OPENAI_EMBEDDING_MODEL",
            "text-embedding-3-small".to_string(),
        ),
    ]);

    let rt = rt();

    exec(
        &rt,
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'match-a')",
    );
    exec(
        &rt,
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([0.0, 1.0], 'match-b')",
    );

    let result = exec(
        &rt,
        "VECTOR SEARCH embeddings SIMILAR TO 'remote code execution' LIMIT 1",
    );

    assert_eq!(result.result.records.len(), 1);
    assert_eq!(text(&result.result.records[0], "content"), "match-a");
    assert_eq!(text(&result.result.records[0], "red_entity_type"), "vector");

    server
        .join()
        .expect("mock server thread should exit cleanly");
}

#[test]
fn test_vector_search_with_reference_source_uses_stored_vector() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);

    let anchor = entity
        .create_vector(CreateVectorInput {
            collection: "refs".into(),
            dense: vec![1.0, 0.0],
            content: Some("anchor".into()),
            metadata: vec![],
            link_row: None,
            link_node: None,
        })
        .expect("anchor vector insert should succeed");

    entity
        .create_vector(CreateVectorInput {
            collection: "refs".into(),
            dense: vec![0.0, 1.0],
            content: Some("other".into()),
            metadata: vec![],
            link_row: None,
            link_node: None,
        })
        .expect("second vector insert should succeed");

    let result = exec(
        &rt,
        &format!(
            "VECTOR SEARCH refs SIMILAR TO (refs, {}) LIMIT 1",
            anchor.id.raw()
        ),
    );

    assert_eq!(result.result.records.len(), 1);
    assert_eq!(text(&result.result.records[0], "content"), "anchor");
    assert_eq!(text(&result.result.records[0], "red_entity_type"), "vector");
}

#[test]
fn test_vector_search_with_subquery_source_uses_first_subquery_match() {
    let rt = rt();

    exec(
        &rt,
        "INSERT INTO refs VECTOR (dense, content) VALUES ([1.0, 0.0], 'anchor')",
    );
    exec(
        &rt,
        "INSERT INTO refs VECTOR (dense, content) VALUES ([0.0, 1.0], 'other')",
    );

    let result = exec(
        &rt,
        "VECTOR SEARCH refs \
         SIMILAR TO (VECTOR SEARCH refs SIMILAR TO [1.0, 0.0] LIMIT 1) \
         LIMIT 1",
    );

    assert_eq!(result.result.records.len(), 1);
    assert_eq!(text(&result.result.records[0], "content"), "anchor");
    assert_eq!(text(&result.result.records[0], "red_entity_type"), "vector");
}
