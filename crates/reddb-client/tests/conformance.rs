#![cfg(feature = "embedded")]

//! SDK Helper Spec — conformance harness.
//!
//! Spec: `docs/spec/sdk-helpers.md` (v1.0).
//!
//! Each test corresponds to one case ID in §12 of the spec. The case body
//! lives in a `case_*` function that takes a `&Reddb`, so the *same* assertions
//! run against **both** transports the issue requires:
//!
//! - **Embedded** (`memory://`) — always on. One `#[tokio::test]` per case,
//!   named after the case ID (dots → underscores) so cross-driver CI dashboards
//!   line up.
//! - **Client** (`red://`, gRPC) — gated behind `RED_SMOKE=1` + `RED_BIN` and
//!   the `grpc` feature (see the `client_transport` module). The harness spawns
//!   one `red server` process and replays every case body over the wire,
//!   proving the helper surface is transport-agnostic rather than asserting it
//!   by comment.
//!
//! Other-language drivers MUST port the same case IDs verbatim.

use reddb_client::{ErrorCode, JsonValue, ListOptions, Reddb, ValueOut};

fn field<'a>(row: &'a [(String, ValueOut)], name: &str) -> &'a ValueOut {
    row.iter()
        .find(|(column, _)| column == name)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("missing column {name}: {row:?}"))
}

// ============================================================================
// Case bodies — transport-agnostic. Each takes a connected `Reddb` and asserts
// the spec §12 contract. Collection / table / key names are unique per case so
// the suite can replay every case against one shared client-transport server.
// ============================================================================

// ---------------------------------------------------------------- generic.*

async fn case_generic_query_no_params(db: &Reddb) {
    db.query("CREATE TABLE generic_q (id INTEGER, name TEXT)")
        .await
        .expect("create");
    db.query("INSERT INTO generic_q (id, name) VALUES (1, 'a')")
        .await
        .expect("insert");
    let r = db
        .query("SELECT id, name FROM generic_q")
        .await
        .expect("select");
    assert_eq!(r.rows.len(), 1);
    assert_eq!(field(&r.rows[0], "name"), &ValueOut::String("a".into()));
}

async fn case_generic_query_with_params(db: &Reddb) {
    db.query("CREATE TABLE generic_p (id INTEGER, name TEXT)")
        .await
        .expect("create");
    db.execute_with(
        "INSERT INTO generic_p (id, name) VALUES ($1, $2)",
        (42i64, "alice"),
    )
    .await
    .expect("insert with");
    let r = db
        .query_with("SELECT name FROM generic_p WHERE id = $1", (42i64,))
        .await
        .expect("select with");
    assert_eq!(r.rows.len(), 1);
    assert_eq!(field(&r.rows[0], "name"), &ValueOut::String("alice".into()));
}

async fn case_generic_insert_rid(db: &Reddb) {
    let r = db
        .insert(
            "generic_ins",
            &JsonValue::object([("name", JsonValue::string("eve"))]),
        )
        .await
        .expect("insert");
    assert_eq!(r.affected, 1, "InsertResult.affected must be 1");
    let rid = r.rid.expect("InsertResult.rid must be present");
    assert!(!rid.is_empty(), "InsertResult.rid must be non-empty");
}

async fn case_generic_bulk_insert_rids(db: &Reddb) {
    // Empty is a no-op (spec §3.4 v1.0).
    let empty = db.bulk_insert("generic_bulk", &[]).await.expect("empty");
    assert_eq!(empty.affected, 0);
    assert!(empty.rids.is_empty());

    // Non-empty preserves order and length.
    let payloads = vec![
        JsonValue::object([("idx", JsonValue::number(0.0))]),
        JsonValue::object([("idx", JsonValue::number(1.0))]),
        JsonValue::object([("idx", JsonValue::number(2.0))]),
    ];
    let r = db
        .bulk_insert("generic_bulk", &payloads)
        .await
        .expect("bulk");
    assert_eq!(r.affected, 3);
    assert_eq!(r.rids.len(), 3);
    // Rids are unique.
    let mut s = r.rids.clone();
    s.sort();
    s.dedup();
    assert_eq!(s.len(), 3, "rids must be unique");
}

async fn case_generic_delete(db: &Reddb) {
    let ins = db
        .insert(
            "generic_del",
            &JsonValue::object([("name", JsonValue::string("k"))]),
        )
        .await
        .expect("insert");
    let rid = ins.rid.expect("rid present");
    let n = db.delete("generic_del", &rid).await.expect("delete");
    assert_eq!(n, 1, "delete must report 1 affected for an existing rid");
}

