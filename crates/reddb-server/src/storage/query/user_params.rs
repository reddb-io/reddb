//! User-supplied positional parameter binding for `$N` placeholders.
//!
//! Tracer-bullet half of issue #353. The parser emits `Expr::Parameter`
//! nodes when it sees `$N`; this module validates that the indices form
//! a contiguous 0-based range and substitutes the user-provided values
//! into the AST. Type validation is delegated to the existing engine
//! type checker, which runs on the substituted literals downstream.

use crate::storage::query::ast::{Expr, QueryExpr, SearchCommand};
use crate::storage::query::planner::shape::bind_user_param_query;
use crate::storage::query::sql_lowering::fold_expr_to_value;
use crate::storage::schema::Value;

/// Recursively check whether `expr` contains any `Expr::Parameter` node.
/// Used by the INSERT parser to know when to defer literal folding to
/// the user_params binder.
pub fn expr_contains_parameter(expr: &Expr) -> bool {
    match expr {
        Expr::Parameter { .. } => true,
        Expr::Literal { .. } | Expr::Column { .. } => false,
        Expr::BinaryOp { lhs, rhs, .. } => {
            expr_contains_parameter(lhs) || expr_contains_parameter(rhs)
        }
        Expr::UnaryOp { operand, .. } => expr_contains_parameter(operand),
        Expr::Cast { inner, .. } => expr_contains_parameter(inner),
        Expr::FunctionCall { args, .. } => args.iter().any(expr_contains_parameter),
        Expr::Case { branches, else_, .. } => {
            branches
                .iter()
                .any(|(c, v)| expr_contains_parameter(c) || expr_contains_parameter(v))
                || else_.as_deref().is_some_and(expr_contains_parameter)
        }
        Expr::IsNull { operand, .. } => expr_contains_parameter(operand),
        Expr::InList { target, values, .. } => {
            expr_contains_parameter(target) || values.iter().any(expr_contains_parameter)
        }
        Expr::Between {
            target, low, high, ..
        } => {
            expr_contains_parameter(target)
                || expr_contains_parameter(low)
                || expr_contains_parameter(high)
        }
    }
}

/// Substitute every `Expr::Parameter { index }` in `expr` with
/// `Expr::Literal { value: params[index] }`. Used by INSERT binding,
/// which must hand a fully literal AST to `fold_expr_to_value`.
fn substitute_params_in_expr(expr: Expr, params: &[Value]) -> Result<Expr, UserParamError> {
    match expr {
        Expr::Parameter { index, span } => {
            let value = params.get(index).ok_or(UserParamError::Arity {
                expected: index + 1,
                got: params.len(),
            })?;
            Ok(Expr::Literal {
                value: value.clone(),
                span,
            })
        }
        Expr::Literal { .. } | Expr::Column { .. } => Ok(expr),
        Expr::BinaryOp { op, lhs, rhs, span } => Ok(Expr::BinaryOp {
            op,
            lhs: Box::new(substitute_params_in_expr(*lhs, params)?),
            rhs: Box::new(substitute_params_in_expr(*rhs, params)?),
            span,
        }),
        Expr::UnaryOp { op, operand, span } => Ok(Expr::UnaryOp {
            op,
            operand: Box::new(substitute_params_in_expr(*operand, params)?),
            span,
        }),
        Expr::Cast { inner, target, span } => Ok(Expr::Cast {
            inner: Box::new(substitute_params_in_expr(*inner, params)?),
            target,
            span,
        }),
        Expr::FunctionCall { name, args, span } => {
            let new_args = args
                .into_iter()
                .map(|a| substitute_params_in_expr(a, params))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expr::FunctionCall {
                name,
                args: new_args,
                span,
            })
        }
        Expr::Case {
            branches,
            else_,
            span,
        } => {
            let new_branches = branches
                .into_iter()
                .map(|(c, v)| {
                    Ok::<_, UserParamError>((
                        substitute_params_in_expr(c, params)?,
                        substitute_params_in_expr(v, params)?,
                    ))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let new_else = match else_ {
                Some(e) => Some(Box::new(substitute_params_in_expr(*e, params)?)),
                None => None,
            };
            Ok(Expr::Case {
                branches: new_branches,
                else_: new_else,
                span,
            })
        }
        Expr::IsNull {
            operand,
            negated,
            span,
        } => Ok(Expr::IsNull {
            operand: Box::new(substitute_params_in_expr(*operand, params)?),
            negated,
            span,
        }),
        Expr::InList {
            target,
            values,
            negated,
            span,
        } => Ok(Expr::InList {
            target: Box::new(substitute_params_in_expr(*target, params)?),
            values: values
                .into_iter()
                .map(|v| substitute_params_in_expr(v, params))
                .collect::<Result<Vec<_>, _>>()?,
            negated,
            span,
        }),
        Expr::Between {
            target,
            low,
            high,
            negated,
            span,
        } => Ok(Expr::Between {
            target: Box::new(substitute_params_in_expr(*target, params)?),
            low: Box::new(substitute_params_in_expr(*low, params)?),
            high: Box::new(substitute_params_in_expr(*high, params)?),
            negated,
            span,
        }),
    }
}


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
    /// A parameter was supplied in a slot that requires a specific type
    /// (e.g. a vector slot received a string). `slot` describes the
    /// context, `got` describes the user-supplied value's variant.
    TypeMismatch { slot: &'static str, got: &'static str },
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
            UserParamError::TypeMismatch { slot, got } => write!(
                f,
                "parameter type mismatch: {slot} requires a vector, got {got}"
            ),
        }
    }
}

