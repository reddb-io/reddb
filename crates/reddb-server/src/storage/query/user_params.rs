//! User-supplied positional parameter binding for `$N` placeholders.
//!
//! Tracer-bullet half of issue #353. The parser emits `Expr::Parameter`
//! nodes when it sees `$N`; this module validates that the indices form
//! a contiguous 0-based range and substitutes the user-provided values
//! into the AST. Type validation is delegated to the existing engine
//! type checker, which runs on the substituted literals downstream.

use crate::storage::query::ast::{Expr, QueryExpr, SearchCommand, Span};
use crate::storage::query::planner::shape::bind_user_param_query;
use crate::storage::query::sql_lowering::{expr_to_filter, fold_expr_to_value};
use crate::storage::schema::Value;

/// One parameter placeholder found in the parsed query AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParameterRef {
    /// Zero-based index into the caller-supplied parameter slice.
    pub index: usize,
    /// Source span of the placeholder token.
    pub span: Span,
}

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
        Expr::Case {
            branches, else_, ..
        } => {
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
        Expr::Subquery { .. } => false,
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
        Expr::Cast {
            inner,
            target,
            span,
        } => Ok(Expr::Cast {
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
        Expr::Subquery { .. } => Ok(expr),
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
    TypeMismatch {
        slot: &'static str,
        got: &'static str,
    },
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
                "parameter type mismatch: {slot} (got {got})"
            ),
        }
    }
}

impl std::error::Error for UserParamError {}

/// Public bind error alias matching the parameter-contract ADR wording.
pub type BindError = UserParamError;

/// Walk `expr`, collecting parameter placeholders that carry source spans.
pub fn scan_parameters(expr: &QueryExpr) -> Vec<ParameterRef> {
    let mut out = Vec::new();
    visit_query_expr(expr, &mut |e| {
        if let Expr::Parameter { index, span } = e {
            out.push(ParameterRef {
                index: *index,
                span: *span,
            });
        }
    });
    out
}

/// Walk `expr`, collect every `Expr::Parameter { index }` encountered.
/// Also picks up parameter slots that live outside the `Expr` tree —
/// today only the vector slot of `SEARCH SIMILAR $N` (see #355).
pub fn collect_indices(expr: &QueryExpr) -> Vec<usize> {
    let mut out: Vec<usize> = scan_parameters(expr)
        .into_iter()
        .map(|param| param.index)
        .collect();
    collect_non_expr_indices(expr, &mut out);
    out
}

