//! Adversarial-input fixtures.
//!
//! Each entry is an `(name, input)` pair. The same corpus is
//! consumed by:
//!   - the panic-safety property tests in
//!     `tests/parser_hardening.rs`
//!   - the fuzz seed corpus loaded by `fuzz_targets/sql_parser.rs`
//!
//! Adding a regression case here automatically extends both
//! safety nets.

/// Adversarial inputs that historically (or theoretically) trip
/// recursion / memory paths. None of these should panic; all
/// should either parse or return an `Err`.
pub fn adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        ("empty", String::new()),
        ("only_whitespace", "    \n\t  ".to_string()),
        (
            "deep_parens_50",
            format!("SELECT {}1{} FROM t", "(".repeat(50), ")".repeat(50),),
        ),
        (
            "deep_parens_500",
            format!("SELECT {}1{} FROM t", "(".repeat(500), ")".repeat(500),),
        ),
        (
            "deep_not_chain",
            format!("SELECT * FROM t WHERE {} a = 1", "NOT ".repeat(500),),
        ),
        (
            "long_identifier",
            format!("SELECT * FROM {}", "x".repeat(10_000),),
        ),
        ("oversized_input", "a".repeat(2 * 1024 * 1024)),
        ("unbalanced_parens", "SELECT (((1 FROM t".to_string()),
        ("dangling_comma", "SELECT a, b, FROM t".to_string()),
        ("missing_from", "SELECT x WHERE y = 1".to_string()),
        ("eof_mid_stmt", "SELECT * FROM".to_string()),
        ("garbage_bytes", "@#$%^&*()_+|}{:?><".to_string()),
        (
            "invalid_escape_in_string",
            r"SELECT '\\xff' FROM t".to_string(),
        ),
        ("leading_number_ident", "SELECT 1abc FROM t".to_string()),
        ("nul_byte", "SELECT * FROM t\0".to_string()),
        (
            "very_long_string_lit",
            format!("SELECT '{}' FROM t", "x".repeat(100_000),),
        ),
    ]
}

/// Adversarial inputs that target the graph DSL surface (issue #99).
/// These exercise `parse_match_query`, `parse_graph_pattern`,
/// `parse_path_query`, and `parse_graph_command` entry points. Each
/// entry must either parse cleanly or surface a `ParseError`; an
/// unwind panic is the only observable regression.
pub fn graph_dsl_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        ("graph_match_eof", "MATCH".to_string()),
        ("graph_match_open_paren_eof", "MATCH (".to_string()),
        ("graph_match_open_alias_eof", "MATCH (a".to_string()),
        ("graph_match_no_return", "MATCH (a)-[r]->(b)".to_string()),
        (
            "graph_match_unbalanced_bracket",
            "MATCH (a)-[r:KNOWS->(b) RETURN a".to_string(),
        ),
        (
            "graph_match_unbalanced_brace",
            "MATCH (a {name: 'x') RETURN a".to_string(),
        ),
        (
            "graph_match_dangling_props_comma",
            "MATCH (a:person {name: 'x',}) RETURN a".to_string(),
        ),
        (
            "graph_match_dangling_return_comma",
            "MATCH (a)-[r]->(b) RETURN a,".to_string(),
        ),
        (
            "graph_match_bad_var_length",
            "MATCH (a)-[r*1..]->(b) RETURN a".to_string(),
        ),
        (
            "graph_match_var_length_no_max",
            "MATCH (a)-[r*..3]->(b) RETURN a".to_string(),
        ),
        (
            "graph_match_deep_chain",
            // 200 hops of `(x)-[]->` to stress the depth tracker.
            format!(
                "MATCH (a){} RETURN a",
                "-[]->(x)".repeat(200),
            ),
        ),
        (
            "graph_match_long_alias",
            format!("MATCH ({}) RETURN a", "a".repeat(10_000)),
        ),
        (
            "graph_match_oversized",
            format!("MATCH (a) RETURN a {}", " ".repeat(2 * 1024 * 1024)),
        ),
        (
            "graph_match_nul_byte",
            "MATCH (a)-[r]->(b) RETURN a\0".to_string(),
        ),
        ("graph_match_garbage", "MATCH @#$%".to_string()),
        // PATH FROM ... TO ... surface
        ("graph_path_eof", "PATH".to_string()),
        ("graph_path_no_to", "PATH FROM host('x')".to_string()),
        ("graph_path_garbage_via", "PATH FROM host('a') TO host('b') VIA @#$%".to_string()),
        // GRAPH command surface
        ("graph_cmd_eof", "GRAPH".to_string()),
        ("graph_cmd_unknown_subcmd", "GRAPH NONESUCH".to_string()),
        ("graph_neighborhood_no_src", "GRAPH NEIGHBORHOOD".to_string()),
        (
            "graph_shortest_path_no_to",
            "GRAPH SHORTEST_PATH 'a'".to_string(),
        ),
        (
            "graph_traverse_garbage_strategy",
            "GRAPH TRAVERSE 'a' STRATEGY".to_string(),
        ),
        // Phase A: CREATE NODE / CREATE EDGE shapes do not exist in
        // the SQL grammar (they ship as API calls). The parser must
        // surface a Syntax error rather than panicking.
        (
            "graph_create_node_attempt",
            "CREATE NODE (a:person {name: 'x'})".to_string(),
        ),
        (
            "graph_create_edge_attempt",
            "CREATE EDGE (a)-[:KNOWS]->(b)".to_string(),
        ),
    ]
}

/// Adversarial inputs that target the migration DSL surface (issue
/// #88). These exercise the `parse_create_migration_body`,
/// `parse_apply_migration`, `parse_rollback_migration_after_keyword`,
/// and `parse_explain_migration_after_keyword` entry points.
pub fn migration_adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        ("migration_eof_after_create", "CREATE MIGRATION".to_string()),
        (
            "migration_eof_after_name",
            "CREATE MIGRATION m1".to_string(),
        ),
        (
            "migration_eof_after_depends",
            "CREATE MIGRATION m1 DEPENDS ON".to_string(),
        ),
        (
            "migration_dangling_depends_comma",
            "CREATE MIGRATION m1 DEPENDS ON a, AS CREATE TABLE t (id INTEGER)".to_string(),
        ),
        (
            "migration_apply_eof",
            "APPLY MIGRATION".to_string(),
        ),
        (
            "migration_rollback_eof",
            "ROLLBACK MIGRATION".to_string(),
        ),
        (
            "migration_explain_eof",
            "EXPLAIN MIGRATION".to_string(),
        ),
        (
            "migration_apply_for_no_tenant",
            "APPLY MIGRATION m1 FOR".to_string(),
        ),
        (
            "migration_long_name",
            format!("CREATE MIGRATION {} AS CREATE TABLE t (id INTEGER)", "m".repeat(10_000)),
        ),
        (
            "migration_deep_paren_body",
            format!(
                "CREATE MIGRATION m1 AS SELECT {}1{} FROM t",
                "(".repeat(500),
                ")".repeat(500),
            ),
        ),
        (
            "migration_oversized_body",
            format!("CREATE MIGRATION m1 AS {}", "a".repeat(2 * 1024 * 1024)),
        ),
        (
            "migration_nul_byte",
            "CREATE MIGRATION m1 AS CREATE TABLE t (id INTEGER)\0".to_string(),
        ),
        (
            "migration_garbage",
            "CREATE MIGRATION @#$%".to_string(),
        ),
    ]
}
