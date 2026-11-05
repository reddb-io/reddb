use reddb_engine::query::engine::binding::{Binding, BindingBuilder, Value as BindingValue, Var};
use reddb_engine::query::engine::cancel::CancelToken;
use reddb_engine::query::engine::iterator::{BindingIterator, IterError, QueryIterBase};
use reddb_engine::query::executor::{MemoryExecutor, QueryExecutor, QueryPlan};
use reddb_engine::query::executors::aggregation::{
    execute_group_by, AggregationDef, SumAggregator,
};
use reddb_engine::query::executors::join::{hash_join, nested_loop_join, JoinCondition, JoinType};
use reddb_engine::query::executors::set_ops::{set_except, set_intersect, set_union};
use reddb_engine::query::executors::window::{
    WindowDef, WindowExecutor, WindowFunc, WindowFuncType, WindowOrderBy,
};
use reddb_engine::query::filter::Filter;
use reddb_engine::query::scalar::{self, ScalarError};
use reddb_engine::query::sort::OrderBy;
use reddb_types::{json, Row, Value};

fn binding(value: i64) -> Binding {
    BindingBuilder::new()
        .add_named("value", BindingValue::Integer(value))
        .build()
}

pub fn scalar_goldens() {
    let cases: json::Value = json::from_str(include_str!("../scalar-cases.json"))
        .expect("checked-in scalar corpus is valid JSON");
    for case in cases.as_array().expect("scalar corpus is an array") {
        let sql = case["sql"].as_str().expect("each case has SQL");
        let actual = scalar::execute(sql).expect(sql).to_json();
        assert_eq!(actual, case["expected"], "{sql}");
    }
}

pub fn scalar_failures() {
    for sql in [
        "CREATE TABLE t (id INT)",
        "INSERT INTO t (id) VALUES (1)",
        "SELECT 1 FROM t",
        "SELECT 1 WHERE FALSE",
        "SELECT 1 ORDER BY 1",
        "SELECT 1 GROUP BY 1",
        "SELECT *",
        "SELECT 1 LIMIT $1",
    ] {
        assert!(
            matches!(scalar::execute(sql), Err(ScalarError::Unsupported)),
            "{sql}"
        );
    }
    for sql in ["SELECT 1 / 0", "SELECT no_such_function(1)", "SELECT $1"] {
        assert!(
            matches!(scalar::execute(sql), Err(ScalarError::Evaluation(_))),
            "{sql}"
        );
    }
    for sql in ["SELECT FROM WHERE", "SELECT 1; SELECT 2"] {
        assert!(
            matches!(scalar::execute(sql), Err(ScalarError::Parse(_))),
            "{sql}"
        );
    }
    let deep = format!("SELECT {}1{}", "(".repeat(128), ")".repeat(128));
    assert!(matches!(scalar::execute(&deep), Err(ScalarError::Parse(_))));
    assert_eq!(
        scalar::execute("SELECT 1 AS n")
            .expect("executor survives errors")
            .rows
            .len(),
        1
    );
}

pub fn row_execution() {
    let mut executor = MemoryExecutor::new();
    executor.add_table(
        "people",
        vec!["id".into(), "age".into()],
        vec![
            Row::new(vec![Value::Integer(1), Value::Integer(20)]),
            Row::new(vec![Value::Integer(2), Value::Integer(40)]),
            Row::new(vec![Value::Integer(3), Value::Integer(30)]),
        ],
    );
    let plan = QueryPlan::new("people")
        .filter(Filter::ge("age", Value::Integer(30)))
        .order_by(OrderBy::new().desc("age"))
        .select(vec!["id".into()])
        .offset(1)
        .limit(1)
        .with_total_count();
    let result = executor
        .execute(&plan)
        .expect("shared row execution succeeds");
    assert_eq!(result.columns, vec!["id"]);
    assert_eq!(result.rows[0].get(0), Some(&Value::Integer(3)));
    assert_eq!(result.total_count, Some(2));
    assert_eq!(result.stats.rows_scanned, 3);
    assert_eq!(
        executor
            .count("people", plan.filter.as_ref())
            .expect("count succeeds"),
        2
    );
}

pub fn binary_document_evaluation() {
    use reddb_rql::ast::{Expr, FieldRef, Span};

    let mut bytes = Vec::new();
    reddb_types::document_body_codec::encode(&[("count", &Value::Integer(42))], &mut bytes)
        .expect("native document container encodes");
    let expression = Expr::FunctionCall {
        name: "JSON_EXTRACT_TEXT".into(),
        args: vec![
            Expr::lit(Value::Json(bytes)),
            Expr::lit(Value::text("$.count")),
        ],
        span: Span::synthetic(),
    };
    let value = reddb_engine::query::evaluator::evaluate(&expression, &|_: &FieldRef| None)
        .expect("shared evaluator reads the native document container");
    assert_eq!(value, Value::text("42"));
}

pub fn join_execution() {
    let left = vec![binding(1), binding(2)];
    let right = vec![binding(2), binding(3)];
    let condition = JoinCondition::Eq(Var::new("value"), Var::new("value"));
    for result in [
        hash_join(left.clone(), right.clone(), &condition, JoinType::Inner),
        nested_loop_join(left, right, &condition, JoinType::Inner),
    ] {
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].get(&Var::new("value")),
            Some(&BindingValue::Integer(2))
        );
    }
}

pub fn aggregation_execution() {
    let rows = vec![binding(2), binding(3), binding(5)];
    let results = execute_group_by(
        rows,
        &[],
        &[AggregationDef {
            source_var: Var::new("value"),
            result_var: Var::new("total"),
            aggregator: Box::new(SumAggregator::new()),
        }],
    );
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].get(&Var::new("total")),
        Some(&BindingValue::Integer(10))
    );
}

pub fn window_execution() {
    let rows = vec![binding(100), binding(100), binding(90), binding(80)];
    let function = WindowFunc::new(
        WindowFuncType::Rank,
        Var::new("rank"),
        WindowDef::default().with_order_by(vec![WindowOrderBy::new(Var::new("value")).desc()]),
    );
    let results = WindowExecutor::execute(rows, &[function]);
    let ranks: Vec<_> = results
        .iter()
        .map(|row| {
            row.get(&Var::new("rank"))
                .expect("rank was computed")
                .as_integer()
                .expect("rank is integer")
        })
        .collect();
    assert_eq!(ranks, [1, 1, 3, 4]);
}

pub fn set_execution() {
    let left = vec![binding(1), binding(2)];
    let right = vec![binding(2), binding(3)];
    assert_eq!(set_union(left.clone(), right.clone(), true).0.len(), 3);
    assert_eq!(set_intersect(left.clone(), right.clone()).len(), 1);
    assert_eq!(set_except(left, right).len(), 1);
}

pub fn cancellation() {
    let token = CancelToken::new();
    let mut iterator = QueryIterBase::new(vec![binding(1), binding(2)]).with_cancel(token.clone());
    assert!(iterator
        .next_binding()
        .expect("first row succeeds")
        .is_some());
    token.cancel();
    assert!(matches!(iterator.next_binding(), Err(IterError::Cancelled)));
}

pub const CASES: [(&str, fn()); 9] = [
    ("scalar_goldens", scalar_goldens),
    ("scalar_failures", scalar_failures),
    ("row_execution", row_execution),
    ("binary_document_evaluation", binary_document_evaluation),
    ("join_execution", join_execution),
    ("aggregation_execution", aggregation_execution),
    ("window_execution", window_execution),
    ("set_execution", set_execution),
    ("cancellation", cancellation),
];
