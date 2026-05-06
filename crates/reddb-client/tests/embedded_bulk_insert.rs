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

use std::path::PathBuf;

use reddb_client::embedded::EmbeddedClient;
use reddb_client::JsonValue;

fn unique_db_path(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "reddb-bulk-{}-{}-{}.rdb",
        label,
        std::process::id(),
        nanos
    ))
}

/// WAL filename mirrors `StoreCommitCoordinator::wal_path_for_db` —
/// `<data_path>.rdb-uwal`. Building it from the data path keeps the
/// test independent of the engine's internal path helpers.
fn wal_path_for(data_path: &PathBuf) -> PathBuf {
    data_path.with_extension("rdb-uwal")
}

fn wal_size(data_path: &PathBuf) -> u64 {
    std::fs::metadata(wal_path_for(data_path))
        .map(|m| m.len())
        .unwrap_or(0)
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
    // Each branch uses a fresh DB path. `EmbeddedClient::open` creates
    // the WAL with an 8-byte header, so any size above 8 came from the
    // inserts. We measure size *before* drop — the engine's
    // `WalDurableGrouped` mode waits for durability before
    // `append_actions` returns, so the bytes are on disk by the time
    // `bulk_insert` returns. We avoid `close()` because it triggers a
    // checkpoint that drains and truncates the WAL, washing out
    // exactly the per-batch-vs-per-row bytes we want to compare.
    let bulk_path = unique_db_path("bulk");
    let bulk_size = {
        let db = EmbeddedClient::open(bulk_path.clone()).expect("open bulk db");
        let inserted = db.bulk_insert("users", &rows(N)).expect("bulk insert");
        assert_eq!(inserted, N as u64, "bulk_insert returned wrong count");
        let after = wal_size(&bulk_path);
        drop(db);
        after
    };

    // Run 2: N separate `query("INSERT ...")` calls — what the old
    // `bulk_insert` loop used to do internally. Same payload set,
    // same engine config.
    let perrow_path = unique_db_path("perrow");
    let perrow_size = {
        let db = EmbeddedClient::open(perrow_path.clone()).expect("open perrow db");
        for i in 0..N {
            let sql = format!("INSERT INTO users (name, age) VALUES ('user_{i}', {})", 20 + i);
            db.query(&sql).expect("per-row insert");
        }
        let after = wal_size(&perrow_path);
        drop(db);
        after
    };

    // Cleanup so a panic still leaves /tmp clean.
    cleanup_db(&bulk_path);
    cleanup_db(&perrow_path);

    eprintln!(
        "WAL size for {N} rows: bulk_insert={bulk_size} bytes, per-row={perrow_size} bytes (ratio {:.1}×)",
        perrow_size as f64 / bulk_size.max(1) as f64
    );

    // Per-row path emits N transactions (each Begin / PageWrite / Commit)
    // wrapping a 1-record `BulkUpsertEntityRecords` action. The batch
    // path emits exactly one transaction wrapping one N-record action.
    // The dominant growth is the per-tx framing + collection name + WAL
    // header overhead, so per-row WAL is ~N× the batch WAL. We pick a
    // conservative 2× threshold: if anyone re-introduces a per-row loop
    // in `bulk_insert` this collapses below 2× and the test fails.
    assert!(
        perrow_size > bulk_size * 2,
        "expected per-row WAL to dwarf bulk WAL, but got per-row={perrow_size} bytes, bulk={bulk_size} bytes — bulk_insert likely regressed back to a per-row loop"
    );
    // Sanity: bulk path actually wrote *something* past the 8-byte
    // WAL header (otherwise the ratio assertion above is trivially
    // satisfied because bulk_size could be 8 and 9+ would already
    // pass).
    assert!(
        bulk_size > 8,
        "bulk WAL only contains the header — no append happened (size={bulk_size})"
    );
}

