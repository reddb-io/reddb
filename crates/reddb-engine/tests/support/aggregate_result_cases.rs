//! Native result assembly with an explicit test-only group-label and effect policy.
use super::*;
use reddb_engine::query::{
    planning::aggregation::{prepare_aggregate_runtime_plan, render_expr_signature},
    runtime::{aggregate_results::*, ordering::sort_records_by_order_by_with_host},
    unified::{UnifiedRecord, UnifiedResult},
};
use reddb_rql::{
    ast::{Expr, FieldRef, Filter, OrderByClause, TableQuery},
    sql_lowering::{effective_table_group_by_exprs, effective_table_projections},
};
use std::cell::RefCell;

#[derive(Default)]
pub(super) struct ResultHost {
    events: RefCell<Vec<&'static str>>,
    group_limit: Option<usize>,
    group_counts: RefCell<Vec<usize>>,
    deny_output: bool,
}
impl AggregateHost for ResultHost {
    fn compare_values(&self, left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
        Host.compare_values(left, right)
    }
    fn evaluate_static_projection(&self, projection: &Projection) -> Option<Value> {
        Host.evaluate_static_projection(projection)
    }
}
impl AggregateResultHost for ResultHost {
    type Error = String;
    fn guard_materialization(&self, groups: usize) -> Result<(), String> {
        self.events.borrow_mut().push("guard");
        self.group_counts.borrow_mut().push(groups);
        if self.group_limit.is_some_and(|limit| groups > limit) {
            Err("materialization denied".into())
        } else {
            Ok(())
        }
    }
    fn group_output_label(&self, _: &TableQuery, _: &Expr) -> String {
        "tenant".into()
    }
    fn group_expression_key(&self, expression: &Expr) -> Option<String> {
        render_expr_signature(expression)
    }
    fn having_matches(&self, filter: &Filter, record: &UnifiedRecord) -> bool {
        self.events.borrow_mut().push("having");
        // The fixture only admits pure, typed post-aggregate predicates.
        let mut query = super::super::record_filters::table_query("SELECT * FROM orders");
        query.filter = Some(filter.clone());
        super::super::record_filters::matches_where(record, &query, None).expect("fixture HAVING")
    }
    fn sort_records(&self, records: &mut Vec<UnifiedRecord>, clauses: &[OrderByClause]) {
        self.events.borrow_mut().push("sort");
        sort_records_by_order_by_with_host(&Host, records, clauses, None, None)
    }
    fn secret_query_output(&self, _: Value) -> Result<Value, String> {
        self.events.borrow_mut().push("secret");
        if self.deny_output {
            Err("secret output denied".into())
        } else {
            Ok(Value::Secret(vec![7]))
        }
    }
}

struct Group {
    values: Vec<Value>,
    state: SlottedAggState,
}
fn query(sql: &str) -> TableQuery {
    super::super::record_filters::table_query(sql)
}
fn run(
    host: &ResultHost,
    sql: &str,
    inputs: &[(&str, &[i64])],
    sensitive: Option<&UnifiedRecord>,
) -> Result<UnifiedResult, String> {
    let query = query(sql);
    run_query(host, &query, inputs, sensitive)
}
fn run_query(
    host: &ResultHost,
    query: &TableQuery,
    inputs: &[(&str, &[i64])],
    sensitive: Option<&UnifiedRecord>,
) -> Result<UnifiedResult, String> {
    let runtime = prepare_aggregate_runtime_plan(query).expect("fixture aggregate planning");
    let slots = CompiledAggPlan::compile(&runtime.aggregate_projections).expect("fixture slots");
    let projections = effective_table_projections(query);
    let group_by = effective_table_group_by_exprs(query);
    let groups: Vec<_> = inputs
        .iter()
        .map(|(tenant, input)| {
            let values: Vec<_> = input.iter().copied().map(Value::Integer).collect();
            let (_, state) = state(&runtime.aggregate_projections, &values);
            Group {
                values: vec![Value::text(*tenant)],
                state,
            }
        })
        .collect();
    finish_aggregate_results_with_host(
        host,
        AggregateResultContext {
            query,
            visible_projections: &projections,
            group_by: &group_by,
            runtime_plan: &runtime,
            slots: &slots,
            sensitivity_source: sensitive,
        },
        groups.iter().map(|group| AggregateGroupRef {
            group_values: &group.values,
            state: &group.state,
        }),
    )
}

