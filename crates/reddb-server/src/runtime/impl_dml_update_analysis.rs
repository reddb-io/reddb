//! DML UPDATE analysis helpers (column-reference detection, ordering, dedupe,
//! CDC item kind) extracted from `impl_dml`.
//!
//! Behaviour-preserving move (issue #1633); `pub(super)` visibility keeps the
//! sibling `impl_dml` call sites unchanged.

use super::record_search::runtime_any_record_from_entity_ref;
use super::*;
use crate::storage::query::ast::Expr;

pub(super) fn expr_references_update_column(
    expr: &Expr,
    table_name: &str,
    target_column: &str,
) -> bool {
    match expr {
        Expr::Literal { .. } | Expr::Parameter { .. } | Expr::Subquery { .. } => false,
        Expr::Column { field, .. } => {
            field_ref_matches_update_column(field, table_name, target_column)
        }
        Expr::BinaryOp { lhs, rhs, .. } => {
            expr_references_update_column(lhs, table_name, target_column)
                || expr_references_update_column(rhs, table_name, target_column)
        }
        Expr::UnaryOp { operand, .. } | Expr::Cast { inner: operand, .. } => {
            expr_references_update_column(operand, table_name, target_column)
        }
        Expr::FunctionCall { args, .. } => args
            .iter()
            .any(|arg| expr_references_update_column(arg, table_name, target_column)),
        Expr::Case {
            branches, else_, ..
        } => {
            branches.iter().any(|(cond, value)| {
                expr_references_update_column(cond, table_name, target_column)
                    || expr_references_update_column(value, table_name, target_column)
            }) || else_
                .as_deref()
                .is_some_and(|expr| expr_references_update_column(expr, table_name, target_column))
        }
        Expr::IsNull { operand, .. } => {
            expr_references_update_column(operand, table_name, target_column)
        }
        Expr::InList { target, values, .. } => {
            expr_references_update_column(target, table_name, target_column)
                || values
                    .iter()
                    .any(|value| expr_references_update_column(value, table_name, target_column))
        }
        Expr::Between {
            target, low, high, ..
        } => {
            expr_references_update_column(target, table_name, target_column)
                || expr_references_update_column(low, table_name, target_column)
                || expr_references_update_column(high, table_name, target_column)
        }
        Expr::WindowFunctionCall { args, window, .. } => {
            args.iter()
                .any(|arg| expr_references_update_column(arg, table_name, target_column))
                || window
                    .partition_by
                    .iter()
                    .any(|e| expr_references_update_column(e, table_name, target_column))
                || window
                    .order_by
                    .iter()
                    .any(|o| expr_references_update_column(&o.expr, table_name, target_column))
        }
    }
}

pub(super) fn field_ref_matches_update_column(
    field: &FieldRef,
    table_name: &str,
    target_column: &str,
) -> bool {
    match field {
        FieldRef::TableColumn { table, column } => {
            column.eq_ignore_ascii_case(target_column)
                && (table.is_empty() || table.eq_ignore_ascii_case(table_name))
        }
        FieldRef::NodeProperty { .. } | FieldRef::EdgeProperty { .. } | FieldRef::NodeId { .. } => {
            false
        }
    }
}

pub(super) fn resolve_update_entity_by_logical_id(
    runtime: &RedDBRuntime,
    table: &str,
    logical_id: EntityId,
) -> Option<UnifiedEntity> {
    let store = runtime.inner.db.store();
    // Read-modify-write pre-image must be resolved through the *current
    // statement snapshot*, not merely the latest live physical version.
    // `get_table_row_by_logical_id` returns whichever version currently
    // carries `xmax == 0`, which under concurrent same-row UPDATEs can be a
    // sibling writer's still-uncommitted version. Applying a compound
    // assignment (`value += 1`) on top of that pre-image lets a first-
    // committer-wins winner fold a concurrent loser's (later-aborted) write
    // into committed state — an isolation violation. Routing through the
    // MVCC resolver reads the version this transaction is actually allowed
    // to observe (including its own in-flight writes via `own_xids`).
    let resolver =
        crate::runtime::table_row_mvcc_resolver::TableRowMvccReadResolver::current_statement();
    if let Some(entity) = resolver.resolve_logical_id(&store, table, logical_id) {
        return Some(entity);
    }
    // Fallback for non-table-row entities (graph nodes/edges, etc.) where
    // entity_id == logical_id and the MVCC table-row resolver doesn't apply.
    store.get(table, logical_id)
}

