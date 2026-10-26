//! Shared native projection traversal, including host policy boundaries.
use reddb_engine::catalog::CollectionContract;
use reddb_engine::query::{
    evaluator::{self, EvalError},
    runtime::{
        filter::{evaluate_runtime_filter_result_with_host, resolve_filter_row_value, FilterHost},
        projection::*,
        projection_value::*,
        scalar_functions::*,
    },
    unified::{MatchedEdge, MatchedNode, UnifiedRecord, VectorSearchResult},
};
use reddb_rql::ast::{CompareOp, Expr, FieldRef, Filter, Projection};
use reddb_types::Value;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    sync::Arc,
};

#[path = "table_plan_projection.rs"]
mod table_plan_projection;

#[path = "geographic_projection.rs"]
mod geographic_projection;

pub const CASES: &[(&str, fn())] = &[
    (
        "scalar_geo_aliases_and_operations",
        geographic_projection::aliases_and_operations,
    ),
    (
        "scalar_geo_argument_forms",
        geographic_projection::argument_forms,
    ),
    (
        "scalar_geo_input_policy",
        geographic_projection::input_policy,
    ),
    ("scalar_dispatch_clock_values", scalar_clock_values),
    (
        "scalar_dispatch_time_bucket_inputs",
        scalar_time_bucket_inputs,
    ),
    (
        "scalar_dispatch_substrings_and_patterns",
        scalar_substrings_and_patterns,
    ),
    (
        "scalar_dispatch_trim_slice_and_quote",
        scalar_trim_slice_and_quote,
    ),
    (
        "scalar_dispatch_arithmetic_and_cast",
        scalar_arithmetic_and_cast,
    ),
    (
        "scalar_dispatch_case_and_coalesce_lazy",
        scalar_case_and_coalesce,
    ),
    ("scalar_dispatch_text_and_lengths", scalar_text_and_lengths),
    ("scalar_dispatch_money", scalar_money),
    (
        "scalar_dispatch_missing_and_host_fallback",
        scalar_missing_and_fallback,
    ),
    ("projection_value_legacy_literals", legacy_literals),
    (
        "projection_value_resolution_and_fallback",
        value_resolution_and_fallback,
    ),
    ("projection_value_secret_input_order", secret_input_order),
    (
        "record_projection_fields_aliases_and_literals",
        fields_aliases_and_literals,
    ),
    (
        "record_projection_star_and_column_layout",
        star_and_column_layout,
    ),
    ("record_projection_document_paths", document_paths),
    (
        "record_projection_bindings_and_window_output",
        bindings_and_window_output,
    ),
    (
        "record_projection_typed_values_and_labels",
        typed_values_and_labels,
    ),
    (
        "record_projection_fallback_and_data_errors",
        fallback_and_data_errors,
    ),
    ("record_projection_secret_sensitivity", secret_sensitivity),
    (
        "record_projection_output_policy_and_redaction",
        output_policy_and_redaction,
    ),
];

struct Host<'a> {
    contract: Option<&'a CollectionContract>,
    forced_error: Option<EvalError>,
    fallback_calls: Cell<usize>,
    output_calls: Cell<usize>,
    deny_output: bool,
    input_calls: Cell<usize>,
    deny_input: bool,
    resolved_columns: RefCell<Vec<String>>,
    unix_ms: Option<i64>,
    clock_reads: Cell<usize>,
}

impl<'a> Host<'a> {
    fn new(contract: Option<&'a CollectionContract>) -> Self {
        Self {
            contract,
            forced_error: None,
            fallback_calls: Cell::new(0),
            output_calls: Cell::new(0),
            deny_output: false,
            input_calls: Cell::new(0),
            deny_input: false,
            resolved_columns: RefCell::new(vec![]),
            unix_ms: None,
            clock_reads: Cell::new(0),
        }
    }
}

impl ProjectionHost for Host<'_> {
    type Error = String;
    fn evaluate_expression(
        &self,
        expression: &Expr,
        source: &UnifiedRecord,
    ) -> Result<Value, EvalError> {
        if let Some(error) = &self.forced_error {
            return Err(error.clone());
        }
        evaluator::evaluate(expression, &|field: &FieldRef| {
            resolve_filter_row_value(
                source,
                field,
                Some("orders"),
                Some("o"),
                &super::record_filters::plaintext,
                |collection, name| {
                    self.contract.is_some_and(|contract| {
                        contract.name == collection
                            && contract
                                .declared_columns
                                .iter()
                                .any(|column| column.name == name && !column.not_null)
                    })
                },
            )
        })
    }
    fn evaluate_filter(&self, _: &Filter, _: &UnifiedRecord) -> bool {
        self.fallback_calls.set(self.fallback_calls.get() + 1);
        true
    }
    fn evaluate_function(
        &self,
        name: &str,
        arguments: &[Projection],
        source: &UnifiedRecord,
    ) -> Option<Value> {
        self.fallback_calls.set(self.fallback_calls.get() + 1);
        evaluate_scalar_function_with_host(self, name, arguments, source)
    }

    fn evaluate_literal(&self, column: &str, _: &UnifiedRecord) -> Option<Value> {
        decode_projection_literal(column)
    }
    fn evaluation_error(&self, error: EvalError, sensitive: bool) -> String {
        if sensitive {
            "secret expression evaluation failed".into()
        } else {
            error.to_string()
        }
    }
    fn secret_query_output(&self, value: Value) -> Result<Value, String> {
        self.output_calls.set(self.output_calls.get() + 1);
        if self.deny_output {
            Err("secret output denied".into())
        } else {
            Ok(value)
        }
    }
}

impl ScalarFunctionHost for Host<'_> {
    fn current_unix_ms(&self) -> i64 {
        self.clock_reads.set(self.clock_reads.get() + 1);
        self.unix_ms
            .unwrap_or_else(reddb_engine::query::runtime::temporal::current_unix_ms)
    }
    fn resolve_argument(&self, argument: &Projection, source: &UnifiedRecord) -> Option<Value> {
        if let Projection::Column(column) = argument {
            self.resolved_columns.borrow_mut().push(column.clone());
        }
        eval_projection_value_with_host(self, argument, source)
    }
    fn evaluate_condition(&self, filter: &Filter, source: &UnifiedRecord) -> bool {
        evaluate_runtime_filter_result_with_host(
            &ConditionHost { host: self, source },
            source,
            filter,
            None,
            None,
        )
        .expect("fixture filter evaluation")
    }
    fn fallback_function(&self, _: &str, _: &[Projection], _: &UnifiedRecord) -> Option<Value> {
        Some(Value::text("host fallback"))
    }
}