impl std::error::Error for UserParamError {}

/// Walk `expr`, collect every `Expr::Parameter { index }` encountered.
/// Also picks up parameter slots that live outside the `Expr` tree —
/// today only the vector slot of `SEARCH SIMILAR $N` (see #355).
pub fn collect_indices(expr: &QueryExpr) -> Vec<usize> {
    let mut out = Vec::new();
    visit_query_expr(expr, &mut |e| {
        if let Expr::Parameter { index, .. } = e {
            out.push(*index);
        }
    });
    collect_non_expr_indices(expr, &mut out);
    out
}

/// Parameter slots that live on AST nodes outside the `Expr` tree
/// (e.g. `SearchCommand::Similar { vector_param }`).
fn collect_non_expr_indices(expr: &QueryExpr, out: &mut Vec<usize>) {
    if let QueryExpr::SearchCommand(SearchCommand::Similar { vector_param, .. }) = expr {
        if let Some(idx) = vector_param {
            out.push(*idx);
        }
    }
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

    // SEARCH SIMILAR $N has its parameter slot outside the `Expr`
    // tree — handle it here rather than threading the binds through
    // the planner's shape binder, which only knows about `Expr` slots.
    if let QueryExpr::SearchCommand(SearchCommand::Similar {
        vector,
        text,
        provider,
        collection,
        limit,
        min_score,
        vector_param,
    }) = expr
    {
        let mut bound_vector = vector.clone();
        if let Some(idx) = vector_param {
            let value = params
                .get(*idx)
                .ok_or(UserParamError::Arity {
                    expected: idx + 1,
                    got: params.len(),
                })?;
            bound_vector = match value {
                Value::Vector(v) => v.clone(),
                other => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH SIMILAR vector parameter",
                        got: value_variant_name(other),
                    });
                }
            };
        }
        return Ok(QueryExpr::SearchCommand(SearchCommand::Similar {
            vector: bound_vector,
            text: text.clone(),
            provider: provider.clone(),
            collection: collection.clone(),
            limit: *limit,
            min_score: *min_score,
            vector_param: None,
        }));
    }

    if let QueryExpr::Insert(insert) = expr {
        let mut bound = insert.clone();
        let mut new_values: Vec<Vec<Value>> = Vec::with_capacity(bound.value_exprs.len());
        let new_exprs = bound
            .value_exprs
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|e| substitute_params_in_expr(e, params))
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        for row in &new_exprs {
            let folded = row
                .iter()
                .cloned()
                .map(fold_expr_to_value)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| UserParamError::UnsupportedShape)?;
            new_values.push(folded);
        }
        bound.value_exprs = new_exprs;
        bound.values = new_values;
        return Ok(QueryExpr::Insert(bound));
    }

    bind_user_param_query(expr, params).ok_or(UserParamError::UnsupportedShape)
}

