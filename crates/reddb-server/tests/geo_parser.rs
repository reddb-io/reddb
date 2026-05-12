//! Parser hardening test suite for the geo / spatial surface
//! (issue #104).
//!
//! Reuses the `tests/support/parser_hardening` harness from #87 to
//! cover the SEARCH SPATIAL grammar (RADIUS / BBOX / NEAREST), the
//! RTREE index method, and the geo scalar functions
//! (`GEO_DISTANCE`, `HAVERSINE`, `VINCENTY`, …).
//!
//! The geo grammar is reached through the standard
//! `reddb_server::storage::query::parser::parse` entry point, so
//! `ParserLimits` cascade automatically — this file pins the
//! contract.
//!
//! Phase B follow-up (issue #115): the FIXME pins below were flipped
//! into regression guards once the parser landed lat/lon range
//! validation, K/radius positivity checks, and unary-minus support.

mod support {
    pub mod parser_hardening;
}

use proptest::prelude::*;
use reddb_server::storage::query::parser::{self, ParseError, ParserLimits};
use support::parser_hardening::{
    self as harness, assert_no_panic_on, corpus::geo_adversarial_inputs, geo_grammar,
    HardenedParser,
};

/// `HardenedParser` shim around the geo / spatial surface. Funnels
/// into the standard parser entry point — what makes this distinct
/// from the SQL / migration shims is that the property + snapshot
/// suites below only feed geo-shaped inputs.
pub struct GeoParser;

impl HardenedParser for GeoParser {
    type Error = ParseError;

    fn parse(input: &str) -> Result<(), Self::Error> {
        parser::parse(input).map(|_| ())
    }

    fn parse_with_limits(input: &str, limits: ParserLimits) -> Result<(), Self::Error> {
        let mut p = parser::Parser::with_limits(input, limits)?;
        p.parse().map(|_| ())
    }
}

// ---- panic-safety on adversarial corpus -------------------------

#[test]
fn geo_parser_does_not_panic_on_adversarial_corpus() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            for (name, input) in geo_adversarial_inputs() {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    assert_no_panic_on::<GeoParser>(&input);
                }));
                if result.is_err() {
                    panic!("geo adversarial corpus entry {} panicked", name);
                }
            }
        })
        .expect("spawn corpus thread");
    handle.join().expect("corpus thread panic");
}

// ---- property tests ---------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 64,
        ..ProptestConfig::default()
    })]

    /// Generated SEARCH SPATIAL RADIUS shapes parse cleanly.
    #[test]
    fn proptest_radius_roundtrips(s in geo_grammar::radius_stmt()) {
        harness::roundtrip_property::<GeoParser>(&s);
        prop_assert!(
            GeoParser::parse(&s).is_ok(),
            "spatial radius did not parse: {}", s
        );
    }

    /// Generated SEARCH SPATIAL BBOX shapes parse cleanly.
    #[test]
    fn proptest_bbox_roundtrips(s in geo_grammar::bbox_stmt()) {
        harness::roundtrip_property::<GeoParser>(&s);
        prop_assert!(
            GeoParser::parse(&s).is_ok(),
            "spatial bbox did not parse: {}", s
        );
    }

    /// Generated SEARCH SPATIAL NEAREST shapes parse cleanly.
    #[test]
    fn proptest_nearest_roundtrips(s in geo_grammar::nearest_stmt()) {
        harness::roundtrip_property::<GeoParser>(&s);
        prop_assert!(
            GeoParser::parse(&s).is_ok(),
            "spatial nearest did not parse: {}", s
        );
    }

    /// Generated CREATE INDEX … USING RTREE shapes parse cleanly.
    #[test]
    fn proptest_rtree_index_roundtrips(s in geo_grammar::rtree_index_stmt()) {
        harness::roundtrip_property::<GeoParser>(&s);
        prop_assert!(
            GeoParser::parse(&s).is_ok(),
            "rtree index did not parse: {}", s
        );
    }

    /// Generated SELECT <geo-fn>(...) FROM t shapes parse cleanly.
    #[test]
    fn proptest_distance_fn_roundtrips(s in geo_grammar::distance_fn_stmt()) {
        harness::roundtrip_property::<GeoParser>(&s);
        prop_assert!(
            GeoParser::parse(&s).is_ok(),
            "distance fn did not parse: {}", s
        );
    }

    /// Arbitrary suffix after a SEARCH SPATIAL prefix never panics —
    /// Err is fine, panic is not.
    #[test]
    fn proptest_geo_arbitrary_suffix_no_panic(
        prefix in prop_oneof![
            Just("SEARCH SPATIAL RADIUS ".to_string()),
            Just("SEARCH SPATIAL BBOX ".to_string()),
            Just("SEARCH SPATIAL NEAREST ".to_string()),
        ],
        suffix in ".{0,512}",
    ) {
        let s = format!("{}{}", prefix, suffix);
        harness::roundtrip_property::<GeoParser>(&s);
    }

    /// Tighter limits always refuse oversized geo inputs.
    #[test]
    fn proptest_geo_input_size_limit_enforced(len in 200usize..2000) {
        let limits = ParserLimits {
            max_input_bytes: 64,
            ..ParserLimits::default()
        };
        let suffix = "x".repeat(len);
        let input = format!(
            "SEARCH SPATIAL RADIUS 0.0 0.0 10.0 COLLECTION {} COLUMN col",
            suffix
        );
        let r = GeoParser::parse_with_limits(&input, limits);
        prop_assert!(r.is_err(), "oversized geo input must error");
    }
}