struct ConditionHost<'a, 'b> {
    host: &'a Host<'b>,
    source: &'a UnifiedRecord,
}
impl FilterHost for ConditionHost<'_, '_> {
    type Error = EvalError;
    fn secret_query_input(&self, value: Value) -> Option<Value> {
        ProjectionValueHost::secret_query_input(self.host, value)
    }
    fn evaluate_expression(&self, expression: &Expr) -> Result<Option<Value>, EvalError> {
        evaluator::evaluate(expression, &|field: &FieldRef| {
            resolve_filter_row_value(
                self.source,
                field,
                None,
                None,
                &|value| ProjectionValueHost::secret_query_input(self.host, value),
                |_, _| false,
            )
        })
        .map(Some)
    }
}

// Test-only host: shared argument/scalar/filter execution, with explicit
// fallback sentinels for effects not implemented by this fixture.
impl ProjectionValueHost for Host<'_> {
    fn try_expression(&self, expression: &Expr, source: &UnifiedRecord) -> Option<Value> {
        if self.forced_error.is_some() {
            return None;
        }
        evaluator::evaluate(expression, &|field: &FieldRef| {
            resolve_filter_row_value(
                source,
                field,
                None,
                None,
                &super::record_filters::plaintext,
                |_, _| false,
            )
        })
        .ok()
    }
    fn fallback_function(
        &self,
        name: &str,
        arguments: &[Projection],
        source: &UnifiedRecord,
    ) -> Option<Value> {
        self.evaluate_function(name, arguments, source)
    }
    fn fallback_filter(&self, filter: &Filter, source: &UnifiedRecord) -> bool {
        self.evaluate_filter(filter, source)
    }
    fn secret_query_input(&self, value: Value) -> Option<Value> {
        self.input_calls.set(self.input_calls.get() + 1);
        if self.deny_input {
            None
        } else {
            Some(value)
        }
    }
}

fn scalar_values(name: &str, values: Vec<Value>) -> Option<Value> {
    let mut source = UnifiedRecord::new();
    let arguments = values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let column = format!("argument_{index}");
            source.set(&column, value);
            Projection::Column(column)
        })
        .collect::<Vec<_>>();
    evaluate_scalar_function_with_host(&Host::new(None), name, &arguments, &source)
}

pub fn runtime_clock_values() -> Vec<i64> {
    let source = UnifiedRecord::new();
    let host = Host::new(None);
    ["NOW", "CURRENT_TIMESTAMP", "CURRENT_DATE", "CURRENT_TIME"]
        .iter()
        .map(|name| {
            match evaluate_scalar_function_with_host(&host, name, &[], &source)
                .expect("clock scalar")
            {
                Value::TimestampMs(ms) => ms,
                Value::Date(days) => i64::from(days),
                Value::Time(ms) => i64::from(ms),
                other => panic!("unexpected clock result {other:?}"),
            }
        })
        .collect()
}

fn scalar_clock_values() {
    let source = UnifiedRecord::new();
    for (unix_ms, days, time) in [(0, 0, 0), (172_800_123, 2, 123), (-1, 0, 86_399_999)] {
        let host = Host {
            unix_ms: Some(unix_ms),
            ..Host::new(None)
        };
        for (name, expected) in [
            ("NOW:observed", Value::TimestampMs(unix_ms)),
            ("CURRENT_TIMESTAMP", Value::TimestampMs(unix_ms)),
            ("CURRENT_DATE", Value::Date(days)),
            ("CURRENT_TIME", Value::Time(time)),
        ] {
            assert_eq!(
                evaluate_scalar_function_with_host(
                    &host,
                    name,
                    &[Projection::Column("ignored".into()),],
                    &source
                ),
                Some(expected),
                "{name}"
            );
        }
        assert_eq!(
            host.clock_reads.get(),
            4,
            "each clock scalar reads the host once"
        );
        assert!(
            host.resolved_columns.borrow().is_empty(),
            "native clock calls ignore extra arguments"
        );
    }
    let before = reddb_engine::query::runtime::temporal::current_unix_ms();
    let clocks = runtime_clock_values();
    let after = reddb_engine::query::runtime::temporal::current_unix_ms();
    assert!(clocks[..2]
        .iter()
        .all(|value| *value >= before.min(after) && *value <= before.max(after)));
    assert!(
        clocks[2] >= before.min(after) / 86_400_000 && clocks[2] <= before.max(after) / 86_400_000
    );
}

fn scalar_time_bucket_inputs() {
    let mut source = UnifiedRecord::new();
    source.set("timestamp_ns", Value::Integer(17_000_000));
    source.set("timestamp_ms", Value::TimestampMs(99));
    let host = Host::new(None);
    let duration = Projection::Column("LIT:5ms".into());
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "TIME_BUCKET",
            std::slice::from_ref(&duration),
            &source
        ),
        Some(Value::UnsignedInteger(15_000_000))
    );
    for (timestamp, expected) in [
        (
            Projection::Column("missing".into()),
            Some(Value::UnsignedInteger(15_000_000)),
        ),
        (
            Projection::Column("LIT:12000000".into()),
            Some(Value::UnsignedInteger(10_000_000)),
        ),
        (Projection::Column("LIT:@RL:null".into()), None),
        (Projection::Column("LIT:-1".into()), None),
    ] {
        assert_eq!(
            evaluate_scalar_function_with_host(
                &host,
                "TIME_BUCKET",
                &[duration.clone(), timestamp],
                &source
            ),
            expected
        );
    }
    assert_eq!(
        evaluate_scalar_function_with_host(&host, "TIME_BUCKET", &[], &source),
        None
    );
    for invalid in [
        "LIT:100",
        "LIT:invalid",
        "LIT:18446744073709551615s",
        "timestamp_ns",
    ] {
        host.resolved_columns.borrow_mut().clear();
        assert_eq!(
            evaluate_scalar_function_with_host(
                &host,
                "TIME_BUCKET",
                &[
                    Projection::Column(invalid.into()),
                    Projection::Column("unreachable".into()),
                ],
                &source
            ),
            None
        );
        assert!(
            host.resolved_columns.borrow().is_empty(),
            "invalid duration skips timestamp resolution"
        );
    }
    source.set("timestamp_ns", Value::Null);
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "TIME_BUCKET",
            std::slice::from_ref(&duration),
            &source
        ),
        Some(Value::UnsignedInteger(95_000_000))
    );
    source.set("timestamp_ms", Value::Integer(17_000_000));
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "TIME_BUCKET",
            std::slice::from_ref(&duration),
            &source
        ),
        Some(Value::UnsignedInteger(15_000_000)),
        "field labels do not infer timestamp units"
    );
    source.set("timestamp_ms", Value::Null);
    source.set("timestamp", Value::Timestamp(2));
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "TIME_BUCKET",
            &[Projection::Column("LIT:raw".into()),],
            &source
        ),
        Some(Value::UnsignedInteger(2_000_000_000))
    );
    source.set("timestamp", Value::TimestampMs(i64::MAX));
    assert_eq!(
        evaluate_scalar_function_with_host(&host, "TIME_BUCKET", &[duration], &source),
        None,
        "overflow is unresolved rather than a wrapped bucket"
    );
    source.set("timestamp_ns", Value::Integer(17_000_000));
    let host = Host {
        deny_input: true,
        ..Host::new(None)
    };
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "TIME_BUCKET",
            &[
                Projection::Column("LIT:5ms".into()),
                Projection::Column("timestamp_ms".into()),
            ],
            &source
        ),
        Some(Value::UnsignedInteger(15_000_000)),
        "denied/missing explicit argument retains native raw-field fallback"
    );
}

