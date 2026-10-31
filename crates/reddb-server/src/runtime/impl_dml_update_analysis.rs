//! DML UPDATE analysis helpers (column-reference detection, ordering, dedupe,
//! CDC item kind) extracted from `impl_dml`.
//!
//! Behaviour-preserving move (issue #1633); `pub(super)` visibility keeps the
//! sibling `impl_dml` call sites unchanged.

use super::record_search::runtime_any_record_from_entity_ref;
use super::*;
use reddb_rql::ast::Expr;

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

pub(super) fn try_resolve_update_entity_by_logical_id(
    runtime: &RedDBRuntime,
    table: &str,
    logical_id: EntityId,
) -> RedDBResult<Option<UnifiedEntity>> {
    let store = runtime.inner.db.store();

    // Read-modify-write pre-image resolution.
    //
    // A compound assignment (`n = n + 1`) folds the pre-image value into the
    // written row, so the pre-image MUST be the LATEST COMMITTED version,
    // re-evaluated *now* — this call runs while we hold the per-row RMW lock,
    // so every earlier same-row writer has already committed. Two failure
    // modes must both be avoided:
    //   * `get_table_row_by_logical_id` returns whichever physical version
    //     currently carries `xmax == 0`, which under concurrent same-row
    //     writes can be another transaction's still-uncommitted version —
    //     folding a later-aborted write into committed state (dirty read).
    //   * the pinned *statement* snapshot was captured before this statement
    //     acquired the RMW lock, so it can miss a value a peer committed in
    //     the meantime — every concurrent increment then reads the same stale
    //     pre-image and overwrites its predecessor (lost update).
    // A snapshot taken at this instant, paired with this connection's own
    // in-flight xids, threads both: it sees the freshest committed version
    // (no lost update) and this transaction's own writes, while hiding every
    // other transaction's uncommitted version (no dirty read).
    if let Some(base) = crate::runtime::impl_core::capture_current_snapshot() {
        let snapshot = base.manager.fresh_read_snapshot();
        let ctx = crate::runtime::impl_core::SnapshotContext {
            snapshot,
            manager: Arc::clone(&base.manager),
            own_xids: base.own_xids.clone(),
            requires_index_fallback: true,
            serializable_reader: base.serializable_reader,
        };
        let resolver =
            crate::runtime::table_row_mvcc_resolver::TableRowMvccReadResolver::captured(Some(ctx));
        if let Some(entity) = resolver.try_resolve_logical_id(&store, table, logical_id)? {
            return Ok(Some(entity));
        }
    } else if let Some(entity) = store.try_get_table_row_by_logical_id(table, logical_id)? {
        return Ok(Some(entity));
    }
    // Fallback for non-table-row entities (graph nodes/edges, etc.) where
    // entity_id == logical_id and the MVCC table-row resolver doesn't apply.
    Ok(store
        .try_get(table, logical_id, usize::MAX)?
        .filter(|entity| !matches!(entity.kind, EntityKind::TableRow { .. })))
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

/// Keep the top `k` items under `cmp` without a full sort when it pays off.
/// Output is **bit-identical** to `items.sort_by(cmp); items.truncate(k)` for
/// any input: the small-`n` branch is exactly that stable sort, and the
/// quickselect branch appends the original index as the final tie-break so the
/// unstable partition/sort reproduces the stable sort's first `k` elements,
/// preserving `cmp`'s own tie-breaks and NaN/None handling. Mirrors
/// `join_filter::ordering::top_k_records_by_order_by_with_db`.
fn partial_top_k<T: Clone>(items: &mut Vec<T>, k: usize, cmp: impl Fn(&T, &T) -> Ordering) {
    let n = items.len();
    if k == 0 {
        items.clear();
        return;
    }
    if n <= k.saturating_mul(2) {
        items.sort_by(|a, b| cmp(a, b));
        items.truncate(k);
        return;
    }
    let mut idxs: Vec<usize> = (0..n).collect();
    idxs.select_nth_unstable_by(k - 1, |&a, &b| {
        cmp(&items[a], &items[b]).then_with(|| a.cmp(&b))
    });
    idxs.truncate(k);
    idxs.sort_by(|&a, &b| cmp(&items[a], &items[b]).then_with(|| a.cmp(&b)));
    let orig = std::mem::take(items);
    *items = idxs.into_iter().map(|i| orig[i].clone()).collect();
}

pub(super) fn ordered_update_target_ids(
    manager: &Arc<crate::storage::SegmentManager>,
    entity_ids: &[EntityId],
    order_by: &[OrderByClause],
    limit: Option<usize>,
) -> RedDBResult<Vec<EntityId>> {
    #[derive(Clone)]
    struct Target {
        id: EntityId,
        logical_id: EntityId,
        key_offset: usize,
    }
    let key_count = entity_ids
        .len()
        .checked_mul(order_by.len())
        .ok_or_else(|| {
            RedDBError::Query("UPDATE ordering key count exceeds addressable memory".into())
        })?;
    let mut keys = Vec::with_capacity(key_count);
    let mut targets = Vec::with_capacity(entity_ids.len());
    manager.try_for_each_id(entity_ids, usize::MAX, |_, entity| {
        // Borrowed resident visitors retain the compact positional row. Restore
        // names on one temporary row, as the previous owned batch read did.
        let mut restored;
        let entity = match &entity.data {
            EntityData::Row(row) if row.named.is_none() && row.schema.is_none() => {
                restored = entity.clone();
                manager.restore_owned_row_schemas(std::iter::once(&mut restored));
                &restored
            }
            _ => entity,
        };
        targets.push(Target {
            id: entity.id,
            logical_id: entity.logical_id(),
            key_offset: keys.len(),
        });
        keys.extend(
            order_by
                .iter()
                .map(|clause| update_order_value(entity, &clause.field)),
        );
        true
    })?;
    let compare = |left: &Target, right: &Target| {
        for (position, clause) in order_by.iter().enumerate() {
            let ordering = compare_update_order_values(
                keys[left.key_offset + position].as_ref(),
                keys[right.key_offset + position].as_ref(),
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
        left.logical_id.cmp(&right.logical_id)
    };
    match limit {
        Some(limit) => partial_top_k(&mut targets, limit, compare),
        None => targets.sort_by(compare),
    }
    Ok(targets.into_iter().map(|target| target.id).collect())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_targets_restore_missing_positional_names_before_extracting_keys() {
        let manager = Arc::new(crate::storage::SegmentManager::new("ordered_rows"));
        manager.set_column_schema_if_empty(vec!["id".into(), "payload".into()]);
        for (physical, value) in [(1, 2), (2, 1), (3, 0)] {
            let row = UnifiedEntity::table_row(
                EntityId::new(physical),
                "ordered_rows",
                physical,
                vec![Value::Integer(value), Value::text("kept")],
            );
            manager.insert(row).expect("positional row");
            assert!(manager
                .get_with(EntityId::new(physical), |entity| {
                    let row = entity.data.as_row().expect("row");
                    row.named.is_none() && row.schema.is_none()
                })
                .expect("raw positional fixture"));
        }
        let ids = [EntityId::new(1), EntityId::new(2), EntityId::new(3)];
        for (direction, expected) in [("ASC", [3, 2]), ("DESC", [1, 2])] {
            let sql = format!("UPDATE ordered_rows SET id = id ORDER BY id {direction} LIMIT 2");
            let QueryExpr::Update(query) = reddb_rql::parse(&sql).expect("ordered update").query
            else {
                panic!("expected UPDATE");
            };
            for limit in [1, 2] {
                let selected =
                    ordered_update_target_ids(&manager, &ids, &query.order_by, Some(limit))
                        .expect("extract positional keys");
                assert_eq!(
                    selected,
                    expected[..limit]
                        .iter()
                        .copied()
                        .map(EntityId::new)
                        .collect::<Vec<_>>()
                );
            }
        }
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
