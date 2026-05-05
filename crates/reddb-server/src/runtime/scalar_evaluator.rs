//! Scalar evaluator — single owner of typed-expression evaluation.
//!
//! Today, scalar evaluation (SELECT projection RHS + WHERE evaluation)
//! is scattered across:
//!
//! - `runtime/expr_eval.rs` — untyped `Expr` walker that re-resolves
//!   the operator / cast / function it should apply on every row.
//! - `runtime/query_exec/filter_compiled.rs` — opcode interpreter for
//!   `Filter` (the legacy AST shape, no `Expr`).
//! - `storage/query/executors/` — per-backend ad-hoc evaluators that
//!   diverge subtly from each other.
//!
//! `ScalarEvaluator` consolidates the contract:
//!
//! 1. **`compile(&Expr, &dyn Scope) -> CompiledScalar`** runs the
//!    catalog lookups (`schema::cast_catalog`, `schema::operator_catalog`,
//!    `schema::function_catalog`) **once**, and produces a
//!    `CompiledScalar` IR where every operator / cast / function call
//!    carries the resolved catalog entry.
//! 2. **`eval(&CompiledScalar, &dyn RowView) -> Option<Value>`** is
//!    pure dispatch — it never re-enters the catalog. Returning
//!    `None` means "unresolvable / NULL" with the same SQL three-
//!    valued-logic semantics the legacy walker has today.
//!
//! # Where this is wired today
//!
//! `evaluate_runtime_filter_with_db` (the WHERE-clause path on the
//! SELECT planner) routes the `Filter::CompareExpr` arm through the
//! evaluator. The legacy `Expr` walker stays as the fallback for
//! shapes the compile step doesn't yet cover (parameters, KV / CONFIG
//! lookups, ML scalars, …) so semantics are preserved bit-for-bit.
//!
//! # Why a fresh IR instead of `expr_typing::TypedExpr`
//!
//! `expr_typing::TypedExpr` records the *resolved type* of every
//! node, which is what the planner / cost model needs. The runtime
//! needs the *resolved catalog entry* (operator overload, cast
//! entry, function entry) plus a runtime-friendly column reference.
//! Copying types onto `TypedExpr` doesn't carry that information
//! cheaply, so we add a dedicated `CompiledScalar` IR. The two are
//! sibling outputs of the same compile step; a future commit can
//! fold them together if the duplication becomes painful.

use crate::storage::query::ast::{BinOp, CompareOp, Expr, FieldRef, UnaryOp};
use crate::storage::query::unified::UnifiedRecord;
use crate::storage::schema::cast_catalog::{find_cast, CastContext, CastEntry};
use crate::storage::schema::coercion_spine;
use crate::storage::schema::function_catalog::{self, FunctionEntry};
use crate::storage::schema::operator_catalog::{self, OperatorEntry, OperatorKind};
use crate::storage::schema::types::DataType;
use crate::storage::schema::Value;

use super::join_filter::{compare_runtime_values, resolve_runtime_field};

/// Errors reported by the compile step. The runtime hot path never
/// produces these — by the time we're calling `eval` the IR is fully
/// validated.
#[derive(Debug, Clone, PartialEq)]
pub enum CompileError {
    /// Column reference doesn't resolve in the active scope.
    UnknownColumn { table: String, column: String },
    /// Unary operator has no overload accepting the operand type.
    UnaryUnresolved { op: UnaryOp, operand: DataType },
    /// Binary operator has no overload accepting the operand types.
    BinaryUnresolved {
        op: BinOp,
        lhs: DataType,
        rhs: DataType,
    },
    /// Explicit cast is not legal even in `Explicit` context.
    CastUnresolved { src: DataType, target: DataType },
    /// Function name + signature has no entry in the catalog. We
    /// downgrade this to a warning at compile time because the legacy
    /// runtime dispatch table covers a wider surface than the static
    /// catalog (e.g. `LOWER(text)` resolves but extension functions
    /// don't yet have catalog rows). The compile result records the
    /// missing entry so the eval path can still call into the legacy
    /// dispatcher without a catalog reference.
    FunctionUnresolved { name: String, args: Vec<DataType> },
    /// Shape we don't compile yet (parameter, subquery, …). Caller
    /// falls back to the legacy walker for these.
    Unsupported(&'static str),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::UnknownColumn { table, column } => {
                if table.is_empty() {
                    write!(f, "scalar compile: unknown column `{column}`")
                } else {
                    write!(f, "scalar compile: unknown column `{table}.{column}`")
                }
            }
            CompileError::UnaryUnresolved { op, operand } => {
                write!(f, "scalar compile: unary `{op:?}` has no overload for `{operand:?}`")
            }
            CompileError::BinaryUnresolved { op, lhs, rhs } => {
                write!(
                    f,
                    "scalar compile: binary `{op:?}` has no overload for `{lhs:?}` / `{rhs:?}`"
                )
            }
            CompileError::CastUnresolved { src, target } => {
                write!(f, "scalar compile: no cast from `{src:?}` to `{target:?}`")
            }
            CompileError::FunctionUnresolved { name, args } => {
                write!(f, "scalar compile: function `{name}` has no catalog entry for `{args:?}`")
            }
            CompileError::Unsupported(reason) => {
                write!(f, "scalar compile: unsupported shape ({reason})")
            }
        }
    }
}

