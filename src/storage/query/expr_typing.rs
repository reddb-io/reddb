//! Fase 3 expression typer.
//!
//! Walks an `ast::Expr` tree and assigns a concrete `DataType` to
//! every node, using:
//!
//! - Column scope (table → column → DataType) supplied by the
//!   caller as a closure so we don't depend on the schema registry
//!   directly — keeps this module trivially testable.
//! - The cast catalog (`schema::cast_catalog`) for implicit
//!   coercion paths.
//! - The type-category preferred-member rule for tie breaks (PG
//!   `func_select_candidate` heuristic, simplified).
//!
//! Output is a `TypedExpr` mirroring the shape of `Expr` but with a
//! `ty: DataType` slot on every node. The runtime evaluator can use
//! that to skip the Value→Number tagging dance the current
//! `expr_eval` does at every step.
//!
//! Scope today (Fase 3 starter): handles literals, columns, unary,
//! binary arith / comparison / logical, cast, IsNull, Between,
//! InList, Case, and built-in FunctionCall nodes resolved through the
//! static function catalog. The remaining gap is advanced polymorphic
//! signatures beyond lightweight cases such as `COALESCE`.
//! Subqueries / parameters / advanced polymorphism are out of scope.
//!
//! This module is **not yet wired** into the parser → planner flow.
//! It exists so Fase 3 Week 4+ can plug it in once the parser v2
//! emits Expr trees as the canonical projection / filter
//! representation.

use super::ast::{BinOp, Expr, FieldRef, UnaryOp};
use crate::storage::schema::cast_catalog::{can_implicit_cast, CastContext};
use crate::storage::schema::types::{DataType, TypeCategory, Value};

/// Errors reported by the expression typer. All variants are
/// recoverable diagnostics — the analyzer can collect several and
/// emit them together rather than fail-fast like the parser.
#[derive(Debug, Clone)]
pub enum TypeError {
    /// Column reference doesn't resolve in the active scope.
    UnknownColumn { table: String, column: String },
    /// Operator doesn't accept the given operand types after
    /// implicit coercion.
    OperatorMismatch {
        op: BinOp,
        lhs: DataType,
        rhs: DataType,
    },
    /// Unary operator doesn't accept the operand type.
    UnaryMismatch { op: UnaryOp, operand: DataType },
    /// Explicit CAST target not reachable from source even via
    /// the Explicit context.
    InvalidCast { src: DataType, target: DataType },
    /// CASE branches yield different unifiable types.
    CaseBranchMismatch { first: DataType, other: DataType },
    /// IN list elements don't unify with the target.
    InListMismatch { target: DataType, element: DataType },
}

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownColumn { table, column } => {
                if table.is_empty() {
                    write!(f, "unknown column `{column}`")
                } else {
                    write!(f, "unknown column `{table}.{column}`")
                }
            }
            Self::OperatorMismatch { op, lhs, rhs } => {
                write!(
                    f,
                    "operator `{op:?}` cannot apply to `{lhs:?}` and `{rhs:?}`"
                )
            }
            Self::UnaryMismatch { op, operand } => {
                write!(f, "unary `{op:?}` cannot apply to `{operand:?}`")
            }
            Self::InvalidCast { src, target } => {
                write!(f, "no cast from `{src:?}` to `{target:?}`")
            }
            Self::CaseBranchMismatch { first, other } => {
                write!(
                    f,
                    "CASE branches disagree on type: `{first:?}` vs `{other:?}`"
                )
            }
            Self::InListMismatch { target, element } => {
                write!(
                    f,
                    "IN list element `{element:?}` is incompatible with target `{target:?}`"
                )
            }
        }
    }
}

impl std::error::Error for TypeError {}

