//! Canonical GROUP BY metadata and host admission, executed natively and in WASM.
use super::*;
use reddb_engine::query::{planning::aggregation::*, runtime::aggregate_results::*};
use reddb_rql::{
    ast::{Expr, FieldRef, TableQuery},
    sql_lowering::effective_table_projections,
};
use std::cell::RefCell;

#[derive(Default)]
struct ShapeHost {
    calls: RefCell<Vec<&'static str>>,
    deny_projection: bool,
    deny_group: bool,
}
impl AggregateShapeHost for ShapeHost {
    type Error = String;
    fn validate_projection(&self, _: &Projection) -> Result<(), String> {
        self.calls.borrow_mut().push("projection");
        if self.deny_projection {
            Err("projection policy denied".into())
        } else {
            Ok(())
        }
    }
    fn validate_group_expression(&self, _: &Expr) -> Result<(), String> {
        self.calls.borrow_mut().push("group");
        if self.deny_group {
            Err("group policy denied".into())
        } else {
            Ok(())
        }
    }
    fn query_error(&self, message: String) -> String {
        self.calls.borrow_mut().push("query_error");
        message
    }
}
fn query(sql: &str) -> TableQuery {
    super::super::record_filters::table_query(sql)
}
fn field(name: &str) -> FieldRef {
    FieldRef::TableColumn {
        table: String::new(),
        column: name.into(),
    }
}
fn column(name: &str) -> Expr {
    Expr::col(field(name))
}

pub(super) fn alias_resolution() {
    let aliased = query("SELECT category AS tenant, COUNT(*) AS n FROM orders GROUP BY TENANT");
    let bindings = prepare_aggregate_group_bindings(&aliased).expect("case-insensitive alias");
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].expression, column("category"));
    assert_eq!(bindings[0].key.as_deref(), Some("category"));
    assert_eq!(bindings[0].output_label, "tenant");
    validate_aggregate_projection_shape_with_host(&ShapeHost::default(), &aliased)
        .expect("aliased group shape");
    let qualified =
        query("SELECT category AS tenant, COUNT(*) AS n FROM orders AS o GROUP BY o.tenant");
    let binding =
        prepare_aggregate_group_bindings(&qualified).expect("qualified field stays a field");
    assert!(
        matches!(&binding[0].expression, Expr::Column { field: FieldRef::TableColumn { table, column }, .. } if table == "o" && column == "tenant")
    );
    assert!(
        validate_aggregate_projection_shape_with_host(&ShapeHost::default(), &qualified).is_err(),
        "qualified names must not become SELECT aliases"
    );
    let mut legacy = aliased.clone();
    legacy.group_by_exprs.clear();
    legacy.group_by = vec!["TENANT".into()];
    let binding = prepare_aggregate_group_bindings(&legacy).expect("legacy GROUP BY authority");
    assert_eq!(binding[0].key.as_deref(), Some("category"));
    legacy.group_by_exprs = vec![column("category")];
    legacy.group_by = vec!["ignored".into()];
    assert_eq!(
        prepare_aggregate_group_bindings(&legacy).expect("canonical group authority")[0]
            .key
            .as_deref(),
        Some("category")
    );
}

pub(super) fn expression_labels() {
    for sql in [
        "SELECT ABS(quantity) AS magnitude, COUNT(*) AS n FROM orders GROUP BY magnitude",
        "SELECT ABS(quantity) AS magnitude, COUNT(*) AS n FROM orders GROUP BY ABS(quantity)",
    ] {
        let query = query(sql);
        validate_aggregate_projection_shape_with_host(&ShapeHost::default(), &query)
            .expect("function grouping");
        let plan = prepare_aggregate_runtime_plan(&query).expect("prepared function binding");
        assert_eq!(plan.group_bindings[0].key.as_deref(), Some("ABS(quantity)"));
        assert_eq!(plan.group_bindings[0].output_label, "magnitude");
    }
    let json =
        query("SELECT o.meta.tenant AS tenant, COUNT(*) AS n FROM orders AS o GROUP BY tenant");
    validate_aggregate_projection_shape_with_host(&ShapeHost::default(), &json)
        .expect("JSON alias shape");
    let binding = prepare_aggregate_group_bindings(&json).expect("JSON alias binding");
    assert_eq!(binding[0].output_label, "tenant");
    assert_eq!(
        binding[0].key,
        render_expr_signature(&binding[0].expression)
    );
    let mut legacy = query("SELECT COUNT(*) FROM orders");
    legacy.select_items.clear();
    legacy.group_by_exprs.clear();
    legacy.columns = vec![
        Projection::Function(
            "TIME_BUCKET:window".into(),
            vec![
                Projection::Column("LIT:1h".into()),
                Projection::Column("timestamp_ns".into()),
            ],
        ),
        projection("COUNT", "*", "n"),
    ];
    legacy.group_by = vec!["TIME_BUCKET(1h,timestamp_ns)".into()];
    validate_aggregate_projection_shape_with_host(&ShapeHost::default(), &legacy)
        .expect("legacy TIME_BUCKET matching");
    let binding = prepare_aggregate_group_bindings(&legacy).expect("legacy time binding");
    assert_eq!(
        binding[0].key.as_deref(),
        Some("TIME_BUCKET(1h,timestamp_ns)")
    );
    assert_eq!(binding[0].output_label, "window");
}

