#![cfg(feature = "embedded")]

//! Pins the contract that `EmbeddedClient::bulk_insert` routes
//! through the columnar `create_rows_batch_columnar` port (#110).
//!
//! `bulk_insert(N rows)` must produce a single batched
//! `BulkUpsertEntityRecords` WAL action — not N per-row records the
//! old `execute_query` loop emitted. Embedded single-file databases keep
//! WAL payloads inside the `.rdb` artifact, so this test reads that internal
//! WAL region directly and asserts how many payloads each operation appends.

use std::path::{Path, PathBuf};

use reddb_client::embedded::EmbeddedClient;
use reddb_client::JsonValue;
use reddb_server::storage::EmbeddedRdbArtifact;

/// Auto-cleaning DB path: holds the [`tempfile::TempDir`] guard so the temp
/// directory and the `.rdb` are removed on drop, including on panic. Derefs to
/// `&Path` for the helpers below; callers keep the binding alive for the whole
/// test.
struct TempDbPath {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl std::ops::Deref for TempDbPath {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

fn unique_db_path(label: &str) -> TempDbPath {
    let dir = tempfile::Builder::new()
        .prefix(&format!("reddb-test-bulk-{label}-"))
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join(format!("reddb-bulk-{label}.rdb"));
    TempDbPath { _dir: dir, path }
}

fn wal_payload_count(data_path: &Path) -> usize {
    let artifact = EmbeddedRdbArtifact::open(data_path).expect("open embedded rdb artifact");
    EmbeddedRdbArtifact::read_wal_payloads(&artifact)
        .expect("read embedded wal payloads")
        .len()
}

fn rows(n: usize) -> Vec<JsonValue> {
    (0..n)
        .map(|i| {
            JsonValue::object([
                ("name", JsonValue::string(format!("user_{i}"))),
                ("age", JsonValue::number(20.0 + i as f64)),
            ])
        })
        .collect()
}

#[test]
fn bulk_insert_emits_one_wal_record_per_batch() {
    const N: usize = 50;

    // Each branch uses a fresh DB path and reads the embedded WAL before
    // dropping the client, so shutdown checkpointing cannot drain the frames
    // we are asserting on.
    let bulk_path = unique_db_path("bulk");
    let bulk_payloads = {
        let db = EmbeddedClient::open(bulk_path.to_path_buf()).expect("open bulk db");
        let before = wal_payload_count(&bulk_path);
        let inserted = db.bulk_insert("users", &rows(N)).expect("bulk insert");
        assert_eq!(
            inserted.affected, N as u64,
            "bulk_insert returned wrong count"
        );
        assert_eq!(
            inserted.ids.len(),
            N,
            "bulk_insert returned wrong ids count"
        );
        let after = wal_payload_count(&bulk_path);
        drop(db);
        after - before
    };
    assert_eq!(
        bulk_payloads, 1,
        "bulk_insert({N}) should append one WAL payload"
    );

    // Run 2: N separate `query("INSERT ...")` calls — what the old
    // `bulk_insert` loop used to do internally. Same payload set,
    // same engine config.
    let perrow_path = unique_db_path("perrow");
    let perrow_payloads = {
        let db = EmbeddedClient::open(perrow_path.to_path_buf()).expect("open perrow db");
        let before = wal_payload_count(&perrow_path);
        for i in 0..N {
            let sql = format!(
                "INSERT INTO users (name, age) VALUES ('user_{i}', {})",
                20 + i
            );
            db.query(&sql).expect("per-row insert");
        }
        let after = wal_payload_count(&perrow_path);
        drop(db);
        after - before
    };
    assert_eq!(
        perrow_payloads, N,
        "per-row SQL inserts should append one WAL payload per statement"
    );
}

#[test]
fn bulk_insert_round_trip() {
    let path = unique_db_path("round-trip");
    let db = EmbeddedClient::open(path.to_path_buf()).expect("open db");

    let inserted = db
        .bulk_insert(
            "items",
            &[
                JsonValue::object([
                    ("sku", JsonValue::string("A1")),
                    ("qty", JsonValue::number(3.0)),
                ]),
                JsonValue::object([
                    ("sku", JsonValue::string("B2")),
                    ("qty", JsonValue::number(7.0)),
                ]),
                JsonValue::object([
                    ("sku", JsonValue::string("C3")),
                    ("qty", JsonValue::number(11.0)),
                ]),
            ],
        )
        .expect("bulk insert");
    assert_eq!(inserted.affected, 3);
    assert_eq!(inserted.ids.len(), 3);

    let result = db
        .query("SELECT sku, qty FROM items")
        .expect("select after bulk");
    assert_eq!(result.rows.len(), 3, "expected 3 rows back from select");

    drop(db);
}

#[test]
fn bulk_insert_heterogeneous_payloads_still_work() {
    // Mixed key-sets force the `uniform_schema` check to fail and
    // the implementation to fall back to the per-row path. Pin
    // that this still inserts every row.
    let path = unique_db_path("hetero");
    let db = EmbeddedClient::open(path.to_path_buf()).expect("open db");

    let inserted = db
        .bulk_insert(
            "events",
            &[
                JsonValue::object([
                    ("kind", JsonValue::string("login")),
                    ("user", JsonValue::string("alice")),
                ]),
                // Different key — triggers the heterogeneous fallback.
                JsonValue::object([("kind", JsonValue::string("logout"))]),
            ],
        )
        .expect("bulk insert hetero");
    assert_eq!(inserted.affected, 2);
    assert_eq!(inserted.ids.len(), 2);

    let result = db.query("SELECT kind FROM events").expect("select hetero");
    assert_eq!(result.rows.len(), 2);

    drop(db);
}

#[test]
fn bulk_insert_empty_is_noop() {
    let path = unique_db_path("empty");
    let db = EmbeddedClient::open(path.to_path_buf()).expect("open db");
    let inserted = db.bulk_insert("anything", &[]).expect("empty bulk");
    assert_eq!(inserted.affected, 0);
    assert!(inserted.ids.is_empty());
    drop(db);
}

/// Pins #111: `EmbeddedClient::insert` routes through the same
/// `create_rows_batch_columnar` port as `bulk_insert`, so a single
/// `insert` call must produce exactly one batched WAL action carrying
/// one record.
#[test]
fn insert_emits_one_wal_record_per_call() {
    // Run 1: a single `insert` of one row.
    let insert_path = unique_db_path("insert-one");
    let insert_payloads = {
        let db = EmbeddedClient::open(insert_path.to_path_buf()).expect("open insert db");
        let before = wal_payload_count(&insert_path);
        let res = db
            .insert(
                "users",
                &JsonValue::object([
                    ("name", JsonValue::string("solo".to_string())),
                    ("age", JsonValue::number(42.0)),
                ]),
            )
            .expect("single insert");
        assert_eq!(res.affected, 1, "insert returned wrong affected count");
        let after = wal_payload_count(&insert_path);
        drop(db);
        after - before
    };
    assert_eq!(insert_payloads, 1, "insert should append one WAL payload");

    // Run 2: a 1-row `bulk_insert` of the same payload — known to
    // route through `create_rows_batch_columnar` post-#110.
    let bulk_path = unique_db_path("bulk-one");
    let bulk_payloads = {
        let db = EmbeddedClient::open(bulk_path.to_path_buf()).expect("open bulk db");
        let before = wal_payload_count(&bulk_path);
        let inserted = db
            .bulk_insert(
                "users",
                &[JsonValue::object([
                    ("name", JsonValue::string("solo".to_string())),
                    ("age", JsonValue::number(42.0)),
                ])],
            )
            .expect("bulk insert one");
        assert_eq!(inserted.affected, 1);
        assert_eq!(inserted.ids.len(), 1);
        let after = wal_payload_count(&bulk_path);
        drop(db);
        after - before
    };
    assert_eq!(
        bulk_payloads, 1,
        "bulk_insert(1) should append one WAL payload"
    );
}

#[test]
fn insert_round_trip() {
    let path = unique_db_path("insert-round-trip");
    let db = EmbeddedClient::open(path.to_path_buf()).expect("open db");

    let res = db
        .insert(
            "items",
            &JsonValue::object([
                ("sku", JsonValue::string("X9".to_string())),
                ("qty", JsonValue::number(13.0)),
            ]),
        )
        .expect("single insert");
    assert_eq!(res.affected, 1);
    let rid = res.rid.expect("insert returns assigned rid");

    let result = db
        .query("SELECT rid, sku, qty FROM items")
        .expect("select after insert");
    assert_eq!(result.rows.len(), 1, "expected 1 row back from select");
    let returned_rid = result.rows[0]
        .iter()
        .find_map(|(name, value)| (name == "rid").then_some(value.to_string()))
        .expect("select returns rid");
    assert_eq!(rid, returned_rid);

    drop(db);
}
