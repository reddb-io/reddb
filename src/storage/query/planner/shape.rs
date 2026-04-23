use crate::storage::engine::vector_metadata::{MetadataFilter, MetadataValue};
use crate::storage::query::ast::{
    Expr, FusionStrategy, GraphPattern, GraphQuery, HybridQuery, JoinQuery, NodePattern,
    NodeSelector, OrderByClause, PathQuery, Projection, PropertyFilter, QueryExpr, SelectItem,
    TableQuery, TableSource, VectorQuery, VectorSource,
};
use crate::storage::query::sql_lowering::{
    expr_to_filter, filter_to_expr, projection_from_literal,
};
use crate::storage::schema::Value;

const PROJECTION_PARAM_PREFIX: &str = "__shape_projection_param__:";
const STRING_PARAM_PREFIX: &str = "__shape_string_param__:";
const VALUE_PARAM_PREFIX: &str = "__shape_value_param__:";
const ROW_SELECTOR_TABLE_PREFIX: &str = "__shape_row_selector__:";
const METADATA_VALUE_PARAM_PREFIX: &str = "__shape_metadata_value_param__:";
const VECTOR_TEXT_PARAM_PREFIX: &str = "__shape_vector_text_param__:";
const VECTOR_REF_ID_PREFIX: &str = "__shape_vector_ref_id__:";
const FLOAT32_PARAM_BITS_BASE: u32 = 0x7fc0_0000;
const FLOAT64_PARAM_BITS_BASE: u64 = 0x7ff8_0000_0000_0000;
const U32_PARAM_BASE: u32 = 0xfff0_0000;

#[derive(Debug, Clone)]
pub struct ParameterizedQuery {
    pub shape: QueryExpr,
    pub parameter_count: usize,
}

pub fn parameterize_query_expr(expr: &QueryExpr) -> Option<ParameterizedQuery> {
    let mut next_index = 0usize;
    let shape = parameterize_query_expr_inner(expr, &mut next_index)?;
    Some(ParameterizedQuery {
        shape,
        parameter_count: next_index,
    })
}

pub fn bind_parameterized_query(
    expr: &QueryExpr,
    binds: &[Value],
    parameter_count: usize,
) -> Option<QueryExpr> {
    if binds.len() != parameter_count {
        return None;
    }
    bind_query_expr_inner(expr, binds)
}

fn parameterize_query_expr_inner(expr: &QueryExpr, next_index: &mut usize) -> Option<QueryExpr> {
    match expr {
        QueryExpr::Table(query) => Some(QueryExpr::Table(parameterize_table_query(
            query, next_index,
        )?)),
        QueryExpr::Join(query) => {
            Some(QueryExpr::Join(parameterize_join_query(query, next_index)?))
        }
        QueryExpr::Graph(query) => Some(QueryExpr::Graph(parameterize_graph_query(
            query, next_index,
        )?)),
        QueryExpr::Path(query) => {
            Some(QueryExpr::Path(parameterize_path_query(query, next_index)?))
        }
        QueryExpr::Vector(query) => Some(QueryExpr::Vector(parameterize_vector_query(
            query, next_index,
        )?)),
        QueryExpr::Hybrid(query) => Some(QueryExpr::Hybrid(parameterize_hybrid_query(
            query, next_index,
        )?)),
        _ => None,
    }
}

fn parameterize_table_query(query: &TableQuery, next_index: &mut usize) -> Option<TableQuery> {
    let source = match &query.source {
        Some(TableSource::Name(name)) => Some(TableSource::Name(name.clone())),
        Some(TableSource::Subquery(inner)) => Some(TableSource::Subquery(Box::new(
            parameterize_query_expr_inner(inner, next_index)?,
        ))),
        None => None,
    };

    let select_items = query
        .select_items
        .iter()
        .map(|item| parameterize_select_item(item, next_index))
        .collect::<Option<Vec<_>>>()?;

    let where_expr = query
        .where_expr
        .as_ref()
        .map(|expr| parameterize_expr(expr, next_index))
        .or_else(|| {
            query
                .filter
                .as_ref()
                .map(|filter| parameterize_expr(&filter_to_expr(filter), next_index))
        });

    let group_by_exprs = if !query.group_by_exprs.is_empty() {
        query
            .group_by_exprs
            .iter()
            .map(|expr| parameterize_expr(expr, next_index))
            .collect()
    } else {
        Vec::new()
    };

    let having_expr = query
        .having_expr
        .as_ref()
        .map(|expr| parameterize_expr(expr, next_index))
        .or_else(|| {
            query
                .having
                .as_ref()
                .map(|filter| parameterize_expr(&filter_to_expr(filter), next_index))
        });

    let order_by = query
        .order_by
        .iter()
        .map(|clause| parameterize_order_by(clause, next_index))
        .collect::<Option<Vec<_>>>()?;

    Some(TableQuery {
        table: query.table.clone(),
        source,
        alias: query.alias.clone(),
        select_items,
        columns: Vec::new(),
        where_expr,
        filter: None,
        group_by_exprs,
        group_by: Vec::new(),
        having_expr,
        having: None,
        order_by,
        limit: query.limit,
        offset: query.offset,
        expand: query.expand.clone(),
        as_of: query.as_of.clone(),
    })
}

fn parameterize_select_item(item: &SelectItem, next_index: &mut usize) -> Option<SelectItem> {
    match item {
        SelectItem::Wildcard => Some(SelectItem::Wildcard),
        SelectItem::Expr { expr, alias } => Some(SelectItem::Expr {
            expr: parameterize_expr(expr, next_index),
            alias: alias.clone(),
        }),
    }
}

