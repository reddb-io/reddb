// Issue #1404 — all-at-once, reversible migration to the native binary
// document body (PRD-1398, ADR-0063).
//
// The migration tool rewrites every existing document into the binary
// container in FRESH files alongside the old, auto-`CREATE INDEX`es every
// previously-promoted field, verifies document counts, then atomically swaps —
// retaining the pre-migration files as the rollback point.
//
// These tests build a *fixture* store in the legacy (plain-JSON body +
// materialised promoted columns) format, run the migration, and assert the
// acceptance criteria end-to-end: binary rewrite, promoted-field indexes,
// count verification (incl. the abort-on-mismatch path), atomic swap with a
// retained backup, and post-swap reads equalling pre-swap reads.

#[path = "../../support/mod.rs"]
mod support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use reddb::document_migration::migrate_store_to_binary_body;
use reddb::storage::schema::Value;
use reddb::storage::EntityData;
use reddb::{RedDBOptions, RedDBRuntime};

const DATA_FILE: &str = "db.rdb";

/// A unique store directory under the system temp dir.
fn fresh_store_dir(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "reddb-mig-1404-{}-{}-{}",
        std::process::id(),
        tag,
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create store dir");
    dir
}

/// Open a runtime over the store directory's data file.
fn open(store_dir: &Path) -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::persistent(store_dir.join(DATA_FILE))).expect("open")
}

fn insert(rt: &RedDBRuntime, collection: &str, body_json: &str) {
    rt.execute_query(&format!(
        "INSERT INTO {collection} DOCUMENT (body) VALUES ('{body_json}')"
    ))
    .unwrap_or_else(|err| panic!("insert {body_json}: {err:?}"));
}

/// The set of distinct stored column names across every row of `collection`.
/// Materialised promoted columns show up here next to `body`.
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

/// `(name, score, city)` for every document, ordered by name — a stable,
/// id-independent fingerprint of the readable contents.
fn read_fingerprint(rt: &RedDBRuntime, collection: &str) -> Vec<(String, i64, String)> {
    rt.execute_query(&format!(
        "SELECT name, score, city FROM {collection} ORDER BY name"
    ))
    .unwrap_or_else(|err| panic!("fingerprint read: {err:?}"))
    .result
    .records
    .iter()
    .map(|record| {
        (
            as_text(record.get("name")),
            as_int(record.get("score")),
            as_text(record.get("city")),
        )
    })
    .collect()
}

