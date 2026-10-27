//! The native optional-value Expr evaluator, exercised in native and WASM.
use reddb_engine::query::runtime::expression::{
    evaluate_runtime_expr_with_host, RuntimeExpressionHost,
};
use reddb_rql::ast::{BinOp, Expr, ExprSubquery, FieldRef, QueryExpr, Span, UnaryOp, WindowSpec};
use reddb_types::{DataType, Value};
use std::cell::RefCell;

pub const CASES: &[(&str, fn())] = &[
    (
        "runtime_expression_native_values_and_casts",
        native_values_and_casts,
    ),
    (
        "runtime_expression_lazy_boolean_and_case",
        lazy_boolean_and_case,
    ),
    (
        "runtime_expression_eager_functions_and_raw_specials",
        eager_functions_and_raw_specials,
    ),
    (
        "runtime_expression_predicates_and_unsupported",
        predicates_and_unsupported,
    ),
    (
        "runtime_expression_secret_policy_and_depth",
        secret_policy_and_depth,
    ),
];

#[derive(Default)]
struct Host {
    events: RefCell<Vec<String>>,
    calls: RefCell<Vec<(String, Vec<Value>)>>,
    admit_secret: bool,
    special_hit: bool,
}
impl RuntimeExpressionHost for Host {
    fn secret_query_input(&self, value: Value) -> Option<Value> {
        self.events.borrow_mut().push("input".into());
        if matches!(value, Value::Secret(_)) {
            self.admit_secret.then_some(Value::Integer(7))
        } else {
            Some(value)
        }
    }
    fn resolve_field(&self, field: &FieldRef) -> Option<Value> {
        let FieldRef::TableColumn { column, .. } = field else {
            return None;
        };
        self.events.borrow_mut().push(format!("field:{column}"));
        match column.as_str() {
            "quantity" => Some(Value::Integer(3)),
            "secret" => Some(Value::Secret(vec![1, 2, 3])),
            _ => None,
        }
    }
    fn evaluate_special_function(
        &self,
        name: &str,
        arguments: &[Expr],
        evaluate: &dyn Fn(&Expr) -> Option<Value>,
    ) -> Option<Value> {
        self.events.borrow_mut().push(format!("special:{name}"));
        if self.special_hit {
            return Some(Value::Integer(42));
        }
        // Test-only special resolver. The production native host retains its
        // context/authorization/key semantics; this verifies callback traversal.
        match name {
            "CONFIG" => arguments.get(1).and_then(evaluate).or(Some(Value::Null)),
            "KV" => arguments.get(2).and_then(evaluate).or(Some(Value::Null)),
            "__SECRET_REF" | "__KV_REF" => Some(Value::Null),
            _ => panic!("only canonical special functions may reach this callback"),
        }
    }
    fn evaluate_function(&self, name: &str, arguments: &[Value]) -> Option<Value> {
        self.events.borrow_mut().push(format!("function:{name}"));
        self.calls
            .borrow_mut()
            .push((name.into(), arguments.to_vec()));
        match name {
            "COLLECT" => Some(Value::Array(arguments.to_vec())),
            "TRUE" => Some(Value::Boolean(true)),
            _ => None,
        }
    }
}
fn evaluate(host: &Host, expression: &Expr) -> Option<Value> {
    evaluate_runtime_expr_with_host(host, expression)
}
fn literal(value: Value) -> Expr {
    Expr::lit(value)
}
fn integer(value: i64) -> Expr {
    literal(Value::Integer(value))
}
fn field(column: &str) -> Expr {
    Expr::Column {
        field: FieldRef::TableColumn {
            table: "orders".into(),
            column: column.into(),
        },
        span: Span::synthetic(),
    }
}
fn binary(op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
    Expr::BinaryOp {
        op,
        lhs: Box::new(lhs),
        rhs: Box::new(rhs),
        span: Span::synthetic(),
    }
}
fn unary(op: UnaryOp, operand: Expr) -> Expr {
    Expr::UnaryOp {
        op,
        operand: Box::new(operand),
        span: Span::synthetic(),
    }
}
fn function(name: &str, args: Vec<Expr>) -> Expr {
    Expr::FunctionCall {
        name: name.into(),
        args,
        span: Span::synthetic(),
    }
}
fn cast(inner: Expr, target: DataType) -> Expr {
    Expr::Cast {
        inner: Box::new(inner),
        target,
        span: Span::synthetic(),
    }
}

