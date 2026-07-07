//! #861 — columnar migration read-bridge, end-to-end.
//!
//! The migration/compat path under the chunk model: when columnar projection is
//! enabled on a collection that ALREADY holds row-stored data, the
//! pre-existing row chunks keep serving through the row path and only NEW
//! chunks seal columnar (`RDCC`). The two coexist in one collection,
//! disambiguated by format version (`ChunkMeta.format()` /
//! `ChunkMeta.columnar_page`), and a read bridges across the boundary so
//! no prior data is lost or stranded — with no mass rewrite.
//!
//! This pins the three acceptance criteria:
//!   1. After enabling columnar, all prior row-stored data stays readable.
//!   2. New chunks seal columnar; old row + new columnar coexist,
//!      disambiguated by format version.
//!   3. No data is lost or stranded across the upgrade.

use reddb_server::catalog::AnalyticalStorageConfig;
use reddb_server::storage::schema::Value;
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

/// 1-hour chunk interval, in nanoseconds — matches `CHUNK_INTERVAL '1h'`.
const HOUR_NS: u64 = 3_600 * 1_000_000_000;
const GRANULE_ROWS_PLUS_ONE: u64 = 8_193;

/// Pre-existing (row-stored) data: lands wholly in the chunk starting at 0.
const OLD_ROW: &[(u64, i64)] = &[
    (1_000_000_000, 10),
    (2_000_000_000, 20),
    (3_000_000_000, 30),
];

/// Post-upgrade data: lands in the SECOND chunk (`[1h, 2h)`), sealed
/// columnar after columnar projection is turned on. Four rows crosses
/// the automatic projection size floor.
const NEW_PROJECTED: &[(u64, i64)] = &[
    (HOUR_NS + 1_000_000_000, 40),
    (HOUR_NS + 2_000_000_000, 50),
    (HOUR_NS + 3_000_000_000, 60),
    (HOUR_NS + 4_000_000_000, 70),
];

fn insert(rt: &RedDBRuntime, collection: &str, rows: &[(u64, i64)]) {
    for (ts, value) in rows {
        rt.execute_query(&format!(
            "INSERT INTO {collection} (ts, value) VALUES ({ts}, {value})"
        ))
        .unwrap_or_else(|e| panic!("insert ({ts}, {value}): {e}"));
    }
}

fn insert_many(rt: &RedDBRuntime, collection: &str, row_count: u64) {
    for batch_start in (0..row_count).step_by(500) {
        let batch_end = (batch_start + 500).min(row_count);
        let values = (batch_start..batch_end)
            .map(|i| format!("({}, {})", i + 1, i as i64))
            .collect::<Vec<_>>()
            .join(", ");
        rt.execute_query(&format!(
            "INSERT INTO {collection} (ts, value) VALUES {values}"
        ))
        .unwrap_or_else(|e| panic!("insert batch {batch_start}..{batch_end}: {e}"));
    }
}

fn insert_chunk_rows(
    rt: &RedDBRuntime,
    collection: &str,
    chunk_index: u64,
    row_count: u64,
) -> Vec<(u64, i64)> {
    let start = chunk_index * HOUR_NS;
    let rows = (0..row_count)
        .map(|i| (start + i + 1, (chunk_index * 100 + i) as i64))
        .collect::<Vec<_>>();
    insert(rt, collection, &rows);
    rows
}