impl std::error::Error for CompileError {}

/// Resolved scalar IR produced by `compile`. Every node carries the
/// information `eval` needs to dispatch without re-entering the
/// catalogs.
#[derive(Debug, Clone)]
pub enum CompiledScalar {
    /// A literal value cloned from the parsed AST.
    Literal(Value),
    /// Column read against the row view. The compile step already
    /// flattened `FieldRef` to a path string so the row lookup is a
    /// single map probe — no per-row qualification re-walk.
    Column {
        field: FieldRef,
        ty: DataType,
    },
    /// Pre-resolved unary operator. `op_entry.return_type` is the
    /// node's static type.
    Unary {
        op: UnaryOp,
        op_entry: &'static OperatorEntry,
        operand: Box<CompiledScalar>,
        ty: DataType,
    },
    /// Pre-resolved binary operator. The compile step decided which
    /// catalog overload applies; the eval path executes it directly.
    Binary {
        op: BinOp,
        /// `Some` when the catalog has a matching overload; `None`
        /// when none of the static rows match (e.g. `int + bigint` —
        /// the catalog only lists same-category overloads). The eval
        /// path falls back to the legacy numeric coercion in that
        /// case so user-visible semantics don't change.
        op_entry: Option<&'static OperatorEntry>,
        lhs: Box<CompiledScalar>,
        rhs: Box<CompiledScalar>,
        ty: DataType,
    },
    /// Pre-resolved CAST. `entry` is the catalog row that authorised
    /// the conversion. `ty` mirrors `entry.target` for ergonomics.
    Cast {
        inner: Box<CompiledScalar>,
        entry: CastEntry,
        ty: DataType,
    },
    /// Pre-resolved function call. `entry` is `Some` when the static
    /// catalog matched the call site; `None` falls back to the
    /// legacy runtime dispatcher (which covers a few extension
    /// functions not yet in the catalog).
    Call {
        name: String,
        entry: Option<&'static FunctionEntry>,
        args: Vec<CompiledScalar>,
        ty: DataType,
    },
}

impl CompiledScalar {
    /// Static type of this node. Used by the planner / cost model
    /// after compile; the runtime hot path doesn't need it.
    pub fn data_type(&self) -> DataType {
        match self {
            CompiledScalar::Literal(v) => literal_type(v),
            CompiledScalar::Column { ty, .. } => *ty,
            CompiledScalar::Unary { ty, .. } => *ty,
            CompiledScalar::Binary { ty, .. } => *ty,
            CompiledScalar::Cast { ty, .. } => *ty,
            CompiledScalar::Call { ty, .. } => *ty,
        }
    }
}

/// Column scope. Maps `(table, column)` to a `DataType`. The compile
/// step uses this to validate column references and to score
/// operator / function overloads. Callers wire this to the schema
/// registry in production and to a static map in tests.
pub trait Scope {
    fn lookup(&self, table: &str, column: &str) -> Option<DataType>;
}

impl<F> Scope for F
where
    F: Fn(&str, &str) -> Option<DataType>,
{
    fn lookup(&self, table: &str, column: &str) -> Option<DataType> {
        self(table, column)
    }
}

/// Row source. Plain `UnifiedRecord` is the production view; tests
/// can implement this directly to avoid building a record.
pub trait RowView {
    fn read(
        &self,
        field: &FieldRef,
        table_name: Option<&str>,
        table_alias: Option<&str>,
    ) -> Option<Value>;
}

impl RowView for UnifiedRecord {
    fn read(
        &self,
        field: &FieldRef,
        table_name: Option<&str>,
        table_alias: Option<&str>,
    ) -> Option<Value> {
        resolve_runtime_field(self, field, table_name, table_alias)
    }
}

/// The scalar evaluator interface. `compile` resolves catalogs
/// once per query plan; `eval` is the hot per-row path that only
/// dispatches on the resolved IR.
pub trait ScalarEvaluator {
    fn compile(&self, expr: &Expr, scope: &dyn Scope) -> Result<CompiledScalar, CompileError>;
    fn eval(
        &self,
        expr: &CompiledScalar,
        row: &dyn RowView,
        table_name: Option<&str>,
        table_alias: Option<&str>,
    ) -> Option<Value>;
}