// ---- happy-path regression tests --------------------------------
//
// 5–10 hand-rolled positive tests pin the documented happy-path
// shapes so a future grammar tweak that breaks one of these
// surfaces as a precise failure rather than a fuzzy proptest
// shrink.

use reddb_server::storage::query::ast::{IndexMethod, QueryExpr, SearchCommand};

fn parse_query(input: &str) -> QueryExpr {
    parser::parse(input)
        .unwrap_or_else(|e| panic!("expected ok for {input:?}, got error: {e}"))
        .query
}

#[test]
fn happy_radius_paris_10km_parses() {
    let q = parse_query(
        "SEARCH SPATIAL RADIUS 48.8566 2.3522 10.0 COLLECTION sites COLUMN location LIMIT 50",
    );
    match q {
        QueryExpr::SearchCommand(SearchCommand::SpatialRadius {
            center_lat,
            center_lon,
            radius_km,
            collection,
            column,
            limit,
            ..
        }) => {
            assert!((center_lat - 48.8566).abs() < 1e-9);
            assert!((center_lon - 2.3522).abs() < 1e-9);
            assert!((radius_km - 10.0).abs() < 1e-9);
            assert_eq!(collection, "sites");
            assert_eq!(column, "location");
            assert_eq!(limit, 50);
        }
        other => panic!("expected SpatialRadius, got {other:?}"),
    }
}

#[test]
fn happy_radius_default_limit_is_one_hundred() {
    let q = parse_query("SEARCH SPATIAL RADIUS 0.0 0.0 1.5 COLLECTION sites COLUMN location");
    match q {
        QueryExpr::SearchCommand(SearchCommand::SpatialRadius { limit, .. }) => {
            assert_eq!(limit, 100, "default limit pinned at 100");
        }
        other => panic!("expected SpatialRadius, got {other:?}"),
    }
}

#[test]
fn happy_bbox_unit_square_parses() {
    let q = parse_query(
        "SEARCH SPATIAL BBOX 0.0 0.0 1.0 1.0 COLLECTION sites COLUMN location LIMIT 25",
    );
    match q {
        QueryExpr::SearchCommand(SearchCommand::SpatialBbox {
            min_lat,
            min_lon,
            max_lat,
            max_lon,
            collection,
            column,
            limit,
        }) => {
            assert_eq!(min_lat, 0.0);
            assert_eq!(min_lon, 0.0);
            assert_eq!(max_lat, 1.0);
            assert_eq!(max_lon, 1.0);
            assert_eq!(collection, "sites");
            assert_eq!(column, "location");
            assert_eq!(limit, 25);
        }
        other => panic!("expected SpatialBbox, got {other:?}"),
    }
}

