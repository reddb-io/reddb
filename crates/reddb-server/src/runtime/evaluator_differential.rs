//! Differential semantic inventory for the scalar and filter evaluators.
//!
//! This is intentionally a report generator, not a semantic oracle. The test
//! fails only when evaluator behavior drifts from the committed v1 matrix.

use std::cmp::Ordering;
use std::collections::HashMap;

use super::expr_eval::evaluate_runtime_expr;
use super::join_filter::{
    evaluate_runtime_filter_result_with_db, resolve_runtime_field, runtime_partial_cmp,
    runtime_values_equal,
};
use crate::storage::query::engine::binding::Value as BindingValue;
use crate::storage::query::filter::Filter as StorageFilter;
use crate::storage::query::filter_compiled::CompiledFilter;
use crate::storage::query::unified::UnifiedRecord;
use reddb_rql::ast::{BinOp, CompareOp, Expr, FieldRef, Filter as RuntimeFilter, Span};
use reddb_types::Value;

const MATRIX_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/evaluator_divergence_matrix_v1.md"
);
const COMMITTED_MATRIX: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/evaluator_divergence_matrix_v1.md"
));

#[derive(Clone)]
enum Operand {
    Value(Value),
    Missing,
}

#[derive(Clone)]
enum CaseKind {
    Binary {
        op: BinOp,
        left: Operand,
        right: Operand,
    },
    Like {
        value: &'static str,
        pattern: &'static str,
    },
    IsNull(Operand),
}

struct Case {
    family: &'static str,
    name: &'static str,
    kind: CaseKind,
}

impl Case {
    fn binary(
        family: &'static str,
        name: &'static str,
        op: BinOp,
        left: Value,
        right: Value,
    ) -> Self {
        Self {
            family,
            name,
            kind: CaseKind::Binary {
                op,
                left: Operand::Value(left),
                right: Operand::Value(right),
            },
        }
    }
}

#[test]
fn evaluator_divergence_matrix_matches_v1_report() {
    let produced = render_matrix();
    if std::env::var_os("REDDB_UPDATE_EVALUATOR_MATRIX").is_some() {
        std::fs::write(MATRIX_PATH, produced).expect("write evaluator divergence matrix");
        return;
    }

    // Normalize line endings so a CRLF checkout (e.g. Windows autocrlf)
    // cannot fail the drift check spuriously.
    let committed = COMMITTED_MATRIX.replace("\r\n", "\n");
    assert_eq!(
        produced, committed,
        "evaluator divergence matrix drifted; inspect the semantic change, then regenerate with \
         REDDB_UPDATE_EVALUATOR_MATRIX=1 cargo test -p reddb-io-server --lib \
         evaluator_divergence_matrix_matches_v1_report"
    );
}

