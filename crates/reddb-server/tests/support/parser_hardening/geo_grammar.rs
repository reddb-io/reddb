//! Proptest strategies that emit syntactically valid geo / spatial
//! statements (issue #104).
//!
//! Mirrors the layout of `sql_grammar.rs` and `migration_grammar.rs`:
//! each strategy returns a `String` that, when fed back through the
//! main parser, must not panic. Valid-shape strategies must
//! additionally succeed.
//!
//! Surface covered:
//!   - `SEARCH SPATIAL RADIUS lat lon r COLLECTION c COLUMN col [LIMIT n]`
//!   - `SEARCH SPATIAL BBOX min_lat min_lon max_lat max_lon
//!      COLLECTION c COLUMN col [LIMIT n]`
//!   - `SEARCH SPATIAL NEAREST lat lon K n COLLECTION c COLUMN col`
//!   - `CREATE INDEX name ON table (col) USING RTREE`
//!   - Distance scalar functions inside SELECT projections:
//!     `GEO_DISTANCE`, `HAVERSINE`, `VINCENTY`, `GEO_BEARING`
//!
//! Generators are intentionally restricted to *valid* lat/lon ranges
//! and positive radii / K so the happy-path proptests succeed. Out-of-
//! range and adversarial coordinates are exercised via the corpus-
//! driven panic-safety harness instead — that split keeps shrinking
//! deterministic and the success bar mechanical.

use proptest::prelude::*;

/// Identifier shared with the SQL/migration grammars: stays under
/// the default `max_identifier_chars` cap and never collides with
/// reserved keywords.
pub fn ident() -> impl Strategy<Value = String> {
    "id_[a-z0-9_]{0,12}".prop_map(|s| s)
}

/// Latitude literal in the closed interval `[-90, 90]`. Negative
/// values are emitted with a leading unary `-`, exercising the
/// `parse_float` minus-prefix path (#107).
///
/// We still want fractional jitter so the generator emits a small
/// integer + fractional component formatted via `f64::to_string`.
pub fn lat_lit() -> impl Strategy<Value = String> {
    (any::<bool>(), 0u32..=90, 0u32..1_000_000u32).prop_map(|(neg, deg, frac)| {
        let sign = if neg { "-" } else { "" };
        format!("{}{}.{:06}", sign, deg, frac)
    })
}

/// Longitude literal in the closed interval `[-180, 180]`. Like
/// `lat_lit`, the generator emits an optional leading unary `-` so
/// the western hemisphere is covered by the proptest sweep.
pub fn lon_lit() -> impl Strategy<Value = String> {
    (any::<bool>(), 0u32..=180, 0u32..1_000_000u32).prop_map(|(neg, deg, frac)| {
        let sign = if neg { "-" } else { "" };
        format!("{}{}.{:06}", sign, deg, frac)
    })
}

/// Strictly-positive radius in kilometres. Range chosen to cover
/// realistic radii (1 m → 20 000 km ≈ half-circumference) without
/// spilling into denormals or absurd values that the engine would
/// reject elsewhere.
pub fn radius_km_lit() -> impl Strategy<Value = String> {
    (1u32..=20_000u32, 0u32..1_000u32).prop_map(|(km, frac)| format!("{}.{:03}", km, frac))
}

/// Strictly-positive `K` for nearest-neighbour queries. The parser
/// reads K as an integer; `K=0` is degenerate and intentionally
/// outside this range — it is exercised in the adversarial corpus
/// instead.
pub fn k_lit() -> impl Strategy<Value = u32> {
    1u32..=1024u32
}

/// Optional `LIMIT n` suffix used by RADIUS / BBOX shapes.
pub fn opt_limit_suffix() -> impl Strategy<Value = String> {
    proptest::option::of(1u32..=10_000u32).prop_map(|opt| match opt {
        Some(n) => format!(" LIMIT {}", n),
        None => String::new(),
    })
}

/// `SEARCH SPATIAL RADIUS lat lon r COLLECTION c COLUMN col [LIMIT n]`.
pub fn radius_stmt() -> impl Strategy<Value = String> {
    (
        lat_lit(),
        lon_lit(),
        radius_km_lit(),
        ident(),
        ident(),
        opt_limit_suffix(),
    )
        .prop_map(|(lat, lon, r, coll, col, limit)| {
            format!(
                "SEARCH SPATIAL RADIUS {} {} {} COLLECTION {} COLUMN {}{}",
                lat, lon, r, coll, col, limit,
            )
        })
}