fn scalar_substrings_and_patterns() {
    for (name, values, expected) in [
        (
            "SUBSTRING",
            vec![Value::text("aé🦀z"), Value::Integer(2), Value::Integer(2)],
            Some(Value::text("é🦀")),
        ),
        (
            "SUBSTR:label",
            vec![Value::text("aé🦀z"), Value::Integer(2)],
            Some(Value::text("é🦀z")),
        ),
        (
            "SUBSTRING",
            vec![
                Value::text("aé🦀z"),
                Value::Integer(2),
                Value::Integer(4_294_967_296),
            ],
            Some(Value::text("é🦀z")),
        ),
        (
            "SUBSTRING",
            vec![Value::text("aé"), Value::Integer(i64::MAX)],
            Some(Value::text("")),
        ),
        (
            "SUBSTRING",
            vec![Value::text("aé"), Value::Integer(2), Value::Null],
            Some(Value::text("é")),
        ),
        (
            "SUBSTRING",
            vec![Value::text("aé"), Value::Integer(2), Value::text("invalid")],
            Some(Value::text("é")),
        ),
        (
            "SUBSTRING",
            vec![Value::text("é42z"), Value::text("é([0-9]+)z")],
            Some(Value::text("42")),
        ),
        (
            "SUBSTRING",
            vec![Value::text("abc"), Value::text("[")],
            Some(Value::Null),
        ),
        (
            "SUBSTRING",
            vec![Value::text("b"), Value::text("(a)?b")],
            Some(Value::Null),
        ),
        (
            "SUBSTRING",
            vec![Value::text("abc"), Value::text("z")],
            Some(Value::Null),
        ),
        (
            "SUBSTRING",
            vec![Value::Null, Value::Integer(1)],
            Some(Value::Null),
        ),
        (
            "SUBSTRING",
            vec![Value::text("abc"), Value::Integer(1), Value::Integer(-1)],
            None,
        ),
        ("SUBSTR", vec![Value::text("abc"), Value::text("a")], None),
        (
            "SUBSTRING",
            vec![Value::text("abc"), Value::text("a"), Value::Integer(1)],
            None,
        ),
    ] {
        assert_eq!(scalar_values(name, values), expected, "{name}");
    }
    let host = Host::new(None);
    let source = source();
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "SUBSTRING",
            &[
                Projection::Column("nickname".into()),
                Projection::Column("LIT:2".into()),
                Projection::Column("missing".into()),
            ],
            &source
        ),
        Some(Value::text("ee")),
        "absent optional count retains unbounded-tail behavior"
    );
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "SUBSTRING",
            &[
                Projection::Column("missing".into()),
                Projection::Column("unreachable".into()),
            ],
            &source
        ),
        None
    );
}

fn scalar_trim_slice_and_quote() {
    for (name, values, expected) in [
        (
            "POSITION",
            vec![Value::text("🦀"), Value::text("aé🦀z")],
            Some(Value::Integer(3)),
        ),
        (
            "POSITION",
            vec![Value::text(""), Value::text("é")],
            Some(Value::Integer(1)),
        ),
        (
            "POSITION",
            vec![Value::Null, Value::text("é")],
            Some(Value::Null),
        ),
        (
            "TRIM",
            vec![Value::text("\u{2003}é ")],
            Some(Value::text("é")),
        ),
        (
            "BTRIM",
            vec![Value::text("xyéyx"), Value::text("xy")],
            Some(Value::text("é")),
        ),
        (
            "LTRIM",
            vec![Value::text("xyéyx"), Value::text("xy")],
            Some(Value::text("éyx")),
        ),
        (
            "RTRIM",
            vec![Value::text("xyéyx"), Value::text("xy")],
            Some(Value::text("xyé")),
        ),
        (
            "TRIM",
            vec![Value::text(" é "), Value::Null],
            Some(Value::Null),
        ),
        (
            "TRIM",
            vec![Value::text(" é "), Value::Integer(1)],
            Some(Value::Null),
        ),
        (
            "LEFT",
            vec![Value::text("aé🦀z"), Value::Integer(-1)],
            Some(Value::text("aé🦀")),
        ),
        (
            "RIGHT",
            vec![Value::text("aé🦀z"), Value::Integer(-1)],
            Some(Value::text("é🦀z")),
        ),
        (
            "LEFT",
            vec![Value::text("aé🦀z"), Value::Integer(-4_294_967_296)],
            Some(Value::text("")),
        ),
        (
            "RIGHT",
            vec![Value::text("aé🦀z"), Value::Integer(i64::MIN)],
            Some(Value::text("")),
        ),
        (
            "LEFT",
            vec![Value::Null, Value::Integer(1)],
            Some(Value::Null),
        ),
        ("RIGHT", vec![Value::text("aé"), Value::Null], None),
        (
            "QUOTE_LITERAL",
            vec![Value::text("a'b")],
            Some(Value::text("'a''b'")),
        ),
        (
            "QUOTE_LITERAL",
            vec![Value::Integer(42)],
            Some(Value::text("'42'")),
        ),
        ("QUOTE_LITERAL", vec![Value::Null], Some(Value::Null)),
    ] {
        assert_eq!(scalar_values(name, values), expected, "{name}");
    }
    let source = source();
    let host = Host::new(None);
    for name in [
        "SUBSTRING",
        "SUBSTR",
        "POSITION",
        "TRIM",
        "BTRIM",
        "LTRIM",
        "RTRIM",
        "LEFT",
        "RIGHT",
        "QUOTE_LITERAL",
    ] {
        assert_eq!(
            evaluate_scalar_function_with_host(&host, name, &[], &source),
            None,
            "{name}"
        );
    }
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "TRIM",
            &[
                Projection::Column("LIT: é ".into()),
                Projection::Column("missing".into()),
            ],
            &source
        ),
        Some(Value::text("é")),
        "missing optional trim characters retain whitespace trimming"
    );
    let host = Host {
        deny_input: true,
        ..Host::new(None)
    };
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "LEFT",
            &[
                Projection::Column("nickname".into()),
                Projection::Column("LIT:1".into()),
            ],
            &source
        ),
        None
    );
}

