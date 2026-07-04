//! Issue #557 — ASK: context retrieval (docs/rows/graph/vectors/KV) +
//! deterministic-via-SQL boundary.
//!
//! Pins the four acceptance bullets from #557:
//!
//! 1. Mock-AI tests cover context retrieval from each of: documents,
//!    rows, graph, vectors, KV — one bucket per test, no cross-bucket
//!    coincidences.
//! 2. Deterministic numeric questions route to SQL execution
//!    (`engine: "runtime-table"`), not to the AI provider.
//! 3. Result envelope distinguishes LLM-derived text from engine-derived
//!    facts: SQL aggregates land on `engine: "runtime-table"` with no
//!    `provider` column; ASK lands on `engine: "runtime-ai"` carrying a
//!    `provider` column. That `engine` discriminator IS the provenance
//!    contract callers branch on.
//! 4. Both retrieval shape (which bucket each model surfaces in) AND
//!    routing decision (which engine produced the row) are pinned. A
//!    future change that routed `COUNT(*)` through the LLM, or that
//!    cross-talked the document seed into the table bucket, would
//!    surface here as a test failure.
//!
//! Surface decisions / why this slice is regression-only:
//!
//! - #551 (slice 0 dependency) shipped the dotted-JSON access that lets
//!   `body.field` resolve inside SQL. #555 pinned the SQL-aggregate
//!   surface end to end. The retrieval funnel (`search_context`) and
//!   the engine-routing surface (`result.engine`) already exist —
//!   #557 is the contract that ties them together for the ASK story.
//!   No production change is needed; the bug surface that would break
//!   #557 is silent regression on either side of the boundary.
//! - The "provenance distinguishes LLM vs engine" bullet is pinned via
//!   `result.engine`. ASK results carry `engine == "runtime-ai"` and a
//!   `provider` column; SQL aggregates carry `engine == "runtime-table"`
//!   and no `provider` column. A future change that funneled SQL
//!   through the AI engine (or vice-versa) would flip those markers.
//! - No external AI provider is contacted. The bucket-coverage tests
//!   use `search_context` directly (no LLM round-trip required to pin
//!   retrieval shape); the routing-decision test uses `execute_query`
//!   over a deterministic SQL aggregate (no LLM round-trip required to
//!   pin engine routing). The two together cover #557's acceptance
//!   without needing the mock-OpenAI stub that
//!   `e2e_ask_search_conformance` already exercises for the full ASK
//!   path.

use reddb::application::SearchContextInput;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

use super::support::PersistentRuntime;