fn native_values_and_casts() {
    let host = Host::default();
    for (expression, expected) in [
        (
            binary(BinOp::Add, literal(Value::text("2")), field("quantity")),
            Some(Value::Integer(5)),
        ),
        (
            binary(BinOp::Div, integer(3), integer(2)),
            Some(Value::Float(1.5)),
        ),
        (
            binary(BinOp::Mod, integer(7), integer(3)),
            Some(Value::Integer(1)),
        ),
        (binary(BinOp::Div, integer(3), integer(0)), None),
        (binary(BinOp::Add, literal(Value::Null), integer(1)), None),
        (
            binary(BinOp::Eq, literal(Value::Null), literal(Value::Null)),
            Some(Value::Boolean(true)),
        ),
        (
            binary(
                BinOp::Gt,
                literal(Value::UnsignedInteger(9_007_199_254_740_993)),
                literal(Value::UnsignedInteger(9_007_199_254_740_992)),
            ),
            Some(Value::Boolean(true)),
        ),
        (
            binary(
                BinOp::Concat,
                literal(Value::text("a")),
                literal(Value::text("b")),
            ),
            Some(Value::text("ab")),
        ),
        (
            binary(
                BinOp::Add,
                literal(Value::TimestampMs(10)),
                literal(Value::Duration(5)),
            ),
            Some(Value::TimestampMs(15)),
        ),
        (
            binary(BinOp::Sub, literal(Value::TimestampMs(10)), integer(5)),
            Some(Value::TimestampMs(5)),
        ),
        (
            binary(
                BinOp::Add,
                literal(Value::TimestampMs(i64::MAX)),
                integer(1),
            ),
            None,
        ),
        (
            cast(literal(Value::text("bad")), DataType::Integer),
            Some(Value::Null),
        ),
        (
            cast(literal(Value::Boolean(true)), DataType::Integer),
            Some(Value::Integer(1)),
        ),
        (unary(UnaryOp::Neg, integer(i64::MIN)), None),
        (unary(UnaryOp::Neg, literal(Value::BigInt(i64::MIN))), None),
        (unary(UnaryOp::Neg, integer(3)), Some(Value::Integer(-3))),
    ] {
        assert_eq!(evaluate(&host, &expression), expected, "{expression:?}");
    }
}

fn lazy_boolean_and_case() {
    let host = Host::default();
    for (op, value) in [(BinOp::And, false), (BinOp::Or, true)] {
        assert_eq!(
            evaluate(
                &host,
                &binary(
                    op,
                    literal(Value::Boolean(value)),
                    function("unvisited", vec![])
                )
            ),
            Some(Value::Boolean(value))
        );
    }
    let expression = Expr::Case {
        branches: vec![
            (
                literal(Value::Boolean(false)),
                function("unvisited", vec![]),
            ),
            (function("true", vec![]), integer(7)),
            (function("unvisited", vec![]), integer(8)),
        ],
        else_: Some(Box::new(function("unvisited", vec![]))),
        span: Span::synthetic(),
    };
    assert_eq!(evaluate(&host, &expression), Some(Value::Integer(7)));
    assert_eq!(host.calls.borrow().as_slice(), [("TRUE".into(), vec![])]);
    assert_eq!(
        evaluate(
            &host,
            &binary(
                BinOp::And,
                literal(Value::Null),
                literal(Value::Boolean(false))
            )
        ),
        None,
        "preserve native optional-value logic rather than typed three-valued logic"
    );
    let expression = Expr::Case {
        branches: vec![],
        else_: None,
        span: Span::synthetic(),
    };
    assert_eq!(evaluate(&host, &expression), Some(Value::Null));
}