#[test]
fn happy_nearest_k_5_parses() {
    let q =
        parse_query("SEARCH SPATIAL NEAREST 40.7128 74.0060 K 5 COLLECTION sites COLUMN location");
    match q {
        QueryExpr::SearchCommand(SearchCommand::SpatialNearest {
            lat,
            lon,
            k,
            collection,
            column,
            ..
        }) => {
            assert!((lat - 40.7128).abs() < 1e-9);
            assert!((lon - 74.0060).abs() < 1e-9);
            assert_eq!(k, 5);
            assert_eq!(collection, "sites");
            assert_eq!(column, "location");
        }
        other => panic!("expected SpatialNearest, got {other:?}"),
    }
}

#[test]
fn happy_rtree_index_parses_with_method() {
    let q = parse_query("CREATE INDEX gix_loc ON sites (location) USING RTREE");
    match q {
        QueryExpr::CreateIndex(ci) => {
            assert_eq!(ci.name, "gix_loc");
            assert_eq!(ci.table, "sites");
            assert_eq!(ci.columns, vec!["location".to_string()]);
            assert_eq!(ci.method, IndexMethod::RTree);
            assert!(!ci.unique);
        }
        other => panic!("expected CreateIndex, got {other:?}"),
    }
}

#[test]
fn happy_geo_distance_in_projection_parses() {
    // The geo scalar fn lives in projection position; the parser
    // accepts it as a function call lowered into the SELECT list.
    parse_query("SELECT GEO_DISTANCE(0.0, 0.0, 1.0, 1.0) FROM t");
}

#[test]
fn happy_haversine_in_projection_parses() {
    parse_query("SELECT HAVERSINE(48.8566, 2.3522, 51.5074, 0.1278) FROM t");
}

#[test]
fn happy_vincenty_in_projection_parses() {
    parse_query("SELECT VINCENTY(48.8566, 2.3522, 51.5074, 0.1278) FROM t");
}

#[test]
fn happy_radius_at_equator_parses() {
    parse_query("SEARCH SPATIAL RADIUS 0.0 0.0 6371.0 COLLECTION sites COLUMN location");
}

#[test]
fn happy_nearest_at_origin_parses() {
    parse_query("SEARCH SPATIAL NEAREST 0.0 0.0 K 1 COLLECTION sites COLUMN location");
}

// ---- regression guards (originally FIXME pins) ------------------
//
// Phase B / issue #115 landed the parser fixes; the tests below were
// flipped from `#[ignore]` FIXME pins into live guards.

/// Regression guard for #107: `parse_float` accepts a unary `-`,
/// so southern / western hemisphere coordinates parse cleanly.
/// Real-world geo applications routinely query Sydney
/// (-33.86, 151.21) or Buenos Aires (-34.60, -58.38).
#[test]
fn negative_latitude_parses() {
    let r = parser::parse(
        "SEARCH SPATIAL NEAREST -33.8688 151.2093 K 5 COLLECTION sites COLUMN location",
    );
    assert!(
        r.is_ok(),
        "negative lat should parse with unary-minus wired into the SPATIAL float position"
    );
}

/// Regression guard for #104-followup-2: the parser now rejects
/// out-of-range latitude (`> 90` or `< -90`) and longitude
/// (`> 180` or `< -180`) with a `ValueOutOfRange` error.
#[test]
fn lat_out_of_range_rejected() {
    let r = parser::parse("SEARCH SPATIAL RADIUS 91.0 0.0 10.0 COLLECTION sites COLUMN location");
    assert!(r.is_err(), "lat=91 should be rejected as out-of-range");
}

/// Regression guard for #104-followup-3: `K = 0` is degenerate;
/// rejected at parse time with a `ValueOutOfRange` error.
#[test]
fn nearest_k_zero_rejected() {
    let r = parser::parse("SEARCH SPATIAL NEAREST 0.0 0.0 K 0 COLLECTION sites COLUMN location");
    assert!(r.is_err(), "K=0 should be rejected as degenerate");
}

/// Regression guard for #104-followup-4: `RADIUS r = 0` is
/// degenerate; rejected at parse time with a `ValueOutOfRange`
/// error.
#[test]
fn radius_zero_rejected() {
    let r = parser::parse("SEARCH SPATIAL RADIUS 0.0 0.0 0.0 COLLECTION sites COLUMN location");
    assert!(r.is_err(), "radius=0 should be rejected as degenerate");
}
