// Issue #1403 — DOCUMENT single source of truth (PRD-1398 / ADR-0063).
//
// With the `storage.binary_document_body` flag on, the body IS the document:
//   - Document writes no longer materialise promoted columns (only `body`).
//   - A `WHERE` on a top-level field resolves via the index (when one exists)
//     or by reading the body; projections offset-read the field from the body.
//   - Versioned (MVCC, ADR-0014) documents stay correct, including `AS OF`.
//
// Everything here is exercised through the high-level RQL seam
// (`RedDBRuntime::execute_query`), the same surface every client speaks.

#[path = "../../support/mod.rs"]
mod support;

use std::collections::BTreeSet;

use reddb::application::{Author, CreateCommitInput, VcsUseCases};
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::schema::Value;
use reddb::storage::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
use reddb::{RedDBOptions, RedDBRuntime};

/// In-memory runtime with the single-source binary body enabled.
fn binary_runtime() -> RedDBRuntime {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    rt.execute_query("SET CONFIG storage.binary_document_body = true")
        .expect("enable storage.binary_document_body");
    rt
}

fn insert(rt: &RedDBRuntime, collection: &str, body_json: &str) {
    rt.execute_query(&format!(
        "INSERT INTO {collection} DOCUMENT VALUES ({body_json})"
    ))
    .unwrap_or_else(|err| panic!("insert {body_json}: {err:?}"));
}

/// The set of distinct stored column names across every row of `collection`.
/// Promoted columns, if materialised, would show up here next to `body`.
fn stored_columns(rt: &RedDBRuntime, collection: &str) -> BTreeSet<String> {
    let page = rt
        .scan_collection(collection, None, 10_000)
        .expect("scan_collection");
    let mut cols = BTreeSet::new();
    for entity in &page.items {
        if let EntityData::Row(row) = &entity.data {
            for (name, _) in row.iter_fields() {
                cols.insert(name.to_string());
            }
        }
    }
    cols
}

fn as_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::Text(text)) => text.to_string(),
        other => panic!("expected text, got {other:?}"),
    }
}

fn as_int(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Integer(v)) => *v,
        Some(Value::UnsignedInteger(v)) => i64::try_from(*v).expect("u64 fits i64"),
        other => panic!("expected int, got {other:?}"),
    }
}

/// Run `sql` and collect the text column `col` from every record, in order.
fn texts(rt: &RedDBRuntime, sql: &str, col: &str) -> Vec<String> {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
        .result
        .records
        .iter()
        .map(|record| as_text(record.get(col)))
        .collect()
}

