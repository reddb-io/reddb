//! Parser hardening test suite for the ASK / AI-extension surface
//! (issue #101).
//!
//! Reuses the `tests/support/parser_hardening` harness from #87 to
//! cover the `ASK '<question>' [USING …] [MODEL …] [DEPTH n]
//! [LIMIT n] [COLLECTION col]` and the `SEARCH CONTEXT '<query>' …`
//! shapes. Both reach the production parser through the standard
//! `reddb_server::storage::query::parser::parse` entry point so
//! `ParserLimits` cascade automatically.
//!
//! Phase A — tests-only. Bugs uncovered here are pinned with
//! `FIXME(#101): …` in the failing assertion; follow-up issues fix
//! the parser source without touching this file.

mod support {
    pub mod parser_hardening;
}

use proptest::prelude::*;
use reddb_server::storage::query::ast::{QueryExpr, SearchCommand};
use reddb_server::storage::query::parser::{self, ParseError, ParserLimits};
use support::parser_hardening::{
    self as harness, ask_grammar, assert_no_panic_on, corpus::ask_adversarial_inputs,
    HardenedParser,
};

/// `HardenedParser` shim around the ASK / SEARCH CONTEXT surface.
/// Both shapes share the top-level entry point with the rest of the
/// SQL grammar, so the shim simply funnels into `parser::parse` —
/// the property + snapshot suites below are what concentrate the
/// coverage on the AI extension subset.
pub struct AskParser;

impl HardenedParser for AskParser {
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
fn ask_parser_does_not_panic_on_adversarial_corpus() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            for (name, input) in ask_adversarial_inputs() {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    assert_no_panic_on::<AskParser>(&input);
                }));
                if result.is_err() {
                    panic!("ask adversarial corpus entry {} panicked", name);
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

    /// Strategy 1: full ASK shape with all clauses optional
    /// (USING excluded — see strategy 2 + FIXME(#101)). Generated
    /// input must parse cleanly.
    #[test]
    fn proptest_ask_full_shape_roundtrips(s in ask_grammar::ask_stmt()) {
        harness::roundtrip_property::<AskParser>(&s);
        prop_assert!(
            AskParser::parse(&s).is_ok(),
            "ask full shape did not parse: {}", s
        );
    }

    /// Strategy 2: ASK with the USING-provider clause concentrated.
    /// Now that #101 / #108 fixed `parse_ask_query` to match `USING`
    /// via `Token::Using`, this property asserts the generated input
    /// parses cleanly — same shape as the other strategies.
    #[test]
    fn proptest_ask_using_provider_roundtrips(
        s in ask_grammar::ask_using_provider_stmt(),
    ) {
        harness::roundtrip_property::<AskParser>(&s);
        prop_assert!(
            AskParser::parse(&s).is_ok(),
            "ask USING <provider> did not parse: {}", s
        );
    }

    /// Strategy 3: ASK with the MODEL string-literal clause
    /// concentrated. Pins the `MODEL '<name>'` slot.
    #[test]
    fn proptest_ask_model_ident_roundtrips(s in ask_grammar::ask_model_ident_stmt()) {
        harness::roundtrip_property::<AskParser>(&s);
        prop_assert!(
            AskParser::parse(&s).is_ok(),
            "ask MODEL '<name>' did not parse: {}", s
        );
    }

    /// Strategy 4: SEARCH CONTEXT with optional FIELD / COLLECTION /
    /// LIMIT / DEPTH clauses.
    #[test]
    fn proptest_search_context_roundtrips(s in ask_grammar::search_context_stmt()) {
        harness::roundtrip_property::<AskParser>(&s);
        prop_assert!(
            AskParser::parse(&s).is_ok(),
            "search context did not parse: {}", s
        );
    }

    /// Strategy 5: ASK with depth + scope (LIMIT) numeric ranges.
    #[test]
    fn proptest_ask_depth_scope_roundtrips(s in ask_grammar::ask_depth_scope_stmt()) {
        harness::roundtrip_property::<AskParser>(&s);
        prop_assert!(
            AskParser::parse(&s).is_ok(),
            "ask DEPTH/LIMIT did not parse: {}", s
        );
    }

    /// Clean break (ADR 0068, #1751): the removed `AS RQL` / `EXECUTE`
    /// clauses must never round-trip — the parser rejects them (didactically)
    /// rather than accepting the dead grammar. Pins the removal across the
    /// whole question-string corpus.
    #[test]
    fn proptest_ask_removed_clauses_reject(s in ask_grammar::ask_removed_clause_stmt()) {
        harness::roundtrip_property::<AskParser>(&s);
        prop_assert!(
            AskParser::parse(&s).is_err(),
            "removed AS RQL / EXECUTE clause must not parse: {}", s
        );
    }

    /// Arbitrary-bytes suffix on each AI keyword: never panic.
    #[test]
    fn proptest_ask_arbitrary_suffix_no_panic(
        prefix in prop_oneof![
            Just("ASK ".to_string()),
            Just("ASK 'q' ".to_string()),
            Just("SEARCH CONTEXT ".to_string()),
            Just("SEARCH CONTEXT 'q' ".to_string()),
        ],
        suffix in ".{0,512}",
    ) {
        let s = format!("{}{}", prefix, suffix);
        harness::roundtrip_property::<AskParser>(&s);
    }

    /// Tighter input-size limit refuses oversized ASK questions. The
    /// 64-byte ceiling is well below the expanded input length so the
    /// parser must reject before the lexer gets to the question body.
    #[test]
    fn proptest_ask_input_size_limit_enforced(len in 200usize..2000) {
        let limits = ParserLimits {
            max_input_bytes: 64,
            ..ParserLimits::default()
        };
        let body = "x".repeat(len);
        let input = format!("ASK '{}'", body);
        let r = AskParser::parse_with_limits(&input, limits);
        prop_assert!(r.is_err(), "oversized ASK question must error");
    }
}

