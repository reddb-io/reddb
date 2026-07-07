//! Parser hardening test suite for the time-series DSL (issue #102).
//!
//! Reuses the `tests/support/parser_hardening` harness from #87 and
//! the time-series strategy module added by this slice. The
//! time-series parser shares the top-level entry point with the rest
//! of the SQL grammar, so the shim funnels into `parser::parse` —
//! what makes this distinct is the strategy + corpus pair feeding
//! exclusively `CREATE TIMESERIES` / `CREATE HYPERTABLE` /
//! `CREATE MATERIALIZED VIEW` shapes.
//!
//! Phase A constraint: tests-only. Bugs surfaced by the suite are
//! pinned with `// FIXME(#NN)` markers — no source mods land here.

mod support {
    pub mod parser_hardening;
}

use proptest::prelude::*;
use reddb_server::storage::query::parser::{self, ParseError, ParserLimits};
use support::parser_hardening::{
    self as harness, assert_no_panic_on, corpus::timeseries_adversarial_inputs, timeseries_grammar,
    HardenedParser,
};

/// `HardenedParser` shim around the time-series surface. Identical to
/// the migration shim — the differentiator is the strategy mix the
/// proptest blocks below feed in.
pub struct TimeseriesParser;

impl HardenedParser for TimeseriesParser {
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
fn timeseries_parser_does_not_panic_on_adversarial_corpus() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            for (name, input) in timeseries_adversarial_inputs() {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    assert_no_panic_on::<TimeseriesParser>(&input);
                }));
                if result.is_err() {
                    panic!("timeseries adversarial corpus entry {} panicked", name);
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

    /// Generated CREATE TIMESERIES shapes parse cleanly.
    #[test]
    fn proptest_create_timeseries_roundtrips(s in timeseries_grammar::create_timeseries_stmt()) {
        harness::roundtrip_property::<TimeseriesParser>(&s);
        prop_assert!(
            TimeseriesParser::parse(&s).is_ok(),
            "create timeseries did not parse: {}", s
        );
    }

    /// Generated CREATE HYPERTABLE shapes parse cleanly.
    #[test]
    fn proptest_create_hypertable_roundtrips(s in timeseries_grammar::create_hypertable_stmt()) {
        harness::roundtrip_property::<TimeseriesParser>(&s);
        prop_assert!(
            TimeseriesParser::parse(&s).is_ok(),
            "create hypertable did not parse: {}", s
        );
    }

    /// Every supported `CHUNK_INTERVAL '<dur>'` literal parses.
    #[test]
    fn proptest_chunk_interval_units_roundtrip(
        s in timeseries_grammar::chunk_interval_focused_stmt()
    ) {
        harness::roundtrip_property::<TimeseriesParser>(&s);
        prop_assert!(
            TimeseriesParser::parse(&s).is_ok(),
            "chunk interval focused stmt did not parse: {}", s
        );
    }

    /// Every supported `RETENTION n unit` clause parses.
    #[test]
    fn proptest_retention_clause_roundtrip(
        s in timeseries_grammar::retention_focused_stmt()
    ) {
        harness::roundtrip_property::<TimeseriesParser>(&s);
        prop_assert!(
            TimeseriesParser::parse(&s).is_ok(),
            "retention focused stmt did not parse: {}", s
        );
    }

    /// Continuous-aggregate envelope (CREATE MATERIALIZED VIEW) parses.
    #[test]
    fn proptest_continuous_aggregate_roundtrips(
        s in timeseries_grammar::continuous_aggregate_stmt()
    ) {
        harness::roundtrip_property::<TimeseriesParser>(&s);
        prop_assert!(
            TimeseriesParser::parse(&s).is_ok(),
            "continuous aggregate did not parse: {}", s
        );
    }

    /// Arbitrary bytes prefixed with a time-series keyword never
    /// panic — Err is fine, panic is not.
    #[test]
    fn proptest_timeseries_arbitrary_suffix_no_panic(
        prefix in prop_oneof![
            Just("CREATE TIMESERIES ".to_string()),
            Just("CREATE HYPERTABLE ".to_string()),
            Just("DROP TIMESERIES ".to_string()),
            Just("CREATE MATERIALIZED VIEW ".to_string()),
        ],
        suffix in ".{0,512}",
    ) {
        let s = format!("{}{}", prefix, suffix);
        harness::roundtrip_property::<TimeseriesParser>(&s);
    }

    /// Tighter limits always refuse oversized hypertable bodies.
    #[test]
    fn proptest_timeseries_input_size_limit_enforced(len in 200usize..2000) {
        let limits = ParserLimits {
            max_input_bytes: 64,
            ..ParserLimits::default()
        };
        let body = "x".repeat(len);
        let input = format!("CREATE TIMESERIES m1 {}", body);
        let r = TimeseriesParser::parse_with_limits(&input, limits);
        prop_assert!(r.is_err(), "oversized timeseries body must error");
    }
}

// ---- happy-path regression tests --------------------------------
//
// Pin the canonical shapes documented in `parser/timeseries.rs` so a
// future grammar tweak surfaces as a diff here rather than as a
// silent behavioural drift downstream of the runtime dispatcher.

use reddb_server::storage::query::ast::QueryExpr;

fn parse_query(input: &str) -> QueryExpr {
    parser::parse(input)
        .unwrap_or_else(|e| panic!("expected ok for {input:?}, got error: {e}"))
        .query
}