pub(super) fn grouped_columns_and_hidden_values() {
    let host = ResultHost::default();
    let result = run(&host, "SELECT SUM(quantity) AS amount, tenant, COUNT(*) AS n FROM orders GROUP BY tenant HAVING MAX(quantity) >= 10 ORDER BY SUM(quantity) DESC", &[("blue", &[5,10]), ("red", &[10,20]), ("small", &[2,3])], None).expect("grouped results");
    assert_eq!(
        result.columns,
        ["tenant", "amount", "n"],
        "native group columns precede aggregate columns"
    );
    assert_eq!(result.records.len(), 2);
    assert_eq!(result.records[0].get("tenant"), Some(&Value::text("red")));
    assert_eq!(result.records[0].get("amount"), Some(&Value::Integer(30)));
    assert_eq!(result.records[1].get("amount"), Some(&Value::Integer(15)));
    assert_eq!(
        result.records[0].get("max(quantity)"),
        Some(&Value::Integer(20))
    );
    assert!(
        !result.columns.contains(&"max(quantity)".into()),
        "hidden results do not become visible columns"
    );
    assert_eq!(
        &*host.events.borrow(),
        &["guard", "having", "having", "having", "sort"]
    );
}

pub(super) fn empty_input_semantics() {
    let host = ResultHost::default();
    let result = run(
        &host,
        "SELECT COUNT(*) AS n, SUM(quantity) AS amount FROM orders",
        &[],
        None,
    )
    .expect("empty scalar aggregation");
    assert_eq!(result.columns, ["n", "amount"]);
    assert_eq!(result.records.len(), 1);
    assert_eq!(result.records[0].get("n"), Some(&Value::Integer(0)));
    assert_eq!(result.records[0].get("amount"), Some(&Value::Null));
    let result = run(
        &host,
        "SELECT tenant, COUNT(*) AS n FROM orders GROUP BY tenant",
        &[],
        None,
    )
    .expect("empty grouped aggregation");
    assert!(result.records.is_empty());
    assert!(
        result.columns.is_empty(),
        "preserve native empty grouped schema behavior"
    );
    // Supply an admitted legacy HAVING for the no-GROUP-BY empty-row path.
    let mut query = query("SELECT COUNT(*) AS n FROM orders");
    query.having_expr = Some(Expr::binop(
        reddb_rql::ast::BinOp::Gt,
        Expr::FunctionCall {
            name: "COUNT".into(),
            args: vec![Expr::col(FieldRef::TableColumn {
                table: String::new(),
                column: "*".into(),
            })],
            span: reddb_rql::ast::Span::synthetic(),
        },
        Expr::lit(Value::Integer(0)),
    ));
    let result = run_query(&host, &query, &[], None).expect("empty HAVING result");
    assert!(result.records.is_empty());
    assert_eq!(result.columns, ["n"], "columns survive rejection by HAVING");
}

