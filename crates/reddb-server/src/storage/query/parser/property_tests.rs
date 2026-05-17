//! Property-based round-trip tests for the parser and AstRenderer.
//!
//! Property: for the covered subset of QueryExpr,
//!   `render(parse(render(ast))) == render(ast)`
//!
//! Run in isolation:
//!   cargo test -p reddb-server property_round_trip

use proptest::prelude::*;

use crate::storage::query::ast::{
    CompareOp, FieldRef, Filter, InsertEntityType, InsertQuery, Projection, QueryExpr,
    QueueCommand, QueueSide, TableQuery,
};
use crate::storage::query::renderer::render;
use crate::storage::schema::Value;

// ---------------------------------------------------------------------------
// Identifier strategy — 2–4 lowercase letters + 1–2 digits.
// All reserved words in the lexer are purely alphabetic (except `l2` which
// has only 1 alpha prefix and doesn't match this pattern), so this pattern
// guarantees no reserved-word collisions without maintaining an explicit list.
// ---------------------------------------------------------------------------

fn arb_ident() -> impl Strategy<Value = String> {
    "[a-z]{2,4}[1-9][0-9]?"
}

// ---------------------------------------------------------------------------
// Value strategies (only variants that survive render → parse unchanged)
// ---------------------------------------------------------------------------

fn arb_sql_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        // Restrict to non-negative: negative integers in WHERE need unary-minus
        // Expr lowering, but parse_value() (the fast literal path) doesn't handle
        // `Token::Dash`. Stick to the safe range.
        (0i64..=1_000_000i64).prop_map(Value::Integer),
        any::<bool>().prop_map(Value::Boolean),
        Just(Value::Null),
        "[a-z0-9]{1,16}".prop_map(|s| Value::text(s)),
    ]
}

fn arb_json_value() -> impl Strategy<Value = Value> {
    // Single-key JSON object with an integer value — trivially canonical.
    // Keys use BTreeMap so ordering is deterministic; a single key avoids
    // any concern about key-sort differences.
    ("[a-z]{1,4}", 0i64..1000i64).prop_map(|(key, val)| {
        let json = format!("{{\"{}\":{}}}", key, val);
        let bytes = json.into_bytes();
        Value::Json(bytes)
    })
}

// ---------------------------------------------------------------------------
// Query strategies
// ---------------------------------------------------------------------------

fn arb_table_query_no_filter() -> impl Strategy<Value = QueryExpr> {
    (
        arb_ident(),
        proptest::collection::vec(arb_ident(), 1..4usize),
    )
        .prop_map(|(table, cols)| {
            let columns = cols.into_iter().map(Projection::Column).collect();
            QueryExpr::Table(TableQuery {
                table,
                source: None,
                alias: None,
                select_items: Vec::new(),
                columns,
                where_expr: None,
                filter: None,
                group_by_exprs: Vec::new(),
                group_by: Vec::new(),
                having_expr: None,
                having: None,
                order_by: Vec::new(),
                limit: None,
                limit_param: None,
                offset: None,
                offset_param: None,
                expand: None,
                as_of: None,
                sessionize: None,
            })
        })
}

fn arb_table_query_with_filter() -> impl Strategy<Value = QueryExpr> {
    (
        arb_ident(),
        proptest::collection::vec(arb_ident(), 1..4usize),
        arb_ident(),
        arb_sql_value(),
    )
        .prop_map(|(table, cols, filter_col, filter_val)| {
            let columns = cols.into_iter().map(Projection::Column).collect();
            let filter = Some(Filter::Compare {
                field: FieldRef::TableColumn {
                    table: String::new(),
                    column: filter_col,
                },
                op: CompareOp::Eq,
                value: filter_val,
            });
            QueryExpr::Table(TableQuery {
                table,
                source: None,
                alias: None,
                select_items: Vec::new(),
                columns,
                where_expr: None,
                filter,
                group_by_exprs: Vec::new(),
                group_by: Vec::new(),
                having_expr: None,
                having: None,
                order_by: Vec::new(),
                limit: None,
                limit_param: None,
                offset: None,
                offset_param: None,
                expand: None,
                as_of: None,
                sessionize: None,
            })
        })
}

fn arb_insert_query() -> impl Strategy<Value = QueryExpr> {
    (
        arb_ident(),
        proptest::collection::vec(arb_ident(), 1..4usize),
        proptest::collection::vec(arb_sql_value(), 1..4usize),
    )
        .prop_filter("col count == val count", |(_, cols, vals)| {
            cols.len() == vals.len()
        })
        .prop_map(|(table, columns, values)| {
            QueryExpr::Insert(InsertQuery {
                table,
                entity_type: InsertEntityType::Row,
                columns,
                value_exprs: Vec::new(),
                values: vec![values],
                returning: None,
                ttl_ms: None,
                expires_at_ms: None,
                with_metadata: Vec::new(),
                auto_embed: None,
                suppress_events: false,
            })
        })
}

fn arb_queue_push() -> impl Strategy<Value = QueryExpr> {
    (arb_ident(), arb_json_value()).prop_map(|(queue, value)| {
        QueryExpr::QueueCommand(QueueCommand::Push {
            queue,
            value,
            side: QueueSide::Right,
            priority: None,
        })
    })
}

// ---------------------------------------------------------------------------
// Helper: parse and immediately unwrap `QueryWithCte` to `QueryExpr`
// ---------------------------------------------------------------------------

fn parse_q(sql: &str) -> Result<QueryExpr, crate::storage::query::parser::ParseError> {
    crate::storage::query::parser::parse(sql).map(|q| q.query)
}

// ---------------------------------------------------------------------------
// Round-trip property test
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    #[test]
    fn property_round_trip_select_no_filter(ast in arb_table_query_no_filter()) {
        let sql1 = render(&ast);
        prop_assume!(!sql1.is_empty());
        let ast2 = parse_q(&sql1).expect(&format!("failed to re-parse: {sql1}"));
        let sql2 = render(&ast2);
        prop_assert_eq!(sql1, sql2, "render(parse(render(ast))) != render(ast)");
    }

    #[test]
    fn property_round_trip_select_with_filter(ast in arb_table_query_with_filter()) {
        let sql1 = render(&ast);
        prop_assume!(!sql1.is_empty());
        let ast2 = parse_q(&sql1).expect(&format!("failed to re-parse: {sql1}"));
        let sql2 = render(&ast2);
        prop_assert_eq!(sql1, sql2, "render(parse(render(ast))) != render(ast)");
    }

    #[test]
    fn property_round_trip_insert(ast in arb_insert_query()) {
        let sql1 = render(&ast);
        prop_assume!(!sql1.is_empty());
        let ast2 = parse_q(&sql1).expect(&format!("failed to re-parse: {sql1}"));
        let sql2 = render(&ast2);
        prop_assert_eq!(sql1, sql2, "render(parse(render(ast))) != render(ast)");
    }

    #[test]
    fn property_round_trip_queue_push_json(ast in arb_queue_push()) {
        let sql1 = render(&ast);
        prop_assume!(!sql1.is_empty());
        let ast2 = parse_q(&sql1).expect(&format!("failed to re-parse: {sql1}"));
        let sql2 = render(&ast2);
        prop_assert_eq!(sql1, sql2, "render(parse(render(ast))) != render(ast)");
    }
}