fn parameterize_order_by(clause: &OrderByClause, next_index: &mut usize) -> Option<OrderByClause> {
    Some(OrderByClause {
        field: clause.field.clone(),
        expr: clause
            .expr
            .as_ref()
            .map(|expr| parameterize_expr(expr, next_index)),
        ascending: clause.ascending,
        nulls_first: clause.nulls_first,
    })
}

fn parameterize_expr(expr: &Expr, next_index: &mut usize) -> Expr {
    match expr {
        Expr::Literal { value, span } => {
            let index = *next_index;
            *next_index += 1;
            let _ = value;
            Expr::Parameter { index, span: *span }
        }
        Expr::Column { .. } | Expr::Parameter { .. } => expr.clone(),
        Expr::BinaryOp { op, lhs, rhs, span } => Expr::BinaryOp {
            op: *op,
            lhs: Box::new(parameterize_expr(lhs, next_index)),
            rhs: Box::new(parameterize_expr(rhs, next_index)),
            span: *span,
        },
        Expr::UnaryOp { op, operand, span } => Expr::UnaryOp {
            op: *op,
            operand: Box::new(parameterize_expr(operand, next_index)),
            span: *span,
        },
        Expr::Cast {
            inner,
            target,
            span,
        } => Expr::Cast {
            inner: Box::new(parameterize_expr(inner, next_index)),
            target: *target,
            span: *span,
        },
        Expr::FunctionCall { name, args, span } => Expr::FunctionCall {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| parameterize_expr(arg, next_index))
                .collect(),
            span: *span,
        },
        Expr::Case {
            branches,
            else_,
            span,
        } => Expr::Case {
            branches: branches
                .iter()
                .map(|(cond, value)| {
                    (
                        parameterize_expr(cond, next_index),
                        parameterize_expr(value, next_index),
                    )
                })
                .collect(),
            else_: else_
                .as_ref()
                .map(|expr| Box::new(parameterize_expr(expr, next_index))),
            span: *span,
        },
        Expr::IsNull {
            operand,
            negated,
            span,
        } => Expr::IsNull {
            operand: Box::new(parameterize_expr(operand, next_index)),
            negated: *negated,
            span: *span,
        },
        Expr::InList {
            target,
            values,
            negated,
            span,
        } => Expr::InList {
            target: Box::new(parameterize_expr(target, next_index)),
            values: values
                .iter()
                .map(|value| parameterize_expr(value, next_index))
                .collect(),
            negated: *negated,
            span: *span,
        },
        Expr::Between {
            target,
            low,
            high,
            negated,
            span,
        } => Expr::Between {
            target: Box::new(parameterize_expr(target, next_index)),
            low: Box::new(parameterize_expr(low, next_index)),
            high: Box::new(parameterize_expr(high, next_index)),
            negated: *negated,
            span: *span,
        },
    }
}

fn bind_query_expr_inner(expr: &QueryExpr, binds: &[Value]) -> Option<QueryExpr> {
    match expr {
        QueryExpr::Table(query) => Some(QueryExpr::Table(bind_table_query(query, binds)?)),
        QueryExpr::Join(query) => Some(QueryExpr::Join(bind_join_query(query, binds)?)),
        QueryExpr::Graph(query) => Some(QueryExpr::Graph(bind_graph_query(query, binds)?)),
        QueryExpr::Path(query) => Some(QueryExpr::Path(bind_path_query(query, binds)?)),
        QueryExpr::Vector(query) => Some(QueryExpr::Vector(bind_vector_query(query, binds)?)),
        QueryExpr::Hybrid(query) => Some(QueryExpr::Hybrid(bind_hybrid_query(query, binds)?)),
        _ => None,
    }
}

fn parameterize_vector_query(query: &VectorQuery, next_index: &mut usize) -> Option<VectorQuery> {
    Some(VectorQuery {
        alias: query.alias.clone(),
        collection: query.collection.clone(),
        query_vector: parameterize_vector_source(&query.query_vector, next_index)?,
        k: query.k,
        filter: query
            .filter
            .as_ref()
            .map(|filter| parameterize_metadata_filter(filter, next_index)),
        metric: query.metric,
        include_vectors: query.include_vectors,
        include_metadata: query.include_metadata,
        threshold: query
            .threshold
            .map(|_| encode_f32_placeholder(allocate_param_index(next_index))),
    })
}

fn bind_vector_query(query: &VectorQuery, binds: &[Value]) -> Option<VectorQuery> {
    Some(VectorQuery {
        alias: query.alias.clone(),
        collection: query.collection.clone(),
        query_vector: bind_vector_source(&query.query_vector, binds)?,
        k: query.k,
        filter: query
            .filter
            .as_ref()
            .and_then(|filter| bind_metadata_filter(filter, binds)),
        metric: query.metric,
        include_vectors: query.include_vectors,
        include_metadata: query.include_metadata,
        threshold: query
            .threshold
            .and_then(|value| bind_placeholder_f32(value, binds)),
    })
}

fn parameterize_hybrid_query(query: &HybridQuery, next_index: &mut usize) -> Option<HybridQuery> {
    Some(HybridQuery {
        alias: query.alias.clone(),
        structured: Box::new(parameterize_query_expr_inner(
            &query.structured,
            next_index,
        )?),
        vector: parameterize_vector_query(&query.vector, next_index)?,
        fusion: parameterize_fusion_strategy(&query.fusion, next_index),
        limit: query.limit,
    })
}

fn bind_hybrid_query(query: &HybridQuery, binds: &[Value]) -> Option<HybridQuery> {
    Some(HybridQuery {
        alias: query.alias.clone(),
        structured: Box::new(bind_query_expr_inner(&query.structured, binds)?),
        vector: bind_vector_query(&query.vector, binds)?,
        fusion: bind_fusion_strategy(&query.fusion, binds)?,
        limit: query.limit,
    })
}