/// Resolved type for an expression node. Mirrors `Expr` shape with
/// an added `ty` slot. Span is preserved so analyzer diagnostics can
/// still point at the original token range.
///
/// Stored as `Box<…>` for the recursive variants because the typer
/// runs once per query and the trees are bounded — no need for the
/// arena tricks the runtime evaluator uses.
#[derive(Debug, Clone)]
pub struct TypedExpr {
    pub kind: TypedExprKind,
    pub ty: DataType,
}

#[derive(Debug, Clone)]
pub enum TypedExprKind {
    Literal(Value),
    Column(FieldRef),
    UnaryOp {
        op: UnaryOp,
        operand: Box<TypedExpr>,
    },
    BinaryOp {
        op: BinOp,
        lhs: Box<TypedExpr>,
        rhs: Box<TypedExpr>,
    },
    Cast {
        inner: Box<TypedExpr>,
    },
    FunctionCall {
        name: String,
        args: Vec<TypedExpr>,
    },
    Case {
        branches: Vec<(TypedExpr, TypedExpr)>,
        else_: Option<Box<TypedExpr>>,
    },
    IsNull {
        operand: Box<TypedExpr>,
        negated: bool,
    },
    InList {
        target: Box<TypedExpr>,
        values: Vec<TypedExpr>,
        negated: bool,
    },
    Between {
        target: Box<TypedExpr>,
        low: Box<TypedExpr>,
        high: Box<TypedExpr>,
        negated: bool,
    },
}

/// Closure-based column scope. The analyzer passes a closure that
/// resolves `(table, column)` to a `DataType`, returning `None` if
/// the column doesn't exist in the active scope. Callers wire this
/// to the schema registry (production) or a static map (tests).
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

