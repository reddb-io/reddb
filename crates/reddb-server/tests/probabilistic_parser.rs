//! Parser hardening test suite for the probabilistic data-structure
//! surface (issue #105): HyperLogLog, Count-Min Sketch, Cuckoo Filter.
//!
//! Reuses the `tests/support/parser_hardening` harness from #87 to
//! cover CREATE / DROP envelopes, HLL ADD / COUNT / MERGE / INFO,
//! SKETCH ADD / COUNT / MERGE / INFO, and FILTER ADD / CHECK /
//! DELETE / COUNT / INFO. The probabilistic grammar is reached
//! through the standard `reddb_server::storage::query::parser::parse`
//! entry point, so `ParserLimits` (max_depth / max_input_bytes /
//! max_identifier_chars) cascade automatically — this file pins the
//! contract.
//!
//! Phase B follow-up (issue #115): the FIXME pins below were flipped
//! into regression guards once the parser landed `Token::Depth`
//! handling, `ValueOutOfRange` for zero-valued modifiers, and a
//! clearer "must be a positive integer" message via
//! `parse_positive_integer`.

mod support {
    pub mod parser_hardening;
}

use proptest::prelude::*;
use reddb_server::storage::query::parser::{self, ParseError, ParserLimits};
use support::parser_hardening::{
    self as harness, assert_no_panic_on, corpus::probabilistic_adversarial_inputs,
    probabilistic_grammar, HardenedParser,
};

/// `HardenedParser` shim around the probabilistic surface. Funnels
/// into the standard parser entry point — what makes this distinct
/// from the SQL / migration / queue shims is that the property +
/// snapshot suites below only feed probabilistic-shaped inputs.
pub struct ProbabilisticParser;

impl HardenedParser for ProbabilisticParser {
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
fn probabilistic_parser_does_not_panic_on_adversarial_corpus() {
    // Bigger stack: a couple of corpus entries probe oversized-input
    // limits and the default 2 MiB test thread stack runs them too
    // close to the line.
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            for (name, input) in probabilistic_adversarial_inputs() {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    assert_no_panic_on::<ProbabilisticParser>(&input);
                }));
                if result.is_err() {
                    panic!("probabilistic adversarial corpus entry {} panicked", name);
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

    /// Generated CREATE / DROP envelopes parse cleanly across all
    /// three structures (HLL / SKETCH / FILTER).
    #[test]
    fn proptest_create_drop_roundtrips(s in probabilistic_grammar::create_drop_stmt()) {
        harness::roundtrip_property::<ProbabilisticParser>(&s);
        prop_assert!(
            ProbabilisticParser::parse(&s).is_ok(),
            "create/drop did not parse: {}", s
        );
    }

    /// Generated HLL operational shapes (ADD / COUNT / MERGE / INFO)
    /// parse cleanly.
    #[test]
    fn proptest_hll_op_roundtrips(s in probabilistic_grammar::hll_op_stmt()) {
        harness::roundtrip_property::<ProbabilisticParser>(&s);
        prop_assert!(
            ProbabilisticParser::parse(&s).is_ok(),
            "hll op did not parse: {}", s
        );
    }

    /// Generated SKETCH operational shapes (ADD / COUNT / MERGE /
    /// INFO) parse cleanly.
    #[test]
    fn proptest_sketch_op_roundtrips(s in probabilistic_grammar::sketch_op_stmt()) {
        harness::roundtrip_property::<ProbabilisticParser>(&s);
        prop_assert!(
            ProbabilisticParser::parse(&s).is_ok(),
            "sketch op did not parse: {}", s
        );
    }

    /// Generated FILTER operational shapes (ADD / CHECK / DELETE /
    /// COUNT / INFO) parse cleanly. Pinned independently because
    /// `DELETE` is the only delete-style sub-command in the
    /// probabilistic surface and the parser dispatches on the
    /// reserved `Token::Delete`.
    #[test]
    fn proptest_filter_op_roundtrips(s in probabilistic_grammar::filter_op_stmt()) {
        harness::roundtrip_property::<ProbabilisticParser>(&s);
        prop_assert!(
            ProbabilisticParser::parse(&s).is_ok(),
            "filter op did not parse: {}", s
        );
    }

    /// Generated `WIDTH` (sketch) and `CAPACITY` (filter) modifier
    /// shapes parse cleanly. Pinned as its own strategy so a
    /// regression in the modifier shrinks directly to the modifier
    /// keyword rather than a fuzzy whole-statement diff.
    #[test]
    fn proptest_modifier_roundtrips(s in probabilistic_grammar::modifier_stmt()) {
        harness::roundtrip_property::<ProbabilisticParser>(&s);
        prop_assert!(
            ProbabilisticParser::parse(&s).is_ok(),
            "modifier did not parse: {}", s
        );
    }

    /// Arbitrary suffix after a probabilistic prefix never panics —
    /// `Err` is fine, panic is not.
    #[test]
    fn proptest_probabilistic_arbitrary_suffix_no_panic(
        prefix in prop_oneof![
            Just("CREATE HLL ".to_string()),
            Just("CREATE SKETCH ".to_string()),
            Just("CREATE FILTER ".to_string()),
            Just("HLL ADD ".to_string()),
            Just("HLL COUNT ".to_string()),
            Just("SKETCH ADD ".to_string()),
            Just("FILTER ADD ".to_string()),
            Just("FILTER CHECK ".to_string()),
            Just("FILTER DELETE ".to_string()),
            Just("DROP FILTER ".to_string()),
        ],
        suffix in ".{0,512}",
    ) {
        let s = format!("{}{}", prefix, suffix);
        harness::roundtrip_property::<ProbabilisticParser>(&s);
    }

    /// Tighter limits always refuse oversized probabilistic inputs.
    /// Pins the DoS-cap contract: a 64-byte cap rejects a long
    /// trailing identifier even though the prefix itself fits.
    #[test]
    fn proptest_probabilistic_input_size_limit_enforced(len in 200usize..2000) {
        let limits = ParserLimits {
            max_input_bytes: 64,
            ..ParserLimits::default()
        };
        let suffix = "x".repeat(len);
        let input = format!("CREATE FILTER {} CAPACITY 100", suffix);
        let r = ProbabilisticParser::parse_with_limits(&input, limits);
        prop_assert!(r.is_err(), "oversized probabilistic input must error");
    }
}

