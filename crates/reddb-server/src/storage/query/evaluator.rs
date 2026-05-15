//! Scalar expression evaluator — single owner of value-level
//! evaluation for SQL scalar `Expr` trees.
//!
//! Today scalar expression evaluation is fragmented:
//!
//! - `query::filter::Predicate::evaluate` runs comparison predicates
//!   against a single column value.
//! - `query::filter_compiled::CompiledFilter::evaluate` runs the
//!   compiled filter opcode tape over a row slot, but still defers to
//!   `Predicate::evaluate` per opcode for the actual comparison logic.
//! - Several inlined arms in `query::core` and `query::executor`
//!   compute projections, DEFAULT / CHECK expressions, RETURNING
//!   columns, COMPUTED columns, and ON CONFLICT updates by walking
//!   the AST and dispatching arithmetic / function calls inline.
//! - `query::expr_typing` knows how to type an `Expr` against a
//!   `Scope` but never produces a `Value` — it stops at `TypedExpr`.
//!
//! Operator / function / cast resolution is centralised in
//! `schema::coercion_spine`, but **value-level dispatch** is not — so
//! adding a new operator overload, a new implicit cast edge, or a
//! new built-in function requires synchronised edits across each of
//! the inline evaluators above.
//!
//! This module is the first vertical slice of the deep evaluator
//! interface tracked by `draft-53-scalar-expression-evaluator`. It
//! exposes:
//!
//! - [`Row`]: pluggable column-binding lookup so callers can wire
//!   either a planner-resolved slot vector or a flat column-name map.
//! - [`evaluate`]: the single entry point. Walks an `Expr`, resolves
//!   operators / functions / casts through `coercion_spine`, applies
//!   the implicit casts the spine asks for, and produces a `Value`.
//! - [`EvalError`]: typed diagnostic surface — every failure mode the
//!   evaluator can encounter at runtime is enumerated here so callers
//!   don't have to parse strings.
//!
//! The slice is intentionally additive: callers in `filter.rs`,
//! `filter_compiled.rs`, and `executor.rs` still own their own
//! evaluation paths. Subsequent slices migrate those callers onto
//! `evaluate` once the dispatch surface has proven out under the
//! focused test set in this module.
//!
//! ## Semantics summary
//!
//! - **NULL propagation.** Arithmetic, comparison, concat, and CAST
//!   propagate `Value::Null` — any `Null` operand short-circuits to
//!   `Null`. `AND` / `OR` follow SQL three-valued logic.
//! - **Arithmetic overflow.** Signed checked arithmetic; overflow
//!   surfaces as [`EvalError::ArithmeticOverflow`] rather than wrap
//!   or panic.
//! - **Division.** Division by zero surfaces as
//!   [`EvalError::DivisionByZero`]. Integer `/` always promotes to
//!   `Float` per the operator catalog; integer `%` stays integer.
//! - **Implicit cast triggers.** When the operator catalog has no
//!   exact-type overload, the spine returns the per-operand
//!   coercions; this evaluator applies them via
//!   [`coerce::coerce_via_catalog`] before dispatch.
//! - **Unknown function rejection.** Calls that don't resolve in the
//!   built-in function catalog produce
//!   [`EvalError::UnknownFunction`]. Variadic / catalog-resolved
//!   functions still return through the same dispatch.

use std::sync::Arc;

use super::ast::{BinOp, Expr, FieldRef, UnaryOp};
use crate::storage::schema::coerce::coerce_via_catalog;
use crate::storage::schema::coercion_spine;
use crate::storage::schema::function_catalog::FUNCTION_CATALOG;
use crate::storage::schema::operator_catalog::OperatorEntry;
use crate::storage::schema::{DataType, Value};

/// Pluggable row-binding lookup. The evaluator stays agnostic of
/// whether the caller has a slot vector indexed by planner-assigned
/// position, a flat `HashMap<String, Value>`, or a graph binding —
/// every consumer just provides a `Row` impl that resolves a
/// `FieldRef` to a `Value`.
pub trait Row {
    /// Resolve a column / property reference. Returns `None` when the
    /// reference is unknown (the evaluator surfaces this as
    /// [`EvalError::UnknownColumn`]) or `Some(Value::Null)` when the
    /// column exists but the row's value is null.
    fn get(&self, field: &FieldRef) -> Option<Value>;
}

/// Trivial `Row` impl over a flat `(table, column) -> Value` closure
/// — useful for tests and for callers that only have a column-name
/// map.
impl<F> Row for F
where
    F: Fn(&FieldRef) -> Option<Value>,
{
    fn get(&self, field: &FieldRef) -> Option<Value> {
        self(field)
    }
}

/// Errors surfaced by [`evaluate`]. Every variant is a runtime
/// failure shape — type-resolution failures live in
/// `expr_typing::TypeError`, not here.
#[derive(Debug, Clone, PartialEq)]
pub enum EvalError {
    /// Column / property not present in the row binding.
    UnknownColumn(FieldRef),
    /// Query parameter placeholders are not resolved at this layer
    /// — the bind phase substitutes a concrete value before
    /// evaluation.
    UnboundParameter(usize),
    /// Operator catalog has no overload that accepts these operand
    /// types, even after considering implicit coercions.
    OperatorMismatch {
        op: BinOp,
        lhs: DataType,
        rhs: DataType,
    },
    /// Unary operator doesn't accept the operand type.
    UnaryMismatch { op: UnaryOp, operand: DataType },
    /// Function catalog has no overload matching this call's
    /// argument types. Includes user-defined functions because
    /// today's catalog is the static built-in table only.
    UnknownFunction { name: String, args: Vec<DataType> },
    /// Implicit cast required by the spine failed at runtime — the
    /// catalog said the conversion was legal at `Implicit` context
    /// but the value's bytes couldn't be converted (e.g. overflow on
    /// `BigInt` → `Integer`).
    ImplicitCastFailed {
        from: DataType,
        to: DataType,
        reason: String,
    },
    /// Explicit `CAST(x AS T)` failed at runtime.
    CastFailed {
        from: DataType,
        to: DataType,
        reason: String,
    },
    /// Signed-integer overflow during arithmetic.
    ArithmeticOverflow { op: BinOp },
    /// `n / 0` or `n % 0`.
    DivisionByZero,
    /// Numeric scalar evaluation produced an undefined or non-finite
    /// float result (domain error, overflow, NaN, or infinity).
    InvalidNumericResult { function: String, reason: String },
    /// `IN (...)` against an empty value list — preserves the legacy
    /// "always false" semantic but recorded explicitly so the
    /// optimiser can fold it.
    EmptyInList,
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalError::UnknownColumn(field) => write!(f, "unknown column: {field:?}"),
            EvalError::UnboundParameter(idx) => {
                write!(f, "unbound query parameter: ${idx}")
            }
            EvalError::OperatorMismatch { op, lhs, rhs } => {
                write!(f, "operator {op:?} not defined for ({lhs:?}, {rhs:?})")
            }
            EvalError::UnaryMismatch { op, operand } => {
                write!(f, "unary {op:?} not defined for {operand:?}")
            }
            EvalError::UnknownFunction { name, args } => {
                write!(f, "unknown function: {name}({args:?})")
            }
            EvalError::ImplicitCastFailed { from, to, reason } => {
                write!(f, "implicit cast {from:?} -> {to:?} failed: {reason}")
            }
            EvalError::CastFailed { from, to, reason } => {
                write!(f, "cast {from:?} -> {to:?} failed: {reason}")
            }
            EvalError::ArithmeticOverflow { op } => {
                write!(f, "arithmetic overflow in {op:?}")
            }
            EvalError::DivisionByZero => write!(f, "division by zero"),
            EvalError::InvalidNumericResult { function, reason } => {
                write!(f, "invalid numeric result in {function}: {reason}")
            }
            EvalError::EmptyInList => write!(f, "IN list is empty"),
        }
    }
}

