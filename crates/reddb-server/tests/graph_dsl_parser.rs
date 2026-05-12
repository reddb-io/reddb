//! Parser hardening test suite for the Graph DSL (issue #99).
//!
//! Reuses the `tests/support/parser_hardening` harness from #87 to
//! cover the `MATCH` pattern surface, multi-hop pattern paths, the
//! optional `WHERE` slot inside `MATCH`, the `PATH FROM ... TO ...`
//! query, and the `GRAPH ...` traversal commands. The Graph DSL is
//! reached through the same `parser::parse` entry point as SQL —
//! `MATCH` / `PATH` / `GRAPH` are dispatched in
//! `parse_frontend_statement` — so `ParserLimits` cascade
//! automatically. This file pins the contract.
//!
//! Phase A constraint (issue #99): tests-only. If a property test
//! flushes out a parser bug, pin the broken behaviour with a
//! `// FIXME:` comment and file a follow-up — do *not* modify
//! `lexer.rs` / `parser/` to "fix" it from inside this PR.

mod support {
    pub mod parser_hardening;
}

use proptest::prelude::*;
use reddb_server::storage::query::parser::{self, ParseError, ParserLimits};
use support::parser_hardening::{
    self as harness, assert_no_panic_on, corpus::graph_dsl_adversarial_inputs, graph_dsl_grammar,
    HardenedParser,
};

/// `HardenedParser` shim around the Graph DSL surface. The graph
/// parser shares the top-level entry point with the rest of the
/// SQL-flavoured grammar, so the shim simply funnels into
/// `parser::parse` — what makes this distinct from the SQL shim is
/// the property + snapshot suites below, which only feed graph-
/// shaped inputs.
pub struct GraphDslParser;

impl HardenedParser for GraphDslParser {
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
fn graph_dsl_parser_does_not_panic_on_adversarial_corpus() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            for (name, input) in graph_dsl_adversarial_inputs() {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    assert_no_panic_on::<GraphDslParser>(&input);
                }));
                if result.is_err() {
                    panic!("graph DSL adversarial corpus entry {} panicked", name);
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

    /// Generated single-hop `MATCH (a)-[r]->(b) RETURN a` shapes
    /// parse cleanly.
    #[test]
    fn proptest_match_simple_roundtrips(s in graph_dsl_grammar::match_simple_stmt()) {
        harness::roundtrip_property::<GraphDslParser>(&s);
        prop_assert!(
            GraphDslParser::parse(&s).is_ok(),
            "match simple did not parse: {}", s
        );
    }

    /// Generated multi-hop pattern paths
    /// `(a)-[]->(b)-[]->(c)…` parse cleanly.
    #[test]
    fn proptest_match_pattern_path_roundtrips(
        s in graph_dsl_grammar::match_pattern_path_stmt(),
    ) {
        harness::roundtrip_property::<GraphDslParser>(&s);
        prop_assert!(
            GraphDslParser::parse(&s).is_ok(),
            "match pattern path did not parse: {}", s
        );
    }

    /// Generated `MATCH … WHERE … RETURN …` shapes parse cleanly.
    #[test]
    fn proptest_match_with_where_roundtrips(
        s in graph_dsl_grammar::match_with_where_stmt(),
    ) {
        harness::roundtrip_property::<GraphDslParser>(&s);
        prop_assert!(
            GraphDslParser::parse(&s).is_ok(),
            "match with where did not parse: {}", s
        );
    }

    /// Generated `PATH FROM host('x') TO host('y') …` shapes parse
    /// cleanly.
    #[test]
    fn proptest_path_query_roundtrips(s in graph_dsl_grammar::path_query_stmt()) {
        harness::roundtrip_property::<GraphDslParser>(&s);
        prop_assert!(
            GraphDslParser::parse(&s).is_ok(),
            "path query did not parse: {}", s
        );
    }

    /// Generated `GRAPH NEIGHBORHOOD/TRAVERSE/SHORTEST_PATH/CENTRALITY`
    /// shapes parse cleanly.
    #[test]
    fn proptest_graph_traversal_roundtrips(
        s in graph_dsl_grammar::graph_traversal_stmt(),
    ) {
        harness::roundtrip_property::<GraphDslParser>(&s);
        prop_assert!(
            GraphDslParser::parse(&s).is_ok(),
            "graph traversal did not parse: {}", s
        );
    }

    /// Arbitrary bytes prefixed with a graph DSL keyword never panic.
    /// `Err` is the expected outcome; only an unwind panic is a
    /// regression.
    #[test]
    fn proptest_graph_arbitrary_suffix_no_panic(
        prefix in prop_oneof![
            Just("MATCH ".to_string()),
            Just("PATH ".to_string()),
            Just("GRAPH ".to_string()),
        ],
        suffix in ".{0,512}",
    ) {
        let s = format!("{}{}", prefix, suffix);
        harness::roundtrip_property::<GraphDslParser>(&s);
    }

    /// `CREATE NODE …` shapes are not part of the SQL-side graph
    /// grammar (the operation lives behind API entry points). The
    /// parser must Err but never panic.
    #[test]
    fn proptest_create_node_attempt_no_panic(
        s in graph_dsl_grammar::create_node_attempt_stmt(),
    ) {
        harness::roundtrip_property::<GraphDslParser>(&s);
    }

    /// `CREATE EDGE …` shapes are not part of the SQL-side graph
    /// grammar — same caveat as `CREATE NODE`. Must Err, must not
    /// panic.
    #[test]
    fn proptest_create_edge_attempt_no_panic(
        s in graph_dsl_grammar::create_edge_attempt_stmt(),
    ) {
        harness::roundtrip_property::<GraphDslParser>(&s);
    }

    /// Tighter limits always refuse oversized graph DSL inputs.
    #[test]
    fn proptest_graph_input_size_limit_enforced(len in 200usize..2000) {
        let limits = ParserLimits {
            max_input_bytes: 64,
            ..ParserLimits::default()
        };
        let body = "x".repeat(len);
        let input = format!("MATCH (a) RETURN {}", body);
        let r = GraphDslParser::parse_with_limits(&input, limits);
        prop_assert!(r.is_err(), "oversized graph input must error");
    }
}