/// Default evaluator used by the runtime. Stateless; callers can
/// share a single instance across queries.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultScalarEvaluator;

impl ScalarEvaluator for DefaultScalarEvaluator {
    fn compile(&self, expr: &Expr, scope: &dyn Scope) -> Result<CompiledScalar, CompileError> {
        compile_expr(expr, scope)
    }

    fn eval(
        &self,
        expr: &CompiledScalar,
        row: &dyn RowView,
        table_name: Option<&str>,
        table_alias: Option<&str>,
    ) -> Option<Value> {
        eval_compiled(expr, row, table_name, table_alias)
    }
}

// ---------------------------------------------------------------------------
// compile() — resolve catalogs once
// ---------------------------------------------------------------------------

fn compile_expr(expr: &Expr, scope: &dyn Scope) -> Result<CompiledScalar, CompileError> {
    match expr {
        Expr::Literal { value, .. } => Ok(CompiledScalar::Literal(value.clone())),

        Expr::Column { field, .. } => {
            let (table, column) = field_qualifier(field);
            let ty = scope
                .lookup(table, column)
                .ok_or_else(|| CompileError::UnknownColumn {
                    table: table.to_string(),
                    column: column.to_string(),
                })?;
            Ok(CompiledScalar::Column {
                field: field.clone(),
                ty,
            })
        }

        Expr::UnaryOp { op, operand, .. } => {
            let inner = compile_expr(operand, scope)?;
            let operand_ty = inner.data_type();
            let symbol = unary_op_symbol(*op);
            // Resolve once. Catalog only carries Numeric / Boolean
            // unaries today; if no overload matches we report a hard
            // error so the caller can fall back instead of silently
            // running a stale legacy path.
            let entry = operator_catalog::resolve(
                symbol,
                OperatorKind::Prefix,
                DataType::Nullable,
                operand_ty,
            )
            .ok_or(CompileError::UnaryUnresolved {
                op: *op,
                operand: operand_ty,
            })?;
            Ok(CompiledScalar::Unary {
                op: *op,
                op_entry: entry,
                operand: Box::new(inner),
                ty: entry.return_type,
            })
        }

        Expr::BinaryOp { op, lhs, rhs, .. } => {
            let l = compile_expr(lhs, scope)?;
            let r = compile_expr(rhs, scope)?;
            let lty = l.data_type();
            let rty = r.data_type();
            // Route the binop overload pick through the coercion
            // spine (issue #82): a single Module owns "given (op,
            // lhs, rhs), which overload applies and what implicit
            // casts must we insert". The spine first tries an exact
            // match (preserving legacy behaviour for queries the
            // catalog already covers), then falls back to a
            // coercion-aware widening pick. We discard the
            // OperandCoercions slot today because the runtime eval
            // path still uses dynamic numeric coercion in `arith()`
            // — a future commit can synthesize explicit Cast nodes
            // here once every consumer respects them.
            let entry = coercion_spine::resolve_binop(*op, lty, rty).map(|(e, _)| e);
            // For comparisons / arith we still want a static return
            // type even when the spine has no overload — the legacy
            // numeric coercion path handles cross-type arithmetic
            // that neither catalog enumerates. We use the operator
            // family to assign a default return type.
            let ty = match entry {
                Some(e) => e.return_type,
                None => default_binop_result_type(*op, lty, rty),
            };
            Ok(CompiledScalar::Binary {
                op: *op,
                op_entry: entry,
                lhs: Box::new(l),
                rhs: Box::new(r),
                ty,
            })
        }

        Expr::Cast { inner, target, .. } => {
            let inner_compiled = compile_expr(inner, scope)?;
            let src = inner_compiled.data_type();
            // CAST(...) is user-written → Explicit context is the
            // widest. find_cast returns the matching catalog row.
            let entry = find_cast(src, *target, CastContext::Explicit).ok_or(
                CompileError::CastUnresolved {
                    src,
                    target: *target,
                },
            )?;
            Ok(CompiledScalar::Cast {
                inner: Box::new(inner_compiled),
                entry,
                ty: *target,
            })
        }

        Expr::FunctionCall { name, args, .. } => {
            let mut compiled_args = Vec::with_capacity(args.len());
            for a in args {
                compiled_args.push(compile_expr(a, scope)?);
            }
            let arg_types: Vec<DataType> =
                compiled_args.iter().map(|c| c.data_type()).collect();
            // Functions resolve through the static catalog. When
            // there's no match we keep `entry = None` and let the
            // eval path fall back to the legacy dispatcher — that
            // covers the long tail of extension functions not yet
            // in the catalog. Callers that need strict compile-time
            // resolution can inspect the resulting node and emit
            // `FunctionUnresolved` themselves.
            let upper = name.to_ascii_uppercase();
            let entry = function_catalog::resolve(&upper, &arg_types);
            let ty = entry
                .map(|e| e.return_type)
                .unwrap_or(DataType::Nullable);
            Ok(CompiledScalar::Call {
                name: upper,
                entry,
                args: compiled_args,
                ty,
            })
        }

        // Shapes we don't compile yet — caller falls back to the
        // legacy walker.
        Expr::Parameter { .. } => Err(CompileError::Unsupported("parameter")),
        Expr::Case { .. } => Err(CompileError::Unsupported("CASE")),
        Expr::IsNull { .. } => Err(CompileError::Unsupported("IS NULL")),
        Expr::InList { .. } => Err(CompileError::Unsupported("IN list")),
        Expr::Between { .. } => Err(CompileError::Unsupported("BETWEEN")),
    }
}

