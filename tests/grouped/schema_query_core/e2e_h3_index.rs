//! H3 spatial index (PRD #1574 slice 2, #1576).
//!
//! `CREATE INDEX … USING H3 (col [, resolution])` encodes a GeoPoint
//! column to its H3 cell-id `u64` and stores it in the existing
//! disk-paged sorted (B-tree) index — NOT the retired in-RAM R-tree.
//! These tests assert:
//!   1. the index builds from existing rows and surfaces through normal
//!      index introspection (`red.show_indexes`),
//!   2. it survives a restart, rebuilt from the catalog like any other
//!      B-tree index,
//!   3. the write path (insert / update) maintains the sorted index
//!      with no per-point resident structure, and a point move is a
//!      single B-tree key update.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use reddb::storage::schema::Value;
use reddb::storage::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
use reddb::{RedDBOptions, RedDBRuntime};
use std::sync::Arc;

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

fn show_index_kinds(rt: &RedDBRuntime) -> Vec<String> {
    let res = rt
        .execute_query("SELECT kind FROM red.show_indexes")
        .expect("red.show_indexes must be queryable");
    res.result
        .records
        .iter()
        .filter_map(|r| match r.get("kind") {
            Some(Value::Text(t)) => Some(t.to_string()),
            _ => None,
        })
        .collect()
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

fn insert_legacy_index_descriptor(rt: &RedDBRuntime, name: &str, method: &str) {
    let store = rt.db().store();
    let _ = store.get_or_create_collection("red_index_registry");
    let entity = UnifiedEntity::new(
        EntityId::new(0),
        EntityKind::TableRow {
            table: Arc::from("red_index_registry"),
            row_id: 0,
        },
        EntityData::Row(RowData {
            columns: Vec::new(),
            named: Some(
                [
                    ("collection".to_string(), Value::text("places")),
                    ("name".to_string(), Value::text(name)),
                    ("columns".to_string(), Value::text("loc")),
                    ("method".to_string(), Value::text(method)),
                    ("resolution".to_string(), Value::Integer(0)),
                    ("unique".to_string(), Value::Boolean(false)),
                    ("dropped".to_string(), Value::Boolean(false)),
                ]
                .into_iter()
                .collect(),
            ),
            schema: None,
        }),
    );
    store
        .insert_auto("red_index_registry", entity)
        .expect("legacy descriptor fixture insert must succeed");
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

    assert!(
        !show_index_kinds(&rt).iter().any(|kind| kind == "RTREE"),
        "catalog views must not advertise the retired RTREE method"
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
        !show_index_kinds(&rt).iter().any(|kind| kind == "RTREE"),
        "H3 write-path maintenance must not surface a retired RTREE index"
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

    assert!(
        !show_index_kinds(&rt).iter().any(|kind| kind == "RTREE"),
        "rehydrated catalog must not advertise RTREE"
    );
}

// ── SEARCH SPATIAL parity: H3 ring scan vs full scan (PRD #1574 slice 3) ─────
//
// `SEARCH SPATIAL RADIUS/BBOX/NEAREST` must return byte-identical results
// whether the geo column carries an H3 index (covering-ring scan over the
// disk B-tree) or no index at all (full collection scan + haversine). The
// parity is asserted on the SAME table and the SAME runtime: the queries run
// once with no index (full scan), then again after `CREATE INDEX … USING H3`
// (the ring scan), so the underlying entity store — and therefore the
// `entity_id` ordering for equidistant ties — is identical between the two
// runs. Only the index mechanism changes.

/// A geo corpus chosen to exercise cell boundaries: points cluster around
/// central Paris in *different* res-9 cells, plus an exact duplicate of the
/// centre (a 0-km tie that must keep its store order on both paths) and
/// far-away outliers in other regions.
const GEO_CORPUS: &[(i64, &str)] = &[
    (1, "48.8566,2.3522"),     // Notre-Dame (centre)
    (2, "48.8606,2.3376"),     // Louvre, ~1.2 km
    (3, "48.8530,2.3499"),     // ~0.4 km
    (4, "48.8738,2.2950"),     // Arc de Triomphe, ~4.5 km
    (5, "48.8584,2.2945"),     // Eiffel Tower, ~4.2 km
    (6, "48.8000,2.3000"),     // ~6.7 km
    (7, "49.0000,2.5000"),     // ~20 km
    (8, "48.8566,2.3522"),     // exact duplicate of the centre (0-km tie)
    (9, "51.5074,-0.1278"),    // London, ~344 km
    (10, "-23.5505,-46.6333"), // São Paulo, far hemisphere
];

fn seed_geo_corpus(table: &str) -> RedDBRuntime {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    rt.execute_query(&format!("CREATE TABLE {table} (id INT, loc GEOPOINT)"))
        .unwrap();
    for (id, loc) in GEO_CORPUS {
        rt.execute_query(&format!(
            "INSERT INTO {table} (id, loc) VALUES ({id}, '{loc}')"
        ))
        .unwrap();
    }
    rt
}

/// Ordered `(entity_id, distance_km_bits)` rows for exact comparison. The
/// distance is compared by raw bits so two haversine computations are held
/// to byte-for-byte equality; rows without a `distance_km` column (BBOX)
/// contribute a `0` placeholder, which is identical on both paths.
fn spatial_rows(rt: &RedDBRuntime, query: &str) -> Vec<(u64, u64)> {
    let res = rt.execute_query(query).expect("spatial query must execute");
    res.result
        .records
        .iter()
        .map(|r| {
            let id = match r.get("entity_id") {
                Some(Value::UnsignedInteger(n)) => *n,
                other => panic!("missing entity_id: {other:?}"),
            };
            let dist = match r.get("distance_km") {
                Some(Value::Float(f)) => f.to_bits(),
                _ => 0,
            };
            (id, dist)
        })
        .collect()
}

/// For each query: run the full scan (no index), create the H3 index, run
/// the ring scan, and assert the two are byte-identical.
fn assert_h3_parity(create_index: &str, queries: &[String]) {
    let rt = seed_geo_corpus("places");
    let baseline: Vec<Vec<(u64, u64)>> = queries.iter().map(|q| spatial_rows(&rt, q)).collect();
    rt.execute_query(create_index).unwrap();
    for (q, expected) in queries.iter().zip(&baseline) {
        assert_eq!(
            &spatial_rows(&rt, q),
            expected,
            "H3 ring scan diverged from full scan for: {q}"
        );
    }
}

#[test]
fn h3_radius_parity_with_full_scan() {
    // Tight radii land on res-9 cell boundaries (the +1 ring margin must
    // still find the neighbouring in-radius points); large radii exceed the
    // cover cap and fall back to the full scan — both must match.
    let queries: Vec<String> = [
        (48.8566, 2.3522, 0.5),
        (48.8566, 2.3522, 1.0),
        (48.8566, 2.3522, 5.0),
        (48.8584, 2.2945, 2.0),
        (48.8566, 2.3522, 50.0),
        (48.8566, 2.3522, 500.0),
        (48.8566, 2.3522, 20000.0),
    ]
    .iter()
    .map(|(clat, clon, r)| {
        format!("SEARCH SPATIAL RADIUS {clat} {clon} {r} COLLECTION places COLUMN loc")
    })
    .collect();
    assert_h3_parity("CREATE INDEX idx ON places (loc) USING H3", &queries);
}

#[test]
fn h3_nearest_parity_with_full_scan() {
    let queries: Vec<String> = [
        (48.8566, 2.3522, 1),
        (48.8566, 2.3522, 4),
        (48.8566, 2.3522, 6),
        (48.8584, 2.2945, 3),
        // K larger than the corpus → ring expansion exhausts and the cover
        // proof falls back to the full scan; results must still match.
        (48.8566, 2.3522, 50),
    ]
    .iter()
    .map(|(lat, lon, k)| {
        format!("SEARCH SPATIAL NEAREST {lat} {lon} K {k} COLLECTION places COLUMN loc")
    })
    .collect();
    assert_h3_parity("CREATE INDEX idx ON places (loc) USING H3 (9)", &queries);
}

#[test]
fn h3_bbox_parity_with_full_scan() {
    let queries: Vec<String> = [
        (48.84, 2.33, 48.87, 2.36),   // tight box around the centre cluster
        (48.80, 2.25, 48.90, 2.40),   // wider Paris box
        (48.00, 2.00, 50.00, 3.00),   // region-scale box
        (-90.0, -180.0, 90.0, 180.0), // whole globe
    ]
    .iter()
    .map(|(min_lat, min_lon, max_lat, max_lon)| {
        format!(
            "SEARCH SPATIAL BBOX {min_lat} {min_lon} {max_lat} {max_lon} COLLECTION places COLUMN loc"
        )
    })
    .collect();
    assert_h3_parity("CREATE INDEX idx ON places (loc) USING H3", &queries);
}

// ── Slice 4 (PRD #1574 / #1578): H3 is the DEFAULT spatial index ─────────────
//
// A generic spatial index request (`USING SPATIAL`) resolves to the
// disk-resident H3 index, NOT the retired in-RAM R-tree.

#[test]
fn bare_spatial_index_defaults_to_h3() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_places(&rt);

    // Generic spatial request — no explicit backend named.
    rt.execute_query("CREATE INDEX idx_loc ON places (loc) USING SPATIAL")
        .unwrap();

    // It resolves to the H3 disk index.
    let (kind, entries) = show_index(&rt, "places", "idx_loc")
        .expect("generic spatial index must appear in red.show_indexes");
    assert_eq!(
        kind, "H3",
        "a bare/generic spatial index must default to H3"
    );
    assert_eq!(
        entries, 3,
        "the H3 default must build over all existing rows"
    );

    assert!(
        !show_index_kinds(&rt).iter().any(|kind| kind == "RTREE"),
        "the default spatial index must not advertise the retired RTREE method"
    );

    // SEARCH SPATIAL works unchanged against the defaulted index: a radius
    // centred on Paris finds the Paris row (an exact 0-km hit). `entity_id`
    // is the engine's internal id, so we assert on the zero distance of the
    // centre point rather than the user `id` column.
    let hits = spatial_rows(
        &rt,
        "SEARCH SPATIAL RADIUS 48.8566 2.3522 5.0 COLLECTION places COLUMN loc",
    );
    assert!(
        !hits.is_empty(),
        "SEARCH SPATIAL must return hits through the defaulted H3 index"
    );
    assert!(
        hits.iter().any(|(_, dist)| *dist == 0.0_f64.to_bits()),
        "SEARCH SPATIAL must find the Paris centre point (0 km) via the H3 default"
    );
}

#[test]
fn explicit_rtree_is_rejected_with_didactic_message() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_places(&rt);

    let err = rt
        .execute_query("CREATE INDEX idx_r ON places (loc) USING RTREE")
        .expect_err("USING RTREE must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("USING RTREE was removed"), "{msg}");
    assert!(msg.contains("Use USING H3"), "{msg}");
    assert!(
        msg.contains("CREATE INDEX idx_loc ON events (gpsLocation) USING H3"),
        "{msg}"
    );
    assert!(
        show_index(&rt, "places", "idx_r").is_none(),
        "rejected RTREE DDL must not register a catalog entry"
    );
}

#[test]
fn h3_radius_uses_disk_btree_not_rtree() {
    // The H3 radius path must run off the sorted disk B-tree cell index and
    // never surface the retired in-RAM R-tree for the column.
    let rt = seed_geo_corpus("places");
    rt.execute_query("CREATE INDEX idx ON places (loc) USING H3")
        .unwrap();
    let _ = rt
        .execute_query("SEARCH SPATIAL RADIUS 48.8566 2.3522 5.0 COLLECTION places COLUMN loc")
        .unwrap();
    assert!(
        !show_index_kinds(&rt).iter().any(|kind| kind == "RTREE"),
        "an H3 radius query must not create or advertise a retired RTREE index"
    );
}

#[test]
fn retired_rtree_descriptors_are_dropped_on_load() {
    let dir = support::temp_data_dir("e2e-retired-rtree-descriptor");
    let path = dir.join("data.rdb");
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
        seed_places(&rt);
        insert_legacy_index_descriptor(&rt, "idx_legacy_spatial", "spatial");
        insert_legacy_index_descriptor(&rt, "idx_legacy_rtree", "rtree");
    }

    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
    assert!(
        show_index(&rt, "places", "idx_legacy_spatial").is_none(),
        "legacy spatial descriptor must be dropped during load"
    );
    assert!(
        show_index(&rt, "places", "idx_legacy_rtree").is_none(),
        "legacy rtree descriptor must be dropped during load"
    );
    assert!(
        rt.execute_query("SELECT * FROM places")
            .expect("store must open and serve queries after dropping descriptors")
            .result
            .records
            .len()
            == 3
    );
}