impl std::error::Error for EvalError {}

/// Evaluate a scalar `Expr` against a row binding. Single entry
/// point for the deep evaluator interface — every recursive call
/// folds back through this function so the resolution surface stays
/// uniform.
pub fn evaluate(expr: &Expr, row: &dyn Row) -> Result<Value, EvalError> {
    match expr {
        Expr::Literal { value, .. } => Ok(value.clone()),
        Expr::Column { field, .. } => row
            .get(field)
            .ok_or_else(|| EvalError::UnknownColumn(field.clone())),
        Expr::Parameter { index, .. } => Err(EvalError::UnboundParameter(*index)),
        Expr::UnaryOp { op, operand, .. } => eval_unary(*op, operand, row),
        Expr::BinaryOp { op, lhs, rhs, .. } => eval_binary(*op, lhs, rhs, row),
        Expr::Cast { inner, target, .. } => eval_cast(inner, *target, row),
        Expr::FunctionCall { name, args, .. } => eval_function(name, args, row),
        Expr::Case {
            branches, else_, ..
        } => eval_case(branches, else_.as_deref(), row),
        Expr::IsNull {
            operand, negated, ..
        } => {
            let v = evaluate(operand, row)?;
            let is_null = v.is_null();
            Ok(Value::Boolean(if *negated { !is_null } else { is_null }))
        }
        Expr::InList {
            target,
            values,
            negated,
            ..
        } => eval_in_list(target, values, *negated, row),
        Expr::Between {
            target,
            low,
            high,
            negated,
            ..
        } => eval_between(target, low, high, *negated, row),
        Expr::Subquery { .. } => Err(EvalError::UnknownFunction {
            name: "SUBQUERY".to_string(),
            args: Vec::new(),
        }),
    }
}

fn eval_unary(op: UnaryOp, operand: &Expr, row: &dyn Row) -> Result<Value, EvalError> {
    let v = evaluate(operand, row)?;
    if v.is_null() {
        return Ok(Value::Null);
    }
    match op {
        UnaryOp::Neg => match &v {
            Value::Integer(n) => n
                .checked_neg()
                .map(Value::Integer)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Sub }),
            Value::BigInt(n) => n
                .checked_neg()
                .map(Value::BigInt)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Sub }),
            Value::Float(n) => Ok(Value::Float(-*n)),
            Value::Decimal(n) => n
                .checked_neg()
                .map(Value::Decimal)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Sub }),
            other => Err(EvalError::UnaryMismatch {
                op,
                operand: other.data_type(),
            }),
        },
        UnaryOp::Not => match &v {
            Value::Boolean(b) => Ok(Value::Boolean(!b)),
            other => Err(EvalError::UnaryMismatch {
                op,
                operand: other.data_type(),
            }),
        },
    }
}

fn eval_binary(op: BinOp, lhs: &Expr, rhs: &Expr, row: &dyn Row) -> Result<Value, EvalError> {
    // Logical ops use SQL three-valued logic so we eval both sides
    // and short-circuit on Null *after* type-checking; arithmetic /
    // comparison / concat short-circuit before dispatch.
    let l = evaluate(lhs, row)?;
    let r = evaluate(rhs, row)?;

    match op {
        BinOp::And => return three_valued_and(&l, &r),
        BinOp::Or => return three_valued_or(&l, &r),
        _ => {}
    }

    if l.is_null() || r.is_null() {
        return Ok(Value::Null);
    }

    let lhs_dt = l.data_type();
    let rhs_dt = r.data_type();
    let (entry, coercions) =
        coercion_spine::resolve_binop(op, lhs_dt, rhs_dt).ok_or(EvalError::OperatorMismatch {
            op,
            lhs: lhs_dt,
            rhs: rhs_dt,
        })?;

    let l = match coercions.at(0) {
        Some(target) => apply_implicit_cast(&l, lhs_dt, target)?,
        None => l,
    };
    let r = match coercions.at(1) {
        Some(target) => apply_implicit_cast(&r, rhs_dt, target)?,
        None => r,
    };

    dispatch_binop(op, entry, l, r)
}

