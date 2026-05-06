//! Pinned Graph DSL parse-error snapshots (issue #99).
//!
//! Mirrors `parser_snapshots.rs` and `migration_parser_snapshots.rs`
//! for the graph surface. Each test calls `assert_parse_error_snapshot`
//! on a hand-crafted bad input drawn from the kinds of mistakes a
//! human writing `MATCH` / `PATH` / `GRAPH` / `CREATE NODE` queries
//! actually makes; snapshot files live in `tests/snapshots/`.
//!
//! Phase A constraint (#99): tests-only. If a snapshot reveals that
//! the parser handles a shape badly (panics, swallows the error, or
//! produces a misleading message), the snapshot still pins the
//! current behaviour and a `// FIXME: bug — fix in #NN` comment is
//! left next to the test so the follow-up issue is greppable.
//!
//! Every snapshot test begins with
//! `let _g = secret_redactor::install_redactions();` per #98 — the
//! `snapshot_redaction_lint` integration test fails CI if a
//! committed `*.snap` ever contains an unmasked secret-shaped
//! substring.

mod support {
    pub mod parser_hardening;
}

use reddb_server::storage::query::parser;

use support::parser_hardening::secret_redactor;

/// Parse `input` and format the resulting error for snapshotting.
/// Successful parses render as `UNEXPECTED OK` so a missing error
/// path is visible in the diff.
fn fmt_parse_error(input: &str) -> String {
    match parser::parse(input) {
        Ok(_) => format!("UNEXPECTED OK\ninput: {:?}\n", input),
        Err(e) => format!("input: {:?}\nkind:  {:?}\nerror: {}\n", input, e.kind, e),
    }
}

/// Macro wrapper around `insta::assert_snapshot!` that names the
/// snapshot after the test function and pins the error format. The
/// shared secret redactor is installed at the top of every test
/// scope so a stray credential-shaped substring never reaches the
/// committed `*.snap`.
macro_rules! snap {
    ($name:ident, $input:expr) => {
        #[test]
        fn $name() {
            let _g = secret_redactor::install_redactions();
            insta::assert_snapshot!(stringify!($name), fmt_parse_error($input));
        }
    };
}

// ----- MATCH error scenarios -------------------------------------

snap!(graph_match_eof_after_keyword, "MATCH");
snap!(graph_match_eof_after_open_paren, "MATCH (");
snap!(graph_match_eof_after_alias, "MATCH (a");
snap!(graph_match_missing_return, "MATCH (a)-[r]->(b)");
snap!(graph_match_unbalanced_bracket, "MATCH (a)-[r:KNOWS->(b) RETURN a");
snap!(graph_match_unbalanced_brace, "MATCH (a {name: 'x') RETURN a");
snap!(
    graph_match_dangling_props_comma,
    "MATCH (a:person {name: 'x',}) RETURN a"
);
snap!(
    graph_match_dangling_return_comma,
    "MATCH (a)-[r]->(b) RETURN a,"
);
snap!(graph_match_no_alias, "MATCH (:person) RETURN a");
snap!(
    graph_match_var_length_no_min,
    "MATCH (a)-[r*..3]->(b) RETURN a"
);
snap!(
    graph_match_garbage_after_match,
    "MATCH @#$%"
);
// ----- PATH error scenarios --------------------------------------

snap!(graph_path_eof_after_keyword, "PATH");
snap!(graph_path_missing_to, "PATH FROM host('alice')");
snap!(
    graph_path_via_garbage,
    "PATH FROM host('a') TO host('b') VIA @#$%"
);

// ----- GRAPH command error scenarios -----------------------------

snap!(graph_cmd_eof_after_keyword, "GRAPH");
snap!(graph_cmd_unknown_subcommand, "GRAPH NONESUCH");
snap!(graph_neighborhood_no_source, "GRAPH NEIGHBORHOOD");
snap!(graph_shortest_path_no_target, "GRAPH SHORTEST_PATH 'a'");

// ----- CREATE NODE / CREATE EDGE attempts ------------------------
//
// These shapes do not exist in the SQL-side graph grammar — the
// real CREATE NODE / CREATE EDGE operations ship as API calls
// (`CreateNodeInput` / `CreateEdgeInput`). The parser must surface
// a Syntax error instead of panicking. The snapshot pins the
// current error message so a future grammar tweak that *does*
// implement these forms produces a reviewable diff.
snap!(
    graph_create_node_attempt,
    "CREATE NODE (a:person {name: 'alice'})"
);
snap!(
    graph_create_edge_attempt,
    "CREATE EDGE (a)-[:KNOWS]->(b)"
);