fn open_paged_runtime(path: &std::path::Path) -> RedDBRuntime {
    let options = RedDBOptions::persistent(path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("serverless profile");
    RedDBRuntime::with_options(options).expect("persistent runtime boots")
}

/// Flip an existing collection's contract to columnar projection.
fn enable_columnar(rt: &RedDBRuntime, collection: &str, time_key: &str) {
    let db = rt.db();
    let mut contract = db
        .collection_contract(collection)
        .expect("collection has a contract");
    contract.analytical_storage = Some(AnalyticalStorageConfig {
        columnar: true,
        time_key: time_key.to_string(),
        order_by_key: None,
    });
    db.save_collection_contract(contract)
        .expect("save columnar contract");
}

fn want(rows: &[(u64, i64)]) -> Vec<(u64, f64)> {
    rows.iter().map(|(ts, v)| (*ts, *v as f64)).collect()
}

fn assert_recent_sql_rows(rt: &RedDBRuntime, collection: &str, start_ns: u64, want_rows: usize) {
    let result = rt
        .execute_query(&format!(
            "SELECT ts, value FROM {collection} WHERE ts >= {start_ns} ORDER BY ts ASC"
        ))
        .expect("recent row query");
    assert_eq!(result.result.records.len(), want_rows);
    if let Some(row) = result.result.records.first() {
        let ts = match row.get("ts") {
            Some(Value::Integer(n)) if *n >= 0 => *n as u64,
            other => panic!("unexpected ts value: {other:?}"),
        };
        assert!(ts >= start_ns);
    }
}

#[test]
fn read_bridge_serves_row_and_columnar_chunks_after_enabling_columnar() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");

    // --- Before the upgrade: an opted-out hypertable with row-stored data ---
    rt.execute_query("CREATE HYPERTABLE cpu TIME_COLUMN ts CHUNK_INTERVAL '1h' NO COLUMNAR")
        .expect("create hypertable");
    insert(&rt, "cpu", OLD_ROW);

    // Seal the existing chunk row-oriented (the collection opted out).
    assert_eq!(
        rt.seal_hypertable_chunks("cpu").expect("seal row"),
        0,
        "the opt-out keeps existing chunks row-stored"
    );
    assert_eq!(
        rt.columnar_chunk_count("cpu"),
        0,
        "pre-upgrade chunk is row-stored"
    );

    // --- Enable projection on the collection that already holds data ---
    enable_columnar(&rt, "cpu", "ts");

    // New data lands in a NEW chunk and seals columnar.
    insert(&rt, "cpu", NEW_PROJECTED);
    assert_eq!(
        rt.seal_hypertable_chunks("cpu").expect("seal columnar"),
        1,
        "the new chunk seals columnar; the pre-existing row chunk is skipped"
    );

    // Criterion 2: old row + new columnar coexist, disambiguated by format.
    let chunks = rt.db().hypertables().show_chunks("cpu");
    assert_eq!(chunks.len(), 2, "two chunks: one row, one columnar");
    let row_chunks = chunks.iter().filter(|c| !c.is_columnar()).count();
    let col_chunks = chunks.iter().filter(|c| c.is_columnar()).count();
    assert_eq!(row_chunks, 1, "pre-existing chunk stays row-stored");
    assert_eq!(col_chunks, 1, "new chunk is columnar (RDCC)");
    assert_eq!(rt.columnar_chunk_count("cpu"), 1);

    // No rewrite: the old chunk (start 0) carries no columnar_page.
    let old = chunks.iter().find(|c| c.id.start_ns == 0).unwrap();
    assert!(
        old.columnar_page.is_none(),
        "enabling columnar must NOT rewrite the pre-existing row chunk"
    );

    // Criterion 1 + 3: the read-bridge returns ALL data, both formats,
    // merged in timestamp order — nothing lost or stranded.
    let all = rt
        .read_bridge_points("cpu", 0, u64::MAX)
        .expect("read bridge");
    let mut expected = want(OLD_ROW);
    expected.extend(want(NEW_PROJECTED));
    assert_eq!(
        all, expected,
        "every prior + new point is readable, in order"
    );
}

#[test]
fn read_bridge_prunes_chunks_outside_the_query_range() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("CREATE HYPERTABLE cpu TIME_COLUMN ts CHUNK_INTERVAL '1h' NO COLUMNAR")
        .expect("create hypertable");

    insert(&rt, "cpu", OLD_ROW);
    rt.seal_hypertable_chunks("cpu").expect("seal row");
    enable_columnar(&rt, "cpu", "ts");
    insert(&rt, "cpu", NEW_PROJECTED);
    rt.seal_hypertable_chunks("cpu").expect("seal columnar");

    // A window covering only the first (row) chunk returns only its points.
    let only_row = rt
        .read_bridge_points("cpu", 0, HOUR_NS - 1)
        .expect("read bridge");
    assert_eq!(only_row, want(OLD_ROW), "row chunk served alone");

    // A window covering only the second (columnar) chunk returns only its
    // points — proving the columnar reader is exercised in isolation too.
    let only_col = rt
        .read_bridge_points("cpu", HOUR_NS, u64::MAX)
        .expect("read bridge");
    assert_eq!(only_col, want(NEW_PROJECTED), "columnar chunk served alone");
}