fn value_variant_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Integer(_) => "integer",
        Value::UnsignedInteger(_) => "unsigned integer",
        Value::BigInt(_) => "bigint",
        Value::Float(_) => "float",
        Value::Text(_) => "text",
        Value::Boolean(_) => "boolean",
        Value::Vector(_) => "vector",
        Value::Json(_) => "json",
        Value::Blob(_) => "bytes",
        _ => "other",
    }
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
        QueryExpr::Insert(q) => {
            for row in &q.value_exprs {
                for e in row {
                    visit_expr(e, visit);
                }
            }
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
    fn bind_search_similar_vector_param() {
        // Tracer for #355: `SEARCH SIMILAR $1 COLLECTION embeddings`
        // binds the supplied `Value::Vector` into the vector slot.
        let q = parse("SEARCH SIMILAR $1 COLLECTION embeddings LIMIT 5");
        let bound = bind(&q, &[Value::Vector(vec![0.1, 0.2, 0.3])]).unwrap();
        let QueryExpr::SearchCommand(SearchCommand::Similar {
            vector,
            vector_param,
            collection,
            limit,
            ..
        }) = bound
        else {
            panic!("expected SearchCommand::Similar");
        };
        assert_eq!(vector, vec![0.1f32, 0.2, 0.3]);
        assert_eq!(vector_param, None, "vector_param must be cleared post-bind");
        assert_eq!(collection, "embeddings");
        assert_eq!(limit, 5);
    }

    #[test]
    fn bind_search_similar_rejects_non_vector_param() {
        let q = parse("SEARCH SIMILAR $1 COLLECTION embeddings");
        let err = bind(&q, &[Value::Integer(42)]).unwrap_err();
        assert!(
            matches!(
                err,
                UserParamError::TypeMismatch {
                    slot: "SEARCH SIMILAR vector parameter",
                    got: "integer"
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn bind_search_similar_empty_vector_param() {
        let q = parse("SEARCH SIMILAR $1 COLLECTION embeddings");
        let bound = bind(&q, &[Value::Vector(vec![])]).unwrap();
        let QueryExpr::SearchCommand(SearchCommand::Similar { vector, .. }) = bound else {
            panic!("expected SearchCommand::Similar");
        };
        assert!(vector.is_empty());
    }

    #[test]
    fn bind_insert_values_with_vector_param() {
        // Issue #355 INSERT half: $1 in VALUES is bound to a Value::Vector
        // and surfaces in both `value_exprs` (as a Literal) and `values`.
        let q = parse(
            "INSERT INTO embeddings (dense, content) VALUES ($1, $2)",
        );
        let vec = Value::Vector(vec![0.1, 0.2, 0.3]);
        let bound = bind(&q, &[vec.clone(), Value::text("doc text")]).unwrap();
        let QueryExpr::Insert(insert) = bound else {
            panic!("expected Insert");
        };
        assert_eq!(insert.values.len(), 1);
        assert_eq!(insert.values[0].len(), 2);
        assert!(matches!(insert.values[0][0], Value::Vector(ref v) if v == &vec![0.1f32, 0.2, 0.3]));
        assert!(matches!(insert.values[0][1], Value::Text(ref s) if s.as_ref() == "doc text"));
        // value_exprs row 0 col 0 is now a Literal carrying the vector.
        let row0 = &insert.value_exprs[0];
        assert!(matches!(
            &row0[0],
            Expr::Literal { value: Value::Vector(_), .. }
        ));
    }

    #[test]
    fn bind_insert_arity_mismatch() {
        let q = parse("INSERT INTO t (a, b) VALUES ($1, $2)");
        let err = bind(&q, &[Value::Integer(1)]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::Arity {
                expected: 2,
                got: 1
            }
        ));
    }

    #[test]
    fn bind_no_params_is_noop() {
        let q = parse("SELECT * FROM users");
        let bound = bind(&q, &[]).unwrap();
        assert!(matches!(bound, QueryExpr::Table(_)));
    }
}