// -------------------------------------------------------------- documents.*

async fn case_documents_crud_nested_patch(db: &Reddb) {
    let docs = db.documents();

    let inserted = docs
        .insert(
            "events",
            &JsonValue::object([
                ("event_type", JsonValue::string("login")),
                ("attempts", JsonValue::number(2.0)),
                ("success", JsonValue::bool(true)),
            ]),
        )
        .await
        .expect("insert");
    assert!(!inserted.rid.is_empty());

    let fetched = docs.get("events", &inserted.rid).await.expect("get");
    assert_eq!(fetched.rid, inserted.rid);
    assert_eq!(
        field(&fetched.fields, "event_type"),
        &ValueOut::String("login".into())
    );

    let listed = docs
        .list("events", ListOptions::new())
        .await
        .expect("list");
    assert!(!listed.items.is_empty());

    let patched = docs
        .patch(
            "events",
            &inserted.rid,
            &JsonValue::object([("attempts", JsonValue::number(3.0))]),
        )
        .await
        .expect("patch");
    // Unrelated fields must survive a top-level merge patch.
    assert_eq!(
        field(&patched.fields, "event_type"),
        &ValueOut::String("login".into()),
        "patch must preserve unrelated fields"
    );

    let del = docs.delete("events", &inserted.rid).await.expect("delete");
    assert_eq!(del.affected, 1);
    assert!(del.deleted);
}

async fn case_documents_delete_missing_no_error(db: &Reddb) {
    // Force the collection to exist by inserting + deleting first.
    let ins = db
        .documents()
        .insert(
            "events_missing",
            &JsonValue::object([("k", JsonValue::string("v"))]),
        )
        .await
        .expect("insert");
    db.documents()
        .delete("events_missing", &ins.rid)
        .await
        .expect("first delete");
    // Now delete a definitely-absent rid: must NOT error.
    let r = db
        .documents()
        .delete("events_missing", "rid_that_does_not_exist")
        .await
        .expect("delete missing must not error");
    assert_eq!(r.affected, 0);
    assert!(!r.deleted);
}

async fn case_documents_patch_empty_rejects(db: &Reddb) {
    let ins = db
        .documents()
        .insert(
            "events_patch",
            &JsonValue::object([("k", JsonValue::string("v"))]),
        )
        .await
        .expect("insert");
    let err = db
        .documents()
        .patch(
            "events_patch",
            &ins.rid,
            &JsonValue::object(Vec::<(&str, JsonValue)>::new()),
        )
        .await
        .expect_err("empty patch must reject");
    assert_eq!(err.code, ErrorCode::InvalidArgument);
}

// --------------------------------------------------------------------- kv.*

async fn case_kv_exact_key_round_trip(db: &Reddb) {
    let kv = db.kv_collection("conf_kv");
    let key = "characters:hansel";

    kv.set(key, JsonValue::string("witch")).await.expect("set");
    let got = kv.get(key).await.expect("get").expect("present");
    assert_eq!(got.key, key, "key must round trip without normalisation");
    assert_eq!(got.value, ValueOut::String("witch".into()));
}

async fn case_kv_missing_get_returns_none(db: &Reddb) {
    let kv = db.kv_collection("conf_kv_missing");
    // Touch the collection so it exists.
    kv.set("seed", JsonValue::string("v")).await.expect("seed");
    let got = kv.get("never:set").await.expect("missing get must not error");
    assert!(
        got.is_none(),
        "kv.get on missing key must return None, not NOT_FOUND"
    );
}

async fn case_kv_delete_returns_envelope(db: &Reddb) {
    let kv = db.kv_collection("conf_kv_del");
    kv.set("k", JsonValue::string("v")).await.expect("set");
    let r = kv.delete("k").await.expect("delete");
    assert_eq!(r.affected, 1);
    assert!(r.deleted);
    // Second delete of the same key is not an error.
    let r2 = kv.delete("k").await.expect("delete missing must not error");
    assert_eq!(r2.affected, 0);
    assert!(!r2.deleted);
}

// ----------------------------------------------------------------- queues.*

