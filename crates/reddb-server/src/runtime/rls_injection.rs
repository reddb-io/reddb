//! Row-Level-Security policy injection & foreign-table filtering.
//!
//! Extracted verbatim from `impl_core.rs` (impl_core slice 4/10, issue #1625).
//! Houses the RLS injection family that PRD #1619 keeps out of the central
//! dispatch file:
//!
//! - **Policy-filter free fns** — `rls_policy_filter`,
//!   `rls_policy_filter_for_kind`, `rls_is_enabled`, `node_passes_rls`,
//!   `edge_passes_rls`, the query-rewrite injectors (`inject_rls_filters`,
//!   `inject_rls_into_join`, `collect_join_side_policy`), and
//!   `apply_foreign_table_filters`.
//! - **Runtime accessor methods** — `is_rls_enabled`, `matching_rls_policies`,
//!   `matching_rls_policies_for_kind`, and the coupled FDW accessor
//!   `foreign_tables`.
//!
//! Names, signatures and visibility are preserved so the central dispatch and
//! sibling-file callers (including `crate::runtime::impl_core::rls_*` paths)
//! need no edits; `impl_core` re-exports the externally-referenced fns.
use super::execution_context::current_auth_identity;
use super::*;

/// Combine matching RLS policies for a table + action into a single
/// `Filter` suitable for AND-ing into a caller's `WHERE` clause.
///
/// Returns `None` when RLS is disabled or no policy admits the caller's
/// role — callers use that to short-circuit the mutation (for DELETE /
/// UPDATE we simply skip the operation, which PG expresses as "no rows
/// match the policy + predicate combination").
pub(crate) fn rls_policy_filter(
    runtime: &RedDBRuntime,
    table: &str,
    action: crate::storage::query::ast::PolicyAction,
) -> Option<crate::storage::query::ast::Filter> {
    rls_policy_filter_for_kind(
        runtime,
        table,
        action,
        crate::storage::query::ast::PolicyTargetKind::Table,
    )
}

/// Kind-aware policy filter combiner (Phase 2.5.5 RLS universal).
/// Graph / vector / queue / timeseries scans pass the concrete kind;
/// policies targeting other kinds are ignored. Legacy Table-scoped
/// policies still apply cross-kind — callers register auto-tenancy
/// policies as Table today.
pub(crate) fn rls_policy_filter_for_kind(
    runtime: &RedDBRuntime,
    table: &str,
    action: crate::storage::query::ast::PolicyAction,
    kind: crate::storage::query::ast::PolicyTargetKind,
) -> Option<crate::storage::query::ast::Filter> {
    use crate::storage::query::ast::Filter;

    if !runtime.inner.rls_enabled_tables.read().contains(table) {
        return None;
    }
    let role = current_auth_identity().map(|(_, role)| role);
    let role_str = role.map(|r| r.as_str().to_string());
    let policies = runtime.matching_rls_policies_for_kind(table, role_str.as_deref(), action, kind);
    if policies.is_empty() {
        return None;
    }
    policies
        .into_iter()
        .reduce(|acc, f| Filter::Or(Box::new(acc), Box::new(f)))
}

/// Returns true when the table has RLS enforcement enabled. Convenience
/// shortcut so DML paths can gate the AND-combine work without reaching
/// into `runtime.inner.rls_enabled_tables` directly.
pub(crate) fn rls_is_enabled(runtime: &RedDBRuntime, table: &str) -> bool {
    runtime.inner.rls_enabled_tables.read().contains(table)
}

/// Per-entity gate used by the graph materialiser for `GraphNode`
/// entities. RLS is checked against the source collection with
/// `kind = Nodes`, which `matching_rls_policies_for_kind` resolves to
/// either `Nodes`-targeted policies or legacy `Table`-targeted ones
/// (for back-compat with auto-tenancy declarations). Cached per
/// collection so big graphs only resolve the policy chain once.
pub(crate) fn node_passes_rls(
    runtime: &RedDBRuntime,
    collection: &str,
    role: Option<&str>,
    cache: &mut std::collections::HashMap<String, Option<crate::storage::query::ast::Filter>>,
    entity: &crate::storage::unified::entity::UnifiedEntity,
) -> bool {
    use crate::storage::query::ast::{Filter, PolicyAction, PolicyTargetKind};

    if !runtime.inner.rls_enabled_tables.read().contains(collection) {
        return true;
    }
    let filter = cache.entry(collection.to_string()).or_insert_with(|| {
        let policies = runtime.matching_rls_policies_for_kind(
            collection,
            role,
            PolicyAction::Select,
            PolicyTargetKind::Nodes,
        );
        if policies.is_empty() {
            None
        } else {
            policies
                .into_iter()
                .reduce(|acc, f| Filter::Or(Box::new(acc), Box::new(f)))
        }
    });
    let Some(filter) = filter else {
        return false;
    };
    crate::runtime::query_exec::evaluate_entity_filter_with_db(
        Some(&runtime.inner.db),
        entity,
        filter,
        collection,
        collection,
    )
}