// ---- happy-path regression tests --------------------------------
//
// Each test below pins a concrete user-visible Graph DSL shape so
// future grammar tweaks that silently break a documented example
// surface as a test failure (with the exact AST shape printed),
// not a runtime regression three layers deep.

use reddb_server::storage::query::ast::{EdgeDirection, GraphCommand, NodeSelector, QueryExpr};

fn parse_query(input: &str) -> QueryExpr {
    parser::parse(input)
        .unwrap_or_else(|e| panic!("expected ok for {input:?}, got error: {e}"))
        .query
}

#[test]
fn match_single_hop_outgoing_parses() {
    let q = parse_query("MATCH (a:person)-[r:KNOWS]->(b:person) RETURN a, b");
    match q {
        QueryExpr::Graph(g) => {
            assert_eq!(g.pattern.nodes.len(), 2);
            assert_eq!(g.pattern.edges.len(), 1);
            assert_eq!(g.pattern.nodes[0].alias, "a");
            assert_eq!(g.pattern.nodes[0].node_label.as_deref(), Some("person"));
            assert_eq!(g.pattern.edges[0].direction, EdgeDirection::Outgoing);
            assert_eq!(g.pattern.edges[0].edge_label.as_deref(), Some("knows"));
            assert_eq!(g.return_.len(), 2);
        }
        other => panic!("expected Graph, got {other:?}"),
    }
}

#[test]
fn match_incoming_edge_parses() {
    let q = parse_query("MATCH (a)<-[r:FOLLOWS]-(b:person) RETURN b");
    match q {
        QueryExpr::Graph(g) => {
            assert_eq!(g.pattern.edges.len(), 1);
            assert_eq!(g.pattern.edges[0].direction, EdgeDirection::Incoming);
        }
        other => panic!("expected Graph, got {other:?}"),
    }
}

#[test]
fn match_multi_hop_pattern_path_parses() {
    let q = parse_query(
        "MATCH (a:person)-[:WORKS_AT]->(c:company)-[:LOCATED_IN]->(city) RETURN a, c, city",
    );
    match q {
        QueryExpr::Graph(g) => {
            assert_eq!(g.pattern.nodes.len(), 3);
            assert_eq!(g.pattern.edges.len(), 2);
            assert_eq!(g.return_.len(), 3);
        }
        other => panic!("expected Graph, got {other:?}"),
    }
}

#[test]
fn match_with_where_clause_parses() {
    let q = parse_query(
        "MATCH (a:person)-[r:COLLABORATES]->(b:person) WHERE a.department = 'engineering' \
         RETURN a, b",
    );
    match q {
        QueryExpr::Graph(g) => {
            assert!(g.filter.is_some(), "WHERE filter must be present");
            assert_eq!(
                g.pattern.edges[0].edge_label.as_deref(),
                Some("collaborates")
            );
        }
        other => panic!("expected Graph, got {other:?}"),
    }
}