async fn case_queues_fifo_peek_pop_len(db: &Reddb) {
    let q = db.queue();
    q.create("conf_q").await.expect("create");
    q.push("conf_q", &JsonValue::object([("n", JsonValue::number(1.0))]))
        .await
        .expect("push 1");
    q.push("conf_q", &JsonValue::object([("n", JsonValue::number(2.0))]))
        .await
        .expect("push 2");

    assert_eq!(q.len("conf_q").await.expect("len"), 2);

    let peeked = q.peek("conf_q", Some(1)).await.expect("peek");
    assert_eq!(peeked.items.len(), 1, "peek 1 must return one item");
    // peek must not decrement length.
    assert_eq!(q.len("conf_q").await.expect("len after peek"), 2);

    let popped = q.pop("conf_q").await.expect("pop");
    assert_eq!(popped.items.len(), 1);
    assert_eq!(q.len("conf_q").await.expect("len after pop"), 1);
}

async fn case_queues_empty_pop_returns_empty(db: &Reddb) {
    let q = db.queue();
    q.create("conf_q_empty").await.expect("create");
    let r = q
        .pop("conf_q_empty")
        .await
        .expect("pop on empty must not error");
    assert!(
        r.items.is_empty(),
        "empty pop must return empty items, NOT raise"
    );
}

async fn case_queues_purge_resets_len(db: &Reddb) {
    let q = db.queue();
    q.create("conf_q_purge").await.expect("create");
    for i in 0..3 {
        q.push(
            "conf_q_purge",
            &JsonValue::object([("i", JsonValue::number(i as f64))]),
        )
        .await
        .expect("push");
    }
    assert_eq!(q.len("conf_q_purge").await.expect("len"), 3);
    q.purge("conf_q_purge").await.expect("purge");
    assert_eq!(q.len("conf_q_purge").await.expect("len after purge"), 0);
}

// --------------------------------------------------------------------- tx.*

async fn case_tx_commit_persists(db: &Reddb) {
    db.query("CREATE TABLE conf_tx_commit (name TEXT)")
        .await
        .expect("create");
    db.begin().await.expect("begin");
    db.query("INSERT INTO conf_tx_commit (name) VALUES ('keep')")
        .await
        .expect("insert");
    db.commit().await.expect("commit");
    let r = db
        .query("SELECT name FROM conf_tx_commit WHERE name = 'keep'")
        .await
        .expect("select");
    assert_eq!(r.rows.len(), 1, "commit must persist the row");
}

async fn case_tx_rollback_discards(db: &Reddb) {
    db.query("CREATE TABLE conf_tx_rb (name TEXT)")
        .await
        .expect("create");
    db.begin().await.expect("begin");
    db.query("INSERT INTO conf_tx_rb (name) VALUES ('drop')")
        .await
        .expect("insert");
    db.rollback().await.expect("rollback");
    let r = db
        .query("SELECT name FROM conf_tx_rb WHERE name = 'drop'")
        .await
        .expect("select");
    assert!(r.rows.is_empty(), "rollback must discard the row");
}

// ----------------------------------------------------------------- errors.*

async fn case_errors_invalid_argument_empty_sql(db: &Reddb) {
    // Spec §3.1 — empty SQL must reject with INVALID_ARGUMENT before the
    // request is sent. The guard lives on `Reddb::query`, so it fires for
    // every transport identically.
    let err = db.query("").await.expect_err("empty SQL must reject");
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    let err_ws = db
        .query("   \n\t ")
        .await
        .expect_err("whitespace-only SQL must reject");
    assert_eq!(err_ws.code, ErrorCode::InvalidArgument);
}

async fn case_errors_not_found_document_get(db: &Reddb) {
    // Touch the collection so the SELECT doesn't fail with QueryError on a
    // missing table — we want to isolate the NOT_FOUND on a missing rid.
    let ins = db
        .documents()
        .insert(
            "errors_nf",
            &JsonValue::object([("k", JsonValue::string("v"))]),
        )
        .await
        .expect("insert");
    db.documents()
        .delete("errors_nf", &ins.rid)
        .await
        .expect("delete to clear");
    let err = db
        .documents()
        .get("errors_nf", "rid_definitely_missing")
        .await
        .expect_err("get of missing rid must error");
    assert_eq!(err.code, ErrorCode::NotFound);
}

// ------------------------- wire.* (provisional: SQL-only namespaces) -------
//
// Spec §§8–11: vectors / graph / time-series / probabilistic have no
// first-class helpers in v1.0. These cases pin the wire-level SQL surface
// every driver MUST be able to reach via `db.query()`. The exact SQL nouns
// are owned by the engine (the spec's snippets are illustrative); the case
// asserts the round trip reaches the engine and returns a usable envelope.

