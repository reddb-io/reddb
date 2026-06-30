//! H3 spatial index (PRD #1574 slice 2, #1576).
//!
//! `CREATE INDEX … USING H3 (col [, resolution])` encodes a GeoPoint
//! column to its H3 cell-id `u64` and stores it in the existing
//! disk-paged sorted (B-tree) index — NOT the in-RAM rstar R-tree.
//! These tests assert:
//!   1. the index builds from existing rows and surfaces through normal
//!      index introspection (`red.show_indexes`),
//!   2. it survives a restart, rebuilt from the catalog like any other
//!      B-tree index,
//!   3. the write path (insert / update) maintains the sorted index
//!      with no per-point resident structure — the rstar R-tree is never
//!      touched, and a point move is a single B-tree key update.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

/// Pull a single index row from `red.show_indexes` for `table` + `name`.
/// Returns `(kind, entries_indexed)`.
fn show_index(rt: &RedDBRuntime, table: &str, name: &str) -> Option<(String, u64)> {
    let res = rt
        .execute_query("SELECT name, table, kind, entries_indexed FROM red.show_indexes")
        .expect("red.show_indexes must be queryable");
    res.result.records.iter().find_map(|r| {
        let r_name = match r.get("name") {
            Some(Value::Text(t)) => t.to_string(),
            _ => return None,
        };
        let r_table = match r.get("table") {
            Some(Value::Text(t)) => t.to_string(),
            _ => return None,
        };
        if r_name != name || r_table != table {
            return None;
        }
        let kind = match r.get("kind") {
            Some(Value::Text(t)) => t.to_string(),
            _ => return None,
        };
        let entries = match r.get("entries_indexed") {
            Some(Value::UnsignedInteger(n)) => *n,
            Some(Value::Integer(n)) => *n as u64,
            _ => return None,
        };
        Some((kind, entries))
    })
}

fn seed_places(rt: &RedDBRuntime) {
    rt.execute_query("CREATE TABLE places (id INT, loc GEOPOINT)")
        .unwrap();
    // Paris, São Paulo, London.
    for (id, loc) in [
        (1, "48.8566,2.3522"),
        (2, "-23.550520,-46.633308"),
        (3, "51.5074,-0.1278"),
    ] {
        rt.execute_query(&format!(
            "INSERT INTO places (id, loc) VALUES ({id}, '{loc}')"
        ))
        .unwrap();
    }
}

#[test]
fn h3_index_builds_and_surfaces_in_introspection() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_places(&rt);

    rt.execute_query("CREATE INDEX idx_loc ON places (loc) USING H3")
        .unwrap();

    // Introspection: the index shows up as an H3 kind over all 3 rows.
    let (kind, entries) =
        show_index(&rt, "places", "idx_loc").expect("idx_loc must appear in red.show_indexes");
    assert_eq!(kind, "H3", "index kind must render as H3");
    assert_eq!(entries, 3, "H3 index must cover all 3 existing geo rows");

    // It is the disk B-tree (sorted) index — the in-RAM rstar R-tree is
    // never touched by an H3 index (that is slice 4's concern).
    let store = rt.index_store_ref();
    assert!(
        store.spatial.index_stats("places", "loc").is_err(),
        "H3 index must NOT create a resident rstar R-tree for the column"
    );
}

#[test]
fn h3_index_accepts_explicit_resolution() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_places(&rt);

    rt.execute_query("CREATE INDEX idx_loc12 ON places (loc) USING H3 (12)")
        .unwrap();

    let (kind, entries) =
        show_index(&rt, "places", "idx_loc12").expect("idx_loc12 must appear in introspection");
    assert_eq!(kind, "H3");
    assert_eq!(entries, 3);
}

#[test]
fn h3_index_write_path_is_single_btree_key_update() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_places(&rt);
    rt.execute_query("CREATE INDEX idx_loc ON places (loc) USING H3")
        .unwrap();

    // Insert a new point — index grows by exactly one key.
    rt.execute_query("INSERT INTO places (id, loc) VALUES (4, '40.7128,-74.0060')")
        .unwrap();
    let (_, after_insert) = show_index(&rt, "places", "idx_loc").unwrap();
    assert_eq!(after_insert, 4, "insert must add exactly one B-tree key");

    // Move an existing point — a single key re-key (delete old cell +
    // insert new cell), NOT growth. The entry count is unchanged, which
    // is the "writes are single-integer B-tree updates, no per-point RAM
    // growth" acceptance. (DELETE-driven index cleanup is deferred to
    // MVCC vacuum engine-wide, so we assert the move, not a raw delete.)
    rt.execute_query("UPDATE places SET loc = '35.6895,139.6917' WHERE id = 1")
        .unwrap();
    let (_, after_update) = show_index(&rt, "places", "idx_loc").unwrap();
    assert_eq!(
        after_update, 4,
        "a point move is a single B-tree key update — no per-point growth"
    );

    assert!(
        rt.index_store_ref()
            .spatial
            .index_stats("places", "loc")
            .is_err(),
        "no resident rstar R-tree may be allocated by the H3 write path"
    );
}

#[test]
fn h3_index_survives_restart_rebuilt_from_catalog() {
    let dir = support::temp_data_dir("e2e-h3-index-replay");
    let path = dir.join("data.rdb");
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
        seed_places(&rt);
        rt.execute_query("CREATE INDEX idx_loc ON places (loc) USING H3 (10)")
            .unwrap();
        let (kind, entries) = show_index(&rt, "places", "idx_loc").unwrap();
        assert_eq!(kind, "H3");
        assert_eq!(entries, 3, "pre-restart sanity");
    }

    // Reopen the same path: the H3 index must be rebuilt from the
    // persisted catalog descriptor (including its resolution), exactly
    // like any other B-tree index — not lost, not a separate map.
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();

    let total = rt.execute_query("SELECT * FROM places").unwrap();
    assert_eq!(
        total.result.records.len(),
        3,
        "table data must survive restart"
    );

    let (kind, entries) = show_index(&rt, "places", "idx_loc")
        .expect("H3 index must be rehydrated into introspection after restart");
    assert_eq!(kind, "H3", "rehydrated index must still be H3 kind");
    assert_eq!(
        entries, 3,
        "rehydrated H3 index must be rebuilt over all 3 rows from the catalog"
    );

    // Still purely a disk B-tree after rehydrate — no rstar R-tree.
    assert!(
        rt.index_store_ref()
            .spatial
            .index_stats("places", "loc")
            .is_err(),
        "rehydrated H3 index must not allocate a resident R-tree"
    );
}