fn eager_functions_and_raw_specials() {
    let host = Host::default();
    let expression = function(
        "collect",
        vec![
            field("missing"),
            function("true", vec![]),
            field("quantity"),
        ],
    );
    assert_eq!(
        evaluate(&host, &expression),
        Some(Value::Array(vec![
            Value::Null,
            Value::Boolean(true),
            Value::Integer(3)
        ]))
    );
    assert_eq!(
        *host.events.borrow(),
        [
            "field:missing",
            "function:TRUE",
            "field:quantity",
            "input",
            "function:COLLECT"
        ]
    );
    for (name, arguments) in [
        ("config", vec![function("unvisited", vec![]), integer(5)]),
        (
            "kv",
            vec![
                field("unvisited"),
                function("unvisited", vec![]),
                integer(5),
            ],
        ),
        ("__secret_ref", vec![function("unvisited", vec![])]),
        ("__kv_ref", vec![function("unvisited", vec![])]),
    ] {
        let host = Host {
            special_hit: true,
            ..Default::default()
        };
        assert_eq!(
            evaluate(&host, &function(name, arguments)),
            Some(Value::Integer(42))
        );
        assert_eq!(
            *host.events.borrow(),
            [format!("special:{}", name.to_uppercase())]
        );
        assert!(
            host.calls.borrow().is_empty(),
            "special raw arguments must not be pre-evaluated"
        );
    }
    let host = Host::default();
    assert_eq!(
        evaluate(
            &host,
            &function(
                "CONFIG",
                vec![
                    field("unvisited"),
                    binary(BinOp::Add, integer(2), integer(3))
                ]
            )
        ),
        Some(Value::Integer(5))
    );
    assert_eq!(*host.events.borrow(), ["special:CONFIG", "input", "input"]);
}

fn predicates_and_unsupported() {
    let host = Host::default();
    let expression = Expr::InList {
        target: Box::new(integer(3)),
        values: vec![field("missing"), integer(3), function("unvisited", vec![])],
        negated: false,
        span: Span::synthetic(),
    };
    assert_eq!(evaluate(&host, &expression), Some(Value::Boolean(true)));
    assert!(host.calls.borrow().is_empty());
    let expression = Expr::Between {
        target: Box::new(field("quantity")),
        low: Box::new(integer(1)),
        high: Box::new(integer(4)),
        negated: true,
        span: Span::synthetic(),
    };
    assert_eq!(evaluate(&host, &expression), Some(Value::Boolean(false)));
    for (operand, negated, expected) in [
        (field("missing"), false, true),
        (literal(Value::Null), true, false),
    ] {
        assert_eq!(
            evaluate(
                &host,
                &Expr::IsNull {
                    operand: Box::new(operand),
                    negated,
                    span: Span::synthetic()
                }
            ),
            Some(Value::Boolean(expected))
        );
    }
    let host = Host::default();
    let table = super::record_filters::table_query("SELECT * FROM orders");
    for expression in [
        Expr::Parameter {
            index: 1,
            span: Span::synthetic(),
        },
        Expr::Subquery {
            query: ExprSubquery {
                query: Box::new(QueryExpr::Table(table)),
            },
            span: Span::synthetic(),
        },
        Expr::WindowFunctionCall {
            name: "ROW_NUMBER".into(),
            args: vec![function("unvisited", vec![])],
            window: WindowSpec::default(),
            span: Span::synthetic(),
        },
    ] {
        assert_eq!(evaluate(&host, &expression), None);
    }
    assert!(host.events.borrow().is_empty());
}

fn secret_policy_and_depth() {
    let host = Host::default();
    assert_eq!(evaluate(&host, &field("secret")), None);
    let host = Host {
        admit_secret: true,
        ..Default::default()
    };
    assert_eq!(
        evaluate(&host, &binary(BinOp::Add, field("secret"), integer(1))),
        Some(Value::Integer(8))
    );
    let mut expression = literal(Value::Boolean(true));
    for _ in 0..reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        expression = unary(UnaryOp::Not, expression);
    }
    assert_eq!(
        evaluate(&Host::default(), &expression),
        Some(Value::Boolean(true))
    );
    let host = Host::default();
    let too_deep = unary(UnaryOp::Not, expression);
    assert_eq!(evaluate(&host, &too_deep), None);
    assert!(
        host.events.borrow().is_empty(),
        "depth rejection precedes host effects"
    );
    assert_eq!(
        evaluate(
            &host,
            &binary(BinOp::And, literal(Value::Boolean(false)), too_deep)
        ),
        Some(Value::Boolean(false)),
        "unvisited branches stay lazy"
    );
    // A special default cannot reset the recursion counter on each callback.
    let mut expression = function("unvisited", vec![]);
    for _ in 0..=reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        expression = function("CONFIG", vec![field("unvisited"), expression]);
    }
    let host = Host::default();
    assert_eq!(evaluate(&host, &expression), Some(Value::Null));
    assert!(host.calls.borrow().is_empty());
    assert_eq!(
        host.events.borrow().len(),
        reddb_rql::limits::JSON_LITERAL_MAX_DEPTH as usize + 1
    );
}