fn field_qualifier(field: &FieldRef) -> (&str, &str) {
    match field {
        FieldRef::TableColumn { table, column } => (table.as_str(), column.as_str()),
        FieldRef::NodeProperty { alias, property } => (alias.as_str(), property.as_str()),
        FieldRef::EdgeProperty { alias, property } => (alias.as_str(), property.as_str()),
        FieldRef::NodeId { .. } => ("", ""),
    }
}

fn unary_op_symbol(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Not => "NOT",
    }
}

fn binop_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Concat => "||",
        BinOp::Eq => "=",
        BinOp::Ne => "<>",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "AND",
        BinOp::Or => "OR",
    }
}

/// Result type when the catalog has no matching overload but the
/// operator family is well-known. Mirrors the rules in
/// `expr_typing::binop_result_type` for the comparisons / logicals;
/// arith falls back to Float so the runtime numeric coercion path
/// preserves the legacy behaviour (mixed-precision arithmetic
/// promotes to Float).
fn default_binop_result_type(op: BinOp, lhs: DataType, rhs: DataType) -> DataType {
    use BinOp::*;
    match op {
        And | Or => DataType::Boolean,
        Eq | Ne | Lt | Le | Gt | Ge => DataType::Boolean,
        Concat => DataType::Text,
        Add | Sub | Mul | Div | Mod => {
            if lhs == DataType::Float || rhs == DataType::Float || matches!(op, Div) {
                DataType::Float
            } else if lhs == DataType::Decimal || rhs == DataType::Decimal {
                DataType::Decimal
            } else if lhs == DataType::BigInt || rhs == DataType::BigInt {
                DataType::BigInt
            } else {
                DataType::Integer
            }
        }
    }
}

fn literal_type(v: &Value) -> DataType {
    match v {
        Value::Null => DataType::Nullable,
        Value::Boolean(_) => DataType::Boolean,
        Value::Integer(_) => DataType::Integer,
        Value::UnsignedInteger(_) => DataType::UnsignedInteger,
        Value::Float(_) => DataType::Float,
        Value::BigInt(_) => DataType::BigInt,
        Value::Decimal(_) => DataType::Decimal,
        Value::Text(_) => DataType::Text,
        Value::Blob(_) => DataType::Blob,
        Value::Timestamp(_) => DataType::Timestamp,
        Value::TimestampMs(_) => DataType::TimestampMs,
        Value::Duration(_) => DataType::Duration,
        Value::Date(_) => DataType::Date,
        Value::Time(_) => DataType::Time,
        // The remaining domain types are rare in WHERE expressions
        // resolved via the evaluator; fall back to Nullable so the
        // catalog scoring remains permissive.
        _ => DataType::Nullable,
    }
}

// ---------------------------------------------------------------------------
// eval() — pure dispatch over CompiledScalar
// ---------------------------------------------------------------------------