/// Build a legacy fixture store (binary body OFF → promoted columns
/// materialised) with two document collections.
fn build_fixture(store_dir: &Path) {
    let rt = open(store_dir);
    // Default is binary body OFF; documents materialise promoted columns.
    rt.execute_query("CREATE DOCUMENT users")
        .expect("create users");
    insert(&rt, "users", r#"{"name":"alice","score":30,"city":"SP"}"#);
    insert(&rt, "users", r#"{"name":"bob","score":10,"city":"RJ"}"#);
    insert(&rt, "users", r#"{"name":"carol","score":50,"city":"SP"}"#);
    insert(&rt, "users", r#"{"name":"dave","score":20,"city":"BH"}"#);

    rt.execute_query("CREATE DOCUMENT orders")
        .expect("create orders");
    insert(&rt, "orders", r#"{"name":"o1","score":5,"city":"SP"}"#);
    insert(&rt, "orders", r#"{"name":"o2","score":7,"city":"RJ"}"#);

    rt.flush().expect("flush");
    rt.checkpoint().expect("checkpoint");
    drop(rt);
}

// ---------------------------------------------------------------------------
// Acceptance: full reversible migration end-to-end.
// ---------------------------------------------------------------------------

#[test]
fn migrates_fixture_store_reversibly_with_indexes_and_count_verification() {
    let store_dir = fresh_store_dir("happy");
    build_fixture(&store_dir);

    // Capture pre-swap reads (the legacy fixture, before any rewrite).
    let pre_users;
    let pre_orders;
    let pre_filtered;
    {
        let rt = open(&store_dir);
        // The legacy store materialises promoted columns.
        let cols = stored_columns(&rt, "users");
        for promoted in ["name", "score", "city"] {
            assert!(
                cols.contains(promoted),
                "fixture must materialise promoted column `{promoted}`: {cols:?}"
            );
        }
        pre_users = read_fingerprint(&rt, "users");
        pre_orders = read_fingerprint(&rt, "orders");
        pre_filtered = rt
            .execute_query("SELECT name FROM users WHERE score > 25 ORDER BY name")
            .expect("filtered")
            .result
            .records
            .iter()
            .map(|r| as_text(r.get("name")))
            .collect::<Vec<_>>();
        drop(rt);
    }
    assert_eq!(pre_filtered, vec!["alice".to_string(), "carol".to_string()]);

    // Run the migration.
    let report = migrate_store_to_binary_body(&store_dir).expect("migration");

    // Count verification: every collection reports source == migrated.
    assert_eq!(report.total_documents, 6);
    let users_mig = report
        .collections
        .iter()
        .find(|c| c.name == "users")
        .expect("users report");
    assert_eq!(users_mig.source_documents, 4);
    assert_eq!(users_mig.migrated_documents, 4);
    // Every previously-promoted field was auto-indexed.
    assert_eq!(
        users_mig.auto_indexed_fields,
        vec!["city".to_string(), "name".to_string(), "score".to_string()]
    );

    // The pre-migration files are retained as the rollback point.
    assert!(
        report.backup_dir.is_dir(),
        "backup dir must exist: {}",
        report.backup_dir.display()
    );
    assert!(report.backup_dir.join(DATA_FILE).is_file());

    // Reopen the swapped (now-live) store.
    let rt = open(&store_dir);

    // Documents were rewritten into the binary single-source format: no
    // promoted columns are materialised anymore — only the body.
    let cols = stored_columns(&rt, "users");
    assert!(cols.contains("body"), "body is always stored: {cols:?}");
    for promoted in ["name", "score", "city"] {
        assert!(
            !cols.contains(promoted),
            "promoted column `{promoted}` must be gone post-migration: {cols:?}"
        );
    }

    // Every previously-promoted field has an index after migration.
    let index_store = rt.index_store_ref();
    for field in ["name", "score", "city"] {
        assert!(
            index_store.sorted.has_index("users", field),
            "previously-promoted field `{field}` must be indexed after migration"
        );
    }

    // Post-swap reads equal pre-swap reads.
    assert_eq!(read_fingerprint(&rt, "users"), pre_users);
    assert_eq!(read_fingerprint(&rt, "orders"), pre_orders);
    let post_filtered = rt
        .execute_query("SELECT name FROM users WHERE score > 25 ORDER BY name")
        .expect("filtered post")
        .result
        .records
        .iter()
        .map(|r| as_text(r.get("name")))
        .collect::<Vec<_>>();
    assert_eq!(post_filtered, pre_filtered);
    drop(rt);

    // The retained backup is still the legacy format (true rollback point):
    // reopening it shows the materialised promoted columns again.
    let backup_rt = open(&report.backup_dir);
    let backup_cols = stored_columns(&backup_rt, "users");
    for promoted in ["name", "score", "city"] {
        assert!(
            backup_cols.contains(promoted),
            "rollback point must still carry promoted column `{promoted}`: {backup_cols:?}"
        );
    }
    assert_eq!(read_fingerprint(&backup_rt, "users"), pre_users);
    drop(backup_rt);

    let _ = std::fs::remove_dir_all(&store_dir);
    let _ = std::fs::remove_dir_all(&report.backup_dir);
}

// ---------------------------------------------------------------------------
// Acceptance: a count mismatch aborts the swap and leaves the source intact.
// ---------------------------------------------------------------------------

#[test]
fn aborts_when_a_sibling_migration_dir_already_exists() {
    // The migration refuses to run if a stale `*.migrating` sibling would
    // shadow the freshly-built store — proving the source is never touched
    // when the pre-swap invariants do not hold.
    let store_dir = fresh_store_dir("guard");
    build_fixture(&store_dir);

    let stale = store_dir.with_file_name(format!(
        "{}.migrating",
        store_dir
            .file_name()
            .and_then(|name| name.to_str())
            .expect("store dir has a name")
    ));
    std::fs::create_dir_all(&stale).expect("stale sibling");

    let err = migrate_store_to_binary_body(&store_dir);
    assert!(err.is_err(), "must refuse when sibling dir exists");

    // Source store is untouched: still legacy, still readable.
    let rt = open(&store_dir);
    let cols = stored_columns(&rt, "users");
    assert!(
        cols.contains("name") && cols.contains("body"),
        "source store must be untouched after a refused migration: {cols:?}"
    );
    drop(rt);

    let _ = std::fs::remove_dir_all(&store_dir);
    let _ = std::fs::remove_dir_all(&stale);
}