fn cases() -> Vec<Case> {
    vec![
        Case::binary(
            "NULL / 3VL",
            "NULL = NULL",
            BinOp::Eq,
            Value::Null,
            Value::Null,
        ),
        Case::binary(
            "NULL / 3VL",
            "NULL AND TRUE",
            BinOp::And,
            Value::Null,
            Value::Boolean(true),
        ),
        Case::binary(
            "NULL / 3VL",
            "NULL OR FALSE",
            BinOp::Or,
            Value::Null,
            Value::Boolean(false),
        ),
        Case {
            family: "NULL / 3VL",
            name: "missing column = NULL",
            kind: CaseKind::Binary {
                op: BinOp::Eq,
                left: Operand::Missing,
                right: Operand::Value(Value::Null),
            },
        },
        Case {
            family: "NULL / 3VL",
            name: "missing column IS NULL",
            kind: CaseKind::IsNull(Operand::Missing),
        },
        Case::binary(
            "float edge cases",
            "NaN = NaN",
            BinOp::Eq,
            Value::Float(f64::NAN),
            Value::Float(f64::NAN),
        ),
        Case::binary(
            "float edge cases",
            "NaN < +inf",
            BinOp::Lt,
            Value::Float(f64::NAN),
            Value::Float(f64::INFINITY),
        ),
        Case::binary(
            "float edge cases",
            "-inf < +inf",
            BinOp::Lt,
            Value::Float(f64::NEG_INFINITY),
            Value::Float(f64::INFINITY),
        ),
        Case::binary(
            "float edge cases",
            "-0.0 = 0.0",
            BinOp::Eq,
            Value::Float(-0.0),
            Value::Float(0.0),
        ),
        Case::binary(
            "integer boundaries",
            "i64::MAX = same u64",
            BinOp::Eq,
            Value::Integer(i64::MAX),
            Value::UnsignedInteger(i64::MAX as u64),
        ),
        Case::binary(
            "integer boundaries",
            "i64::MIN < -1",
            BinOp::Lt,
            Value::Integer(i64::MIN),
            Value::Integer(-1),
        ),
        Case::binary(
            "integer boundaries",
            "i64::MAX < u64::MAX",
            BinOp::Lt,
            Value::Integer(i64::MAX),
            Value::UnsignedInteger(u64::MAX),
        ),
        Case::binary(
            "arithmetic errors",
            "i64::MAX + 1",
            BinOp::Add,
            Value::Integer(i64::MAX),
            Value::Integer(1),
        ),
        Case::binary(
            "arithmetic errors",
            "1 / 0",
            BinOp::Div,
            Value::Integer(1),
            Value::Integer(0),
        ),
        Case::binary(
            "numeric coercion",
            "Integer(5) = Text(5)",
            BinOp::Eq,
            Value::Integer(5),
            Value::text("5"),
        ),
        Case::binary(
            "decimal",
            "Decimal(1.0000) = DecimalText(1)",
            BinOp::Eq,
            Value::Decimal(10_000),
            Value::DecimalText("1".to_string()),
        ),
        Case::binary(
            "decimal",
            "DecimalText(2) < DecimalText(10)",
            BinOp::Lt,
            Value::DecimalText("2".to_string()),
            Value::DecimalText("10".to_string()),
        ),
        Case::binary(
            "unicode text",
            "combining e-acute = precomposed",
            BinOp::Eq,
            Value::text("e\u{301}"),
            Value::text("é"),
        ),
        Case {
            family: "LIKE case / byte",
            name: "AbC LIKE a%",
            kind: CaseKind::Like {
                value: "AbC",
                pattern: "a%",
            },
        },
        Case {
            family: "LIKE case / byte",
            name: "café LIKE caf_",
            kind: CaseKind::Like {
                value: "café",
                pattern: "caf_",
            },
        },
        Case {
            family: "LIKE case / byte",
            name: "combining e-acute LIKE _",
            kind: CaseKind::Like {
                value: "e\u{301}",
                pattern: "_",
            },
        },
        Case {
            family: "LIKE case / byte",
            name: "Turkish dotted İ LIKE i",
            kind: CaseKind::Like {
                value: "İ",
                pattern: "i",
            },
        },
        Case {
            family: "LIKE case / byte",
            name: "Turkish dotless ı LIKE I",
            kind: CaseKind::Like {
                value: "ı",
                pattern: "I",
            },
        },
        Case::binary(
            "temporal",
            "Timestamp(1) < Timestamp(2)",
            BinOp::Lt,
            Value::Timestamp(1),
            Value::Timestamp(2),
        ),
        Case::binary(
            "temporal",
            "Date(1) < Date(2)",
            BinOp::Lt,
            Value::Date(1),
            Value::Date(2),
        ),
        Case::binary(
            "temporal",
            "Time(1) < Time(2)",
            BinOp::Lt,
            Value::Time(1),
            Value::Time(2),
        ),
        Case::binary(
            "boolean",
            "FALSE < TRUE",
            BinOp::Lt,
            Value::Boolean(false),
            Value::Boolean(true),
        ),
    ]
}