fn eval_compiled(
    expr: &CompiledScalar,
    row: &dyn RowView,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Option<Value> {
    match expr {
        CompiledScalar::Literal(v) => Some(v.clone()),

        CompiledScalar::Column { field, .. } => row.read(field, table_name, table_alias),

        CompiledScalar::Unary { op, operand, .. } => {
            let v = eval_compiled(operand, row, table_name, table_alias)?;
            match op {
                UnaryOp::Neg => negate_value(&v),
                UnaryOp::Not => match v {
                    Value::Boolean(b) => Some(Value::Boolean(!b)),
                    _ => None,
                },
            }
        }

        CompiledScalar::Binary { op, lhs, rhs, .. } => {
            // AND / OR short-circuit on truthy LHS to skip expensive
            // RHS subtrees.
            match op {
                BinOp::And => {
                    let l = eval_compiled(lhs, row, table_name, table_alias)?;
                    if let Value::Boolean(false) = l {
                        return Some(Value::Boolean(false));
                    }
                    let r = eval_compiled(rhs, row, table_name, table_alias)?;
                    match (l, r) {
                        (Value::Boolean(a), Value::Boolean(b)) => Some(Value::Boolean(a && b)),
                        _ => None,
                    }
                }
                BinOp::Or => {
                    let l = eval_compiled(lhs, row, table_name, table_alias)?;
                    if let Value::Boolean(true) = l {
                        return Some(Value::Boolean(true));
                    }
                    let r = eval_compiled(rhs, row, table_name, table_alias)?;
                    match (l, r) {
                        (Value::Boolean(a), Value::Boolean(b)) => Some(Value::Boolean(a || b)),
                        _ => None,
                    }
                }
                _ => {
                    let l = eval_compiled(lhs, row, table_name, table_alias)?;
                    let r = eval_compiled(rhs, row, table_name, table_alias)?;
                    apply_binop(*op, l, r)
                }
            }
        }

        CompiledScalar::Cast { inner, entry, .. } => {
            let v = eval_compiled(inner, row, table_name, table_alias)?;
            Some(apply_cast(&v, entry.target))
        }

        CompiledScalar::Call { name, args, .. } => {
            let mut arg_values = Vec::with_capacity(args.len());
            for a in args {
                arg_values.push(
                    eval_compiled(a, row, table_name, table_alias).unwrap_or(Value::Null),
                );
            }
            // Route through the legacy builtin dispatcher. The
            // catalog `entry` is informational on the hot path —
            // it told the compile step what the static return type
            // is, but the actual implementation lives in the
            // dispatcher table. Future commits move the dispatch
            // table itself behind the catalog so this fallback
            // disappears entirely.
            super::expr_eval::scalar_dispatch_builtin(name, &arg_values)
        }
    }
}

fn negate_value(v: &Value) -> Option<Value> {
    match v {
        Value::Integer(n) => Some(Value::Integer(-n)),
        Value::BigInt(n) => Some(Value::BigInt(-n)),
        Value::Float(f) => Some(Value::Float(-f)),
        _ => None,
    }
}

fn apply_binop(op: BinOp, a: Value, b: Value) -> Option<Value> {
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => arith(op, a, b),
        BinOp::Concat => Some(Value::text(format!(
            "{}{}",
            a.display_string(),
            b.display_string()
        ))),
        BinOp::Eq => Some(Value::Boolean(compare_runtime_values(&a, &b, CompareOp::Eq))),
        BinOp::Ne => Some(Value::Boolean(compare_runtime_values(&a, &b, CompareOp::Ne))),
        BinOp::Lt => Some(Value::Boolean(compare_runtime_values(&a, &b, CompareOp::Lt))),
        BinOp::Le => Some(Value::Boolean(compare_runtime_values(&a, &b, CompareOp::Le))),
        BinOp::Gt => Some(Value::Boolean(compare_runtime_values(&a, &b, CompareOp::Gt))),
        BinOp::Ge => Some(Value::Boolean(compare_runtime_values(&a, &b, CompareOp::Ge))),
        BinOp::And | BinOp::Or => None, // short-circuited above
    }
}

fn arith(op: BinOp, a: Value, b: Value) -> Option<Value> {
    let (la, l_is_float) = value_as_number(&a)?;
    let (lb, r_is_float) = value_as_number(&b)?;
    let force_float = matches!(op, BinOp::Div) || l_is_float || r_is_float;
    let out = match op {
        BinOp::Add => la + lb,
        BinOp::Sub => la - lb,
        BinOp::Mul => la * lb,
        BinOp::Div => {
            if lb == 0.0 {
                return None;
            }
            la / lb
        }
        BinOp::Mod => {
            if lb == 0.0 {
                return None;
            }
            la % lb
        }
        _ => return None,
    };
    Some(if force_float {
        Value::Float(out)
    } else {
        Value::Integer(out as i64)
    })
}

fn value_as_number(v: &Value) -> Option<(f64, bool)> {
    match v {
        Value::Integer(n) | Value::BigInt(n) => Some((*n as f64, false)),
        Value::UnsignedInteger(n) => Some((*n as f64, false)),
        Value::Float(f) => Some((*f, true)),
        Value::Decimal(d) => Some((*d as f64 / 10_000.0, true)),
        Value::Text(s) => s
            .parse::<i64>()
            .map(|n| (n as f64, false))
            .or_else(|_| s.parse::<f64>().map(|f| (f, true)))
            .ok(),
        _ => None,
    }
}

