#![cfg(feature = "embedded")]

//! SDK Helper Spec — conformance harness.
//!
//! Spec: `docs/spec/sdk-helpers.md` (v1.0).
//!
//! Each test corresponds to one case ID in §12 of the spec. The harness runs
//! against `memory://`, which exercises the same transport-agnostic helper
//! code path that fires for `grpc://`, `http://`, and `red://` targets — the
//! helper methods on `Reddb` dispatch into the embedded engine here, but the
//! envelopes, error codes, and validation are the same surface every
//! external driver sees.
//!
//! Other-language drivers MUST port the same case IDs verbatim. The case ID
//! is encoded in each test function name so cross-driver CI dashboards line
//! up.

use reddb_client::{ErrorCode, JsonValue, ListOptions, Reddb, ValueOut};

fn field<'a>(row: &'a [(String, ValueOut)], name: &str) -> &'a ValueOut {
    row.iter()
        .find(|(column, _)| column == name)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("missing column {name}: {row:?}"))
}

// ---------------------------------------------------------------- generic.*

#[tokio::test]
async fn generic_query_no_params() {
    let db = Reddb::connect("memory://").await.expect("connect");
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

#[tokio::test]
async fn generic_query_with_params() {
    let db = Reddb::connect("memory://").await.expect("connect");
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

#[tokio::test]
async fn generic_insert_rid() {
    let db = Reddb::connect("memory://").await.expect("connect");
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

#[tokio::test]
async fn generic_bulk_insert_rids() {
    let db = Reddb::connect("memory://").await.expect("connect");

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

#[tokio::test]
async fn generic_delete() {
    let db = Reddb::connect("memory://").await.expect("connect");
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

#[tokio::test]
async fn documents_crud_nested_patch() {
    let db = Reddb::connect("memory://").await.expect("connect");
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

#[tokio::test]
async fn documents_delete_missing_no_error() {
    let db = Reddb::connect("memory://").await.expect("connect");
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

#[tokio::test]
async fn documents_patch_empty_rejects() {
    let db = Reddb::connect("memory://").await.expect("connect");
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
        .patch("events_patch", &ins.rid, &JsonValue::object(Vec::<(&str, JsonValue)>::new()))
        .await
        .expect_err("empty patch must reject");
    assert_eq!(err.code, ErrorCode::InvalidArgument);
}

// --------------------------------------------------------------------- kv.*

#[tokio::test]
async fn kv_exact_key_round_trip() {
    let db = Reddb::connect("memory://").await.expect("connect");
    let kv = db.kv_collection("conf_kv");
    let key = "characters:hansel";

    kv.set(key, JsonValue::string("witch"))
        .await
        .expect("set");
    let got = kv.get(key).await.expect("get").expect("present");
    assert_eq!(got.key, key, "key must round trip without normalisation");
    assert_eq!(got.value, ValueOut::String("witch".into()));
}

#[tokio::test]
async fn kv_missing_get_returns_none() {
    let db = Reddb::connect("memory://").await.expect("connect");
    let kv = db.kv_collection("conf_kv_missing");
    // Touch the collection so it exists.
    kv.set("seed", JsonValue::string("v")).await.expect("seed");
    let got = kv.get("never:set").await.expect("missing get must not error");
    assert!(
        got.is_none(),
        "kv.get on missing key must return None, not NOT_FOUND"
    );
}

#[tokio::test]
async fn kv_delete_returns_envelope() {
    let db = Reddb::connect("memory://").await.expect("connect");
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

#[tokio::test]
async fn queues_fifo_peek_pop_len() {
    let db = Reddb::connect("memory://").await.expect("connect");
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

#[tokio::test]
async fn queues_empty_pop_returns_empty() {
    let db = Reddb::connect("memory://").await.expect("connect");
    let q = db.queue();
    q.create("conf_q_empty").await.expect("create");
    let r = q.pop("conf_q_empty").await.expect("pop on empty must not error");
    assert!(
        r.items.is_empty(),
        "empty pop must return empty items, NOT raise"
    );
}

#[tokio::test]
async fn queues_purge_resets_len() {
    let db = Reddb::connect("memory://").await.expect("connect");
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

#[tokio::test]
async fn tx_commit_persists() {
    let db = Reddb::connect("memory://").await.expect("connect");
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

#[tokio::test]
async fn tx_rollback_discards() {
    let db = Reddb::connect("memory://").await.expect("connect");
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

#[tokio::test]
async fn errors_not_found_document_get() {
    let db = Reddb::connect("memory://").await.expect("connect");
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

#[tokio::test]
async fn wire_probabilistic_hll_round_trip() {
    // Spec §11 — `probabilistic.*` has no first-class helpers in v1.0; this
    // pins the wire-level SQL surface drivers MUST be able to reach via
    // `db.query()`.
    let db = Reddb::connect("memory://").await.expect("connect");
    db.query("CREATE HLL conf_visitors").await.expect("create hll");
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
