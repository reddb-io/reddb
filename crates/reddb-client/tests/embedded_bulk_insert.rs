#![cfg(feature = "embedded")]

//! Pins the contract that `EmbeddedClient::bulk_insert` routes
//! through the columnar `create_rows_batch_columnar` port (#110).
//!
//! `bulk_insert(N rows)` must produce a single batched
//! `BulkUpsertEntityRecords` WAL action — not N per-row records the
//! old `execute_query` loop emitted. We pin that by comparing on-disk
//! WAL byte growth: per-row inserts grow proportional to N records
//! (each carries the full collection-name + per-row framing overhead),
//! a single batch grows by one record's worth of framing plus the
//! row payloads.

use std::path::{Path, PathBuf};

use reddb_client::embedded::EmbeddedClient;
use reddb_client::JsonValue;
use reddb_server::storage::EmbeddedRdbArtifact;

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

/// Inspects the live WAL embedded in the single-file `.rdb` artifact (there is
/// no `.rdb-uwal` sidecar — the WAL lives inside the artifact). Returns
/// `(record_count, encoded_bytes)`. Must be called *before* dropping the
/// client: drop triggers a checkpoint that drains and truncates the WAL,
/// washing out exactly the per-batch-vs-per-row records we compare.
fn wal_stats(data_path: &Path) -> (usize, u64) {
    let open = EmbeddedRdbArtifact::open(data_path).expect("open rdb artifact");
    let payloads = EmbeddedRdbArtifact::read_wal_payloads(&open).expect("read wal payloads");
    let bytes = EmbeddedRdbArtifact::wal_payloads_encoded_len(&payloads).expect("wal encoded len");
    (payloads.len(), bytes)
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
    //
    // We count WAL records *before* drop — the engine's
    // `WalDurableGrouped` mode waits for durability before
    // `append_actions` returns, so the records are on disk by the time
    // `bulk_insert` returns. We avoid `close()` because it triggers a
    // checkpoint that drains and truncates the WAL, washing out exactly
    // the per-batch-vs-per-row records we want to compare. Opening a DB
    // writes a shared ~115-record catalog baseline, so we compare the
    // *delta* between the two paths, not absolute counts.
    let bulk_path = unique_db_path("bulk");
    let bulk_count = {
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
        let after = wal_stats(&bulk_path).0;
        drop(db);
        after
    };

    // Run 2: N separate `query("INSERT ...")` calls — what the old
    // `bulk_insert` loop used to do internally. Same payload set,
    // same engine config.
    let perrow_path = unique_db_path("perrow");
    let perrow_count = {
        let db = EmbeddedClient::open(perrow_path.to_path_buf()).expect("open perrow db");
        for i in 0..N {
            let sql = format!(
                "INSERT INTO users (name, age) VALUES ('user_{i}', {})",
                20 + i
            );
            db.query(&sql).expect("per-row insert");
        }
        let after = wal_stats(&perrow_path).0;
        drop(db);
        after
    };

    eprintln!(
        "WAL records for {N} rows: bulk_insert={bulk_count}, per-row={perrow_count} (delta {})",
        perrow_count.saturating_sub(bulk_count)
    );

    // The batch path emits one `BulkUpsertEntityRecords` WAL record for
    // all N rows; the per-row path emits one transaction per row. Both
    // share the catalog baseline, so the signal is the delta: per-row
    // adds ~N records, the batch adds 1. A conservative N/2 threshold
    // catches a regression where `bulk_insert` reverts to a per-row loop
    // (its count would jump by ~N and collapse this margin).
    assert!(
        perrow_count >= bulk_count + N / 2,
        "expected per-row WAL ({perrow_count} records) to dwarf batch WAL ({bulk_count}) by ~N — bulk_insert likely regressed back to a per-row loop"
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
/// `insert` call must produce a byte-identical WAL append to a 1-row
/// `bulk_insert`. If anyone re-introduces the `build_insert_sql` +
/// `execute_query` round-trip, the SQL parser path changes the WAL
/// framing and these counts/bytes diverge.
#[test]
fn insert_emits_one_wal_record_per_call() {
    // Run 1: a single `insert` of one row.
    let insert_path = unique_db_path("insert-one");
    let (insert_count, insert_bytes) = {
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
        let after = wal_stats(&insert_path);
        drop(db);
        after
    };

    // Run 2: a 1-row `bulk_insert` of the same payload — known to
    // route through `create_rows_batch_columnar` post-#110. Both runs
    // hit the same port with the same row, so the WAL must be
    // byte-identical.
    let bulk_path = unique_db_path("bulk-one");
    let (bulk_count, bulk_bytes) = {
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
        let after = wal_stats(&bulk_path);
        drop(db);
        after
    };

    eprintln!(
        "WAL for 1 row: insert=({insert_count} records, {insert_bytes} bytes), bulk_insert(1)=({bulk_count} records, {bulk_bytes} bytes)"
    );

    // The direct columnar port shares the same WAL framing as the 1-row
    // bulk path. Identical record count *and* byte length pin that
    // `insert` hits `create_rows_batch_columnar`, not the SQL
    // `execute_query` round-trip (which wraps the row in extra
    // parser/transaction framing and would shift these values).
    assert_eq!(
        insert_count, bulk_count,
        "insert WAL record count ({insert_count}) should match 1-row bulk_insert ({bulk_count})"
    );
    assert_eq!(
        insert_bytes, bulk_bytes,
        "insert WAL bytes ({insert_bytes}) should match 1-row bulk_insert ({bulk_bytes}) — insert likely regressed back to the SQL round-trip"
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
