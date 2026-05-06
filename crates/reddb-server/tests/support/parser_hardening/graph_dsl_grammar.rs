//! Proptest strategies that emit syntactically valid Graph DSL
//! statements (issue #99).
//!
//! Mirrors the layout of `sql_grammar.rs` and `migration_grammar.rs`:
//! each strategy returns a `String` that, when fed back through
//! `parser::parse`, must not panic. Valid-shape strategies must
//! additionally succeed.
//!
//! The graph DSL surface covered here is the parser's read-only
//! Cypher-flavoured grammar (`crates/reddb-server/src/storage/query/
//! parser/graph.rs` + `path.rs` + `graph_commands.rs`):
//!
//!   - `MATCH (a:label)-[r:edge]->(b:label) [WHERE ...] RETURN ...`
//!   - Multi-hop pattern paths `(a)-[r1]->(b)-[r2]->(c)`
//!   - Variable-length edges `-[r*1..3]->`
//!   - Property filters `(a:label {name: 'value'})`
//!   - Optional WHERE inside MATCH
//!   - `PATH FROM selector TO selector [VIA ...] [WHERE ...] [RETURN ...]`
//!   - `GRAPH NEIGHBORHOOD/SHORTEST_PATH/TRAVERSE/...` traversals
//!
//! The grammar does not currently include a `CREATE NODE` /
//! `CREATE EDGE` statement form — those operations are reached
//! through API entry points (`CreateNodeInput`/`CreateEdgeInput`).
//! The `create_node_attempt_stmt` / `create_edge_attempt_stmt`
//! generators emit the *user-attempted* shapes (which the parser
//! must Err on without panicking) so the snapshot suite can pin the
//! error path and the property suite can confirm panic-safety.

use proptest::prelude::*;

/// Identifier suitable for node aliases / property names. Stays well
/// below the `max_identifier_chars` cap.
pub fn ident() -> impl Strategy<Value = String> {
    "id_[a-z0-9_]{0,12}".prop_map(|s| s)
}

/// Node label drawn from a small set of canonical labels recognised
/// by `parse_node_label` plus a free-form identifier so the
/// "forward unknown labels verbatim" path is also exercised.
pub fn node_label() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("person".to_string()),
        Just("host".to_string()),
        Just("service".to_string()),
        Just("user".to_string()),
        Just("domain".to_string()),
        ident(),
    ]
}

/// Edge label drawn from a small set of canonical labels plus a
/// free-form identifier. Mirrors `node_label`.
pub fn edge_label() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("HAS_SERVICE".to_string()),
        Just("CONNECTS_TO".to_string()),
        Just("RELATED_TO".to_string()),
        Just("HAS_USER".to_string()),
        Just("REPORTS_TO".to_string()),
        ident(),
    ]
}

/// Small string literal with no embedded quotes.
pub fn str_lit() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 ]{0,12}".prop_map(|s| format!("'{}'", s))
}

/// Small non-negative integer literal.
pub fn int_lit() -> impl Strategy<Value = String> {
    (0u64..1000u64).prop_map(|n| n.to_string())
}

/// Property literal usable inside `{key: value}` filters and on
/// the RHS of WHERE comparisons.
pub fn prop_value() -> impl Strategy<Value = String> {
    prop_oneof![
        int_lit(),
        str_lit(),
        Just("TRUE".to_string()),
        Just("FALSE".to_string()),
    ]
}

/// `key: value` pair for a brace-property filter.
pub fn prop_pair() -> impl Strategy<Value = String> {
    (ident(), prop_value()).prop_map(|(k, v)| format!("{}: {}", k, v))
}

/// `{k1: v1, k2: v2, ...}` brace property filter (1..3 pairs).
/// Property names are drawn from a numbered pool (`prop_0`,
/// `prop_1`, …) so the parser never sees a duplicate key in the
/// same brace.
pub fn brace_props() -> impl Strategy<Value = String> {
    (1usize..=3usize, proptest::collection::vec(prop_value(), 3)).prop_map(|(n, vals)| {
        let pairs: Vec<String> = (0..n)
            .map(|i| format!("prop_{}: {}", i, vals[i]))
            .collect();
        format!("{{{}}}", pairs.join(", "))
    })
}

/// Node pattern `(alias)`, `(alias:label)`, or
/// `(alias:label {props})`. The alias is required by the grammar.
pub fn node_pattern() -> impl Strategy<Value = String> {
    (
        ident(),
        proptest::option::of(node_label()),
        proptest::option::of(brace_props()),
    )
        .prop_map(|(alias, label, props)| {
            let mut s = format!("({}", alias);
            if let Some(l) = label {
                s.push(':');
                s.push_str(&l);
            }
            if let Some(p) = props {
                s.push(' ');
                s.push_str(&p);
            }
            s.push(')');
            s
        })
}