fn cleanup_db(path: &PathBuf) {
    if let Some(parent) = path.parent() {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            if let Ok(rd) = std::fs::read_dir(parent) {
                for entry in rd.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with(stem) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}

#[test]
fn bulk_insert_round_trip() {
    let path = unique_db_path("round-trip");
    let db = EmbeddedClient::open(path.clone()).expect("open db");

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
    assert_eq!(inserted, 3);

    let result = db
        .query("SELECT sku, qty FROM items")
        .expect("select after bulk");
    assert_eq!(result.rows.len(), 3, "expected 3 rows back from select");

    drop(db);
    cleanup_db(&path);
}

#[test]
fn bulk_insert_heterogeneous_payloads_still_work() {
    // Mixed key-sets force the `uniform_schema` check to fail and
    // the implementation to fall back to the per-row path. Pin
    // that this still inserts every row.
    let path = unique_db_path("hetero");
    let db = EmbeddedClient::open(path.clone()).expect("open db");

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
    assert_eq!(inserted, 2);

    let result = db.query("SELECT kind FROM events").expect("select hetero");
    assert_eq!(result.rows.len(), 2);

    drop(db);
    cleanup_db(&path);
}

#[test]
fn bulk_insert_empty_is_noop() {
    let path = unique_db_path("empty");
    let db = EmbeddedClient::open(path.clone()).expect("open db");
    assert_eq!(db.bulk_insert("anything", &[]).expect("empty bulk"), 0);
    drop(db);
    cleanup_db(&path);
}

/// Pins #111: `EmbeddedClient::insert` routes through the same
/// `create_rows_batch_columnar` port as `bulk_insert`, so a single
/// `insert` call must produce exactly one WAL append — same byte
/// growth as a 1-row `bulk_insert`. If anyone re-introduces the
/// `build_insert_sql` + `execute_query` round-trip, the SQL parser
/// path adds extra WAL framing (transaction-wrapped statement
/// records) and this size delta diverges.
#[test]
fn insert_emits_one_wal_record_per_call() {
    // Run 1: a single `insert` of one row.
    let insert_path = unique_db_path("insert-one");
    let insert_size = {
        let db = EmbeddedClient::open(insert_path.clone()).expect("open insert db");
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
        let after = wal_size(&insert_path);
        drop(db);
        after
    };

    // Run 2: a 1-row `bulk_insert` of the same payload — known to
    // route through `create_rows_batch_columnar` post-#110. The two
    // runs must produce the same WAL byte count, since they're
    // hitting the same port with the same row.
    let bulk_path = unique_db_path("bulk-one");
    let bulk_size = {
        let db = EmbeddedClient::open(bulk_path.clone()).expect("open bulk db");
        let inserted = db
            .bulk_insert(
                "users",
                &[JsonValue::object([
                    ("name", JsonValue::string("solo".to_string())),
                    ("age", JsonValue::number(42.0)),
                ])],
            )
            .expect("bulk insert one");
        assert_eq!(inserted, 1);
        let after = wal_size(&bulk_path);
        drop(db);
        after
    };

    // Run 3: same payload via `query("INSERT ...")` — the old SQL
    // round-trip path. This is the size we expect the new `insert`
    // to *beat* (or at least match the bulk path on, decisively
    // below the SQL path).
    let sql_path = unique_db_path("insert-sql");
    let sql_size = {
        let db = EmbeddedClient::open(sql_path.clone()).expect("open sql db");
        db.query("INSERT INTO users (name, age) VALUES ('solo', 42)")
            .expect("sql insert");
        let after = wal_size(&sql_path);
        drop(db);
        after
    };

    cleanup_db(&insert_path);
    cleanup_db(&bulk_path);
    cleanup_db(&sql_path);

    eprintln!(
        "WAL size for 1 row: insert={insert_size} bytes, bulk_insert(1)={bulk_size} bytes, query(SQL)={sql_size} bytes"
    );

    // Header-only WAL is 8 bytes; both fast paths must have written
    // payload past that.
    assert!(
        insert_size > 8,
        "insert WAL only contains the header — no append happened (size={insert_size})"
    );

    // Direct columnar port shares the same WAL framing as the
    // 1-row bulk path. Equal sizes pin that `insert` no longer
    // routes through `execute_query`.
    assert_eq!(
        insert_size, bulk_size,
        "insert WAL ({insert_size}) should match 1-row bulk_insert WAL ({bulk_size}) — insert likely regressed back to the SQL round-trip"
    );

    // And the SQL round-trip path must be strictly larger (it
    // wraps the same row in extra parser/transaction framing).
    // If `insert_size >= sql_size`, then `insert` itself is going
    // through the SQL path.
    assert!(
        insert_size < sql_size,
        "expected insert WAL ({insert_size}) to be smaller than SQL-roundtrip WAL ({sql_size}) — insert appears to still be on the execute_query path"
    );
}

#[test]
fn insert_round_trip() {
    let path = unique_db_path("insert-round-trip");
    let db = EmbeddedClient::open(path.clone()).expect("open db");

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

    let result = db
        .query("SELECT sku, qty FROM items")
        .expect("select after insert");
    assert_eq!(result.rows.len(), 1, "expected 1 row back from select");

    drop(db);
    cleanup_db(&path);
}