fn dispatch_binop(
    op: BinOp,
    entry: &OperatorEntry,
    l: Value,
    r: Value,
) -> Result<Value, EvalError> {
    match op {
        BinOp::Add => arith_add(entry, l, r),
        BinOp::Sub => arith_sub(entry, l, r),
        BinOp::Mul => arith_mul(entry, l, r),
        BinOp::Div => arith_div(entry, l, r),
        BinOp::Mod => arith_mod(entry, l, r),
        BinOp::Concat => match (&l, &r) {
            (Value::Text(a), Value::Text(b)) => {
                let mut s = String::with_capacity(a.len() + b.len());
                s.push_str(a);
                s.push_str(b);
                Ok(Value::Text(Arc::from(s)))
            }
            _ => Err(EvalError::OperatorMismatch {
                op,
                lhs: l.data_type(),
                rhs: r.data_type(),
            }),
        },
        BinOp::Eq => Ok(Value::Boolean(values_equal(&l, &r))),
        BinOp::Ne => Ok(Value::Boolean(!values_equal(&l, &r))),
        BinOp::Lt => cmp_op(op, l, r, |o| o == std::cmp::Ordering::Less),
        BinOp::Le => cmp_op(op, l, r, |o| o != std::cmp::Ordering::Greater),
        BinOp::Gt => cmp_op(op, l, r, |o| o == std::cmp::Ordering::Greater),
        BinOp::Ge => cmp_op(op, l, r, |o| o != std::cmp::Ordering::Less),
        BinOp::And | BinOp::Or => unreachable!("handled before dispatch"),
    }
}

fn arith_add(entry: &OperatorEntry, l: Value, r: Value) -> Result<Value, EvalError> {
    match entry.return_type {
        DataType::Integer => match (l, r) {
            (Value::Integer(a), Value::Integer(b)) => a
                .checked_add(b)
                .map(Value::Integer)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Add }),
            _ => unreachable_after_coercion("Add", DataType::Integer),
        },
        DataType::BigInt => match (l, r) {
            (Value::BigInt(a), Value::BigInt(b)) => a
                .checked_add(b)
                .map(Value::BigInt)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Add }),
            _ => unreachable_after_coercion("Add", DataType::BigInt),
        },
        DataType::Float => checked_float_binop(BinOp::Add, as_f64(&l) + as_f64(&r)),
        DataType::Decimal => match (l, r) {
            (Value::Decimal(a), Value::Decimal(b)) => a
                .checked_add(b)
                .map(Value::Decimal)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Add }),
            _ => unreachable_after_coercion("Add", DataType::Decimal),
        },
        other => Err(EvalError::OperatorMismatch {
            op: BinOp::Add,
            lhs: other,
            rhs: other,
        }),
    }
}

fn arith_sub(entry: &OperatorEntry, l: Value, r: Value) -> Result<Value, EvalError> {
    match entry.return_type {
        DataType::Integer => match (l, r) {
            (Value::Integer(a), Value::Integer(b)) => a
                .checked_sub(b)
                .map(Value::Integer)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Sub }),
            _ => unreachable_after_coercion("Sub", DataType::Integer),
        },
        DataType::BigInt => match (l, r) {
            (Value::BigInt(a), Value::BigInt(b)) => a
                .checked_sub(b)
                .map(Value::BigInt)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Sub }),
            _ => unreachable_after_coercion("Sub", DataType::BigInt),
        },
        DataType::Float => checked_float_binop(BinOp::Sub, as_f64(&l) - as_f64(&r)),
        DataType::Decimal => match (l, r) {
            (Value::Decimal(a), Value::Decimal(b)) => a
                .checked_sub(b)
                .map(Value::Decimal)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Sub }),
            _ => unreachable_after_coercion("Sub", DataType::Decimal),
        },
        other => Err(EvalError::OperatorMismatch {
            op: BinOp::Sub,
            lhs: other,
            rhs: other,
        }),
    }
}

fn arith_mul(entry: &OperatorEntry, l: Value, r: Value) -> Result<Value, EvalError> {
    match entry.return_type {
        DataType::Integer => match (l, r) {
            (Value::Integer(a), Value::Integer(b)) => a
                .checked_mul(b)
                .map(Value::Integer)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Mul }),
            _ => unreachable_after_coercion("Mul", DataType::Integer),
        },
        DataType::BigInt => match (l, r) {
            (Value::BigInt(a), Value::BigInt(b)) => a
                .checked_mul(b)
                .map(Value::BigInt)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Mul }),
            _ => unreachable_after_coercion("Mul", DataType::BigInt),
        },
        DataType::Float => checked_float_binop(BinOp::Mul, as_f64(&l) * as_f64(&r)),
        other => Err(EvalError::OperatorMismatch {
            op: BinOp::Mul,
            lhs: other,
            rhs: other,
        }),
    }
}

fn arith_div(entry: &OperatorEntry, l: Value, r: Value) -> Result<Value, EvalError> {
    // Operator catalog promotes integer / integer to Float —
    // mirror that here so behavior stays identical to the typer.
    match entry.return_type {
        DataType::Float => {
            let denom = as_f64(&r);
            if denom == 0.0 {
                return Err(EvalError::DivisionByZero);
            }
            checked_float_binop(BinOp::Div, as_f64(&l) / denom)
        }
        other => Err(EvalError::OperatorMismatch {
            op: BinOp::Div,
            lhs: other,
            rhs: other,
        }),
    }
}

fn arith_mod(entry: &OperatorEntry, l: Value, r: Value) -> Result<Value, EvalError> {
    match entry.return_type {
        DataType::Integer => match (l, r) {
            (Value::Integer(_), Value::Integer(0)) => Err(EvalError::DivisionByZero),
            (Value::Integer(a), Value::Integer(b)) => a
                .checked_rem(b)
                .map(Value::Integer)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Mod }),
            _ => unreachable_after_coercion("Mod", DataType::Integer),
        },
        DataType::BigInt => match (l, r) {
            (Value::BigInt(_), Value::BigInt(0)) => Err(EvalError::DivisionByZero),
            (Value::BigInt(a), Value::BigInt(b)) => a
                .checked_rem(b)
                .map(Value::BigInt)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Mod }),
            _ => unreachable_after_coercion("Mod", DataType::BigInt),
        },
        other => Err(EvalError::OperatorMismatch {
            op: BinOp::Mod,
            lhs: other,
            rhs: other,
        }),
    }
}

fn unreachable_after_coercion(op: &'static str, expected: DataType) -> Result<Value, EvalError> {
    Err(EvalError::OperatorMismatch {
        op: match op {
            "Add" => BinOp::Add,
            "Sub" => BinOp::Sub,
            "Mul" => BinOp::Mul,
            "Div" => BinOp::Div,
            "Mod" => BinOp::Mod,
            _ => BinOp::Add,
        },
        lhs: expected,
        rhs: expected,
    })
}

fn checked_float_binop(op: BinOp, value: f64) -> Result<Value, EvalError> {
    if value.is_finite() {
        Ok(Value::Float(value))
    } else {
        Err(EvalError::InvalidNumericResult {
            function: format!("{op:?}"),
            reason: "result is NaN or infinite".to_string(),
        })
    }
}

fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Float(x) => *x,
        Value::Integer(x) => *x as f64,
        Value::BigInt(x) => *x as f64,
        Value::UnsignedInteger(x) => *x as f64,
        Value::Decimal(x) => *x as f64,
        _ => 0.0,
    }
}

fn cmp_op<F>(op: BinOp, l: Value, r: Value, pick: F) -> Result<Value, EvalError>
where
    F: Fn(std::cmp::Ordering) -> bool,
{
    let ord = compare_values(&l, &r).ok_or(EvalError::OperatorMismatch {
        op,
        lhs: l.data_type(),
        rhs: r.data_type(),
    })?;
    Ok(Value::Boolean(pick(ord)))
}

/// Total ordering for the numeric and text families that the
/// catalog declares comparison overloads for. Returns `None` when
/// the values aren't comparable — callers surface this as
/// [`EvalError::OperatorMismatch`].
fn compare_values(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Integer(x), Value::Integer(y)) => Some(x.cmp(y)),
        (Value::BigInt(x), Value::BigInt(y)) => Some(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Text(x), Value::Text(y)) => Some(x.as_ref().cmp(y.as_ref())),
        (Value::Boolean(x), Value::Boolean(y)) => Some(x.cmp(y)),
        (Value::Timestamp(x), Value::Timestamp(y)) => Some(x.cmp(y)),
        (Value::TimestampMs(x), Value::TimestampMs(y)) => Some(x.cmp(y)),
        (Value::Date(x), Value::Date(y)) => Some(x.cmp(y)),
        (Value::Time(x), Value::Time(y)) => Some(x.cmp(y)),
        (Value::Uuid(x), Value::Uuid(y)) => Some(x.cmp(y)),
        (Value::Decimal(x), Value::Decimal(y)) => Some(x.cmp(y)),
        // Cross-numeric — operand coercion should have homogenised
        // these but if a caller invokes the evaluator with mixed
        // numerics directly, fall back to f64 ordering.
        (Value::Integer(_) | Value::Float(_) | Value::BigInt(_), _) => {
            let l = as_f64(a);
            let r = as_f64(b);
            l.partial_cmp(&r)
        }
        _ => None,
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => x == y,
        _ => a == b,
    }
}

fn three_valued_and(l: &Value, r: &Value) -> Result<Value, EvalError> {
    match (l, r) {
        (Value::Boolean(false), _) | (_, Value::Boolean(false)) => Ok(Value::Boolean(false)),
        (Value::Boolean(true), Value::Boolean(true)) => Ok(Value::Boolean(true)),
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        _ => Err(EvalError::OperatorMismatch {
            op: BinOp::And,
            lhs: l.data_type(),
            rhs: r.data_type(),
        }),
    }
}