fn parameterize_vector_source(
    source: &VectorSource,
    next_index: &mut usize,
) -> Option<VectorSource> {
    match source {
        VectorSource::Literal(values) => Some(VectorSource::Literal(
            values
                .iter()
                .map(|_| encode_f32_placeholder(allocate_param_index(next_index)))
                .collect(),
        )),
        VectorSource::Text(_) => Some(VectorSource::Text(format!(
            "{VECTOR_TEXT_PARAM_PREFIX}{}",
            allocate_param_index(next_index)
        ))),
        VectorSource::Reference { collection, .. } => Some(VectorSource::Reference {
            collection: format!(
                "{VECTOR_REF_ID_PREFIX}{}:{collection}",
                allocate_param_index(next_index)
            ),
            vector_id: 0,
        }),
        VectorSource::Subquery(expr) => Some(VectorSource::Subquery(Box::new(
            parameterize_query_expr_inner(expr, next_index)?,
        ))),
    }
}

fn bind_vector_source(source: &VectorSource, binds: &[Value]) -> Option<VectorSource> {
    match source {
        VectorSource::Literal(values) => Some(VectorSource::Literal(
            values
                .iter()
                .map(|value| bind_placeholder_f32(*value, binds))
                .collect::<Option<Vec<_>>>()?,
        )),
        VectorSource::Text(text) => {
            if let Some(index) = parse_placeholder_index(text, VECTOR_TEXT_PARAM_PREFIX) {
                Some(VectorSource::Text(bind_value_to_string(binds.get(index)?)?))
            } else {
                Some(VectorSource::Text(text.clone()))
            }
        }
        VectorSource::Reference {
            collection,
            vector_id,
        } => {
            if let Some((index, original_collection)) =
                parse_prefixed_index_with_suffix(collection, VECTOR_REF_ID_PREFIX)
            {
                Some(VectorSource::Reference {
                    collection: original_collection.to_string(),
                    vector_id: bind_value_to_u64(binds.get(index)?)?,
                })
            } else {
                Some(VectorSource::Reference {
                    collection: collection.clone(),
                    vector_id: *vector_id,
                })
            }
        }
        VectorSource::Subquery(expr) => Some(VectorSource::Subquery(Box::new(
            bind_query_expr_inner(expr, binds)?,
        ))),
    }
}

fn parameterize_fusion_strategy(fusion: &FusionStrategy, next_index: &mut usize) -> FusionStrategy {
    match fusion {
        FusionStrategy::Rerank { .. } => FusionStrategy::Rerank {
            weight: encode_f32_placeholder(allocate_param_index(next_index)),
        },
        FusionStrategy::FilterThenSearch => FusionStrategy::FilterThenSearch,
        FusionStrategy::SearchThenFilter => FusionStrategy::SearchThenFilter,
        FusionStrategy::RRF { .. } => FusionStrategy::RRF {
            k: encode_u32_placeholder(allocate_param_index(next_index)),
        },
        FusionStrategy::Intersection => FusionStrategy::Intersection,
        FusionStrategy::Union { .. } => FusionStrategy::Union {
            structured_weight: encode_f32_placeholder(allocate_param_index(next_index)),
            vector_weight: encode_f32_placeholder(allocate_param_index(next_index)),
        },
    }
}

fn bind_fusion_strategy(fusion: &FusionStrategy, binds: &[Value]) -> Option<FusionStrategy> {
    match fusion {
        FusionStrategy::Rerank { weight } => Some(FusionStrategy::Rerank {
            weight: bind_placeholder_f32(*weight, binds)?,
        }),
        FusionStrategy::FilterThenSearch => Some(FusionStrategy::FilterThenSearch),
        FusionStrategy::SearchThenFilter => Some(FusionStrategy::SearchThenFilter),
        FusionStrategy::RRF { k } => Some(FusionStrategy::RRF {
            k: bind_placeholder_u32(*k, binds)?,
        }),
        FusionStrategy::Intersection => Some(FusionStrategy::Intersection),
        FusionStrategy::Union {
            structured_weight,
            vector_weight,
        } => Some(FusionStrategy::Union {
            structured_weight: bind_placeholder_f32(*structured_weight, binds)?,
            vector_weight: bind_placeholder_f32(*vector_weight, binds)?,
        }),
    }
}