pub(super) fn wide_pagination() {
    let inputs: &[(&str, &[i64])] = &[("red", &[10, 20]), ("blue", &[5, 10])];
    // The native AST/binder stores u64 bounds; the SQL literal grammar still uses i64.
    let mut widest = query("SELECT tenant, SUM(quantity) AS amount FROM orders GROUP BY tenant ORDER BY SUM(quantity) DESC");
    widest.limit = Some(u64::MAX);
    assert_eq!(
        run_query(&ResultHost::default(), &widest, inputs, None)
            .expect("u64 AST limit")
            .records
            .len(),
        2
    );
    widest.offset = Some(u64::MAX);
    assert!(run_query(&ResultHost::default(), &widest, inputs, None)
        .expect("u64 AST offset")
        .records
        .is_empty());
    for limit in [1u64 << 32, (1u64 << 32) + 1, i64::MAX as u64] {
        let sql = format!("SELECT tenant, SUM(quantity) AS amount FROM orders GROUP BY tenant ORDER BY SUM(quantity) DESC LIMIT {limit}");
        let result = run(&ResultHost::default(), &sql, inputs, None).expect("wide limit");
        assert_eq!(
            result.records.len(),
            2,
            "limit {limit} must not truncate to wasm32 width"
        );
    }
    for offset in [1u64 << 32, i64::MAX as u64] {
        let sql = format!("SELECT tenant, SUM(quantity) AS amount FROM orders GROUP BY tenant ORDER BY SUM(quantity) DESC LIMIT 1 OFFSET {offset}");
        assert!(run(&ResultHost::default(), &sql, inputs, None)
            .expect("wide offset")
            .records
            .is_empty());
    }
    let result = run(&ResultHost::default(), "SELECT tenant, SUM(quantity) AS amount FROM orders GROUP BY tenant ORDER BY SUM(quantity) DESC LIMIT 1 OFFSET 1", inputs, None).expect("normal page");
    assert_eq!(result.records.len(), 1);
    assert_eq!(result.records[0].get("tenant"), Some(&Value::text("blue")));
}

pub(super) fn admission_and_secret_policy_order() {
    let inputs: &[(&str, &[i64])] = &[("red", &[10, 20]), ("blue", &[5, 10])];
    let sql = "SELECT tenant, SUM(quantity) AS amount FROM orders GROUP BY tenant HAVING MAX(quantity) >= 10 ORDER BY SUM(quantity) DESC LIMIT 1 OFFSET 1";
    let host = ResultHost {
        group_limit: Some(1),
        ..Default::default()
    };
    assert_eq!(
        run(&host, sql, inputs, None).expect_err("guard rejection"),
        "materialization denied"
    );
    assert_eq!(&*host.events.borrow(), &["guard"]);
    assert_eq!(&*host.group_counts.borrow(), &[2]);
    let denied_empty_page = sql
        .replace("MAX(quantity) >= 10", "MAX(quantity) >= 1000")
        .replace("LIMIT 1 OFFSET 1", "LIMIT 0");
    assert_eq!(
        run(&host, &denied_empty_page, inputs, None)
            .expect_err("HAVING/LIMIT cannot bypass materialization admission"),
        "materialization denied"
    );
    let mut source = UnifiedRecord::new();
    source.set("quantity", Value::Secret(vec![1]));
    let host = ResultHost::default();
    let result = run(&host, sql, inputs, Some(&source)).expect("protected page");
    assert_eq!(result.records.len(), 1);
    assert_eq!(
        result.records[0].get("amount"),
        Some(&Value::Secret(vec![7]))
    );
    assert_eq!(
        result.records[0].get("max(quantity)"),
        Some(&Value::Secret(vec![7]))
    );
    assert_eq!(
        &*host.events.borrow(),
        &["guard", "having", "having", "sort", "secret", "secret"]
    );
    let host = ResultHost {
        deny_output: true,
        ..Default::default()
    };
    assert_eq!(
        run(&host, sql, inputs, Some(&source)).expect_err("secret output policy"),
        "secret output denied"
    );
    assert_eq!(
        &*host.events.borrow(),
        &["guard", "having", "having", "sort", "secret"]
    );
    let host = ResultHost {
        deny_output: true,
        ..Default::default()
    };
    let result = run(
        &host,
        &sql.replace("LIMIT 1 OFFSET 1", "LIMIT 0"),
        inputs,
        Some(&source),
    )
    .expect("discarded rows have no secret output callback");
    assert!(result.records.is_empty());
    assert!(!host.events.borrow().contains(&"secret"));
}