fn three_valued_or(l: &Value, r: &Value) -> Result<Value, EvalError> {
    match (l, r) {
        (Value::Boolean(true), _) | (_, Value::Boolean(true)) => Ok(Value::Boolean(true)),
        (Value::Boolean(false), Value::Boolean(false)) => Ok(Value::Boolean(false)),
        (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
        _ => Err(EvalError::OperatorMismatch {
            op: BinOp::Or,
            lhs: l.data_type(),
            rhs: r.data_type(),
        }),
    }
}

fn apply_implicit_cast(value: &Value, src: DataType, target: DataType) -> Result<Value, EvalError> {
    if src == target {
        return Ok(value.clone());
    }
    coerce_via_catalog(value, src, target, None).map_err(|reason| EvalError::ImplicitCastFailed {
        from: src,
        to: target,
        reason,
    })
}

fn eval_cast(inner: &Expr, target: DataType, row: &dyn Row) -> Result<Value, EvalError> {
    let v = evaluate(inner, row)?;
    if v.is_null() {
        return Ok(Value::Null);
    }
    let src = v.data_type();
    if src == target {
        return Ok(v);
    }
    coerce_via_catalog(&v, src, target, None).map_err(|reason| EvalError::CastFailed {
        from: src,
        to: target,
        reason,
    })
}

fn eval_function(name: &str, args: &[Expr], row: &dyn Row) -> Result<Value, EvalError> {
    // COALESCE has SQL-special semantics that the catalog can't
    // express (variadic + uniform arg type unifies poorly with
    // first-non-null). Handle it before catalog dispatch so we
    // preserve `COALESCE(int, int) -> int` rather than coercing
    // every argument to Text.
    if name.eq_ignore_ascii_case("COALESCE") {
        for arg in args {
            let v = evaluate(arg, row)?;
            if !v.is_null() {
                return Ok(v);
            }
        }
        return Ok(Value::Null);
    }

    let arg_values: Vec<Value> = args
        .iter()
        .map(|a| evaluate(a, row))
        .collect::<Result<Vec<_>, _>>()?;
    let arg_types: Vec<DataType> = arg_values.iter().map(|v| v.data_type()).collect();

    // Strict NULL propagation for built-in scalar functions: any
    // null arg short-circuits the call to NULL. Only applies when
    // the function name exists in the catalog so unknown functions
    // with null args still surface as `UnknownFunction` rather than
    // silently returning null.
    if arg_values.iter().any(Value::is_null)
        && FUNCTION_CATALOG
            .iter()
            .any(|e| e.name.eq_ignore_ascii_case(name))
    {
        return Ok(Value::Null);
    }

    let (entry, coercions) =
        coercion_spine::resolve_function(name, &arg_types).ok_or_else(|| {
            EvalError::UnknownFunction {
                name: name.to_string(),
                args: arg_types.clone(),
            }
        })?;

    // Apply per-arg implicit casts.
    let mut coerced: Vec<Value> = Vec::with_capacity(arg_values.len());
    for (idx, value) in arg_values.into_iter().enumerate() {
        let src = arg_types[idx];
        match coercions.at(idx) {
            Some(target) if src != target => {
                coerced.push(apply_implicit_cast(&value, src, target)?);
            }
            _ => coerced.push(value),
        }
    }

    // NULL propagation for built-ins: if any non-variadic argument
    // is null, return null. Variadic / aggregate semantics handle
    // null differently and aren't in scope for this slice.
    if !entry.variadic && coerced.iter().any(|v| v.is_null()) {
        return Ok(Value::Null);
    }

    dispatch_function(entry.name, &coerced)
}

fn dispatch_function(name: &str, args: &[Value]) -> Result<Value, EvalError> {
    match name {
        "UPPER" => match &args[0] {
            Value::Text(s) => Ok(Value::Text(Arc::from(s.to_uppercase()))),
            other => Err(EvalError::UnknownFunction {
                name: name.to_string(),
                args: vec![other.data_type()],
            }),
        },
        "LOWER" => match &args[0] {
            Value::Text(s) => Ok(Value::Text(Arc::from(s.to_lowercase()))),
            other => Err(EvalError::UnknownFunction {
                name: name.to_string(),
                args: vec![other.data_type()],
            }),
        },
        "LENGTH" | "CHAR_LENGTH" | "CHARACTER_LENGTH" => match &args[0] {
            Value::Text(s) => Ok(Value::Integer(s.chars().count() as i64)),
            other => Err(EvalError::UnknownFunction {
                name: name.to_string(),
                args: vec![other.data_type()],
            }),
        },
        "OCTET_LENGTH" => match &args[0] {
            Value::Text(s) => Ok(Value::Integer(s.len() as i64)),
            Value::Blob(b) => Ok(Value::Integer(b.len() as i64)),
            other => Err(EvalError::UnknownFunction {
                name: name.to_string(),
                args: vec![other.data_type()],
            }),
        },
        "JSON_EXTRACT" => Ok(json_extract_value(&args[0], &args[1], false)),
        "JSON_EXTRACT_TEXT" => Ok(json_extract_value(&args[0], &args[1], true)),
        "CONTAINS" => Ok(contains_value(&args[0], &args[1])),
        "ABS" => match &args[0] {
            Value::Integer(n) => n
                .checked_abs()
                .map(Value::Integer)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Sub }),
            Value::BigInt(n) => n
                .checked_abs()
                .map(Value::BigInt)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Sub }),
            Value::Float(n) => Ok(Value::Float(n.abs())),
            Value::Decimal(n) => n
                .checked_abs()
                .map(Value::Decimal)
                .ok_or(EvalError::ArithmeticOverflow { op: BinOp::Sub }),
            other => Err(EvalError::UnknownFunction {
                name: name.to_string(),
                args: vec![other.data_type()],
            }),
        },
        "SQRT" => unary_math(name, args, |x| {
            if x < 0.0 {
                return Err("input must be greater than or equal to zero");
            }
            Ok(x.sqrt())
        }),
        "POWER" | "POW" => binary_math(name, args, |base, exp| Ok(base.powf(exp))),
        "EXP" => unary_math(name, args, |x| Ok(x.exp())),
        "LN" => unary_math(name, args, |x| {
            if x <= 0.0 {
                return Err("input must be greater than zero");
            }
            Ok(x.ln())
        }),
        "LOG" if args.len() == 1 => unary_math(name, args, |x| {
            if x <= 0.0 {
                return Err("input must be greater than zero");
            }
            Ok(x.log10())
        }),
        "LOG" => binary_math(name, args, |base, x| {
            if base <= 0.0 {
                return Err("base must be greater than zero");
            }
            if base == 1.0 {
                return Err("base must not equal one");
            }
            if x <= 0.0 {
                return Err("input must be greater than zero");
            }
            Ok(x.log(base))
        }),
        "LOG10" => unary_math(name, args, |x| {
            if x <= 0.0 {
                return Err("input must be greater than zero");
            }
            Ok(x.log10())
        }),
        "SIN" => unary_math(name, args, |x| Ok(x.sin())),
        "COS" => unary_math(name, args, |x| Ok(x.cos())),
        "TAN" => unary_math(name, args, |x| Ok(x.tan())),
        "ASIN" | "ARCSIN" => unary_math(name, args, |x| {
            if !(-1.0..=1.0).contains(&x) {
                return Err("input must be between -1 and 1");
            }
            Ok(x.asin())
        }),
        "ACOS" | "ARCCOS" => unary_math(name, args, |x| {
            if !(-1.0..=1.0).contains(&x) {
                return Err("input must be between -1 and 1");
            }
            Ok(x.acos())
        }),
        "ATAN" | "ARCTAN" => unary_math(name, args, |x| Ok(x.atan())),
        "ATAN2" => binary_math(name, args, |y, x| Ok(y.atan2(x))),
        "COT" => unary_math(name, args, |x| {
            let tan = x.tan();
            if tan == 0.0 {
                return Err("input must not produce zero tangent");
            }
            Ok(1.0 / tan)
        }),
        "DEGREES" => unary_math(name, args, |x| Ok(x.to_degrees())),
        "RADIANS" => unary_math(name, args, |x| Ok(x.to_radians())),
        "PI" => checked_math_result(name, std::f64::consts::PI),
        // Functions whose runtime body the slice doesn't yet cover
        // surface as UnknownFunction with the resolved arg types so
        // callers can see the catalog matched but the dispatch
        // didn't. Subsequent slices fill in CONCAT, time functions, …
        other => Err(EvalError::UnknownFunction {
            name: other.to_string(),
            args: args.iter().map(|v| v.data_type()).collect(),
        }),
    }
}

fn unary_math<F>(name: &str, args: &[Value], op: F) -> Result<Value, EvalError>
where
    F: FnOnce(f64) -> Result<f64, &'static str>,
{
    let input = math_arg(name, args.first(), 0)?;
    let value = op(input).map_err(|reason| EvalError::InvalidNumericResult {
        function: name.to_string(),
        reason: reason.to_string(),
    })?;
    checked_math_result(name, value)
}

fn binary_math<F>(name: &str, args: &[Value], op: F) -> Result<Value, EvalError>
where
    F: FnOnce(f64, f64) -> Result<f64, &'static str>,
{
    let left = math_arg(name, args.first(), 0)?;
    let right = math_arg(name, args.get(1), 1)?;
    let value = op(left, right).map_err(|reason| EvalError::InvalidNumericResult {
        function: name.to_string(),
        reason: reason.to_string(),
    })?;
    checked_math_result(name, value)
}

fn math_arg(name: &str, value: Option<&Value>, index: usize) -> Result<f64, EvalError> {
    let value = value.ok_or_else(|| EvalError::UnknownFunction {
        name: name.to_string(),
        args: Vec::new(),
    })?;
    let numeric = as_f64(value);
    if numeric.is_finite() {
        Ok(numeric)
    } else {
        Err(EvalError::InvalidNumericResult {
            function: name.to_string(),
            reason: format!("argument {index} is NaN or infinite"),
        })
    }
}

