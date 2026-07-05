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

    // Exactly one chat completion — the planner. No synthesis call is
    // made for a refused mutating candidate. (The funnel may separately
    // hit /embeddings, which is not a chat completion.)
    assert_eq!(
        stub.chat_bodies().len(),
        1,
        "mutating plan must not synthesize"
    );

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
// #1748 — grounding critique: a question that grounds nothing (even after the
// single refine_retrieval re-funnel) returns the honest "no matching sources"
// outcome and NEVER calls the LLM to invent an answer.
// ===========================================================================

#[test]
fn ungrounded_question_returns_no_matching_sources_without_inventing() {
    let _env = env_lock().lock().expect("env lock");

    // The stub scripts no chat answers: if the planner/synthesis LLM were
    // ever called for an ungrounded question, it would fall through to the
    // canned filler — the test asserts that never happens.
    let stub = ScriptedStub::start(vec![]);
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

    // The question's tokens match no collection or column in the schema, so
    // the funnel grounds nothing on the first pass and on the single refine
    // re-funnel — the honest no-matching-sources path.
    let result = rt
        .execute_query("ASK 'quantum dragons volcano wibble' STRICT OFF")
        .expect("ungrounded ASK is a structured outcome, not an error");
    let record = result.result.records.first().expect("one canonical row");

    assert_eq!(
        record.get("no_matching_sources"),
        Some(&Value::Boolean(true)),
        "an ungrounded question must report no matching sources"
    );
    let answer = text(record, "answer").expect("answer column");
    assert!(
        answer.contains("No matching sources"),
        "answer must honestly report the grounding failure, got: {answer:?}"
    );

    // The LLM was never called to invent an answer — grounding failure short
    // circuits before any chat completion. (The funnel may still hit
    // /embeddings, which is not a chat completion.)
    assert_eq!(
        stub.chat_bodies().len(),
        0,
        "no planner/synthesis chat call may be made for an ungrounded question"
    );
}

// ===========================================================================
// 7 — #1749: a synthesis question routes to the ADR 0013 RAG path unchanged;
//     the audit row records the routed intent.
// ===========================================================================

#[test]
fn synthesis_question_routes_to_rag_and_audit_records_intent() {
    let _env = env_lock().lock().expect("env lock");

    let stub = ScriptedStub::start(vec![
        // Planner call → classifies the question as synthesis (no query).
        "{\"intent\":\"synthesis\",\"query\":null,\"rationale\":\"summarise incidents\"}"
            .to_string(),
        // RAG synthesis call → cited answer over the retrieved sources. This
        // is the ADR 0013 path, untouched by the planner.
        "Two incidents were reported: an outage and a latency spike [^1]".to_string(),
    ]);
    let _p = EnvVarGuard::set("REDDB_AI_PROVIDER", "openai");
    let _b = EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
    let _k = EnvVarGuard::set("REDDB_OPENAI_API_KEY", "sk-test-mock");
    let _m = EnvVarGuard::set("REDDB_OPENAI_PROMPT_MODEL", "synth-main");

    let rt = open_rt();
    exec(
        &rt,
        "CREATE TABLE incidents (id TEXT PRIMARY KEY, title TEXT, day TEXT) \
         WITH CONTEXT INDEX ON (id, title, day)",
    );
    exec(
        &rt,
        "INSERT INTO incidents (id, title, day) VALUES \
           ('i1', 'checkout outage', 'yesterday'), ('i2', 'latency spike', 'yesterday')",
    );

    configure_planner(&rt);

    let result = rt
        .execute_query("ASK 'summarise the incidents from yesterday' STRICT OFF LIMIT 5")
        .expect("synthesis ASK should route to RAG and succeed");
    let record = result
        .result
        .records
        .first()
        .expect("RAG ASK returns one canonical row");

    // (contract) The result row is identical to today's RAG ASK — the routing
    // decision lives in the audit row, never in the answer contract. The
    // planner-only columns must NOT appear.
    assert!(
        record.get("intent").is_none(),
        "RAG result contract must be unchanged: no `intent` column"
    );
    assert!(
        record.get("executed_query").is_none(),
        "RAG result contract must be unchanged: no `executed_query` column"
    );
    let answer = text(record, "answer").expect("answer column");
    assert!(
        answer.contains("incident"),
        "RAG answer should come back over the retrieved sources, got: {answer:?}"
    );
    // The cited-answer contract is preserved: a citations column is present.
    assert!(
        record.get("citations").is_some(),
        "RAG cited-answer contract must be preserved"
    );

    // Both the planner (classification) and the RAG synthesis LLM were called.
    assert!(
        stub.request_count() >= 2,
        "planner classification + RAG synthesis calls expected, got {}",
        stub.request_count()
    );

    // The audit row records the routed intent (#1749). No plan summary /
    // executed query — synthesis carries only the routing decision.
    let audit = rt
        .execute_query("SELECT * FROM red_ask_audit")
        .expect("read audit rows");
    let audit_row = audit
        .result
        .records
        .iter()
        .find(|r| text(r, "intent").as_deref() == Some("synthesis"))
        .expect("an audit row recording the routed synthesis intent");
    assert!(
        text(audit_row, "executed_query").is_none(),
        "synthesis routes to RAG — no executed query is recorded"
    );
}

// ===========================================================================
// #1750 — how-to intent: the suggestion envelope. A meta-language question
// ("how would I capture events into a queue?") routes to the how-to intent.
// The answer explains the approach in natural language and the envelope
// carries parser-validated statements, each flagged mutating, plus rationale.
// Mutating/DDL statements appear but are NEVER executed — no write side-effect.
// ===========================================================================

