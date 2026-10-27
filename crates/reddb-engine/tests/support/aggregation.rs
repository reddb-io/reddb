//! Shared native aggregate kernels exercised on native and browser WASM.
use reddb_engine::query::{
    evaluator,
    runtime::{aggregation::*, values::runtime_partial_cmp},
};
use reddb_rql::{ast::Projection, sql_lowering::projection_to_expr};
use reddb_types::Value;

#[path = "aggregate_planning.rs"]
mod aggregate_planning;

#[path = "aggregate_result_cases.rs"]
mod aggregate_result_cases;

#[path = "group_binding_cases.rs"]
mod group_binding_cases;

pub const CASES: &[(&str, fn())] = &[
    (
        "aggregate_group_bindings_alias_resolution",
        group_binding_cases::alias_resolution,
    ),
    (
        "aggregate_group_bindings_expression_labels",
        group_binding_cases::expression_labels,
    ),
    (
        "aggregate_group_bindings_shape_and_host_admission",
        group_binding_cases::shape_and_host_admission,
    ),
    (
        "aggregate_group_bindings_depth_limits",
        group_binding_cases::depth_limits,
    ),
    (
        "aggregate_group_bindings_result_metadata",
        group_binding_cases::result_metadata,
    ),
    (
        "aggregate_results_grouped_columns_and_hidden",
        aggregate_result_cases::grouped_columns_and_hidden_values,
    ),
    (
        "aggregate_results_empty_input_semantics",
        aggregate_result_cases::empty_input_semantics,
    ),
    (
        "aggregate_results_wide_pagination",
        aggregate_result_cases::wide_pagination,
    ),
    (
        "aggregate_results_admission_and_secret_policy",
        aggregate_result_cases::admission_and_secret_policy_order,
    ),
    (
        "aggregate_planning_aliases_and_hidden_reuse",
        aggregate_planning::aliases_and_hidden_reuse,
    ),
    (
        "aggregate_planning_dedup_and_legacy_inputs",
        aggregate_planning::projection_dedup_and_legacy_inputs,
    ),
    (
        "aggregate_planning_nested_expressions_and_ordering",
        aggregate_planning::nested_expressions_and_ordering,
    ),
    (
        "aggregate_planning_parameters_and_pagination",
        aggregate_planning::hidden_parameters_and_pagination,
    ),
    (
        "aggregate_planning_signatures_and_unsupported",
        aggregate_planning::signatures_and_unsupported_shapes,
    ),
    (
        "aggregate_planning_depth_bounds",
        aggregate_planning::depth_before_lowering_and_clone,
    ),
    ("aggregate_shared_numeric_slots", shared_numeric_slots),
    ("aggregate_counts_nulls_and_empty", counts_and_empty),
    ("aggregate_ordered_values_and_distinct", ordered_values),
    ("aggregate_parameters_and_aliases", parameters_and_aliases),
    ("aggregate_merge_batches", merge_batches),
    ("aggregate_numeric_coercion", numeric_coercion),
    ("aggregate_typed_group_keys", group_keys),
    ("aggregate_depth_and_host_policy", depth_and_host_policy),
];
pub struct Host;
impl AggregateHost for Host {
    fn compare_values(&self, left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
        runtime_partial_cmp(left, right, &|_| None)
    }
    fn evaluate_static_projection(&self, projection: &Projection) -> Option<Value> {
        let (expression, _) = projection_to_expr(projection)?;
        evaluator::evaluate(&expression, &|_: &reddb_rql::ast::FieldRef| None).ok()
    }
}
fn projection(name: &str, argument: &str, alias: &str) -> Projection {
    Projection::Function(
        format!("{name}:{alias}"),
        vec![Projection::Column(argument.into())],
    )
}
fn state(projections: &[Projection], values: &[Value]) -> (CompiledAggPlan, SlottedAggState) {
    let plan = CompiledAggPlan::compile(projections).expect("admitted aggregate plan");
    let mut state = SlottedAggState::new(&plan);
    for value in values {
        state.count += 1;
        for slot in &plan.proj_slots {
            state.accumulate(&Host, slot, value.clone());
        }
    }
    (plan, state)
}
fn result(projection: &Projection, plan: &CompiledAggPlan, state: &SlottedAggState) -> Value {
    aggregate_projection_result_with_host(&Host, projection, state, plan)
        .expect("aggregate output")
        .1
}
fn shared_numeric_slots() {
    for names in [
        ["SUM", "AVG", "VARIANCE", "STDDEV"],
        ["STDDEV", "VARIANCE", "AVG", "SUM"],
    ] {
        let projections = names.map(|name| projection(name, "value", name));
        let (plan, state) = state(&projections, &[Value::Integer(10), Value::Integer(20)]);
        for projection in &projections {
            let name = match projection {
                Projection::Function(name, _) => base_function_name(name),
                _ => unreachable!(),
            };
            let expected = match name {
                "SUM" => Value::Integer(30),
                "AVG" => Value::Float(15.0),
                "VARIANCE" => Value::Float(25.0),
                "STDDEV" => Value::Float(5.0),
                _ => unreachable!(),
            };
            assert_eq!(result(projection, &plan, &state), expected, "{name}");
        }
    }
}