fn parameterize_metadata_filter(filter: &MetadataFilter, next_index: &mut usize) -> MetadataFilter {
    match filter {
        MetadataFilter::Eq(key, value) => {
            MetadataFilter::Eq(key.clone(), parameterize_metadata_value(value, next_index))
        }
        MetadataFilter::Ne(key, value) => {
            MetadataFilter::Ne(key.clone(), parameterize_metadata_value(value, next_index))
        }
        MetadataFilter::Gt(key, value) => {
            MetadataFilter::Gt(key.clone(), parameterize_metadata_value(value, next_index))
        }
        MetadataFilter::Gte(key, value) => {
            MetadataFilter::Gte(key.clone(), parameterize_metadata_value(value, next_index))
        }
        MetadataFilter::Lt(key, value) => {
            MetadataFilter::Lt(key.clone(), parameterize_metadata_value(value, next_index))
        }
        MetadataFilter::Lte(key, value) => {
            MetadataFilter::Lte(key.clone(), parameterize_metadata_value(value, next_index))
        }
        MetadataFilter::In(key, values) => MetadataFilter::In(
            key.clone(),
            values
                .iter()
                .map(|value| parameterize_metadata_value(value, next_index))
                .collect(),
        ),
        MetadataFilter::NotIn(key, values) => MetadataFilter::NotIn(
            key.clone(),
            values
                .iter()
                .map(|value| parameterize_metadata_value(value, next_index))
                .collect(),
        ),
        MetadataFilter::Contains(_, _) => MetadataFilter::Contains(
            match filter {
                MetadataFilter::Contains(key, _) => key.clone(),
                _ => unreachable!(),
            },
            format!("{STRING_PARAM_PREFIX}{}", allocate_param_index(next_index)),
        ),
        MetadataFilter::StartsWith(_, _) => MetadataFilter::StartsWith(
            match filter {
                MetadataFilter::StartsWith(key, _) => key.clone(),
                _ => unreachable!(),
            },
            format!("{STRING_PARAM_PREFIX}{}", allocate_param_index(next_index)),
        ),
        MetadataFilter::EndsWith(_, _) => MetadataFilter::EndsWith(
            match filter {
                MetadataFilter::EndsWith(key, _) => key.clone(),
                _ => unreachable!(),
            },
            format!("{STRING_PARAM_PREFIX}{}", allocate_param_index(next_index)),
        ),
        MetadataFilter::Exists(key) => MetadataFilter::Exists(key.clone()),
        MetadataFilter::NotExists(key) => MetadataFilter::NotExists(key.clone()),
        MetadataFilter::And(filters) => MetadataFilter::And(
            filters
                .iter()
                .map(|filter| parameterize_metadata_filter(filter, next_index))
                .collect(),
        ),
        MetadataFilter::Or(filters) => MetadataFilter::Or(
            filters
                .iter()
                .map(|filter| parameterize_metadata_filter(filter, next_index))
                .collect(),
        ),
        MetadataFilter::Not(inner) => {
            MetadataFilter::Not(Box::new(parameterize_metadata_filter(inner, next_index)))
        }
    }
}

fn bind_metadata_filter(filter: &MetadataFilter, binds: &[Value]) -> Option<MetadataFilter> {
    match filter {
        MetadataFilter::Eq(key, value) => Some(MetadataFilter::Eq(
            key.clone(),
            bind_metadata_value(value, binds)?,
        )),
        MetadataFilter::Ne(key, value) => Some(MetadataFilter::Ne(
            key.clone(),
            bind_metadata_value(value, binds)?,
        )),
        MetadataFilter::Gt(key, value) => Some(MetadataFilter::Gt(
            key.clone(),
            bind_metadata_value(value, binds)?,
        )),
        MetadataFilter::Gte(key, value) => Some(MetadataFilter::Gte(
            key.clone(),
            bind_metadata_value(value, binds)?,
        )),
        MetadataFilter::Lt(key, value) => Some(MetadataFilter::Lt(
            key.clone(),
            bind_metadata_value(value, binds)?,
        )),
        MetadataFilter::Lte(key, value) => Some(MetadataFilter::Lte(
            key.clone(),
            bind_metadata_value(value, binds)?,
        )),
        MetadataFilter::In(key, values) => Some(MetadataFilter::In(
            key.clone(),
            values
                .iter()
                .map(|value| bind_metadata_value(value, binds))
                .collect::<Option<Vec<_>>>()?,
        )),
        MetadataFilter::NotIn(key, values) => Some(MetadataFilter::NotIn(
            key.clone(),
            values
                .iter()
                .map(|value| bind_metadata_value(value, binds))
                .collect::<Option<Vec<_>>>()?,
        )),
        MetadataFilter::Contains(key, value) => Some(MetadataFilter::Contains(
            key.clone(),
            bind_placeholder_string(value, binds)?.unwrap_or_default(),
        )),
        MetadataFilter::StartsWith(key, value) => Some(MetadataFilter::StartsWith(
            key.clone(),
            bind_placeholder_string(value, binds)?.unwrap_or_default(),
        )),
        MetadataFilter::EndsWith(key, value) => Some(MetadataFilter::EndsWith(
            key.clone(),
            bind_placeholder_string(value, binds)?.unwrap_or_default(),
        )),
        MetadataFilter::Exists(key) => Some(MetadataFilter::Exists(key.clone())),
        MetadataFilter::NotExists(key) => Some(MetadataFilter::NotExists(key.clone())),
        MetadataFilter::And(filters) => Some(MetadataFilter::And(
            filters
                .iter()
                .map(|filter| bind_metadata_filter(filter, binds))
                .collect::<Option<Vec<_>>>()?,
        )),
        MetadataFilter::Or(filters) => Some(MetadataFilter::Or(
            filters
                .iter()
                .map(|filter| bind_metadata_filter(filter, binds))
                .collect::<Option<Vec<_>>>()?,
        )),
        MetadataFilter::Not(inner) => Some(MetadataFilter::Not(Box::new(bind_metadata_filter(
            inner, binds,
        )?))),
    }
}

fn parameterize_metadata_value(_value: &MetadataValue, next_index: &mut usize) -> MetadataValue {
    MetadataValue::String(format!(
        "{METADATA_VALUE_PARAM_PREFIX}{}",
        allocate_param_index(next_index)
    ))
}

fn bind_metadata_value(value: &MetadataValue, binds: &[Value]) -> Option<MetadataValue> {
    match value {
        MetadataValue::String(text) => {
            if let Some(index) = parse_placeholder_index(text, METADATA_VALUE_PARAM_PREFIX) {
                Some(bind_value_to_metadata_value(binds.get(index)?)?)
            } else {
                Some(MetadataValue::String(text.clone()))
            }
        }
        other => Some(other.clone()),
    }
}