#[test]
fn match_with_property_filter_parses() {
    let q = parse_query("MATCH (a:person {name: 'alice'}) RETURN a");
    match q {
        QueryExpr::Graph(g) => {
            assert_eq!(g.pattern.nodes[0].properties.len(), 1);
            assert_eq!(g.pattern.nodes[0].properties[0].name, "name");
        }
        other => panic!("expected Graph, got {other:?}"),
    }
}

#[test]
fn match_with_variable_length_edge_parses() {
    let q = parse_query("MATCH (a:person)-[r*1..3]->(b:person) RETURN a, b");
    match q {
        QueryExpr::Graph(g) => {
            assert_eq!(g.pattern.edges[0].min_hops, 1);
            assert_eq!(g.pattern.edges[0].max_hops, 3);
        }
        other => panic!("expected Graph, got {other:?}"),
    }
}

#[test]
fn graph_neighborhood_command_parses() {
    let q = parse_query("GRAPH NEIGHBORHOOD 'alice' DEPTH 2 DIRECTION outgoing");
    match q {
        QueryExpr::GraphCommand(GraphCommand::Neighborhood {
            source,
            depth,
            direction,
        }) => {
            assert_eq!(source, "alice");
            assert_eq!(depth, 2);
            assert_eq!(direction, "outgoing");
        }
        other => panic!("expected Neighborhood, got {other:?}"),
    }
}

#[test]
fn graph_shortest_path_command_parses() {
    let q = parse_query("GRAPH SHORTEST_PATH 'alice' TO 'bob' ALGORITHM dijkstra");
    match q {
        QueryExpr::GraphCommand(GraphCommand::ShortestPath {
            source,
            target,
            algorithm,
            ..
        }) => {
            assert_eq!(source, "alice");
            assert_eq!(target, "bob");
            assert_eq!(algorithm, "dijkstra");
        }
        other => panic!("expected ShortestPath, got {other:?}"),
    }
}

#[test]
fn graph_traverse_command_parses() {
    let q = parse_query("GRAPH TRAVERSE 'alice' STRATEGY bfs DEPTH 3");
    match q {
        QueryExpr::GraphCommand(GraphCommand::Traverse {
            source,
            strategy,
            depth,
            ..
        }) => {
            assert_eq!(source, "alice");
            assert_eq!(strategy, "bfs");
            assert_eq!(depth, 3);
        }
        other => panic!("expected Traverse, got {other:?}"),
    }
}

// Issue #417: docs↔parser drift — documented `GRAPH TRAVERSE FROM '<id>'
// STRATEGY bfs MAX_DEPTH n` form must parse identically to the bare form.
#[test]
fn graph_traverse_from_strategy_max_depth_form_parses() {
    let q = parse_query("GRAPH TRAVERSE FROM 'alice' STRATEGY bfs DIRECTION outgoing MAX_DEPTH 3");
    match q {
        QueryExpr::GraphCommand(GraphCommand::Traverse {
            source,
            strategy,
            depth,
            direction,
        }) => {
            assert_eq!(source, "alice");
            assert_eq!(strategy, "bfs");
            assert_eq!(depth, 3);
            assert_eq!(direction, "outgoing");
        }
        other => panic!("expected Traverse, got {other:?}"),
    }
}

#[test]
fn graph_shortest_path_from_to_form_parses() {
    let q = parse_query("GRAPH SHORTEST_PATH FROM 'a' TO 'b' ALGORITHM dijkstra");
    match q {
        QueryExpr::GraphCommand(GraphCommand::ShortestPath {
            source,
            target,
            algorithm,
            ..
        }) => {
            assert_eq!(source, "a");
            assert_eq!(target, "b");
            assert_eq!(algorithm, "dijkstra");
        }
        other => panic!("expected ShortestPath, got {other:?}"),
    }
}

#[test]
fn path_query_with_via_parses() {
    let q = parse_query("PATH FROM host('a') TO host('b') VIA [:KNOWS, :FOLLOWS]");
    match q {
        QueryExpr::Path(p) => {
            assert!(matches!(p.from, NodeSelector::ById(ref id) if id == "a"));
            assert!(matches!(p.to, NodeSelector::ById(ref id) if id == "b"));
            assert_eq!(p.via.len(), 2);
        }
        other => panic!("expected Path, got {other:?}"),
    }
}
