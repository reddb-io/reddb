#![cfg(feature = "embedded")]

//! Pins the contract that `EmbeddedClient::bulk_insert` routes
//! through the columnar `create_rows_batch_columnar` port (#110).
//!
//! `bulk_insert(N rows)` must produce a single batched
//! `BulkUpsertEntityRecords` write — not N per-row writes the old
//! `execute_query` loop emitted. We pin that through the SQL stats
//! surface: before a checkpoint, per-row inserts create many more
//! pending embedded WAL records than the batched path.

use std::path::{Path, PathBuf};

use reddb_client::embedded::EmbeddedClient;
use reddb_client::{JsonValue, ValueOut};

/// Auto-cleaning DB path: holds the [`tempfile::TempDir`] guard so the temp
/// directory and the `.rdb` artifact are removed on drop, including on panic.
/// Derefs to `&Path` for the helpers below; callers keep the binding alive for
/// the whole test.
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

fn stats_metric(db: &EmbeddedClient, collection: &str, metric: &str) -> u64 {
    let result = db
        .query(&format!(
            "SELECT value FROM red.stats WHERE collection = '{collection}' \
             AND metric = '{metric}'"
        ))
        .expect("stats query");
    let row = result.rows.first().expect("stats row");
    let value = row
        .iter()
        .find(|(name, _)| name == "value")
        .map(|(_, value)| value)
        .expect("stats value");
    match value {
        ValueOut::Integer(value) if *value >= 0 => *value as u64,
        other => panic!("expected unsigned {metric}, got {other:?}"),
    }
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

    // Run 1: single bulk_insert(N).
    let bulk_path = unique_db_path("bulk");
    let bulk_lag = {
        let db = EmbeddedClient::open(bulk_path.to_path_buf()).expect("open bulk db");
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
        let after = stats_metric(&db, "users", "pending_wal_records");
        drop(db);
        after
    };

    // Run 2: N separate `query("INSERT ...")` calls — what the old
    // `bulk_insert` loop used to do internally. Same payload set,
    // same engine config.
    let perrow_path = unique_db_path("perrow");
    let perrow_lag = {
        let db = EmbeddedClient::open(perrow_path.to_path_buf()).expect("open perrow db");
        for i in 0..N {
            let sql = format!(
                "INSERT INTO users (name, age) VALUES ('user_{i}', {})",
                20 + i
            );
            db.query(&sql).expect("per-row insert");
        }
        let after = stats_metric(&db, "users", "pending_wal_records");
        drop(db);
        after
    };

    eprintln!(
        "pending WAL records for {N} rows: bulk_insert={bulk_lag}, per-row={perrow_lag} (delta {})",
        perrow_lag.saturating_sub(bulk_lag)
    );

    // A conservative N/2 threshold catches a regression where
    // `bulk_insert` reverts to a per-row loop: pending WAL records
    // would jump by ~N and collapse this margin.
    assert!(
        perrow_lag >= bulk_lag + N as u64 / 2,
        "expected per-row pending WAL records ({perrow_lag}) to dwarf batch pending WAL records ({bulk_lag}) by ~N; bulk_insert likely regressed back to a per-row loop"
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
/// `insert` call should leave pending WAL records exactly like a 1-row
/// `bulk_insert`. If anyone re-introduces the `build_insert_sql` +
/// `execute_query` round-trip, the SQL parser path changes the write
/// shape and these stats diverge.
#[test]
fn insert_and_one_row_bulk_insert_advance_projection_lag_equally() {
    // Run 1: a single `insert` of one row.
    let insert_path = unique_db_path("insert-one");
    let insert_lag = {
        let db = EmbeddedClient::open(insert_path.to_path_buf()).expect("open insert db");
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
        let after = stats_metric(&db, "users", "pending_wal_records");
        drop(db);
        after
    };

    // Run 2: a 1-row `bulk_insert` of the same payload — known to
    // route through `create_rows_batch_columnar` post-#110. Both runs
    // hit the same port with the same row, so the stats should match.
    let bulk_path = unique_db_path("bulk-one");
    let bulk_lag = {
        let db = EmbeddedClient::open(bulk_path.to_path_buf()).expect("open bulk db");
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
        let after = stats_metric(&db, "users", "pending_wal_records");
        drop(db);
        after
    };

    eprintln!("pending WAL records for 1 row: insert={insert_lag}, bulk_insert(1)={bulk_lag}");

    // The direct columnar port shares the same write shape as the 1-row
    // bulk path. Identical lag pins that `insert` hits
    // `create_rows_batch_columnar`, not the SQL `execute_query`
    // round-trip.
    assert_eq!(
        insert_lag, bulk_lag,
        "insert pending WAL records ({insert_lag}) should match 1-row bulk_insert ({bulk_lag})"
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