/// Edge counterpart of `node_passes_rls`. Same caching strategy with
/// `kind = Edges`.
pub(crate) fn edge_passes_rls(
    runtime: &RedDBRuntime,
    collection: &str,
    role: Option<&str>,
    cache: &mut std::collections::HashMap<String, Option<crate::storage::query::ast::Filter>>,
    entity: &crate::storage::unified::entity::UnifiedEntity,
) -> bool {
    use crate::storage::query::ast::{Filter, PolicyAction, PolicyTargetKind};

    if !runtime.inner.rls_enabled_tables.read().contains(collection) {
        return true;
    }
    let filter = cache.entry(collection.to_string()).or_insert_with(|| {
        let policies = runtime.matching_rls_policies_for_kind(
            collection,
            role,
            PolicyAction::Select,
            PolicyTargetKind::Edges,
        );
        if policies.is_empty() {
            None
        } else {
            policies
                .into_iter()
                .reduce(|acc, f| Filter::Or(Box::new(acc), Box::new(f)))
        }
    });
    let Some(filter) = filter else {
        return false;
    };
    crate::runtime::query_exec::evaluate_entity_filter_with_db(
        Some(&runtime.inner.db),
        entity,
        filter,
        collection,
        collection,
    )
}

/// RLS policy injection (Phase 2.5.2 PG parity).
///
/// Fetch every matching policy for the current thread-local role and
/// fold them into the query's filter. Semantics mirror PostgreSQL:
///
/// * Multiple policies on the same table combine with **OR** — a row is
///   visible if *any* policy admits it.
/// * The combined policy predicate is **AND**-ed into the caller's
///   existing `WHERE` clause so explicit predicates continue to trim
///   the policy-allowed set.
/// * No matching policies + RLS enabled = zero rows (PG's
///   restrictive-default). Callers get `None` and return an empty
///   `UnifiedResult` without ever dispatching the scan.
///
/// This runs only when `RuntimeInner::rls_enabled_tables` already
/// contains the table name — callers gate the hot path upfront to
/// avoid the lock acquisition on tables without RLS.
///
/// Returns `None` when no policy admits the current role; returns
/// `Some(mutated_table)` with policy filters folded in otherwise.
pub(crate) fn inject_rls_filters(
    runtime: &RedDBRuntime,
    frame: &dyn super::statement_frame::ReadFrame,
    mut table: crate::storage::query::ast::TableQuery,
) -> Option<crate::storage::query::ast::TableQuery> {
    use crate::storage::query::ast::{Filter, PolicyAction};

    // `None` role falls through to policies with no `TO role` clause.
    let role = frame.identity().map(|(_, role)| role);
    let role_str = role.map(|r| r.as_str().to_string());
    let policies =
        runtime.matching_rls_policies(&table.table, role_str.as_deref(), PolicyAction::Select);

    if policies.is_empty() {
        // RLS enabled + no policy match = deny everything. Signal the
        // caller to short-circuit with an empty result set.
        return None;
    }

    // Combine policy predicates with OR (PG's permissive default).
    let combined = policies
        .into_iter()
        .reduce(|acc, f| Filter::Or(Box::new(acc), Box::new(f)))
        .expect("policies non-empty");

    // AND into the caller's existing predicate. The predicate may live
    // in `where_expr` rather than `filter`: `resolve_table_expr_subqueries`
    // nulls `filter` whenever `where_expr` is present (the case for a
    // view body rewritten into `SELECT … WHERE …`). Folding only into
    // `filter` here would silently drop that `where_expr` predicate at
    // eval time because `effective_table_filter` prefers `filter` —
    // e.g. `WITHIN TENANT … SELECT * FROM <view>` would apply the
    // tenant policy but lose the view's own WHERE (#635).
    use crate::storage::query::sql_lowering::{expr_to_filter, filter_to_expr};
    let had_where_expr = table.where_expr.is_some();
    let existing = table
        .filter
        .take()
        .or_else(|| table.where_expr.as_ref().map(expr_to_filter));
    let new_filter = match existing {
        Some(existing) => Filter::And(Box::new(existing), Box::new(combined)),
        None => combined,
    };
    // Keep `where_expr` in lock-step with the merged `filter` so
    // whichever the executor consults sees the full predicate.
    if had_where_expr {
        table.where_expr = Some(filter_to_expr(&new_filter));
    }
    table.filter = Some(new_filter);
    Some(table)
}