fn counts_and_empty() {
    let projections = [
        projection("COUNT", "*", "rows"),
        projection("COUNT", "value", "present"),
        projection("SUM", "value", "sum"),
        projection("AVG", "value", "avg"),
    ];
    let (plan, empty) = state(&projections, &[]);
    for (projection, expected) in projections.iter().zip([
        Value::Integer(0),
        Value::Integer(0),
        Value::Null,
        Value::Null,
    ]) {
        assert_eq!(result(projection, &plan, &empty), expected);
    }
    let (_, values) = state(
        &projections,
        &[Value::Null, Value::Integer(6), Value::text("ignored")],
    );
    for (projection, expected) in projections.iter().zip([
        Value::Integer(3),
        Value::Integer(2),
        Value::Integer(6),
        Value::Float(6.0),
    ]) {
        assert_eq!(result(projection, &plan, &values), expected);
    }
}
fn ordered_values() {
    let projections = ["MIN", "MAX", "FIRST", "LAST", "ARRAY_AGG", "COUNT_DISTINCT"]
        .map(|name| projection(name, "value", name));
    let inputs = [
        Value::Null,
        Value::Timestamp(20),
        Value::Timestamp(10),
        Value::Timestamp(20),
    ];
    let (plan, values) = state(&projections, &inputs);
    for (projection, expected) in projections.iter().zip([
        Value::Timestamp(10),
        Value::Timestamp(20),
        Value::Null,
        Value::Timestamp(20),
        Value::Array(inputs.to_vec()),
        Value::Integer(2),
    ]) {
        assert_eq!(result(projection, &plan, &values), expected);
    }
    let inputs = [
        Value::UnsignedInteger(9_007_199_254_740_993),
        Value::UnsignedInteger(9_007_199_254_740_992),
    ];
    let (_, exact) = state(&projections, &inputs);
    assert_eq!(result(&projections[0], &plan, &exact), inputs[1]);
    assert_eq!(result(&projections[1], &plan, &exact), inputs[0]);
}
fn parameters_and_aliases() {
    argument_identity();
    let projections = [
        projection("MEDIAN", "value", "middle"),
        Projection::Function(
            "PERCENTILE:quarter".into(),
            vec![
                Projection::Column("value".into()),
                Projection::Column("LIT:0.25".into()),
            ],
        ),
        Projection::Function(
            "GROUP_CONCAT:commas".into(),
            vec![
                Projection::Column("value".into()),
                Projection::Column("LIT:,".into()),
            ],
        ),
        Projection::Function(
            "STRING_AGG:pipes".into(),
            vec![
                Projection::Column("value".into()),
                Projection::Column("LIT:|".into()),
            ],
        ),
    ];
    let (plan, values) = state(
        &projections,
        &[Value::Integer(10), Value::Integer(20), Value::Integer(30)],
    );
    for (projection, expected) in projections.iter().zip([
        Value::Float(20.0),
        Value::Float(20.0),
        Value::text("10,20,30"),
        Value::text("10|20|30"),
    ]) {
        assert_eq!(result(projection, &plan, &values), expected);
    }
    assert_eq!(
        aggregate_projection_result_with_host(&Host, &projections[1], &values, &plan)
            .expect("named output")
            .0,
        "quarter"
    );
    assert_eq!(
        aggregate_output_name(&projection("SUM", "*", "total"), "SUM", "*"),
        "total"
    );
    assert_eq!(
        aggregate_output_name(
            &Projection::Function("COUNT".into(), vec![Projection::All]),
            "COUNT",
            "*"
        ),
        "count(*)"
    );
}
fn argument_identity() {
    let projections = [
        projection("SUM", "value", "column"),
        projection("SUM", "LIT:value", "literal"),
    ];
    let plan = CompiledAggPlan::compile(&projections).expect("typed arguments");
    let mut values = SlottedAggState::new(&plan);
    values.accumulate(&Host, &plan.proj_slots[0], Value::Integer(10));
    values.accumulate(&Host, &plan.proj_slots[1], Value::text("value"));
    assert_eq!(result(&projections[0], &plan, &values), Value::Integer(10));
    assert_eq!(result(&projections[1], &plan, &values), Value::Null);
    use reddb_rql::ast::{CompareOp, Expr, Filter};
    let nan = Projection::Expression(
        Box::new(Filter::CompareExpr {
            lhs: Expr::lit(Value::Float(f64::NAN)),
            op: CompareOp::Eq,
            rhs: Expr::lit(Value::Float(0.0)),
        }),
        None,
    );
    let projection = Projection::Function("SUM:nan_comparison".into(), vec![nan]);
    let (plan, values) = state(std::slice::from_ref(&projection), &[Value::Boolean(false)]);
    assert_eq!(result(&projection, &plan, &values), Value::Integer(0));
}