fn apply_cast(src: &Value, target: DataType) -> Value {
    use DataType as DT;
    match (src, target) {
        (v, DT::Text) => Value::text(v.display_string()),
        (Value::Integer(n), DT::Float) => Value::Float(*n as f64),
        (Value::Integer(n), DT::BigInt) => Value::BigInt(*n),
        (Value::Integer(n), DT::UnsignedInteger) if *n >= 0 => Value::UnsignedInteger(*n as u64),
        (Value::UnsignedInteger(n), DT::Integer) if *n <= i64::MAX as u64 => {
            Value::Integer(*n as i64)
        }
        (Value::UnsignedInteger(n), DT::Float) => Value::Float(*n as f64),
        (Value::Float(f), DT::Integer) => Value::Integer(*f as i64),
        (Value::Float(f), DT::UnsignedInteger) if *f >= 0.0 => Value::UnsignedInteger(*f as u64),
        (Value::Boolean(b), DT::Integer) => Value::Integer(if *b { 1 } else { 0 }),
        (Value::Integer(n), DT::Boolean) => Value::Boolean(*n != 0),
        (Value::Text(s), t) => crate::storage::schema::coerce::coerce(s, t, None)
            .unwrap_or(Value::Null),
        (v, t) => crate::storage::schema::coerce::coerce(&v.display_string(), t, None)
            .unwrap_or(Value::Null),
    }
}

// ---------------------------------------------------------------------------
// Filter integration — compile `Filter::CompareExpr` arms once per query.
// ---------------------------------------------------------------------------

use crate::storage::query::ast::Filter;

/// Compiled WHERE filter: a Filter tree where every `CompareExpr`
/// arm has been replaced with pre-resolved scalar IR. Other arms
/// keep their original `Filter` shape so the existing runtime
/// dispatcher continues to handle them — the deletion test still
/// holds because removing this wrapper forces the per-row evaluator
/// back to walking raw `Expr` trees and re-resolving operator /
/// cast / function entries on every row.
#[derive(Debug, Clone)]
pub enum CompiledFilter {
    /// Original filter shape (legacy path handles it).
    Legacy(Filter),
    /// `lhs op rhs` with both sides pre-compiled.
    CompareExpr {
        lhs: CompiledScalar,
        op: CompareOp,
        rhs: CompiledScalar,
    },
    And(Box<CompiledFilter>, Box<CompiledFilter>),
    Or(Box<CompiledFilter>, Box<CompiledFilter>),
    Not(Box<CompiledFilter>),
}

/// Compile a `Filter` tree, pre-resolving every `CompareExpr` arm
/// through `ScalarEvaluator::compile`. Other arms are wrapped in
/// `Legacy` so the per-row evaluator falls back to the existing
/// walker for shapes the scalar evaluator doesn't yet cover
/// (`Filter::Compare`, `Filter::Like`, `Filter::Between`, etc).
///
/// `scope` resolves column types — when it can't, this returns the
/// branch as `Legacy` so the existing per-row walker can handle the
/// case (preserves SELECT semantics during the migration). The
/// catalog lookups happen here, exactly once per query plan.
pub fn compile_filter(filter: &Filter, scope: &dyn Scope) -> CompiledFilter {
    match filter {
        Filter::CompareExpr { lhs, op, rhs } => {
            let evaluator = DefaultScalarEvaluator;
            match (
                evaluator.compile(lhs, scope),
                evaluator.compile(rhs, scope),
            ) {
                (Ok(l), Ok(r)) => CompiledFilter::CompareExpr {
                    lhs: l,
                    op: *op,
                    rhs: r,
                },
                _ => CompiledFilter::Legacy(filter.clone()),
            }
        }
        Filter::And(a, b) => CompiledFilter::And(
            Box::new(compile_filter(a, scope)),
            Box::new(compile_filter(b, scope)),
        ),
        Filter::Or(a, b) => CompiledFilter::Or(
            Box::new(compile_filter(a, scope)),
            Box::new(compile_filter(b, scope)),
        ),
        Filter::Not(inner) => CompiledFilter::Not(Box::new(compile_filter(inner, scope))),
        // Every other Filter variant stays on the legacy walker for
        // now. The migration moves them over one at a time so
        // semantics stay identical at each step.
        _ => CompiledFilter::Legacy(filter.clone()),
    }
}