/// Type a single expression against the given scope.
pub fn type_expr(expr: &Expr, scope: &dyn Scope) -> Result<TypedExpr, TypeError> {
    match expr {
        Expr::Literal { value, .. } => Ok(TypedExpr {
            ty: literal_type(value),
            kind: TypedExprKind::Literal(value.clone()),
        }),
        Expr::Column { field, .. } => {
            let (table, column) = match field {
                FieldRef::TableColumn { table, column } => (table.as_str(), column.as_str()),
                FieldRef::NodeProperty { alias, property } => (alias.as_str(), property.as_str()),
                FieldRef::EdgeProperty { alias, property } => (alias.as_str(), property.as_str()),
                FieldRef::NodeId { .. } => ("", ""),
            };
            let ty = scope
                .lookup(table, column)
                .ok_or(TypeError::UnknownColumn {
                    table: table.to_string(),
                    column: column.to_string(),
                })?;
            Ok(TypedExpr {
                ty,
                kind: TypedExprKind::Column(field.clone()),
            })
        }
        Expr::Parameter { .. } => {
            // Parameters get the catch-all Nullable type until the
            // bind phase substitutes a concrete value. The Fase 4
            // plan-cache parameter work tracks this properly.
            Ok(TypedExpr {
                ty: DataType::Nullable,
                kind: TypedExprKind::Literal(Value::Null),
            })
        }
        Expr::UnaryOp { op, operand, .. } => {
            let inner = type_expr(operand, scope)?;
            let ty = unary_result_type(*op, inner.ty)?;
            Ok(TypedExpr {
                ty,
                kind: TypedExprKind::UnaryOp {
                    op: *op,
                    operand: Box::new(inner),
                },
            })
        }
        Expr::BinaryOp { op, lhs, rhs, .. } => {
            let l = type_expr(lhs, scope)?;
            let r = type_expr(rhs, scope)?;
            let ty = binop_result_type(*op, l.ty, r.ty)?;
            Ok(TypedExpr {
                ty,
                kind: TypedExprKind::BinaryOp {
                    op: *op,
                    lhs: Box::new(l),
                    rhs: Box::new(r),
                },
            })
        }
        Expr::Cast { inner, target, .. } => {
            let inner_typed = type_expr(inner, scope)?;
            // Validate the cast against the catalog using Explicit
            // context — user wrote it, so the widest rule applies.
            if !crate::storage::schema::cast_catalog::can_explicit_cast(inner_typed.ty, *target) {
                return Err(TypeError::InvalidCast {
                    src: inner_typed.ty,
                    target: *target,
                });
            }
            Ok(TypedExpr {
                ty: *target,
                kind: TypedExprKind::Cast {
                    inner: Box::new(inner_typed),
                },
            })
        }
        Expr::FunctionCall { name, args, .. } => {
            let typed_args = args
                .iter()
                .map(|a| type_expr(a, scope))
                .collect::<Result<Vec<_>, _>>()?;
            // Look up the function in the static catalog. Resolution
            // picks the best-matching overload by exact-type score
            // with a preferred-return-type tie break (see
            // schema::function_catalog::resolve). Unknown functions
            // fall through to `Nullable` so the rest of the query
            // still types — matches PG's permissive
            // `function does not exist` warning rather than hard fail.
            let arg_dt: Vec<DataType> = typed_args.iter().map(|t| t.ty).collect();
            let return_ty = resolve_function_return_type(name, &arg_dt);
            Ok(TypedExpr {
                ty: return_ty,
                kind: TypedExprKind::FunctionCall {
                    name: name.clone(),
                    args: typed_args,
                },
            })
        }
        Expr::Case {
            branches, else_, ..
        } => {
            let mut typed_branches = Vec::with_capacity(branches.len());
            let mut result_ty: Option<DataType> = None;
            for (cond, val) in branches {
                let cond_typed = type_expr(cond, scope)?;
                let val_typed = type_expr(val, scope)?;
                let prev_ty = result_ty;
                result_ty = merge_compatible_type(result_ty, val_typed.ty).map_err(|_| {
                    TypeError::CaseBranchMismatch {
                        first: prev_ty.unwrap_or(val_typed.ty),
                        other: val_typed.ty,
                    }
                })?;
                typed_branches.push((cond_typed, val_typed));
            }
            let typed_else = if let Some(else_expr) = else_ {
                let e = type_expr(else_expr, scope)?;
                let prev_ty = result_ty;
                result_ty = merge_compatible_type(result_ty, e.ty).map_err(|_| {
                    TypeError::CaseBranchMismatch {
                        first: prev_ty.unwrap_or(e.ty),
                        other: e.ty,
                    }
                })?;
                Some(Box::new(e))
            } else {
                None
            };
            let ty = result_ty.unwrap_or(DataType::Nullable);
            Ok(TypedExpr {
                ty,
                kind: TypedExprKind::Case {
                    branches: typed_branches,
                    else_: typed_else,
                },
            })
        }
        Expr::IsNull {
            operand, negated, ..
        } => {
            let inner = type_expr(operand, scope)?;
            Ok(TypedExpr {
                ty: DataType::Boolean,
                kind: TypedExprKind::IsNull {
                    operand: Box::new(inner),
                    negated: *negated,
                },
            })
        }
        Expr::InList {
            target,
            values,
            negated,
            ..
        } => {
            let target_typed = type_expr(target, scope)?;
            let mut typed_values = Vec::with_capacity(values.len());
            for v in values {
                let vt = type_expr(v, scope)?;
                if vt.ty != target_typed.ty && !can_implicit_cast(vt.ty, target_typed.ty) {
                    return Err(TypeError::InListMismatch {
                        target: target_typed.ty,
                        element: vt.ty,
                    });
                }
                typed_values.push(vt);
            }
            Ok(TypedExpr {
                ty: DataType::Boolean,
                kind: TypedExprKind::InList {
                    target: Box::new(target_typed),
                    values: typed_values,
                    negated: *negated,
                },
            })
        }
        Expr::Between {
            target,
            low,
            high,
            negated,
            ..
        } => {
            let target_typed = type_expr(target, scope)?;
            let low_typed = type_expr(low, scope)?;
            let high_typed = type_expr(high, scope)?;
            // Both bounds must coerce to the target's type.
            for bound in &[&low_typed, &high_typed] {
                if bound.ty != target_typed.ty && !can_implicit_cast(bound.ty, target_typed.ty) {
                    return Err(TypeError::OperatorMismatch {
                        op: BinOp::Ge,
                        lhs: target_typed.ty,
                        rhs: bound.ty,
                    });
                }
            }
            Ok(TypedExpr {
                ty: DataType::Boolean,
                kind: TypedExprKind::Between {
                    target: Box::new(target_typed),
                    low: Box::new(low_typed),
                    high: Box::new(high_typed),
                    negated: *negated,
                },
            })
        }
    }
}

