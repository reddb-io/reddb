//! #861 — columnar migration read-bridge, end-to-end.
//!
//! The migration/compat path under the chunk model: when `COLUMNAR` is
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
use reddb_server::{RedDBOptions, RedDBRuntime};

/// 1-hour chunk interval, in nanoseconds — matches `CHUNK_INTERVAL '1h'`.
const HOUR_NS: u64 = 3_600 * 1_000_000_000;

/// Pre-existing (row-stored) data: lands wholly in the chunk starting at 0.
const OLD_ROW: &[(u64, i64)] = &[
    (1_000_000_000, 10),
    (2_000_000_000, 20),
    (3_000_000_000, 30),
];

/// Post-upgrade data: lands in the SECOND chunk (`[1h, 2h)`), sealed
/// columnar after `COLUMNAR` is turned on.
const NEW_COLUMNAR: &[(u64, i64)] = &[(HOUR_NS + 1_000_000_000, 40), (HOUR_NS + 2_000_000_000, 50)];

fn insert(rt: &RedDBRuntime, collection: &str, rows: &[(u64, i64)]) {
    for (ts, value) in rows {
        rt.execute_query(&format!(
            "INSERT INTO {collection} (ts, value) VALUES ({ts}, {value})"
        ))
        .unwrap_or_else(|e| panic!("insert ({ts}, {value}): {e}"));
    }
}

/// Flip an existing collection's contract to columnar — the "enable
/// COLUMNAR on a collection that already has data" operator action.
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
        .expect("save columnar-enabled contract");
}

fn want(rows: &[(u64, i64)]) -> Vec<(u64, f64)> {
    rows.iter().map(|(ts, v)| (*ts, *v as f64)).collect()
}

#[test]
fn read_bridge_serves_row_and_columnar_chunks_after_enabling_columnar() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");

    // --- Before the upgrade: a plain (non-columnar) hypertable with data ---
    rt.execute_query("CREATE HYPERTABLE cpu TIME_COLUMN ts CHUNK_INTERVAL '1h'")
        .expect("create hypertable");
    insert(&rt, "cpu", OLD_ROW);

    // Seal the existing chunk row-oriented (no columnar flag yet).
    assert_eq!(
        rt.seal_hypertable_chunks("cpu").expect("seal row"),
        0,
        "without the flag nothing seals columnar"
    );
    assert_eq!(
        rt.columnar_chunk_count("cpu"),
        0,
        "pre-upgrade chunk is row-stored"
    );

    // --- Enable COLUMNAR on the collection that already holds data ---
    enable_columnar(&rt, "cpu", "ts");

    // New data lands in a NEW chunk and seals columnar.
    insert(&rt, "cpu", NEW_COLUMNAR);
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
    expected.extend(want(NEW_COLUMNAR));
    assert_eq!(
        all, expected,
        "every prior + new point is readable, in order"
    );
}

#[test]
fn read_bridge_prunes_chunks_outside_the_query_range() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("CREATE HYPERTABLE cpu TIME_COLUMN ts CHUNK_INTERVAL '1h'")
        .expect("create hypertable");

    insert(&rt, "cpu", OLD_ROW);
    rt.seal_hypertable_chunks("cpu").expect("seal row");
    enable_columnar(&rt, "cpu", "ts");
    insert(&rt, "cpu", NEW_COLUMNAR);
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
    assert_eq!(only_col, want(NEW_COLUMNAR), "columnar chunk served alone");
}