fn scalar_arithmetic_and_cast() {
    for (name, values, expected) in [
        (
            "ADD:sum",
            vec![Value::Integer(2), Value::Integer(3)],
            Value::Integer(5),
        ),
        (
            "SUB",
            vec![Value::Integer(2), Value::Integer(3)],
            Value::Integer(-1),
        ),
        (
            "MUL",
            vec![Value::Integer(2), Value::Integer(3)],
            Value::Integer(6),
        ),
        (
            "DIV",
            vec![Value::Integer(6), Value::Integer(2)],
            Value::Float(3.0),
        ),
        (
            "MOD",
            vec![Value::Integer(6), Value::Integer(0)],
            Value::Null,
        ),
        ("ABS", vec![Value::Integer(-3)], Value::Integer(3)),
        ("ABS", vec![Value::Float(-2.5)], Value::Float(2.5)),
        ("ABS", vec![Value::text("3")], Value::Null),
        ("ROUND", vec![Value::Float(2.5)], Value::Float(3.0)),
        ("ROUND", vec![Value::text("2.5")], Value::text("2.5")),
    ] {
        assert_eq!(scalar_values(name, values), Some(expected), "{name}");
    }
    let host = Host::new(None);
    let source = source();
    for (target, expected) in [
        ("TYPE:TEXT", Value::text("3")),
        ("TYPE:DOUBLE", Value::Float(3.0)),
        ("TYPE:unknown", Value::Null),
        ("TEXT", Value::Null),
    ] {
        assert_eq!(
            evaluate_scalar_function_with_host(
                &host,
                "CAST",
                &[
                    Projection::Column("quantity".into()),
                    Projection::Column(target.into()),
                ],
                &source
            ),
            Some(expected)
        );
    }
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "CAST",
            &[Projection::Column("quantity".into()), Projection::All,],
            &source
        ),
        Some(Value::Null)
    );
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "CAST",
            &[
                Projection::Column("nickname".into()),
                Projection::Column("TYPE:INT".into()),
            ],
            &source
        ),
        Some(Value::Null)
    );
}

fn scalar_case_and_coalesce() {
    let source = source();
    let host = Host::new(None);
    let condition = |op, value| {
        Projection::Expression(
            Box::new(Filter::Compare {
                field: field("quantity"),
                op,
                value: Value::Integer(value),
            }),
            None,
        )
    };
    let column = |name: &str| Projection::Column(name.into());
    let arguments = [
        condition(CompareOp::Gt, 4),
        column("unreachable"),
        condition(CompareOp::Eq, 3),
        column("nickname"),
        condition(CompareOp::Eq, 3),
        column("unreachable"),
        column("unreachable"),
    ];
    assert_eq!(
        evaluate_scalar_function_with_host(&host, "CASE:chosen", &arguments, &source),
        Some(Value::text("Bee"))
    );
    assert_eq!(*host.resolved_columns.borrow(), ["nickname"]);
    host.resolved_columns.borrow_mut().clear();
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "CASE",
            &[
                condition(CompareOp::Gt, 4),
                column("unreachable"),
                column("nickname"),
            ],
            &source
        ),
        Some(Value::text("Bee"))
    );
    assert_eq!(*host.resolved_columns.borrow(), ["nickname"]);
    host.resolved_columns.borrow_mut().clear();
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "CASE",
            &[condition(CompareOp::Gt, 4), column("unreachable"),],
            &source
        ),
        Some(Value::Null)
    );
    assert!(host.resolved_columns.borrow().is_empty());
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "CASE",
            &[condition(CompareOp::Eq, 3), column("missing"),],
            &source
        ),
        Some(Value::Null),
        "selected missing value becomes NULL"
    );
    host.resolved_columns.borrow_mut().clear();
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "COALESCE",
            &[
                column("missing"),
                column("LIT:@RL:null"),
                column("nickname"),
                column("unreachable"),
            ],
            &source
        ),
        Some(Value::text("Bee"))
    );
    assert_eq!(
        *host.resolved_columns.borrow(),
        ["missing", "LIT:@RL:null", "nickname"]
    );
    assert_eq!(
        scalar_values("COALESCE", vec![Value::Null, Value::Null]),
        Some(Value::Null)
    );
    assert_eq!(scalar_values("CASE", vec![]), Some(Value::Null));
    assert_eq!(scalar_values("COALESCE", vec![]), Some(Value::Null));
}

fn scalar_text_and_lengths() {
    for (name, values, expected) in [
        (
            "CONCAT",
            vec![Value::text("a"), Value::Null, Value::Integer(2)],
            Value::text("a2"),
        ),
        ("CONCAT", vec![], Value::text("")),
        (
            "CONCAT_WS",
            vec![
                Value::text(":"),
                Value::Null,
                Value::text("a"),
                Value::Integer(2),
            ],
            Value::text("'a':2"),
        ),
        (
            "CONCAT_WS",
            vec![Value::Null, Value::text("a")],
            Value::Null,
        ),
        (
            "CONCAT_WS",
            vec![Value::Integer(7), Value::text("a"), Value::text("b")],
            Value::text("'a'7'b'"),
        ),
        ("UPPER", vec![Value::text("éa")], Value::text("ÉA")),
        ("LOWER", vec![Value::text("ÉA")], Value::text("éa")),
        ("UPPER", vec![Value::Integer(7)], Value::Integer(7)),
        ("LOWER", vec![Value::Null], Value::Null),
        ("LENGTH", vec![Value::text("é🦀")], Value::Integer(2)),
        (
            "CHAR_LENGTH",
            vec![Value::Blob(vec![1, 2, 3])],
            Value::Integer(3),
        ),
        (
            "CHARACTER_LENGTH",
            vec![Value::Array(vec![Value::Null, Value::Null])],
            Value::Integer(2),
        ),
        ("LENGTH", vec![Value::Integer(7)], Value::Null),
        ("OCTET_LENGTH", vec![Value::text("é🦀")], Value::Integer(6)),
        ("BIT_LENGTH", vec![Value::text("é🦀")], Value::Integer(48)),
        (
            "OCTET_LENGTH",
            vec![Value::Blob(vec![1, 2])],
            Value::Integer(2),
        ),
        (
            "BIT_LENGTH",
            vec![Value::Blob(vec![1, 2])],
            Value::Integer(16),
        ),
        ("OCTET_LENGTH", vec![Value::Array(vec![])], Value::Null),
        ("BIT_LENGTH", vec![Value::Integer(1)], Value::Null),
        ("REVERSE", vec![Value::text("é🦀a")], Value::text("a🦀é")),
        ("REVERSE", vec![Value::Integer(1)], Value::Null),
    ] {
        assert_eq!(scalar_values(name, values), Some(expected), "{name}");
    }
}