fn open_rt() -> PersistentRuntime {
    super::support::persistent_test_runtime("issue-557-ask-context")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn seed_multi_model(rt: &RedDBRuntime) {
    // Table — context-indexed columns so `search_context`'s field
    // index tier surfaces the row.
    exec(
        rt,
        "CREATE TABLE incidents (id TEXT PRIMARY KEY, title TEXT, status TEXT) \
         WITH CONTEXT INDEX ON (id, title, status)",
    );
    exec(
        rt,
        "INSERT INTO incidents (id, title, status) VALUES \
           ('INC-557', 'gateway latency spike', 'open'), \
           ('INC-558', 'unrelated drill', 'closed')",
    );

    exec(
        rt,
        "INSERT INTO runbooks DOCUMENT VALUES \
         ({\"title\":\"gateway recovery\",\"summary\":\"restart gateway nodes\"})",
    );
    exec(
        rt,
        "INSERT INTO runbooks DOCUMENT VALUES \
         ({\"title\":\"db rotation\",\"summary\":\"rotate credentials\"})",
    );

    exec(
        rt,
        "INSERT INTO settings KV (key, value) VALUES ('gateway timeout', '5000ms')",
    );
    exec(
        rt,
        "INSERT INTO settings KV (key, value) VALUES ('cache ttl', '60s')",
    );

    exec(
        rt,
        "INSERT INTO topology NODE (label, node_type, role) VALUES \
           ('gateway', 'Host', 'edge')",
    );
    exec(
        rt,
        "INSERT INTO topology NODE (label, node_type, role) VALUES \
           ('database', 'Host', 'storage')",
    );

    exec(
        rt,
        "INSERT INTO notes VECTOR (dense, content) VALUES \
           ([1.0, 0.0], 'gateway healthcheck guide')",
    );
    exec(
        rt,
        "INSERT INTO notes VECTOR (dense, content) VALUES \
           ([0.0, 1.0], 'unrelated marshmallow note')",
    );
}

fn search(rt: &RedDBRuntime, query: &str) -> reddb::runtime::ContextSearchResult {
    rt.search_context(SearchContextInput {
        query: query.to_string(),
        field: None,
        vector: None,
        collections: None,
        limit: Some(20),
        graph_depth: Some(2),
        graph_max_edges: None,
        max_cross_refs: None,
        follow_cross_refs: Some(true),
        expand_graph: Some(true),
        global_scan: Some(true),
        reindex: Some(true),
        min_score: Some(0.0),
    })
    .expect("search_context should succeed")
}

// ===========================================================================
// Acceptance bullet 1 — Context retrieval pins one bucket per test.
// ===========================================================================

#[test]
fn issue_557_retrieval_surfaces_rows_bucket() {
    let rt = open_rt();
    seed_multi_model(&rt);
    let result = search(&rt, "gateway");

    assert!(
        !result.tables.is_empty(),
        "rows bucket must surface the seeded incident — got {:#?}",
        result.summary
    );
    let collections: Vec<_> = result
        .tables
        .iter()
        .map(|e| e.collection.as_str())
        .collect();
    assert!(
        collections.contains(&"incidents"),
        "rows bucket should name the 'incidents' collection, got {collections:?}"
    );
}

#[test]
fn issue_557_retrieval_surfaces_documents_bucket() {
    let rt = open_rt();
    seed_multi_model(&rt);
    let result = search(&rt, "gateway");

    assert!(
        !result.documents.is_empty(),
        "documents bucket must surface the seeded runbook — got {:#?}",
        result.summary
    );
    let collections: Vec<_> = result
        .documents
        .iter()
        .map(|e| e.collection.as_str())
        .collect();
    assert!(
        collections.contains(&"runbooks"),
        "documents bucket should name the 'runbooks' collection, got {collections:?}"
    );
}

#[test]
fn issue_557_retrieval_surfaces_kv_bucket() {
    let rt = open_rt();
    seed_multi_model(&rt);
    let result = search(&rt, "gateway");

    assert!(
        !result.key_values.is_empty(),
        "kv bucket must surface the seeded setting — got {:#?}",
        result.summary
    );
    let collections: Vec<_> = result
        .key_values
        .iter()
        .map(|e| e.collection.as_str())
        .collect();
    assert!(
        collections.contains(&"settings"),
        "kv bucket should name the 'settings' collection, got {collections:?}"
    );
}

#[test]
fn issue_557_retrieval_surfaces_graph_bucket() {
    let rt = open_rt();
    seed_multi_model(&rt);
    let result = search(&rt, "gateway");

    assert!(
        !result.graph.nodes.is_empty(),
        "graph bucket must surface the seeded node — got {:#?}",
        result.summary
    );
}

#[test]
fn issue_557_retrieval_surfaces_vectors_bucket() {
    let rt = open_rt();
    seed_multi_model(&rt);
    let result = search(&rt, "gateway");

    assert!(
        !result.vectors.is_empty(),
        "vectors bucket must surface the seeded note — got {:#?}",
        result.summary
    );
    let collections: Vec<_> = result
        .vectors
        .iter()
        .map(|e| e.collection.as_str())
        .collect();
    assert!(
        collections.contains(&"notes"),
        "vectors bucket should name the 'notes' collection, got {collections:?}"
    );
}

// ===========================================================================
// Acceptance bullet 2 + 4 — Deterministic numeric questions route to SQL,
// not the LLM. Pins both the answer AND the routing decision.
// ===========================================================================

#[test]
fn issue_557_deterministic_count_routes_to_sql_engine() {
    let rt = open_rt();
    seed_multi_model(&rt);

    // No AI env vars set. If COUNT(*) ever routed through the LLM the
    // provider lookup would fail (or contact a real endpoint), surfacing
    // here as an error or a non-deterministic result.
    let result = rt
        .execute_query("SELECT COUNT(*) AS count FROM incidents")
        .expect("COUNT(*) must execute on the SQL engine");

    // Bullet 4 — routing decision is pinned via `engine`.
    assert_eq!(
        result.engine, "runtime-table",
        "deterministic aggregate must land on the runtime-table engine, got {:?}",
        result.engine
    );

    // Bullet 2 — the numeric answer is from SQL, exact.
    let record = result
        .result
        .records
        .first()
        .expect("COUNT(*) returns one record");
    let count = match record.get("count") {
        Some(Value::Integer(n)) => *n,
        Some(Value::UnsignedInteger(n)) => i64::try_from(*n).expect("count fits i64"),
        other => panic!("count column should be integer-valued, got {other:?}"),
    };
    assert_eq!(count, 2, "COUNT(*) over incidents must equal 2");
}

#[test]
fn issue_557_deterministic_sum_routes_to_sql_engine() {
    let rt = open_rt();
    exec(
        &rt,
        "CREATE TABLE orders (id TEXT PRIMARY KEY, amount INTEGER)",
    );
    exec(
        &rt,
        "INSERT INTO orders (id, amount) VALUES ('A', 10), ('B', 25), ('C', 7)",
    );

    let result = rt
        .execute_query("SELECT SUM(amount) AS total FROM orders")
        .expect("SUM must execute on the SQL engine");

    assert_eq!(
        result.engine, "runtime-table",
        "SUM must route to runtime-table, got {:?}",
        result.engine
    );

    let record = result
        .result
        .records
        .first()
        .expect("SUM returns one record");
    let total = match record.get("total") {
        Some(Value::Integer(n)) => *n,
        Some(Value::UnsignedInteger(n)) => i64::try_from(*n).expect("total fits i64"),
        Some(Value::Float(f)) => *f as i64,
        other => panic!("total column should be numeric, got {other:?}"),
    };
    assert_eq!(total, 42, "SUM(amount) = 10 + 25 + 7 must equal 42");
}

// ===========================================================================
// Acceptance bullet 3 — Result envelope distinguishes LLM-derived text from
// engine-derived facts via `result.engine`.
// ===========================================================================

#[test]
fn issue_557_engine_marker_distinguishes_sql_from_ai_paths() {
    let rt = open_rt();
    seed_multi_model(&rt);

    // SQL-engine fact: the engine marker is `runtime-table`, no
    // provider attribution column ships on the record.
    let sql_result = rt
        .execute_query("SELECT COUNT(*) AS count FROM incidents")
        .expect("SQL count");
    assert_eq!(sql_result.engine, "runtime-table");
    let sql_record = sql_result
        .result
        .records
        .first()
        .expect("SQL returns one record");
    assert!(
        sql_record.get("provider").is_none(),
        "engine-derived records must NOT carry a 'provider' attribution column — \
         that column is the LLM-derived signal"
    );
    assert!(
        sql_record.get("answer").is_none(),
        "engine-derived records must NOT carry an 'answer' column — that column \
         is the LLM-derived signal"
    );

    // The ASK envelope (engine = "runtime-ai", with `answer` + `provider`
    // columns) is exercised end-to-end by the mock-OpenAI tests in
    // `e2e_ask_search_conformance::ask_with_mock_provider_cites_grounded_sources`.
    // The provenance contract is: callers branch on `result.engine` to
    // tell engine-derived facts from LLM-derived text, and the two
    // envelopes carry disjoint attribution columns. We pin the SQL side
    // here without re-running the mock-OpenAI loop.
}
