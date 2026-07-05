//! #1747 — ASK planner core: the factual intent, end-to-end (ADR 0068).
//!
//! Pins the planner-first factual path with a mock OpenAI-compatible HTTP
//! stub, fully offline. The stub returns a *scripted sequence* of chat
//! completions (plan JSON first, then the cited synthesis) and captures
//! every chat request body so the tests can assert what reached the model.
//!
//! Coverage:
//!   1. A factual `ASK` generates → validates → executes a read-only query
//!      and answers with citations over the executed rows.
//!   2. The planner prompt carries only the funnel-narrowed slice — a
//!      collection the funnel did not select never reaches the model.
//!   3. A multi-collection question produces a global-`any` candidate that
//!      executes across collections and grounds the answer.
//!   4. A mutating candidate is refused and never executed under any flag.
//!   5. The `red_ask_audit` row grows intent, plan summary, executed query.
//!   6. `red.config.ai.ask.planner_model` selects the planner model
//!      independently of the synthesis model.
//!
//! The planner routing/refusal logic itself is unit-tested without HTTP in
//! `crates/reddb-server/src/runtime/ai/ask_planner.rs`.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::support::env_lock;
use super::support::PersistentRuntime;
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

fn open_rt() -> PersistentRuntime {
    super::support::persistent_test_runtime("ask-planner-core")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

/// Wire the runtime + env at a scripted mock stub and enable the planner.
fn configure_planner(rt: &RedDBRuntime) {
    rt.execute_query("SET CONFIG runtime.ai.transport_retry_max_attempts = 1")
        .expect("disable transport retries");
    rt.execute_query("SET CONFIG red.config.ai.ask.planner = true")
        .expect("enable planner-first path");
}

fn text(record: &UnifiedRecord, col: &str) -> Option<String> {
    match record.get(col)? {
        Value::Text(s) => Some(s.to_string()),
        _ => None,
    }
}

// ===========================================================================
// 1 + 2 + 5 + 6 — factual end-to-end, narrowed-slice prompt, audit, models.
// ===========================================================================

#[test]
fn factual_ask_generates_executes_and_cites_over_executed_rows() {
    let _env = env_lock().lock().expect("env lock");

    let stub = ScriptedStub::start(vec![
        // Planner call → typed plan with a read-only query step.
        "{\"intent\":\"factual\",\"query\":\"SELECT * FROM travelers WHERE passport = 'FDD-1'\",\"rationale\":\"passport lookup\"}".to_string(),
        // Synthesis call → cited answer over the executed rows.
        "Passport FDD-1 belongs to Alice [^1]".to_string(),
    ]);
    let _p = EnvVarGuard::set("REDDB_AI_PROVIDER", "openai");
    let _b = EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
    let _k = EnvVarGuard::set("REDDB_OPENAI_API_KEY", "sk-test-mock");
    let _m = EnvVarGuard::set("REDDB_OPENAI_PROMPT_MODEL", "synth-main");

    let rt = open_rt();
    exec(
        &rt,
        "CREATE TABLE travelers (id TEXT PRIMARY KEY, passport TEXT, name TEXT) \
         WITH CONTEXT INDEX ON (id, passport, name)",
    );
    exec(
        &rt,
        "INSERT INTO travelers (id, passport, name) VALUES \
           ('t1', 'FDD-1', 'Alice'), ('t2', 'FDD-2', 'Bob')",
    );
    // Collections the question does not touch — must never reach the planner.
    exec(
        &rt,
        "CREATE TABLE orders (id TEXT PRIMARY KEY, sku TEXT) WITH CONTEXT INDEX ON (id, sku)",
    );
    exec(&rt, "INSERT INTO orders (id, sku) VALUES ('o1', 'WIDGET')");

    configure_planner(&rt);
    // Independent planner model (criterion 9).
    rt.execute_query("SET CONFIG red.config.ai.ask.planner_model = 'planner-mini'")
        .expect("set planner model");

    let result = rt
        .execute_query("ASK 'who owns passport FDD-1?' STRICT OFF LIMIT 5")
        .expect("planner-first factual ASK should succeed");
    let record = result
        .result
        .records
        .first()
        .expect("ASK returns one canonical row");

    // (1) Cited synthesis over the executed rows.
    let answer = text(record, "answer").expect("answer column");
    assert!(
        answer.contains("Alice"),
        "answer should be grounded in the executed row, got: {answer:?}"
    );
    let intent = text(record, "intent").expect("intent column");
    assert_eq!(intent, "factual");
    let executed_query = text(record, "executed_query").expect("executed_query column");
    assert_eq!(
        executed_query,
        "SELECT * FROM travelers WHERE passport = 'FDD-1'"
    );
    let sources_count = match record.get("sources_count") {
        Some(Value::Integer(n)) => *n,
        other => panic!("sources_count must be Integer, got {other:?}"),
    };
    assert_eq!(sources_count, 1, "one executed row → one source");

    // Both the planner and the synthesis LLM were called.
    assert!(
        stub.request_count() >= 2,
        "planner + synthesis calls expected, got {}",
        stub.request_count()
    );

    // (2) The planner prompt carried only the funnel-narrowed slice.
    let bodies = stub.chat_bodies();
    let planner_body = &bodies[0];
    assert!(
        planner_body.contains("travelers"),
        "planner prompt must include the narrowed collection"
    );
    assert!(
        !planner_body.contains("orders"),
        "planner prompt must NOT include a collection the funnel did not select: {planner_body}"
    );

    // (6) Planner and synthesis used distinct, independently-selected models.
    assert!(
        planner_body.contains("planner-mini"),
        "planner call must use red.config.ai.ask.planner_model"
    );
    let synth_body = &bodies[1];
    assert!(
        synth_body.contains("synth-main"),
        "synthesis call must use the synthesis model, not the planner model"
    );

    // (5) Audit row grows intent, plan summary, executed query.
    let audit = rt
        .execute_query("SELECT * FROM red_ask_audit")
        .expect("read audit rows");
    let audit_row = audit
        .result
        .records
        .iter()
        .find(|r| text(r, "intent").as_deref() == Some("factual"))
        .expect("a factual audit row");
    assert_eq!(
        text(audit_row, "executed_query").as_deref(),
        Some("SELECT * FROM travelers WHERE passport = 'FDD-1'")
    );
    assert!(
        text(audit_row, "plan_summary")
            .map(|s| s.contains("intent=factual"))
            .unwrap_or(false),
        "audit plan_summary must record the routed intent"
    );
}

// ===========================================================================
// 3 — multi-collection question → global-`any` candidate grounds the answer.
// ===========================================================================

#[test]
fn multi_collection_question_executes_global_any_candidate() {
    let _env = env_lock().lock().expect("env lock");

    let stub = ScriptedStub::start(vec![
        "{\"intent\":\"factual\",\"query\":\"SELECT * WHERE passport = 'FDD-9'\",\"rationale\":\"cross-collection passport lookup\"}".to_string(),
        "The passport appears in travelers and trips [^1][^2]".to_string(),
    ]);
    let _p = EnvVarGuard::set("REDDB_AI_PROVIDER", "openai");
    let _b = EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
    let _k = EnvVarGuard::set("REDDB_OPENAI_API_KEY", "sk-test-mock");
    let _m = EnvVarGuard::set("REDDB_OPENAI_PROMPT_MODEL", "synth-main");

    let rt = open_rt();
    exec(
        &rt,
        "CREATE TABLE travelers (id TEXT PRIMARY KEY, passport TEXT, name TEXT) \
         WITH CONTEXT INDEX ON (id, passport, name)",
    );
    exec(
        &rt,
        "INSERT INTO travelers (id, passport, name) VALUES ('t9', 'FDD-9', 'Carol')",
    );
    exec(
        &rt,
        "CREATE TABLE trips (id TEXT PRIMARY KEY, passport TEXT, city TEXT) \
         WITH CONTEXT INDEX ON (id, passport, city)",
    );
    exec(
        &rt,
        "INSERT INTO trips (id, passport, city) VALUES ('tr9', 'FDD-9', 'Lisbon')",
    );

    configure_planner(&rt);

    let result = rt
        .execute_query("ASK 'what records mention passport FDD-9?' STRICT OFF LIMIT 10")
        .expect("multi-collection ASK should succeed");
    let record = result.result.records.first().expect("one canonical row");

    let executed_query = text(record, "executed_query").expect("executed_query column");
    assert_eq!(executed_query, "SELECT * WHERE passport = 'FDD-9'");
    let sources_count = match record.get("sources_count") {
        Some(Value::Integer(n)) => *n,
        other => panic!("sources_count must be Integer, got {other:?}"),
    };
    assert!(
        sources_count >= 2,
        "global-`any` candidate should ground rows from both collections, got {sources_count}"
    );
    let sources_flat = match record.get("sources_flat") {
        Some(Value::Json(bytes)) => String::from_utf8_lossy(bytes).to_string(),
        Some(Value::Text(s)) => s.to_string(),
        other => panic!("sources_flat must be JSON/Text, got {other:?}"),
    };
    assert!(
        sources_flat.contains("Carol") && sources_flat.contains("Lisbon"),
        "executed rows from both collections must appear in sources_flat: {sources_flat}"
    );
}

// ===========================================================================
// 4 — a mutating candidate is refused and never executed.
// ===========================================================================

#[test]
fn mutating_candidate_is_refused_and_never_executed() {
    let _env = env_lock().lock().expect("env lock");

    let stub = ScriptedStub::start(vec![
        // The planner (mis)classifies to a mutating candidate; the parser +
        // classifier catch it and the path refuses without executing.
        "{\"intent\":\"factual\",\"query\":\"DELETE FROM travelers WHERE passport = 'FDD-1'\",\"rationale\":\"oops\"}".to_string(),
    ]);
    let _p = EnvVarGuard::set("REDDB_AI_PROVIDER", "openai");
    let _b = EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
    let _k = EnvVarGuard::set("REDDB_OPENAI_API_KEY", "sk-test-mock");
    let _m = EnvVarGuard::set("REDDB_OPENAI_PROMPT_MODEL", "synth-main");

    let rt = open_rt();
    exec(
        &rt,
        "CREATE TABLE travelers (id TEXT PRIMARY KEY, passport TEXT, name TEXT) \
         WITH CONTEXT INDEX ON (id, passport, name)",
    );
    exec(
        &rt,
        "INSERT INTO travelers (id, passport, name) VALUES ('t1', 'FDD-1', 'Alice')",
    );

    configure_planner(&rt);

    let result = rt
        .execute_query("ASK 'delete traveler passport FDD-1' STRICT OFF")
        .expect("refusal is a structured result, not an error");
    let record = result.result.records.first().expect("one row");
    assert_eq!(record.get("refused"), Some(&Value::Boolean(true)));
    assert_eq!(text(record, "candidate_type").as_deref(), Some("delete"));

    // Only the planner was called; no synthesis.
    assert_eq!(stub.request_count(), 1, "mutating plan must not synthesize");

    // The row is untouched — the mutating candidate never executed.
    let survivors = rt
        .execute_query("SELECT * FROM travelers WHERE passport = 'FDD-1'")
        .expect("select survivors");
    assert_eq!(
        survivors.result.records.len(),
        1,
        "the mutating candidate must never execute under any flag"
    );
}

// ===========================================================================
// Scripted mock stub — sequenced chat completions + request-body capture.
// ===========================================================================

struct ScriptedStub {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    chat_bodies: Arc<Mutex<Vec<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl ScriptedStub {
    fn start(chat_responses: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("stub bind");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let addr = listener.local_addr().expect("local addr");
        let shutdown = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let chat_bodies = Arc::new(Mutex::new(Vec::new()));
        let server_shutdown = Arc::clone(&shutdown);
        let server_requests = Arc::clone(&requests);
        let server_bodies = Arc::clone(&chat_bodies);
        let mut queue: VecDeque<String> = chat_responses.into();
        let mut last = "(no scripted response) [^1]".to_string();

        let handle = thread::spawn(move || {
            while !server_shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let (path, body) = read_request(&mut stream);
                        server_requests.fetch_add(1, Ordering::Relaxed);
                        if path.contains("/embeddings") {
                            write_embedding_response(&mut stream);
                        } else {
                            server_bodies.lock().unwrap().push(body);
                            let answer = queue.pop_front().unwrap_or_else(|| last.clone());
                            last = answer.clone();
                            write_chat_response(&mut stream, &answer);
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            addr,
            shutdown,
            requests,
            chat_bodies,
            handle: Some(handle),
        }
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }

    fn request_count(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    fn chat_bodies(&self) -> Vec<String> {
        self.chat_bodies.lock().unwrap().clone()
    }
}

impl Drop for ScriptedStub {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Read a full HTTP/1.1 request; return (request-target, body).
fn read_request(stream: &mut TcpStream) -> (String, String) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    let mut buffer = [0u8; 4096];
    let mut out = Vec::new();
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&buffer[..n]);
                if out.len() > 256 * 1024 {
                    break;
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&out).to_string();
    let path = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("")
        .to_string();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (path, body)
}

fn write_embedding_response(stream: &mut TcpStream) {
    let body = r#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.1,0.2,0.3]}],"model":"mock-embedding","usage":{"prompt_tokens":1,"total_tokens":1}}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn write_chat_response(stream: &mut TcpStream, answer: &str) {
    let escaped = answer.replace('\\', "\\\\").replace('"', "\\\"");
    let body = format!(
        r#"{{"id":"chatcmpl-mock","object":"chat.completion","model":"mock-chat","choices":[{{"index":0,"message":{{"role":"assistant","content":"{escaped}"}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":4,"completion_tokens":4,"total_tokens":8}}}}"#
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

struct EnvVarGuard {
    name: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let previous = std::env::var(name).ok();
        std::env::set_var(name, value);
        Self { name, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}
