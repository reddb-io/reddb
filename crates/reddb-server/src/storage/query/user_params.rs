//! User-supplied positional parameter binding for `$N` placeholders.
//!
//! Tracer-bullet half of issue #353. The parser emits `Expr::Parameter`
//! nodes when it sees `$N`; this module validates that the indices form
//! a contiguous 0-based range and substitutes the user-provided values
//! into the AST. Type validation is delegated to the existing engine
//! type checker, which runs on the substituted literals downstream.

use crate::storage::query::ast::{Expr, QueryExpr};
use crate::storage::query::planner::shape::bind_user_param_query;
use crate::storage::schema::Value;

/// Errors surfaced when binding fails. The wire layer turns these into
/// `QUERY_ERROR` / `INVALID_PARAMS` responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserParamError {
    /// Caller supplied fewer or more values than the SQL references.
    /// `expected` is the highest `$N` index in the SQL (so a SQL using
    /// `$1` and `$3` reports `expected = 3`).
    Arity { expected: usize, got: usize },
    /// SQL uses `$1` and `$3` but not `$2` — placeholder indices must
    /// be a contiguous run starting from 1.
    Gap { missing: usize, max: usize },
    /// The runtime accepts only `QueryExpr` variants supported by the
    /// shape binder (Table / Join / Graph / Path / Vector / Hybrid).
    /// Other shapes (DDL, KV ops, etc.) cannot carry placeholders in
    /// the tracer-bullet scope.
    UnsupportedShape,
}

impl std::fmt::Display for UserParamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserParamError::Arity { expected, got } => write!(
                f,
                "wrong number of parameters: SQL expects {expected}, got {got}"
            ),
            UserParamError::Gap { missing, max } => write!(
                f,
                "parameter $`{missing}` is missing (max index used is ${max}) — `$N` indices must be contiguous starting at $1"
            ),
            UserParamError::UnsupportedShape => f.write_str(
                "this query shape does not support `$N` parameters in the tracer-bullet slice",
            ),
        }
    }
}

impl std::error::Error for UserParamError {}

/// Walk `expr`, collect every `Expr::Parameter { index }` encountered.
pub fn collect_indices(expr: &QueryExpr) -> Vec<usize> {
    let mut out = Vec::new();
    visit_query_expr(expr, &mut |e| {
        if let Expr::Parameter { index, .. } = e {
            out.push(*index);
        }
    });
    out
}

/// Validate that the indices used by the SQL match the caller's
/// supplied params (contiguous from 0, length match).
pub fn validate(indices: &[usize], param_count: usize) -> Result<(), UserParamError> {
    let max_used = indices.iter().copied().max();

    let expected = match max_used {
        Some(m) => m + 1,
        None => 0,
    };

    if expected != param_count {
        return Err(UserParamError::Arity {
            expected,
            got: param_count,
        });
    }

    if let Some(max) = max_used {
        let mut seen = vec![false; max + 1];
        for &i in indices {
            seen[i] = true;
        }
        for (i, used) in seen.iter().enumerate() {
            if !used {
                return Err(UserParamError::Gap {
                    missing: i + 1,
                    max: max + 1,
                });
            }
        }
    }

    Ok(())
}

/// One-shot helper: validate arity/gaps then substitute the values.
pub fn bind(expr: &QueryExpr, params: &[Value]) -> Result<QueryExpr, UserParamError> {
    let indices = collect_indices(expr);
    validate(&indices, params.len())?;

    if indices.is_empty() {
        return Ok(expr.clone());
    }

    bind_user_param_query(expr, params).ok_or(UserParamError::UnsupportedShape)
}

fn visit_query_expr<F: FnMut(&Expr)>(expr: &QueryExpr, visit: &mut F) {
    match expr {
        QueryExpr::Table(q) => {
            for item in &q.select_items {
                if let crate::storage::query::ast::SelectItem::Expr { expr, .. } = item {
                    visit_expr(expr, visit);
                }
            }
            if let Some(e) = &q.where_expr {
                visit_expr(e, visit);
            }
            for e in &q.group_by_exprs {
                visit_expr(e, visit);
            }
            if let Some(e) = &q.having_expr {
                visit_expr(e, visit);
            }
            for clause in &q.order_by {
                if let Some(e) = &clause.expr {
                    visit_expr(e, visit);
                }
            }
            if let Some(crate::storage::query::ast::TableSource::Subquery(inner)) = &q.source {
                visit_query_expr(inner, visit);
            }
        }
        QueryExpr::Join(q) => {
            visit_query_expr(&q.left, visit);
            visit_query_expr(&q.right, visit);
        }
        QueryExpr::Hybrid(q) => {
            visit_query_expr(&q.structured, visit);
        }
        // Vector / Graph / Path: parameter slots in #355 / later issues.
        _ => {}
    }
}