// ---- happy-path regression tests --------------------------------
//
// These pin the documented ASK / SEARCH CONTEXT shapes against
// future grammar drift. The `parse_query` helper unwraps the
// `QueryWithCte` envelope so each test asserts directly against the
// AST node.

fn parse_query(input: &str) -> QueryExpr {
    parser::parse(input)
        .unwrap_or_else(|e| panic!("expected ok for {input:?}, got error: {e}"))
        .query
}

#[test]
fn ask_minimal_question_parses() {
    let q = parse_query("ASK 'why is the sky blue?'");
    match q {
        QueryExpr::Ask(ask) => {
            assert_eq!(ask.question, "why is the sky blue?");
            assert_eq!(ask.provider, None);
            assert_eq!(ask.model, None);
            assert_eq!(ask.depth, None);
            assert_eq!(ask.limit, None);
            assert_eq!(ask.collection, None);
        }
        other => panic!("expected Ask, got {other:?}"),
    }
}

#[test]
fn ask_with_model_only_parses() {
    let q = parse_query("ASK 'q' MODEL 'gpt-4o-mini'");
    match q {
        QueryExpr::Ask(ask) => {
            assert_eq!(ask.question, "q");
            assert_eq!(ask.model.as_deref(), Some("gpt-4o-mini"));
            assert_eq!(ask.provider, None);
        }
        other => panic!("expected Ask, got {other:?}"),
    }
}

#[test]
fn ask_with_depth_limit_collection_parses() {
    let q = parse_query("ASK 'q' DEPTH 3 LIMIT 25 MIN_SCORE 0.7 COLLECTION docs");
    match q {
        QueryExpr::Ask(ask) => {
            assert_eq!(ask.depth, Some(3));
            assert_eq!(ask.limit, Some(25));
            assert_eq!(ask.min_score, Some(0.7));
            assert_eq!(ask.collection.as_deref(), Some("docs"));
            assert_eq!(ask.provider, None);
        }
        other => panic!("expected Ask, got {other:?}"),
    }
}

#[test]
fn ask_full_chain_without_using_parses() {
    // `USING <provider>` is omitted here; the dedicated
    // `ask_using_provider_parses` test pins the USING clause shape
    // separately. Every other documented clause is exercised here.
    let q = parse_query(
        "ASK 'what happened?' MODEL 'claude-3-5-sonnet' \
         DEPTH 2 LIMIT 50 MIN_SCORE 0.7 COLLECTION events",
    );
    match q {
        QueryExpr::Ask(ask) => {
            assert_eq!(ask.question, "what happened?");
            assert_eq!(ask.model.as_deref(), Some("claude-3-5-sonnet"));
            assert_eq!(ask.depth, Some(2));
            assert_eq!(ask.limit, Some(50));
            assert_eq!(ask.min_score, Some(0.7));
            assert_eq!(ask.collection.as_deref(), Some("events"));
            assert_eq!(ask.provider, None);
        }
        other => panic!("expected Ask, got {other:?}"),
    }
}

/// Regression guard for #101 / #108: `parse_ask_query` now matches
/// `USING` via `Token::Using` (the typed-keyword consumer), so the
/// optional `USING <provider>` clause on `ASK '…'` parses
/// end-to-end. Mirrors the #92 fix that flipped `DEPENDS ON` /
/// `FOR TENANT` / `APPLY MIGRATION` to typed consumers.
#[test]
fn ask_using_provider_parses() {
    let q = parse_query("ASK 'who?' USING openai");
    match q {
        QueryExpr::Ask(ask) => {
            assert_eq!(ask.question, "who?");
            assert_eq!(ask.provider.as_deref(), Some("openai"));
        }
        other => panic!("expected Ask, got {other:?}"),
    }
}