fn checked_math_result(name: &str, value: f64) -> Result<Value, EvalError> {
    if value.is_finite() {
        Ok(Value::Float(value))
    } else {
        Err(EvalError::InvalidNumericResult {
            function: name.to_string(),
            reason: "result is NaN or infinite".to_string(),
        })
    }
}

fn json_extract_value(input: &Value, path: &Value, as_text: bool) -> Value {
    let Value::Text(path) = path else {
        return Value::Null;
    };
    let Some(json) = value_to_json(input) else {
        return Value::Null;
    };
    let Some(steps) = parse_json_path(path) else {
        return Value::Null;
    };
    let Some(target) = json_path_get(&json, &steps) else {
        return Value::Null;
    };

    if as_text {
        match target {
            crate::serde_json::Value::String(value) => Value::text(value.clone()),
            crate::serde_json::Value::Null => Value::Null,
            crate::serde_json::Value::Bool(value) => Value::text(value.to_string()),
            crate::serde_json::Value::Number(value) => Value::text(value.to_string()),
            other => Value::text(other.to_string_compact()),
        }
    } else {
        Value::text(target.to_string_compact())
    }
}

fn contains_value(input: &Value, needle: &Value) -> Value {
    let Value::Text(needle) = needle else {
        return Value::Null;
    };
    Value::Boolean(value_contains(input, needle))
}

fn value_contains(value: &Value, needle: &str) -> bool {
    match value {
        Value::Array(values) => values.iter().any(|value| value_contains(value, needle)),
        Value::Json(_) => value_to_json(value)
            .as_ref()
            .is_some_and(|json| json_value_contains(json, needle)),
        Value::Text(value) => value.contains(needle),
        other => other.display_string().contains(needle),
    }
}

fn json_value_contains(value: &crate::serde_json::Value, needle: &str) -> bool {
    match value {
        crate::serde_json::Value::Array(values) => values
            .iter()
            .any(|value| json_value_contains(value, needle)),
        crate::serde_json::Value::String(value) => value == needle,
        crate::serde_json::Value::Number(value) => value.to_string() == needle,
        crate::serde_json::Value::Bool(value) => value.to_string() == needle,
        crate::serde_json::Value::Null | crate::serde_json::Value::Object(_) => false,
    }
}

fn value_to_json(value: &Value) -> Option<crate::serde_json::Value> {
    match value {
        Value::Null => Some(crate::serde_json::Value::Null),
        Value::Json(bytes) => crate::serde_json::from_slice(bytes).ok(),
        Value::Text(value) => crate::serde_json::from_str(value).ok(),
        _ => None,
    }
}

enum JsonPathStep<'a> {
    Field(&'a str),
    Index(usize),
}

fn parse_json_path(path: &str) -> Option<Vec<JsonPathStep<'_>>> {
    let path = path.trim();
    let rest = path.strip_prefix('$').unwrap_or(path);
    let mut steps = Vec::new();
    let bytes = rest.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'.' => {
                index += 1;
                let start = index;
                while index < bytes.len() && bytes[index] != b'.' && bytes[index] != b'[' {
                    index += 1;
                }
                if start == index {
                    return None;
                }
                steps.push(JsonPathStep::Field(
                    std::str::from_utf8(&bytes[start..index]).ok()?,
                ));
            }
            b'[' => {
                index += 1;
                let start = index;
                while index < bytes.len() && bytes[index] != b']' {
                    index += 1;
                }
                if index >= bytes.len() {
                    return None;
                }
                steps.push(JsonPathStep::Index(
                    std::str::from_utf8(&bytes[start..index])
                        .ok()?
                        .parse()
                        .ok()?,
                ));
                index += 1;
            }
            _ => return None,
        }
    }
    Some(steps)
}

fn json_path_get<'a>(
    root: &'a crate::serde_json::Value,
    steps: &[JsonPathStep<'_>],
) -> Option<&'a crate::serde_json::Value> {
    let mut current = root;
    for step in steps {
        current = match (step, current) {
            (JsonPathStep::Field(name), crate::serde_json::Value::Object(map)) => map.get(*name)?,
            (JsonPathStep::Index(index), crate::serde_json::Value::Array(values)) => {
                values.get(*index)?
            }
            _ => return None,
        };
    }
    Some(current)
}

fn eval_case(
    branches: &[(Expr, Expr)],
    else_: Option<&Expr>,
    row: &dyn Row,
) -> Result<Value, EvalError> {
    for (cond, value) in branches {
        let c = evaluate(cond, row)?;
        if matches!(c, Value::Boolean(true)) {
            return evaluate(value, row);
        }
    }
    match else_ {
        Some(e) => evaluate(e, row),
        None => Ok(Value::Null),
    }
}

fn eval_in_list(
    target: &Expr,
    values: &[Expr],
    negated: bool,
    row: &dyn Row,
) -> Result<Value, EvalError> {
    if values.is_empty() {
        return Err(EvalError::EmptyInList);
    }
    let needle = evaluate(target, row)?;
    if needle.is_null() {
        return Ok(Value::Null);
    }
    let mut saw_null = false;
    for v in values {
        let candidate = evaluate(v, row)?;
        if candidate.is_null() {
            saw_null = true;
            continue;
        }
        if values_equal(&needle, &candidate) {
            return Ok(Value::Boolean(!negated));
        }
    }
    if saw_null {
        return Ok(Value::Null);
    }
    Ok(Value::Boolean(negated))
}