#[test]
fn create_timeseries_with_retention_days_parses() {
    let q = parse_query("CREATE TIMESERIES cpu_metrics RETENTION 90 d");
    match q {
        QueryExpr::CreateTimeSeries(ts) => {
            assert_eq!(ts.name, "cpu_metrics");
            assert_eq!(ts.retention_ms, Some(90 * 86_400_000));
            assert!(ts.hypertable.is_none());
        }
        other => panic!("expected CreateTimeSeries, got {other:?}"),
    }
}

#[test]
fn create_timeseries_with_downsample_policies_parses() {
    let q =
        parse_query("CREATE TIMESERIES cpu_metrics RETENTION 90 d DOWNSAMPLE 1h:5m:avg, 1d:1h:max");
    match q {
        QueryExpr::CreateTimeSeries(ts) => {
            assert_eq!(
                ts.downsample_policies,
                vec!["1h:5m:avg".to_string(), "1d:1h:max".to_string()]
            );
        }
        other => panic!("expected CreateTimeSeries, got {other:?}"),
    }
}

#[test]
fn create_timeseries_with_chunk_size_parses() {
    let q = parse_query("CREATE TIMESERIES m1 CHUNK_SIZE 4096");
    match q {
        QueryExpr::CreateTimeSeries(ts) => {
            assert_eq!(ts.name, "m1");
            assert_eq!(ts.chunk_size, Some(4096));
        }
        other => panic!("expected CreateTimeSeries, got {other:?}"),
    }
}

#[test]
fn create_timeseries_rejects_retired_columnar_keyword() {
    for sql in [
        "CREATE TIMESERIES m1 COLUMNAR",
        "CREATE HYPERTABLE m1 TIME_COLUMN ts CHUNK_INTERVAL '1h' COLUMNAR",
    ] {
        let err = parser::parse(sql)
            .expect_err("COLUMNAR is retired; projection is automatic")
            .to_string();
        assert!(
            err.contains("COLUMNAR is no longer accepted"),
            "unexpected error for {sql}: {err}"
        );
        assert!(
            err.contains("automatic"),
            "error should point at the automatic posture for {sql}: {err}"
        );
    }
}

#[test]
fn create_hypertable_minimal_parses_with_required_clauses() {
    let q = parse_query("CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d'");
    match q {
        QueryExpr::CreateTimeSeries(ts) => {
            let ht = ts.hypertable.expect("hypertable spec populated");
            assert_eq!(ts.name, "metrics");
            assert_eq!(ht.time_column, "ts");
            assert_eq!(ht.chunk_interval_ns, 86_400_000_000_000);
            assert!(ht.default_ttl_ns.is_none());
        }
        other => panic!("expected CreateTimeSeries, got {other:?}"),
    }
}

#[test]
fn create_hypertable_with_ttl_and_retention_parses() {
    let q = parse_query(
        "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d' TTL '90d' RETENTION 90 d",
    );
    match q {
        QueryExpr::CreateTimeSeries(ts) => {
            let ht = ts.hypertable.expect("hypertable spec populated");
            assert_eq!(ht.chunk_interval_ns, 86_400_000_000_000);
            assert_eq!(ht.default_ttl_ns, Some(90 * 86_400_000_000_000));
            assert_eq!(ts.retention_ms, Some(90 * 86_400_000));
        }
        other => panic!("expected CreateTimeSeries, got {other:?}"),
    }
}

#[test]
fn create_hypertable_clause_order_is_unrestricted() {
    // RETENTION before TTL, CHUNK_INTERVAL last — parser accepts any
    // permutation as long as required clauses are present.
    let q = parse_query(
        "CREATE HYPERTABLE metrics RETENTION 30 d TIME_COLUMN ts TTL '7d' CHUNK_INTERVAL '1h'",
    );
    match q {
        QueryExpr::CreateTimeSeries(ts) => {
            let ht = ts.hypertable.expect("hypertable spec populated");
            assert_eq!(ht.time_column, "ts");
            assert_eq!(ht.chunk_interval_ns, 3_600_000_000_000);
            assert_eq!(ht.default_ttl_ns, Some(7 * 86_400_000_000_000));
            assert_eq!(ts.retention_ms, Some(30 * 86_400_000));
        }
        other => panic!("expected CreateTimeSeries, got {other:?}"),
    }
}

#[test]
fn drop_timeseries_with_if_exists_parses() {
    let q = parse_query("DROP TIMESERIES IF EXISTS m1");
    match q {
        QueryExpr::DropTimeSeries(ts) => {
            assert_eq!(ts.name, "m1");
            assert!(ts.if_exists);
        }
        other => panic!("expected DropTimeSeries, got {other:?}"),
    }
}

#[test]
fn create_materialized_view_for_continuous_aggregate_parses() {
    // The continuous-aggregate surface today rides through the
    // materialized-view envelope; pin the shape end-to-end.
    let q =
        parse_query("CREATE MATERIALIZED VIEW IF NOT EXISTS cpu_5m AS SELECT id FROM cpu_metrics");
    match q {
        QueryExpr::CreateView(_) => {}
        other => panic!("expected CreateView, got {other:?}"),
    }
}

#[test]
fn refresh_materialized_view_parses() {
    let q = parse_query("REFRESH MATERIALIZED VIEW cpu_5m");
    match q {
        QueryExpr::RefreshMaterializedView(_) => {}
        other => panic!("expected RefreshMaterializedView, got {other:?}"),
    }
}

#[test]
fn create_hypertable_with_hour_chunk_interval_parses() {
    let q = parse_query("CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '6h'");
    match q {
        QueryExpr::CreateTimeSeries(ts) => {
            let ht = ts.hypertable.expect("hypertable spec populated");
            assert_eq!(ht.chunk_interval_ns, 6 * 3_600_000_000_000);
        }
        other => panic!("expected CreateTimeSeries, got {other:?}"),
    }
}