/// Apply per-table RLS to a `JoinQuery` by folding each side's policy
/// predicate into the join's outer filter. Walking the merged record
/// at the join layer (rather than mutating the per-side scan filter)
/// keeps the planner's strategy choice and per-side index selection
/// undisturbed — the policy predicate uses the qualified `t.col` form
/// that resolves cleanly against the merged record's keys.
///
/// Returns `None` when any leaf has RLS enabled and no policy admits
/// the caller — the join short-circuits to an empty result.
pub(crate) fn inject_rls_into_join(
    runtime: &RedDBRuntime,
    frame: &dyn super::statement_frame::ReadFrame,
    mut join: crate::storage::query::ast::JoinQuery,
) -> Option<crate::storage::query::ast::JoinQuery> {
    use crate::storage::query::ast::Filter;

    let mut policy_filters: Vec<Filter> = Vec::new();
    if !collect_join_side_policy(runtime, frame, join.left.as_ref(), &mut policy_filters) {
        return None;
    }
    if !collect_join_side_policy(runtime, frame, join.right.as_ref(), &mut policy_filters) {
        return None;
    }

    if policy_filters.is_empty() {
        return Some(join);
    }

    let combined = policy_filters
        .into_iter()
        .reduce(|acc, f| Filter::And(Box::new(acc), Box::new(f)))
        .expect("policy_filters non-empty");

    join.filter = Some(match join.filter.take() {
        Some(existing) => Filter::And(Box::new(existing), Box::new(combined)),
        None => combined,
    });

    Some(join)
}

/// For each `Table` leaf reachable through nested joins, append the
/// RLS-policy filter (combined with OR across that side's matching
/// policies) into `out`. Returns `false` when a side has RLS enabled
/// but no policy admits the caller — the join must short-circuit.
fn collect_join_side_policy(
    runtime: &RedDBRuntime,
    frame: &dyn super::statement_frame::ReadFrame,
    expr: &crate::storage::query::ast::QueryExpr,
    out: &mut Vec<crate::storage::query::ast::Filter>,
) -> bool {
    use crate::storage::query::ast::{Filter, PolicyAction, QueryExpr};
    match expr {
        QueryExpr::Table(t) => {
            if !runtime.inner.rls_enabled_tables.read().contains(&t.table) {
                return true;
            }
            let role = frame.identity().map(|(_, role)| role);
            let role_str = role.map(|r| r.as_str().to_string());
            let policies =
                runtime.matching_rls_policies(&t.table, role_str.as_deref(), PolicyAction::Select);
            if policies.is_empty() {
                return false;
            }
            let combined = policies
                .into_iter()
                .reduce(|acc, f| Filter::Or(Box::new(acc), Box::new(f)))
                .expect("policies non-empty");
            out.push(combined);
            true
        }
        QueryExpr::Join(inner) => {
            collect_join_side_policy(runtime, frame, inner.left.as_ref(), out)
                && collect_join_side_policy(runtime, frame, inner.right.as_ref(), out)
        }
        _ => true,
    }
}

/// Foreign-table post-scan filter application (Phase 3.2.2 PG parity).
///
/// Phase 3.2 FDW wrappers don't advertise filter pushdown, so the runtime
/// applies `WHERE` / `ORDER BY` / `LIMIT` / `OFFSET` after the wrapper
/// materialises all rows. Projections are best-effort — when the query
/// lists explicit columns we keep only those; a `SELECT *` keeps every
/// wrapper-emitted field verbatim.
///
/// When a wrapper later opts into pushdown (`supports_pushdown = true`)
/// the runtime will pass the compiled filter down instead of post-filtering.
pub(crate) fn apply_foreign_table_filters(
    records: Vec<crate::storage::query::unified::UnifiedRecord>,
    query: &crate::storage::query::ast::TableQuery,
) -> crate::storage::query::unified::UnifiedResult {
    use crate::storage::query::sql_lowering::{
        effective_table_filter, effective_table_projections,
    };
    use crate::storage::query::unified::UnifiedResult;

    let filter = effective_table_filter(query);
    let projections = effective_table_projections(query);

    // Step 1 — WHERE. Reuse the cross-store evaluator so the semantics
    // match native-collection queries (same operators, same NULL handling).
    let mut filtered: Vec<_> = records
        .into_iter()
        .filter(|record| match &filter {
            Some(f) => {
                super::join_filter::evaluate_runtime_filter_with_db(None, record, f, None, None)
            }
            None => true,
        })
        .collect();

    // Step 2 — LIMIT / OFFSET. Applied after filter to match SQL semantics.
    if let Some(offset) = query.offset {
        let offset = offset as usize;
        if offset >= filtered.len() {
            filtered.clear();
        } else {
            filtered.drain(0..offset);
        }
    }
    if let Some(limit) = query.limit {
        filtered.truncate(limit as usize);
    }

    // Step 3 — columns list. `SELECT *` (no explicit projections) keeps
    // the wrapper's column set; an explicit list trims to those names.
    let columns: Vec<String> = if projections.is_empty() {
        filtered
            .first()
            .map(|r| r.column_names().iter().map(|k| k.to_string()).collect())
            .unwrap_or_default()
    } else {
        projections
            .iter()
            .map(super::join_filter::projection_name)
            .collect()
    };

    let mut result = UnifiedResult::empty();
    result.columns = columns;
    result.records = filtered;
    result
}

