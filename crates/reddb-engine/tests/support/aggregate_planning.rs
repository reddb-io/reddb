//! Test-only policy host for the shared native aggregate reference planner.
use super::*;
use reddb_engine::query::{
    planning::aggregation::*,
    runtime::{
        field_resolution::resolve_runtime_field,
        ordering::{sort_records_by_order_by_with_host, OrderingHost},
    },
    unified::UnifiedRecord,
};
use reddb_rql::ast::{
    BinOp, CompareOp, Expr, FieldRef, Filter, SelectItem, Span, TableQuery, UnaryOp,
};

pub(super) fn aliases_and_hidden_reuse() {
    let query = parse_query(
        "SELECT SUM(total) AS amount FROM orders GROUP BY tenant HAVING SUM(total) > 20 ORDER BY SUM(total) DESC",
    );
    let plan = prepare_aggregate_runtime_plan(&query).expect("visible aggregate alias");
    assert!(plan.hidden_aggregates.is_empty());
    assert_eq!(plan.aggregate_projections.len(), 1);
    let rows = finish(
        &query,
        &plan,
        vec![output("red", &[("amount", Value::Integer(30))])],
    );
    assert_eq!(
        rows.len(),
        1,
        "HAVING resolves SUM(total) through its visible alias"
    );

    let query = parse_query(
        "SELECT COUNT(*) AS n FROM orders GROUP BY tenant HAVING SUM(total) > 20 ORDER BY SUM(total) DESC",
    );
    let plan = prepare_aggregate_runtime_plan(&query).expect("hidden aggregate");
    assert_eq!(
        plan.hidden_aggregates.len(),
        1,
        "HAVING/ORDER BY share one hidden aggregate"
    );
    assert_eq!(plan.aggregate_projections.len(), 2);
    let rows = finish(
        &query,
        &plan,
        vec![
            output(
                "red",
                &[("n", Value::Integer(2)), ("sum(total)", Value::Integer(30))],
            ),
            output(
                "blue",
                &[("n", Value::Integer(2)), ("sum(total)", Value::Integer(10))],
            ),
        ],
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("tenant"), Some(&Value::text("red")));
}

pub(super) fn projection_dedup_and_legacy_inputs() {
    let query = parse_query(
        "SELECT SUM(total) AS sum_a, SUM(total) AS sum_b FROM orders GROUP BY tenant HAVING SUM(total) >= 5",
    );
    let plan = prepare_aggregate_runtime_plan(&query).expect("repeated visible aggregate");
    assert_eq!(plan.aggregate_projections.len(), 1);
    assert!(plan.hidden_aggregates.is_empty());
    let first_projection = &plan.aggregate_projections[0];
    let (compiled, state) = state(std::slice::from_ref(first_projection), &[Value::Integer(5)]);
    for projection in reddb_rql::sql_lowering::effective_table_projections(&query) {
        assert_eq!(result(&projection, &compiled, &state), Value::Integer(5));
    }
    let rows = finish(
        &query,
        &plan,
        vec![output("red", &[("sum_b", Value::Integer(5))])],
    );
    assert_eq!(
        rows.len(),
        1,
        "the native registry keeps the last visible alias for reference reuse"
    );

    let mut legacy = parse_query("SELECT COUNT(*) FROM orders");
    legacy.select_items.clear();
    legacy.columns = vec![projection("SUM", "total", "amount")];
    legacy.having = Some(Filter::Compare {
        field: field("amount"),
        op: CompareOp::Gt,
        value: Value::Integer(1),
    });
    legacy.having_expr = Some(Expr::lit(Value::Boolean(false)));
    let plan = prepare_aggregate_runtime_plan(&legacy).expect("legacy projection/filter authority");
    assert_eq!(plan.aggregate_projections.len(), 1);
    assert_eq!(
        finish(
            &legacy,
            &plan,
            vec![output("red", &[("amount", Value::Integer(5))])]
        )
        .len(),
        1
    );
    legacy.select_items = vec![SelectItem::Expr {
        expr: Expr::FunctionCall {
            name: "COUNT".into(),
            args: vec![Expr::col(field("*"))],
            span: Span::synthetic(),
        },
        alias: Some("n".into()),
    }];
    let plan = prepare_aggregate_runtime_plan(&legacy).expect("canonical select authority");
    assert!(
        matches!(&plan.aggregate_projections[0], Projection::Function(name, _) if name == "COUNT:n")
    );
}