fn body_bytes(rt: &RedDBRuntime, collection: &str) -> Vec<u8> {
    let page = rt
        .scan_collection(collection, None, 1)
        .expect("scan_collection");
    let row = page.items[0].data.as_row().expect("row");
    match row.get_field("body") {
        Some(Value::Json(bytes)) => bytes.clone(),
        other => panic!("expected document body bytes, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Acceptance: writes no longer materialise promoted columns.
// ---------------------------------------------------------------------------

#[test]
fn default_writes_binary_single_source_documents() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    rt.execute_query("CREATE DOCUMENT docs").expect("create");
    insert(&rt, "docs", r#"{"name":"alice","score":30,"city":"SP"}"#);

    let cols = stored_columns(&rt, "docs");
    assert!(cols.contains("body"), "the body is always stored: {cols:?}");
    for promoted in ["name", "score", "city"] {
        assert!(
            !cols.contains(promoted),
            "default document write must not materialise `{promoted}`: {cols:?}"
        );
    }
    assert_eq!(
        texts(&rt, "SELECT name FROM docs WHERE score = 30", "name"),
        vec!["alice"],
    );
}

#[test]
fn writes_do_not_materialise_promoted_columns() {
    let rt = binary_runtime();
    rt.execute_query("CREATE DOCUMENT docs").expect("create");
    insert(&rt, "docs", r#"{"name":"alice","score":30,"city":"SP"}"#);
    insert(&rt, "docs", r#"{"name":"bob","score":10,"city":"RJ"}"#);

    let cols = stored_columns(&rt, "docs");
    assert!(cols.contains("body"), "the body is always stored: {cols:?}");
    for promoted in ["name", "score", "city"] {
        assert!(
            !cols.contains(promoted),
            "promoted column `{promoted}` must NOT be materialised: {cols:?}"
        );
    }
}

#[test]
fn live_predicates_read_binary_body_not_stale_materialised_columns() {
    let rt = binary_runtime();
    rt.execute_query("CREATE DOCUMENT source")
        .expect("create source");
    insert(&rt, "source", r#"{"name":"body","score":7}"#);
    let body = body_bytes(&rt, "source");

    rt.execute_query("CREATE DOCUMENT stale_docs")
        .expect("create stale_docs");
    let row = RowData::with_names(
        vec![Value::Json(body), Value::text("stale"), Value::Integer(7)],
        vec!["body".to_string(), "name".to_string(), "score".to_string()],
    );
    let entity = UnifiedEntity::new(
        EntityId::new(0),
        EntityKind::TableRow {
            table: "stale_docs".into(),
            row_id: 0,
        },
        EntityData::Row(row),
    );
    rt.db()
        .store()
        .insert("stale_docs", entity)
        .expect("insert stale-shaped row");

    assert_eq!(
        texts(
            &rt,
            "SELECT name FROM stale_docs WHERE name = 'body'",
            "name"
        ),
        vec!["body"],
        "live predicates and projections must source document fields from the body"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: WHERE resolves from the body (no index needed); projections
// offset-read the field from the body.
// ---------------------------------------------------------------------------

#[test]
fn bare_filter_and_projection_resolve_through_the_body() {
    let rt = binary_runtime();
    rt.execute_query("CREATE DOCUMENT docs").expect("create");
    insert(&rt, "docs", r#"{"name":"alice","score":30}"#);
    insert(&rt, "docs", r#"{"name":"bob","score":10}"#);
    insert(&rt, "docs", r#"{"name":"carol","score":50}"#);

    // Equality filter on a bare promoted field, with NO index — must read the
    // value straight from the body.
    assert_eq!(
        texts(&rt, "SELECT name FROM docs WHERE score = 30", "name"),
        vec!["alice"],
    );
    assert!(
        texts(&rt, "SELECT name FROM docs WHERE score = 999", "name").is_empty(),
        "a non-matching predicate returns nothing"
    );

    // Projection of bare promoted fields comes from the body by offset-read.
    let rows = rt
        .execute_query("SELECT name, score FROM docs WHERE name = 'carol'")
        .expect("project");
    assert_eq!(rows.result.records.len(), 1);
    assert_eq!(as_text(rows.result.records[0].get("name")), "carol");
    assert_eq!(as_int(rows.result.records[0].get("score")), 50);

    // SELECT * expands the promoted columns back out of the body.
    let star = rt
        .execute_query("SELECT * FROM docs WHERE name = 'alice'")
        .expect("star");
    let row = &star.result.records[0];
    assert_eq!(as_text(row.get("name")), "alice");
    assert_eq!(as_int(row.get("score")), 30);
}

// ---------------------------------------------------------------------------
// Acceptance: range / ORDER BY route through the ordered (BTREE) index, which
// is backed by the body — and the body stays the single source of truth.
// ---------------------------------------------------------------------------

#[test]
fn range_and_order_by_route_through_the_ordered_index() {
    let rt = binary_runtime();
    rt.execute_query("CREATE DOCUMENT docs").expect("create");
    for (name, score) in [("alice", 30), ("bob", 10), ("carol", 50), ("dave", 20)] {
        insert(
            &rt,
            "docs",
            &format!(r#"{{"name":"{name}","score":{score}}}"#),
        );
    }
    rt.execute_query("CREATE INDEX idx_score ON docs (score) USING BTREE")
        .expect("ordered index on a promoted field");
    assert!(
        rt.index_store_ref().sorted.has_index("docs", "score"),
        "the ordered index is registered on the document field"
    );

    assert_eq!(
        texts(
            &rt,
            "SELECT name FROM docs WHERE score > 20 ORDER BY score ASC",
            "name"
        ),
        vec!["alice", "carol"],
    );
    assert_eq!(
        texts(
            &rt,
            "SELECT name FROM docs WHERE score BETWEEN 15 AND 35 ORDER BY score ASC",
            "name"
        ),
        vec!["dave", "alice"],
    );
    assert_eq!(
        texts(&rt, "SELECT name FROM docs ORDER BY score DESC", "name"),
        vec!["carol", "alice", "dave", "bob"],
    );

    // The index is an accelerator, not a second copy of the data: still no
    // promoted columns on disk.
    assert!(
        !stored_columns(&rt, "docs").contains("score"),
        "indexed field must still live only in the body"
    );
}

// ---------------------------------------------------------------------------
// Acceptance: UPDATE keeps the body the single source of truth and refreshes
// the body-backed index.
// ---------------------------------------------------------------------------

#[test]
fn update_keeps_single_source_and_refreshes_index() {
    let rt = binary_runtime();
    rt.execute_query("CREATE DOCUMENT docs").expect("create");
    insert(&rt, "docs", r#"{"name":"alice","score":30}"#);
    rt.execute_query("CREATE INDEX idx_score ON docs (score) USING BTREE")
        .expect("index");

    rt.execute_query("UPDATE docs DOCUMENTS SET score = 80 WHERE name = 'alice'")
        .expect("update");

    // Still no promoted columns after the update.
    assert!(
        !stored_columns(&rt, "docs").contains("score"),
        "update must not re-materialise the promoted column"
    );

    // The new value is found via the refreshed index; the old key is gone.
    assert_eq!(
        texts(
            &rt,
            "SELECT name FROM docs WHERE score > 50 ORDER BY score",
            "name"
        ),
        vec!["alice"],
    );
    assert!(
        texts(&rt, "SELECT name FROM docs WHERE score = 30", "name").is_empty(),
        "the stale index key must be removed on update"
    );

    // The projected value reflects the update too.
    let rows = rt
        .execute_query("SELECT score FROM docs WHERE name = 'alice'")
        .expect("read");
    assert_eq!(as_int(rows.result.records[0].get("score")), 80);
}

// ---------------------------------------------------------------------------
// Acceptance: versioned (MVCC) documents stay correct, including AS OF — the
// historical version is projected from its own body.
// ---------------------------------------------------------------------------

fn commit(rt: &RedDBRuntime, conn: u64, message: &str) -> String {
    VcsUseCases::new(rt)
        .commit(CreateCommitInput {
            connection_id: conn,
            message: message.to_string(),
            author: Author {
                name: "t".to_string(),
                email: "t@reddb.io".to_string(),
            },
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .expect("commit")
        .hash
}

#[test]
fn versioned_document_as_of_projects_the_historical_body() {
    let rt = binary_runtime();
    let conn = 14_030_001;
    set_current_connection_id(conn);

    rt.execute_query("CREATE DOCUMENT vdocs").expect("create");
    VcsUseCases::new(&rt)
        .set_versioned("vdocs", true)
        .expect("versioned");

    insert(&rt, "vdocs", r#"{"name":"alice","score":1}"#);
    let p1 = commit(&rt, conn, "p1");

    rt.execute_query("UPDATE vdocs DOCUMENTS SET score = 2 WHERE name = 'alice'")
        .expect("update");
    let _p2 = commit(&rt, conn, "p2");

    // Live read: newest version, projected from its body.
    let live = rt
        .execute_query("SELECT score FROM vdocs WHERE name = 'alice'")
        .expect("live");
    assert_eq!(
        as_int(live.result.records[0].get("score")),
        2,
        "live is newest"
    );

    // AS OF P1: the prior version, projected from the prior body.
    let historical = rt
        .execute_query(&format!(
            "SELECT score FROM vdocs AS OF COMMIT '{p1}' WHERE name = 'alice'"
        ))
        .expect("as of");
    assert_eq!(
        historical.result.records.len(),
        1,
        "AS OF must resolve exactly one historical version"
    );
    assert_eq!(
        as_int(historical.result.records[0].get("score")),
        1,
        "AS OF P1 must project the historical body value"
    );

    // History keeps the body as the single source of truth too.
    assert!(
        !stored_columns(&rt, "vdocs").contains("score"),
        "versioned writes must not materialise promoted columns"
    );

    clear_current_connection_id();
}
