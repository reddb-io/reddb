//! Native/WASM coverage for the canonical window phase and policy boundaries.
use reddb_engine::query::{
    evaluator,
    runtime::{field_resolution::resolve_runtime_field, window_phase::*},
    unified::UnifiedRecord,
};
use reddb_rql::ast::{
    Expr, FieldRef, Projection, WindowFrame, WindowFrameBound, WindowFrameUnit, WindowOrderItem,
    WindowSpec,
};
use reddb_types::Value;
use std::cell::Cell;

pub const CASES: &[(&str, fn())] = &[
    ("window_ranks_partition_order_and_ties", ranks),
    ("window_lag_lead_values_and_defaults", lag_lead),
    ("window_aggregate_frames_and_peers", aggregates),
    ("window_wide_offsets_and_bounds", wide_offsets),
    ("window_errors_and_publication_order", errors),
    ("window_materialization_admission", admission),
    ("window_cross_row_secret_policy", secrets),
    (
        "window_projection_depth_and_subtraction",
        depth_and_subtraction,
    ),
    (
        "window_fallible_inputs_and_retry",
        fallible_inputs_and_retry,
    ),
];

#[derive(Default)]
struct Host {
    guards: Cell<usize>,
    evaluations: Cell<usize>,
    outputs: Cell<usize>,
    deny_materialization: bool,
    deny_output: bool,
    failure_at: Cell<Option<usize>>,
}
impl WindowHost for Host {
    type Error = String;
    fn guard_materialization(&self, _: usize) -> Result<(), String> {
        self.guards.set(self.guards.get() + 1);
        if self.deny_materialization {
            Err("window limit".into())
        } else {
            Ok(())
        }
    }
    fn evaluate_expression(&self, expression: &Expr, source: &UnifiedRecord) -> Option<Value> {
        self.evaluations.set(self.evaluations.get() + 1);
        evaluator::evaluate(expression, &|field: &_| {
            resolve_runtime_field(source, field, Some("orders"), Some("o")).map(|value| {
                if matches!(value, Value::Secret(_)) {
                    Value::Integer(7)
                } else {
                    value
                }
            })
        })
        .ok()
    }
    fn try_evaluate_expression(
        &self,
        expression: &Expr,
        source: &UnifiedRecord,
    ) -> Result<Option<Value>, String> {
        if self.failure_at.get() == Some(self.evaluations.get() + 1) {
            self.evaluations.set(self.evaluations.get() + 1);
            return Err("window input read failed".into());
        }
        Ok(self.evaluate_expression(expression, source))
    }
    fn secret_query_output(&self, value: Value) -> Result<Value, String> {
        self.outputs.set(self.outputs.get() + 1);
        if self.deny_output {
            Err("secret output denied".into())
        } else {
            Ok(value)
        }
    }
    fn query_error(&self, message: String) -> String {
        message
    }
}
fn row(group: &str, key: i64, value: Value) -> UnifiedRecord {
    let mut record = UnifiedRecord::new();
    record.set("team", Value::text(group));
    record.set("key", Value::Integer(key));
    record.set("value", value);
    record
}
fn field(name: &str) -> Expr {
    Expr::col(FieldRef::TableColumn {
        table: String::new(),
        column: name.into(),
    })
}
fn spec(partition: bool) -> WindowSpec {
    WindowSpec {
        partition_by: if partition {
            vec![field("team")]
        } else {
            vec![]
        },
        order_by: vec![WindowOrderItem {
            expr: field("key"),
            ascending: true,
            nulls_first: false,
        }],
        frame: None,
    }
}
fn window(name: &str, args: Vec<Projection>, spec: WindowSpec, label: &str) -> Projection {
    Projection::Window {
        name: name.into(),
        args,
        window: Box::new(spec),
        alias: Some(label.into()),
    }
}
fn column() -> Projection {
    Projection::Column("value".into())
}
fn literal(value: i64) -> Projection {
    Projection::Column(format!("LIT:{value}"))
}
fn run(host: &Host, rows: &mut [UnifiedRecord], projections: &[Projection]) -> Result<(), String> {
    apply_with_host(host, rows, projections, Some("orders"), Some("o"))
}
fn values(rows: &[UnifiedRecord], label: &str) -> Vec<Value> {
    rows.iter()
        .map(|row| row.get(label).expect("window output").clone())
        .collect()
}
fn integers(values: &[i64]) -> Vec<Value> {
    values.iter().copied().map(Value::Integer).collect()
}
fn ranks() {
    let mut rows = vec![
        row("a", 2, Value::Null),
        row("b", 9, Value::Null),
        row("a", 1, Value::Null),
        row("a", 1, Value::Null),
    ];
    run(
        &Host::default(),
        &mut rows,
        &[
            window("ROW_NUMBER", vec![], spec(true), "rn"),
            window("RANK", vec![], spec(true), "rank"),
            window("DENSE_RANK", vec![], spec(true), "dense"),
        ],
    )
    .expect("rank windows");
    assert_eq!(values(&rows, "rn"), integers(&[3, 1, 1, 2]));
    assert_eq!(values(&rows, "rank"), integers(&[3, 1, 1, 1]));
    assert_eq!(values(&rows, "dense"), integers(&[2, 1, 1, 1]));
    let mut descending = spec(false);
    descending.order_by[0].ascending = false;
    run(
        &Host::default(),
        &mut rows,
        &[window("ROW_NUMBER", vec![], descending, "desc")],
    )
    .expect("descending");
    assert_eq!(values(&rows, "desc"), integers(&[2, 1, 3, 4]));
    // NULL placement retains native comparator behavior in both directions.
    rows[0].set("key", Value::Null);
    let mut nulls = spec(false);
    nulls.order_by[0].nulls_first = true;
    run(
        &Host::default(),
        &mut rows,
        &[window("ROW_NUMBER", vec![], nulls, "null_first")],
    )
    .expect("null order");
    assert_eq!(values(&rows, "null_first"), integers(&[1, 4, 2, 3]));
}
fn lag_lead() {
    let mut rows = vec![
        row("a", 2, Value::UnsignedInteger(9_007_199_254_740_993)),
        row("a", 1, Value::Timestamp(123)),
        row("b", 1, Value::Null),
    ];
    run(
        &Host::default(),
        &mut rows,
        &[
            window(
                "LAG",
                vec![column(), literal(1), literal(-1)],
                spec(true),
                "lag",
            ),
            window("LEAD", vec![column()], spec(true), "lead"),
            window("LAG", vec![column(), literal(0)], spec(true), "self"),
        ],
    )
    .expect("lag/lead");
    assert_eq!(
        values(&rows, "lag"),
        vec![
            Value::Timestamp(123),
            Value::Integer(-1),
            Value::Integer(-1)
        ]
    );
    assert_eq!(
        values(&rows, "lead"),
        vec![
            Value::Null,
            Value::UnsignedInteger(9_007_199_254_740_993),
            Value::Null
        ]
    );
    assert_eq!(values(&rows, "self"), values(&rows, "value"));
}
fn aggregates() {
    let mut rows = vec![
        row("a", 1, Value::Integer(10)),
        row("a", 1, Value::Null),
        row("a", 2, Value::Integer(30)),
        row("b", 1, Value::Integer(90)),
    ];
    let projections = ["SUM", "COUNT", "AVG", "MIN", "MAX"]
        .map(|name| window(name, vec![column()], spec(true), name));
    run(&Host::default(), &mut rows, &projections).expect("aggregate peers");
    assert_eq!(values(&rows, "SUM"), integers(&[10, 10, 40, 90]));
    assert_eq!(values(&rows, "COUNT"), integers(&[1, 1, 2, 1]));
    assert_eq!(
        values(&rows, "AVG"),
        vec![
            Value::Float(10.0),
            Value::Float(10.0),
            Value::Float(20.0),
            Value::Float(90.0)
        ]
    );
    assert_eq!(values(&rows, "MIN"), integers(&[10, 10, 10, 90]));
    assert_eq!(values(&rows, "MAX"), integers(&[10, 10, 30, 90]));
    let mut unordered = spec(true);
    unordered.order_by.clear();
    let mut sliding = spec(true);
    sliding.frame = Some(WindowFrame {
        unit: WindowFrameUnit::Rows,
        start: WindowFrameBound::Preceding(Box::new(Expr::lit(Value::Integer(1)))),
        end: Some(WindowFrameBound::CurrentRow),
    });
    let mut cumulative = spec(true);
    cumulative.frame = Some(WindowFrame {
        unit: WindowFrameUnit::Rows,
        start: WindowFrameBound::UnboundedPreceding,
        end: None,
    });
    run(
        &Host::default(),
        &mut rows,
        &[
            window("COUNT", vec![Projection::All], spec(true), "all"),
            window("SUM", vec![column()], unordered, "whole"),
            window("SUM", vec![column()], sliding, "sliding"),
            window("COUNT", vec![Projection::All], cumulative, "cumulative"),
        ],
    )
    .expect("frame modes");
    assert_eq!(values(&rows, "all"), integers(&[2, 2, 3, 1]));
    assert_eq!(values(&rows, "whole"), integers(&[40, 40, 40, 90]));
    assert_eq!(values(&rows, "sliding"), integers(&[10, 10, 30, 90]));
    assert_eq!(values(&rows, "cumulative"), integers(&[1, 2, 3, 1]));
    let mut null_rows = vec![row("a", 1, Value::Null)];
    run(&Host::default(), &mut null_rows, &projections).expect("all null frame");
    for name in ["SUM", "AVG", "MIN", "MAX"] {
        assert_eq!(values(&null_rows, name), vec![Value::Null]);
    }
    assert_eq!(values(&null_rows, "COUNT"), integers(&[0]));
}
fn wide_offsets() {
    let host = Host::default();
    for offset in [
        Value::Integer(i64::MAX),
        Value::UnsignedInteger(u64::MAX),
        Value::UnsignedInteger(1u64 << 32),
    ] {
        let mut rows = vec![
            row("a", 1, Value::Integer(10)),
            row("a", 2, Value::Integer(20)),
        ];
        // Offset expression resolves from the first input record, as on native.
        for row in &mut rows {
            row.set("offset", offset.clone());
        }
        for name in ["LAG", "LEAD"] {
            run(
                &host,
                &mut rows,
                &[window(
                    name,
                    vec![column(), Projection::Column("offset".into()), literal(-1)],
                    spec(false),
                    name,
                )],
            )
            .expect("wide lag/lead");
            assert_eq!(values(&rows, name), integers(&[-1, -1]));
        }
        let mut frame = spec(false);
        frame.frame = Some(WindowFrame {
            unit: WindowFrameUnit::Rows,
            start: WindowFrameBound::Preceding(Box::new(Expr::lit(offset))),
            end: Some(WindowFrameBound::CurrentRow),
        });
        run(
            &host,
            &mut rows,
            &[window("SUM", vec![column()], frame, "sum")],
        )
        .expect("wide preceding");
        assert_eq!(values(&rows, "sum"), integers(&[10, 30]));
    }
}
fn errors() {
    let mut rows = vec![row("a", 1, Value::Integer(10))];
    for (name, args, diagnostic) in [
        ("LAG", vec![], "requires at least one argument"),
        (
            "LEAD",
            vec![column(), literal(-1)],
            "offset must be non-negative",
        ),
        (
            "LAG",
            vec![column(), Projection::Column("LIT:oops".into())],
            "offset must evaluate to an integer",
        ),
        ("UNKNOWN", vec![], "is not supported"),
    ] {
        let error = run(
            &Host::default(),
            &mut rows,
            &[window(name, args, spec(false), "bad")],
        )
        .expect_err("invalid window");
        assert!(error.contains(diagnostic), "{error}");
        assert!(rows[0].get("bad").is_none());
    }
    let mut unsupported = spec(false);
    unsupported.frame = Some(WindowFrame {
        unit: WindowFrameUnit::Rows,
        start: WindowFrameBound::CurrentRow,
        end: Some(WindowFrameBound::UnboundedFollowing),
    });
    let error = run(
        &Host::default(),
        &mut rows,
        &[
            window("ROW_NUMBER", vec![], spec(false), "good"),
            window("SUM", vec![column()], unsupported, "bad"),
        ],
    )
    .expect_err("unsupported frame");
    assert!(error.contains("window frame variant not supported"));
    assert_eq!(rows[0].get("good"), Some(&Value::Integer(1)));
    assert!(rows[0].get("bad").is_none(), "failed column not published");
}
fn admission() {
    let host = Host {
        deny_materialization: true,
        ..Host::default()
    };
    let projection = window("ROW_NUMBER", vec![], spec(false), "rn");
    run(&host, &mut [], std::slice::from_ref(&projection)).expect("empty bypass");
    let mut rows = vec![row("a", 1, Value::Null)];
    run(&host, &mut rows, &[column()]).expect("plain projection bypass");
    assert_eq!(host.guards.get(), 0);
    assert_eq!(
        run(&host, &mut rows, &[projection]),
        Err("window limit".into())
    );
    assert_eq!(host.guards.get(), 1);
    assert_eq!(host.evaluations.get(), 0);
    assert!(rows[0].get("rn").is_none());
}
fn secrets() {
    let projections = [window("LAG", vec![column()], spec(false), "previous")];
    let source = vec![
        row("a", 1, Value::Secret(vec![1])),
        row("a", 2, Value::Null),
    ];
    let host = Host::default();
    let mut rows = source.clone();
    run(&host, &mut rows, &projections).expect("secret output policy");
    assert_eq!(
        values(&rows, "previous"),
        vec![Value::Null, Value::Integer(7)]
    );
    assert_eq!(
        host.outputs.get(),
        2,
        "all destination rows remain sensitive"
    );
    let host = Host {
        deny_output: true,
        ..Host::default()
    };
    assert_eq!(
        run(&host, &mut source.clone(), &projections),
        Err("secret output denied".into())
    );
    let mut public = vec![row("a", 1, Value::Integer(3))];
    run(&host, &mut public, &projections).expect("public window bypasses output policy");
    assert_eq!(host.outputs.get(), 1);
}
fn depth_and_subtraction() {
    let mut rows = vec![row("a", 1, Value::Integer(7))];
    let subtraction = Projection::Function("SUB".into(), vec![literal(0), literal(1)]);
    let overflow = Projection::Function("SUB".into(), vec![literal(i64::MIN), literal(1)]);
    run(
        &Host::default(),
        &mut rows,
        &[
            window(
                "LAG",
                vec![column(), literal(1), subtraction],
                spec(false),
                "negative",
            ),
            window(
                "LAG",
                vec![column(), literal(1), overflow],
                spec(false),
                "overflow",
            ),
        ],
    )
    .expect("checked defaults");
    assert_eq!(values(&rows, "negative"), integers(&[-1]));
    assert_eq!(values(&rows, "overflow"), vec![Value::Null]);
    let mut nested = literal(1);
    for _ in 0..=reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        nested = Projection::Function("SUB".into(), vec![literal(0), nested]);
    }
    let host = Host::default();
    assert_eq!(
        run(
            &host,
            &mut rows,
            &[window(
                "LAG",
                vec![column(), literal(1), nested],
                spec(false),
                "deep"
            )]
        ),
        Err("window projection exceeds nesting limit".into())
    );
    assert_eq!(host.evaluations.get(), 0);
    assert!(rows[0].get("deep").is_none());
}