pub(super) fn nested_expressions_and_ordering() {
    let query = parse_query("SELECT SUM(total) AS amount FROM orders GROUP BY tenant HAVING COALESCE(SUM(total), 0) BETWEEN 10 AND 40 AND SUM(total) IN (20, 30) AND SUM(total) IS NOT NULL ORDER BY CASE WHEN SUM(total) > 0 THEN -CAST(SUM(total) AS BIGINT) ELSE 0 END ASC, tenant ASC");
    let plan = prepare_aggregate_runtime_plan(&query).expect("nested aggregate references");
    assert!(plan.hidden_aggregates.is_empty());
    let rows = finish(
        &query,
        &plan,
        vec![
            output("blue", &[("amount", Value::Integer(20))]),
            output("red", &[("amount", Value::Integer(30))]),
            output("other", &[("amount", Value::Integer(60))]),
        ],
    );
    assert_eq!(
        rows.iter()
            .map(|row| row.get("tenant").cloned())
            .collect::<Vec<_>>(),
        vec![Some(Value::text("red")), Some(Value::text("blue"))]
    );
    assert_eq!(plan.order_by[0].ascending, query.order_by[0].ascending);
    assert_eq!(plan.order_by[0].nulls_first, query.order_by[0].nulls_first);
}

pub(super) fn hidden_parameters_and_pagination() {
    let query = parse_query("SELECT COUNT(*) AS n FROM orders GROUP BY tenant HAVING PERCENTILE(total, 0.25) = 10 ORDER BY PERCENTILE(total, 0.75) DESC, tenant ASC LIMIT 1 OFFSET 1");
    let plan = prepare_aggregate_runtime_plan(&query).expect("parameter-sensitive registration");
    assert_eq!(
        plan.hidden_aggregates.len(),
        2,
        "parameters remain part of native signatures"
    );
    assert_eq!(plan.aggregate_projections.len(), 3);
    // Materialize both percentile results as the native executor does.
    let visible = reddb_rql::sql_lowering::effective_table_projections(&query);
    let mut rows = Vec::new();
    for (tenant, input) in [("red", [10, 10, 20, 90]), ("blue", [10, 10, 50, 60])] {
        let input = input.map(Value::Integer);
        let (compiled, accumulated) = state(&plan.aggregate_projections, &input);
        let mut row = output(tenant, &[]);
        for projection in visible.iter().chain(&plan.hidden_aggregates) {
            let (label, value) =
                aggregate_projection_result_with_host(&Host, projection, &accumulated, &compiled)
                    .expect("percentile output");
            row.set(&label, value);
        }
        rows.push(row);
    }
    // Both groups have a lower quartile of 10; the upper quartile orders blue first.
    let rows = finish(&query, &plan, rows);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("tenant"), Some(&Value::text("red")));
    let mut collision = parse_query("SELECT COUNT(*) AS n FROM orders GROUP BY tenant HAVING SUM(total) > 10 ORDER BY PERCENTILE(total, 0.25), PERCENTILE(total, 0.75)");
    let SelectItem::Expr { alias, .. } = &mut collision.select_items[0] else {
        unreachable!()
    };
    *alias = Some("sum(total)".into());
    let collision = prepare_aggregate_runtime_plan(&collision).expect("visible alias collision");
    let mut labels = std::collections::HashSet::from(["sum(total)".to_string()]);
    for projection in &collision.hidden_aggregates {
        let (_, label) = visible_aggregate_output_name(projection).expect("hidden output label");
        assert!(
            labels.insert(label),
            "hidden labels never overwrite a visible alias or another aggregate"
        );
    }
    let collision = parse_query("SELECT COUNT(*) AS __reddb_aggregate_0 FROM orders GROUP BY tenant HAVING PERCENTILE(total, 0.25) > 0 ORDER BY PERCENTILE(total, 0.75)");
    let collision = prepare_aggregate_runtime_plan(&collision).expect("generated label collision");
    let labels: Vec<_> = collision
        .hidden_aggregates
        .iter()
        .map(|projection| {
            visible_aggregate_output_name(projection)
                .expect("hidden label")
                .1
        })
        .collect();
    assert!(!labels.contains(&"__reddb_aggregate_0".to_string()));
    let query = parse_query("SELECT COUNT(*) AS n FROM orders GROUP BY tenant HAVING MAX(total) >= 10 ORDER BY MAX(total) DESC, tenant ASC LIMIT 1 OFFSET 1");
    let plan = prepare_aggregate_runtime_plan(&query).expect("hidden max pagination");
    let rows = finish(
        &query,
        &plan,
        vec![
            output(
                "blue",
                &[("n", Value::Integer(2)), ("max(total)", Value::Integer(15))],
            ),
            output(
                "red",
                &[("n", Value::Integer(2)), ("max(total)", Value::Integer(30))],
            ),
            output(
                "none",
                &[("n", Value::Integer(2)), ("max(total)", Value::Integer(5))],
            ),
        ],
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("tenant"), Some(&Value::text("blue")));
}