fn scalar_money() {
    let money = Value::Money {
        asset_code: "BRL".into(),
        minor_units: 1099,
        scale: 2,
    };
    for values in [
        vec![Value::text("BRL 10.99")],
        vec![Value::AssetCode("BRL".into()), Value::text("10.99")],
        vec![Value::text("10.99"), Value::Currency(*b"BRL")],
    ] {
        assert_eq!(scalar_values("MONEY", values), Some(money.clone()));
    }
    assert_eq!(
        scalar_values("MONEY_ASSET", vec![money.clone()]),
        Some(Value::AssetCode("BRL".into()))
    );
    assert_eq!(
        scalar_values("MONEY_MINOR", vec![money.clone()]),
        Some(Value::BigInt(1099))
    );
    assert_eq!(
        scalar_values("MONEY_SCALE", vec![money]),
        Some(Value::Integer(2))
    );
    for name in ["MONEY_ASSET", "MONEY_MINOR", "MONEY_SCALE"] {
        assert_eq!(
            scalar_values(name, vec![Value::text("invalid")]),
            Some(Value::Null)
        );
    }
    assert_eq!(scalar_values("MONEY", vec![]), Some(Value::Null));
    assert_eq!(
        scalar_values("MONEY", vec![Value::text("invalid")]),
        Some(Value::Null)
    );
    assert_eq!(scalar_values("MONEY", vec![Value::Null]), None);
    assert_eq!(
        scalar_values(
            "MONEY",
            vec![Value::text("invalid"), Value::text("invalid")]
        ),
        None
    );
    let mut source = UnifiedRecord::new();
    source.set("left", Value::text("invalid"));
    source.set("right", Value::text("invalid"));
    let host = Host::new(None);
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "MONEY",
            &[
                Projection::Column("left".into()),
                Projection::Column("right".into()),
            ],
            &source
        ),
        None
    );
    assert_eq!(
        *host.resolved_columns.borrow(),
        ["left", "right", "right", "left"],
        "retain native reversed retry and admission order"
    );
}

fn scalar_missing_and_fallback() {
    let host = Host::new(None);
    let source = source();
    for name in [
        "ADD",
        "SUB",
        "MUL",
        "DIV",
        "MOD",
        "CAST",
        "UPPER",
        "LOWER",
        "LENGTH",
        "OCTET_LENGTH",
        "BIT_LENGTH",
        "CONCAT_WS",
        "REVERSE",
        "ABS",
        "ROUND",
        "MONEY_ASSET",
        "MONEY_MINOR",
        "MONEY_SCALE",
    ] {
        assert_eq!(
            evaluate_scalar_function_with_host(&host, name, &[], &source),
            None,
            "{name}"
        );
    }
    let missing = [
        Projection::Column("missing".into()),
        Projection::Column("unreachable".into()),
    ];
    for name in ["ADD", "CONCAT", "CONCAT_WS"] {
        host.resolved_columns.borrow_mut().clear();
        assert_eq!(
            evaluate_scalar_function_with_host(&host, name, &missing, &source),
            None
        );
        assert_eq!(
            *host.resolved_columns.borrow(),
            ["missing"],
            "do not resolve past an absent argument"
        );
    }
    for name in ["NATIVE_ONLY:label", "upper", ""] {
        host.resolved_columns.borrow_mut().clear();
        assert_eq!(
            evaluate_scalar_function_with_host(&host, name, &missing, &source),
            Some(Value::text("host fallback"))
        );
        assert!(
            host.resolved_columns.borrow().is_empty(),
            "fallback owns its own arguments"
        );
    }
    let host = Host {
        deny_input: true,
        ..Host::new(None)
    };
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "UPPER",
            &[Projection::Column("nickname".into()),],
            &source
        ),
        None,
        "shared dispatch must honor input denial"
    );
}