/// Parameter slots that live on AST nodes outside the `Expr` tree
/// (e.g. `SearchCommand::Similar { vector_param }`).
//
// `clippy::collapsible_match` would have us fold each `if let Some(idx) =
// limit_param` into the outer pattern. With 10+ near-identical SearchCommand
// variants, the collapsed form doubles the match arm count and obscures the
// shared shape. Keep the two-level form for symmetry.
#[allow(clippy::collapsible_match)]
fn collect_non_expr_indices(expr: &QueryExpr, out: &mut Vec<usize>) {
    match expr {
        QueryExpr::SearchCommand(SearchCommand::Similar {
            vector_param,
            limit_param,
            min_score_param,
            text_param,
            ..
        }) => {
            if let Some(idx) = vector_param {
                out.push(*idx);
            }
            if let Some(idx) = limit_param {
                out.push(*idx);
            }
            if let Some(idx) = min_score_param {
                out.push(*idx);
            }
            if let Some(idx) = text_param {
                out.push(*idx);
            }
        }
        QueryExpr::SearchCommand(SearchCommand::Hybrid { limit_param, .. }) => {
            if let Some(idx) = limit_param {
                out.push(*idx);
            }
        }
        QueryExpr::SearchCommand(SearchCommand::SpatialNearest { k_param, .. }) => {
            if let Some(idx) = k_param {
                out.push(*idx);
            }
        }
        QueryExpr::SearchCommand(SearchCommand::SpatialRadius { limit_param, .. }) => {
            if let Some(idx) = limit_param {
                out.push(*idx);
            }
        }
        QueryExpr::SearchCommand(SearchCommand::SpatialBbox { limit_param, .. }) => {
            if let Some(idx) = limit_param {
                out.push(*idx);
            }
        }
        QueryExpr::SearchCommand(SearchCommand::Text { limit_param, .. }) => {
            if let Some(idx) = limit_param {
                out.push(*idx);
            }
        }
        QueryExpr::SearchCommand(SearchCommand::Multimodal { limit_param, .. }) => {
            if let Some(idx) = limit_param {
                out.push(*idx);
            }
        }
        QueryExpr::SearchCommand(SearchCommand::Index { limit_param, .. }) => {
            if let Some(idx) = limit_param {
                out.push(*idx);
            }
        }
        QueryExpr::SearchCommand(SearchCommand::Context { limit_param, .. }) => {
            if let Some(idx) = limit_param {
                out.push(*idx);
            }
        }
        QueryExpr::Table(q) => {
            if let Some(idx) = q.limit_param {
                out.push(idx);
            }
            if let Some(idx) = q.offset_param {
                out.push(idx);
            }
        }
        QueryExpr::Ask(q) => {
            if let Some(idx) = q.question_param {
                out.push(idx);
            }
        }
        _ => {}
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
        limit_param,
        min_score_param,
        text_param,
    }) = expr
    {
        let mut bound_vector = vector.clone();
        if let Some(idx) = vector_param {
            let value = params.get(*idx).ok_or(UserParamError::Arity {
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
        let bound_limit = if let Some(idx) = limit_param {
            let value = params.get(*idx).ok_or(UserParamError::Arity {
                expected: idx + 1,
                got: params.len(),
            })?;
            match value {
                Value::Integer(n) if *n > 0 => *n as usize,
                Value::UnsignedInteger(n) if *n > 0 => *n as usize,
                Value::BigInt(n) if *n > 0 => *n as usize,
                Value::Integer(_) | Value::UnsignedInteger(_) | Value::BigInt(_) => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH SIMILAR LIMIT parameter (must be > 0)",
                        got: value_variant_name(value),
                    });
                }
                other => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH SIMILAR LIMIT parameter",
                        got: value_variant_name(other),
                    });
                }
            }
        } else {
            *limit
        };
        let bound_min_score = if let Some(idx) = min_score_param {
            let value = params.get(*idx).ok_or(UserParamError::Arity {
                expected: idx + 1,
                got: params.len(),
            })?;
            match value {
                Value::Float(f) => *f as f32,
                Value::Integer(n) => *n as f32,
                Value::UnsignedInteger(n) => *n as f32,
                Value::BigInt(n) => *n as f32,
                other => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH SIMILAR MIN_SCORE parameter",
                        got: value_variant_name(other),
                    });
                }
            }
        } else {
            *min_score
        };
        let bound_text = if let Some(idx) = text_param {
            let value = params.get(*idx).ok_or(UserParamError::Arity {
                expected: idx + 1,
                got: params.len(),
            })?;
            match value {
                Value::Text(s) => Some(s.to_string()),
                other => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH SIMILAR TEXT parameter",
                        got: value_variant_name(other),
                    });
                }
            }
        } else {
            text.clone()
        };
        return Ok(QueryExpr::SearchCommand(SearchCommand::Similar {
            vector: bound_vector,
            text: bound_text,
            provider: provider.clone(),
            collection: collection.clone(),
            limit: bound_limit,
            min_score: bound_min_score,
            vector_param: None,
            limit_param: None,
            min_score_param: None,
            text_param: None,
        }));
    }

    if let QueryExpr::SearchCommand(SearchCommand::Hybrid {
        vector,
        query,
        collection,
        limit,
        limit_param,
    }) = expr
    {
        let bound_limit = if let Some(idx) = limit_param {
            let value = params.get(*idx).ok_or(UserParamError::Arity {
                expected: idx + 1,
                got: params.len(),
            })?;
            match value {
                Value::Integer(n) if *n > 0 => *n as usize,
                Value::UnsignedInteger(n) if *n > 0 => *n as usize,
                Value::BigInt(n) if *n > 0 => *n as usize,
                Value::Integer(_) | Value::UnsignedInteger(_) | Value::BigInt(_) => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH HYBRID LIMIT parameter (must be > 0)",
                        got: value_variant_name(value),
                    });
                }
                other => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH HYBRID LIMIT parameter",
                        got: value_variant_name(other),
                    });
                }
            }
        } else {
            *limit
        };
        return Ok(QueryExpr::SearchCommand(SearchCommand::Hybrid {
            vector: vector.clone(),
            query: query.clone(),
            collection: collection.clone(),
            limit: bound_limit,
            limit_param: None,
        }));
    }

    if let QueryExpr::SearchCommand(SearchCommand::SpatialNearest {
        lat,
        lon,
        k,
        collection,
        column,
        k_param,
    }) = expr
    {
        let bound_k = if let Some(idx) = k_param {
            let value = params.get(*idx).ok_or(UserParamError::Arity {
                expected: idx + 1,
                got: params.len(),
            })?;
            match value {
                Value::Integer(n) if *n > 0 => *n as usize,
                Value::UnsignedInteger(n) if *n > 0 => *n as usize,
                Value::BigInt(n) if *n > 0 => *n as usize,
                Value::Integer(_) | Value::UnsignedInteger(_) | Value::BigInt(_) => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH SPATIAL NEAREST K parameter (must be > 0)",
                        got: value_variant_name(value),
                    });
                }
                other => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH SPATIAL NEAREST K parameter",
                        got: value_variant_name(other),
                    });
                }
            }
        } else {
            *k
        };
        return Ok(QueryExpr::SearchCommand(SearchCommand::SpatialNearest {
            lat: *lat,
            lon: *lon,
            k: bound_k,
            collection: collection.clone(),
            column: column.clone(),
            k_param: None,
        }));
    }

    if let QueryExpr::SearchCommand(SearchCommand::SpatialRadius {
        center_lat,
        center_lon,
        radius_km,
        collection,
        column,
        limit,
        limit_param,
    }) = expr
    {
        let bound_limit = if let Some(idx) = limit_param {
            let value = params.get(*idx).ok_or(UserParamError::Arity {
                expected: idx + 1,
                got: params.len(),
            })?;
            match value {
                Value::Integer(n) if *n > 0 => *n as usize,
                Value::UnsignedInteger(n) if *n > 0 => *n as usize,
                Value::BigInt(n) if *n > 0 => *n as usize,
                Value::Integer(_) | Value::UnsignedInteger(_) | Value::BigInt(_) => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH SPATIAL RADIUS LIMIT parameter (must be > 0)",
                        got: value_variant_name(value),
                    });
                }
                other => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH SPATIAL RADIUS LIMIT parameter",
                        got: value_variant_name(other),
                    });
                }
            }
        } else {
            *limit
        };
        return Ok(QueryExpr::SearchCommand(SearchCommand::SpatialRadius {
            center_lat: *center_lat,
            center_lon: *center_lon,
            radius_km: *radius_km,
            collection: collection.clone(),
            column: column.clone(),
            limit: bound_limit,
            limit_param: None,
        }));
    }

    if let QueryExpr::SearchCommand(SearchCommand::SpatialBbox {
        min_lat,
        min_lon,
        max_lat,
        max_lon,
        collection,
        column,
        limit,
        limit_param,
    }) = expr
    {
        let bound_limit = if let Some(idx) = limit_param {
            let value = params.get(*idx).ok_or(UserParamError::Arity {
                expected: idx + 1,
                got: params.len(),
            })?;
            match value {
                Value::Integer(n) if *n > 0 => *n as usize,
                Value::UnsignedInteger(n) if *n > 0 => *n as usize,
                Value::BigInt(n) if *n > 0 => *n as usize,
                Value::Integer(_) | Value::UnsignedInteger(_) | Value::BigInt(_) => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH SPATIAL BBOX LIMIT parameter (must be > 0)",
                        got: value_variant_name(value),
                    });
                }
                other => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH SPATIAL BBOX LIMIT parameter",
                        got: value_variant_name(other),
                    });
                }
            }
        } else {
            *limit
        };
        return Ok(QueryExpr::SearchCommand(SearchCommand::SpatialBbox {
            min_lat: *min_lat,
            min_lon: *min_lon,
            max_lat: *max_lat,
            max_lon: *max_lon,
            collection: collection.clone(),
            column: column.clone(),
            limit: bound_limit,
            limit_param: None,
        }));
    }

    if let QueryExpr::SearchCommand(SearchCommand::Text {
        query,
        collection,
        limit,
        fuzzy,
        limit_param,
    }) = expr
    {
        let bound_limit = if let Some(idx) = limit_param {
            let value = params.get(*idx).ok_or(UserParamError::Arity {
                expected: idx + 1,
                got: params.len(),
            })?;
            match value {
                Value::Integer(n) if *n > 0 => *n as usize,
                Value::UnsignedInteger(n) if *n > 0 => *n as usize,
                Value::BigInt(n) if *n > 0 => *n as usize,
                Value::Integer(_) | Value::UnsignedInteger(_) | Value::BigInt(_) => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH TEXT LIMIT parameter (must be > 0)",
                        got: value_variant_name(value),
                    });
                }
                other => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH TEXT LIMIT parameter",
                        got: value_variant_name(other),
                    });
                }
            }
        } else {
            *limit
        };
        return Ok(QueryExpr::SearchCommand(SearchCommand::Text {
            query: query.clone(),
            collection: collection.clone(),
            limit: bound_limit,
            fuzzy: *fuzzy,
            limit_param: None,
        }));
    }

    if let QueryExpr::SearchCommand(SearchCommand::Multimodal {
        query,
        collection,
        limit,
        limit_param,
    }) = expr
    {
        let bound_limit = if let Some(idx) = limit_param {
            let value = params.get(*idx).ok_or(UserParamError::Arity {
                expected: idx + 1,
                got: params.len(),
            })?;
            match value {
                Value::Integer(n) if *n > 0 => *n as usize,
                Value::UnsignedInteger(n) if *n > 0 => *n as usize,
                Value::BigInt(n) if *n > 0 => *n as usize,
                Value::Integer(_) | Value::UnsignedInteger(_) | Value::BigInt(_) => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH MULTIMODAL LIMIT parameter (must be > 0)",
                        got: value_variant_name(value),
                    });
                }
                other => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH MULTIMODAL LIMIT parameter",
                        got: value_variant_name(other),
                    });
                }
            }
        } else {
            *limit
        };
        return Ok(QueryExpr::SearchCommand(SearchCommand::Multimodal {
            query: query.clone(),
            collection: collection.clone(),
            limit: bound_limit,
            limit_param: None,
        }));
    }

    if let QueryExpr::SearchCommand(SearchCommand::Index {
        index,
        value,
        collection,
        limit,
        exact,
        limit_param,
    }) = expr
    {
        let bound_limit = if let Some(idx) = limit_param {
            let value = params.get(*idx).ok_or(UserParamError::Arity {
                expected: idx + 1,
                got: params.len(),
            })?;
            match value {
                Value::Integer(n) if *n > 0 => *n as usize,
                Value::UnsignedInteger(n) if *n > 0 => *n as usize,
                Value::BigInt(n) if *n > 0 => *n as usize,
                Value::Integer(_) | Value::UnsignedInteger(_) | Value::BigInt(_) => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH INDEX LIMIT parameter (must be > 0)",
                        got: value_variant_name(value),
                    });
                }
                other => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH INDEX LIMIT parameter",
                        got: value_variant_name(other),
                    });
                }
            }
        } else {
            *limit
        };
        return Ok(QueryExpr::SearchCommand(SearchCommand::Index {
            index: index.clone(),
            value: value.clone(),
            collection: collection.clone(),
            limit: bound_limit,
            exact: *exact,
            limit_param: None,
        }));
    }

    if let QueryExpr::SearchCommand(SearchCommand::Context {
        query,
        field,
        collection,
        limit,
        depth,
        limit_param,
    }) = expr
    {
        let bound_limit = if let Some(idx) = limit_param {
            let value = params.get(*idx).ok_or(UserParamError::Arity {
                expected: idx + 1,
                got: params.len(),
            })?;
            match value {
                Value::Integer(n) if *n > 0 => *n as usize,
                Value::UnsignedInteger(n) if *n > 0 => *n as usize,
                Value::BigInt(n) if *n > 0 => *n as usize,
                Value::Integer(_) | Value::UnsignedInteger(_) | Value::BigInt(_) => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH CONTEXT LIMIT parameter (must be > 0)",
                        got: value_variant_name(value),
                    });
                }
                other => {
                    return Err(UserParamError::TypeMismatch {
                        slot: "SEARCH CONTEXT LIMIT parameter",
                        got: value_variant_name(other),
                    });
                }
            }
        } else {
            *limit
        };
        return Ok(QueryExpr::SearchCommand(SearchCommand::Context {
            query: query.clone(),
            field: field.clone(),
            collection: collection.clone(),
            limit: bound_limit,
            depth: *depth,
            limit_param: None,
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

    if let QueryExpr::Update(update) = expr {
        let mut bound = update.clone();
        let assignment_exprs = bound
            .assignment_exprs
            .into_iter()
            .map(|(column, expr)| Ok((column, substitute_params_in_expr(expr, params)?)))
            .collect::<Result<Vec<_>, UserParamError>>()?;
        let assignments = assignment_exprs
            .iter()
            .filter_map(|(column, expr)| {
                fold_expr_to_value(expr.clone())
                    .ok()
                    .map(|value| (column.clone(), value))
            })
            .collect();
        let where_expr = bound
            .where_expr
            .map(|expr| substitute_params_in_expr(expr, params))
            .transpose()?;
        let filter = where_expr.as_ref().map(expr_to_filter);
        bound.assignment_exprs = assignment_exprs;
        bound.assignments = assignments;
        bound.where_expr = where_expr;
        bound.filter = filter;
        return Ok(QueryExpr::Update(bound));
    }

    if let QueryExpr::Delete(delete) = expr {
        let mut bound = delete.clone();
        let where_expr = bound
            .where_expr
            .map(|expr| substitute_params_in_expr(expr, params))
            .transpose()?;
        let filter = where_expr.as_ref().map(expr_to_filter);
        bound.where_expr = where_expr;
        bound.filter = filter;
        return Ok(QueryExpr::Delete(bound));
    }

    if let QueryExpr::Ask(ask) = expr {
        let Some(idx) = ask.question_param else {
            return Ok(QueryExpr::Ask(ask.clone()));
        };
        let value = params.get(idx).ok_or(UserParamError::Arity {
            expected: idx + 1,
            got: params.len(),
        })?;
        let question = match value {
            Value::Text(s) => s.to_string(),
            other => {
                return Err(UserParamError::TypeMismatch {
                    slot: "ASK question parameter",
                    got: value_variant_name(other),
                });
            }
        };
        let mut bound = ask.clone();
        bound.question = question;
        bound.question_param = None;
        return Ok(QueryExpr::Ask(bound));
    }

    // SELECT LIMIT / OFFSET $N — the planner's Expr-tree binder doesn't
    // see these slots (they live on TableQuery, not inside any Expr).
    // Run the Expr-tree bind first, then substitute the non-Expr slots
    // post-hoc. Mirrors the SearchCommand::Similar pattern above.
    if let QueryExpr::Table(table) = expr {
        if table.limit_param.is_some() || table.offset_param.is_some() {
            let bound_inner =
                bind_user_param_query(expr, params).ok_or(UserParamError::UnsupportedShape)?;
            let mut bound_table = match bound_inner {
                QueryExpr::Table(t) => t,
                _ => return Err(UserParamError::UnsupportedShape),
            };
            if let Some(idx) = table.limit_param {
                let value = params.get(idx).ok_or(UserParamError::Arity {
                    expected: idx + 1,
                    got: params.len(),
                })?;
                let n = match value {
                    Value::Integer(n) if *n > 0 => *n as u64,
                    Value::UnsignedInteger(n) if *n > 0 => *n,
                    Value::BigInt(n) if *n > 0 => *n as u64,
                    Value::Integer(_) | Value::UnsignedInteger(_) | Value::BigInt(_) => {
                        return Err(UserParamError::TypeMismatch {
                            slot: "SELECT LIMIT parameter (must be > 0)",
                            got: value_variant_name(value),
                        });
                    }
                    other => {
                        return Err(UserParamError::TypeMismatch {
                            slot: "SELECT LIMIT parameter",
                            got: value_variant_name(other),
                        });
                    }
                };
                bound_table.limit = Some(n);
                bound_table.limit_param = None;
            }
            if let Some(idx) = table.offset_param {
                let value = params.get(idx).ok_or(UserParamError::Arity {
                    expected: idx + 1,
                    got: params.len(),
                })?;
                let n = match value {
                    Value::Integer(n) if *n >= 0 => *n as u64,
                    Value::UnsignedInteger(n) => *n,
                    Value::BigInt(n) if *n >= 0 => *n as u64,
                    Value::Integer(_) | Value::BigInt(_) => {
                        return Err(UserParamError::TypeMismatch {
                            slot: "SELECT OFFSET parameter (must be >= 0)",
                            got: value_variant_name(value),
                        });
                    }
                    other => {
                        return Err(UserParamError::TypeMismatch {
                            slot: "SELECT OFFSET parameter",
                            got: value_variant_name(other),
                        });
                    }
                };
                bound_table.offset = Some(n);
                bound_table.offset_param = None;
            }
            return Ok(QueryExpr::Table(bound_table));
        }
    }

    bind_user_param_query(expr, params).ok_or(UserParamError::UnsupportedShape)
}

/// One-shot helper matching the parameter-contract ADR wording.
pub fn bind_parameters(expr: &QueryExpr, params: &[Value]) -> Result<QueryExpr, BindError> {
    bind(expr, params)
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
        QueryExpr::Update(q) => {
            for (_, e) in &q.assignment_exprs {
                visit_expr(e, visit);
            }
            if let Some(e) = &q.where_expr {
                visit_expr(e, visit);
            }
        }
        QueryExpr::Delete(q) => {
            if let Some(e) = &q.where_expr {
                visit_expr(e, visit);
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
        Expr::Case {
            branches, else_, ..
        } => {
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
        Expr::Subquery { .. } => {}
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
    fn scan_parameters_reports_index_and_span() {
        let sql = "SELECT * FROM users WHERE id = $1 AND name = $2";
        let q = parse(sql);
        let params = scan_parameters(&q);
        assert_eq!(
            params.iter().map(|param| param.index).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(
            sql[params[0].span.start.offset as usize..params[0].span.end.offset as usize].trim(),
            "$1"
        );
        assert_eq!(
            sql[params[1].span.start.offset as usize..params[1].span.end.offset as usize].trim(),
            "$2"
        );
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
    fn bind_substitutes_question_numbered_param() {
        let q = parse("SELECT * FROM users WHERE id = ?1 AND name = ?2");
        let bound = bind(&q, &[Value::Integer(42), Value::text("Alice")]).unwrap();
        let QueryExpr::Table(t) = bound else {
            panic!("expected Table");
        };
        let mut literals: Vec<Value> = Vec::new();
        visit_expr(&t.where_expr.unwrap(), &mut |e| {
            if let Expr::Literal { value, .. } = e {
                literals.push(value.clone());
            }
        });
        assert!(literals.iter().any(|v| matches!(v, Value::Integer(42))));
        assert!(literals
            .iter()
            .any(|v| matches!(v, Value::Text(s) if s.as_ref() == "Alice")));
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
    fn bind_search_similar_limit_param() {
        // Issue #361: `LIMIT $N` binds an integer parameter.
        let q = parse("SEARCH SIMILAR [0.1, 0.2] COLLECTION embeddings LIMIT $1");
        let bound = bind(&q, &[Value::Integer(25)]).unwrap();
        let QueryExpr::SearchCommand(SearchCommand::Similar {
            limit,
            limit_param,
            min_score_param,
            ..
        }) = bound
        else {
            panic!("expected SearchCommand::Similar");
        };
        assert_eq!(limit, 25);
        assert_eq!(limit_param, None, "limit_param must be cleared post-bind");
        assert_eq!(min_score_param, None);
    }

    #[test]
    fn bind_search_similar_min_score_param() {
        let q = parse("SEARCH SIMILAR [0.1, 0.2] COLLECTION embeddings MIN_SCORE $1");
        let bound = bind(&q, &[Value::Float(0.42)]).unwrap();
        let QueryExpr::SearchCommand(SearchCommand::Similar {
            min_score,
            min_score_param,
            ..
        }) = bound
        else {
            panic!("expected SearchCommand::Similar");
        };
        assert!((min_score - 0.42_f32).abs() < 1e-6);
        assert_eq!(min_score_param, None);
    }

    #[test]
    fn bind_search_similar_limit_and_min_score_together() {
        let q = parse("SEARCH SIMILAR $1 COLLECTION embeddings LIMIT $2 MIN_SCORE $3");
        let bound = bind(
            &q,
            &[
                Value::Vector(vec![0.1, 0.2]),
                Value::Integer(7),
                Value::Float(0.9),
            ],
        )
        .unwrap();
        let QueryExpr::SearchCommand(SearchCommand::Similar {
            limit,
            min_score,
            vector,
            vector_param,
            limit_param,
            min_score_param,
            ..
        }) = bound
        else {
            panic!("expected SearchCommand::Similar");
        };
        assert_eq!(vector, vec![0.1_f32, 0.2]);
        assert_eq!(limit, 7);
        assert!((min_score - 0.9_f32).abs() < 1e-6);
        assert_eq!(vector_param, None);
        assert_eq!(limit_param, None);
        assert_eq!(min_score_param, None);
    }

    #[test]
    fn bind_ask_question_param() {
        let q = parse("ASK $1 USING openai LIMIT 1");
        let bound = bind(&q, &[Value::text("why did incident FDD-12313 fail?")]).unwrap();
        let QueryExpr::Ask(ask) = bound else {
            panic!("expected Ask");
        };
        assert_eq!(ask.question, "why did incident FDD-12313 fail?");
        assert_eq!(ask.question_param, None);
        assert_eq!(ask.provider.as_deref(), Some("openai"));
    }

    #[test]
    fn bind_ask_question_param_rejects_non_text() {
        let q = parse("ASK $1 USING openai LIMIT 1");
        let err = bind(&q, &[Value::Integer(42)]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "ASK question parameter",
                got: "integer"
            }
        ));
    }

    #[test]
    fn bind_search_similar_limit_rejects_non_integer() {
        let q = parse("SEARCH SIMILAR [0.1] COLLECTION e LIMIT $1");
        let err = bind(&q, &[Value::text("five")]).unwrap_err();
        assert!(
            matches!(
                err,
                UserParamError::TypeMismatch {
                    slot: "SEARCH SIMILAR LIMIT parameter",
                    got: "text"
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn bind_search_similar_limit_rejects_zero_or_negative() {
        let q = parse("SEARCH SIMILAR [0.1] COLLECTION e LIMIT $1");
        let err = bind(&q, &[Value::Integer(0)]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH SIMILAR LIMIT parameter (must be > 0)",
                ..
            }
        ));
        let err = bind(&q, &[Value::Integer(-3)]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH SIMILAR LIMIT parameter (must be > 0)",
                ..
            }
        ));
    }

    #[test]
    fn bind_search_similar_min_score_rejects_non_numeric() {
        let q = parse("SEARCH SIMILAR [0.1] COLLECTION e MIN_SCORE $1");
        let err = bind(&q, &[Value::Vector(vec![1.0])]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH SIMILAR MIN_SCORE parameter",
                got: "vector"
            }
        ));
    }

    // Note: `?` placeholder at LIMIT/MIN_SCORE is correctly handled by
    // `parse_param_slot`, but `parse_multi` routes any `?`-bearing input
    // to the SPARQL frontend (see modes::detect). Exercising `?` for
    // non-Expr slots will land alongside the SPARQL detector tightening
    // tracked separately. The Dollar path covers the same code below.

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
    fn bind_parameters_substitutes_all_wire_value_variants() {
        let q = parse(
            "INSERT INTO value_params \
             (n, ok, count, score, name, payload, dense, body, seen_at, ident) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        );
        let uuid = [1_u8; 16];
        let params = vec![
            Value::Null,
            Value::Boolean(true),
            Value::Integer(42),
            Value::Float(1.5),
            Value::text("alice"),
            Value::Blob(vec![0, 1, 2]),
            Value::Vector(vec![0.25, 0.5]),
            Value::Json(br#"{"a":1}"#.to_vec()),
            Value::Timestamp(1_700_000_000),
            Value::Uuid(uuid),
        ];
        let bound = bind_parameters(&q, &params).unwrap();
        let QueryExpr::Insert(insert) = bound else {
            panic!("expected Insert");
        };
        assert_eq!(insert.values, vec![params]);
    }

    #[test]
    fn bind_parameters_reuses_duplicate_index() {
        let q = parse("SELECT * FROM users WHERE id = $1 OR manager_id = $1");
        let bound = bind_parameters(&q, &[Value::Integer(7)]).unwrap();
        let QueryExpr::Table(table) = bound else {
            panic!("expected Table");
        };
        assert!(table.where_expr.is_some());
        assert_eq!(
            collect_indices(&QueryExpr::Table(table)),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn bind_search_hybrid_limit_param() {
        // Issue #361: `SEARCH HYBRID ... LIMIT $N` binds integer parameter.
        let q = parse("SEARCH HYBRID SIMILAR [0.1, 0.2] TEXT 'q' COLLECTION svc LIMIT $1");
        let bound = bind(&q, &[Value::Integer(30)]).unwrap();
        let QueryExpr::SearchCommand(SearchCommand::Hybrid {
            limit, limit_param, ..
        }) = bound
        else {
            panic!("expected SearchCommand::Hybrid");
        };
        assert_eq!(limit, 30);
        assert_eq!(limit_param, None, "limit_param must be cleared post-bind");
    }

    #[test]
    fn bind_search_hybrid_k_param() {
        // `K $N` is an alias for LIMIT in HYBRID.
        let q = parse("SEARCH HYBRID TEXT 'q' COLLECTION svc K $1");
        let bound = bind(&q, &[Value::Integer(7)]).unwrap();
        let QueryExpr::SearchCommand(SearchCommand::Hybrid {
            limit, limit_param, ..
        }) = bound
        else {
            panic!("expected SearchCommand::Hybrid");
        };
        assert_eq!(limit, 7);
        assert_eq!(limit_param, None);
    }

    #[test]
    fn bind_search_hybrid_limit_rejects_non_integer() {
        let q = parse("SEARCH HYBRID TEXT 'q' COLLECTION svc LIMIT $1");
        let err = bind(&q, &[Value::text("five")]).unwrap_err();
        assert!(
            matches!(
                err,
                UserParamError::TypeMismatch {
                    slot: "SEARCH HYBRID LIMIT parameter",
                    got: "text"
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn bind_search_hybrid_limit_rejects_zero() {
        let q = parse("SEARCH HYBRID TEXT 'q' COLLECTION svc LIMIT $1");
        let err = bind(&q, &[Value::Integer(0)]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH HYBRID LIMIT parameter (must be > 0)",
                ..
            }
        ));
    }

    #[test]
    fn bind_search_spatial_nearest_k_param() {
        // Issue #361: `SEARCH SPATIAL NEAREST ... K $N` binds an integer.
        let q =
            parse("SEARCH SPATIAL NEAREST 40.7128 74.0060 K $1 COLLECTION sites COLUMN location");
        let bound = bind(&q, &[Value::Integer(7)]).unwrap();
        let QueryExpr::SearchCommand(SearchCommand::SpatialNearest { k, k_param, .. }) = bound
        else {
            panic!("expected SpatialNearest");
        };
        assert_eq!(k, 7);
        assert_eq!(k_param, None, "k_param must be cleared post-bind");
    }

    #[test]
    fn bind_search_spatial_nearest_k_rejects_zero() {
        let q =
            parse("SEARCH SPATIAL NEAREST 40.7128 74.0060 K $1 COLLECTION sites COLUMN location");
        let err = bind(&q, &[Value::Integer(0)]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH SPATIAL NEAREST K parameter (must be > 0)",
                ..
            }
        ));
    }

    #[test]
    fn bind_search_spatial_nearest_k_rejects_non_integer() {
        let q =
            parse("SEARCH SPATIAL NEAREST 40.7128 74.0060 K $1 COLLECTION sites COLUMN location");
        let err = bind(&q, &[Value::text("five")]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH SPATIAL NEAREST K parameter",
                got: "text"
            }
        ));
    }

    #[test]
    fn bind_search_text_limit_param() {
        // Issue #361: `SEARCH TEXT ... LIMIT $N` binds an integer.
        let q = parse("SEARCH TEXT 'hello' COLLECTION docs LIMIT $1");
        let bound = bind(&q, &[Value::Integer(15)]).unwrap();
        let QueryExpr::SearchCommand(SearchCommand::Text {
            limit, limit_param, ..
        }) = bound
        else {
            panic!("expected SearchCommand::Text");
        };
        assert_eq!(limit, 15);
        assert_eq!(limit_param, None, "limit_param must be cleared post-bind");
    }

    #[test]
    fn bind_search_text_limit_rejects_zero() {
        let q = parse("SEARCH TEXT 'hello' COLLECTION docs LIMIT $1");
        let err = bind(&q, &[Value::Integer(0)]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH TEXT LIMIT parameter (must be > 0)",
                ..
            }
        ));
    }

    #[test]
    fn bind_search_text_limit_rejects_non_integer() {
        let q = parse("SEARCH TEXT 'hello' COLLECTION docs LIMIT $1");
        let err = bind(&q, &[Value::text("five")]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH TEXT LIMIT parameter",
                got: "text"
            }
        ));
    }

    #[test]
    fn bind_search_multimodal_limit_param() {
        // Issue #361: `SEARCH MULTIMODAL ... LIMIT $N` binds an integer.
        let q = parse("SEARCH MULTIMODAL 'user:123' COLLECTION people LIMIT $1");
        let bound = bind(&q, &[Value::Integer(40)]).unwrap();
        let QueryExpr::SearchCommand(SearchCommand::Multimodal {
            limit, limit_param, ..
        }) = bound
        else {
            panic!("expected SearchCommand::Multimodal");
        };
        assert_eq!(limit, 40);
        assert_eq!(limit_param, None, "limit_param must be cleared post-bind");
    }

    #[test]
    fn bind_search_multimodal_limit_rejects_zero() {
        let q = parse("SEARCH MULTIMODAL 'k' COLLECTION people LIMIT $1");
        let err = bind(&q, &[Value::Integer(0)]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH MULTIMODAL LIMIT parameter (must be > 0)",
                ..
            }
        ));
    }

    #[test]
    fn bind_search_multimodal_limit_rejects_non_integer() {
        let q = parse("SEARCH MULTIMODAL 'k' COLLECTION people LIMIT $1");
        let err = bind(&q, &[Value::text("five")]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH MULTIMODAL LIMIT parameter",
                got: "text"
            }
        ));
    }

    #[test]
    fn bind_search_index_limit_param() {
        // Issue #361: `SEARCH INDEX ... LIMIT $N` binds an integer.
        let q = parse("SEARCH INDEX cpf VALUE '000.000.000-00' COLLECTION people LIMIT $1");
        let bound = bind(&q, &[Value::Integer(50)]).unwrap();
        let QueryExpr::SearchCommand(SearchCommand::Index {
            limit, limit_param, ..
        }) = bound
        else {
            panic!("expected SearchCommand::Index");
        };
        assert_eq!(limit, 50);
        assert_eq!(limit_param, None, "limit_param must be cleared post-bind");
    }

    #[test]
    fn bind_search_index_limit_rejects_zero() {
        let q = parse("SEARCH INDEX cpf VALUE 'x' COLLECTION people LIMIT $1");
        let err = bind(&q, &[Value::Integer(0)]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH INDEX LIMIT parameter (must be > 0)",
                ..
            }
        ));
    }

    #[test]
    fn bind_search_index_limit_rejects_non_integer() {
        let q = parse("SEARCH INDEX cpf VALUE 'x' COLLECTION people LIMIT $1");
        let err = bind(&q, &[Value::text("five")]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH INDEX LIMIT parameter",
                got: "text"
            }
        ));
    }

    #[test]
    fn bind_search_context_limit_param() {
        // Issue #361: `SEARCH CONTEXT ... LIMIT $N` binds an integer.
        let q = parse("SEARCH CONTEXT 'hello' COLLECTION docs LIMIT $1");
        let bound = bind(&q, &[Value::Integer(60)]).unwrap();
        let QueryExpr::SearchCommand(SearchCommand::Context {
            limit, limit_param, ..
        }) = bound
        else {
            panic!("expected SearchCommand::Context");
        };
        assert_eq!(limit, 60);
        assert_eq!(limit_param, None, "limit_param must be cleared post-bind");
    }

    #[test]
    fn bind_search_context_limit_rejects_zero() {
        let q = parse("SEARCH CONTEXT 'hello' COLLECTION docs LIMIT $1");
        let err = bind(&q, &[Value::Integer(0)]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH CONTEXT LIMIT parameter (must be > 0)",
                ..
            }
        ));
    }

    #[test]
    fn bind_search_context_limit_rejects_non_integer() {
        let q = parse("SEARCH CONTEXT 'hello' COLLECTION docs LIMIT $1");
        let err = bind(&q, &[Value::text("five")]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH CONTEXT LIMIT parameter",
                got: "text"
            }
        ));
    }

    #[test]
    fn bind_search_spatial_radius_limit_param() {
        // Issue #361: `SEARCH SPATIAL RADIUS ... LIMIT $N` binds an integer.
        let q = parse(
            "SEARCH SPATIAL RADIUS 48.8566 2.3522 10.0 COLLECTION sites COLUMN location LIMIT $1",
        );
        let bound = bind(&q, &[Value::Integer(50)]).unwrap();
        let QueryExpr::SearchCommand(SearchCommand::SpatialRadius {
            limit, limit_param, ..
        }) = bound
        else {
            panic!("expected SearchCommand::SpatialRadius");
        };
        assert_eq!(limit, 50);
        assert_eq!(limit_param, None, "limit_param must be cleared post-bind");
    }

    #[test]
    fn bind_search_spatial_radius_limit_rejects_zero() {
        let q = parse(
            "SEARCH SPATIAL RADIUS 48.8566 2.3522 10.0 COLLECTION sites COLUMN location LIMIT $1",
        );
        let err = bind(&q, &[Value::Integer(0)]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH SPATIAL RADIUS LIMIT parameter (must be > 0)",
                ..
            }
        ));
    }

    #[test]
    fn bind_search_spatial_radius_limit_rejects_non_integer() {
        let q = parse(
            "SEARCH SPATIAL RADIUS 48.8566 2.3522 10.0 COLLECTION sites COLUMN location LIMIT $1",
        );
        let err = bind(&q, &[Value::text("five")]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH SPATIAL RADIUS LIMIT parameter",
                got: "text"
            }
        ));
    }

    #[test]
    fn bind_search_spatial_bbox_limit_param() {
        // Issue #361: `SEARCH SPATIAL BBOX ... LIMIT $N` binds an integer.
        let q =
            parse("SEARCH SPATIAL BBOX 0.0 0.0 1.0 1.0 COLLECTION sites COLUMN location LIMIT $1");
        let bound = bind(&q, &[Value::Integer(50)]).unwrap();
        let QueryExpr::SearchCommand(SearchCommand::SpatialBbox {
            limit, limit_param, ..
        }) = bound
        else {
            panic!("expected SearchCommand::SpatialBbox");
        };
        assert_eq!(limit, 50);
        assert_eq!(limit_param, None, "limit_param must be cleared post-bind");
    }

    #[test]
    fn bind_search_spatial_bbox_limit_rejects_zero() {
        let q =
            parse("SEARCH SPATIAL BBOX 0.0 0.0 1.0 1.0 COLLECTION sites COLUMN location LIMIT $1");
        let err = bind(&q, &[Value::Integer(0)]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH SPATIAL BBOX LIMIT parameter (must be > 0)",
                ..
            }
        ));
    }

    #[test]
    fn bind_search_spatial_bbox_limit_rejects_non_integer() {
        let q =
            parse("SEARCH SPATIAL BBOX 0.0 0.0 1.0 1.0 COLLECTION sites COLUMN location LIMIT $1");
        let err = bind(&q, &[Value::text("five")]).unwrap_err();
        assert!(matches!(
            err,
            UserParamError::TypeMismatch {
                slot: "SEARCH SPATIAL BBOX LIMIT parameter",
                got: "text"
            }
        ));
    }

    #[test]
    fn bind_search_similar_text_param() {
        // Issue #361: `SEARCH SIMILAR TEXT $N` binds a Value::Text into
        // the text slot. The embedding pipeline reads `text` downstream.
        let q = parse("SEARCH SIMILAR TEXT $1 COLLECTION docs LIMIT 5 USING openai");
        let bound = bind(&q, &[Value::text("find vulnerabilities")]).unwrap();
        let QueryExpr::SearchCommand(SearchCommand::Similar {
            vector,
            text,
            text_param,
            collection,
            limit,
            provider,
            ..
        }) = bound
        else {
            panic!("expected SearchCommand::Similar");
        };
        assert!(vector.is_empty());
        assert_eq!(text.as_deref(), Some("find vulnerabilities"));
        assert_eq!(text_param, None, "text_param must be cleared post-bind");
        assert_eq!(collection, "docs");
        assert_eq!(limit, 5);
        assert_eq!(provider.as_deref(), Some("openai"));
    }

    #[test]
    fn bind_search_similar_text_rejects_non_text() {
        let q = parse("SEARCH SIMILAR TEXT $1 COLLECTION docs");
        let err = bind(&q, &[Value::Integer(42)]).unwrap_err();
        assert!(
            matches!(
                err,
                UserParamError::TypeMismatch {
                    slot: "SEARCH SIMILAR TEXT parameter",
                    got: "integer"
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn bind_search_similar_text_with_limit_param() {
        // TEXT $1 + LIMIT $2 — verify both non-Expr param slots bind
        // together without cross-talk.
        let q = parse("SEARCH SIMILAR TEXT $1 COLLECTION docs LIMIT $2");
        let bound = bind(&q, &[Value::text("hello"), Value::Integer(11)]).unwrap();
        let QueryExpr::SearchCommand(SearchCommand::Similar {
            text,
            text_param,
            limit,
            limit_param,
            ..
        }) = bound
        else {
            panic!("expected SearchCommand::Similar");
        };
        assert_eq!(text.as_deref(), Some("hello"));
        assert_eq!(text_param, None);
        assert_eq!(limit, 11);
        assert_eq!(limit_param, None);
    }

    #[test]
    fn bind_insert_values_with_vector_param() {
        // Issue #355 INSERT half: $1 in VALUES is bound to a Value::Vector
        // and surfaces in both `value_exprs` (as a Literal) and `values`.
        let q = parse("INSERT INTO embeddings (dense, content) VALUES ($1, $2)");
        let vec = Value::Vector(vec![0.1, 0.2, 0.3]);
        let bound = bind(&q, &[vec.clone(), Value::text("doc text")]).unwrap();
        let QueryExpr::Insert(insert) = bound else {
            panic!("expected Insert");
        };
        assert_eq!(insert.values.len(), 1);
        assert_eq!(insert.values[0].len(), 2);
        assert!(
            matches!(insert.values[0][0], Value::Vector(ref v) if v == &vec![0.1f32, 0.2, 0.3])
        );
        assert!(matches!(insert.values[0][1], Value::Text(ref s) if s.as_ref() == "doc text"));
        // value_exprs row 0 col 0 is now a Literal carrying the vector.
        let row0 = &insert.value_exprs[0];
        assert!(matches!(
            &row0[0],
            Expr::Literal {
                value: Value::Vector(_),
                ..
            }
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
    fn bind_update_assignments_and_where_params() {
        let q = parse("UPDATE users SET age = $1, active = $2 WHERE name = $3");
        let bound = bind(
            &q,
            &[
                Value::Integer(31),
                Value::Boolean(true),
                Value::text("Alice"),
            ],
        )
        .unwrap();
        let QueryExpr::Update(update) = bound else {
            panic!("expected Update");
        };
        assert_eq!(update.assignments.len(), 2);
        assert!(matches!(update.assignments[0].1, Value::Integer(31)));
        assert!(matches!(update.assignments[1].1, Value::Boolean(true)));
        assert!(update.where_expr.is_some());
        assert!(update.filter.is_some());
    }

    #[test]
    fn bind_delete_where_param() {
        let q = parse("DELETE FROM users WHERE active = $1");
        let bound = bind(&q, &[Value::Boolean(false)]).unwrap();
        let QueryExpr::Delete(delete) = bound else {
            panic!("expected Delete");
        };
        assert!(delete.where_expr.is_some());
        assert!(delete.filter.is_some());
    }

    #[test]
    fn bind_select_limit_param() {
        let q = parse("SELECT * FROM users LIMIT $1");
        let bound = bind(&q, &[Value::Integer(7)]).unwrap();
        let QueryExpr::Table(t) = bound else {
            panic!("expected Table");
        };
        assert_eq!(t.limit, Some(7));
        assert_eq!(t.limit_param, None, "limit_param must be cleared post-bind");
        assert_eq!(t.offset, None);
        assert_eq!(t.offset_param, None);
    }

    #[test]
    fn bind_select_offset_param() {
        let q = parse("SELECT * FROM users LIMIT 10 OFFSET $1");
        let bound = bind(&q, &[Value::Integer(20)]).unwrap();
        let QueryExpr::Table(t) = bound else {
            panic!("expected Table");
        };
        assert_eq!(t.limit, Some(10));
        assert_eq!(t.offset, Some(20));
        assert_eq!(t.offset_param, None);
    }

    #[test]
    fn bind_select_limit_and_offset_params_together() {
        let q = parse("SELECT * FROM users WHERE id = $1 LIMIT $2 OFFSET $3");
        let bound = bind(
            &q,
            &[Value::Integer(5), Value::Integer(10), Value::Integer(20)],
        )
        .unwrap();
        let QueryExpr::Table(t) = bound else {
            panic!("expected Table");
        };
        assert_eq!(t.limit, Some(10));
        assert_eq!(t.offset, Some(20));
        assert_eq!(t.limit_param, None);
        assert_eq!(t.offset_param, None);
        // WHERE id = $1 → Expr-tree bind also ran.
        assert!(t.where_expr.is_some());
    }

    #[test]
    fn bind_select_offset_zero_is_valid() {
        let q = parse("SELECT * FROM users LIMIT 10 OFFSET $1");
        let bound = bind(&q, &[Value::Integer(0)]).unwrap();
        let QueryExpr::Table(t) = bound else {
            panic!("expected Table");
        };
        assert_eq!(t.offset, Some(0));
    }

    #[test]
    fn bind_select_limit_rejects_zero() {
        let q = parse("SELECT * FROM users LIMIT $1");
        let err = bind(&q, &[Value::Integer(0)]).unwrap_err();
        assert!(
            matches!(
                err,
                UserParamError::TypeMismatch {
                    slot: "SELECT LIMIT parameter (must be > 0)",
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn bind_select_limit_rejects_non_integer() {
        let q = parse("SELECT * FROM users LIMIT $1");
        let err = bind(&q, &[Value::text("ten")]).unwrap_err();
        assert!(
            matches!(
                err,
                UserParamError::TypeMismatch {
                    slot: "SELECT LIMIT parameter",
                    got: "text"
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn bind_select_offset_rejects_negative() {
        let q = parse("SELECT * FROM users LIMIT 10 OFFSET $1");
        let err = bind(&q, &[Value::Integer(-1)]).unwrap_err();
        assert!(
            matches!(
                err,
                UserParamError::TypeMismatch {
                    slot: "SELECT OFFSET parameter (must be >= 0)",
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn bind_no_params_is_noop() {
        let q = parse("SELECT * FROM users");
        let bound = bind(&q, &[]).unwrap();
        assert!(matches!(bound, QueryExpr::Table(_)));
    }
}