impl RedDBRuntime {
    /// Access the shared `ForeignTableRegistry` (Phase 3.2 PG parity).
    ///
    /// Callers use this to check whether a table name is a registered
    /// foreign table (`registry.is_foreign_table(name)`) and, if so, to
    /// scan it (`registry.scan(name)`). The read-path rewriter consults
    /// this before dispatching into native-collection lookup.
    pub fn foreign_tables(&self) -> Arc<crate::storage::fdw::ForeignTableRegistry> {
        Arc::clone(&self.inner.foreign_tables)
    }

    /// Is Row-Level Security enabled for this table? (Phase 2.5 PG parity)
    pub fn is_rls_enabled(&self, table: &str) -> bool {
        self.inner.rls_enabled_tables.read().contains(table)
    }

    /// Collect the USING predicates that apply to this `(table, role, action)`.
    ///
    /// Returned filters should be OR-combined (a row passes RLS when *any*
    /// matching policy accepts it) and then AND-ed into the query's WHERE.
    /// When the table has RLS disabled this returns an empty Vec — callers
    /// can fast-path back to the unfiltered read.
    pub fn matching_rls_policies(
        &self,
        table: &str,
        role: Option<&str>,
        action: crate::storage::query::ast::PolicyAction,
    ) -> Vec<crate::storage::query::ast::Filter> {
        // Default kind = Table preserves the pre-Phase-2.5.5 behaviour:
        // callers that don't name a kind only see Table-scoped
        // policies (which is what execute SELECT / UPDATE / DELETE
        // expect).
        self.matching_rls_policies_for_kind(
            table,
            role,
            action,
            crate::storage::query::ast::PolicyTargetKind::Table,
        )
    }

    /// Kind-aware variant used by cross-model scans (Phase 2.5.5).
    ///
    /// Graph scans request `Nodes` / `Edges`, vector ANN requests
    /// `Vectors`, queue consumers request `Messages`, and timeseries
    /// range scans request `Points`. Policies tagged with a
    /// different kind are skipped so a graph-scoped policy doesn't
    /// accidentally gate a table SELECT on the same collection.
    pub fn matching_rls_policies_for_kind(
        &self,
        table: &str,
        role: Option<&str>,
        action: crate::storage::query::ast::PolicyAction,
        kind: crate::storage::query::ast::PolicyTargetKind,
    ) -> Vec<crate::storage::query::ast::Filter> {
        if !self.is_rls_enabled(table) {
            return Vec::new();
        }
        let policies = self.inner.rls_policies.read();
        policies
            .iter()
            .filter_map(|((t, _), p)| {
                if t != table {
                    return None;
                }
                // Kind gate — Table policies also apply to every
                // other kind *iff* the policy predicate evaluates
                // against entity fields that exist uniformly; the
                // caller's kind filter is the stricter check, so
                // match literally. Auto-tenancy policies stamp
                // Table and the caller passes the concrete kind —
                // we allow Table policies to apply cross-kind for
                // backwards compat.
                if p.target_kind != kind
                    && p.target_kind != crate::storage::query::ast::PolicyTargetKind::Table
                {
                    return None;
                }
                // Action gate — `None` means "ALL" actions.
                if let Some(a) = p.action {
                    if a != action {
                        return None;
                    }
                }
                // Role gate — `None` means "any role".
                if let Some(p_role) = p.role.as_deref() {
                    match role {
                        Some(r) if r == p_role => {}
                        _ => return None,
                    }
                }
                Some((*p.using).clone())
            })
            .collect()
    }
}