#[test]
fn durable_columnar_blocks_survive_restart_and_keep_pruning() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("columnar.rdb");

    let before_range;
    let before_eq;
    {
        let rt = open_paged_runtime(&path);
        rt.execute_query("CREATE HYPERTABLE cpu TIME_COLUMN ts CHUNK_INTERVAL '1h'")
            .expect("create hypertable");
        insert_many(&rt, "cpu", GRANULE_ROWS_PLUS_ONE);
        insert(&rt, "cpu", &[(HOUR_NS + 1, 99)]);

        rt.checkpoint().expect("checkpoint seals closed chunk");
        assert_eq!(rt.columnar_chunk_count("cpu"), 1);
        let chunks = rt.db().hypertables().show_chunks("cpu");
        assert_eq!(chunks.len(), 2, "closed chunk plus open row tail");
        let chunk = chunks
            .iter()
            .find(|chunk| chunk.id.start_ns == 0)
            .expect("closed chunk")
            .clone();
        assert!(
            chunk.columnar_page.is_some_and(|loc| loc.page_id != 0),
            "columnar seal must record a real engine page location"
        );
        let tail = chunks
            .iter()
            .find(|chunk| chunk.id.start_ns == HOUR_NS)
            .expect("tail chunk");
        assert!(
            !tail.sealed && tail.columnar_page.is_none(),
            "checkpoint must leave the newest chunk on the row tail"
        );
        let metadata = rt.db().physical_metadata().expect("physical metadata");
        let persisted_chunk = metadata.hypertables[0]
            .chunks
            .iter()
            .find(|chunk| chunk.start_ns == 0)
            .expect("persisted closed chunk");
        assert!(
            persisted_chunk.columnar_derived,
            "persisted metadata must mark columnar blocks derived"
        );

        before_range = rt
            .columnar_chunk_range_scan("cpu", 0, 100, 120)
            .expect("pre-restart range scan");
        assert!(
            before_range.granules_scanned < before_range.granules_total,
            "selective range scan should prune granules before restart"
        );
        before_eq = rt
            .columnar_chunk_value_eq_scan("cpu", 0, 42.0)
            .expect("pre-restart equality scan");

        assert_recent_sql_rows(&rt, "cpu", HOUR_NS, 1);
    }

    let rt = open_paged_runtime(&path);
    let after_range = rt
        .columnar_chunk_range_scan("cpu", 0, 100, 120)
        .expect("post-restart range scan");
    assert_eq!(after_range.points, before_range.points);
    assert_eq!(after_range.granules_total, before_range.granules_total);
    assert_eq!(after_range.granules_scanned, before_range.granules_scanned);
    assert!(
        after_range.granules_scanned < after_range.granules_total,
        "durable range path should keep granule pruning observable"
    );

    let after_eq = rt
        .columnar_chunk_value_eq_scan("cpu", 0, 42.0)
        .expect("post-restart equality scan");
    assert_eq!(after_eq.points, before_eq.points);
    assert_eq!(after_eq.granules_total, before_eq.granules_total);
    assert_eq!(after_eq.granules_scanned, before_eq.granules_scanned);
}

#[test]
fn checkpoint_columnar_budget_defers_closed_chunks_and_keeps_row_tail_fresh() {
    let options = RedDBOptions::in_memory().with_checkpoint_columnar_emission_budget_chunks(1);
    let rt = RedDBRuntime::with_options(options).expect("runtime boots");
    rt.execute_query("CREATE HYPERTABLE cpu TIME_COLUMN ts CHUNK_INTERVAL '1h'")
        .expect("create hypertable");

    let mut expected = Vec::new();
    for chunk_index in 0..4 {
        expected.extend(insert_chunk_rows(&rt, "cpu", chunk_index, 4));
    }
    expected.extend(insert_chunk_rows(&rt, "cpu", 4, 1));

    rt.checkpoint().expect("first checkpoint");
    assert_eq!(
        rt.columnar_chunk_count("cpu"),
        1,
        "budget allows one closed chunk per checkpoint"
    );
    assert_eq!(
        rt.read_bridge_points("cpu", 0, u64::MAX).unwrap(),
        want(&expected)
    );

    rt.checkpoint().expect("second checkpoint");
    assert_eq!(
        rt.columnar_chunk_count("cpu"),
        2,
        "deferred chunks are picked up on the next checkpoint"
    );

    rt.checkpoint().expect("third checkpoint");
    rt.checkpoint().expect("fourth checkpoint");
    assert_eq!(
        rt.columnar_chunk_count("cpu"),
        4,
        "all closed chunks eventually seal"
    );

    let tail = rt
        .db()
        .hypertables()
        .show_chunks("cpu")
        .into_iter()
        .find(|chunk| chunk.id.start_ns == 4 * HOUR_NS)
        .expect("open tail chunk");
    assert!(
        !tail.sealed && tail.columnar_page.is_none(),
        "the newest chunk remains row-backed"
    );
    assert_recent_sql_rows(&rt, "cpu", 4 * HOUR_NS, 1);
}