/// Edge body bracket: `[]`, `[r]`, `[:LABEL]`, `[r:LABEL]`,
/// `[r:LABEL*1..3]`, `[*]`. The grammar requires `LBracket … RBracket`
/// even for the unfiltered case.
pub fn edge_body() -> impl Strategy<Value = String> {
    (
        proptest::option::of(ident()),
        proptest::option::of(edge_label()),
        proptest::option::of(0u32..5),
        proptest::option::of(0u32..5),
    )
        .prop_map(|(alias, label, min_hops, max_hops)| {
            let mut s = String::from("[");
            if let Some(a) = alias {
                s.push_str(&a);
            }
            if let Some(l) = label {
                s.push(':');
                s.push_str(&l);
            }
            // Variable-length edge spec. Emit either `*`, `*N`, or
            // `*N..M` so the `parse_edge_and_node` star-handling
            // branches are all exercised. The grammar requires
            // `min..max` (DotDot) when both are present, with
            // integers on both sides.
            match (min_hops, max_hops) {
                (Some(mn), Some(mx)) => {
                    let lo = mn.min(mx);
                    let hi = mn.max(mx);
                    s.push_str(&format!("*{}..{}", lo, hi));
                }
                (Some(mn), None) => s.push_str(&format!("*{}", mn)),
                (None, Some(_)) => s.push_str("*"),
                (None, None) => {}
            }
            s.push(']');
            s
        })
}

/// Edge with direction: `-[…]->`, `<-[…]-`, or `-[…]-` (any).
pub fn edge_with_direction() -> impl Strategy<Value = String> {
    (edge_body(), 0u8..3u8).prop_map(|(body, dir)| match dir {
        0 => format!("-{}->", body),
        1 => format!("<-{}-", body),
        _ => format!("-{}-", body),
    })
}

/// `MATCH (a)-[r]->(b) RETURN a` — single-hop pattern.
///
/// This is the canonical valid-shape strategy: every emitted
/// statement must parse.
pub fn match_simple_stmt() -> impl Strategy<Value = String> {
    (node_pattern(), edge_with_direction(), node_pattern()).prop_map(|(a, e, b)| {
        // Extract the alias of the first node so the RETURN list
        // references something the pattern actually defined. Aliases
        // are the substring between `(` and the next `:` / ` ` / `)`.
        let alias = first_alias(&a);
        format!("MATCH {}{}{} RETURN {}", a, e, b, alias)
    })
}

/// `MATCH (a)-[]->(b)-[]->(c) ... RETURN a` — multi-hop pattern
/// path with 2..4 nodes (1..3 edges).
pub fn match_pattern_path_stmt() -> impl Strategy<Value = String> {
    (
        node_pattern(),
        proptest::collection::vec((edge_with_direction(), node_pattern()), 1..=3),
    )
        .prop_map(|(head, tail)| {
            let alias = first_alias(&head);
            let mut s = format!("MATCH {}", head);
            for (e, n) in tail {
                s.push_str(&e);
                s.push_str(&n);
            }
            s.push_str(" RETURN ");
            s.push_str(&alias);
            s
        })
}

/// `MATCH (a)-[r]->(b) WHERE a.prop = lit RETURN a` —
/// MATCH with an inline WHERE clause.
pub fn match_with_where_stmt() -> impl Strategy<Value = String> {
    (
        node_pattern(),
        edge_with_direction(),
        node_pattern(),
        ident(),
        prop_value(),
    )
        .prop_map(|(a, e, b, prop, val)| {
            let alias = first_alias(&a);
            format!(
                "MATCH {}{}{} WHERE {}.{} = {} RETURN {}",
                a, e, b, alias, prop, val, alias
            )
        })
}

/// `PATH FROM <selector> TO <selector> [VIA […]] [LIMIT N] RETURN …`
/// — exercises the path-query surface (`parser/path.rs`).
pub fn path_query_stmt() -> impl Strategy<Value = String> {
    (
        str_lit(),
        str_lit(),
        proptest::option::of(proptest::collection::vec(edge_label(), 1..=2)),
        proptest::option::of(1u32..50),
    )
        .prop_map(|(from, to, via, limit)| {
            let mut s = format!("PATH FROM host({}) TO host({})", from, to);
            if let Some(types) = via {
                let inner: Vec<String> = types.iter().map(|t| format!(":{}", t)).collect();
                s.push_str(&format!(" VIA [{}]", inner.join(", ")));
            }
            if let Some(lim) = limit {
                s.push_str(&format!(" LIMIT {}", lim));
            }
            s
        })
}