fn legacy_literals() {
    for (input, expected) in [
        ("LIT:", Value::Null),
        ("LIT:42", Value::Integer(42)),
        ("LIT:2.5", Value::Float(2.5)),
        ("LIT:hello", Value::text("hello")),
        ("LIT:null", Value::text("null")),
        ("LIT:@RL:null", Value::Null),
        ("LIT:@RL:[]", Value::Array(vec![])),
        ("LIT:@RL:V[]", Value::Vector(vec![])),
        ("LIT:@RL:V[1,2.5]", Value::Vector(vec![1.0, 2.5])),
        (
            "LIT:@RL:[true,false,null,[1],V[2,3],\"text\"]",
            Value::Array(vec![
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Null,
                Value::Array(vec![Value::Integer(1)]),
                Value::Vector(vec![2.0, 3.0]),
                Value::text("text"),
            ]),
        ),
        ("LIT:@RL:V[bad]", Value::text("@RL:V[bad]")),
        ("LIT:@RL:[", Value::text("@RL:[")),
        ("LIT:@RL:V[", Value::text("@RL:V[")),
        ("LIT:@RL:[[0]", Value::text("@RL:[[0]")),
        ("LIT:@RL:[1,]", Value::text("@RL:[1,]")),
        // Preserve the legacy splitter's quoted-comma behavior.
        ("LIT:@RL:[\"a,b\"]", Value::text("@RL:[\"a,b\"]")),
        (r#"LIT:@RL:"a\"b\\c""#, Value::text("a\"b\\c")),
    ] {
        assert_eq!(decode_projection_literal(input), Some(expected), "{input}");
    }
    assert_eq!(decode_projection_literal("quantity"), None);
    let depth = reddb_rql::limits::JSON_LITERAL_MAX_DEPTH as usize;
    let payload = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
    let value = decode_projection_literal(&format!("LIT:@RL:{payload}"));
    assert!(matches!(value, Some(Value::Array(_))), "depth boundary");
    let payload = format!("[{payload}]");
    assert_eq!(
        decode_projection_literal(&format!("LIT:@RL:{payload}")),
        Some(Value::text(format!("@RL:{payload}"))),
        "over-depth retains text fallback"
    );
}

fn value_resolution_and_fallback() {
    let mut source = source();
    source.set("position", Value::Integer(8));
    let host = Host::new(None);
    for projection in [
        Projection::Column("quantity".into()),
        Projection::Alias("quantity".into(), "amount".into()),
        Projection::Field(field("quantity"), None),
    ] {
        assert_eq!(
            eval_projection_value_with_host(&host, &projection, &source),
            Some(Value::Integer(3))
        );
    }
    let window =
        projections("SELECT ROW_NUMBER() OVER (ORDER BY quantity) AS position FROM orders");
    assert_eq!(
        eval_projection_value_with_host(&host, &window[0], &source),
        Some(Value::Integer(8))
    );
    assert_eq!(
        eval_projection_value_with_host(&host, &Projection::All, &source),
        None
    );
    assert_eq!(
        eval_projection_value_with_host(&host, &Projection::Column("missing".into()), &source),
        None
    );
    let columns =
        projections("SELECT UPPER(nickname) AS display, quantity > 1 AS accepted FROM orders");
    assert_eq!(
        eval_projection_value_with_host(&host, &columns[0], &source),
        Some(Value::text("BEE"))
    );
    assert_eq!(
        eval_projection_value_with_host(&host, &columns[1], &source),
        Some(Value::Boolean(true))
    );
    assert_eq!(
        host.fallback_calls.get(),
        0,
        "successful typed arguments skip fallback"
    );
    let columns = projections("SELECT (quantity + 1) * 2 AS nested FROM orders");
    assert_eq!(
        eval_projection_value_with_host(&host, &columns[0], &source),
        Some(Value::Integer(8))
    );
    assert_eq!(
        host.fallback_calls.get(),
        2,
        "nested ADD and MUL use canonical legacy arguments"
    );
    let host = Host {
        forced_error: Some(EvalError::DivisionByZero),
        ..Host::new(None)
    };
    let columns =
        projections("SELECT UPPER(nickname) AS display, quantity > 1 AS accepted FROM orders");
    assert_eq!(
        eval_projection_value_with_host(&host, &columns[0], &source),
        Some(Value::text("BEE"))
    );
    assert_eq!(
        eval_projection_value_with_host(&host, &columns[1], &source),
        Some(Value::Boolean(true))
    );
    assert_eq!(
        host.fallback_calls.get(),
        2,
        "optional arguments fall back even on typed data errors"
    );
}

fn secret_input_order() {
    let source = source();
    let host = Host::new(None);
    let plain = Projection::Column("quantity".into());
    let alias = Projection::Alias("quantity".into(), "amount".into());
    assert_eq!(
        eval_projection_value_with_host(&host, &alias, &source),
        Some(Value::Integer(3))
    );
    assert_eq!(
        host.input_calls.get(),
        2,
        "column and outer alias each admit input"
    );
    host.input_calls.set(0);
    let literal = Projection::Column("LIT:7".into());
    assert_eq!(
        eval_projection_value_with_host(&host, &literal, &source),
        Some(Value::Integer(7))
    );
    assert_eq!(
        host.input_calls.get(),
        0,
        "direct literal returns before admission"
    );
    let host = Host {
        deny_input: true,
        ..Host::new(None)
    };
    assert_eq!(
        eval_projection_value_with_host(&host, &plain, &source),
        None
    );
    assert_eq!(host.input_calls.get(), 1);
    host.input_calls.set(0);
    assert_eq!(
        eval_projection_value_with_host(&host, &alias, &source),
        None
    );
    assert_eq!(
        host.input_calls.get(),
        1,
        "denied inner value skips outer admission"
    );
    assert_eq!(
        eval_projection_value_with_host(&host, &literal, &source),
        Some(Value::Integer(7))
    );
    host.input_calls.set(0);
    let alias = Projection::Alias("LIT:7".into(), "literal".into());
    assert_eq!(
        eval_projection_value_with_host(&host, &alias, &source),
        None
    );
    assert_eq!(
        host.input_calls.get(),
        1,
        "aliased literal still requires admission"
    );
}

fn field(column: &str) -> FieldRef {
    FieldRef::TableColumn {
        table: "o".into(),
        column: column.into(),
    }
}

fn project(
    host: &Host<'_>,
    source: &UnifiedRecord,
    projections: &[Projection],
) -> Result<UnifiedRecord, String> {
    project_runtime_record_with_host(
        host,
        source,
        projections,
        Some("orders"),
        Some("o"),
        false,
        false,
    )
}

fn projections(sql: &str) -> Vec<Projection> {
    let query = super::record_filters::table_query(sql);
    reddb_rql::sql_lowering::effective_table_projections(&query)
}

fn expression(text: &str) -> Expr {
    reddb_rql::schema_expression::SchemaExpression::parse(text.into())
        .expect("bounded expression")
        .expression()
        .clone()
}

fn source() -> UnifiedRecord {
    let mut row = UnifiedRecord::new();
    row.set("rid", Value::UnsignedInteger(9_007_199_254_740_993));
    row.set("quantity", Value::Integer(3));
    row.set("nickname", Value::text("Bee"));
    row
}

fn fields_aliases_and_literals() {
    let source = source();
    let projections = vec![
        Projection::Alias("quantity".into(), "amount".into()),
        Projection::Field(field("rid"), Some("identity".into())),
        Projection::Column("entity_id".into()),
        Projection::Column("missing".into()),
        Projection::Alias("LIT:42".into(), "answer".into()),
    ];
    let row = project(&Host::new(None), &source, &projections).expect("projection");
    assert_eq!(row.get("amount"), Some(&Value::Integer(3)));
    assert_eq!(row.get("identity"), source.get("rid"));
    assert_eq!(row.get("entity_id"), source.get("rid"));
    assert_eq!(row.get("missing"), Some(&Value::Null));
    assert_eq!(row.get("answer"), Some(&Value::Integer(42)));
    assert_eq!(
        projected_columns(&[row], &projections),
        ["amount", "identity", "entity_id", "missing", "answer"]
    );
}

fn star_and_column_layout() {
    let mut row = UnifiedRecord::with_schema(
        Arc::new(vec![Arc::from("quantity"), Arc::from("_private")]),
        vec![Value::Integer(3), Value::text("internal")],
    );
    row.set("nickname", Value::text("Bee"));
    let projected = project(&Host::new(None), &row, &[Projection::All]).expect("star");
    assert_eq!(visible_value_keys(&projected), ["nickname", "quantity"]);
    assert!(projected.get("_private").is_none());
    assert_eq!(projected.get("quantity"), Some(&Value::Integer(3)));
    let mut sparse = UnifiedRecord::new();
    sparse.set("extra", Value::Integer(7));
    assert_eq!(
        collect_visible_columns(&[projected.clone(), sparse]),
        ["extra", "nickname", "quantity"]
    );
    assert_eq!(
        collect_visible_columns(&[projected.clone(), projected]),
        ["nickname", "quantity"]
    );
    let collision = [
        Projection::Alias("quantity".into(), "same".into()),
        Projection::Alias("nickname".into(), "same".into()),
    ];
    let projected = project(&Host::new(None), &row, &collision).expect("colliding labels");
    assert_eq!(
        projected.get("same"),
        Some(&Value::text("Bee")),
        "last projection retains the native overwrite rule"
    );
    assert_eq!(
        projected_columns(&[projected], &collision),
        ["same", "same"]
    );
}

fn document_paths() {
    let mut binary = vec![];
    reddb_types::document_body_codec::encode(&[("score", &Value::Integer(9))], &mut binary)
        .expect("binary document");
    let mut source = source();
    source.set("body", Value::Json(binary));
    let columns = [Projection::Alias("body.score".into(), "score".into())];
    let host = Host::new(None);
    assert_eq!(
        project(&host, &source, &columns)
            .expect("plain row")
            .get("score"),
        Some(&Value::Null)
    );
    for (document, entity) in [(true, false), (false, true)] {
        let row = project_runtime_record_with_host(
            &host,
            &source,
            &columns,
            Some("orders"),
            Some("o"),
            document,
            entity,
        )
        .expect("document path");
        assert_eq!(row.get("score"), Some(&Value::Integer(9)));
    }
    let columns = [Projection::Field(field("body.score"), Some("score".into()))];
    assert_eq!(
        project(&host, &source, &columns)
            .expect("explicit field")
            .get("score"),
        Some(&Value::Integer(9))
    );
}

fn bindings_and_window_output() {
    let mut source = source();
    source.nodes.insert(
        "n".into(),
        MatchedNode {
            id: "one".into(),
            label: "One".into(),
            node_label: "person".into(),
            properties: HashMap::new(),
        },
    );
    source.edges.insert(
        "e".into(),
        MatchedEdge::from_tuple("one", "knows", "two", 0.5),
    );
    source
        .vector_results
        .push(VectorSearchResult::new(7, "vectors", 0.25));
    source.set("position", Value::Integer(8));
    let columns =
        projections("SELECT ROW_NUMBER() OVER (ORDER BY quantity) AS position FROM orders");
    let row = project(&Host::new(None), &source, &columns).expect("materialized window");
    assert_eq!(row.get("position"), Some(&Value::Integer(8)));
    assert_eq!(row.nodes["n"].id, "one");
    assert_eq!(row.edges["e"].to, "two");
    assert_eq!(row.vector_results[0].id, 7);
}

fn typed_values_and_labels() {
    let columns = projections("SELECT o.rid AS identity, ABS(quantity) AS magnitude, UPPER(nickname) AS display, quantity > 1 AS accepted FROM orders AS o");
    let row = project(&Host::new(None), &source(), &columns).expect("typed projection");
    assert_eq!(
        row.get("identity"),
        Some(&Value::UnsignedInteger(9_007_199_254_740_993))
    );
    assert_eq!(row.get("magnitude"), Some(&Value::Integer(3)));
    assert_eq!(row.get("display"), Some(&Value::text("BEE")));
    assert_eq!(row.get("accepted"), Some(&Value::Boolean(true)));
    assert_eq!(
        projected_columns(&[row], &columns),
        ["identity", "magnitude", "display", "accepted"]
    );
}

fn fallback_and_data_errors() {
    let function =
        Projection::Function("UPPER".into(), vec![Projection::Column("nickname".into())]);
    let boolean = Projection::Expression(
        Box::new(Filter::Compare {
            field: field("quantity"),
            op: CompareOp::Gt,
            value: Value::Integer(1),
        }),
        Some("accepted".into()),
    );
    for error in [
        EvalError::UnknownColumn(field("missing")),
        EvalError::UnknownFunction {
            name: "host_fn".into(),
            args: vec![],
        },
    ] {
        let host = Host {
            forced_error: Some(error),
            ..Host::new(None)
        };
        let row =
            project(&host, &source(), &[function.clone(), boolean.clone()]).expect("host fallback");
        assert_eq!(row.get("UPPER(nickname)"), Some(&Value::text("BEE")));
        assert_eq!(row.get("accepted"), Some(&Value::Boolean(true)));
        assert_eq!(host.fallback_calls.get(), 2);
    }
    let host = Host {
        forced_error: Some(EvalError::DivisionByZero),
        ..Host::new(None)
    };
    assert_eq!(
        project(&host, &source(), &[function]).expect_err("data error"),
        EvalError::DivisionByZero.to_string()
    );
    assert_eq!(
        project(&host, &source(), &[boolean]).expect_err("data error"),
        EvalError::DivisionByZero.to_string()
    );
    assert_eq!(
        host.fallback_calls.get(),
        0,
        "data errors never invoke the legacy dispatcher"
    );
    let columns = projections("SELECT quantity / 0 > 1 AS invalid FROM orders");
    assert_eq!(
        project(&Host::new(None), &source(), &columns).expect_err("actual arithmetic error"),
        EvalError::DivisionByZero.to_string()
    );
}

fn secret_sensitivity() {
    let mut source = source();
    source.set("protected", Value::Secret(b"fixture ciphertext".to_vec()));
    let case = expression("CASE WHEN 1 = 1 THEN 7 ELSE protected END");
    assert!(expression_uses_secret(
        &case,
        &source,
        Some("orders"),
        Some("o")
    ));
    assert!(expression_uses_secret(
        &Expr::lit(Value::Secret(vec![1])),
        &source,
        None,
        None
    ));
    assert!(projection_uses_secret(
        &Projection::Function("__SECRET_REF".into(), vec![]),
        &source,
        None,
        None
    ));
    let filter = Filter::CompareFields {
        left: field("quantity"),
        op: CompareOp::Eq,
        right: field("protected"),
    };
    assert!(filter_uses_secret(
        &Filter::Not(Box::new(filter)),
        &source,
        Some("orders"),
        Some("o")
    ));
    assert!(!expression_uses_secret(
        &expression("quantity * 2"),
        &source,
        Some("orders"),
        Some("o")
    ));
    let projection = reddb_rql::sql_lowering::expr_to_projection(&case).expect("CASE lowering");
    let host = Host {
        deny_output: true,
        ..Host::new(None)
    };
    assert_eq!(
        project(&host, &source, &[projection])
            .expect_err("untaken secret branch remains sensitive"),
        "secret output denied"
    );
    assert_eq!(host.output_calls.get(), 1);
}

fn output_policy_and_redaction() {
    let mut source = source();
    source.set("protected", Value::Secret(b"fixture ciphertext".to_vec()));
    let host = Host {
        deny_output: true,
        ..Host::new(None)
    };
    let columns = [Projection::Field(field("protected"), Some("value".into()))];
    assert_eq!(
        project(&host, &source, &columns).expect_err("output policy rejection"),
        "secret output denied"
    );
    let host = Host {
        forced_error: Some(EvalError::JsonParseFailed {
            reason: "private payload".into(),
        }),
        ..Host::new(None)
    };
    let sensitive = Projection::Function(
        "UPPER".into(),
        vec![Projection::Field(field("protected"), None)],
    );
    assert_eq!(
        project(&host, &source, &[sensitive]).expect_err("redacted evaluation error"),
        "secret expression evaluation failed"
    );
    assert_eq!(host.fallback_calls.get(), 0);
    let host = Host::new(None);
    let row = project(&host, &source, &[Projection::All]).expect("star preserves wrappers");
    assert!(
        matches!(row.get("protected"), Some(Value::Secret(_))),
        "star must not decrypt or stringify a secret"
    );
    assert_eq!(
        host.output_calls.get(),
        0,
        "native star handling retains wrappers for downstream output policy"
    );
}

pub fn verify_recovered(records: &[UnifiedRecord], contract: &CollectionContract, updated: bool) {
    table_plan_projection::verify_recovered(records, contract, updated);
    super::aggregation::verify_recovered(records, contract, updated);
    let sql = "SELECT o.rid AS identity, total AS amount, ABS(quantity) AS magnitude, COALESCE(nickname, 'none') AS display, payload.score AS score FROM orders AS o WHERE total >= 10 ORDER BY total DESC LIMIT 2 OFFSET 1";
    let query = super::record_filters::table_query(sql);
    let columns = reddb_rql::sql_lowering::effective_table_projections(&query);
    let selected = super::record_ordering::ordered_records(records, &query, Some(contract), false);
    let host = Host::new(Some(contract));
    let projected = selected
        .iter()
        .map(|source| project(&host, source, &columns).expect("recovered projection"))
        .collect::<Vec<_>>();
    let expected = if updated {
        vec![(3, 20, 4, "none", 20), (2, 15, 3, "Bee", 15)]
    } else {
        vec![
            (2, 15, 3, "Bee", 15),
            (9_007_199_254_740_992, 10, 2, "Other", 10),
        ]
    };
    assert_eq!(projected.len(), expected.len());
    for (record, (id, amount, magnitude, display, score)) in projected.iter().zip(expected) {
        assert_eq!(record.get("identity"), Some(&Value::UnsignedInteger(id)));
        assert_eq!(record.get("amount"), Some(&Value::Integer(amount)));
        assert_eq!(record.get("magnitude"), Some(&Value::Integer(magnitude)));
        assert_eq!(record.get("display"), Some(&Value::text(display)));
        assert_eq!(record.get("score"), Some(&Value::Integer(score)));
    }
    assert_eq!(
        projected_columns(&projected, &columns),
        ["identity", "amount", "magnitude", "display", "score"]
    );
    assert_eq!(
        host.fallback_calls.get(),
        0,
        "the recovered query executes through canonical typed evaluation"
    );
    let query = super::record_filters::table_query("SELECT o.rid AS identity, quantity * 2 AS doubled, (quantity + 1) * 2 AS nested, total / quantity AS unit_price, quantity / 0 AS zero_divisor FROM orders AS o WHERE total >= 10 ORDER BY total DESC LIMIT 2 OFFSET 1");
    let columns = reddb_rql::sql_lowering::effective_table_projections(&query);
    let selected = super::record_ordering::ordered_records(records, &query, Some(contract), false);
    for (index, source) in selected.iter().enumerate() {
        let projected = project(&host, source, &columns).expect("recovered legacy arithmetic");
        assert_eq!(projected.get("identity"), source.get("rid"));
        let doubled = if updated { [8, 6] } else { [6, 4] };
        assert_eq!(
            projected.get("doubled"),
            Some(&Value::Integer(doubled[index]))
        );
        assert_eq!(
            projected.get("nested"),
            Some(&Value::Integer(doubled[index] + 2))
        );
        assert_eq!(projected.get("unit_price"), Some(&Value::Float(5.0)));
        assert_eq!(projected.get("zero_divisor"), Some(&Value::Null));
    }
    assert_eq!(
        host.fallback_calls.get(),
        10,
        "five legacy operations including nested arguments per recovered row"
    );
    let columns = projections("SELECT CASE WHEN quantity > 2 THEN quantity * 2 ELSE quantity + 100 END AS adjusted, CONCAT(LOWER(COALESCE(nickname, 'none')), ':', CAST(quantity AS TEXT)) AS caption FROM orders");
    for (index, source) in selected.iter().enumerate() {
        let projected = project(&host, source, &columns).expect("recovered shared scalar dispatch");
        let adjusted = if updated { [8, 6] } else { [6, 102] };
        let captions = if updated {
            ["none:4", "bee:3"]
        } else {
            ["bee:3", "other:2"]
        };
        assert_eq!(
            projected.get("adjusted"),
            Some(&Value::Integer(adjusted[index]))
        );
        assert_eq!(
            projected.get("caption"),
            Some(&Value::text(captions[index]))
        );
    }
    let columns = projections("SELECT SUBSTRING(COALESCE(nickname, 'none'), 1, 4294967296) AS wide_name, RIGHT(CONCAT(COALESCE(nickname, 'none'), ':', CAST(quantity AS TEXT)), 2) AS suffix, SUBSTRING(CONCAT(COALESCE(nickname, 'none'), '42'), '([0-9]+)') AS digits, QUOTE_LITERAL(COALESCE(nickname, 'none')) AS quoted FROM orders");
    for (index, source) in selected.iter().enumerate() {
        let projected = project(&host, source, &columns).expect("recovered shared text scalars");
        let names = if updated {
            ["none", "Bee"]
        } else {
            ["Bee", "Other"]
        };
        let suffixes = if updated { [":4", ":3"] } else { [":3", ":2"] };
        assert_eq!(projected.get("wide_name"), Some(&Value::text(names[index])));
        assert_eq!(projected.get("suffix"), Some(&Value::text(suffixes[index])));
        assert_eq!(projected.get("digits"), Some(&Value::text("42")));
        assert_eq!(
            projected.get("quoted"),
            Some(&Value::text(format!("'{}'", names[index])))
        );
    }
    let columns = projections("SELECT TIME_BUCKET('10ms', total * 1000000) AS bucket, TIME_BUCKET('raw', total) AS raw_total, NOW() AS observed, CURRENT_DATE AS today, CURRENT_TIME AS utc_time FROM orders");
    let host = Host {
        unix_ms: Some(172_800_123),
        ..Host::new(Some(contract))
    };
    for (index, source) in selected.iter().enumerate() {
        let projected = project(&host, source, &columns).expect("recovered temporal scalars");
        let buckets = if updated {
            [20_000_000, 10_000_000]
        } else {
            [10_000_000, 10_000_000]
        };
        let totals = if updated { [20, 15] } else { [15, 10] };
        assert_eq!(
            projected.get("bucket"),
            Some(&Value::UnsignedInteger(buckets[index]))
        );
        assert_eq!(
            projected.get("raw_total"),
            Some(&Value::UnsignedInteger(totals[index]))
        );
        assert_eq!(
            projected.get("observed"),
            Some(&Value::TimestampMs(172_800_123))
        );
        assert_eq!(projected.get("today"), Some(&Value::Date(2)));
        assert_eq!(projected.get("utc_time"), Some(&Value::Time(123)));
    }
    geographic_projection::verify_recovered(&host, &selected, updated);
}

impl reddb_engine::query::runtime::window_phase::WindowHost for Host<'_> {
    type Error = String;
    fn guard_materialization(&self, rows: usize) -> Result<(), String> {
        assert!(rows <= 4, "recovered fixture row bound");
        Ok(())
    }
    fn evaluate_expression(&self, expression: &Expr, source: &UnifiedRecord) -> Option<Value> {
        ProjectionHost::evaluate_expression(self, expression, source).ok()
    }
    fn secret_query_output(&self, value: Value) -> Result<Value, String> {
        ProjectionHost::secret_query_output(self, value)
    }
    fn query_error(&self, message: String) -> String {
        message
    }
}