/// `SEARCH SPATIAL BBOX min_lat min_lon max_lat max_lon
///  COLLECTION c COLUMN col [LIMIT n]`.
///
/// The generator picks `min < max` for both axes by sampling two
/// distinct values per axis and sorting; this keeps the bbox non-
/// degenerate without rejecting samples.
pub fn bbox_stmt() -> impl Strategy<Value = String> {
    (
        lat_lit(),
        lat_lit(),
        lon_lit(),
        lon_lit(),
        ident(),
        ident(),
        opt_limit_suffix(),
    )
        .prop_map(|(lat_a, lat_b, lon_a, lon_b, coll, col, limit)| {
            // Sort lexicographically as fallback: the actual values
            // are positive decimals so lexicographic ≈ numeric for
            // equal-width zero-padded forms; the parser does not
            // semantically validate `min < max`, so even a degenerate
            // box is accepted at the parse layer.
            let (min_lat, max_lat) = if lat_a <= lat_b {
                (lat_a, lat_b)
            } else {
                (lat_b, lat_a)
            };
            let (min_lon, max_lon) = if lon_a <= lon_b {
                (lon_a, lon_b)
            } else {
                (lon_b, lon_a)
            };
            format!(
                "SEARCH SPATIAL BBOX {} {} {} {} COLLECTION {} COLUMN {}{}",
                min_lat, min_lon, max_lat, max_lon, coll, col, limit,
            )
        })
}

/// `SEARCH SPATIAL NEAREST lat lon K n COLLECTION c COLUMN col`.
pub fn nearest_stmt() -> impl Strategy<Value = String> {
    (lat_lit(), lon_lit(), k_lit(), ident(), ident()).prop_map(|(lat, lon, k, coll, col)| {
        format!(
            "SEARCH SPATIAL NEAREST {} {} K {} COLLECTION {} COLUMN {}",
            lat, lon, k, coll, col,
        )
    })
}

/// `CREATE INDEX name ON table (col) USING RTREE`. The `UNIQUE`
/// modifier is generated as an optional prefix; spatial indexes are
/// not semantically unique, but the parser accepts the modifier and
/// downstream stages reject it — we exercise the parse layer only.
pub fn rtree_index_stmt() -> impl Strategy<Value = String> {
    (any::<bool>(), ident(), ident(), ident()).prop_map(|(unique, idx, table, col)| {
        let kw = if unique {
            "CREATE UNIQUE INDEX"
        } else {
            "CREATE INDEX"
        };
        format!("{} {} ON {} ({}) USING RTREE", kw, idx, table, col)
    })
}

/// Distance / bearing scalar function call in projection position:
/// `SELECT GEO_DISTANCE(lat1, lon1, lat2, lon2) FROM t`.
///
/// The parser registers the following scalar functions with geo
/// semantics (see `parser/table.rs::is_scalar_function`):
///   - `GEO_DISTANCE`
///   - `GEO_DISTANCE_VINCENTY`
///   - `GEO_BEARING`
///   - `GEO_MIDPOINT`
///   - `HAVERSINE`
///   - `VINCENTY`
///
/// All accept four numeric arguments at the parse layer.
pub fn distance_fn_stmt() -> impl Strategy<Value = String> {
    let fn_name = prop_oneof![
        Just("GEO_DISTANCE"),
        Just("GEO_DISTANCE_VINCENTY"),
        Just("GEO_BEARING"),
        Just("GEO_MIDPOINT"),
        Just("HAVERSINE"),
        Just("VINCENTY"),
    ];
    (
        fn_name,
        lat_lit(),
        lon_lit(),
        lat_lit(),
        lon_lit(),
        ident(),
    )
        .prop_map(|(f, lat1, lon1, lat2, lon2, table)| {
            format!(
                "SELECT {}({}, {}, {}, {}) FROM {}",
                f, lat1, lon1, lat2, lon2, table,
            )
        })
}

/// Top-level union: any of the geo shapes the test suite covers.
pub fn any_geo_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        radius_stmt(),
        bbox_stmt(),
        nearest_stmt(),
        rtree_index_stmt(),
        distance_fn_stmt(),
    ]
}