/// Map a `Value` literal to its concrete `DataType`. Mirrors the
/// existing `Value::data_type()` impl in `schema::types` but is
/// inlined here so the typer doesn't depend on a method that may
/// not yet exist on every Value variant.
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
        Value::IpAddr(_) => DataType::IpAddr,
        Value::Ipv4(_) => DataType::Ipv4,
        Value::Ipv6(_) => DataType::Ipv6,
        Value::Subnet(_, _) => DataType::Subnet,
        Value::Cidr(_, _) => DataType::Cidr,
        Value::MacAddr(_) => DataType::MacAddr,
        Value::Port(_) => DataType::Port,
        Value::Latitude(_) => DataType::Latitude,
        Value::Longitude(_) => DataType::Longitude,
        Value::GeoPoint(_, _) => DataType::GeoPoint,
        Value::Country2(_) => DataType::Country2,
        Value::Country3(_) => DataType::Country3,
        Value::Lang2(_) => DataType::Lang2,
        Value::Lang5(_) => DataType::Lang5,
        Value::Currency(_) => DataType::Currency,
        Value::Color(_) => DataType::Color,
        Value::ColorAlpha(_) => DataType::ColorAlpha,
        Value::Email(_) => DataType::Email,
        Value::Url(_) => DataType::Url,
        Value::Phone(_) => DataType::Phone,
        Value::Semver(_) => DataType::Semver,
        Value::Uuid(_) => DataType::Uuid,
        Value::Vector(_) => DataType::Vector,
        Value::Array(_) => DataType::Array,
        Value::Json(_) => DataType::Json,
        Value::EnumValue(_) => DataType::Enum,
        Value::NodeRef(_) => DataType::NodeRef,
        Value::EdgeRef(_) => DataType::EdgeRef,
        Value::VectorRef(_, _) => DataType::VectorRef,
        Value::RowRef(_, _) => DataType::RowRef,
        Value::KeyRef(_, _) => DataType::KeyRef,
        Value::DocRef(_, _) => DataType::DocRef,
        Value::TableRef(_) => DataType::TableRef,
        Value::PageRef(_) => DataType::PageRef,
        Value::Secret(_) => DataType::Secret,
        Value::Password(_) => DataType::Password,
    }
}

fn resolve_function_return_type(name: &str, arg_types: &[DataType]) -> DataType {
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        // CONCAT stringifies every non-null argument at runtime, so the
        // return type is always text even when the catalog match is
        // intentionally loose.
        "CONCAT" | "CONCAT_WS" | "QUOTE_LITERAL" => DataType::Text,
        // COALESCE is effectively `anycompatible`: ignore NULL/unknown
        // args, then widen left-to-right using the implicit-cast graph.
        "COALESCE" => resolve_coalesce_return_type(arg_types),
        _ => crate::storage::schema::function_catalog::resolve(name, arg_types)
            .map(|entry| entry.return_type)
            .unwrap_or(DataType::Nullable),
    }
}

fn resolve_coalesce_return_type(arg_types: &[DataType]) -> DataType {
    let mut resolved: Option<DataType> = None;

    for &arg_ty in arg_types {
        match merge_compatible_type(resolved, arg_ty) {
            Ok(next) => resolved = next,
            Err(_) => return DataType::Nullable,
        }
    }

    resolved.unwrap_or(DataType::Nullable)
}