fn eval_between(
    target: &Expr,
    low: &Expr,
    high: &Expr,
    negated: bool,
    row: &dyn Row,
) -> Result<Value, EvalError> {
    let v = evaluate(target, row)?;
    let lo = evaluate(low, row)?;
    let hi = evaluate(high, row)?;
    if v.is_null() || lo.is_null() || hi.is_null() {
        return Ok(Value::Null);
    }
    let lo_ok = compare_values(&v, &lo)
        .ok_or(EvalError::OperatorMismatch {
            op: BinOp::Ge,
            lhs: v.data_type(),
            rhs: lo.data_type(),
        })
        .map(|o| o != std::cmp::Ordering::Less)?;
    let hi_ok = compare_values(&v, &hi)
        .ok_or(EvalError::OperatorMismatch {
            op: BinOp::Le,
            lhs: v.data_type(),
            rhs: hi.data_type(),
        })
        .map(|o| o != std::cmp::Ordering::Greater)?;
    let inside = lo_ok && hi_ok;
    Ok(Value::Boolean(if negated { !inside } else { inside }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::ast::Span;

    fn lit(v: Value) -> Expr {
        Expr::Literal {
            value: v,
            span: Span::synthetic(),
        }
    }

    fn binop(op: BinOp, l: Expr, r: Expr) -> Expr {
        Expr::BinaryOp {
            op,
            lhs: Box::new(l),
            rhs: Box::new(r),
            span: Span::synthetic(),
        }
    }

    fn empty_row() -> impl Row {
        |_field: &FieldRef| -> Option<Value> { None }
    }

    #[test]
    fn integer_addition_overflow_surfaces_as_eval_error() {
        let expr = binop(
            BinOp::Add,
            lit(Value::Integer(i64::MAX)),
            lit(Value::Integer(1)),
        );
        let err = evaluate(&expr, &empty_row()).unwrap_err();
        assert_eq!(err, EvalError::ArithmeticOverflow { op: BinOp::Add });
    }

    #[test]
    fn integer_multiplication_overflow_surfaces_as_eval_error() {
        let expr = binop(
            BinOp::Mul,
            lit(Value::Integer(i64::MAX)),
            lit(Value::Integer(2)),
        );
        let err = evaluate(&expr, &empty_row()).unwrap_err();
        assert_eq!(err, EvalError::ArithmeticOverflow { op: BinOp::Mul });
    }

    #[test]
    fn integer_subtraction_overflow_surfaces_as_eval_error() {
        let expr = binop(
            BinOp::Sub,
            lit(Value::Integer(i64::MIN)),
            lit(Value::Integer(1)),
        );
        let err = evaluate(&expr, &empty_row()).unwrap_err();
        assert_eq!(err, EvalError::ArithmeticOverflow { op: BinOp::Sub });
    }

    #[test]
    fn unary_neg_overflow_on_min_int_surfaces_as_eval_error() {
        let expr = Expr::UnaryOp {
            op: UnaryOp::Neg,
            operand: Box::new(lit(Value::Integer(i64::MIN))),
            span: Span::synthetic(),
        };
        let err = evaluate(&expr, &empty_row()).unwrap_err();
        assert_eq!(err, EvalError::ArithmeticOverflow { op: BinOp::Sub });
    }

    #[test]
    fn null_propagates_through_arithmetic() {
        let expr = binop(BinOp::Add, lit(Value::Null), lit(Value::Integer(7)));
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Null);
    }

    #[test]
    fn null_propagates_through_comparison() {
        let expr = binop(BinOp::Lt, lit(Value::Integer(1)), lit(Value::Null));
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Null);
    }

    #[test]
    fn null_propagates_through_concat() {
        let expr = binop(
            BinOp::Concat,
            lit(Value::Text(Arc::from("hi"))),
            lit(Value::Null),
        );
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Null);
    }

    #[test]
    fn three_valued_and_returns_false_when_one_side_false_even_with_null() {
        let expr = binop(BinOp::And, lit(Value::Null), lit(Value::Boolean(false)));
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Boolean(false));
    }

    #[test]
    fn three_valued_or_returns_true_when_one_side_true_even_with_null() {
        let expr = binop(BinOp::Or, lit(Value::Null), lit(Value::Boolean(true)));
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Boolean(true));
    }

    #[test]
    fn three_valued_and_returns_null_for_null_and_true() {
        let expr = binop(BinOp::And, lit(Value::Null), lit(Value::Boolean(true)));
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Null);
    }

    #[test]
    fn implicit_cast_triggers_for_decimal_plus_integer() {
        // Operator catalog has +(Decimal, Decimal) -> Decimal as
        // the only overload that survives coercion (no Decimal ->
        // numeric implicit casts exist in the cast catalog). The
        // spine therefore inserts an Integer -> Decimal cast on
        // the rhs and dispatches the Decimal addition.
        // parse_decimal scales by 10^4, so Integer(2) coerces to
        // Decimal(20000) and Decimal(10000) + Decimal(20000) =
        // Decimal(30000) (fixed-point 3.0000).
        let expr = binop(
            BinOp::Add,
            lit(Value::Decimal(10000)),
            lit(Value::Integer(2)),
        );
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Decimal(30000));
    }

    #[test]
    fn integer_plus_bigint_resolves_to_preferred_float_overload() {
        // (Integer, BigInt) has no exact match. The spine ties
        // between +(Integer, Float, Float) (rhs cast BigInt->Float)
        // and +(BigInt, BigInt, BigInt) (lhs cast Integer->BigInt).
        // Float wins on the preferred-return-type tie-break, so the
        // BigInt operand is coerced to Float and the result is Float.
        let expr = binop(
            BinOp::Add,
            lit(Value::Integer(5)),
            lit(Value::BigInt(40_000_000_000)),
        );
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Float(40_000_000_005.0));
    }

    #[test]
    fn implicit_cast_promotes_integer_to_float_for_float_addition() {
        // The catalog has +(Integer, Float) -> Float as a direct
        // entry, so no actual coercion is inserted, but the result
        // must still be Float.
        let expr = binop(BinOp::Add, lit(Value::Integer(2)), lit(Value::Float(0.5)));
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Float(2.5));
    }

    #[test]
    fn integer_division_promotes_to_float() {
        let expr = binop(BinOp::Div, lit(Value::Integer(7)), lit(Value::Integer(2)));
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Float(3.5));
    }

    #[test]
    fn division_by_zero_is_eval_error() {
        let expr = binop(BinOp::Div, lit(Value::Integer(7)), lit(Value::Integer(0)));
        let err = evaluate(&expr, &empty_row()).unwrap_err();
        assert_eq!(err, EvalError::DivisionByZero);
    }

    #[test]
    fn unknown_function_surfaces_as_eval_error() {
        let expr = Expr::FunctionCall {
            name: "no_such_fn".to_string(),
            args: vec![lit(Value::Integer(1))],
            span: Span::synthetic(),
        };
        let err = evaluate(&expr, &empty_row()).unwrap_err();
        match err {
            EvalError::UnknownFunction { name, args } => {
                assert_eq!(name, "no_such_fn");
                assert_eq!(args, vec![DataType::Integer]);
            }
            other => panic!("expected UnknownFunction, got {other:?}"),
        }
    }

    #[test]
    fn coalesce_returns_first_non_null() {
        let expr = Expr::FunctionCall {
            name: "COALESCE".to_string(),
            args: vec![
                lit(Value::Null),
                lit(Value::Null),
                lit(Value::Integer(42)),
                lit(Value::Integer(99)),
            ],
            span: Span::synthetic(),
        };
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Integer(42));
    }

    #[test]
    fn coalesce_all_null_returns_null() {
        let expr = Expr::FunctionCall {
            name: "COALESCE".to_string(),
            args: vec![lit(Value::Null), lit(Value::Null)],
            span: Span::synthetic(),
        };
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Null);
    }

    #[test]
    fn upper_lower_dispatch_through_function_catalog() {
        let expr = Expr::FunctionCall {
            name: "UPPER".to_string(),
            args: vec![lit(Value::Text(Arc::from("hello")))],
            span: Span::synthetic(),
        };
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Text(Arc::from("HELLO")));
    }

    #[test]
    fn length_of_null_propagates() {
        let expr = Expr::FunctionCall {
            name: "LENGTH".to_string(),
            args: vec![lit(Value::Null)],
            span: Span::synthetic(),
        };
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Null);
    }

    #[test]
    fn case_when_picks_first_true_branch() {
        let expr = Expr::Case {
            branches: vec![
                (lit(Value::Boolean(false)), lit(Value::Integer(1))),
                (lit(Value::Boolean(true)), lit(Value::Integer(2))),
                (lit(Value::Boolean(true)), lit(Value::Integer(3))),
            ],
            else_: Some(Box::new(lit(Value::Integer(99)))),
            span: Span::synthetic(),
        };
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Integer(2));
    }

    #[test]
    fn case_falls_through_to_else_when_no_branch_matches() {
        let expr = Expr::Case {
            branches: vec![(lit(Value::Boolean(false)), lit(Value::Integer(1)))],
            else_: Some(Box::new(lit(Value::Integer(99)))),
            span: Span::synthetic(),
        };
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Integer(99));
    }

    #[test]
    fn case_returns_null_when_no_branch_matches_and_no_else() {
        let expr = Expr::Case {
            branches: vec![(lit(Value::Boolean(false)), lit(Value::Integer(1)))],
            else_: None,
            span: Span::synthetic(),
        };
        let v = evaluate(&expr, &empty_row()).unwrap();
        assert_eq!(v, Value::Null);
    }

    #[test]
    fn is_null_handles_null_and_non_null() {
        let null_expr = Expr::IsNull {
            operand: Box::new(lit(Value::Null)),
            negated: false,
            span: Span::synthetic(),
        };
        assert_eq!(
            evaluate(&null_expr, &empty_row()).unwrap(),
            Value::Boolean(true)
        );

        let non_null_expr = Expr::IsNull {
            operand: Box::new(lit(Value::Integer(7))),
            negated: false,
            span: Span::synthetic(),
        };
        assert_eq!(
            evaluate(&non_null_expr, &empty_row()).unwrap(),
            Value::Boolean(false)
        );
    }

    #[test]
    fn between_inclusive_bounds() {
        let expr = Expr::Between {
            target: Box::new(lit(Value::Integer(5))),
            low: Box::new(lit(Value::Integer(1))),
            high: Box::new(lit(Value::Integer(10))),
            negated: false,
            span: Span::synthetic(),
        };
        assert_eq!(evaluate(&expr, &empty_row()).unwrap(), Value::Boolean(true));
    }

    #[test]
    fn in_list_match_and_miss() {
        let hit = Expr::InList {
            target: Box::new(lit(Value::Integer(2))),
            values: vec![
                lit(Value::Integer(1)),
                lit(Value::Integer(2)),
                lit(Value::Integer(3)),
            ],
            negated: false,
            span: Span::synthetic(),
        };
        assert_eq!(evaluate(&hit, &empty_row()).unwrap(), Value::Boolean(true));

        let miss = Expr::InList {
            target: Box::new(lit(Value::Integer(99))),
            values: vec![lit(Value::Integer(1)), lit(Value::Integer(2))],
            negated: false,
            span: Span::synthetic(),
        };
        assert_eq!(
            evaluate(&miss, &empty_row()).unwrap(),
            Value::Boolean(false)
        );
    }

    #[test]
    fn column_lookup_walks_field_ref() {
        let row = |field: &FieldRef| -> Option<Value> {
            match field {
                FieldRef::TableColumn { table, column } if table == "t" && column == "x" => {
                    Some(Value::Integer(11))
                }
                _ => None,
            }
        };
        let expr = Expr::Column {
            field: FieldRef::TableColumn {
                table: "t".to_string(),
                column: "x".to_string(),
            },
            span: Span::synthetic(),
        };
        assert_eq!(evaluate(&expr, &row).unwrap(), Value::Integer(11));
    }

    #[test]
    fn missing_column_surfaces_unknown_column() {
        let row = |_: &FieldRef| -> Option<Value> { None };
        let expr = Expr::Column {
            field: FieldRef::TableColumn {
                table: "t".to_string(),
                column: "missing".to_string(),
            },
            span: Span::synthetic(),
        };
        let err = evaluate(&expr, &row).unwrap_err();
        match err {
            EvalError::UnknownColumn(_) => {}
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }

    #[test]
    fn parameter_without_bind_is_eval_error() {
        let expr = Expr::Parameter {
            index: 1,
            span: Span::synthetic(),
        };
        let err = evaluate(&expr, &empty_row()).unwrap_err();
        assert_eq!(err, EvalError::UnboundParameter(1));
    }

    #[test]
    fn cast_integer_to_text_uses_explicit_path() {
        let expr = Expr::Cast {
            inner: Box::new(lit(Value::Integer(42))),
            target: DataType::Text,
            span: Span::synthetic(),
        };
        assert_eq!(
            evaluate(&expr, &empty_row()).unwrap(),
            Value::Text(Arc::from("42"))
        );
    }

    #[test]
    fn concat_returns_text() {
        let expr = binop(
            BinOp::Concat,
            lit(Value::Text(Arc::from("foo"))),
            lit(Value::Text(Arc::from("bar"))),
        );
        assert_eq!(
            evaluate(&expr, &empty_row()).unwrap(),
            Value::Text(Arc::from("foobar"))
        );
    }
}