fn parameterize_join_query(query: &JoinQuery, next_index: &mut usize) -> Option<JoinQuery> {
    Some(JoinQuery {
        left: Box::new(parameterize_query_expr_inner(&query.left, next_index)?),
        right: Box::new(parameterize_query_expr_inner(&query.right, next_index)?),
        join_type: query.join_type,
        on: query.on.clone(),
        filter: query
            .filter
            .as_ref()
            .map(|filter| parameterize_filter(filter, next_index)),
        order_by: query
            .order_by
            .iter()
            .map(|clause| parameterize_order_by(clause, next_index))
            .collect::<Option<Vec<_>>>()?,
        limit: query.limit,
        offset: query.offset,
        return_items: query
            .return_items
            .iter()
            .map(|item| parameterize_select_item(item, next_index))
            .collect::<Option<Vec<_>>>()?,
        return_: query
            .return_
            .iter()
            .map(|projection| parameterize_projection(projection, next_index))
            .collect::<Option<Vec<_>>>()?,
    })
}

fn bind_join_query(query: &JoinQuery, binds: &[Value]) -> Option<JoinQuery> {
    Some(JoinQuery {
        left: Box::new(bind_query_expr_inner(&query.left, binds)?),
        right: Box::new(bind_query_expr_inner(&query.right, binds)?),
        join_type: query.join_type,
        on: query.on.clone(),
        filter: query
            .filter
            .as_ref()
            .and_then(|filter| bind_filter(filter, binds)),
        order_by: query
            .order_by
            .iter()
            .map(|clause| bind_order_by(clause, binds))
            .collect::<Option<Vec<_>>>()?,
        limit: query.limit,
        offset: query.offset,
        return_items: query
            .return_items
            .iter()
            .map(|item| bind_select_item(item, binds))
            .collect::<Option<Vec<_>>>()?,
        return_: query
            .return_
            .iter()
            .map(|projection| bind_projection(projection, binds))
            .collect::<Option<Vec<_>>>()?,
    })
}

fn parameterize_graph_query(query: &GraphQuery, next_index: &mut usize) -> Option<GraphQuery> {
    Some(GraphQuery {
        alias: query.alias.clone(),
        pattern: parameterize_graph_pattern(&query.pattern, next_index),
        filter: query
            .filter
            .as_ref()
            .map(|filter| parameterize_filter(filter, next_index)),
        return_: query
            .return_
            .iter()
            .map(|projection| parameterize_projection(projection, next_index))
            .collect::<Option<Vec<_>>>()?,
    })
}

fn bind_graph_query(query: &GraphQuery, binds: &[Value]) -> Option<GraphQuery> {
    Some(GraphQuery {
        alias: query.alias.clone(),
        pattern: bind_graph_pattern(&query.pattern, binds)?,
        filter: query
            .filter
            .as_ref()
            .and_then(|filter| bind_filter(filter, binds)),
        return_: query
            .return_
            .iter()
            .map(|projection| bind_projection(projection, binds))
            .collect::<Option<Vec<_>>>()?,
    })
}

fn parameterize_path_query(query: &PathQuery, next_index: &mut usize) -> Option<PathQuery> {
    Some(PathQuery {
        alias: query.alias.clone(),
        from: parameterize_node_selector(&query.from, next_index),
        to: parameterize_node_selector(&query.to, next_index),
        via: query.via.clone(),
        max_length: query.max_length,
        filter: query
            .filter
            .as_ref()
            .map(|filter| parameterize_filter(filter, next_index)),
        return_: query
            .return_
            .iter()
            .map(|projection| parameterize_projection(projection, next_index))
            .collect::<Option<Vec<_>>>()?,
    })
}

fn bind_path_query(query: &PathQuery, binds: &[Value]) -> Option<PathQuery> {
    Some(PathQuery {
        alias: query.alias.clone(),
        from: bind_node_selector(&query.from, binds)?,
        to: bind_node_selector(&query.to, binds)?,
        via: query.via.clone(),
        max_length: query.max_length,
        filter: query
            .filter
            .as_ref()
            .and_then(|filter| bind_filter(filter, binds)),
        return_: query
            .return_
            .iter()
            .map(|projection| bind_projection(projection, binds))
            .collect::<Option<Vec<_>>>()?,
    })
}

fn parameterize_filter(
    filter: &crate::storage::query::ast::Filter,
    next_index: &mut usize,
) -> crate::storage::query::ast::Filter {
    expr_to_filter(&parameterize_expr(&filter_to_expr(filter), next_index))
}

fn bind_filter(
    filter: &crate::storage::query::ast::Filter,
    binds: &[Value],
) -> Option<crate::storage::query::ast::Filter> {
    Some(expr_to_filter(&bind_expr(&filter_to_expr(filter), binds)?))
}

fn parameterize_projection(projection: &Projection, next_index: &mut usize) -> Option<Projection> {
    match projection {
        Projection::All => Some(Projection::All),
        Projection::Column(column) => {
            Some(parameterize_projection_column(column, None, next_index))
        }
        Projection::Alias(column, alias) => Some(parameterize_projection_column(
            column,
            Some(alias.as_str()),
            next_index,
        )),
        Projection::Function(name, args) => Some(Projection::Function(
            name.clone(),
            args.iter()
                .map(|arg| parameterize_projection(arg, next_index))
                .collect::<Option<Vec<_>>>()?,
        )),
        Projection::Expression(filter, alias) => Some(Projection::Expression(
            Box::new(parameterize_filter(filter, next_index)),
            alias.clone(),
        )),
        Projection::Field(field, alias) => Some(Projection::Field(field.clone(), alias.clone())),
    }
}

fn bind_projection(projection: &Projection, binds: &[Value]) -> Option<Projection> {
    match projection {
        Projection::All => Some(Projection::All),
        Projection::Column(column) => bind_projection_column(column, None, binds),
        Projection::Alias(column, alias) => {
            bind_projection_column(column, Some(alias.as_str()), binds)
        }
        Projection::Function(name, args) => Some(Projection::Function(
            name.clone(),
            args.iter()
                .map(|arg| bind_projection(arg, binds))
                .collect::<Option<Vec<_>>>()?,
        )),
        Projection::Expression(filter, alias) => Some(Projection::Expression(
            Box::new(bind_filter(filter, binds)?),
            alias.clone(),
        )),
        Projection::Field(field, alias) => Some(Projection::Field(field.clone(), alias.clone())),
    }
}