fn fallible_inputs_and_retry() {
    for function in ["SUM", "LAG"] {
        let arguments = if function == "SUM" {
            vec![column()]
        } else {
            vec![
                Projection::Function("ABS".into(), vec![column()]),
                Projection::Function("ABS".into(), vec![literal(1)]),
                Projection::Function("ABS".into(), vec![literal(9)]),
            ]
        };
        let projection = window(function, arguments, spec(true), "result");
        let evaluations = if function == "SUM" { 9 } else { 11 };
        for failure_at in 1..=evaluations {
            let host = Host::default();
            host.failure_at.set(Some(failure_at));
            let mut rows = vec![
                row("g", 1, Value::Integer(3)),
                row("g", 2, Value::Integer(4)),
                row("g", 3, Value::Integer(5)),
            ];
            assert_eq!(
                run(&host, &mut rows, std::slice::from_ref(&projection)),
                Err("window input read failed".into())
            );
            assert_eq!(host.evaluations.get(), failure_at);
            assert!(rows.iter().all(|row| row.get("result").is_none()));
            assert_eq!(host.outputs.get(), 0);
            host.failure_at.set(None);
            host.evaluations.set(0);
            run(&host, &mut rows, std::slice::from_ref(&projection)).expect("retry window reads");
            let expected = if function == "SUM" {
                [3, 7, 12]
            } else {
                [9, 3, 4]
            };
            assert_eq!(values(&rows, "result"), integers(&expected));
        }
    }
}