fn merge_batches() {
    let projections = [
        "COUNT",
        "SUM",
        "AVG",
        "VARIANCE",
        "STDDEV",
        "MIN",
        "MAX",
        "MEDIAN",
        "PERCENTILE",
        "GROUP_CONCAT",
        "STRING_AGG",
        "FIRST",
        "LAST",
        "ARRAY_AGG",
        "COUNT_DISTINCT",
    ]
    .map(|name| projection(name, "value", name));
    let inputs = [
        Value::Null,
        Value::Integer(10),
        Value::Integer(20),
        Value::Integer(30),
        Value::Integer(10),
    ];
    let (plan, whole) = state(&projections, &inputs);
    let (_, mut left) = state(&projections, &inputs[..2]);
    let (_, right) = state(&projections, &inputs[2..]);
    left.merge_with_host(&Host, right);
    for projection in &projections {
        assert_eq!(
            result(projection, &plan, &left),
            result(projection, &plan, &whole)
        );
    }
    assert_eq!(left.count, 5);
}
fn numeric_coercion() {
    for (input, expected) in [
        (Value::Integer(-3), -3.0),
        (Value::UnsignedInteger(5), 5.0),
        (Value::BigInt(9), 9.0),
        (Value::Float(0.25), 0.25),
        (Value::Decimal(12500), 1.25),
        (Value::Boolean(true), 1.0),
        (Value::Boolean(false), 0.0),
    ] {
        assert_eq!(aggregate_value_to_f64(&input), Some(expected));
    }
    for value in [
        Value::Null,
        Value::text("123"),
        Value::Timestamp(1),
        Value::Secret(vec![1]),
    ] {
        assert_eq!(aggregate_value_to_f64(&value), None);
    }
    let projections = [projection("SUM", "value", "sum")];
    for input in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let (plan, state) = state(&projections, &[Value::Float(input)]);
        let Value::Float(output) = result(&projections[0], &plan, &state) else {
            panic!("nonfinite sum retains float");
        };
        if input.is_nan() {
            assert!(output.is_nan());
        } else {
            assert_eq!(output, input);
        }
    }
}
fn group_keys() {
    use std::collections::HashSet;
    let inputs = [
        Value::UnsignedInteger(9_007_199_254_740_993),
        Value::UnsignedInteger(9_007_199_254_740_992),
        Value::Null,
        Value::text("null"),
        Value::Array(vec![Value::Integer(1)]),
        Value::Array(vec![Value::text("1")]),
    ];
    let keys: HashSet<_> = inputs
        .iter()
        .map(|value| build_aggregate_group_key(std::slice::from_ref(value)))
        .collect();
    assert_eq!(keys.len(), inputs.len());
    assert_eq!(
        build_aggregate_group_key(&inputs),
        build_aggregate_group_key(&inputs)
    );
}
fn depth_and_host_policy() {
    struct PolicyHost;
    impl AggregateHost for PolicyHost {
        fn compare_values(&self, _: &Value, _: &Value) -> Option<std::cmp::Ordering> {
            None
        }
        fn evaluate_static_projection(&self, _: &Projection) -> Option<Value> {
            Some(Value::Float(1.0))
        }
    }
    let projections = [
        projection("MIN", "value", "min"),
        Projection::Function(
            "PERCENTILE:maximum".into(),
            vec![
                Projection::Column("value".into()),
                Projection::Column("LIT:0".into()),
            ],
        ),
    ];
    let plan = CompiledAggPlan::compile(&projections).expect("bounded plan");
    let mut values = SlottedAggState::new(&plan);
    for value in [Value::Integer(30), Value::Integer(10)] {
        for slot in &plan.proj_slots {
            values.accumulate(&PolicyHost, slot, value.clone());
        }
    }
    assert_eq!(
        aggregate_projection_result_with_host(&PolicyHost, &projections[0], &values, &plan)
            .expect("host min")
            .1,
        Value::Integer(30)
    );
    assert_eq!(
        aggregate_projection_result_with_host(&PolicyHost, &projections[1], &values, &plan)
            .expect("host percentile")
            .1,
        Value::Float(30.0)
    );
    let mut nested = Projection::Column("value".into());
    for _ in 0..reddb_rql::limits::JSON_LITERAL_MAX_DEPTH - 1 {
        nested = Projection::Function("ABS".into(), vec![nested]);
    }
    let accepted = Projection::Function("SUM".into(), vec![nested.clone()]);
    assert!(CompiledAggPlan::compile(&[accepted]).is_some());
    let rejected = Projection::Function(
        "SUM".into(),
        vec![Projection::Function("ABS".into(), vec![nested])],
    );
    assert!(CompiledAggPlan::compile(&[rejected]).is_none());
    assert!(
        aggregate_projection_result_with_host(&Host, &Projection::All, &values, &plan).is_none()
    );
}