fn parameterize_projection_column(
    column: &str,
    alias: Option<&str>,
    next_index: &mut usize,
) -> Projection {
    if column.starts_with("LIT:") {
        let index = *next_index;
        *next_index += 1;
        let placeholder = format!("{PROJECTION_PARAM_PREFIX}{index}");
        if let Some(alias) = alias {
            Projection::Alias(placeholder, alias.to_string())
        } else {
            Projection::Column(placeholder)
        }
    } else if let Some(alias) = alias {
        Projection::Alias(column.to_string(), alias.to_string())
    } else {
        Projection::Column(column.to_string())
    }
}

fn bind_projection_column(
    column: &str,
    alias: Option<&str>,
    binds: &[Value],
) -> Option<Projection> {
    if let Some(index) = parse_placeholder_index(column, PROJECTION_PARAM_PREFIX) {
        let projection = projection_from_literal(binds.get(index)?)?;
        Some(attach_projection_alias(projection, alias))
    } else if let Some(alias) = alias {
        Some(Projection::Alias(column.to_string(), alias.to_string()))
    } else {
        Some(Projection::Column(column.to_string()))
    }
}

fn parameterize_graph_pattern(pattern: &GraphPattern, next_index: &mut usize) -> GraphPattern {
    GraphPattern {
        nodes: pattern
            .nodes
            .iter()
            .map(|node| parameterize_node_pattern(node, next_index))
            .collect(),
        edges: pattern.edges.clone(),
    }
}

fn bind_graph_pattern(pattern: &GraphPattern, binds: &[Value]) -> Option<GraphPattern> {
    Some(GraphPattern {
        nodes: pattern
            .nodes
            .iter()
            .map(|node| bind_node_pattern(node, binds))
            .collect::<Option<Vec<_>>>()?,
        edges: pattern.edges.clone(),
    })
}

fn parameterize_node_pattern(node: &NodePattern, next_index: &mut usize) -> NodePattern {
    NodePattern {
        alias: node.alias.clone(),
        node_type: node.node_type.clone(),
        properties: node
            .properties
            .iter()
            .map(|property| parameterize_property_filter(property, next_index))
            .collect(),
    }
}

fn bind_node_pattern(node: &NodePattern, binds: &[Value]) -> Option<NodePattern> {
    Some(NodePattern {
        alias: node.alias.clone(),
        node_type: node.node_type.clone(),
        properties: node
            .properties
            .iter()
            .map(|property| bind_property_filter(property, binds))
            .collect::<Option<Vec<_>>>()?,
    })
}

fn parameterize_property_filter(filter: &PropertyFilter, next_index: &mut usize) -> PropertyFilter {
    PropertyFilter {
        name: filter.name.clone(),
        op: filter.op,
        value: parameterize_value_placeholder(next_index),
    }
}

fn bind_property_filter(filter: &PropertyFilter, binds: &[Value]) -> Option<PropertyFilter> {
    Some(PropertyFilter {
        name: filter.name.clone(),
        op: filter.op,
        value: bind_value_placeholder(&filter.value, binds)?,
    })
}

fn parameterize_node_selector(selector: &NodeSelector, next_index: &mut usize) -> NodeSelector {
    match selector {
        NodeSelector::ById(_) => {
            let index = *next_index;
            *next_index += 1;
            NodeSelector::ById(format!("{STRING_PARAM_PREFIX}{index}"))
        }
        NodeSelector::ByType { node_type, filter } => NodeSelector::ByType {
            node_type: node_type.clone(),
            filter: filter
                .as_ref()
                .map(|filter| parameterize_property_filter(filter, next_index)),
        },
        NodeSelector::ByRow { table, .. } => {
            let index = *next_index;
            *next_index += 1;
            NodeSelector::ByRow {
                table: format!("{ROW_SELECTOR_TABLE_PREFIX}{index}:{table}"),
                row_id: 0,
            }
        }
    }
}

fn bind_node_selector(selector: &NodeSelector, binds: &[Value]) -> Option<NodeSelector> {
    match selector {
        NodeSelector::ById(id) => {
            if let Some(index) = parse_placeholder_index(id, STRING_PARAM_PREFIX) {
                Some(NodeSelector::ById(bind_value_to_string(binds.get(index)?)?))
            } else {
                Some(NodeSelector::ById(id.clone()))
            }
        }
        NodeSelector::ByType { node_type, filter } => Some(NodeSelector::ByType {
            node_type: node_type.clone(),
            filter: filter
                .as_ref()
                .and_then(|filter| bind_property_filter(filter, binds)),
        }),
        NodeSelector::ByRow { table, row_id } => {
            if let Some((index, original_table)) = parse_row_selector_placeholder(table) {
                Some(NodeSelector::ByRow {
                    table: original_table.to_string(),
                    row_id: bind_value_to_u64(binds.get(index)?)?,
                })
            } else {
                Some(NodeSelector::ByRow {
                    table: table.clone(),
                    row_id: *row_id,
                })
            }
        }
    }
}

fn parameterize_value_placeholder(next_index: &mut usize) -> Value {
    let index = *next_index;
    *next_index += 1;
    Value::text(format!("{VALUE_PARAM_PREFIX}{index}"))
}

fn bind_value_placeholder(value: &Value, binds: &[Value]) -> Option<Value> {
    match value {
        Value::Text(text) => {
            if let Some(index) = parse_placeholder_index(text, VALUE_PARAM_PREFIX) {
                binds.get(index).cloned()
            } else {
                Some(value.clone())
            }
        }
        _ => Some(value.clone()),
    }
}