#[test]
fn how_to_question_returns_answer_plus_validated_suggestion_never_executed() {
    let _env = env_lock().lock().expect("env lock");

    // A single scripted chat completion: the planner's how-to plan. It carries
    // the natural-language answer and a suggestion of four statements — a
    // read-only SELECT, a DDL CREATE QUEUE, a mutating EVENTS BACKFILL, and one
    // unparseable string that must be dropped. No synthesis call follows.
    let plan_json = "{\"intent\":\"how_to\",\
        \"answer\":\"To capture events from orders into a queue, create a work queue and backfill the existing rows into it.\",\
        \"suggestion\":[\
          {\"rql\":\"CREATE QUEUE events_q WORK\",\"rationale\":\"the sink queue\"},\
          {\"rql\":\"EVENTS BACKFILL orders TO events_q\",\"rationale\":\"seed history\"},\
          {\"rql\":\"SELECT * FROM orders WHERE sku = 'WIDGET'\",\"rationale\":\"inspect source rows\"},\
          {\"rql\":\"capture the orders somehow into the queue please\",\"rationale\":\"unparseable\"}\
        ],\"rationale\":\"how-to guide\"}";
    let stub = ScriptedStub::start(vec![plan_json.to_string()]);
    let _p = EnvVarGuard::set("REDDB_AI_PROVIDER", "openai");
    let _b = EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
    let _k = EnvVarGuard::set("REDDB_OPENAI_API_KEY", "sk-test-mock");
    let _m = EnvVarGuard::set("REDDB_OPENAI_PROMPT_MODEL", "synth-main");

    let rt = open_rt();
    exec(
        &rt,
        "CREATE TABLE orders (id TEXT PRIMARY KEY, sku TEXT) WITH CONTEXT INDEX ON (id, sku)",
    );
    exec(
        &rt,
        "INSERT INTO orders (id, sku) VALUES ('o1', 'WIDGET'), ('o2', 'GADGET')",
    );

    configure_planner(&rt);

    let result = rt
        .execute_query("ASK 'how would I capture events from orders into a queue?' STRICT OFF")
        .expect("how-to ASK returns a structured suggestion envelope, not an error");
    let record = result.result.records.first().expect("one canonical row");

    // A natural-language answer explaining the approach.
    let answer = text(record, "answer").expect("answer column");
    assert!(
        answer.contains("queue"),
        "answer should explain the how-to approach, got: {answer:?}"
    );
    // Routed to the how-to intent.
    assert_eq!(text(record, "intent").as_deref(), Some("how_to"));
    // Nothing was executed and the suggestion is advisory.
    assert_eq!(record.get("executed"), Some(&Value::Boolean(false)));
    assert_eq!(record.get("advisory"), Some(&Value::Boolean(true)));

    // The suggestion carries only the parser-validated statements — the
    // unparseable one is dropped and never returned raw.
    let suggestion = match record.get("suggestion") {
        Some(Value::Json(bytes)) => String::from_utf8_lossy(bytes).to_string(),
        Some(Value::Text(s)) => s.to_string(),
        other => panic!("suggestion must be JSON/Text, got {other:?}"),
    };
    assert_eq!(record.get("suggestion_count"), Some(&Value::Integer(3)));
    assert_eq!(record.get("mutating_count"), Some(&Value::Integer(2)));
    // Mutating/DDL statements appear in the envelope, flagged mutating.
    assert!(
        suggestion.contains("CREATE QUEUE events_q WORK") && suggestion.contains("create_queue"),
        "the DDL CREATE QUEUE must appear in the suggestion: {suggestion}"
    );
    assert!(
        suggestion.contains("EVENTS BACKFILL orders TO events_q")
            && suggestion.contains("events_backfill"),
        "the mutating EVENTS BACKFILL must appear in the suggestion: {suggestion}"
    );
    assert!(
        suggestion.contains("\"mutating\":true"),
        "mutating statements must be flagged: {suggestion}"
    );
    assert!(
        suggestion.contains("\"mutating\":false") && suggestion.contains("select"),
        "the read-only SELECT must be flagged non-mutating: {suggestion}"
    );
    // Unparseable model output is never returned raw.
    assert!(
        !suggestion.contains("capture the orders somehow"),
        "unparseable statement text must never survive: {suggestion}"
    );

    // Exactly one chat completion — the planner. No synthesis, no execution.
    assert_eq!(
        stub.chat_bodies().len(),
        1,
        "a how-to question makes only the planner call, never a synthesis call"
    );

    // Conformance pin: no write occurred during the how-to ASK. The source
    // rows are untouched and the suggested CREATE QUEUE never materialized.
    let orders = rt
        .execute_query("SELECT * FROM orders")
        .expect("select orders");
    assert_eq!(
        orders.result.records.len(),
        2,
        "the how-to ASK must not write — source rows are untouched"
    );
    assert!(
        rt.execute_query("QUEUE LEN events_q").is_err(),
        "the suggested CREATE QUEUE must never execute — the queue does not exist"
    );

    // The audit row records the how-to intent and the suggested statement kinds.
    let audit = rt
        .execute_query("SELECT * FROM red_ask_audit")
        .expect("read audit rows");
    let audit_row = audit
        .result
        .records
        .iter()
        .find(|r| text(r, "intent").as_deref() == Some("how_to"))
        .expect("a how_to audit row");
    let plan_summary = text(audit_row, "plan_summary").expect("plan_summary column");
    assert!(
        plan_summary.contains("intent=how_to"),
        "audit plan_summary must record the how-to intent: {plan_summary}"
    );
    assert!(
        plan_summary.contains("create_queue")
            && plan_summary.contains("events_backfill")
            && plan_summary.contains("select"),
        "audit plan_summary must record the suggested statement kinds: {plan_summary}"
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
                            server_bodies.lock().expect("bodies lock").push(body);
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
        self.chat_bodies.lock().expect("bodies lock").clone()
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