// ---- happy-path regression tests --------------------------------
//
// 5–10 hand-rolled positive tests pin the documented happy-path
// shapes so a future grammar tweak that breaks one of these
// surfaces as a precise failure rather than a fuzzy proptest
// shrink.

use reddb_server::storage::query::ast::{ProbabilisticCommand, QueryExpr};

fn parse_query(input: &str) -> QueryExpr {
    parser::parse(input)
        .unwrap_or_else(|e| panic!("expected ok for {input:?}, got error: {e}"))
        .query
}

#[test]
fn happy_create_hll_bare_parses() {
    let q = parse_query("CREATE HLL visitors");
    match q {
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::CreateHll {
            name,
            precision,
            if_not_exists,
        }) => {
            assert_eq!(name, "visitors");
            assert_eq!(precision, 14);
            assert!(!if_not_exists);
        }
        other => panic!("expected CreateHll, got {other:?}"),
    }
}

#[test]
fn happy_create_hll_if_not_exists_parses() {
    let q = parse_query("CREATE HLL IF NOT EXISTS visitors");
    match q {
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::CreateHll {
            if_not_exists, ..
        }) => assert!(if_not_exists),
        other => panic!("expected CreateHll, got {other:?}"),
    }
}

#[test]
fn happy_create_hll_precision_parses() {
    let q = parse_query("CREATE HLL visitors PRECISION 14");
    match q {
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::CreateHll {
            name,
            precision,
            if_not_exists,
        }) => {
            assert_eq!(name, "visitors");
            assert_eq!(precision, 14);
            assert!(!if_not_exists);
        }
        other => panic!("expected CreateHll, got {other:?}"),
    }
}

#[test]
fn happy_hll_add_collects_string_elements() {
    let q = parse_query("HLL ADD visitors 'alice' 'bob' 'carol'");
    match q {
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::HllAdd { name, elements }) => {
            assert_eq!(name, "visitors");
            assert_eq!(elements, vec!["alice", "bob", "carol"]);
        }
        other => panic!("expected HllAdd, got {other:?}"),
    }
}

#[test]
fn happy_hll_count_multi_name_parses() {
    let q = parse_query("HLL COUNT visitors_a visitors_b visitors_c");
    match q {
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::HllCount { names }) => {
            assert_eq!(names, vec!["visitors_a", "visitors_b", "visitors_c"]);
        }
        other => panic!("expected HllCount, got {other:?}"),
    }
}

#[test]
fn happy_create_sketch_with_width_parses() {
    let q = parse_query("CREATE SKETCH events WIDTH 5000");
    match q {
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::CreateSketch {
            name,
            width,
            depth,
            if_not_exists,
        }) => {
            assert_eq!(name, "events");
            assert_eq!(width, 5000);
            // DEPTH defaults to 5 when the modifier is omitted.
            assert_eq!(depth, 5, "default depth pinned at 5");
            assert!(!if_not_exists);
        }
        other => panic!("expected CreateSketch, got {other:?}"),
    }
}

#[test]
fn happy_sketch_add_with_count_parses() {
    let q = parse_query("SKETCH ADD events 'click' 7");
    match q {
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::SketchAdd {
            name,
            element,
            count,
        }) => {
            assert_eq!(name, "events");
            assert_eq!(element, "click");
            assert_eq!(count, 7);
        }
        other => panic!("expected SketchAdd, got {other:?}"),
    }
}