#[test]
fn search_context_minimal_parses_with_defaults() {
    let q = parse_query("SEARCH CONTEXT 'find this'");
    match q {
        QueryExpr::SearchCommand(SearchCommand::Context {
            query,
            field,
            collection,
            limit,
            depth,
            ..
        }) => {
            assert_eq!(query, "find this");
            assert_eq!(field, None);
            assert_eq!(collection, None);
            // Default limit and depth are documented in
            // `parse_search_context` (limit=25, depth=1).
            assert_eq!(limit, 25);
            assert_eq!(depth, 1);
        }
        other => panic!("expected SearchCommand::Context, got {other:?}"),
    }
}

#[test]
fn search_context_full_clause_chain_parses() {
    let q = parse_query(
        "SEARCH CONTEXT '000.000.000-00' FIELD cpf COLLECTION customers LIMIT 50 DEPTH 2",
    );
    match q {
        QueryExpr::SearchCommand(SearchCommand::Context {
            query,
            field,
            collection,
            limit,
            depth,
            ..
        }) => {
            assert_eq!(query, "000.000.000-00");
            assert_eq!(field.as_deref(), Some("cpf"));
            assert_eq!(collection.as_deref(), Some("customers"));
            assert_eq!(limit, 50);
            assert_eq!(depth, 2);
        }
        other => panic!("expected SearchCommand::Context, got {other:?}"),
    }
}

#[test]
fn ask_plan_clause_sets_plan_only() {
    // Clean break (ADR 0068, #1751): `PLAN` returns the typed plan and
    // candidate query without executing or synthesizing.
    let q = parse_query("ASK 'who owns passport FDD-12313?' PLAN");
    match q {
        QueryExpr::Ask(ask) => {
            assert!(ask.plan_only, "PLAN should set plan_only");
            assert!(!ask.explain);
        }
        other => panic!("expected Ask, got {other:?}"),
    }
}

#[test]
fn ask_plan_specified_twice_errors() {
    let err = parser::parse("ASK 'q' PLAN PLAN").expect_err("double PLAN must error");
    assert!(
        err.to_string().contains("PLAN specified more than once"),
        "got: {err}"
    );
}

#[test]
fn ask_as_rql_clause_is_removed_with_didactic_error() {
    // `AS RQL` was removed: the didactic error must name the `PLAN`
    // replacement so callers can migrate without reading the changelog.
    let err =
        parser::parse("ASK 'who owns passport FDD-12313?' AS RQL").expect_err("AS RQL was removed");
    assert!(
        err.to_string().contains("AS RQL was removed") && err.to_string().contains("PLAN"),
        "got: {err}"
    );
}

#[test]
fn ask_execute_clause_is_removed_with_didactic_error() {
    // `EXECUTE` was removed: read-only candidates auto-execute by default;
    // the didactic error must name the `PLAN` inspection replacement.
    let err = parser::parse("ASK 'list travelers' EXECUTE").expect_err("EXECUTE was removed");
    assert!(
        err.to_string().contains("EXECUTE was removed") && err.to_string().contains("PLAN"),
        "got: {err}"
    );
}

// ---- read-only candidate validation seam -------------------------
//
// The deterministic `AS RQL` planner and its LLM inference variant were
// removed in the ADR 0068 clean break (#1751); the planner-first path now
// owns candidate generation. What survives is the shared parser + read-only
// classifier seam, exercised here directly.

mod candidate_validation {
    use reddb_server::runtime::ai::ask_rql_planner::{validate_candidate, CandidateDisposition};

    #[test]
    fn invalid_candidate_is_rejected_by_parser() {
        let err = validate_candidate("not valid rql at all").unwrap_err();
        assert!(err.to_string().contains("invalid RQL candidate"), "{err}");
    }

    #[test]
    fn read_only_select_is_classified_read_only() {
        let out =
            validate_candidate("SELECT * FROM travelers WHERE passport = 'FDD-12313'").unwrap();
        assert_eq!(out.disposition, CandidateDisposition::ReadOnly);
        assert!(out.is_read_only());
    }

    #[test]
    fn mutating_delete_is_classified_mutating() {
        let out = validate_candidate("DELETE FROM travelers WHERE passport = 'FDD-12313'").unwrap();
        assert_eq!(out.disposition, CandidateDisposition::Mutating);
        assert_eq!(out.statement_type, "delete");
    }
}

#[test]
fn ask_lowercase_keyword_parses() {
    // The dispatch path uses `eq_ignore_ascii_case("ASK")`, so the
    // lowercase form must round-trip. Pinning prevents a future
    // tightening of the dispatcher from breaking case-insensitive
    // RQL clients.
    let q = parse_query("ask 'q' depth 3");
    match q {
        QueryExpr::Ask(ask) => {
            assert_eq!(ask.question, "q");
            assert_eq!(ask.depth, Some(3));
        }
        other => panic!("expected Ask, got {other:?}"),
    }
}