pub(super) fn signatures_and_unsupported_shapes() {
    let expression = Expr::binop(
        BinOp::Add,
        Expr::col(field("total")),
        Expr::lit(Value::Integer(2)),
    );
    let projection =
        reddb_rql::sql_lowering::expr_to_projection(&expression).expect("native lowering");
    assert_eq!(
        render_expr_signature(&expression).as_deref(),
        Some("ADD(total,2)")
    );
    assert_eq!(
        render_projection_signature(&projection).as_deref(),
        Some("ADD(total,2)")
    );
    assert!(visible_aggregate_output_name(&Projection::All).is_none());
    let query = parse_query(
        "SELECT COUNT(*) AS n FROM orders GROUP BY tenant HAVING MISSING(SUM(total)) > 0",
    );
    let plan = prepare_aggregate_runtime_plan(&query).expect("unknown function remains host-owned");
    assert_eq!(
        plan.hidden_aggregates.len(),
        1,
        "nested known aggregate still materialized"
    );
    let Some(Filter::CompareExpr {
        lhs: Expr::FunctionCall { name, .. },
        ..
    }) = plan.having
    else {
        panic!("outer unknown function is preserved");
    };
    assert_eq!(name, "MISSING");
}

pub(super) fn depth_before_lowering_and_clone() {
    let mut accepted = Expr::col(field("total"));
    for _ in 0..reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        accepted = Expr::UnaryOp {
            op: UnaryOp::Neg,
            operand: Box::new(accepted),
            span: Span::synthetic(),
        };
    }
    assert!(render_expr_signature(&accepted).is_some());
    let rejected = Expr::UnaryOp {
        op: UnaryOp::Neg,
        operand: Box::new(accepted),
        span: Span::synthetic(),
    };
    assert!(render_expr_signature(&rejected).is_none());
    for slot in 0..5 {
        let mut query = parse_query("SELECT COUNT(*) FROM orders");
        match slot {
            0 => {
                query.select_items = vec![SelectItem::Expr {
                    expr: rejected.clone(),
                    alias: None,
                }]
            }
            1 => {
                query.having = None;
                query.having_expr = Some(rejected.clone());
            }
            2 => {
                query.order_by = vec![reddb_rql::ast::OrderByClause {
                    field: field("total"),
                    expr: Some(rejected.clone()),
                    ascending: true,
                    nulls_first: false,
                }]
            }
            3 => query.group_by_exprs = vec![rejected.clone()],
            _ => {
                query.filter = None;
                query.where_expr = Some(rejected.clone());
            }
        }
        assert!(
            validate_aggregate_planning_depth(&query).is_err(),
            "source slot {slot}"
        );
        assert!(
            prepare_aggregate_runtime_plan(&query).is_err(),
            "source slot {slot}"
        );
    }
    let mut projection = Projection::Column("total".into());
    for _ in 0..=reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        projection = Projection::Function("ABS".into(), vec![projection]);
    }
    assert!(render_projection_signature(&projection).is_none());
    assert!(visible_aggregate_output_name(&projection).is_none());
    let mut legacy = parse_query("SELECT COUNT(*) FROM orders");
    legacy.select_items.clear();
    legacy.columns = vec![projection];
    assert!(prepare_aggregate_runtime_plan(&legacy).is_err());
    let mut filter = Filter::IsNull(field("total"));
    for _ in 0..=reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        filter = Filter::Not(Box::new(filter));
    }
    let mut legacy = parse_query("SELECT COUNT(*) FROM orders");
    legacy.having = Some(filter);
    assert!(prepare_aggregate_runtime_plan(&legacy).is_err());
}

fn parse_query(sql: &str) -> TableQuery {
    super::super::record_filters::table_query(sql)
}
fn field(column: &str) -> FieldRef {
    FieldRef::TableColumn {
        table: String::new(),
        column: column.into(),
    }
}
fn output(tenant: &str, values: &[(&str, Value)]) -> UnifiedRecord {
    let mut record = UnifiedRecord::new();
    record.set("tenant", Value::text(tenant));
    for (label, value) in values {
        record.set(label, value.clone());
    }
    record
}

impl OrderingHost for Host {
    fn evaluate_expression(&self, expression: &Expr, record: &UnifiedRecord) -> Option<Value> {
        evaluator::evaluate(expression, &|field: &FieldRef| {
            resolve_runtime_field(record, field, None, None)
        })
        .ok()
    }
    fn secret_query_input(&self, value: Value) -> Option<Value> {
        super::super::record_filters::plaintext(value)
    }
    fn record_identity_key(&self, record: &UnifiedRecord) -> String {
        record
            .get("tenant")
            .map(Value::display_string)
            .unwrap_or_default()
    }
    fn topk_disabled(&self) -> bool {
        false
    }
}

pub(super) fn finish(
    query: &TableQuery,
    plan: &AggregateRuntimePlan,
    mut rows: Vec<UnifiedRecord>,
) -> Vec<UnifiedRecord> {
    let mut having = query.clone();
    having.filter = plan.having.clone();
    having.where_expr = None;
    rows.retain(|row| {
        super::super::record_filters::matches_where(row, &having, None).expect("planned HAVING")
    });
    sort_records_by_order_by_with_host(&Host, &mut rows, &plan.order_by, None, None);
    rows.into_iter()
        .skip(usize::try_from(query.offset.unwrap_or(0)).unwrap_or(usize::MAX))
        .take(
            query
                .limit
                .map(|value| usize::try_from(value).unwrap_or(usize::MAX))
                .unwrap_or(usize::MAX),
        )
        .collect()
}
