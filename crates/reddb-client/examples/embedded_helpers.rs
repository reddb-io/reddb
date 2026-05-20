//! Embedded rich-helper tour — runnable against the in-process engine.
//!
//! Demonstrates the SDK Helper Spec v1.0 surface (`docs/spec/sdk-helpers.md`)
//! over the `embedded` transport. No server, no network: `memory://` opens the
//! engine in-process. Run with:
//!
//! ```sh
//! cargo run -p reddb-io-client --example embedded_helpers
//! ```

use reddb_client::{JsonValue, ListOptions, Reddb, HELPER_SPEC_VERSION};

#[tokio::main]
async fn main() -> reddb_client::Result<()> {
    println!("reddb-io-client {} — Helper Spec v{HELPER_SPEC_VERSION}", reddb_client::version());

    let db = Reddb::connect("memory://").await?;

    // --- generic helpers --------------------------------------------------
    let inserted = db
        .insert(
            "users",
            &JsonValue::object([("name", JsonValue::string("Alice"))]),
        )
        .await?;
    println!("insert → affected={} rid={:?}", inserted.affected, inserted.rid);

    let bulk = db
        .bulk_insert(
            "users",
            &[
                JsonValue::object([("name", JsonValue::string("Bob"))]),
                JsonValue::object([("name", JsonValue::string("Carol"))]),
            ],
        )
        .await?;
    println!("bulk_insert → {} rids (input order preserved)", bulk.rids.len());

    let rows = db.query_with("SELECT * FROM users WHERE name = $1", ("Alice",)).await?;
    println!("query_with → {} row(s)", rows.rows.len());

    // --- documents.* ------------------------------------------------------
    let doc = db
        .documents()
        .insert(
            "events",
            &JsonValue::object([
                ("event_type", JsonValue::string("login")),
                ("attempts", JsonValue::number(1.0)),
            ]),
        )
        .await?;
    db.documents()
        .patch("events", &doc.rid, &JsonValue::object([("attempts", JsonValue::number(2.0))]))
        .await?;
    let recent = db
        .documents()
        .list("events", ListOptions::new().filter("event_type = 'login'").limit(10))
        .await?;
    println!("documents → patched rid {}, {} listed", doc.rid, recent.items.len());

    // --- kv.* (exact, un-normalised keys) ---------------------------------
    let kv = db.kv_collection("settings");
    kv.set("characters:hansel", JsonValue::string("trail")).await?;
    let value = kv.get("characters:hansel").await?;
    println!("kv → characters:hansel = {value:?}");

    // --- queues.* (FIFO) --------------------------------------------------
    let queue = db.queue();
    queue.create("jobs").await?;
    queue
        .push("jobs", &JsonValue::object([("kind", JsonValue::string("email"))]))
        .await?;
    let popped = queue.pop("jobs").await?;
    println!("queues → popped {} job(s)", popped.items.len());

    // --- tx.* (imperative begin/commit) -----------------------------------
    db.query("CREATE TABLE ledger (entry TEXT)").await?;
    db.begin().await?;
    db.query("INSERT INTO ledger (entry) VALUES ('opening')").await?;
    db.commit().await?;
    println!("tx → committed");

    db.close().await?;
    Ok(())
}