type Adapter = (&'static str, fn(&Case) -> String);

fn adapters() -> [Adapter; 8] {
    [
        ("typed-expr", eval_typed_expr),
        ("runtime-expr", eval_runtime_expr),
        ("runtime-filter", eval_runtime_filter_adapter),
        ("legacy-filter", eval_legacy_filter),
        ("compiled-filter", eval_compiled_filter),
        ("types-compare", eval_types_compare),
        ("runtime-compare", eval_runtime_compare),
        ("executor-compare", eval_executor_compare),
    ]
}

fn render_matrix() -> String {
    let adapters = adapters();
    let mut out = String::from(
        "# Evaluator divergence matrix v1\n\n\
Generated by `runtime::evaluator_differential`. This is a versioned report, not a semantic \
oracle: currently-known differences are expected. CI fails only when a produced cell drifts from \
this committed matrix.\n\n\
The table covers the known LIKE case/byte, NULL equality and 3VL, overflow, division-by-zero, \
numeric coercion, decimal, Unicode, temporal, missing-column, and Boolean families. `unsupported` \
means that the implementation has no representation for that operation or value family. Caveat: \
for the three filter engines the boolean-logic rows (`NULL AND TRUE`, `NULL OR FALSE`) are \
encoded as `(left = TRUE) AND/OR (right = TRUE)` with a NULL operand — those cells measure a \
comparison-to-NULL feeding a combinator, not the engines' own three-valued logic on a NULL \
operand.\n\n\
## Implementations\n\n\
- `typed-expr`: `storage/query/evaluator.rs`\n\
- `runtime-expr`: `runtime/expr_eval.rs`\n\
- `runtime-filter`: `runtime/join_filter/filter.rs`\n\
- `legacy-filter`: `storage/query/filter.rs`\n\
- `compiled-filter`: `storage/query/filter_compiled.rs`\n\
- `types-compare`: `storage/query/value_compare.rs` (`reddb-types` authority)\n\
- `runtime-compare`: `runtime/join_filter/value_compare.rs`\n\
- `executor-compare`: `storage/query/executors/value_compare.rs`\n\n\
## Matrix\n\n\
| family / case | typed-expr | runtime-expr | runtime-filter | legacy-filter | compiled-filter | types-compare | runtime-compare | executor-compare |\n\
| --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
    );

    for case in cases() {
        out.push_str("| ");
        out.push_str(case.family);
        out.push_str(" / ");
        out.push_str(case.name);
        for (_, evaluate) in adapters {
            out.push_str(" | `");
            out.push_str(&evaluate(&case));
            out.push('`');
        }
        out.push_str(" |\n");
    }

    out.push_str(
        "\n## Regeneration\n\n\
After reviewing an intentional semantic change, regenerate with:\n\n\
```sh\n\
REDDB_UPDATE_EVALUATOR_MATRIX=1 cargo test -p reddb-io-server --lib evaluator_divergence_matrix_matches_v1_report\n\
```\n",
    );
    out
}

fn eval_typed_expr(case: &Case) -> String {
    let Some(expr) = expression_for(case) else {
        return "unsupported".to_string();
    };
    let record = UnifiedRecord::new();
    let row = |field: &FieldRef| resolve_runtime_field(&record, field, None, None);
    match crate::storage::query::evaluator::evaluate(&expr, &row) {
        Ok(value) => render_value(&value),
        Err(crate::storage::query::evaluator::EvalError::ArithmeticOverflow { .. }) => {
            "error:overflow".to_string()
        }
        Err(crate::storage::query::evaluator::EvalError::DivisionByZero) => {
            "error:division-by-zero".to_string()
        }
        Err(crate::storage::query::evaluator::EvalError::UnknownColumn(_)) => {
            "error:missing-column".to_string()
        }
        Err(error) => format!("error:{error}"),
    }
}

fn eval_runtime_expr(case: &Case) -> String {
    let Some(expr) = expression_for(case) else {
        return "unsupported".to_string();
    };
    let record = UnifiedRecord::new();
    render_optional_value(evaluate_runtime_expr(&expr, &record, None, None))
}

fn eval_runtime_filter_adapter(case: &Case) -> String {
    let Some((filter, record)) = runtime_filter_fixture(case) else {
        return "unsupported".to_string();
    };
    match evaluate_runtime_filter_result_with_db(None, &record, &filter, None, None) {
        Ok(value) => value.to_string(),
        Err(crate::RedDBError::Query(message)) if message.contains("arithmetic overflow") => {
            "error:overflow".to_string()
        }
        Err(crate::RedDBError::Query(message)) if message.contains("division by zero") => {
            "error:division-by-zero".to_string()
        }
        Err(error) => format!("error:{error}"),
    }
}

fn eval_legacy_filter(case: &Case) -> String {
    let Some(fixture) = storage_filter_fixture(case) else {
        return "unsupported".to_string();
    };
    fixture
        .filter
        .evaluate(&|column| fixture.value(column))
        .to_string()
}

fn eval_compiled_filter(case: &Case) -> String {
    let Some(fixture) = storage_filter_fixture(case) else {
        return "unsupported".to_string();
    };
    // A missing column stays out of the schema so the matrix records the
    // compiler's real missing-column behavior (an UnknownColumn compile
    // error), not NULL-slot semantics.
    let present = fixture
        .columns
        .iter()
        .filter_map(|(name, value)| value.clone().map(|value| (*name, value)))
        .collect::<Vec<_>>();
    let schema = present
        .iter()
        .enumerate()
        .map(|(index, (name, _))| ((*name).to_string(), index))
        .collect::<HashMap<_, _>>();
    let slot = present
        .iter()
        .map(|(_, value)| value.clone())
        .collect::<Vec<_>>();
    match CompiledFilter::compile(&fixture.filter, &schema) {
        Ok(filter) => filter.evaluate(&slot).to_string(),
        Err(error) => format!("compile-error:{error}"),
    }
}

fn eval_types_compare(case: &Case) -> String {
    let Some((op, left, right)) = comparable_operands(case) else {
        return "unsupported".to_string();
    };
    comparison_result(
        op,
        crate::storage::query::value_compare::partial_compare_values(left, right),
    )
}

fn eval_runtime_compare(case: &Case) -> String {
    let Some((op, left, right)) = comparable_operands(case) else {
        return "unsupported".to_string();
    };
    match op {
        CompareOp::Eq => runtime_values_equal(left, right).to_string(),
        CompareOp::Ne => (!runtime_values_equal(left, right)).to_string(),
        _ => comparison_result(op, runtime_partial_cmp(left, right)),
    }
}

fn eval_executor_compare(case: &Case) -> String {
    let Some((op, left, right)) = comparable_operands(case) else {
        return "unsupported".to_string();
    };
    let (Some(left), Some(right)) = (binding_value(left), binding_value(right)) else {
        return "unsupported".to_string();
    };
    match op {
        CompareOp::Eq => {
            crate::storage::query::executors::value_compare::values_equal(&left, &right).to_string()
        }
        CompareOp::Ne => {
            (!crate::storage::query::executors::value_compare::values_equal(&left, &right))
                .to_string()
        }
        _ => comparison_result(
            op,
            crate::storage::query::executors::value_compare::partial_compare_values(&left, &right),
        ),
    }
}

fn expression_for(case: &Case) -> Option<Expr> {
    match &case.kind {
        CaseKind::Binary { op, left, right } => Some(Expr::binop(
            *op,
            operand_expr(left, "missing"),
            operand_expr(right, "missing_rhs"),
        )),
        CaseKind::IsNull(operand) => Some(Expr::IsNull {
            operand: Box::new(operand_expr(operand, "missing")),
            negated: false,
            span: Span::synthetic(),
        }),
        CaseKind::Like { .. } => None,
    }
}

fn operand_expr(operand: &Operand, missing_name: &str) -> Expr {
    match operand {
        Operand::Value(value) => Expr::lit(value.clone()),
        Operand::Missing => Expr::col(FieldRef::column("", missing_name)),
    }
}

fn runtime_filter_fixture(case: &Case) -> Option<(RuntimeFilter, UnifiedRecord)> {
    let mut record = UnifiedRecord::new();
    let filter = match &case.kind {
        CaseKind::Binary { op, left, right } if compare_op(*op).is_some() => {
            set_record_operand(&mut record, "value", left);
            RuntimeFilter::Compare {
                field: FieldRef::column("", "value"),
                op: compare_op(*op)?,
                value: operand_value(right)?.clone(),
            }
        }
        CaseKind::Binary { op, left, right } if matches!(op, BinOp::And | BinOp::Or) => {
            set_record_operand(&mut record, "left", left);
            set_record_operand(&mut record, "right", right);
            let left = RuntimeFilter::Compare {
                field: FieldRef::column("", "left"),
                op: CompareOp::Eq,
                value: Value::Boolean(true),
            };
            let right = RuntimeFilter::Compare {
                field: FieldRef::column("", "right"),
                op: CompareOp::Eq,
                value: Value::Boolean(true),
            };
            if *op == BinOp::And {
                RuntimeFilter::And(Box::new(left), Box::new(right))
            } else {
                RuntimeFilter::Or(Box::new(left), Box::new(right))
            }
        }
        CaseKind::Binary { op, .. }
            if matches!(
                op,
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
            ) =>
        {
            RuntimeFilter::CompareExpr {
                lhs: expression_for(case)?,
                op: CompareOp::Eq,
                rhs: Expr::lit(Value::Boolean(true)),
            }
        }
        CaseKind::Like { value, pattern } => {
            record.set("value", Value::text(*value));
            RuntimeFilter::Like {
                field: FieldRef::column("", "value"),
                pattern: (*pattern).to_string(),
            }
        }
        CaseKind::IsNull(operand) => {
            set_record_operand(&mut record, "value", operand);
            RuntimeFilter::IsNull(FieldRef::column("", "value"))
        }
        _ => return None,
    };
    Some((filter, record))
}

struct StorageFilterFixture {
    filter: StorageFilter,
    columns: Vec<(&'static str, Option<Value>)>,
}

impl StorageFilterFixture {
    fn value(&self, column: &str) -> Option<Value> {
        self.columns
            .iter()
            .find(|(name, _)| *name == column)
            .and_then(|(_, value)| value.clone())
    }
}

fn storage_filter_fixture(case: &Case) -> Option<StorageFilterFixture> {
    match &case.kind {
        CaseKind::Binary { op, left, right } if compare_op(*op).is_some() => {
            let right = operand_value(right)?.clone();
            let filter = match op {
                BinOp::Eq => StorageFilter::eq("value", right),
                BinOp::Ne => StorageFilter::ne("value", right),
                BinOp::Lt => StorageFilter::lt("value", right),
                BinOp::Le => StorageFilter::le("value", right),
                BinOp::Gt => StorageFilter::gt("value", right),
                BinOp::Ge => StorageFilter::ge("value", right),
                _ => return None,
            };
            Some(StorageFilterFixture {
                filter,
                columns: vec![("value", operand_value(left).cloned())],
            })
        }
        CaseKind::Binary { op, left, right } if matches!(op, BinOp::And | BinOp::Or) => {
            let left_filter = StorageFilter::eq("left", Value::Boolean(true));
            let right_filter = StorageFilter::eq("right", Value::Boolean(true));
            let filter = if *op == BinOp::And {
                StorageFilter::and(vec![left_filter, right_filter])
            } else {
                StorageFilter::or(vec![left_filter, right_filter])
            };
            Some(StorageFilterFixture {
                filter,
                columns: vec![
                    ("left", operand_value(left).cloned()),
                    ("right", operand_value(right).cloned()),
                ],
            })
        }
        CaseKind::Like { value, pattern } => Some(StorageFilterFixture {
            filter: StorageFilter::like("value", *pattern),
            columns: vec![("value", Some(Value::text(*value)))],
        }),
        CaseKind::IsNull(operand) => Some(StorageFilterFixture {
            filter: StorageFilter::is_null("value"),
            columns: vec![("value", operand_value(operand).cloned())],
        }),
        _ => None,
    }
}

fn set_record_operand(record: &mut UnifiedRecord, column: &str, operand: &Operand) {
    if let Operand::Value(value) = operand {
        record.set(column, value.clone());
    }
}

fn operand_value(operand: &Operand) -> Option<&Value> {
    match operand {
        Operand::Value(value) => Some(value),
        Operand::Missing => None,
    }
}

fn comparable_operands(case: &Case) -> Option<(CompareOp, &Value, &Value)> {
    let CaseKind::Binary { op, left, right } = &case.kind else {
        return None;
    };
    Some((
        compare_op(*op)?,
        operand_value(left)?,
        operand_value(right)?,
    ))
}

fn compare_op(op: BinOp) -> Option<CompareOp> {
    match op {
        BinOp::Eq => Some(CompareOp::Eq),
        BinOp::Ne => Some(CompareOp::Ne),
        BinOp::Lt => Some(CompareOp::Lt),
        BinOp::Le => Some(CompareOp::Le),
        BinOp::Gt => Some(CompareOp::Gt),
        BinOp::Ge => Some(CompareOp::Ge),
        _ => None,
    }
}

fn comparison_result(op: CompareOp, ordering: Option<Ordering>) -> String {
    let result = match op {
        CompareOp::Eq => ordering == Some(Ordering::Equal),
        CompareOp::Ne => ordering != Some(Ordering::Equal),
        CompareOp::Lt => ordering == Some(Ordering::Less),
        CompareOp::Le => matches!(ordering, Some(Ordering::Less | Ordering::Equal)),
        CompareOp::Gt => ordering == Some(Ordering::Greater),
        CompareOp::Ge => matches!(ordering, Some(Ordering::Greater | Ordering::Equal)),
    };
    result.to_string()
}

fn binding_value(value: &Value) -> Option<BindingValue> {
    match value {
        Value::Null => Some(BindingValue::Null),
        Value::Boolean(value) => Some(BindingValue::Boolean(*value)),
        Value::Integer(value) => Some(BindingValue::Integer(*value)),
        Value::Float(value) => Some(BindingValue::Float(*value)),
        Value::Text(value) => Some(BindingValue::String(value.to_string())),
        _ => None,
    }
}

fn render_optional_value(value: Option<Value>) -> String {
    value
        .as_ref()
        .map(render_value)
        .unwrap_or_else(|| "none".to_string())
}

fn render_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Boolean(value) => value.to_string(),
        Value::Integer(value) => format!("i64:{value}"),
        Value::UnsignedInteger(value) => format!("u64:{value}"),
        Value::Float(value) if value.is_nan() => "f64:NaN".to_string(),
        Value::Float(value) if *value == f64::INFINITY => "f64:+inf".to_string(),
        Value::Float(value) if *value == f64::NEG_INFINITY => "f64:-inf".to_string(),
        Value::Float(value) if value.is_sign_negative() && *value == 0.0 => "f64:-0.0".to_string(),
        Value::Float(value) => format!("f64:{value}"),
        Value::Text(value) => format!("text:{value:?}"),
        Value::Timestamp(value) => format!("timestamp:{value}"),
        Value::Date(value) => format!("date:{value}"),
        Value::Time(value) => format!("time:{value}"),
        Value::Decimal(value) => format!("decimal:{value}"),
        Value::DecimalText(value) => format!("decimal-text:{value}"),
        value => format!("{value:?}"),
    }
}