fn merge_compatible_type(
    current: Option<DataType>,
    next: DataType,
) -> Result<Option<DataType>, ()> {
    if next == DataType::Nullable {
        return Ok(current);
    }

    match current {
        None => Ok(Some(next)),
        Some(prev) if prev == next => Ok(Some(prev)),
        Some(prev) if can_implicit_cast(next, prev) => Ok(Some(prev)),
        Some(prev) if can_implicit_cast(prev, next) => Ok(Some(next)),
        Some(_) => Err(()),
    }
}

/// Resolve the result type of a unary operator. Negation requires a
/// numeric operand; logical NOT requires a boolean.
fn unary_result_type(op: UnaryOp, operand: DataType) -> Result<DataType, TypeError> {
    match op {
        UnaryOp::Neg if operand.category() == TypeCategory::Numeric => Ok(operand),
        UnaryOp::Not if operand == DataType::Boolean => Ok(DataType::Boolean),
        _ => Err(TypeError::UnaryMismatch { op, operand }),
    }
}

/// Resolve the result type of a binary operator. Implements a
/// simplified PG `func_select_candidate` heuristic:
///
/// 1. Short-circuit on identical types.
/// 2. Logical (AND/OR) require booleans on both sides.
/// 3. Comparison operators always return Boolean. Operands must
///    share a category; cross-category comparison is an error.
/// 4. Arithmetic operators promote to the preferred type of the
///    common Numeric category (Float wins over Integer / BigInt).
/// 5. `||` (Concat) requires String on both sides — anything that
///    isn't already Text needs an explicit CAST first.
fn binop_result_type(op: BinOp, lhs: DataType, rhs: DataType) -> Result<DataType, TypeError> {
    use BinOp::*;
    match op {
        And | Or => {
            if lhs == DataType::Boolean && rhs == DataType::Boolean {
                Ok(DataType::Boolean)
            } else {
                Err(TypeError::OperatorMismatch { op, lhs, rhs })
            }
        }
        Eq | Ne | Lt | Le | Gt | Ge => {
            // Same type → trivial. Different types → must share a
            // category and have an implicit-cast bridge in either
            // direction.
            if lhs == rhs {
                return Ok(DataType::Boolean);
            }
            if lhs.category() == rhs.category()
                && (can_implicit_cast(lhs, rhs) || can_implicit_cast(rhs, lhs))
            {
                return Ok(DataType::Boolean);
            }
            Err(TypeError::OperatorMismatch { op, lhs, rhs })
        }
        Add | Sub | Mul | Div | Mod => {
            if lhs.category() != TypeCategory::Numeric || rhs.category() != TypeCategory::Numeric {
                return Err(TypeError::OperatorMismatch { op, lhs, rhs });
            }
            // Promote to the preferred member of the category if
            // either side is preferred. Float beats Integer beats
            // BigInt under our preference rules.
            if lhs == DataType::Float || rhs == DataType::Float {
                Ok(DataType::Float)
            } else if lhs == DataType::Decimal || rhs == DataType::Decimal {
                Ok(DataType::Decimal)
            } else if lhs == DataType::BigInt || rhs == DataType::BigInt {
                Ok(DataType::BigInt)
            } else {
                Ok(DataType::Integer)
            }
        }
        Concat => {
            if lhs == DataType::Text && rhs == DataType::Text {
                Ok(DataType::Text)
            } else {
                Err(TypeError::OperatorMismatch { op, lhs, rhs })
            }
        }
    }
}

// `Cast::context_for` lookup helper is unused for now — the typer
// always uses Explicit context for user-written CAST nodes. Kept as
// a hook for the future Implicit-on-arith path.
#[allow(dead_code)]
fn _ctx_explicit() -> CastContext {
    CastContext::Explicit
}