pub(super) fn shape_and_host_admission() {
    let invalid = query("SELECT category, SUM(quantity) FROM orders");
    let host = ShapeHost::default();
    assert_eq!(
        validate_aggregate_projection_shape_with_host(&host, &invalid)
            .expect_err("ungrouped field"),
        "projection `category` must be an aggregate because the query contains aggregate functions"
    );
    assert_eq!(&*host.calls.borrow(), &["projection", "query_error"]);
    let invalid = query("SELECT category, SUM(quantity) FROM orders GROUP BY tenant");
    assert_eq!(
        validate_aggregate_projection_shape_with_host(&ShapeHost::default(), &invalid)
            .expect_err("wrong group"),
        "projection `category` must appear in GROUP BY or be an aggregate"
    );
    let valid =
        query("SELECT category AS tenant, SUM(quantity) AS amount FROM orders GROUP BY tenant");
    let host = ShapeHost::default();
    validate_aggregate_projection_shape_with_host(&host, &valid)
        .expect("structural and host validation");
    assert_eq!(
        &*host.calls.borrow(),
        &["projection", "projection", "group"]
    );
    let host = ShapeHost {
        deny_projection: true,
        ..Default::default()
    };
    assert_eq!(
        validate_aggregate_projection_shape_with_host(&host, &valid)
            .expect_err("projection callback"),
        "projection policy denied"
    );
    assert_eq!(&*host.calls.borrow(), &["projection"]);
    let host = ShapeHost {
        deny_group: true,
        ..Default::default()
    };
    assert_eq!(
        validate_aggregate_projection_shape_with_host(&host, &valid).expect_err("group callback"),
        "group policy denied"
    );
    assert_eq!(
        &*host.calls.borrow(),
        &["projection", "projection", "group"]
    );
}

pub(super) fn depth_limits() {
    let mut query = query("SELECT category AS tenant, COUNT(*) AS n FROM orders GROUP BY tenant");
    let mut nested = column("category");
    for _ in 0..=reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        nested = Expr::FunctionCall {
            name: "ABS".into(),
            args: vec![nested],
            span: reddb_rql::ast::Span::synthetic(),
        };
    }
    let reddb_rql::ast::SelectItem::Expr { expr, .. } = &mut query.select_items[0] else {
        unreachable!()
    };
    *expr = nested;
    assert!(prepare_aggregate_group_bindings(&query).is_err());
    assert!(prepare_aggregate_runtime_plan(&query).is_err());
    let host = ShapeHost::default();
    assert!(validate_aggregate_projection_shape_with_host(&host, &query).is_err());
    assert_eq!(
        &*host.calls.borrow(),
        &["query_error"],
        "bounds precede backend callbacks"
    );
}

pub(super) fn result_metadata() {
    let query = query("SELECT SUM(quantity) AS amount, category AS tenant, region AS zone FROM orders GROUP BY tenant, zone");
    let runtime = prepare_aggregate_runtime_plan(&query).expect("multi-group metadata");
    let slots = CompiledAggPlan::compile(&runtime.aggregate_projections).expect("slot plan");
    let (_, state) = state(
        &runtime.aggregate_projections,
        &[Value::Integer(10), Value::Integer(20)],
    );
    let values = [Value::text("red"), Value::text("east")];
    let projections = effective_table_projections(&query);
    let result = finish_aggregate_results_with_host(
        &aggregate_result_cases::ResultHost::default(),
        AggregateResultContext {
            query: &query,
            visible_projections: &projections,
            runtime_plan: &runtime,
            slots: &slots,
            sensitivity_source: None,
        },
        std::iter::once(AggregateGroupRef {
            group_values: &values,
            state: &state,
        }),
    )
    .expect("compiled result bindings");
    assert_eq!(result.columns, ["tenant", "zone", "amount"]);
    assert_eq!(result.records[0].get("tenant"), Some(&Value::text("red")));
    assert_eq!(result.records[0].get("category"), Some(&Value::text("red")));
    assert_eq!(result.records[0].get("zone"), Some(&Value::text("east")));
    assert_eq!(result.records[0].get("region"), Some(&Value::text("east")));
    assert_eq!(result.records[0].get("amount"), Some(&Value::Integer(30)));
}