pub(super) fn update_cdc_item_kind(
    runtime: &RedDBRuntime,
    collection: &str,
    entity: &UnifiedEntity,
) -> &'static str {
    match &entity.data {
        EntityData::Node(_) => return "node",
        EntityData::Edge(_) => return "edge",
        _ => {}
    }

    match runtime
        .db()
        .collection_contract(collection)
        .map(|contract| contract.declared_model)
    {
        Some(crate::catalog::CollectionModel::Document) => "document",
        Some(crate::catalog::CollectionModel::Kv)
        | Some(crate::catalog::CollectionModel::Vault) => "kv",
        _ => "row",
    }
}

pub(super) fn ordered_update_target_ids(
    manager: &Arc<crate::storage::SegmentManager>,
    entity_ids: &[EntityId],
    order_by: &[OrderByClause],
    limit: Option<usize>,
) -> Vec<EntityId> {
    let mut entities: Vec<UnifiedEntity> =
        manager.get_many(entity_ids).into_iter().flatten().collect();
    entities.sort_by(|left, right| compare_update_order(left, right, order_by));
    if let Some(limit) = limit {
        entities.truncate(limit);
    }
    entities.into_iter().map(|entity| entity.id).collect()
}

pub(super) fn compare_update_order(
    left: &UnifiedEntity,
    right: &UnifiedEntity,
    order_by: &[OrderByClause],
) -> Ordering {
    for clause in order_by {
        let left_value = update_order_value(left, &clause.field);
        let right_value = update_order_value(right, &clause.field);
        let ordering = compare_update_order_values(
            left_value.as_ref(),
            right_value.as_ref(),
            clause.nulls_first,
        );
        if ordering != Ordering::Equal {
            return if clause.ascending {
                ordering
            } else {
                ordering.reverse()
            };
        }
    }
    left.logical_id().raw().cmp(&right.logical_id().raw())
}

pub(super) fn compare_update_order_values(
    left: Option<&Value>,
    right: Option<&Value>,
    nulls_first: bool,
) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => {
            if nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (Some(_), None) => {
            if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (Some(left), Some(right)) => {
            crate::storage::query::value_compare::total_compare_values(left, right)
        }
    }
}

pub(super) fn update_order_value(entity: &UnifiedEntity, field: &FieldRef) -> Option<Value> {
    let FieldRef::TableColumn { table, column } = field else {
        return None;
    };
    if !table.is_empty() {
        return None;
    }
    if column.eq_ignore_ascii_case("rid") {
        return Some(Value::UnsignedInteger(entity.logical_id().raw()));
    }
    match &entity.data {
        // After the single-source binary-body cutover (ADR 0063) a DOCUMENT's
        // top-level fields live only inside the binary `body` container, not as
        // promoted row fields, so a direct `get_field` misses them and the
        // claim/UPDATE `ORDER BY <body-field>` would silently fall back to
        // insertion order. Mirror the filter read-seam: when the field isn't a
        // direct row field, offset-read it from the binary body.
        EntityData::Row(row) => {
            row.get_field(column)
                .cloned()
                .or_else(|| match row.get_field("body") {
                    Some(Value::Json(bytes)) => {
                        crate::document_body::read_body_field(bytes, column)
                    }
                    _ => None,
                })
        }
        EntityData::Node(_) | EntityData::Edge(_) => runtime_any_record_from_entity_ref(entity)
            .and_then(|record| record.get(column).cloned()),
        _ => None,
    }
}

pub(super) fn dedupe_update_columns(mut columns: Vec<String>) -> Vec<String> {
    if columns.is_empty() {
        return columns;
    }

    let mut unique = Vec::with_capacity(columns.len());
    for column in columns.drain(..) {
        if !unique
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&column))
        {
            unique.push(column);
        }
    }
    unique
}
