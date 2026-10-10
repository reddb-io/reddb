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

/// Per-row write locks (`row:<logical_id>`) an UPDATE statement holds from
/// re-reading each target's head version until its successor is published
/// (#2373). Every UPDATE path locks its rows through this type, so plain,
/// conditional and read-modify-write statements on one row serialize on the
/// same mutex and each successor is built from the version its predecessor
/// installed. Dropping the set returns the handles so idle entries leave the
/// lock table.
pub(super) struct UpdateRowLocks<'a> {
    runtime: &'a RedDBRuntime,
    table: &'a str,
    /// Sorted by key and deduplicated: concurrent statements take their rows
    /// in one global order and cannot deadlock on each other.
    entries: Vec<UpdateRowLock>,
}

struct UpdateRowLock {
    key: String,
    /// The first scan candidate (physical version) seen for the row.
    candidate_id: EntityId,
    lock: Arc<parking_lot::Mutex<()>>,
}

impl<'a> UpdateRowLocks<'a> {
    /// Lock handles for the logical rows behind the candidate physical ids
    /// of a target scan. Candidates that no longer exist are skipped.
    pub(super) fn for_candidates(
        runtime: &'a RedDBRuntime,
        table: &'a str,
        candidate_ids: &[EntityId],
    ) -> Self {
        let mut entries = Vec::with_capacity(candidate_ids.len());
        if let Some(manager) = runtime.inner.db.store().get_collection(table) {
            for id in candidate_ids {
                let Some(logical_id) = manager.get_with(*id, |entity| entity.logical_id()) else {
                    continue;
                };
                let key = format!("row:{}", logical_id.raw());
                let lock = runtime.inner.rmw_locks.lock_for(table, &key);
                entries.push(UpdateRowLock {
                    key,
                    candidate_id: *id,
                    lock,
                });
            }
        }
        // Stable sort, so `dedup_by` keeps the first candidate per row.
        entries.sort_by(|left, right| left.key.cmp(&right.key));
        entries.dedup_by(|later, earlier| later.key == earlier.key);
        Self {
            runtime,
            table,
            entries,
        }
    }

    /// Acquire every row lock, in key order. Callers must take the
    /// collection topology read guard first, as the UPDATE paths do.
    pub(super) fn lock(&self) -> Vec<parking_lot::MutexGuard<'_, ()>> {
        self.entries.iter().map(|entry| entry.lock.lock()).collect()
    }

    /// One scan candidate per locked row, in lock order.
    pub(super) fn candidate_ids(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.entries.iter().map(|entry| entry.candidate_id)
    }
}

impl Drop for UpdateRowLocks<'_> {
    fn drop(&mut self) {
        // Guards from `lock` borrow `self`, so every mutex is unlocked here.
        for entry in self.entries.drain(..) {
            self.runtime
                .inner
                .rmw_locks
                .release(self.table, &entry.key, entry.lock);
        }
    }
}

/// Resolver for the head version of rows an UPDATE holds per-row locks on
/// (#2373). Capture it after the locks are taken.
///
/// The successor's pre-image MUST be the LATEST COMMITTED version, evaluated
/// *now*: a compound assignment (`n = n + 1`) folds it into the written row,
/// and a conditional UPDATE compares against it. Two failure modes must both
/// be avoided:
///   * the physical version that currently carries `xmax == 0` can be another
///     transaction's still-uncommitted version — folding a later-aborted
///     write into committed state (dirty read).
///   * the pinned *statement* snapshot was captured before this statement
///     acquired the row locks, so it can miss a value a peer committed in the
///     meantime — every concurrent writer then reads the same stale pre-image
///     and installs its own successor (lost update, forked row).
///
/// A snapshot taken once every earlier writer of these rows has committed,
/// paired with this connection's own in-flight xids, threads both: it sees
/// the freshest committed version and this transaction's own writes, while
/// hiding every other transaction's uncommitted version.
pub(super) fn locked_update_head_resolver(
) -> crate::runtime::table_row_mvcc_resolver::TableRowMvccReadResolver {
    let context = crate::runtime::impl_core::capture_current_snapshot().map(|base| {
        crate::runtime::impl_core::SnapshotContext {
            snapshot: base.manager.fresh_read_snapshot(),
            manager: Arc::clone(&base.manager),
            own_xids: base.own_xids.clone(),
            requires_index_fallback: true,
            serializable_reader: base.serializable_reader,
        }
    });
    crate::runtime::table_row_mvcc_resolver::TableRowMvccReadResolver::captured(context)
}

/// The version of `candidate` an UPDATE holding its per-row lock must build
/// the successor from: the latest committed version (#2373). `candidate` is
/// a fresh read taken under the lock. While it is still the head it is
/// returned as is, which is the uncontended case; when a concurrent writer
/// superseded it after the target scan, the head is looked up by logical id.
/// `None` means no version is visible any more (the row was deleted).
///
/// Only table rows carry logical-id version chains here; other entity kinds
/// are returned unchanged.
pub(super) fn resolve_locked_update_head(
    runtime: &RedDBRuntime,
    table: &str,
    candidate: UnifiedEntity,
    head_resolver: &crate::runtime::table_row_mvcc_resolver::TableRowMvccReadResolver,
) -> Option<UnifiedEntity> {
    if !matches!(candidate.kind, EntityKind::TableRow { .. }) {
        return Some(candidate);
    }
    let store = runtime.inner.db.store();
    if !head_resolver.has_snapshot() {
        if candidate.xmax == 0 {
            return Some(candidate);
        }
        return store
            .get_table_row_by_logical_id(table, candidate.logical_id())
            .filter(|head| head.xmax == 0);
    }
    if head_resolver.resolve_candidate(&candidate).is_some() {
        return Some(candidate);
    }
    // An invisible first version of a row (physical id == logical id) with
    // no successor is another transaction's uncommitted insert: there is no
    // committed version to update, and no version chain worth scanning.
    if candidate.xmax == 0 && candidate.id == candidate.logical_id() {
        return None;
    }
    head_resolver.resolve_logical_id(&store, table, candidate.logical_id())
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
) -> Vec<EntityId> {
    let mut entities: Vec<UnifiedEntity> =
        manager.get_many(entity_ids).into_iter().flatten().collect();
    match limit {
        // Top-k path only when a LIMIT bounds the output.
        Some(limit) => {
            partial_top_k(&mut entities, limit, |left, right| {
                compare_update_order(left, right, order_by)
            });
        }
        None => entities.sort_by(|left, right| compare_update_order(left, right, order_by)),
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