fn visit_expr<F: FnMut(&Expr)>(expr: &Expr, visit: &mut F) {
    visit(expr);
    match expr {
        Expr::Literal { .. } | Expr::Column { .. } | Expr::Parameter { .. } => {}
        Expr::BinaryOp { lhs, rhs, .. } => {
            visit_expr(lhs, visit);
            visit_expr(rhs, visit);
        }
        Expr::UnaryOp { operand, .. } => visit_expr(operand, visit),
        Expr::Cast { inner, .. } => visit_expr(inner, visit),
        Expr::FunctionCall { args, .. } => {
            for a in args {
                visit_expr(a, visit);
            }
        }
        Expr::Case { branches, else_, .. } => {
            for (c, v) in branches {
                visit_expr(c, visit);
                visit_expr(v, visit);
            }
            if let Some(e) = else_ {
                visit_expr(e, visit);
            }
        }
        Expr::IsNull { operand, .. } => visit_expr(operand, visit),
        Expr::InList { target, values, .. } => {
            visit_expr(target, visit);
            for v in values {
                visit_expr(v, visit);
            }
        }
        Expr::Between {
            target, low, high, ..
        } => {
            visit_expr(target, visit);
            visit_expr(low, visit);
            visit_expr(high, visit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::modes::parse_multi;

    fn parse(sql: &str) -> QueryExpr {
        parse_multi(sql).expect("parse")
    }

    #[test]
    fn collect_indices_select_where() {
        let q = parse("SELECT * FROM users WHERE id = $1 AND name = $2");
        let mut ix = collect_indices(&q);
        ix.sort();
        assert_eq!(ix, vec![0, 1]);
    }

    #[test]
    fn validate_ok() {
        assert!(validate(&[0, 1], 2).is_ok());
        assert!(validate(&[0, 1, 0], 2).is_ok());
        assert!(validate(&[], 0).is_ok());
    }

    #[test]
    fn validate_arity_too_few() {
        let err = validate(&[0, 1], 1).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::Arity {
                expected: 2,
                got: 1
            }
        ));
    }

    #[test]
    fn validate_arity_too_many() {
        let err = validate(&[0], 3).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::Arity {
                expected: 1,
                got: 3
            }
        ));
    }

    #[test]
    fn validate_gap() {
        // $1 and $3 used, but not $2.
        let err = validate(&[0, 2], 3).unwrap_err();
        assert!(matches!(err, UserParamError::Gap { missing: 2, .. }));
    }

    #[test]
    fn bind_substitutes_int_param() {
        let q = parse("SELECT * FROM users WHERE id = $1");
        let bound = bind(&q, &[Value::Integer(42)]).unwrap();
        let QueryExpr::Table(t) = bound else {
            panic!("expected Table");
        };
        let Expr::BinaryOp { rhs, .. } = t.where_expr.unwrap() else {
            panic!("expected BinaryOp");
        };
        assert!(matches!(
            *rhs,
            Expr::Literal {
                value: Value::Integer(42),
                ..
            }
        ));
    }

    #[test]
    fn bind_substitutes_text_and_null() {
        let q = parse("SELECT * FROM users WHERE name = $1 AND deleted = $2");
        let bound = bind(&q, &[Value::text("Alice"), Value::Null]).unwrap();
        let QueryExpr::Table(t) = bound else {
            panic!("expected Table");
        };
        let mut literals: Vec<Value> = Vec::new();
        visit_expr(&t.where_expr.unwrap(), &mut |e| {
            if let Expr::Literal { value, .. } = e {
                literals.push(value.clone());
            }
        });
        assert!(literals
            .iter()
            .any(|v| matches!(v, Value::Text(s) if s.as_ref() == "Alice")));
        assert!(literals.iter().any(|v| matches!(v, Value::Null)));
    }

    #[test]
    fn bind_no_params_is_noop() {
        let q = parse("SELECT * FROM users");
        let bound = bind(&q, &[]).unwrap();
        assert!(matches!(bound, QueryExpr::Table(_)));
    }
}