/// `GRAPH NEIGHBORHOOD 'src' [DEPTH n] [DIRECTION dir]`,
/// `GRAPH TRAVERSE 'src' [STRATEGY s] [DEPTH n] [DIRECTION dir]`,
/// `GRAPH SHORTEST_PATH 'src' TO 'tgt' [ALGORITHM …]`,
/// `GRAPH CENTRALITY [ALGORITHM …]`.
pub fn graph_traversal_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        // NEIGHBORHOOD
        (str_lit(), proptest::option::of(1u32..10), proptest::option::of(0u8..3u8)).prop_map(
            |(src, depth, dir)| {
                let mut s = format!("GRAPH NEIGHBORHOOD {}", src);
                if let Some(d) = depth {
                    s.push_str(&format!(" DEPTH {}", d));
                }
                if let Some(d) = dir {
                    let dir_str = match d {
                        0 => "outgoing",
                        1 => "incoming",
                        _ => "both",
                    };
                    s.push_str(&format!(" DIRECTION {}", dir_str));
                }
                s
            },
        ),
        // TRAVERSE
        (
            str_lit(),
            proptest::option::of(any::<bool>()),
            proptest::option::of(1u32..10),
            proptest::option::of(0u8..3u8),
        )
            .prop_map(|(src, strat, depth, dir)| {
                let mut s = format!("GRAPH TRAVERSE {}", src);
                if let Some(b) = strat {
                    s.push_str(&format!(" STRATEGY {}", if b { "bfs" } else { "dfs" }));
                }
                if let Some(d) = depth {
                    s.push_str(&format!(" DEPTH {}", d));
                }
                if let Some(d) = dir {
                    let dir_str = match d {
                        0 => "outgoing",
                        1 => "incoming",
                        _ => "both",
                    };
                    s.push_str(&format!(" DIRECTION {}", dir_str));
                }
                s
            }),
        // SHORTEST_PATH
        (str_lit(), str_lit(), proptest::option::of(0u8..2u8)).prop_map(|(src, tgt, algo)| {
            let mut s = format!("GRAPH SHORTEST_PATH {} TO {}", src, tgt);
            if let Some(a) = algo {
                let algo_str = match a {
                    0 => "bfs",
                    _ => "dijkstra",
                };
                s.push_str(&format!(" ALGORITHM {}", algo_str));
            }
            s
        }),
        // CENTRALITY
        proptest::option::of(0u8..3u8).prop_map(|algo| {
            let mut s = String::from("GRAPH CENTRALITY");
            if let Some(a) = algo {
                let algo_str = match a {
                    0 => "degree",
                    1 => "pagerank",
                    _ => "closeness",
                };
                s.push_str(&format!(" ALGORITHM {}", algo_str));
            }
            s
        }),
    ]
}

/// User-attempted `CREATE NODE` shape. The parser does not
/// implement this form (CREATE NODE is reached through API
/// entrypoints, not the SQL surface), so every emitted string
/// must Err — but never panic. Used only by the panic-safety
/// proptest, never by the valid-shape one.
pub fn create_node_attempt_stmt() -> impl Strategy<Value = String> {
    (ident(), node_label(), proptest::option::of(brace_props())).prop_map(|(alias, label, props)| {
        let body = props.unwrap_or_default();
        format!("CREATE NODE ({}: {} {})", alias, label, body)
    })
}

/// User-attempted `CREATE EDGE` shape. Same caveat as
/// [`create_node_attempt_stmt`]: parser must Err, must not panic.
pub fn create_edge_attempt_stmt() -> impl Strategy<Value = String> {
    (ident(), ident(), edge_label()).prop_map(|(from, to, label)| {
        format!("CREATE EDGE ({})-[:{}]->({})", from, to, label)
    })
}

/// Top-level union: any of the valid-shape graph DSL strategies.
/// `match_simple_stmt`, `match_pattern_path_stmt`,
/// `match_with_where_stmt`, `path_query_stmt`,
/// `graph_traversal_stmt` all emit shapes the parser accepts.
pub fn any_valid_graph_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        match_simple_stmt(),
        match_pattern_path_stmt(),
        match_with_where_stmt(),
        path_query_stmt(),
        graph_traversal_stmt(),
    ]
}

/// Helper: extract the alias of a freshly-emitted node pattern.
/// The pattern is `(alias…)`; the alias is the substring between
/// the leading `(` and the next character that ends the alias
/// (`:`, ` `, or `)`).
fn first_alias(node: &str) -> String {
    debug_assert!(node.starts_with('('));
    let body = &node[1..];
    let end = body
        .find(|c: char| c == ':' || c == ' ' || c == ')')
        .unwrap_or(body.len());
    body[..end].to_string()
}