#[test]
fn happy_sketch_add_default_count_is_one() {
    let q = parse_query("SKETCH ADD events 'click'");
    match q {
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::SketchAdd { count, .. }) => {
            assert_eq!(count, 1, "default count pinned at 1");
        }
        other => panic!("expected SketchAdd, got {other:?}"),
    }
}

#[test]
fn happy_create_filter_with_capacity_parses() {
    let q = parse_query("CREATE FILTER seen CAPACITY 200000");
    match q {
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::CreateFilter {
            name,
            capacity,
            if_not_exists,
        }) => {
            assert_eq!(name, "seen");
            assert_eq!(capacity, 200_000);
            assert!(!if_not_exists);
        }
        other => panic!("expected CreateFilter, got {other:?}"),
    }
}

#[test]
fn happy_create_filter_default_capacity_is_100k() {
    let q = parse_query("CREATE FILTER seen");
    match q {
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::CreateFilter {
            capacity, ..
        }) => {
            assert_eq!(capacity, 100_000, "default capacity pinned at 100k");
        }
        other => panic!("expected CreateFilter, got {other:?}"),
    }
}

#[test]
fn happy_filter_add_check_delete_roundtrip() {
    // ADD
    match parse_query("FILTER ADD seen 'user-42'") {
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::FilterAdd { name, element }) => {
            assert_eq!(name, "seen");
            assert_eq!(element, "user-42");
        }
        other => panic!("expected FilterAdd, got {other:?}"),
    }
    // CHECK
    match parse_query("FILTER CHECK seen 'user-42'") {
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::FilterCheck { name, element }) => {
            assert_eq!(name, "seen");
            assert_eq!(element, "user-42");
        }
        other => panic!("expected FilterCheck, got {other:?}"),
    }
    // DELETE
    match parse_query("FILTER DELETE seen 'user-42'") {
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::FilterDelete { name, element }) => {
            assert_eq!(name, "seen");
            assert_eq!(element, "user-42");
        }
        other => panic!("expected FilterDelete, got {other:?}"),
    }
}

#[test]
fn happy_filter_count_and_drop_filter_if_exists() {
    match parse_query("FILTER COUNT seen") {
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::FilterCount { name }) => {
            assert_eq!(name, "seen");
        }
        other => panic!("expected FilterCount, got {other:?}"),
    }
    match parse_query("DROP FILTER IF EXISTS seen") {
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::DropFilter { name, if_exists }) => {
            assert_eq!(name, "seen");
            assert!(if_exists);
        }
        other => panic!("expected DropFilter, got {other:?}"),
    }
}

// ---- regression guards (originally FIXME pins) ------------------
//
// Phase B / issue #115 landed the parser fixes; the tests below were
// flipped from `#[ignore]` FIXME pins into live guards.

/// Regression guard for #105-followup-1: `Token::Depth` shadowed the
/// `DEPTH` modifier and `consume_ident_ci("DEPTH")` never matched.
/// The fix wires a typed `consume(&Token::Depth)` arm so
/// `CREATE SKETCH name DEPTH n` parses cleanly.
#[test]
fn sketch_depth_clause_parses() {
    let parsed = parser::parse("CREATE SKETCH events DEPTH 5")
        .expect("DEPTH should be accepted as a sketch modifier");
    match parsed.query {
        reddb_server::storage::query::ast::QueryExpr::ProbabilisticCommand(
            ProbabilisticCommand::CreateSketch { width, depth, .. },
        ) => {
            assert_eq!(width, 1000, "default WIDTH stays at 1000");
            assert_eq!(depth, 5, "user-supplied DEPTH lands at 5");
        }
        other => panic!("expected CreateSketch, got {other:?}"),
    }
}

/// Regression guard for #105-followup-2: `CAPACITY 0` is degenerate
/// (a Cuckoo Filter with zero buckets cannot store anything). The
/// parser now rejects it with a `ValueOutOfRange` error.
#[test]
fn filter_capacity_zero_rejected() {
    let r = parser::parse("CREATE FILTER seen CAPACITY 0");
    assert!(r.is_err(), "CAPACITY=0 should be rejected as degenerate");
}

/// Regression guard for #105-followup-3: `WIDTH 0` is degenerate.
/// Same `ValueOutOfRange` shape as `CAPACITY 0`.
#[test]
fn sketch_width_zero_rejected() {
    let r = parser::parse("CREATE SKETCH events WIDTH 0");
    assert!(r.is_err(), "WIDTH=0 should be rejected as degenerate");
}

/// Regression guard for #105-followup-4: negative integer modifiers
/// now surface a clearer "must be a positive integer" message via
/// the typed `parse_positive_integer` helper.
#[test]
fn filter_capacity_negative_surfaces_positive_integer_error() {
    let r = parser::parse("CREATE FILTER seen CAPACITY -1");
    match r {
        Err(e) => {
            assert!(
                format!("{e}")
                    .to_lowercase()
                    .contains("must be a positive integer"),
                "expected 'must be a positive integer' message, got: {e}"
            );
        }
        Ok(_) => panic!("CAPACITY -1 should not parse successfully"),
    }
}
