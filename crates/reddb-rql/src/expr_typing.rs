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

use crate::ast::{BinOp, Expr, FieldRef, UnaryOp};
use reddb_types::cast_catalog::{can_implicit_cast, CastContext};
use reddb_types::types::{DataType, TypeCategory, Value};

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
            if !reddb_types::cast_catalog::can_explicit_cast(inner_typed.ty, *target) {
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
        Expr::Subquery { .. } => Ok(TypedExpr {
            ty: DataType::Nullable,
            kind: TypedExprKind::Literal(Value::Null),
        }),
        // Slice 7a (#589): no concrete type yet — the analytics
        // executor and its type inference land in a follow-up. Fall
        // back to Nullable so downstream callers don't trip.
        Expr::WindowFunctionCall { .. } => Ok(TypedExpr {
            ty: DataType::Nullable,
            kind: TypedExprKind::Literal(Value::Null),
        }),
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
        Value::AssetCode(_) => DataType::AssetCode,
        Value::Money { .. } => DataType::Money,
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
        Value::DecimalText(_) => DataType::DecimalText,
    }
}

fn resolve_function_return_type(name: &str, arg_types: &[DataType]) -> DataType {
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        // CONCAT stringifies every non-null argument at runtime, so the
        // return type is always text even when the catalog match is
        // intentionally loose.
        "CONCAT" | "CONCAT_WS" | "QUOTE_LITERAL" => DataType::Text,
        "MONEY" => DataType::Money,
        "MONEY_ASSET" => DataType::AssetCode,
        "MONEY_MINOR" => DataType::BigInt,
        "MONEY_SCALE" => DataType::Integer,
        // COALESCE is effectively `anycompatible`: ignore NULL/unknown
        // args, then widen left-to-right using the implicit-cast graph.
        "COALESCE" => resolve_coalesce_return_type(arg_types),
        _ => reddb_types::function_catalog::resolve(name, arg_types)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Span;
    use crate::lexer::Position;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;

    fn span() -> Span {
        Span {
            start: Position::default(),
            end: Position::default(),
        }
    }

    fn lit(value: Value) -> Expr {
        Expr::Literal {
            value,
            span: span(),
        }
    }

    fn col(table: &str, column: &str) -> Expr {
        Expr::Column {
            field: FieldRef::column(table, column),
            span: span(),
        }
    }

    fn bin(op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
        Expr::BinaryOp {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            span: span(),
        }
    }

    fn unary(op: UnaryOp, operand: Expr) -> Expr {
        Expr::UnaryOp {
            op,
            operand: Box::new(operand),
            span: span(),
        }
    }

    fn scope(table: &str, column: &str) -> Option<DataType> {
        match (table, column) {
            ("", "age") => Some(DataType::Integer),
            ("", "active") => Some(DataType::Boolean),
            ("users", "name") => Some(DataType::Text),
            ("n", "score") => Some(DataType::Float),
            _ => None,
        }
    }

    fn no_scope(_: &str, _: &str) -> Option<DataType> {
        None
    }

    #[test]
    fn literal_values_map_to_declared_types() {
        let values = vec![
            (Value::Null, DataType::Nullable),
            (Value::Boolean(true), DataType::Boolean),
            (Value::Integer(1), DataType::Integer),
            (Value::UnsignedInteger(1), DataType::UnsignedInteger),
            (Value::Float(1.0), DataType::Float),
            (Value::BigInt(1), DataType::BigInt),
            (Value::Decimal(100), DataType::Decimal),
            (Value::Text(Arc::from("x")), DataType::Text),
            (Value::Blob(vec![1, 2]), DataType::Blob),
            (Value::Timestamp(1), DataType::Timestamp),
            (Value::TimestampMs(1), DataType::TimestampMs),
            (Value::Duration(1), DataType::Duration),
            (Value::Date(1), DataType::Date),
            (Value::Time(1), DataType::Time),
            (
                Value::IpAddr(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                DataType::IpAddr,
            ),
            (Value::Ipv4(0x7f00_0001), DataType::Ipv4),
            (Value::Ipv6([0; 16]), DataType::Ipv6),
            (Value::Subnet(0, 24), DataType::Subnet),
            (Value::Cidr(0, 24), DataType::Cidr),
            (Value::MacAddr([1, 2, 3, 4, 5, 6]), DataType::MacAddr),
            (Value::Port(5432), DataType::Port),
            (Value::Latitude(1), DataType::Latitude),
            (Value::Longitude(1), DataType::Longitude),
            (Value::GeoPoint(1, 2), DataType::GeoPoint),
            (Value::Country2(*b"BR"), DataType::Country2),
            (Value::Country3(*b"BRA"), DataType::Country3),
            (Value::Lang2(*b"pt"), DataType::Lang2),
            (Value::Lang5(*b"pt-BR"), DataType::Lang5),
            (Value::Currency(*b"BRL"), DataType::Currency),
            (Value::AssetCode("BTC".to_string()), DataType::AssetCode),
            (
                Value::Money {
                    asset_code: "BRL".to_string(),
                    minor_units: 123,
                    scale: 2,
                },
                DataType::Money,
            ),
            (Value::Color([1, 2, 3]), DataType::Color),
            (Value::ColorAlpha([1, 2, 3, 4]), DataType::ColorAlpha),
            (Value::Email("a@example.com".to_string()), DataType::Email),
            (Value::Url("https://example.com".to_string()), DataType::Url),
            (Value::Phone(5511999999999), DataType::Phone),
            (Value::Semver(1_002_003), DataType::Semver),
            (Value::Uuid([1; 16]), DataType::Uuid),
            (Value::Vector(vec![1.0, 2.0]), DataType::Vector),
            (Value::Array(vec![Value::Integer(1)]), DataType::Array),
            (Value::Json(br#"{"x":1}"#.to_vec()), DataType::Json),
            (Value::EnumValue(1), DataType::Enum),
            (Value::NodeRef("n1".to_string()), DataType::NodeRef),
            (Value::EdgeRef("e1".to_string()), DataType::EdgeRef),
            (Value::VectorRef("vecs".to_string(), 1), DataType::VectorRef),
            (Value::RowRef("rows".to_string(), 1), DataType::RowRef),
            (
                Value::KeyRef("kv".to_string(), "k".to_string()),
                DataType::KeyRef,
            ),
            (Value::DocRef("docs".to_string(), 1), DataType::DocRef),
            (Value::TableRef("users".to_string()), DataType::TableRef),
            (Value::PageRef(7), DataType::PageRef),
            (Value::Secret(vec![1, 2, 3]), DataType::Secret),
            (Value::Password("argon2".to_string()), DataType::Password),
        ];

        for (value, expected) in values {
            let typed = type_expr(&lit(value), &no_scope).unwrap();
            assert_eq!(typed.ty, expected);
            assert!(matches!(typed.kind, TypedExprKind::Literal(_)));
        }
    }

    #[test]
    fn column_lookup_preserves_field_ref_and_reports_unknowns() {
        let typed = type_expr(&col("users", "name"), &scope).unwrap();
        assert_eq!(typed.ty, DataType::Text);
        assert!(matches!(
            typed.kind,
            TypedExprKind::Column(FieldRef::TableColumn { table, column })
                if table == "users" && column == "name"
        ));

        let err = type_expr(&col("", "missing"), &scope).unwrap_err();
        assert!(matches!(
            err,
            TypeError::UnknownColumn { ref table, ref column }
                if table.is_empty() && column == "missing"
        ));
        assert_eq!(err.to_string(), "unknown column `missing`");
    }

    #[test]
    fn arithmetic_logical_and_unary_ops_return_expected_types() {
        let add = bin(BinOp::Add, lit(Value::Integer(1)), lit(Value::Float(2.0)));
        assert_eq!(type_expr(&add, &scope).unwrap().ty, DataType::Float);

        let and = bin(BinOp::And, col("", "active"), lit(Value::Boolean(false)));
        assert_eq!(type_expr(&and, &scope).unwrap().ty, DataType::Boolean);

        let neg = unary(UnaryOp::Neg, col("", "age"));
        assert_eq!(type_expr(&neg, &scope).unwrap().ty, DataType::Integer);

        let not = unary(UnaryOp::Not, col("", "active"));
        assert_eq!(type_expr(&not, &scope).unwrap().ty, DataType::Boolean);
    }

    #[test]
    fn operator_mismatches_are_reported() {
        let bad_and = bin(
            BinOp::And,
            lit(Value::Boolean(true)),
            lit(Value::Integer(1)),
        );
        assert!(matches!(
            type_expr(&bad_and, &scope).unwrap_err(),
            TypeError::OperatorMismatch {
                op: BinOp::And,
                lhs: DataType::Boolean,
                rhs: DataType::Integer,
            }
        ));

        let bad_neg = unary(UnaryOp::Neg, lit(Value::Text(Arc::from("x"))));
        assert!(matches!(
            type_expr(&bad_neg, &scope).unwrap_err(),
            TypeError::UnaryMismatch {
                op: UnaryOp::Neg,
                operand: DataType::Text,
            }
        ));
    }

    #[test]
    fn casts_functions_and_parameters_have_stable_types() {
        let cast = Expr::Cast {
            inner: Box::new(lit(Value::Integer(1))),
            target: DataType::Text,
            span: span(),
        };
        assert_eq!(type_expr(&cast, &scope).unwrap().ty, DataType::Text);

        let concat = Expr::FunctionCall {
            name: "concat".to_string(),
            args: vec![lit(Value::Text(Arc::from("a"))), lit(Value::Integer(1))],
            span: span(),
        };
        assert_eq!(type_expr(&concat, &scope).unwrap().ty, DataType::Text);

        let money_minor = Expr::FunctionCall {
            name: "money_minor".to_string(),
            args: vec![lit(Value::Money {
                asset_code: "BRL".to_string(),
                minor_units: 10,
                scale: 2,
            })],
            span: span(),
        };
        assert_eq!(
            type_expr(&money_minor, &scope).unwrap().ty,
            DataType::BigInt
        );

        let coalesce = Expr::FunctionCall {
            name: "coalesce".to_string(),
            args: vec![
                lit(Value::Null),
                lit(Value::Integer(1)),
                lit(Value::Float(2.0)),
            ],
            span: span(),
        };
        assert_eq!(type_expr(&coalesce, &scope).unwrap().ty, DataType::Integer);

        let unknown = Expr::FunctionCall {
            name: "not_a_function".to_string(),
            args: Vec::new(),
            span: span(),
        };
        assert_eq!(type_expr(&unknown, &scope).unwrap().ty, DataType::Nullable);

        let parameter = Expr::Parameter {
            index: 1,
            span: span(),
        };
        assert_eq!(
            type_expr(&parameter, &scope).unwrap().ty,
            DataType::Nullable
        );
    }

    #[test]
    fn invalid_casts_case_branches_and_lists_are_errors() {
        let bad_cast = Expr::Cast {
            inner: Box::new(lit(Value::Blob(vec![1]))),
            target: DataType::Money,
            span: span(),
        };
        assert!(matches!(
            type_expr(&bad_cast, &scope).unwrap_err(),
            TypeError::InvalidCast {
                src: DataType::Blob,
                target: DataType::Money,
            }
        ));

        let case = Expr::Case {
            branches: vec![(
                lit(Value::Boolean(true)),
                lit(Value::Text(Arc::from("text"))),
            )],
            else_: Some(Box::new(lit(Value::Integer(1)))),
            span: span(),
        };
        assert!(matches!(
            type_expr(&case, &scope).unwrap_err(),
            TypeError::CaseBranchMismatch {
                first: DataType::Text,
                other: DataType::Integer,
            }
        ));

        let in_list = Expr::InList {
            target: Box::new(lit(Value::Integer(1))),
            values: vec![lit(Value::Text(Arc::from("x")))],
            negated: false,
            span: span(),
        };
        assert!(matches!(
            type_expr(&in_list, &scope).unwrap_err(),
            TypeError::InListMismatch {
                target: DataType::Integer,
                element: DataType::Text,
            }
        ));
    }

    #[test]
    fn predicates_return_boolean_when_bounds_and_values_are_compatible() {
        let is_null = Expr::IsNull {
            operand: Box::new(col("", "age")),
            negated: true,
            span: span(),
        };
        assert_eq!(type_expr(&is_null, &scope).unwrap().ty, DataType::Boolean);

        let in_list = Expr::InList {
            target: Box::new(col("", "age")),
            values: vec![lit(Value::Integer(1)), lit(Value::Integer(2))],
            negated: false,
            span: span(),
        };
        assert_eq!(type_expr(&in_list, &scope).unwrap().ty, DataType::Boolean);

        let between = Expr::Between {
            target: Box::new(col("", "age")),
            low: Box::new(lit(Value::Integer(1))),
            high: Box::new(lit(Value::Integer(9))),
            negated: false,
            span: span(),
        };
        assert_eq!(type_expr(&between, &scope).unwrap().ty, DataType::Boolean);

        let bad_between = Expr::Between {
            target: Box::new(col("", "age")),
            low: Box::new(lit(Value::Text(Arc::from("low")))),
            high: Box::new(lit(Value::Integer(9))),
            negated: false,
            span: span(),
        };
        assert!(matches!(
            type_expr(&bad_between, &scope).unwrap_err(),
            TypeError::OperatorMismatch {
                op: BinOp::Ge,
                lhs: DataType::Integer,
                rhs: DataType::Text,
            }
        ));
    }
}
