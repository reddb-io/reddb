#![cfg(feature = "embedded")]

use reddb_client::{ErrorCode, JsonValue, ListOptions, Reddb, ValueOut};

fn field<'a>(row: &'a [(String, ValueOut)], name: &str) -> &'a ValueOut {
    row.iter()
        .find(|(column, _)| column == name)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("missing column {name}: {row:?}"))
}

#[tokio::test]
async fn document_helpers_cover_crud_filter_and_patch() {
    let db = Reddb::connect("memory://").await.expect("connect");
    let docs = db.documents();

    let inserted = docs
        .insert(
            "events",
            &JsonValue::object([
                ("event_type", JsonValue::string("login")),
                ("success", JsonValue::bool(true)),
                ("attempts", JsonValue::number(2.0)),
            ]),
        )
        .await
        .expect("insert document");
    assert!(!inserted.rid.is_empty());
    assert_eq!(
        field(&inserted.fields, "event_type"),
        &ValueOut::String("login".into())
    );

    let fetched = docs
        .get("events", &inserted.rid)
        .await
        .expect("get document");
    assert_eq!(fetched.rid, inserted.rid);

    let patched = docs
        .patch(
            "events",
            &inserted.rid,
            &JsonValue::object([
                ("attempts", JsonValue::number(3.0)),
                ("status", JsonValue::string("reviewed")),
            ]),
        )
        .await
        .expect("patch document");
    assert_eq!(field(&patched.fields, "attempts"), &ValueOut::Integer(3));
    assert_eq!(
        field(&patched.fields, "status"),
        &ValueOut::String("reviewed".into())
    );

    let listed = docs
        .list(
            "events",
            ListOptions::new()
                .filter("event_type = 'login'")
                .order_by("attempts DESC")
                .limit(5),
        )
        .await
        .expect("list documents");
    assert_eq!(listed.items.len(), 1);

    let deleted = docs
        .delete("events", &inserted.rid)
        .await
        .expect("delete document");
    assert!(deleted.deleted);
    let missing = docs
        .get("events", &inserted.rid)
        .await
        .expect_err("deleted document should be missing");
    assert_eq!(missing.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn kv_helpers_preserve_namespaced_keys_and_missing_state() {
    let db = Reddb::connect("memory://").await.expect("connect");
    let kv = db.kv_collection("settings");

    kv.set("characters:hansel", JsonValue::string("trail"))
        .await
        .expect("set namespaced key");
    let item = kv
        .get("characters:hansel")
        .await
        .expect("get namespaced key")
        .expect("key present");
    assert_eq!(item.collection, "settings");
    assert_eq!(item.key, "characters:hansel");
    assert_eq!(item.value, ValueOut::String("trail".into()));
    assert!(kv.exists("characters:hansel").await.expect("exists").exists);

    let listed = kv
        .list(ListOptions::new().filter("key = 'characters:hansel'"))
        .await
        .expect("list kv");
    assert_eq!(listed.items.len(), 1);

    let deleted = kv.delete("characters:hansel").await.expect("delete key");
    assert!(deleted.deleted);
    assert!(kv
        .get("characters:hansel")
        .await
        .expect("get deleted key")
        .is_none());
}

#[tokio::test]
async fn queue_and_transaction_helpers_use_engine_contracts() {
    let db = Reddb::connect("memory://").await.expect("connect");
    let queue = db.queue();

    queue.create("tasks").await.expect("create queue");
    queue
        .push(
            "tasks",
            &JsonValue::object([
                ("job", JsonValue::string("process")),
                ("retries", JsonValue::number(3.0)),
            ]),
        )
        .await
        .expect("push queue");
    assert_eq!(queue.len("tasks").await.expect("queue len"), 1);
    assert_eq!(
        queue
            .peek("tasks", Some(1))
            .await
            .expect("peek")
            .items
            .len(),
        1
    );
    assert_eq!(queue.pop("tasks").await.expect("pop").items.len(), 1);
    assert_eq!(queue.len("tasks").await.expect("queue len after pop"), 0);

    db.query("CREATE TABLE tx_items (name TEXT)")
        .await
        .expect("create tx table");
    db.begin().await.expect("begin");
    db.query("INSERT INTO tx_items (name) VALUES ('rollback')")
        .await
        .expect("insert in tx");
    db.rollback().await.expect("rollback");
    let rows = db
        .query("SELECT name FROM tx_items WHERE name = 'rollback'")
        .await
        .expect("select tx rows");
    assert!(rows.rows.is_empty());
}