fn attach_projection_alias(projection: Projection, alias: Option<&str>) -> Projection {
    let Some(alias) = alias else {
        return projection;
    };
    match projection {
        Projection::Field(field, _) => Projection::Field(field, Some(alias.to_string())),
        Projection::Expression(filter, _) => {
            Projection::Expression(filter, Some(alias.to_string()))
        }
        Projection::Function(name, args) => {
            if name.contains(':') {
                Projection::Function(name, args)
            } else {
                Projection::Function(format!("{name}:{alias}"), args)
            }
        }
        Projection::Column(column) => Projection::Alias(column, alias.to_string()),
        Projection::Alias(column, _) => Projection::Alias(column, alias.to_string()),
        Projection::All => Projection::All,
    }
}

fn bind_value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        _ => Some(value.to_string()),
    }
}

fn bind_placeholder_string(value: &str, binds: &[Value]) -> Option<Option<String>> {
    if let Some(index) = parse_placeholder_index(value, STRING_PARAM_PREFIX) {
        Some(bind_value_to_string(binds.get(index)?))
    } else {
        Some(Some(value.to_string()))
    }
}

fn bind_value_to_u64(value: &Value) -> Option<u64> {
    match value {
        Value::UnsignedInteger(value) => Some(*value),
        Value::Integer(value) if *value >= 0 => Some(*value as u64),
        Value::BigInt(value) if *value >= 0 => Some(*value as u64),
        Value::Text(value) => value.parse().ok(),
        _ => None,
    }
}

fn parse_placeholder_index(value: &str, prefix: &str) -> Option<usize> {
    value.strip_prefix(prefix)?.parse().ok()
}

fn parse_prefixed_index_with_suffix<'a>(value: &'a str, prefix: &str) -> Option<(usize, &'a str)> {
    let rest = value.strip_prefix(prefix)?;
    let (index, suffix) = rest.split_once(':')?;
    Some((index.parse().ok()?, suffix))
}

fn parse_row_selector_placeholder(value: &str) -> Option<(usize, &str)> {
    let rest = value.strip_prefix(ROW_SELECTOR_TABLE_PREFIX)?;
    let (index, table) = rest.split_once(':')?;
    Some((index.parse().ok()?, table))
}

fn allocate_param_index(next_index: &mut usize) -> usize {
    let index = *next_index;
    *next_index += 1;
    index
}

fn encode_f32_placeholder(index: usize) -> f32 {
    f32::from_bits(FLOAT32_PARAM_BITS_BASE | (index as u32 & 0x003f_ffff))
}

fn decode_f32_placeholder(value: f32) -> Option<usize> {
    let bits = value.to_bits();
    if bits & FLOAT32_PARAM_BITS_BASE == FLOAT32_PARAM_BITS_BASE {
        Some((bits & 0x003f_ffff) as usize)
    } else {
        None
    }
}

fn bind_placeholder_f32(value: f32, binds: &[Value]) -> Option<f32> {
    if let Some(index) = decode_f32_placeholder(value) {
        bind_value_to_f32(binds.get(index)?)
    } else {
        Some(value)
    }
}

fn encode_u32_placeholder(index: usize) -> u32 {
    U32_PARAM_BASE | (index as u32 & 0x000f_ffff)
}

fn decode_u32_placeholder(value: u32) -> Option<usize> {
    if value & 0xfff0_0000 == U32_PARAM_BASE {
        Some((value & 0x000f_ffff) as usize)
    } else {
        None
    }
}

fn bind_placeholder_u32(value: u32, binds: &[Value]) -> Option<u32> {
    if let Some(index) = decode_u32_placeholder(value) {
        bind_value_to_u64(binds.get(index)?).and_then(|value| u32::try_from(value).ok())
    } else {
        Some(value)
    }
}

fn bind_value_to_f32(value: &Value) -> Option<f32> {
    match value {
        Value::Float(value) => Some(*value as f32),
        Value::Integer(value) => Some(*value as f32),
        Value::UnsignedInteger(value) => Some(*value as f32),
        Value::BigInt(value) => Some(*value as f32),
        Value::Text(value) => value.parse().ok(),
        _ => None,
    }
}

fn bind_value_to_metadata_value(value: &Value) -> Option<MetadataValue> {
    match value {
        Value::Text(value) => Some(MetadataValue::String(value.to_string())),
        Value::Integer(value) => Some(MetadataValue::Integer(*value)),
        Value::UnsignedInteger(value) => i64::try_from(*value).ok().map(MetadataValue::Integer),
        Value::BigInt(value) => Some(MetadataValue::Integer(*value)),
        Value::Float(value) => Some(MetadataValue::Float(*value)),
        Value::Boolean(value) => Some(MetadataValue::Bool(*value)),
        Value::Null => Some(MetadataValue::Null),
        _ => None,
    }
}

fn bind_table_query(query: &TableQuery, binds: &[Value]) -> Option<TableQuery> {
    let source = match &query.source {
        Some(TableSource::Name(name)) => Some(TableSource::Name(name.clone())),
        Some(TableSource::Subquery(inner)) => Some(TableSource::Subquery(Box::new(
            bind_query_expr_inner(inner, binds)?,
        ))),
        None => None,
    };

    Some(TableQuery {
        table: query.table.clone(),
        source,
        alias: query.alias.clone(),
        select_items: query
            .select_items
            .iter()
            .map(|item| bind_select_item(item, binds))
            .collect::<Option<Vec<_>>>()?,
        columns: Vec::new(),
        where_expr: query
            .where_expr
            .as_ref()
            .and_then(|expr| bind_expr(expr, binds)),
        filter: None,
        group_by_exprs: query
            .group_by_exprs
            .iter()
            .map(|expr| bind_expr(expr, binds))
            .collect::<Option<Vec<_>>>()?,
        group_by: Vec::new(),
        having_expr: query
            .having_expr
            .as_ref()
            .and_then(|expr| bind_expr(expr, binds)),
        having: None,
        order_by: query
            .order_by
            .iter()
            .map(|clause| bind_order_by(clause, binds))
            .collect::<Option<Vec<_>>>()?,
        limit: query.limit,
        offset: query.offset,
        expand: query.expand.clone(),
        as_of: query.as_of.clone(),
    })
}