/// Evaluate a `CompiledFilter` against a row. Pure dispatch — every
/// catalog lookup already happened at compile time. The `Legacy`
/// arm hands back to the existing walker, which still owns the
/// uncompiled Filter shapes (`Compare`, `Like`, `IN`, ...).
pub fn evaluate_compiled_filter(
    db: Option<&crate::storage::RedDB>,
    compiled: &CompiledFilter,
    record: &UnifiedRecord,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> bool {
    match compiled {
        CompiledFilter::Legacy(filter) => super::join_filter::evaluate_runtime_filter_with_db(
            db,
            record,
            filter,
            table_name,
            table_alias,
        ),
        CompiledFilter::CompareExpr { lhs, op, rhs } => {
            // Pure dispatch — eval both sides and compare. The compile
            // step already picked the operator overload and resolved
            // every nested function / cast.
            let l = eval_compiled(lhs, record, table_name, table_alias);
            let r = eval_compiled(rhs, record, table_name, table_alias);
            match (l, r) {
                (Some(lv), Some(rv)) => compare_runtime_values(&lv, &rv, *op),
                _ => false,
            }
        }
        CompiledFilter::And(a, b) => {
            evaluate_compiled_filter(db, a, record, table_name, table_alias)
                && evaluate_compiled_filter(db, b, record, table_name, table_alias)
        }
        CompiledFilter::Or(a, b) => {
            evaluate_compiled_filter(db, a, record, table_name, table_alias)
                || evaluate_compiled_filter(db, b, record, table_name, table_alias)
        }
        CompiledFilter::Not(inner) => {
            !evaluate_compiled_filter(db, inner, record, table_name, table_alias)
        }
    }
}

/// Permissive scope used when the caller doesn't have a schema
/// registry handy. Every column resolves to `DataType::Nullable` —
/// the catalog scoring still picks an overload via the
/// preferred-type tie break. Tests and in-process call sites that
/// just need *any* legal type can use this.
pub struct PermissiveScope;

impl Scope for PermissiveScope {
    fn lookup(&self, _table: &str, _column: &str) -> Option<DataType> {
        Some(DataType::Nullable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::ast::{BinOp, FieldRef, Span};

    /// Build a `CompareExpr`-style WHERE predicate `lhs op rhs` and
    /// evaluate it against the given record. Returns `Some(true)` /
    /// `Some(false)` for booleans, mirroring the runtime contract.
    fn compile_and_eval(
        expr: &Expr,
        scope: &dyn Scope,
        record: &UnifiedRecord,
    ) -> Option<Value> {
        let evaluator = DefaultScalarEvaluator;
        let compiled = evaluator.compile(expr, scope).expect("compile must succeed");
        evaluator.eval(&compiled, record, None, None)
    }

    fn col(name: &str) -> Expr {
        Expr::col(FieldRef::column("", name))
    }

    fn lit(v: Value) -> Expr {
        Expr::lit(v)
    }

    fn typed_scope<'a>(types: &'a [(&'static str, DataType)]) -> impl Scope + 'a {
        let map: Vec<(String, DataType)> =
            types.iter().map(|(n, t)| ((*n).to_string(), *t)).collect();
        move |_table: &str, column: &str| {
            map.iter()
                .find(|(n, _)| n == column)
                .map(|(_, t)| *t)
        }
    }

    /// `a = 1` — the smallest WHERE predicate. Confirms compile
    /// resolves the `=` operator overload once and eval dispatches
    /// it with no further catalog calls.
    #[test]
    fn compile_eval_eq_int_literal() {
        let scope = typed_scope(&[("a", DataType::Integer)]);
        let mut record = UnifiedRecord::new();
        record.set("a", Value::Integer(1));

        let expr = Expr::binop(BinOp::Eq, col("a"), lit(Value::Integer(1)));

        let evaluator = DefaultScalarEvaluator;
        let compiled = evaluator.compile(&expr, &scope).unwrap();

        // Spot-check: the compile step resolved the `=` overload.
        match &compiled {
            CompiledScalar::Binary { op_entry, ty, .. } => {
                assert_eq!(*ty, DataType::Boolean);
                let entry = op_entry.expect("`=` int overload must resolve");
                assert_eq!(entry.name, "=");
                assert_eq!(entry.return_type, DataType::Boolean);
            }
            other => panic!("expected Binary, got {other:?}"),
        }

        // eval matches.
        assert_eq!(
            evaluator.eval(&compiled, &record, None, None),
            Some(Value::Boolean(true))
        );

        // Non-matching row → false.
        let mut other = UnifiedRecord::new();
        other.set("a", Value::Integer(2));
        assert_eq!(
            evaluator.eval(&compiled, &other, None, None),
            Some(Value::Boolean(false))
        );
    }

    /// `a + b > 10` — exercises arithmetic + comparison composition.
    /// Confirms the binary `+` resolves and the `>` resolves on the
    /// promoted result type.
    #[test]
    fn compile_eval_arith_then_compare() {
        let scope = typed_scope(&[
            ("a", DataType::Integer),
            ("b", DataType::Integer),
        ]);
        let mut record = UnifiedRecord::new();
        record.set("a", Value::Integer(7));
        record.set("b", Value::Integer(5));

        // `a + b > 10`
        let sum = Expr::binop(BinOp::Add, col("a"), col("b"));
        let expr = Expr::binop(BinOp::Gt, sum, lit(Value::Integer(10)));

        let v = compile_and_eval(&expr, &scope, &record);
        assert_eq!(v, Some(Value::Boolean(true)));

        // Flip the row so the predicate becomes false.
        let mut record2 = UnifiedRecord::new();
        record2.set("a", Value::Integer(2));
        record2.set("b", Value::Integer(3));
        let v2 = compile_and_eval(&expr, &scope, &record2);
        assert_eq!(v2, Some(Value::Boolean(false)));
    }

    /// `LOWER(s) = 'x'` — confirms the function path resolves through
    /// the catalog at compile time and the eval dispatches the
    /// builtin once per row without re-resolving.
    #[test]
    fn compile_eval_function_call_lower_eq_literal() {
        let scope = typed_scope(&[("s", DataType::Text)]);
        let mut record = UnifiedRecord::new();
        record.set("s", Value::text("X"));

        // LOWER(s) = 'x'
        let lower_call = Expr::FunctionCall {
            name: "LOWER".to_string(),
            args: vec![col("s")],
            span: Span::synthetic(),
        };
        let expr = Expr::binop(BinOp::Eq, lower_call, lit(Value::text("x")));

        let evaluator = DefaultScalarEvaluator;
        let compiled = evaluator.compile(&expr, &scope).unwrap();

        // Spot-check: LOWER resolved against the function catalog
        // during compile.
        if let CompiledScalar::Binary { lhs, .. } = &compiled {
            if let CompiledScalar::Call { entry, name, .. } = lhs.as_ref() {
                assert_eq!(name, "LOWER");
                assert!(
                    entry.is_some(),
                    "LOWER(text) must resolve in function catalog"
                );
            } else {
                panic!("expected Call on lhs");
            }
        } else {
            panic!("expected Binary at root");
        }

        assert_eq!(
            evaluator.eval(&compiled, &record, None, None),
            Some(Value::Boolean(true))
        );
    }

    /// Filter integration: `compile_filter` resolves `CompareExpr`
    /// arms once; `evaluate_compiled_filter` dispatches without
    /// re-resolving. This is the pathway wired into the SELECT
    /// "filter" stage at `runtime/query_exec/table.rs`.
    #[test]
    fn compile_filter_compares_expr_branch_runs_through_evaluator() {
        let scope = typed_scope(&[
            ("a", DataType::Integer),
            ("b", DataType::Integer),
        ]);

        // WHERE a + b > 10
        let filter = Filter::CompareExpr {
            lhs: Expr::binop(BinOp::Add, col("a"), col("b")),
            op: CompareOp::Gt,
            rhs: lit(Value::Integer(10)),
        };

        let compiled = compile_filter(&filter, &scope);
        // Spot-check: the CompareExpr arm was compiled (NOT Legacy).
        assert!(
            matches!(compiled, CompiledFilter::CompareExpr { .. }),
            "CompareExpr should compile through ScalarEvaluator"
        );

        let mut hit = UnifiedRecord::new();
        hit.set("a", Value::Integer(8));
        hit.set("b", Value::Integer(5));
        assert!(
            evaluate_compiled_filter(None, &compiled, &hit, None, None),
            "8 + 5 > 10 must match"
        );

        let mut miss = UnifiedRecord::new();
        miss.set("a", Value::Integer(2));
        miss.set("b", Value::Integer(3));
        assert!(
            !evaluate_compiled_filter(None, &compiled, &miss, None, None),
            "2 + 3 > 10 must not match"
        );
    }

    /// Filter integration: non-CompareExpr arms stay on the legacy
    /// walker. The interface preserves SELECT semantics by routing
    /// them through `Legacy` rather than failing.
    #[test]
    fn compile_filter_keeps_compare_legacy_arm() {
        let scope = typed_scope(&[("a", DataType::Integer)]);
        let filter = Filter::Compare {
            field: FieldRef::column("", "a"),
            op: CompareOp::Eq,
            value: Value::Integer(1),
        };
        let compiled = compile_filter(&filter, &scope);
        assert!(
            matches!(compiled, CompiledFilter::Legacy(_)),
            "Filter::Compare must stay on the legacy walker"
        );

        let mut record = UnifiedRecord::new();
        record.set("a", Value::Integer(1));
        assert!(evaluate_compiled_filter(None, &compiled, &record, None, None));
    }

    /// Compile error surface: unknown column propagates a structured
    /// error so the caller can decide whether to fall back to the
    /// legacy walker or surface the diagnostic.
    #[test]
    fn compile_unknown_column_errors() {
        let scope = typed_scope(&[("a", DataType::Integer)]);
        let expr = Expr::binop(BinOp::Eq, col("missing"), lit(Value::Integer(1)));
        let evaluator = DefaultScalarEvaluator;
        let err = evaluator.compile(&expr, &scope).unwrap_err();
        match err {
            CompileError::UnknownColumn { column, .. } => {
                assert_eq!(column, "missing");
            }
            other => panic!("expected UnknownColumn, got {other:?}"),
        }
    }
}
