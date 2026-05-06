//! Parser hardening test suite for the vector-search surface
//! (issue #100).
//!
//! Reuses the `tests/support/parser_hardening` harness from #87 to
//! cover the four shapes called out in the issue:
//!   - `SEARCH SIMILAR [floats…]`
//!   - `SEARCH SIMILAR TEXT '...'`
//!   - `INSERT … WITH AUTO EMBED USING <provider>`
//!   - hybrid search (`SEARCH HYBRID …`, `HYBRID FROM … FUSION …`)
//!
//! Phase A (#100): tests-only. No parser source modifications. Bugs
//! revealed during development are pinned with a `// FIXME: bug —
//! fix in #NN` comment and a follow-up issue.

mod support {
    pub mod parser_hardening;
}

use proptest::prelude::*;
use reddb_server::storage::query::parser::{self, ParseError, ParserLimits};
use support::parser_hardening::{
    self as harness, assert_no_panic_on, corpus::vector_search_adversarial_inputs,
    vector_search_grammar, HardenedParser,
};

/// `HardenedParser` shim. The vector-search forms reach the parser
/// through the same top-level `parser::parse` entry point as the SQL
/// and migration shims, so this is mechanically identical to the
/// `SqlParser` / `MigrationParser` shims — what makes it distinct is
/// the property + snapshot suites below, which only feed
/// vector-shaped inputs.
pub struct VectorParser;

impl HardenedParser for VectorParser {
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
fn vector_parser_does_not_panic_on_adversarial_corpus() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            for (name, input) in vector_search_adversarial_inputs() {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    assert_no_panic_on::<VectorParser>(&input);
                }));
                if result.is_err() {
                    panic!("vector-search adversarial corpus entry {} panicked", name);
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

    /// `SEARCH SIMILAR [v1, v2, …] COLLECTION col …` parses cleanly
    /// across dim 1..32 + every combination of optional clauses.
    #[test]
    fn proptest_search_similar_vector_roundtrips(
        s in vector_search_grammar::search_similar_vector_stmt(),
    ) {
        harness::roundtrip_property::<VectorParser>(&s);
        prop_assert!(
            VectorParser::parse(&s).is_ok(),
            "SEARCH SIMILAR vector did not parse: {}", s
        );
    }

    /// `SEARCH SIMILAR TEXT 'q' COLLECTION col …` parses cleanly.
    #[test]
    fn proptest_search_similar_text_roundtrips(
        s in vector_search_grammar::search_similar_text_stmt(),
    ) {
        harness::roundtrip_property::<VectorParser>(&s);
        prop_assert!(
            VectorParser::parse(&s).is_ok(),
            "SEARCH SIMILAR TEXT did not parse: {}", s
        );
    }

    /// `INSERT … WITH AUTO EMBED (…) [USING p] [MODEL '…']` parses
    /// cleanly across 1..3 columns + optional provider + optional
    /// model.
    #[test]
    fn proptest_insert_auto_embed_roundtrips(
        s in vector_search_grammar::insert_auto_embed_stmt(),
    ) {
        harness::roundtrip_property::<VectorParser>(&s);
        prop_assert!(
            VectorParser::parse(&s).is_ok(),
            "INSERT WITH AUTO EMBED did not parse: {}", s
        );
    }

    /// `VECTOR SEARCH col SIMILAR TO ([…] | 'text') …` parses
    /// cleanly. Covers metric / threshold / limit + both vector
    /// sources.
    #[test]
    fn proptest_vector_search_roundtrips(
        s in vector_search_grammar::vector_search_stmt(),
    ) {
        harness::roundtrip_property::<VectorParser>(&s);
        prop_assert!(
            VectorParser::parse(&s).is_ok(),
            "VECTOR SEARCH did not parse: {}", s
        );
    }

    /// `SEARCH HYBRID [SIMILAR [...]] [TEXT '...'] COLLECTION col`
    /// parses across all three (vector-only, text-only, both)
    /// modes.
    #[test]
    fn proptest_search_hybrid_roundtrips(
        s in vector_search_grammar::search_hybrid_stmt(),
    ) {
        harness::roundtrip_property::<VectorParser>(&s);
        prop_assert!(
            VectorParser::parse(&s).is_ok(),
            "SEARCH HYBRID did not parse: {}", s
        );
    }

    /// `HYBRID FROM table VECTOR SEARCH col SIMILAR TO […] FUSION strategy`
    /// parses across every fusion-strategy shape.
    #[test]
    fn proptest_hybrid_from_roundtrips(
        s in vector_search_grammar::hybrid_from_stmt(),
    ) {
        harness::roundtrip_property::<VectorParser>(&s);
        prop_assert!(
            VectorParser::parse(&s).is_ok(),
            "HYBRID FROM did not parse: {}", s
        );
    }

    /// Arbitrary suffix glued to a vector-search keyword prefix
    /// never panics. `Err` is fine.
    #[test]
    fn proptest_vector_arbitrary_suffix_no_panic(
        prefix in prop_oneof![
            Just("SEARCH SIMILAR ".to_string()),
            Just("SEARCH SIMILAR TEXT ".to_string()),
            Just("SEARCH HYBRID ".to_string()),
            Just("VECTOR SEARCH ".to_string()),
            Just("HYBRID FROM ".to_string()),
            Just("INSERT INTO t (a) VALUES ('x') WITH AUTO EMBED ".to_string()),
        ],
        suffix in ".{0,512}",
    ) {
        let s = format!("{}{}", prefix, suffix);
        harness::roundtrip_property::<VectorParser>(&s);
    }

    /// Tighter `max_input_bytes` always refuses oversized vector
    /// queries. The pathologically wide vector literal would
    /// otherwise generate an O(dim) parse — the input-size cap kicks
    /// in first.
    #[test]
    fn proptest_vector_input_size_limit_enforced(
        dim in 200usize..2000,
    ) {
        let limits = ParserLimits {
            max_input_bytes: 64,
            ..ParserLimits::default()
        };
        let body: Vec<String> = (0..dim).map(|_| "0.1".to_string()).collect();
        let input = format!("SEARCH SIMILAR [{}] COLLECTION c", body.join(","));
        let r = VectorParser::parse_with_limits(&input, limits);
        prop_assert!(r.is_err(), "oversized vector input must error");
    }
}