fn bind_select_item(item: &SelectItem, binds: &[Value]) -> Option<SelectItem> {
    match item {
        SelectItem::Wildcard => Some(SelectItem::Wildcard),
        SelectItem::Expr { expr, alias } => Some(SelectItem::Expr {
            expr: bind_expr(expr, binds)?,
            alias: alias.clone(),
        }),
    }
}

fn bind_order_by(clause: &OrderByClause, binds: &[Value]) -> Option<OrderByClause> {
    Some(OrderByClause {
        field: clause.field.clone(),
        expr: clause.expr.as_ref().and_then(|expr| bind_expr(expr, binds)),
        ascending: clause.ascending,
        nulls_first: clause.nulls_first,
    })
}

fn bind_expr(expr: &Expr, binds: &[Value]) -> Option<Expr> {
    match expr {
        Expr::Literal { .. } | Expr::Column { .. } => Some(expr.clone()),
        Expr::Parameter { index, span } => Some(Expr::Literal {
            value: binds.get(*index)?.clone(),
            span: *span,
        }),
        Expr::BinaryOp { op, lhs, rhs, span } => Some(Expr::BinaryOp {
            op: *op,
            lhs: Box::new(bind_expr(lhs, binds)?),
            rhs: Box::new(bind_expr(rhs, binds)?),
            span: *span,
        }),
        Expr::UnaryOp { op, operand, span } => Some(Expr::UnaryOp {
            op: *op,
            operand: Box::new(bind_expr(operand, binds)?),
            span: *span,
        }),
        Expr::Cast {
            inner,
            target,
            span,
        } => Some(Expr::Cast {
            inner: Box::new(bind_expr(inner, binds)?),
            target: *target,
            span: *span,
        }),
        Expr::FunctionCall { name, args, span } => Some(Expr::FunctionCall {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| bind_expr(arg, binds))
                .collect::<Option<Vec<_>>>()?,
            span: *span,
        }),
        Expr::Case {
            branches,
            else_,
            span,
        } => Some(Expr::Case {
            branches: branches
                .iter()
                .map(|(cond, value)| Some((bind_expr(cond, binds)?, bind_expr(value, binds)?)))
                .collect::<Option<Vec<_>>>()?,
            else_: else_
                .as_ref()
                .and_then(|expr| bind_expr(expr, binds).map(Box::new)),
            span: *span,
        }),
        Expr::IsNull {
            operand,
            negated,
            span,
        } => Some(Expr::IsNull {
            operand: Box::new(bind_expr(operand, binds)?),
            negated: *negated,
            span: *span,
        }),
        Expr::InList {
            target,
            values,
            negated,
            span,
        } => Some(Expr::InList {
            target: Box::new(bind_expr(target, binds)?),
            values: values
                .iter()
                .map(|value| bind_expr(value, binds))
                .collect::<Option<Vec<_>>>()?,
            negated: *negated,
            span: *span,
        }),
        Expr::Between {
            target,
            low,
            high,
            negated,
            span,
        } => Some(Expr::Between {
            target: Box::new(bind_expr(target, binds)?),
            low: Box::new(bind_expr(low, binds)?),
            high: Box::new(bind_expr(high, binds)?),
            negated: *negated,
            span: *span,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::ast::{BinOp, FieldRef, SelectItem, TableQuery};

    #[test]
    fn table_shape_round_trips_with_new_binds() {
        let query = QueryExpr::Table(TableQuery {
            table: "users".to_string(),
            source: None,
            alias: None,
            select_items: vec![SelectItem::Expr {
                expr: Expr::Column {
                    field: FieldRef::TableColumn {
                        table: String::new(),
                        column: "name".to_string(),
                    },
                    span: crate::storage::query::ast::Span::synthetic(),
                },
                alias: None,
            }],
            columns: Vec::new(),
            where_expr: Some(Expr::BinaryOp {
                op: BinOp::Eq,
                lhs: Box::new(Expr::Column {
                    field: FieldRef::TableColumn {
                        table: String::new(),
                        column: "age".to_string(),
                    },
                    span: crate::storage::query::ast::Span::synthetic(),
                }),
                rhs: Box::new(Expr::Literal {
                    value: Value::Integer(18),
                    span: crate::storage::query::ast::Span::synthetic(),
                }),
                span: crate::storage::query::ast::Span::synthetic(),
            }),
            filter: None,
            group_by_exprs: Vec::new(),
            group_by: Vec::new(),
            having_expr: None,
            having: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            expand: None,
            as_of: None,
        });

        let prepared = parameterize_query_expr(&query).unwrap();
        assert_eq!(prepared.parameter_count, 1);

        let rebound = bind_parameterized_query(
            &prepared.shape,
            &[Value::Integer(42)],
            prepared.parameter_count,
        )
        .unwrap();

        let QueryExpr::Table(bound_table) = rebound else {
            panic!("expected table query");
        };
        match bound_table.where_expr.unwrap() {
            Expr::BinaryOp { rhs, .. } => match *rhs {
                Expr::Literal { value, .. } => assert_eq!(value, Value::Integer(42)),
                other => panic!("expected rebound literal, got {other:?}"),
            },
            other => panic!("expected binary op, got {other:?}"),
        }
    }
}