pub fn verify_recovered(
    records: &[reddb_engine::query::unified::UnifiedRecord],
    contract: &reddb_engine::catalog::CollectionContract,
    updated: bool,
) {
    use reddb_engine::query::planning::aggregation::prepare_aggregate_runtime_plan;
    use reddb_engine::query::runtime::filter::resolve_filter_row_value;
    use reddb_rql::sql_lowering::effective_table_projections;
    use std::collections::HashMap;
    let query = super::record_filters::table_query("SELECT o.meta.tenant AS tenant, COUNT(*) AS n, SUM(total) AS amount, AVG(total) AS mean, MEDIAN(total) AS middle FROM orders AS o WHERE quantity > 0 GROUP BY tenant HAVING MAX(total) >= 15 ORDER BY MAX(quantity) DESC, tenant ASC LIMIT 1 OFFSET 1");
    let runtime =
        prepare_aggregate_runtime_plan(&query).expect("recovered native aggregate planning");
    let all_visible_projections = effective_table_projections(&query);
    let visible_projections: Vec<_> = all_visible_projections
        .clone()
        .into_iter()
        .filter(|projection| has_aggregate_projections(std::slice::from_ref(projection)))
        .collect();
    let projections = &runtime.aggregate_projections;
    let plan = CompiledAggPlan::compile(projections).expect("recovered aggregate plan");
    let mut groups = HashMap::new();
    for record in records {
        if !super::record_filters::matches_where(record, &query, Some(contract))
            .expect("recovered filter")
        {
            continue;
        }
        let evaluate = |expression: &reddb_rql::ast::Expr| {
            evaluator::evaluate(expression, &|field: &reddb_rql::ast::FieldRef| {
                resolve_filter_row_value(
                    record,
                    field,
                    Some("orders"),
                    Some("o"),
                    &super::record_filters::plaintext,
                    |_, _| false,
                )
            })
            .expect("recovered aggregate expression")
        };
        let group_values: Vec<_> = runtime
            .group_bindings
            .iter()
            .map(|binding| evaluate(&binding.expression))
            .collect();
        let key = build_aggregate_group_key(&group_values);
        let (_, state) = groups
            .entry(key)
            .or_insert_with(|| (group_values, SlottedAggState::new(&plan)));
        state.count += 1;
        for (projection, slot) in projections.iter().zip(&plan.proj_slots) {
            if matches!(slot, ProjSlot::CountStar | ProjSlot::Shared) {
                continue;
            }
            let Projection::Function(_, arguments) = projection else {
                unreachable!()
            };
            let (expression, _) = projection_to_expr(&arguments[0]).expect("aggregate argument");
            state.accumulate(&Host, slot, evaluate(&expression));
        }
    }
    assert_eq!(groups.len(), 2);
    for (tenant, amount, mean) in [
        (
            "red",
            if updated { 45 } else { 25 },
            if updated { 22.5 } else { 12.5 },
        ),
        ("blue", 25, 12.5),
    ] {
        let key = build_aggregate_group_key(&[Value::text(tenant)]);
        let (_, state) = groups.get(&key).expect("recovered tenant group");
        for (projection, expected) in visible_projections.iter().zip([
            Value::Integer(2),
            Value::Integer(amount),
            Value::Float(mean),
            Value::Float(mean),
        ]) {
            assert_eq!(result(projection, &plan, state), expected);
        }
    }
    use reddb_engine::query::runtime::aggregate_results::*;
    let result = finish_aggregate_results_with_host(
        &aggregate_result_cases::ResultHost::default(),
        AggregateResultContext {
            query: &query,
            visible_projections: &all_visible_projections,
            runtime_plan: &runtime,
            slots: &plan,
            sensitivity_source: None,
        },
        groups.values().map(|(values, state)| AggregateGroupRef {
            group_values: values,
            state,
        }),
    )
    .expect("recovered canonical aggregate result phase");
    assert_eq!(result.columns, ["tenant", "n", "amount", "mean", "middle"]);
    let selected = result.records;
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].get("tenant"), Some(&Value::text("blue")));
    assert_eq!(selected[0].get("amount"), Some(&Value::Integer(25)));
    assert_eq!(selected[0].get("max(total)"), Some(&Value::Integer(15)));
    assert_eq!(selected[0].get("max(quantity)"), Some(&Value::Integer(3)));
}