async fn case_wire_vectors_sql_round_trip(db: &Reddb) {
    // Insert auto-creates the vector collection; literal-vector search needs
    // no embedding provider, so this runs offline.
    db.query("INSERT INTO conf_vec VECTOR (dense, content) VALUES ([1.0, 0.0], 'match-a')")
        .await
        .expect("insert vector a");
    db.query("INSERT INTO conf_vec VECTOR (dense, content) VALUES ([0.0, 1.0], 'match-b')")
        .await
        .expect("insert vector b");
    let r = db
        .query("VECTOR SEARCH conf_vec SIMILAR TO [1.0, 0.0] LIMIT 1")
        .await
        .expect("vector search");
    assert_eq!(r.rows.len(), 1, "top-1 vector search must return one row");
    assert_eq!(
        field(&r.rows[0], "content"),
        &ValueOut::String("match-a".into()),
        "nearest vector must be the co-linear one"
    );
}

async fn case_wire_graph_sql_round_trip(db: &Reddb) {
    db.query("INSERT INTO conf_graph NODE (label, name) VALUES ('alice', 'Alice')")
        .await
        .expect("insert node alice");
    db.query("INSERT INTO conf_graph NODE (label, name) VALUES ('bob', 'Bob')")
        .await
        .expect("insert node bob");
    db.query(
        "INSERT INTO conf_graph EDGE (label, from_rid, to_rid) VALUES ('knows', 'alice', 'bob')",
    )
    .await
    .expect("insert edge");
    // `WITH EXPAND GRAPH` must reach the engine and return the base row plus
    // any expanded neighbours without a parse error.
    let r = db
        .query("SELECT * FROM conf_graph WHERE label = 'alice' WITH EXPAND GRAPH DEPTH 1")
        .await
        .expect("expand graph");
    assert!(
        !r.rows.is_empty(),
        "WITH EXPAND GRAPH must return at least the anchor row"
    );
}

async fn case_wire_timeseries_sql_round_trip(db: &Reddb) {
    db.query("CREATE TIMESERIES conf_ts RETENTION 7 d")
        .await
        .expect("create timeseries");
    db.query(
        "INSERT INTO conf_ts (metric, value, tags, timestamp) \
         VALUES ('cpu.idle', 94.8, {host: 'srv1'}, 1704067200000000000)",
    )
    .await
    .expect("insert point");
    let r = db
        .query("SELECT metric, value, timestamp FROM conf_ts WHERE metric = 'cpu.idle'")
        .await
        .expect("select point");
    assert_eq!(r.rows.len(), 1, "time-series point must round trip");
    assert_eq!(
        field(&r.rows[0], "metric"),
        &ValueOut::String("cpu.idle".into())
    );
}

async fn case_wire_probabilistic_hll_round_trip(db: &Reddb) {
    db.query("CREATE HLL conf_visitors")
        .await
        .expect("create hll");
    db.query("HLL ADD conf_visitors 'alice' 'bob' 'alice'")
        .await
        .expect("hll add");
    let r = db
        .query("HLL COUNT conf_visitors")
        .await
        .expect("hll count");
    assert!(!r.rows.is_empty(), "HLL COUNT must return a row");
    // Engine column is `count` today; some drivers project to `cardinality`.
    // Accept either so the harness pins the value contract, not the column
    // name (which the wire SQL surface owns).
    let count_col = r.rows[0]
        .iter()
        .find(|(c, _)| c == "count" || c == "cardinality")
        .map(|(_, v)| v)
        .expect("HLL COUNT must return a count/cardinality column");
    match count_col {
        ValueOut::Integer(n) => assert!(*n >= 1, "count must be at least 1"),
        ValueOut::Float(n) => assert!(*n >= 1.0, "count must be at least 1"),
        other => panic!("HLL COUNT must be numeric, got {other:?}"),
    }
}

// ============================================================================
// Embedded transport — `memory://`. One test per spec §12 case ID.
// ============================================================================

macro_rules! embedded_case {
    ($name:ident) => {
        #[tokio::test]
        async fn $name() {
            let db = Reddb::connect("memory://").await.expect("connect memory://");
            super::$name(&db).await;
        }
    };
}

mod embedded {
    use super::Reddb;

    embedded_case!(case_generic_query_no_params);
    embedded_case!(case_generic_query_with_params);
    embedded_case!(case_generic_insert_rid);
    embedded_case!(case_generic_bulk_insert_rids);
    embedded_case!(case_generic_delete);
    embedded_case!(case_documents_crud_nested_patch);
    embedded_case!(case_documents_delete_missing_no_error);
    embedded_case!(case_documents_patch_empty_rejects);
    embedded_case!(case_kv_exact_key_round_trip);
    embedded_case!(case_kv_missing_get_returns_none);
    embedded_case!(case_kv_delete_returns_envelope);
    embedded_case!(case_queues_fifo_peek_pop_len);
    embedded_case!(case_queues_empty_pop_returns_empty);
    embedded_case!(case_queues_purge_resets_len);
    embedded_case!(case_tx_commit_persists);
    embedded_case!(case_tx_rollback_discards);
    embedded_case!(case_errors_invalid_argument_empty_sql);
    embedded_case!(case_errors_not_found_document_get);
    embedded_case!(case_wire_vectors_sql_round_trip);
    embedded_case!(case_wire_graph_sql_round_trip);
    embedded_case!(case_wire_timeseries_sql_round_trip);
    embedded_case!(case_wire_probabilistic_hll_round_trip);
}

// ============================================================================
// Client transport — `red://` over gRPC. Replays every case body against a
// live `red server`. Gated behind the `grpc` feature + `RED_SMOKE=1` +
// `RED_BIN`, mirroring the `redwire_query_with_live` smoke contract so the
// default `cargo test` run (embedded only) is unaffected.
// ============================================================================

#[cfg(feature = "grpc")]
mod client_transport {
    use super::*;
    use std::net::TcpListener;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn client_transport_conformance_suite() {
        if std::env::var("RED_SMOKE").as_deref() != Ok("1") {
            eprintln!(
                "skipping client-transport conformance; set RED_SMOKE=1 and RED_BIN=/path/to/red"
            );
            return;
        }
        let bin = match std::env::var("RED_BIN") {
            Ok(path) if std::path::Path::new(&path).exists() => path,
            _ => {
                eprintln!("skipping client-transport conformance; RED_BIN is unset or missing");
                return;
            }
        };

        let port = pick_free_port().expect("pick port");
        let data_dir = std::env::temp_dir().join(format!(
            "reddb-rust-conformance-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&data_dir).expect("scratch dir");

        let mut server = Command::new(&bin)
            .arg("server")
            .arg("--grpc")
            .arg("--grpc-bind")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--path")
            .arg(data_dir.join("data.db"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn red server");

        let result = run_suite(port).await;

        let _ = server.kill();
        let _ = server.wait();
        let _ = std::fs::remove_dir_all(&data_dir);

        result.expect("client-transport conformance suite");
    }

    async fn run_suite(port: u16) -> Result<(), Box<dyn std::error::Error>> {
        let db = wait_for_connection(port).await?;

        // Same case bodies as the embedded suite — proves the helper surface
        // is transport-agnostic rather than asserting it by comment.
        case_generic_query_no_params(&db).await;
        case_generic_query_with_params(&db).await;
        case_generic_insert_rid(&db).await;
        case_generic_bulk_insert_rids(&db).await;
        case_generic_delete(&db).await;
        case_documents_crud_nested_patch(&db).await;
        case_documents_delete_missing_no_error(&db).await;
        case_documents_patch_empty_rejects(&db).await;
        case_kv_exact_key_round_trip(&db).await;
        case_kv_missing_get_returns_none(&db).await;
        case_kv_delete_returns_envelope(&db).await;
        case_queues_fifo_peek_pop_len(&db).await;
        case_queues_empty_pop_returns_empty(&db).await;
        case_queues_purge_resets_len(&db).await;
        case_tx_commit_persists(&db).await;
        case_tx_rollback_discards(&db).await;
        case_errors_invalid_argument_empty_sql(&db).await;
        case_errors_not_found_document_get(&db).await;
        case_wire_vectors_sql_round_trip(&db).await;
        case_wire_graph_sql_round_trip(&db).await;
        case_wire_timeseries_sql_round_trip(&db).await;
        case_wire_probabilistic_hll_round_trip(&db).await;

        let _ = db.close().await;
        Ok(())
    }

    async fn wait_for_connection(port: u16) -> Result<Reddb, Box<dyn std::error::Error>> {
        let uri = format!("red://127.0.0.1:{port}");
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut last_error = None;
        while Instant::now() < deadline {
            match Reddb::connect(&uri).await {
                Ok(db) => match db.query("SELECT 1").await {
                    Ok(_) => return Ok(db),
                    Err(err) => {
                        last_error = Some(err.to_string());
                        let _ = db.close().await;
                    }
                },
                Err(err) => last_error = Some(err.to_string()),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(format!(
            "server did not accept gRPC connections on {uri}: {}",
            last_error.unwrap_or_else(|| "timed out".into())
        )
        .into())
    }

    fn pick_free_port() -> std::io::Result<u16> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        Ok(port)
    }
}
