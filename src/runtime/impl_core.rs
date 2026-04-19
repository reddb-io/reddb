use super::*;
use crate::application::entity::metadata_to_json;
use crate::replication::cdc::ChangeRecord;
use crate::replication::logical::{ApplyMode, LogicalChangeApplier};

thread_local! {
    /// Current connection id for the executing statement. Set by the
    /// per-connection wrapper (stdio/gRPC handlers) before dispatching
    /// into `execute_query`; falls back to `0` for embedded callers.
    static CURRENT_CONN_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };

    /// Authenticated user + role for the executing statement (Phase 2.5.2
    /// RLS enforcement). Set by the transport middleware after validating
    /// credentials (password / cert / oauth); unset means "anonymous" /
    /// "embedded" — RLS policies degrade to the role-agnostic subset.
    ///
    /// `None` skips RLS injection entirely; `Some((username, role))`
    /// passes `role` to `matching_rls_policies(table, Some(role), action)`.
    static CURRENT_AUTH_IDENTITY: std::cell::RefCell<Option<(String, crate::auth::Role)>> =
        const { std::cell::RefCell::new(None) };

    /// MVCC snapshot scoped to the currently-executing statement (Phase
    /// 2.3.2d PG parity). `execute_query` captures it on entry and drops
    /// it on exit; every scan consults it via
    /// `entity_visible_under_current_snapshot` to hide tuples whose xmin
    /// hasn't committed or whose xmax already has.
    ///
    /// `None` means "pre-MVCC semantics" — the read path returns every
    /// tuple regardless of xmin/xmax. All embedded callers that bypass
    /// `execute_query` see this default.
    static CURRENT_SNAPSHOT: std::cell::RefCell<Option<SnapshotContext>> =
        const { std::cell::RefCell::new(None) };

    /// Session-scoped tenant id for the current connection (Phase 2.5.3
    /// multi-tenancy). Populated by `SET TENANT 'id'` or by transport
    /// middleware after resolving tenant from auth claims. Read by the
    /// `CURRENT_TENANT()` scalar function — RLS policies typically
    /// combine it as `USING (tenant_id = CURRENT_TENANT())` to scope
    /// every query to one tenant.
    ///
    /// `None` means "no tenant bound" — `CURRENT_TENANT()` returns
    /// NULL, and RLS policies that gate on it hide every row.
    static CURRENT_TENANT_ID: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Snapshot + manager pair used for read-path visibility checks.
///
/// The manager is needed in addition to the snapshot because `aborted`
/// state mutates after the snapshot is captured — a ROLLBACK by a
/// committed-at-capture-time writer must still hide its tuples. Keeping
/// the Arc around is O(pointer) and the RwLock reads on `is_aborted`
/// are cheap (HashSet lookup under a parking_lot read guard).
///
/// `own_xids` (Phase 2.3.2e) lists the xids belonging to the current
/// connection's transaction — the parent xid plus every open
/// savepoint sub-xid. The visibility rule promotes rows stamped with
/// these xids to "always visible (unless aborted)" so the writer sees
/// its own nested-savepoint writes even though their xids exceed
/// `snapshot.xid`.
#[derive(Clone)]
pub struct SnapshotContext {
    pub snapshot: crate::storage::transaction::snapshot::Snapshot,
    pub manager: Arc<crate::storage::transaction::snapshot::SnapshotManager>,
    pub own_xids: std::collections::HashSet<crate::storage::transaction::snapshot::Xid>,
}

/// Install a connection id on the current thread for the duration of a
/// statement. Transaction state (`RuntimeInner::tx_contexts`) is keyed
/// by this id so different connections can hold independent BEGINs.
///
/// Pub so transports (PG wire, gRPC, HTTP per-request spawners) and
/// tests can emulate per-connection isolation. Call it once when
/// binding the connection's worker thread; pair with
/// `clear_current_connection_id` on teardown.
pub fn set_current_connection_id(id: u64) {
    CURRENT_CONN_ID.with(|c| c.set(id));
}

/// Reset the thread's connection id back to `0` (autocommit).
pub fn clear_current_connection_id() {
    CURRENT_CONN_ID.with(|c| c.set(0));
}

/// Read the connection id set by `set_current_connection_id`. Returns
/// `0` when no wrapper installed one — auto-commit path.
pub fn current_connection_id() -> u64 {
    CURRENT_CONN_ID.with(|c| c.get())
}

/// Install the authenticated identity for the current thread (Phase 2.5.2
/// RLS enforcement). Transport layers call this right after resolving
/// auth so the query dispatch can fold RLS policies into the filter.
pub fn set_current_auth_identity(username: String, role: crate::auth::Role) {
    CURRENT_AUTH_IDENTITY.with(|cell| *cell.borrow_mut() = Some((username, role)));
}

/// Clear the thread-local auth identity. Transports call this after the
/// statement completes so pooled threads don't leak identities across
/// requests.
pub fn clear_current_auth_identity() {
    CURRENT_AUTH_IDENTITY.with(|cell| *cell.borrow_mut() = None);
}

/// Read the current-thread auth identity. `None` when no transport
/// installed one (embedded mode / anonymous access).
pub(crate) fn current_auth_identity() -> Option<(String, crate::auth::Role)> {
    CURRENT_AUTH_IDENTITY.with(|cell| cell.borrow().clone())
}

/// Install the session tenant id for the current thread (Phase 2.5.3
/// multi-tenancy). Called by `SET TENANT 'id'` dispatch and by
/// transport middleware that resolves tenant from auth claims (e.g.
/// JWT `tenant` claim, HTTP header, subdomain).
pub fn set_current_tenant(tenant_id: String) {
    CURRENT_TENANT_ID.with(|cell| *cell.borrow_mut() = Some(tenant_id));
}

/// Clear the current-thread tenant — `CURRENT_TENANT()` will then
/// return NULL and any RLS policy gated on it will hide every row.
pub fn clear_current_tenant() {
    CURRENT_TENANT_ID.with(|cell| *cell.borrow_mut() = None);
}

/// Read the current-thread tenant id, applying overrides in priority order:
///   1. `WITHIN TENANT '<id>' …` per-statement override (highest)
///   2. `SET LOCAL TENANT '<id>'` transaction-local override (consulted
///      only when the current connection has an open transaction)
///   3. `SET TENANT '<id>'` session-level thread-local
///   4. `None` (deny-default for RLS).
///
/// The transaction-local layer is read through the runtime; an embedded
/// helper crate that has no `RedDBRuntime` access still gets correct
/// behaviour for layers 1, 3, and 4.
pub fn current_tenant() -> Option<String> {
    let inherited = CURRENT_TENANT_ID.with(|cell| cell.borrow().clone());
    if let Some(over) = current_scope_override() {
        if over.tenant.is_active() {
            return over.tenant.resolve(inherited);
        }
    }
    if let Some(tx_local) = current_tx_local_tenant() {
        return tx_local;
    }
    inherited
}

thread_local! {
    /// Snapshot of the active connection's `tx_local_tenants` entry for
    /// the current `execute_query` call. Outer `Some(_)` means "a
    /// transaction-local tenant override is active for this call";
    /// inner is the override's value (`Some(s)` overrides to `s`,
    /// `None` overrides to NULL/cleared). Refreshed at the top of every
    /// `execute_query` invocation and cleared by the RAII guard on
    /// return so pooled connections cannot leak the override past the
    /// statement that owns it.
    static TX_LOCAL_TENANT: std::cell::RefCell<Option<Option<String>>> =
        const { std::cell::RefCell::new(None) };
}

fn current_tx_local_tenant() -> Option<Option<String>> {
    TX_LOCAL_TENANT.with(|cell| cell.borrow().clone())
}

/// Recognise `SET LOCAL TENANT '<id>'` / `SET LOCAL TENANT NULL` —
/// returns `Ok(Some(Some(id)))` for an explicit value, `Ok(Some(None))`
/// for an explicit NULL clear, `Ok(None)` when the input is not a
/// `SET LOCAL TENANT` statement at all, and `Err` when the prefix
/// matches but the value is malformed.
fn parse_set_local_tenant(query: &str) -> RedDBResult<Option<Option<String>>> {
    let mut tokens = query.split_ascii_whitespace();
    let Some(w1) = tokens.next() else {
        return Ok(None);
    };
    if !w1.eq_ignore_ascii_case("SET") {
        return Ok(None);
    }
    let Some(w2) = tokens.next() else {
        return Ok(None);
    };
    if !w2.eq_ignore_ascii_case("LOCAL") {
        return Ok(None);
    }
    let Some(w3) = tokens.next() else {
        return Ok(None);
    };
    if !w3.eq_ignore_ascii_case("TENANT") {
        return Ok(None);
    }
    let rest: String = tokens.collect::<Vec<_>>().join(" ");
    let rest = rest.trim().trim_end_matches(';').trim();
    let value_str = rest.strip_prefix('=').map(|s| s.trim()).unwrap_or(rest);
    if value_str.is_empty() {
        return Err(RedDBError::Query(
            "SET LOCAL TENANT expects a string literal or NULL".to_string(),
        ));
    }
    if value_str.eq_ignore_ascii_case("NULL") {
        return Ok(Some(None));
    }
    if value_str.starts_with('\'') && value_str.ends_with('\'') && value_str.len() >= 2 {
        let inner = &value_str[1..value_str.len() - 1];
        return Ok(Some(Some(inner.to_string())));
    }
    Err(RedDBError::Query(format!(
        "SET LOCAL TENANT expects a string literal or NULL, got `{value_str}`"
    )))
}

pub(crate) struct TxLocalTenantGuard;

impl TxLocalTenantGuard {
    pub fn install(value: Option<Option<String>>) -> Self {
        TX_LOCAL_TENANT.with(|cell| *cell.borrow_mut() = value);
        Self
    }
}

impl Drop for TxLocalTenantGuard {
    fn drop(&mut self) {
        TX_LOCAL_TENANT.with(|cell| *cell.borrow_mut() = None);
    }
}

thread_local! {
    /// Stack of `WITHIN ... <stmt>` overrides active on the current
    /// thread. Every entry corresponds to one in-flight `execute_query`
    /// call that started with a `WITHIN` prefix; the entry is pushed
    /// before dispatch and popped before the call returns. The stack
    /// shape supports nested invocations (e.g. a view body that itself
    /// re-enters execute_query).
    static SCOPE_OVERRIDES: std::cell::RefCell<Vec<crate::runtime::within_clause::ScopeOverride>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

pub(crate) fn push_scope_override(over: crate::runtime::within_clause::ScopeOverride) {
    SCOPE_OVERRIDES.with(|cell| cell.borrow_mut().push(over));
}

pub(crate) fn pop_scope_override() {
    SCOPE_OVERRIDES.with(|cell| {
        cell.borrow_mut().pop();
    });
}

pub(crate) fn current_scope_override() -> Option<crate::runtime::within_clause::ScopeOverride> {
    SCOPE_OVERRIDES.with(|cell| cell.borrow().last().cloned())
}

/// Cheap probe: is any `WITHIN …` scope override active on this
/// thread? The fast-path needs to know without paying for the full
/// `.last().cloned()` allocation — just peek at stack length.
pub(crate) fn has_scope_override_active() -> bool {
    SCOPE_OVERRIDES.with(|cell| !cell.borrow().is_empty())
}

/// RAII guard pairing `push_scope_override` with the matching pop, so
/// the stack stays balanced even when the inner `execute_query` returns
/// early via `?`.
pub(crate) struct ScopeOverrideGuard;

impl ScopeOverrideGuard {
    pub fn install(over: crate::runtime::within_clause::ScopeOverride) -> Self {
        push_scope_override(over);
        Self
    }
}

impl Drop for ScopeOverrideGuard {
    fn drop(&mut self) {
        pop_scope_override();
    }
}

/// Read the current-thread auth identity, honouring per-statement
/// `WITHIN ... USER '<u>' AS ROLE '<r>'` overrides. The override only
/// supplies projected strings — it never grants additional privilege —
/// so callers that need to make authorisation decisions must read from
/// the underlying `current_auth_identity()` directly.
pub(crate) fn current_user_projected() -> Option<String> {
    let inherited = current_auth_identity().map(|(u, _)| u);
    if let Some(over) = current_scope_override() {
        if over.user.is_active() {
            return over.user.resolve(inherited);
        }
    }
    inherited
}

pub(crate) fn current_role_projected() -> Option<String> {
    let inherited = current_auth_identity().map(|(_, r)| format!("{r:?}").to_lowercase());
    if let Some(over) = current_scope_override() {
        if over.role.is_active() {
            return over.role.resolve(inherited);
        }
    }
    inherited
}

/// Install the MVCC snapshot used by the current thread for the duration
/// of one statement. Paired with `clear_current_snapshot()` — callers
/// should prefer the `CurrentSnapshotGuard` RAII wrapper so early returns
/// still clean up.
pub fn set_current_snapshot(ctx: SnapshotContext) {
    CURRENT_SNAPSHOT.with(|cell| *cell.borrow_mut() = Some(ctx));
}

pub fn clear_current_snapshot() {
    CURRENT_SNAPSHOT.with(|cell| *cell.borrow_mut() = None);
}

/// Drop-guard that restores the previous snapshot on scope exit. Safe to
/// nest — each statement saves the caller's snapshot and puts it back
/// instead of blindly clearing, so a top-level `execute_query` called
/// from inside another statement dispatch (e.g. vector source subqueries)
/// doesn't strip visibility from the outer scan.
pub(crate) struct CurrentSnapshotGuard {
    previous: Option<SnapshotContext>,
}

impl CurrentSnapshotGuard {
    pub(crate) fn install(ctx: SnapshotContext) -> Self {
        let previous = CURRENT_SNAPSHOT.with(|cell| cell.borrow().clone());
        set_current_snapshot(ctx);
        Self { previous }
    }
}

impl Drop for CurrentSnapshotGuard {
    fn drop(&mut self) {
        CURRENT_SNAPSHOT.with(|cell| *cell.borrow_mut() = self.previous.take());
    }
}

/// Is this entity visible under the current thread's MVCC snapshot?
///
/// Returns `true` (no filtering) when no snapshot is installed — that
/// path is used by embedded callers and by operations that intentionally
/// bypass MVCC (VACUUM, snapshot export, admin introspection).
///
/// When a snapshot is installed the result is
///   `snapshot.sees(xmin, xmax) && !mgr.is_aborted(xmin) && !xmax_half_abort`
/// where `xmax_half_abort` re-grants visibility for tuples whose
/// deleting transaction rolled back.
#[inline]
pub fn entity_visible_under_current_snapshot(
    entity: &crate::storage::unified::entity::UnifiedEntity,
) -> bool {
    CURRENT_SNAPSHOT.with(|cell| {
        let guard = cell.borrow();
        let Some(ctx) = guard.as_ref() else {
            return true;
        };
        visibility_check(ctx, entity.xmin, entity.xmax)
    })
}

/// Direct visibility check from raw `(xmin, xmax)` — bypasses the
/// entity borrow for callers that already decomposed the tuple (e.g.
/// pre-materialized scan caches). Same semantics as
/// `entity_visible_under_current_snapshot`.
#[inline]
pub(crate) fn xids_visible_under_current_snapshot(xmin: u64, xmax: u64) -> bool {
    CURRENT_SNAPSHOT.with(|cell| {
        let guard = cell.borrow();
        let Some(ctx) = guard.as_ref() else {
            return true;
        };
        visibility_check(ctx, xmin, xmax)
    })
}

/// Clone the current thread's snapshot context. Parallel scan paths
/// (`query_all_zoned` with `std::thread::scope`) call this on the main
/// thread *before* spawning workers so the captured `SnapshotContext`
/// can be moved into every worker closure. Worker threads do not
/// inherit thread-locals, so calling `entity_visible_under_current_snapshot`
/// from inside a spawned closure would silently skip the filter.
pub fn capture_current_snapshot() -> Option<SnapshotContext> {
    CURRENT_SNAPSHOT.with(|cell| cell.borrow().clone())
}

/// Apply the same visibility rules used by the thread-local helpers
/// against a caller-provided context. Intended for parallel workers
/// that captured the snapshot with `capture_current_snapshot()`.
#[inline]
pub fn entity_visible_with_context(
    ctx: Option<&SnapshotContext>,
    entity: &crate::storage::unified::entity::UnifiedEntity,
) -> bool {
    match ctx {
        Some(ctx) => visibility_check(ctx, entity.xmin, entity.xmax),
        None => true,
    }
}

#[inline]
fn visibility_check(ctx: &SnapshotContext, xmin: u64, xmax: u64) -> bool {
    // Writer aborted → tuple never existed from any future reader's view.
    // Checked *before* the own-xids fast path so an aborted own-sub-xid
    // (rolled-back savepoint) stays hidden from the parent.
    if xmin != 0 && ctx.manager.is_aborted(xmin) {
        return false;
    }
    // Deleter aborted → treat xmax as unset; fall back to xmin-only check.
    let effective_xmax = if xmax != 0 && ctx.manager.is_aborted(xmax) {
        0
    } else {
        xmax
    };
    // Phase 2.3.2e: own-tx writes are always visible to the connection
    // that stamped them, even when xmin/xmax exceed `snapshot.xid` (as
    // happens for sub-xids allocated by SAVEPOINT after BEGIN).
    let own_xmin = xmin != 0 && ctx.own_xids.contains(&xmin);
    let own_xmax = effective_xmax != 0 && ctx.own_xids.contains(&effective_xmax);
    if own_xmax {
        // This connection deleted the row via this xid — hide it from self.
        return false;
    }
    if own_xmin {
        return true;
    }
    ctx.snapshot.sees(xmin, effective_xmax)
}

fn runtime_pool_lock(runtime: &RedDBRuntime) -> std::sync::MutexGuard<'_, PoolState> {
    runtime
        .inner
        .pool
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn cache_scope_insert(scopes: &mut HashSet<String>, name: &str) {
    if name.is_empty() || name.starts_with("__subq_") || is_universal_query_source(name) {
        return;
    }
    scopes.insert(name.to_string());
}

fn collect_table_source_scopes(scopes: &mut HashSet<String>, query: &TableQuery) {
    match query.source.as_ref() {
        Some(crate::storage::query::ast::TableSource::Name(name)) => {
            cache_scope_insert(scopes, name)
        }
        Some(crate::storage::query::ast::TableSource::Subquery(subquery)) => {
            collect_query_expr_result_cache_scopes(scopes, subquery);
        }
        None => cache_scope_insert(scopes, &query.table),
    }
}

fn collect_vector_source_scopes(
    scopes: &mut HashSet<String>,
    source: &crate::storage::query::ast::VectorSource,
) {
    match source {
        crate::storage::query::ast::VectorSource::Reference { collection, .. } => {
            cache_scope_insert(scopes, collection);
        }
        crate::storage::query::ast::VectorSource::Subquery(subquery) => {
            collect_query_expr_result_cache_scopes(scopes, subquery);
        }
        crate::storage::query::ast::VectorSource::Literal(_)
        | crate::storage::query::ast::VectorSource::Text(_) => {}
    }
}

fn collect_path_selector_scopes(
    scopes: &mut HashSet<String>,
    selector: &crate::storage::query::ast::NodeSelector,
) {
    if let crate::storage::query::ast::NodeSelector::ByRow { table, .. } = selector {
        cache_scope_insert(scopes, table);
    }
}

fn collect_query_expr_result_cache_scopes(scopes: &mut HashSet<String>, expr: &QueryExpr) {
    match expr {
        QueryExpr::Table(query) => collect_table_source_scopes(scopes, query),
        QueryExpr::Join(query) => {
            collect_query_expr_result_cache_scopes(scopes, &query.left);
            collect_query_expr_result_cache_scopes(scopes, &query.right);
        }
        QueryExpr::Path(query) => {
            collect_path_selector_scopes(scopes, &query.from);
            collect_path_selector_scopes(scopes, &query.to);
        }
        QueryExpr::Vector(query) => {
            cache_scope_insert(scopes, &query.collection);
            collect_vector_source_scopes(scopes, &query.query_vector);
        }
        QueryExpr::Hybrid(query) => {
            collect_query_expr_result_cache_scopes(scopes, &query.structured);
            cache_scope_insert(scopes, &query.vector.collection);
            collect_vector_source_scopes(scopes, &query.vector.query_vector);
        }
        QueryExpr::Insert(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::Update(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::Delete(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::CreateTable(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropTable(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::AlterTable(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::CreateIndex(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::DropIndex(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::CreateTimeSeries(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropTimeSeries(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::CreateQueue(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropQueue(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::QueueCommand(query) => match query {
            QueueCommand::Push { queue, .. }
            | QueueCommand::Pop { queue, .. }
            | QueueCommand::Peek { queue, .. }
            | QueueCommand::Len { queue }
            | QueueCommand::Purge { queue }
            | QueueCommand::GroupCreate { queue, .. }
            | QueueCommand::GroupRead { queue, .. }
            | QueueCommand::Pending { queue, .. }
            | QueueCommand::Claim { queue, .. }
            | QueueCommand::Ack { queue, .. }
            | QueueCommand::Nack { queue, .. } => cache_scope_insert(scopes, queue),
        },
        QueryExpr::CreateTree(query) => cache_scope_insert(scopes, &query.collection),
        QueryExpr::DropTree(query) => cache_scope_insert(scopes, &query.collection),
        QueryExpr::TreeCommand(query) => match query {
            TreeCommand::Insert { collection, .. }
            | TreeCommand::Move { collection, .. }
            | TreeCommand::Delete { collection, .. }
            | TreeCommand::Validate { collection, .. }
            | TreeCommand::Rebalance { collection, .. } => cache_scope_insert(scopes, collection),
        },
        QueryExpr::SearchCommand(query) => match query {
            SearchCommand::Similar { collection, .. }
            | SearchCommand::Hybrid { collection, .. }
            | SearchCommand::SpatialRadius { collection, .. }
            | SearchCommand::SpatialBbox { collection, .. }
            | SearchCommand::SpatialNearest { collection, .. } => {
                cache_scope_insert(scopes, collection);
            }
            SearchCommand::Text { collection, .. }
            | SearchCommand::Multimodal { collection, .. }
            | SearchCommand::Index { collection, .. }
            | SearchCommand::Context { collection, .. } => {
                if let Some(collection) = collection.as_deref() {
                    cache_scope_insert(scopes, collection);
                }
            }
        },
        QueryExpr::Ask(query) => {
            if let Some(collection) = query.collection.as_deref() {
                cache_scope_insert(scopes, collection);
            }
        }
        QueryExpr::ExplainAlter(query) => cache_scope_insert(scopes, &query.target.name),
        QueryExpr::MaintenanceCommand(cmd) => match cmd {
            crate::storage::query::ast::MaintenanceCommand::Vacuum { target, .. }
            | crate::storage::query::ast::MaintenanceCommand::Analyze { target } => {
                if let Some(t) = target {
                    cache_scope_insert(scopes, t);
                }
            }
        },
        QueryExpr::CopyFrom(cmd) => cache_scope_insert(scopes, &cmd.table),
        QueryExpr::CreateView(cmd) => {
            cache_scope_insert(scopes, &cmd.name);
            // Invalidating the view should also invalidate its dependencies.
            collect_query_expr_result_cache_scopes(scopes, &cmd.query);
        }
        QueryExpr::DropView(cmd) => cache_scope_insert(scopes, &cmd.name),
        QueryExpr::RefreshMaterializedView(cmd) => cache_scope_insert(scopes, &cmd.name),
        QueryExpr::CreatePolicy(cmd) => cache_scope_insert(scopes, &cmd.table),
        QueryExpr::DropPolicy(cmd) => cache_scope_insert(scopes, &cmd.table),
        QueryExpr::CreateServer(_) | QueryExpr::DropServer(_) => {}
        QueryExpr::CreateForeignTable(cmd) => cache_scope_insert(scopes, &cmd.name),
        QueryExpr::DropForeignTable(cmd) => cache_scope_insert(scopes, &cmd.name),
        QueryExpr::Graph(_)
        | QueryExpr::GraphCommand(_)
        | QueryExpr::ProbabilisticCommand(_)
        | QueryExpr::SetConfig { .. }
        | QueryExpr::ShowConfig { .. }
        | QueryExpr::SetTenant(_)
        | QueryExpr::ShowTenant
        | QueryExpr::TransactionControl(_)
        | QueryExpr::CreateSchema(_)
        | QueryExpr::DropSchema(_)
        | QueryExpr::CreateSequence(_)
        | QueryExpr::DropSequence(_) => {}
    }
}

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
fn node_passes_rls(
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
fn edge_passes_rls(
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
fn inject_rls_filters(
    runtime: &RedDBRuntime,
    mut table: crate::storage::query::ast::TableQuery,
) -> Option<crate::storage::query::ast::TableQuery> {
    use crate::storage::query::ast::{Filter, PolicyAction};

    // `None` role falls through to policies with no `TO role` clause.
    let role = current_auth_identity().map(|(_, role)| role);
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

    // AND into the caller's existing filter.
    table.filter = Some(match table.filter.take() {
        Some(existing) => Filter::And(Box::new(existing), Box::new(combined)),
        None => combined,
    });
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
fn inject_rls_into_join(
    runtime: &RedDBRuntime,
    mut join: crate::storage::query::ast::JoinQuery,
) -> Option<crate::storage::query::ast::JoinQuery> {
    use crate::storage::query::ast::Filter;

    let mut policy_filters: Vec<Filter> = Vec::new();
    if !collect_join_side_policy(runtime, join.left.as_ref(), &mut policy_filters) {
        return None;
    }
    if !collect_join_side_policy(runtime, join.right.as_ref(), &mut policy_filters) {
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
    expr: &crate::storage::query::ast::QueryExpr,
    out: &mut Vec<crate::storage::query::ast::Filter>,
) -> bool {
    use crate::storage::query::ast::{Filter, PolicyAction, QueryExpr};
    match expr {
        QueryExpr::Table(t) => {
            if !runtime.inner.rls_enabled_tables.read().contains(&t.table) {
                return true;
            }
            let role = current_auth_identity().map(|(_, role)| role);
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
            collect_join_side_policy(runtime, inner.left.as_ref(), out)
                && collect_join_side_policy(runtime, inner.right.as_ref(), out)
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
fn apply_foreign_table_filters(
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
            .map(|r| r.values.keys().map(|k| k.to_string()).collect())
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

/// Collect every concrete table reference inside a `QueryExpr`.
///
/// Used by view bookkeeping (dependency tracking for materialised
/// invalidation) and any other rewriter that needs to know the base
/// tables a query pulls from. Does not descend into projections/filters;
/// only the `FROM` side.
pub(crate) fn collect_table_refs(expr: &QueryExpr) -> Vec<String> {
    let mut scopes: HashSet<String> = HashSet::new();
    collect_query_expr_result_cache_scopes(&mut scopes, expr);
    scopes.into_iter().collect()
}

fn query_expr_result_cache_scopes(expr: &QueryExpr) -> HashSet<String> {
    let mut scopes = HashSet::new();
    collect_query_expr_result_cache_scopes(&mut scopes, expr);
    scopes
}

/// Pick the `(global_mode, collection_mode)` pair for an expression,
/// or `None` for variants that opt out of intent-locking entirely
/// (admin statements like `SHOW CONFIG`, transaction control, tenant
/// toggles).
///
/// Phase-1 contract:
/// - Reads  — `(IX-compatible) (Global, IS) → (Collection, IS)`
/// - Writes — `(IX-compatible) (Global, IX) → (Collection, IX)`
/// - DDL    — `(strong)        (Global, IX) → (Collection, X)`
fn intent_lock_modes_for(
    expr: &QueryExpr,
) -> Option<(
    crate::storage::transaction::lock::LockMode,
    crate::storage::transaction::lock::LockMode,
)> {
    use crate::storage::transaction::lock::LockMode::{Exclusive, IntentExclusive, IntentShared};

    match expr {
        // Reads — IS / IS.
        QueryExpr::Table(_)
        | QueryExpr::Join(_)
        | QueryExpr::Vector(_)
        | QueryExpr::Hybrid(_)
        | QueryExpr::Graph(_)
        | QueryExpr::Path(_)
        | QueryExpr::Ask(_)
        | QueryExpr::SearchCommand(_)
        | QueryExpr::GraphCommand(_)
        | QueryExpr::QueueCommand(_) => Some((IntentShared, IntentShared)),

        // Writes — IX / IX. Non-tabular mutations (vector insert,
        // graph node insert, queue push, timeseries point insert)
        // don't carry their own dispatch arm here; they ride through
        // the Insert variant or a command variant covered by the
        // read-side arm above. P1.T4 expands only the TableQuery-ish
        // writes; non-tabular kinds inherit when their DML variants
        // land in later phases.
        QueryExpr::Insert(_) | QueryExpr::Update(_) | QueryExpr::Delete(_) => {
            Some((IntentExclusive, IntentExclusive))
        }

        // DDL — IX / X. A DDL against collection `c` blocks all
        // other writers + readers on `c` but leaves other collections
        // running (because Global stays IX, not X).
        QueryExpr::CreateTable(_)
        | QueryExpr::DropTable(_)
        | QueryExpr::AlterTable(_)
        | QueryExpr::CreateIndex(_)
        | QueryExpr::DropIndex(_)
        | QueryExpr::CreateTimeSeries(_)
        | QueryExpr::DropTimeSeries(_)
        | QueryExpr::CreateQueue(_)
        | QueryExpr::DropQueue(_)
        | QueryExpr::CreateTree(_)
        | QueryExpr::DropTree(_)
        | QueryExpr::CreatePolicy(_)
        | QueryExpr::DropPolicy(_)
        | QueryExpr::CreateView(_)
        | QueryExpr::DropView(_)
        | QueryExpr::RefreshMaterializedView(_)
        | QueryExpr::CreateSchema(_)
        | QueryExpr::DropSchema(_)
        | QueryExpr::CreateSequence(_)
        | QueryExpr::DropSequence(_)
        | QueryExpr::CreateServer(_)
        | QueryExpr::DropServer(_)
        | QueryExpr::CreateForeignTable(_)
        | QueryExpr::DropForeignTable(_) => Some((IntentExclusive, Exclusive)),

        // Admin / control — skip intent locks. `SET TENANT`,
        // `BEGIN / COMMIT / ROLLBACK`, `SET CONFIG`, `SHOW CONFIG`,
        // `VACUUM`, etc. don't touch collection data the same way
        // and the existing transaction layer already serialises the
        // pieces that matter.
        _ => None,
    }
}

/// Best-effort collection inventory for an expression. Used to pick
/// `Collection(...)` resources for the intent-lock guard. Overshoots
/// are fine (take an extra IS, benign); undershoots leak writes past
/// DDL X locks, so err on the side of listing more names.
fn collections_referenced(expr: &QueryExpr) -> Vec<String> {
    let mut out = Vec::new();
    walk_collections(expr, &mut out);
    out.sort();
    out.dedup();
    out
}

fn walk_collections(expr: &QueryExpr, out: &mut Vec<String>) {
    match expr {
        QueryExpr::Table(t) => out.push(t.table.clone()),
        QueryExpr::Join(j) => {
            walk_collections(&j.left, out);
            walk_collections(&j.right, out);
        }
        QueryExpr::Insert(i) => out.push(i.table.clone()),
        QueryExpr::Update(u) => out.push(u.table.clone()),
        QueryExpr::Delete(d) => out.push(d.table.clone()),

        // DDL — include the target collection so DDL takes
        // `(Collection, X)` and blocks concurrent readers / writers
        // on the same collection. Other collections stay live
        // because Global is still IX.
        QueryExpr::CreateTable(q) => out.push(q.name.clone()),
        QueryExpr::DropTable(q) => out.push(q.name.clone()),
        QueryExpr::AlterTable(q) => out.push(q.name.clone()),
        QueryExpr::CreateIndex(q) => out.push(q.table.clone()),
        QueryExpr::DropIndex(q) => out.push(q.table.clone()),
        QueryExpr::CreateTimeSeries(q) => out.push(q.name.clone()),
        QueryExpr::DropTimeSeries(q) => out.push(q.name.clone()),
        QueryExpr::CreateQueue(q) => out.push(q.name.clone()),
        QueryExpr::DropQueue(q) => out.push(q.name.clone()),
        QueryExpr::CreatePolicy(q) => out.push(q.table.clone()),
        QueryExpr::CreateView(q) => out.push(q.name.clone()),
        QueryExpr::DropView(q) => out.push(q.name.clone()),
        QueryExpr::RefreshMaterializedView(q) => out.push(q.name.clone()),

        // Vector / Hybrid / Graph / Path / commands reference
        // collections through fields whose shape varies; without a
        // uniform accessor we fall back to the global lock only —
        // benign because every runtime path still holds the global
        // mode.
        _ => {}
    }
}

impl RedDBRuntime {
    pub fn in_memory() -> RedDBResult<Self> {
        Self::with_options(RedDBOptions::in_memory())
    }

    /// Handle to the intent-lock manager for tests + introspection.
    /// Production code acquires via `LockerGuard::new(rt.lock_manager())`
    /// rather than touching the manager directly.
    pub fn lock_manager(&self) -> std::sync::Arc<crate::storage::transaction::lock::LockManager> {
        self.inner.lock_manager.clone()
    }

    #[inline(never)]
    pub fn with_options(options: RedDBOptions) -> RedDBResult<Self> {
        Self::with_pool(options, ConnectionPoolConfig::default())
    }

    pub fn with_pool(
        options: RedDBOptions,
        pool_config: ConnectionPoolConfig,
    ) -> RedDBResult<Self> {
        let db = Arc::new(
            RedDB::open_with_options(&options)
                .map_err(|err| RedDBError::Internal(err.to_string()))?,
        );

        let runtime = Self {
            inner: Arc::new(RuntimeInner {
                db,
                layout: PhysicalLayout::from_options(&options),
                indices: IndexCatalog::register_default_vector_graph(
                    options.has_capability(crate::api::Capability::Table),
                    options.has_capability(crate::api::Capability::Graph),
                ),
                pool_config,
                pool: Mutex::new(PoolState::default()),
                started_at_unix_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
                probabilistic: super::probabilistic_store::ProbabilisticStore::new(),
                index_store: super::index_store::IndexStore::new(),
                cdc: crate::replication::cdc::CdcBuffer::new(100_000),
                backup_scheduler: crate::replication::scheduler::BackupScheduler::new(3600),
                query_cache: parking_lot::RwLock::new(
                    crate::storage::query::planner::cache::PlanCache::new(1000),
                ),
                result_cache: parking_lot::RwLock::new((
                    HashMap::new(),
                    std::collections::VecDeque::new(),
                )),
                queue_message_locks: parking_lot::RwLock::new(HashMap::new()),
                planner_dirty_tables: parking_lot::RwLock::new(HashSet::new()),
                ec_registry: Arc::new(crate::ec::config::EcRegistry::new()),
                ec_worker: crate::ec::worker::EcWorker::new(),
                auth_store: parking_lot::RwLock::new(None),
                commit_lock: Mutex::new(()),
                views: parking_lot::RwLock::new(HashMap::new()),
                materialized_views: parking_lot::RwLock::new(
                    crate::storage::cache::result::MaterializedViewCache::new(),
                ),
                snapshot_manager: Arc::new(
                    crate::storage::transaction::snapshot::SnapshotManager::new(),
                ),
                tx_contexts: parking_lot::RwLock::new(HashMap::new()),
                tx_local_tenants: parking_lot::RwLock::new(HashMap::new()),
                env_config_overrides: crate::runtime::config_overlay::collect_env_overrides(),
                lock_manager: Arc::new({
                    // Sourced from the matrix: Tier B key
                    // `concurrency.locking.deadlock_timeout_ms`
                    // (default 5000). Env var wins at boot so
                    // operators can tune without touching red_config.
                    let env = crate::runtime::config_overlay::collect_env_overrides();
                    let timeout_ms = env
                        .get("concurrency.locking.deadlock_timeout_ms")
                        .and_then(|raw| raw.parse::<u64>().ok())
                        .unwrap_or_else(|| {
                            match crate::runtime::config_matrix::default_for(
                                "concurrency.locking.deadlock_timeout_ms",
                            ) {
                                Some(crate::serde_json::Value::Number(n)) => n as u64,
                                _ => 5000,
                            }
                        });
                    let cfg = crate::storage::transaction::lock::LockConfig {
                        default_timeout: std::time::Duration::from_millis(timeout_ms),
                        ..Default::default()
                    };
                    crate::storage::transaction::lock::LockManager::new(cfg)
                }),
                rls_policies: parking_lot::RwLock::new(HashMap::new()),
                rls_enabled_tables: parking_lot::RwLock::new(HashSet::new()),
                foreign_tables: Arc::new(crate::storage::fdw::ForeignTableRegistry::with_builtins()),
                pending_tombstones: parking_lot::RwLock::new(HashMap::new()),
                tenant_tables: parking_lot::RwLock::new(HashMap::new()),
            }),
        };

        let restored_cdc_lsn = runtime
            .inner
            .db
            .replication
            .as_ref()
            .map(|repl| {
                repl.logical_wal_spool
                    .as_ref()
                    .map(|spool| spool.current_lsn())
                    .unwrap_or(0)
            })
            .unwrap_or(0)
            .max(runtime.config_u64("red.config.timeline.last_archived_lsn", 0));
        runtime.inner.cdc.set_current_lsn(restored_cdc_lsn);

        // Phase 2.5.4: replay `tenant_tables.{table}.column` markers so
        // tables declared via `TENANT BY (col)` survive restart. Each
        // entry re-registers the auto-policy and flips RLS on again.
        runtime.rehydrate_tenant_tables();
        if let Some(repl) = &runtime.inner.db.replication {
            repl.wal_buffer.set_current_lsn(restored_cdc_lsn);
        }

        // Save system info to red_config on boot
        {
            let sys = SystemInfo::collect();
            runtime.inner.db.store().set_config_tree(
                "red.system",
                &crate::serde_json::json!({
                    "pid": sys.pid,
                    "cpu_cores": sys.cpu_cores,
                    "total_memory_bytes": sys.total_memory_bytes,
                    "available_memory_bytes": sys.available_memory_bytes,
                    "os": sys.os,
                    "arch": sys.arch,
                    "hostname": sys.hostname,
                    "started_at": SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64
                }),
            );

            // Seed defaults on first boot (only if red_config is empty or missing defaults)
            let store = runtime.inner.db.store();
            if store
                .get_collection("red_config")
                .map(|m| m.query_all(|_| true).len())
                .unwrap_or(0)
                <= 10
            {
                store.set_config_tree("red.ai", &crate::json!({
                    "default": crate::json!({
                        "provider": "openai",
                        "model": crate::ai::DEFAULT_OPENAI_PROMPT_MODEL
                    }),
                    "max_embedding_inputs": 256,
                    "max_prompt_batch": 256,
                    "timeout": crate::json!({ "connect_secs": 10, "read_secs": 90, "write_secs": 30 })
                }));
                store.set_config_tree(
                    "red.server",
                    &crate::json!({
                        "max_scan_limit": 1000,
                        "max_body_size": 1048576,
                        "read_timeout_ms": 5000,
                        "write_timeout_ms": 5000
                    }),
                );
                store.set_config_tree(
                    "red.storage",
                    &crate::json!({
                        "page_size": 4096,
                        "page_cache_capacity": 100000,
                        "auto_checkpoint_pages": 1000,
                        "snapshot_retention": 16,
                        "verify_checksums": true,
                        "segment": crate::json!({
                            "max_entities": 100000,
                            "max_bytes": 268435456_u64,
                            "compression_level": 6
                        }),
                        "hnsw": crate::json!({ "m": 16, "ef_construction": 100, "ef_search": 50 }),
                        "ivf": crate::json!({ "n_lists": 100, "n_probes": 10 }),
                        "bm25": crate::json!({ "k1": 1.2, "b": 0.75 })
                    }),
                );
                store.set_config_tree(
                    "red.search",
                    &crate::json!({
                        "rag": crate::json!({
                            "max_chunks_per_source": 10,
                            "max_total_chunks": 25,
                            "similarity_threshold": 0.8,
                            "graph_depth": 2,
                            "min_relevance": 0.3
                        }),
                        "fusion": crate::json!({
                            "vector_weight": 0.5,
                            "graph_weight": 0.3,
                            "table_weight": 0.2,
                            "dedup_threshold": 0.85
                        })
                    }),
                );
                store.set_config_tree(
                    "red.auth",
                    &crate::json!({
                        "enabled": false,
                        "session_ttl_secs": 3600,
                        "require_auth": false
                    }),
                );
                store.set_config_tree(
                    "red.query",
                    &crate::json!({
                        "connection_pool": crate::json!({ "max_connections": 64, "max_idle": 16 }),
                        "max_recursion_depth": 1000
                    }),
                );
                store.set_config_tree(
                    "red.indexes",
                    &crate::json!({
                        "auto_select": true,
                        "bloom_filter": crate::json!({
                            "enabled": true,
                            "false_positive_rate": 0.01,
                            "prune_on_scan": true
                        }),
                        "hash": crate::json!({ "enabled": true }),
                        "bitmap": crate::json!({ "enabled": true, "max_cardinality": 1000 }),
                        "spatial": crate::json!({ "enabled": true })
                    }),
                );
                store.set_config_tree(
                    "red.memtable",
                    &crate::json!({
                        "enabled": true,
                        "max_bytes": 67108864_u64,
                        "flush_threshold": 0.75
                    }),
                );
                store.set_config_tree(
                    "red.probabilistic",
                    &crate::json!({
                        "hll_registers": 16384,
                        "sketch_default_width": 1000,
                        "sketch_default_depth": 5,
                        "filter_default_capacity": 100000
                    }),
                );
                store.set_config_tree(
                    "red.timeseries",
                    &crate::json!({
                        "default_chunk_size": 1024,
                        "compression": crate::json!({
                            "timestamps": "delta_of_delta",
                            "values": "gorilla_xor"
                        }),
                        "default_retention_days": 0
                    }),
                );
                store.set_config_tree(
                    "red.queue",
                    &crate::json!({
                        "default_max_size": 0,
                        "default_max_attempts": 3,
                        "visibility_timeout_ms": 30000,
                        "consumer_idle_timeout_ms": 60000
                    }),
                );
                store.set_config_tree(
                    "red.backup",
                    &crate::json!({
                        "enabled": false,
                        "interval_secs": 3600,
                        "retention_count": 24,
                        "upload": false,
                        "backend": "local"
                    }),
                );
                store.set_config_tree(
                    "red.wal",
                    &crate::json!({
                        "archive": crate::json!({
                            "enabled": false,
                            "retention_hours": 168,
                            "prefix": "wal/"
                        })
                    }),
                );
                store.set_config_tree(
                    "red.cdc",
                    &crate::json!({
                        "enabled": true,
                        "buffer_size": 100000
                    }),
                );
                store.set_config_tree(
                    "red.config.secret",
                    &crate::json!({
                        "auto_encrypt": true,
                        "auto_decrypt": true
                    }),
                );
            }

            // Perf-parity config matrix: heal the Tier A (critical)
            // keys unconditionally on every boot. Idempotent — only
            // writes the default when the key is missing. Keeps
            // `SHOW CONFIG` showing every guarantee the operator has
            // (durability.mode, concurrency.locking.enabled, …) even
            // on long-running datadirs that predate the matrix.
            crate::runtime::config_matrix::heal_critical_keys(store.as_ref());

            // Phase 5 — Lehman-Yao runtime flag. Read the Tier A
            // `storage.btree.lehman_yao` value from the matrix (env
            // > file > red_config > default) and publish it to the
            // storage layer's atomic so the B-tree read / split
            // paths can branch without re-reading the config on
            // every hot-path call.
            let lehman_yao = runtime.config_bool("storage.btree.lehman_yao", true);
            crate::storage::engine::btree::lehman_yao::set_enabled(lehman_yao);
            if lehman_yao {
                tracing::info!(
                    "storage.btree.lehman_yao=true — lock-free concurrent descent enabled"
                );
            }

            // Config file overlay — mounted `/etc/reddb/config.json`
            // (override path via REDDB_CONFIG_FILE). Writes keys with
            // write-if-absent semantics so a later user `SET CONFIG`
            // always wins. Missing file = silent no-op.
            let overlay_path = crate::runtime::config_overlay::config_file_path();
            let _ =
                crate::runtime::config_overlay::apply_config_file(store.as_ref(), &overlay_path);
        }

        // Start background maintenance thread (context index refresh +
        // session purge). Held by a WEAK reference to `RuntimeInner`
        // so dropping the last `RedDBRuntime` handle actually releases
        // the underlying Arc<Pager> (and its file lock). Polling at
        // 200ms means shutdown latency is bounded; the real 60-second
        // work cadence is tracked independently via a `last_work`
        // timestamp.
        //
        // The previous version captured `rt = runtime.clone()` by
        // strong reference and ran an unterminated `loop`, which held
        // Arc<RuntimeInner> forever — reopening a persistent database
        // in the same process failed with "Database is locked" because
        // the pager could never drop. See the regression test
        // `finding_1_select_after_bulk_insert_persistent_reopen`.
        {
            let weak = Arc::downgrade(&runtime.inner);
            std::thread::Builder::new()
                .name("reddb-maintenance".into())
                .spawn(move || {
                    let tick = std::time::Duration::from_millis(200);
                    let work_interval = std::time::Duration::from_secs(60);
                    let mut last_work = std::time::Instant::now();
                    loop {
                        std::thread::sleep(tick);
                        let Some(inner) = weak.upgrade() else {
                            // All strong references dropped — the
                            // runtime is gone, exit cleanly.
                            break;
                        };
                        if last_work.elapsed() >= work_interval {
                            let _stats = inner.db.store().context_index().stats();
                            last_work = std::time::Instant::now();
                        }
                    }
                })
                .ok();
        }

        // Start backup scheduler if enabled via red_config
        {
            let store = runtime.inner.db.store();
            let mut backup_enabled = false;
            let mut backup_interval = 3600u64;

            if let Some(manager) = store.get_collection("red_config") {
                manager.for_each_entity(|entity| {
                    if let Some(row) = entity.data.as_row() {
                        let key = row.get_field("key").and_then(|v| match v {
                            crate::storage::schema::Value::Text(s) => Some(s.as_str()),
                            _ => None,
                        });
                        let val = row.get_field("value");
                        if key == Some("red.config.backup.enabled") {
                            backup_enabled = match val {
                                Some(crate::storage::schema::Value::Boolean(true)) => true,
                                Some(crate::storage::schema::Value::Text(s)) => s == "true",
                                _ => false,
                            };
                        } else if key == Some("red.config.backup.interval_secs") {
                            if let Some(crate::storage::schema::Value::Integer(n)) = val {
                                backup_interval = *n as u64;
                            }
                        }
                    }
                    true
                });
            }

            if backup_enabled {
                runtime.inner.backup_scheduler.set_interval(backup_interval);
                let rt = runtime.clone();
                runtime
                    .inner
                    .backup_scheduler
                    .start(move || rt.trigger_backup().map_err(|e| format!("{}", e)));
            }
        }

        // Load EC registry from red_config and start worker
        {
            runtime
                .inner
                .ec_registry
                .load_from_config_store(runtime.inner.db.store().as_ref());
            if !runtime.inner.ec_registry.async_configs().is_empty() {
                runtime.inner.ec_worker.start(
                    Arc::clone(&runtime.inner.ec_registry),
                    Arc::clone(&runtime.inner.db.store()),
                );
            }
        }

        if let crate::replication::ReplicationRole::Replica { primary_addr } =
            runtime.inner.db.options().replication.role.clone()
        {
            let rt = runtime.clone();
            std::thread::Builder::new()
                .name("reddb-replica".into())
                .spawn(move || rt.run_replica_loop(primary_addr))
                .ok();
        }

        Ok(runtime)
    }

    pub fn db(&self) -> Arc<RedDB> {
        Arc::clone(&self.inner.db)
    }

    /// Execute `f` holding the runtime-wide commit lock.
    ///
    /// Used by the stdio `tx.commit` path to serialize write-set replays
    /// so concurrent transactional commits do not interleave their
    /// buffered operations. Auto-committed writes (outside any `tx.begin`
    /// session) bypass this lock entirely and keep their current
    /// throughput.
    ///
    /// The lock is held for the full closure — any I/O or long-running
    /// work inside `f` blocks other commit attempts, so callers should
    /// keep the critical section tight (just the replay loop).
    pub fn with_commit_lock<T>(&self, f: impl FnOnce() -> T) -> T {
        let _guard = self
            .inner
            .commit_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f()
    }

    /// Direct access to the runtime's secondary-index store.
    /// Used by bulk-insert entry points (gRPC binary bulk, HTTP bulk,
    /// wire bulk) that need to push new rows through the per-index
    /// maintenance hook after `store.bulk_insert` returns.
    pub fn index_store_ref(&self) -> &super::index_store::IndexStore {
        &self.inner.index_store
    }

    /// Inject an AuthStore into the runtime. Called by server boot
    /// after the vault has been bootstrapped, so that `Value::Secret`
    /// auto-encrypt/decrypt can reach the vault AES key.
    pub fn set_auth_store(&self, store: Arc<crate::auth::store::AuthStore>) {
        *self.inner.auth_store.write() = Some(store);
    }

    /// Returns the vault AES key (`red.secret.aes_key`) if an auth
    /// store is wired and a key has been generated. Used by the
    /// `Value::Secret` encrypt/decrypt pipeline.
    pub(crate) fn secret_aes_key(&self) -> Option<[u8; 32]> {
        let guard = self.inner.auth_store.read();
        guard.as_ref().and_then(|s| s.vault_secret_key())
    }

    /// Resolve a boolean flag from `red_config`. Defaults to `default`
    /// when the key is missing or not coercible. If the same key has
    /// been written multiple times (SET CONFIG appends new rows), the
    /// most recent entity wins. Env-var overrides
    /// (`REDDB_<UP_DOTTED>`) take highest precedence.
    pub(crate) fn config_bool(&self, key: &str, default: bool) -> bool {
        if let Some(raw) = self.inner.env_config_overrides.get(key) {
            if let Some(crate::storage::schema::Value::Boolean(b)) =
                crate::runtime::config_overlay::coerce_env_value(key, raw)
            {
                return b;
            }
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return default;
        };
        let mut result = default;
        let mut latest_id: u64 = 0;
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let entry_key = row.get_field("key").and_then(|v| match v {
                    crate::storage::schema::Value::Text(s) => Some(s.as_str()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        result = match row.get_field("value") {
                            Some(crate::storage::schema::Value::Boolean(b)) => *b,
                            Some(crate::storage::schema::Value::Text(s)) => {
                                matches!(s.as_str(), "true" | "TRUE" | "True" | "1")
                            }
                            Some(crate::storage::schema::Value::Integer(n)) => *n != 0,
                            _ => default,
                        };
                    }
                }
            }
            true
        });
        result
    }

    pub(crate) fn config_u64(&self, key: &str, default: u64) -> u64 {
        if let Some(raw) = self.inner.env_config_overrides.get(key) {
            if let Some(crate::storage::schema::Value::UnsignedInteger(n)) =
                crate::runtime::config_overlay::coerce_env_value(key, raw)
            {
                return n;
            }
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return default;
        };
        let mut result = default;
        let mut latest_id: u64 = 0;
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let entry_key = row.get_field("key").and_then(|v| match v {
                    crate::storage::schema::Value::Text(s) => Some(s.as_str()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        result = match row.get_field("value") {
                            Some(crate::storage::schema::Value::Integer(n)) => *n as u64,
                            Some(crate::storage::schema::Value::UnsignedInteger(n)) => *n,
                            Some(crate::storage::schema::Value::Text(s)) => {
                                s.parse::<u64>().unwrap_or(default)
                            }
                            _ => default,
                        };
                    }
                }
            }
            true
        });
        result
    }

    pub(crate) fn config_string(&self, key: &str, default: &str) -> String {
        if let Some(raw) = self.inner.env_config_overrides.get(key) {
            return raw.clone();
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return default.to_string();
        };
        let mut result = default.to_string();
        let mut latest_id: u64 = 0;
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let entry_key = row.get_field("key").and_then(|v| match v {
                    crate::storage::schema::Value::Text(s) => Some(s.as_str()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        if let Some(crate::storage::schema::Value::Text(value)) =
                            row.get_field("value")
                        {
                            result = value.clone();
                        }
                    }
                }
            }
            true
        });
        result
    }

    fn latest_metadata_for(
        &self,
        collection: &str,
        entity_id: u64,
    ) -> Option<crate::serde_json::Value> {
        self.inner
            .db
            .store()
            .get_metadata(collection, EntityId::new(entity_id))
            .map(|metadata| metadata_to_json(&metadata))
    }

    fn persist_replica_lsn(&self, lsn: u64) {
        self.inner.db.store().set_config_tree(
            "red.replication",
            &crate::json!({
                "last_applied_lsn": lsn
            }),
        );
    }

    fn persist_replication_health(
        &self,
        state: &str,
        last_error: &str,
        primary_lsn: Option<u64>,
        oldest_available_lsn: Option<u64>,
    ) {
        self.inner.db.store().set_config_tree(
            "red.replication",
            &crate::json!({
                "state": state,
                "last_error": last_error,
                "last_seen_primary_lsn": primary_lsn.unwrap_or(0),
                "last_seen_oldest_lsn": oldest_available_lsn.unwrap_or(0),
                "updated_at_unix_ms": SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
            }),
        );
    }

    /// Whether `SECRET('...')` literals should be encrypted with the
    /// vault AES key on INSERT. Default `true`.
    pub(crate) fn secret_auto_encrypt(&self) -> bool {
        self.config_bool("red.config.secret.auto_encrypt", true)
    }

    /// Whether `Value::Secret` columns should be decrypted back to
    /// plaintext on SELECT when the vault is unsealed. Default `true`.
    /// Turning this off keeps secrets masked as `***` even while the
    /// vault is open — useful for audit trails or read-only exports.
    pub(crate) fn secret_auto_decrypt(&self) -> bool {
        self.config_bool("red.config.secret.auto_decrypt", true)
    }

    /// Walk every record in `result` and swap `Value::Secret(bytes)`
    /// for the decrypted plaintext when the runtime has the vault
    /// AES key AND `red.config.secret.auto_decrypt = true`. If the
    /// key is missing, the vault is sealed, or auto_decrypt is off,
    /// secrets are left as `Value::Secret` which every formatter
    /// (Display, JSON) already masks as `***`.
    pub(crate) fn apply_secret_decryption(&self, result: &mut RuntimeQueryResult) {
        if !self.secret_auto_decrypt() {
            return;
        }
        let Some(key) = self.secret_aes_key() else {
            return;
        };
        for record in result.result.records.iter_mut() {
            for value in record.values.values_mut() {
                if let Value::Secret(ref bytes) = value {
                    if let Some(plain) =
                        super::impl_dml::decrypt_secret_payload(&key, bytes.as_slice())
                    {
                        if let Ok(text) = String::from_utf8(plain) {
                            *value = Value::Text(text);
                        }
                    }
                }
            }
        }
    }

    /// Emit a CDC change event and replicate to WAL buffer.
    /// Create a `MutationEngine` bound to this runtime.
    ///
    /// The engine is cheap to construct (no allocation) and should be
    /// dropped after `apply` returns. Use this from application-layer
    /// `create_row` / `create_rows_batch` instead of calling
    /// `bulk_insert` + `index_entity_insert` + `cdc_emit` separately.
    pub(crate) fn mutation_engine(&self) -> crate::runtime::mutation::MutationEngine<'_> {
        crate::runtime::mutation::MutationEngine::new(self)
    }

    /// Emit a CDC record without invalidating the result cache.
    ///
    /// Used by `MutationEngine::append_batch` which calls
    /// `invalidate_result_cache` once for the whole batch before this
    /// loop, avoiding N write-lock acquisitions.
    pub(crate) fn cdc_emit_no_cache_invalidate(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        entity_id: u64,
        entity_kind: &str,
    ) {
        let lsn = self
            .inner
            .cdc
            .emit(operation, collection, entity_id, entity_kind);

        // Append to logical WAL replication buffer (if primary mode)
        if let Some(ref primary) = self.inner.db.replication {
            let store = self.inner.db.store();
            let entity = if operation == crate::replication::cdc::ChangeOperation::Delete {
                None
            } else {
                store.get(collection, EntityId::new(entity_id))
            };
            let record = ChangeRecord {
                lsn,
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                operation,
                collection: collection.to_string(),
                entity_id,
                entity_kind: entity_kind.to_string(),
                entity_bytes: entity
                    .as_ref()
                    .map(|e| UnifiedStore::serialize_entity(e, store.format_version())),
                metadata: self.latest_metadata_for(collection, entity_id),
            };
            let encoded = record.encode();
            primary.wal_buffer.append(record.lsn, encoded.clone());
            if let Some(spool) = &primary.logical_wal_spool {
                let _ = spool.append(record.lsn, &encoded);
            }
        }
    }

    pub fn cdc_emit(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        entity_id: u64,
        entity_kind: &str,
    ) {
        let lsn = self
            .inner
            .cdc
            .emit(operation, collection, entity_id, entity_kind);
        // Perf: prior to this we called `invalidate_result_cache()`
        // which wipes EVERY cached query, across every table, under
        // a write lock — turning each INSERT into a serialisation
        // point for all readers. Swap to the per-table variant so
        // unrelated query caches survive.
        self.invalidate_result_cache_for_table(collection);

        // Append to logical WAL replication buffer (if primary mode)
        if let Some(ref primary) = self.inner.db.replication {
            let store = self.inner.db.store();
            let entity = if operation == crate::replication::cdc::ChangeOperation::Delete {
                None
            } else {
                store.get(collection, EntityId::new(entity_id))
            };
            let record = ChangeRecord {
                lsn,
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                operation,
                collection: collection.to_string(),
                entity_id,
                entity_kind: entity_kind.to_string(),
                entity_bytes: entity
                    .as_ref()
                    .map(|entity| UnifiedStore::serialize_entity(entity, store.format_version())),
                metadata: self.latest_metadata_for(collection, entity_id),
            };
            let encoded = record.encode();
            primary.wal_buffer.append(record.lsn, encoded.clone());
            if let Some(spool) = &primary.logical_wal_spool {
                let _ = spool.append(record.lsn, &encoded);
            }
        }
    }

    pub(crate) fn cdc_emit_prebuilt(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        entity: &UnifiedEntity,
        entity_kind: &str,
        metadata: Option<&crate::storage::Metadata>,
        invalidate_cache: bool,
    ) {
        self.cdc_emit_prebuilt_with_columns(
            operation,
            collection,
            entity,
            entity_kind,
            metadata,
            invalidate_cache,
            None,
        )
    }

    /// `cdc_emit_prebuilt` plus the list of column names whose values
    /// changed on this update. Callers that have already computed a
    /// `RowDamageVector` pass it here so downstream CDC consumers can
    /// filter events by touched column without re-diffing.
    /// `changed_columns` is only meaningful for `Update` operations —
    /// insert and delete events ignore it.
    pub(crate) fn cdc_emit_prebuilt_with_columns(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        entity: &UnifiedEntity,
        entity_kind: &str,
        metadata: Option<&crate::storage::Metadata>,
        invalidate_cache: bool,
        changed_columns: Option<Vec<String>>,
    ) {
        if invalidate_cache {
            self.invalidate_result_cache();
        }

        let lsn = self.inner.cdc.emit_with_columns(
            operation,
            collection,
            entity.id.raw(),
            entity_kind,
            changed_columns,
        );

        if let Some(ref primary) = self.inner.db.replication {
            let store = self.inner.db.store();
            let record = ChangeRecord {
                lsn,
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                operation,
                collection: collection.to_string(),
                entity_id: entity.id.raw(),
                entity_kind: entity_kind.to_string(),
                entity_bytes: Some(UnifiedStore::serialize_entity(
                    entity,
                    store.format_version(),
                )),
                metadata: metadata
                    .map(metadata_to_json)
                    .or_else(|| self.latest_metadata_for(collection, entity.id.raw())),
            };
            let encoded = record.encode();
            primary.wal_buffer.append(record.lsn, encoded.clone());
            if let Some(spool) = &primary.logical_wal_spool {
                let _ = spool.append(record.lsn, &encoded);
            }
        }
    }

    pub(crate) fn cdc_emit_prebuilt_batch<'a, I>(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        entity_kind: &str,
        items: I,
        invalidate_cache: bool,
    ) where
        I: IntoIterator<
            Item = (
                &'a str,
                &'a UnifiedEntity,
                Option<&'a crate::storage::Metadata>,
            ),
        >,
    {
        let items: Vec<(&str, &UnifiedEntity, Option<&crate::storage::Metadata>)> =
            items.into_iter().collect();
        if items.is_empty() {
            return;
        }

        if invalidate_cache {
            self.invalidate_result_cache();
        }

        for (collection, entity, metadata) in items {
            self.cdc_emit_prebuilt(operation, collection, entity, entity_kind, metadata, false);
        }
    }

    fn run_replica_loop(&self, primary_addr: String) {
        let endpoint = if primary_addr.starts_with("http") {
            primary_addr
        } else {
            format!("http://{primary_addr}")
        };
        let poll_ms = self.inner.db.options().replication.poll_interval_ms;
        let max_count = self.inner.db.options().replication.max_batch_size;
        let mut since_lsn = self.config_u64("red.replication.last_applied_lsn", 0);

        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => return,
        };

        runtime.block_on(async move {
            use crate::grpc::proto::red_db_client::RedDbClient;
            use crate::grpc::proto::JsonPayloadRequest;

            let mut client = loop {
                match RedDbClient::connect(endpoint.clone()).await {
                    Ok(client) => {
                        self.persist_replication_health("connecting", "", None, None);
                        break client;
                    }
                    Err(_) => {
                        self.persist_replication_health(
                            "connecting",
                            "waiting for primary connection",
                            None,
                            None,
                        );
                        std::thread::sleep(std::time::Duration::from_millis(poll_ms.max(250)))
                    }
                }
            };

            loop {
                let payload = crate::json!({
                    "since_lsn": since_lsn,
                    "max_count": max_count
                });
                let request = tonic::Request::new(JsonPayloadRequest {
                    payload_json: crate::json::to_string(&payload)
                        .unwrap_or_else(|_| "{}".to_string()),
                });

                if let Ok(response) = client.pull_wal_records(request).await {
                    if let Ok(value) =
                        crate::json::from_str::<crate::json::Value>(&response.into_inner().payload)
                    {
                        let current_lsn =
                            value.get("current_lsn").and_then(crate::json::Value::as_u64);
                        let oldest_available_lsn = value
                            .get("oldest_available_lsn")
                            .and_then(crate::json::Value::as_u64);
                        if since_lsn > 0
                            && oldest_available_lsn
                                .map(|oldest| oldest > since_lsn.saturating_add(1))
                                .unwrap_or(false)
                        {
                            self.persist_replication_health(
                                "stalled_gap",
                                "replica is behind the oldest logical WAL available on primary; re-bootstrap required",
                                current_lsn,
                                oldest_available_lsn,
                            );
                            std::thread::sleep(std::time::Duration::from_millis(poll_ms.max(250)));
                            continue;
                        }
                        if let Some(records) =
                            value.get("records").and_then(crate::json::Value::as_array)
                        {
                            for record in records {
                                let Some(data_hex) =
                                    record.get("data").and_then(crate::json::Value::as_str)
                                else {
                                    continue;
                                };
                                let Ok(data) = hex::decode(data_hex) else {
                                    self.persist_replication_health(
                                        "apply_error",
                                        "failed to decode WAL record hex payload",
                                        current_lsn,
                                        oldest_available_lsn,
                                    );
                                    continue;
                                };
                                let Ok(change) = ChangeRecord::decode(&data) else {
                                    self.persist_replication_health(
                                        "apply_error",
                                        "failed to decode logical WAL record",
                                        current_lsn,
                                        oldest_available_lsn,
                                    );
                                    continue;
                                };
                                if LogicalChangeApplier::apply_record(
                                    self.inner.db.as_ref(),
                                    &change,
                                    ApplyMode::Replica,
                                )
                                .is_err()
                                {
                                    self.persist_replication_health(
                                        "apply_error",
                                        "failed to apply logical WAL record on replica",
                                        current_lsn,
                                        oldest_available_lsn,
                                    );
                                    continue;
                                }
                                since_lsn = since_lsn.max(change.lsn);
                                self.persist_replica_lsn(since_lsn);
                            }
                        }
                        self.persist_replication_health(
                            "healthy",
                            "",
                            current_lsn,
                            oldest_available_lsn,
                        );
                    } else {
                        self.persist_replication_health(
                            "apply_error",
                            "failed to parse pull_wal_records response",
                            None,
                            None,
                        );
                    }
                } else {
                    self.persist_replication_health(
                        "connecting",
                        "primary pull_wal_records request failed",
                        None,
                        None,
                    );
                }

                std::thread::sleep(std::time::Duration::from_millis(poll_ms));
            }
        });
    }

    /// Poll CDC events since a given LSN.
    pub fn cdc_poll(
        &self,
        since_lsn: u64,
        max_count: usize,
    ) -> Vec<crate::replication::cdc::ChangeEvent> {
        self.inner.cdc.poll(since_lsn, max_count)
    }

    /// Get backup scheduler status.
    pub fn backup_status(&self) -> crate::replication::scheduler::BackupStatus {
        self.inner.backup_scheduler.status()
    }

    /// Trigger an immediate backup.
    pub fn trigger_backup(&self) -> RedDBResult<crate::replication::scheduler::BackupResult> {
        let started = std::time::Instant::now();
        let snapshot = self.create_snapshot()?;
        let mut uploaded = false;

        if let (Some(backend), Some(path)) = (&self.inner.db.remote_backend, self.inner.db.path()) {
            let default_snapshot_prefix = self.inner.db.options().default_snapshot_prefix();
            let default_wal_prefix = self.inner.db.options().default_wal_archive_prefix();
            let default_head_key = self.inner.db.options().default_backup_head_key();
            let snapshot_prefix = self.config_string(
                "red.config.backup.snapshot_prefix",
                &default_snapshot_prefix,
            );
            let wal_prefix =
                self.config_string("red.config.wal.archive.prefix", &default_wal_prefix);
            let head_key = self.config_string("red.config.backup.head_key", &default_head_key);
            let timeline_id = self.config_string("red.config.timeline.id", "main");
            let snapshot_key = crate::storage::wal::archive_snapshot(
                backend.as_ref(),
                path,
                snapshot.snapshot_id,
                &snapshot_prefix,
            )
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
            let current_lsn = self
                .inner
                .db
                .replication
                .as_ref()
                .map(|repl| {
                    repl.logical_wal_spool
                        .as_ref()
                        .map(|spool| spool.current_lsn())
                        .unwrap_or_else(|| repl.wal_buffer.current_lsn())
                })
                .unwrap_or_else(|| self.inner.cdc.current_lsn());
            let last_archived_lsn = self.config_u64("red.config.timeline.last_archived_lsn", 0);
            let manifest = crate::storage::wal::SnapshotManifest {
                timeline_id: timeline_id.clone(),
                snapshot_key: snapshot_key.clone(),
                snapshot_id: snapshot.snapshot_id,
                snapshot_time: snapshot.created_at_unix_ms as u64,
                base_lsn: current_lsn,
                schema_version: crate::api::REDDB_FORMAT_VERSION,
                format_version: crate::api::REDDB_FORMAT_VERSION,
            };
            crate::storage::wal::publish_snapshot_manifest(backend.as_ref(), &manifest)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;

            let archived_lsn = if let Some(primary) = &self.inner.db.replication {
                let oldest = primary
                    .logical_wal_spool
                    .as_ref()
                    .and_then(|spool| spool.oldest_lsn().ok().flatten())
                    .or_else(|| primary.wal_buffer.oldest_lsn())
                    .unwrap_or(last_archived_lsn);
                if last_archived_lsn > 0 && last_archived_lsn < oldest.saturating_sub(1) {
                    return Err(RedDBError::Internal(format!(
                        "logical WAL gap detected: last_archived_lsn={last_archived_lsn}, oldest_available_lsn={oldest}"
                    )));
                }
                let records = if let Some(spool) = &primary.logical_wal_spool {
                    spool
                        .read_since(last_archived_lsn, usize::MAX)
                        .map_err(|err| RedDBError::Internal(err.to_string()))?
                } else {
                    primary.wal_buffer.read_since(last_archived_lsn, usize::MAX)
                };
                if let Some(meta) = crate::storage::wal::archive_change_records(
                    backend.as_ref(),
                    &wal_prefix,
                    &records,
                )
                .map_err(|err| RedDBError::Internal(err.to_string()))?
                {
                    if let Some(spool) = &primary.logical_wal_spool {
                        let _ = spool.prune_through(meta.lsn_end);
                    }
                    meta.lsn_end
                } else {
                    last_archived_lsn
                }
            } else {
                last_archived_lsn
            };

            let head = crate::storage::wal::BackupHead {
                timeline_id,
                snapshot_key,
                snapshot_id: snapshot.snapshot_id,
                snapshot_time: snapshot.created_at_unix_ms as u64,
                current_lsn,
                last_archived_lsn: archived_lsn,
                wal_prefix,
            };
            crate::storage::wal::publish_backup_head(backend.as_ref(), &head_key, &head)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            self.inner.db.store().set_config_tree(
                "red.config.timeline",
                &crate::json!({
                    "last_archived_lsn": archived_lsn,
                    "id": head.timeline_id
                }),
            );
            uploaded = true;
        }

        Ok(crate::replication::scheduler::BackupResult {
            snapshot_id: snapshot.snapshot_id,
            uploaded,
            duration_ms: started.elapsed().as_millis() as u64,
            timestamp: snapshot.created_at_unix_ms as u64,
        })
    }

    pub fn acquire(&self) -> RedDBResult<RuntimeConnection> {
        let mut pool = self
            .inner
            .pool
            .lock()
            .map_err(|e| RedDBError::Internal(format!("connection pool lock poisoned: {e}")))?;
        if pool.active >= self.inner.pool_config.max_connections {
            return Err(RedDBError::Internal(
                "connection pool exhausted".to_string(),
            ));
        }

        let id = if let Some(id) = pool.idle.pop() {
            id
        } else {
            let id = pool.next_id;
            pool.next_id += 1;
            id
        };
        pool.active += 1;
        pool.total_checkouts += 1;
        drop(pool);

        Ok(RuntimeConnection {
            id,
            inner: Arc::clone(&self.inner),
        })
    }

    pub fn checkpoint(&self) -> RedDBResult<()> {
        self.inner
            .db
            .flush()
            .map_err(|err| RedDBError::Engine(err.to_string()))
    }

    pub fn run_maintenance(&self) -> RedDBResult<()> {
        self.inner
            .db
            .run_maintenance()
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn scan_collection(
        &self,
        collection: &str,
        cursor: Option<ScanCursor>,
        limit: usize,
    ) -> RedDBResult<ScanPage> {
        let store = self.inner.db.store();
        let manager = store
            .get_collection(collection)
            .ok_or_else(|| RedDBError::NotFound(collection.to_string()))?;

        let mut entities = manager.query_all(|_| true);
        entities.sort_by_key(|entity| entity.id.raw());

        let offset = cursor.map(|cursor| cursor.offset).unwrap_or(0);
        let total = entities.len();
        let end = total.min(offset.saturating_add(limit.max(1)));
        let items = if offset >= total {
            Vec::new()
        } else {
            entities[offset..end].to_vec()
        };
        let next = (end < total).then_some(ScanCursor { offset: end });

        Ok(ScanPage {
            collection: collection.to_string(),
            items,
            next,
            total,
        })
    }

    pub fn catalog(&self) -> CatalogModelSnapshot {
        self.inner.db.catalog_model_snapshot()
    }

    pub fn catalog_consistency_report(&self) -> crate::catalog::CatalogConsistencyReport {
        self.inner.db.catalog_consistency_report()
    }

    pub fn catalog_attention_summary(&self) -> CatalogAttentionSummary {
        crate::catalog::attention_summary(&self.catalog())
    }

    pub fn collection_attention(&self) -> Vec<CollectionDescriptor> {
        crate::catalog::collection_attention(&self.catalog())
    }

    pub fn index_attention(&self) -> Vec<CatalogIndexStatus> {
        crate::catalog::index_attention(&self.catalog())
    }

    pub fn graph_projection_attention(&self) -> Vec<CatalogGraphProjectionStatus> {
        crate::catalog::graph_projection_attention(&self.catalog())
    }

    pub fn analytics_job_attention(&self) -> Vec<CatalogAnalyticsJobStatus> {
        crate::catalog::analytics_job_attention(&self.catalog())
    }

    pub fn stats(&self) -> RuntimeStats {
        let pool = runtime_pool_lock(self);
        RuntimeStats {
            active_connections: pool.active,
            idle_connections: pool.idle.len(),
            total_checkouts: pool.total_checkouts,
            paged_mode: self.inner.db.is_paged(),
            started_at_unix_ms: self.inner.started_at_unix_ms,
            store: self.inner.db.stats(),
            system: SystemInfo::collect(),
        }
    }

    /// Execute a query under a typed scope override without embedding
    /// the tenant / user / role values into the SQL string. Use this
    /// from transport middleware (HTTP / gRPC / worker loops) where the
    /// scope is resolved from auth claims and the SQL is a parameterised
    /// template — avoids the string-concat injection risk of building
    /// `WITHIN TENANT '<id>' …` manually, and is drop-in compatible with
    /// prepared statements that didn't know about tenancy.
    ///
    /// Precedence matches the `WITHIN` clause: the passed `scope`
    /// overrides `SET LOCAL TENANT`, which overrides `SET TENANT`.
    /// The override is pushed on the thread-local scope stack for the
    /// duration of the call and popped on return — pool-shared
    /// connections cannot leak it across requests.
    pub fn execute_query_with_scope(
        &self,
        query: &str,
        scope: crate::runtime::within_clause::ScopeOverride,
    ) -> RedDBResult<RuntimeQueryResult> {
        if scope.is_empty() {
            return self.execute_query(query);
        }
        let _scope_guard = ScopeOverrideGuard::install(scope);
        self.execute_query(query)
    }

    #[inline(never)]
    pub fn execute_query(&self, query: &str) -> RedDBResult<RuntimeQueryResult> {
        // ── ULTRA-TURBO: autocommit `SELECT * FROM t WHERE _entity_id = N` ──
        //
        // Moved above every boot-cost the normal path pays (WITHIN
        // strip, SET LOCAL parse, tx_local_tenants read, snapshot
        // guard, tracing span, tx_contexts read) because the bench's
        // `select_point` scenario was observed at 28× vs PostgreSQL —
        // the dominant cost wasn't the entity fetch but the ceremony
        // before it. Only fires when there's no ambient transaction
        // context or WITHIN override, so the snapshot install we skip
        // truly is a no-op for this query.
        if !has_scope_override_active()
            && !query.trim_start().starts_with("WITHIN")
            && !query.trim_start().starts_with("within")
            && !self
                .inner
                .tx_contexts
                .read()
                .contains_key(&current_connection_id())
        {
            if let Some(result) = self.try_fast_entity_lookup(query) {
                return result;
            }
        }

        // `WITHIN TENANT '<id>' [USER '<u>'] [AS ROLE '<r>'] <stmt>` —
        // strip the prefix, push a stack-scoped override, recurse on
        // the inner statement, pop on return. Stack lives in a
        // thread-local but is balanced by the RAII guard, so a
        // pool-shared connection cannot leak the override across
        // requests and an early `?` return still pops cleanly.
        match crate::runtime::within_clause::try_strip_within_prefix(query) {
            Ok(Some((scope, inner))) => {
                let _scope_guard = ScopeOverrideGuard::install(scope);
                return self.execute_query(inner);
            }
            Ok(None) => {}
            Err(msg) => return Err(RedDBError::Query(msg)),
        }

        // `SET LOCAL TENANT '<id>'` — write the per-transaction tenant
        // override and return. Outside a transaction the statement is
        // an error (matches PG semantics: SET LOCAL only takes effect
        // within an active transaction).
        if let Some(value) = parse_set_local_tenant(query)? {
            let conn_id = current_connection_id();
            if !self.inner.tx_contexts.read().contains_key(&conn_id) {
                return Err(RedDBError::Query(
                    "SET LOCAL TENANT requires an active transaction".to_string(),
                ));
            }
            self.inner
                .tx_local_tenants
                .write()
                .insert(conn_id, value.clone());
            return Ok(RuntimeQueryResult::ok_message(
                query.to_string(),
                &match &value {
                    Some(id) => format!("local tenant set: {id}"),
                    None => "local tenant cleared".to_string(),
                },
                "set_local_tenant",
            ));
        }

        // Refresh the thread-local snapshot of this connection's
        // tx-local tenant so `current_tenant()` deep in expr eval can
        // read it without crossing back into the runtime. Guard pops
        // on return — even early errors via `?` clean up.
        let _tx_local_guard = {
            let conn_id = current_connection_id();
            let snap = self.inner.tx_local_tenants.read().get(&conn_id).cloned();
            TxLocalTenantGuard::install(snap)
        };

        // Phase 6 logging: enter a span stamped with conn_id / tenant
        // / query_len. Every downstream tracing::info!/warn!/error!
        // inherits these fields — no need to thread them manually
        // through storage/scan layers. Entered AFTER the WITHIN /
        // SET LOCAL TENANT resolution above so the span reflects the
        // effective scope for this statement.
        let _log_span = crate::telemetry::span::query_span(query).entered();

        // Phase 2.3.2d: install the MVCC snapshot for this statement so
        // every downstream scan sees a consistent row set. Dropped at
        // function exit — `CurrentSnapshotGuard` restores the caller's
        // snapshot (if any) on every return path, including early errors.
        //
        // Phase 2.3.2e: own-tx xids (parent + open savepoints) hitch a
        // ride on the context so visibility_check can always reveal
        // the current connection's own writes.
        let own_xids = {
            let mut set = std::collections::HashSet::new();
            if let Some(ctx) = self.inner.tx_contexts.read().get(&current_connection_id()) {
                set.insert(ctx.xid);
                for (_, sub) in &ctx.savepoints {
                    set.insert(*sub);
                }
            }
            set
        };
        let _snapshot_guard = CurrentSnapshotGuard::install(SnapshotContext {
            snapshot: self.current_snapshot(),
            manager: Arc::clone(&self.inner.snapshot_manager),
            own_xids,
        });

        // ── TURBO: bypass SQL parse for SELECT * FROM x WHERE _entity_id = N ──
        if let Some(result) = self.try_fast_entity_lookup(query) {
            return result;
        }

        // ── Result cache: return cached result if still fresh (30s TTL) ──
        //
        // Phase 2.5.4: mix tenant id + auth identity into the cache
        // key so an RLS/tenant-scoped query doesn't serve a prior
        // session's filtered result to a different tenant. Same query
        // string under `SET TENANT 'acme'` and `SET TENANT 'globex'`
        // must resolve against distinct entries.
        let cache_key_str = {
            let tenant = current_tenant().unwrap_or_default();
            let auth = current_auth_identity()
                .map(|(u, r)| format!("{}|{:?}", u, r))
                .unwrap_or_default();
            if tenant.is_empty() && auth.is_empty() {
                query.to_string()
            } else {
                format!("{query}\u{001e}{tenant}\u{001e}{auth}")
            }
        };
        {
            let cache = self.inner.result_cache.read();
            if let Some(entry) = cache.0.get(&cache_key_str) {
                if entry.cached_at.elapsed().as_secs() < 30 {
                    return Ok(entry.result.clone());
                }
            }
        }

        let mode = detect_mode(query);
        if matches!(mode, QueryMode::Unknown) {
            return Err(RedDBError::Query("unable to detect query mode".to_string()));
        }

        // ── Plan cache: reuse only exact-query ASTs ──
        //
        // DML statements (INSERT/UPDATE/DELETE) almost always have unique literal
        // values, so caching them burns CPU on eviction bookkeeping (Vec::remove(0)
        // shifts the entire LRU list) with zero hit rate. Skip the cache entirely
        // for write operations — parse directly.
        //
        // Only SELECT/DDL statements benefit from plan caching.
        // Detect by peeking at the first keyword of the trimmed query.
        let first_word = query
            .trim()
            .split_ascii_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        let is_write_op =
            first_word == "INSERT" || first_word == "UPDATE" || first_word == "DELETE";

        let cache_key = if is_write_op {
            String::new() // unused
        } else {
            crate::storage::query::planner::cache_key::normalize_cache_key(query)
        };

        let expr = if is_write_op {
            // Bypass plan cache for write operations — no benefit, pure overhead
            parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?
        } else {
            // ── Hot path: read lock only (no writer serialization on cache hits) ──
            //
            // peek() is a non-mutating probe: no LRU promotion, no touch().
            // This lets concurrent readers proceed without blocking each other.
            // On hit we bind literals if needed and return immediately.
            // Only on miss do we drop to a write lock to parse + insert.
            let hit = {
                let plan_cache = self.inner.query_cache.read();
                plan_cache.peek(&cache_key).map(|cached| {
                    let parameter_count = cached.parameter_count;
                    let optimized = cached.plan.optimized.clone();
                    let exact_query = cached.exact_query.clone();
                    (parameter_count, optimized, exact_query)
                })
            };

            if let Some((parameter_count, optimized, exact_query)) = hit {
                if parameter_count > 0 {
                    // Shape hit: substitute the current literal values into the shape.
                    let shape_binds =
                        crate::storage::query::planner::cache_key::extract_literal_bindings(query)
                            .unwrap_or_default();
                    if let Some(bound) =
                        crate::storage::query::planner::shape::bind_parameterized_query(
                            &optimized,
                            &shape_binds,
                            parameter_count,
                        )
                    {
                        bound
                    } else if exact_query.as_deref() == Some(query) {
                        // Bind failed but exact query matches — use as-is.
                        optimized
                    } else {
                        // Bind failed and literals differ: re-parse fresh.
                        parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?
                    }
                } else {
                    // No parameters means either there truly are no literals,
                    // or this statement type does not participate in shape
                    // parameterization (for example graph/queue commands).
                    // Reusing a normalized-cache hit across a different exact
                    // query can therefore leak stale literals into execution.
                    if exact_query.as_deref() == Some(query) {
                        optimized
                    } else {
                        parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?
                    }
                }
            } else {
                // Cache miss — parse, parameterize, store.
                let parsed =
                    parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?;
                let (cached_expr, parameter_count) = if let Some(prepared) =
                    crate::storage::query::planner::shape::parameterize_query_expr(&parsed)
                {
                    (prepared.shape, prepared.parameter_count)
                } else {
                    (parsed.clone(), 0)
                };
                {
                    let mut pc = self.inner.query_cache.write();
                    let plan = crate::storage::query::planner::QueryPlan::new(
                        parsed.clone(),
                        cached_expr,
                        Default::default(),
                    );
                    pc.insert(
                        cache_key.clone(),
                        crate::storage::query::planner::CachedPlan::new(plan)
                            .with_shape_key(cache_key.clone())
                            .with_exact_query(query.to_string())
                            .with_parameter_count(parameter_count),
                    );
                }
                parsed
            }
        };

        // Phase 5 PG parity: substitute any registered view name that
        // appears in the expression with its stored body. Runs here
        // (after parse, before dispatch) so the SQL entrypoint gets
        // the same view resolution `execute_query_expr` already does.
        let expr = self.rewrite_view_refs(expr);

        let statement = query_expr_name(&expr);
        let result_cache_scopes = query_expr_result_cache_scopes(&expr);

        // Phase 1 perf-parity — intent-lock acquisition. Reads take
        // IS (T3), writes take IX (T4), DDL takes Collection X with
        // Global IX (T5). Guard drops at end of dispatch so the lock
        // holds only for one statement. Gated on
        // `concurrency.locking.enabled` so the feature can be
        // disabled via env / SET CONFIG without reverting.
        let _lock_guard = if self.config_bool("concurrency.locking.enabled", true) {
            intent_lock_modes_for(&expr).map(|(global_mode, coll_mode)| {
                let mut g =
                    crate::runtime::locking::LockerGuard::new(self.inner.lock_manager.clone());
                // Non-fatal on failure: a misbehaving lock manager
                // shouldn't wedge queries. Errors surface via tracing.
                let _ = g.acquire(crate::runtime::locking::Resource::Global, global_mode);
                for collection in collections_referenced(&expr) {
                    let _ = g.acquire(
                        crate::runtime::locking::Resource::Collection(collection),
                        coll_mode,
                    );
                }
                g
            })
        } else {
            None
        };

        let query_result = match expr {
            QueryExpr::Graph(_) | QueryExpr::Path(_) => {
                // Apply MVCC visibility + RLS gate while materialising the
                // graph: every node entity is screened against the source
                // collection's policy chain (basic and `Nodes`-targeted)
                // and dropped when the caller's tenant / role doesn't
                // admit it. Edges are pruned automatically because the
                // graph builder skips edges whose endpoints aren't in
                // `allowed_nodes`.
                let (graph, node_properties) = self.materialize_graph_with_rls()?;
                let result =
                    crate::storage::query::unified::UnifiedExecutor::execute_on_with_node_properties(
                        &graph,
                        &expr,
                        node_properties,
                    )
                        .map_err(|err| RedDBError::Query(err.to_string()))?;

                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement,
                    engine: "materialized-graph",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            QueryExpr::Table(table) => {
                // Foreign-table intercept (Phase 3.2.2 PG parity).
                //
                // When the referenced table matches a `CREATE FOREIGN TABLE`
                // registration, short-circuit into the FDW scan. Phase 3.2
                // wrappers don't yet support pushdown, so filters/projections
                // apply post-scan via `apply_foreign_table_filters` — good
                // enough for correctness; perf work lands in 3.2.3.
                if self.inner.foreign_tables.is_foreign_table(&table.table) {
                    let records = self
                        .inner
                        .foreign_tables
                        .scan(&table.table)
                        .map_err(|e| RedDBError::Internal(e.to_string()))?;
                    let result = apply_foreign_table_filters(records, &table);
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-fdw",
                        result,
                        affected_rows: 0,
                        statement_type: "select",
                    });
                }

                // Row-Level Security enforcement (Phase 2.5.2 PG parity).
                //
                // When RLS is enabled on this table, fetch every policy
                // that applies to the current (role, SELECT) pair and
                // fold them into the query's WHERE clause: policies
                // OR-combine (any of them admitting the row is enough),
                // then AND into the caller's existing filter.
                //
                // Anonymous callers (no thread-local identity) pass
                // `role = None`; policies with a specific `TO role`
                // clause skip, but `TO PUBLIC` policies still apply.
                //
                // When `inject_rls_filters` returns `None` the table has
                // RLS enabled but no policy admits the caller's role —
                // short-circuit with an empty result set instead of
                // synthesising a contradiction filter.
                let table_with_rls = if self.inner.rls_enabled_tables.read().contains(&table.table)
                {
                    match inject_rls_filters(self, table) {
                        Some(t) => t,
                        None => {
                            let empty = crate::storage::query::unified::UnifiedResult::empty();
                            return Ok(RuntimeQueryResult {
                                query: query.to_string(),
                                mode,
                                statement,
                                engine: "runtime-table-rls",
                                result: empty,
                                affected_rows: 0,
                                statement_type: "select",
                            });
                        }
                    }
                } else {
                    table
                };
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement,
                    engine: "runtime-table",
                    result: execute_runtime_table_query(
                        &self.inner.db,
                        &table_with_rls,
                        Some(&self.inner.index_store),
                    )?,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            QueryExpr::Join(join) => {
                // Fold per-table RLS filters into each `QueryExpr::Table`
                // leaf of the join tree before executing. Without this
                // the join executor scans both tables raw and ignores
                // policies — a `WITHIN TENANT 'x'` against a join of
                // two tenant-scoped tables would leak cross-tenant rows.
                // When any leaf has RLS enabled and zero matching policy,
                // short-circuit to an empty join result instead of
                // emitting a contradiction filter.
                let join_with_rls = match inject_rls_into_join(self, join) {
                    Some(j) => j,
                    None => {
                        return Ok(RuntimeQueryResult {
                            query: query.to_string(),
                            mode,
                            statement,
                            engine: "runtime-join-rls",
                            result: crate::storage::query::unified::UnifiedResult::empty(),
                            affected_rows: 0,
                            statement_type: "select",
                        });
                    }
                };
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement,
                    engine: "runtime-join",
                    result: execute_runtime_join_query(&self.inner.db, &join_with_rls)?,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            QueryExpr::Vector(vector) => Ok(RuntimeQueryResult {
                query: query.to_string(),
                mode,
                statement,
                engine: "runtime-vector",
                result: execute_runtime_vector_query(&self.inner.db, &vector)?,
                affected_rows: 0,
                statement_type: "select",
            }),
            QueryExpr::Hybrid(hybrid) => Ok(RuntimeQueryResult {
                query: query.to_string(),
                mode,
                statement,
                engine: "runtime-hybrid",
                result: execute_runtime_hybrid_query(&self.inner.db, &hybrid)?,
                affected_rows: 0,
                statement_type: "select",
            }),
            // DML execution
            QueryExpr::Insert(ref insert) => self.execute_insert(query, insert),
            QueryExpr::Update(ref update) => self.execute_update(query, update),
            QueryExpr::Delete(ref delete) => self.execute_delete(query, delete),
            // DDL execution
            QueryExpr::CreateTable(ref create) => self.execute_create_table(query, create),
            QueryExpr::DropTable(ref drop_tbl) => self.execute_drop_table(query, drop_tbl),
            QueryExpr::AlterTable(ref alter) => self.execute_alter_table(query, alter),
            QueryExpr::ExplainAlter(ref explain) => self.execute_explain_alter(query, explain),
            // Graph analytics commands
            QueryExpr::GraphCommand(ref cmd) => self.execute_graph_command(query, cmd),
            // Search commands
            QueryExpr::SearchCommand(ref cmd) => self.execute_search_command(query, cmd),
            // ASK: RAG query with LLM synthesis
            QueryExpr::Ask(ref ask) => self.execute_ask(query, ask),
            QueryExpr::CreateIndex(ref create_idx) => self.execute_create_index(query, create_idx),
            QueryExpr::DropIndex(ref drop_idx) => self.execute_drop_index(query, drop_idx),
            QueryExpr::ProbabilisticCommand(ref cmd) => {
                self.execute_probabilistic_command(query, cmd)
            }
            // Time-series DDL
            QueryExpr::CreateTimeSeries(ref ts) => self.execute_create_timeseries(query, ts),
            QueryExpr::DropTimeSeries(ref ts) => self.execute_drop_timeseries(query, ts),
            // Queue DDL and commands
            QueryExpr::CreateQueue(ref q) => self.execute_create_queue(query, q),
            QueryExpr::DropQueue(ref q) => self.execute_drop_queue(query, q),
            QueryExpr::QueueCommand(ref cmd) => self.execute_queue_command(query, cmd),
            QueryExpr::CreateTree(ref tree) => self.execute_create_tree(query, tree),
            QueryExpr::DropTree(ref tree) => self.execute_drop_tree(query, tree),
            QueryExpr::TreeCommand(ref cmd) => self.execute_tree_command(query, cmd),
            // SET CONFIG key = value
            QueryExpr::SetConfig { ref key, ref value } => {
                let store = self.inner.db.store();
                let json_val = match value {
                    Value::Text(s) => crate::serde_json::Value::String(s.clone()),
                    Value::Integer(n) => crate::serde_json::Value::Number(*n as f64),
                    Value::Float(n) => crate::serde_json::Value::Number(*n),
                    Value::Boolean(b) => crate::serde_json::Value::Bool(*b),
                    _ => crate::serde_json::Value::String(value.to_string()),
                };
                store.set_config_tree(key, &json_val);
                // Config changes can flip runtime behavior mid-session
                // (auto_decrypt, auto_encrypt, etc.) — invalidate the
                // result cache so subsequent reads re-execute against
                // the new config.
                self.invalidate_result_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("config set: {key}"),
                    "set",
                ))
            }
            // SHOW CONFIG [prefix]
            QueryExpr::ShowConfig { ref prefix } => {
                let store = self.inner.db.store();
                let all_collections = store.list_collections();
                if !all_collections.contains(&"red_config".to_string()) {
                    let result = UnifiedResult::with_columns(vec!["key".into(), "value".into()]);
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement: "show_config",
                        engine: "runtime-config",
                        result,
                        affected_rows: 0,
                        statement_type: "select",
                    });
                }
                let manager = store
                    .get_collection("red_config")
                    .ok_or_else(|| RedDBError::NotFound("red_config".to_string()))?;
                let entities = manager.query_all(|_| true);
                let mut result = UnifiedResult::with_columns(vec!["key".into(), "value".into()]);
                for entity in entities {
                    if let EntityData::Row(ref row) = entity.data {
                        if let Some(ref named) = row.named {
                            let key_val = named.get("key").cloned().unwrap_or(Value::Null);
                            let val = named.get("value").cloned().unwrap_or(Value::Null);
                            let key_str = match &key_val {
                                Value::Text(s) => s.as_str(),
                                _ => continue,
                            };
                            if let Some(ref pfx) = prefix {
                                if !key_str.starts_with(pfx.as_str()) {
                                    continue;
                                }
                            }
                            let mut record = UnifiedRecord::new();
                            record.set("key", key_val);
                            record.set("value", val);
                            result.push(record);
                        }
                    }
                }
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement: "show_config",
                    engine: "runtime-config",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            // Session-local multi-tenancy handle (Phase 2.5.3).
            //
            // SET TENANT 'id' / SET TENANT NULL / RESET TENANT — writes
            // the thread-local; SHOW TENANT returns it. Paired with the
            // CURRENT_TENANT() scalar for use in RLS policies.
            QueryExpr::SetTenant(ref value) => {
                match value {
                    Some(id) => set_current_tenant(id.clone()),
                    None => clear_current_tenant(),
                }
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &match value {
                        Some(id) => format!("tenant set: {id}"),
                        None => "tenant cleared".to_string(),
                    },
                    "set_tenant",
                ))
            }
            QueryExpr::ShowTenant => {
                let mut result = UnifiedResult::with_columns(vec!["tenant".into()]);
                let mut record = UnifiedRecord::new();
                record.set(
                    "tenant",
                    current_tenant().map(Value::Text).unwrap_or(Value::Null),
                );
                result.push(record);
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement: "show_tenant",
                    engine: "runtime-tenant",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            // Transaction control (Phase 2.3 PG parity).
            //
            // BEGIN allocates a real `Xid` and stores a `TxnContext` keyed by
            // the current connection's id. COMMIT/ROLLBACK release it through
            // the `SnapshotManager` so future snapshots see the correct set of
            // active/aborted transactions.
            //
            // Tuple stamping (xmin/xmax) and read-path visibility filtering
            // land in Phase 2.3.2 — this dispatch only manages the snapshot
            // registry. Statements running outside a TxnContext still behave
            // as autocommit (xid=0 → visible to every snapshot).
            QueryExpr::TransactionControl(ref ctl) => {
                use crate::storage::query::ast::TxnControl;
                use crate::storage::transaction::snapshot::{TxnContext, Xid};
                use crate::storage::transaction::IsolationLevel;

                // Phase 2.3 keys transactions by a thread-local connection id.
                // The stdio/gRPC paths wire a real per-connection id later;
                // for embedded use (one RedDBRuntime per process-ish caller)
                // we fall back to a deterministic placeholder.
                let conn_id = current_connection_id();

                let (kind, msg) = match ctl {
                    TxnControl::Begin => {
                        let mgr = Arc::clone(&self.inner.snapshot_manager);
                        let xid = mgr.begin();
                        let snapshot = mgr.snapshot(xid);
                        let ctx = TxnContext {
                            xid,
                            isolation: IsolationLevel::SnapshotIsolation,
                            snapshot,
                            savepoints: Vec::new(),
                            released_sub_xids: Vec::new(),
                        };
                        self.inner.tx_contexts.write().insert(conn_id, ctx);
                        ("begin", format!("BEGIN — xid={xid} (snapshot isolation)"))
                    }
                    TxnControl::Commit => {
                        // SET LOCAL TENANT ends with the transaction.
                        self.inner.tx_local_tenants.write().remove(&conn_id);
                        let ctx = self.inner.tx_contexts.write().remove(&conn_id);
                        match ctx {
                            Some(ctx) => {
                                // Phase 2.3.2e: commit every open sub-xid
                                // so they also become visible. Their
                                // work is promoted to the parent txn's
                                // result exactly like a RELEASE would
                                // have done.
                                for (_, sub) in &ctx.savepoints {
                                    self.inner.snapshot_manager.commit(*sub);
                                }
                                for sub in &ctx.released_sub_xids {
                                    self.inner.snapshot_manager.commit(*sub);
                                }
                                self.inner.snapshot_manager.commit(ctx.xid);
                                // Phase 2.3.2b: physically remove tuples the txn
                                // marked for deletion. Before commit the rows
                                // only had their xmax stamped — now the
                                // deletion is durable.
                                self.finalize_pending_tombstones(conn_id);
                                ("commit", format!("COMMIT — xid={} committed", ctx.xid))
                            }
                            None => (
                                "commit",
                                "COMMIT outside transaction — no-op (autocommit)".to_string(),
                            ),
                        }
                    }
                    TxnControl::Rollback => {
                        self.inner.tx_local_tenants.write().remove(&conn_id);
                        let ctx = self.inner.tx_contexts.write().remove(&conn_id);
                        match ctx {
                            Some(ctx) => {
                                // Phase 2.3.2e: abort every open sub-xid
                                // too so their writes stay hidden.
                                for (_, sub) in &ctx.savepoints {
                                    self.inner.snapshot_manager.rollback(*sub);
                                }
                                for sub in &ctx.released_sub_xids {
                                    self.inner.snapshot_manager.rollback(*sub);
                                }
                                self.inner.snapshot_manager.rollback(ctx.xid);
                                // Phase 2.3.2b: tuples that the txn had
                                // xmax-stamped become live again — wipe xmax
                                // back to 0 so later snapshots see them.
                                self.revive_pending_tombstones(conn_id);
                                ("rollback", format!("ROLLBACK — xid={} aborted", ctx.xid))
                            }
                            None => (
                                "rollback",
                                "ROLLBACK outside transaction — no-op (autocommit)".to_string(),
                            ),
                        }
                    }
                    // Phase 2.3.2e: savepoints map onto sub-xids. Each
                    // SAVEPOINT allocates a fresh xid and pushes it
                    // onto the per-txn stack so subsequent writes can
                    // be selectively rolled back. RELEASE pops without
                    // aborting; ROLLBACK TO aborts the sub-xid (and
                    // any nested ones) + revives their tombstones.
                    TxnControl::Savepoint(name) => {
                        let mgr = Arc::clone(&self.inner.snapshot_manager);
                        let mut guard = self.inner.tx_contexts.write();
                        match guard.get_mut(&conn_id) {
                            Some(ctx) => {
                                let sub = mgr.begin();
                                ctx.savepoints.push((name.clone(), sub));
                                ("savepoint", format!("SAVEPOINT {name} — sub_xid={sub}"))
                            }
                            None => (
                                "savepoint",
                                "SAVEPOINT outside transaction — no-op".to_string(),
                            ),
                        }
                    }
                    TxnControl::ReleaseSavepoint(name) => {
                        let mut guard = self.inner.tx_contexts.write();
                        match guard.get_mut(&conn_id) {
                            Some(ctx) => {
                                let pos = ctx
                                    .savepoints
                                    .iter()
                                    .position(|(n, _)| n == name)
                                    .ok_or_else(|| {
                                        RedDBError::Internal(format!(
                                            "savepoint {name} does not exist"
                                        ))
                                    })?;
                                // RELEASE pops the named savepoint and
                                // any nested ones. Their sub-xids move
                                // to `released_sub_xids` so they commit
                                // (or roll back) alongside the parent
                                // xid — PG semantics: released
                                // savepoints still contribute their
                                // work, but their names are gone.
                                let released = ctx.savepoints.len() - pos;
                                let popped: Vec<Xid> = ctx
                                    .savepoints
                                    .split_off(pos)
                                    .into_iter()
                                    .map(|(_, x)| x)
                                    .collect();
                                ctx.released_sub_xids.extend(popped);
                                (
                                    "release_savepoint",
                                    format!("RELEASE SAVEPOINT {name} — {released} level(s)"),
                                )
                            }
                            None => (
                                "release_savepoint",
                                "RELEASE outside transaction — no-op".to_string(),
                            ),
                        }
                    }
                    TxnControl::RollbackToSavepoint(name) => {
                        let mgr = Arc::clone(&self.inner.snapshot_manager);
                        // Splice out the savepoint + nested ones under
                        // a narrow lock, then run the snapshot-manager
                        // + tombstone side-effects without the tx map
                        // held so nothing re-enters.
                        let drop_result: Option<(Xid, Vec<Xid>)> = {
                            let mut guard = self.inner.tx_contexts.write();
                            if let Some(ctx) = guard.get_mut(&conn_id) {
                                let pos = ctx
                                    .savepoints
                                    .iter()
                                    .position(|(n, _)| n == name)
                                    .ok_or_else(|| {
                                        RedDBError::Internal(format!(
                                            "savepoint {name} does not exist"
                                        ))
                                    })?;
                                let savepoint_xid = ctx.savepoints[pos].1;
                                let aborted: Vec<Xid> = ctx
                                    .savepoints
                                    .split_off(pos)
                                    .into_iter()
                                    .map(|(_, x)| x)
                                    .collect();
                                Some((savepoint_xid, aborted))
                            } else {
                                None
                            }
                        };

                        match drop_result {
                            Some((savepoint_xid, aborted)) => {
                                for x in &aborted {
                                    mgr.rollback(*x);
                                }
                                let revived = self.revive_tombstones_since(conn_id, savepoint_xid);
                                (
                                    "rollback_to_savepoint",
                                    format!(
                                        "ROLLBACK TO SAVEPOINT {name} — aborted {} sub_xid(s), revived {revived} tombstone(s)",
                                        aborted.len()
                                    ),
                                )
                            }
                            None => (
                                "rollback_to_savepoint",
                                "ROLLBACK TO outside transaction — no-op".to_string(),
                            ),
                        }
                    }
                };
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &msg,
                    kind,
                ))
            }
            // Schema + Sequence DDL (Phase 1.3 PG parity).
            //
            // Schemas are lightweight logical namespaces: a CREATE SCHEMA call
            // just registers the name in `red_config` under `schema.{name}`.
            // Table lookups still happen by collection name; clients using
            // `schema.table` qualified names collapse to collection `schema.table`.
            //
            // Sequences persist a 64-bit counter + metadata (start, increment)
            // in `red_config` under `sequence.{name}.*`. Scalar callers
            // `nextval('name')` / `currval('name')` arrive with the MVCC phase
            // once we have a proper mutating-function dispatch path; for now the
            // DDL just establishes the catalog entry so clients don't error.
            QueryExpr::CreateSchema(ref q) => {
                let store = self.inner.db.store();
                let key = format!("schema.{}", q.name);
                if store.get_config(&key).is_some() {
                    if q.if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("schema {} already exists — skipped", q.name),
                            "create_schema",
                        ));
                    }
                    return Err(RedDBError::Internal(format!(
                        "schema {} already exists",
                        q.name
                    )));
                }
                store.set_config_tree(&key, &crate::serde_json::Value::Bool(true));
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("schema {} created", q.name),
                    "create_schema",
                ))
            }
            QueryExpr::DropSchema(ref q) => {
                let store = self.inner.db.store();
                let key = format!("schema.{}", q.name);
                let existed = store.get_config(&key).is_some();
                if !existed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "schema {} does not exist",
                        q.name
                    )));
                }
                // Remove marker from red_config via set to null.
                store.set_config_tree(&key, &crate::serde_json::Value::Null);
                let suffix = if q.cascade {
                    " (CASCADE accepted — tables untouched)"
                } else {
                    ""
                };
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("schema {} dropped{}", q.name, suffix),
                    "drop_schema",
                ))
            }
            QueryExpr::CreateSequence(ref q) => {
                let store = self.inner.db.store();
                let base = format!("sequence.{}", q.name);
                let start_key = format!("{base}.start");
                let incr_key = format!("{base}.increment");
                let curr_key = format!("{base}.current");
                if store.get_config(&start_key).is_some() {
                    if q.if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("sequence {} already exists — skipped", q.name),
                            "create_sequence",
                        ));
                    }
                    return Err(RedDBError::Internal(format!(
                        "sequence {} already exists",
                        q.name
                    )));
                }
                // Persist start + increment, and set current so the first
                // nextval returns `start`.
                let initial_current = q.start - q.increment;
                store.set_config_tree(
                    &start_key,
                    &crate::serde_json::Value::Number(q.start as f64),
                );
                store.set_config_tree(
                    &incr_key,
                    &crate::serde_json::Value::Number(q.increment as f64),
                );
                store.set_config_tree(
                    &curr_key,
                    &crate::serde_json::Value::Number(initial_current as f64),
                );
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!(
                        "sequence {} created (start={}, increment={})",
                        q.name, q.start, q.increment
                    ),
                    "create_sequence",
                ))
            }
            QueryExpr::DropSequence(ref q) => {
                let store = self.inner.db.store();
                let base = format!("sequence.{}", q.name);
                let existed = store.get_config(&format!("{base}.start")).is_some();
                if !existed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "sequence {} does not exist",
                        q.name
                    )));
                }
                for k in ["start", "increment", "current"] {
                    store.set_config_tree(&format!("{base}.{k}"), &crate::serde_json::Value::Null);
                }
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("sequence {} dropped", q.name),
                    "drop_sequence",
                ))
            }
            // Views — CREATE [MATERIALIZED] VIEW (Phase 2.1 PG parity).
            //
            // The view definition is stored in-memory on RuntimeInner (not
            // persisted). SELECTs that reference the view name will substitute
            // the stored `QueryExpr` via `resolve_view_reference` during
            // planning (same entry point used by table-name resolution).
            //
            // Materialized views additionally allocate a slot in
            // `MaterializedViewCache`; a REFRESH repopulates that slot.
            QueryExpr::CreateView(ref q) => {
                let mut views = self.inner.views.write();
                if views.contains_key(&q.name) && !q.or_replace {
                    if q.if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("view {} already exists — skipped", q.name),
                            "create_view",
                        ));
                    }
                    return Err(RedDBError::Internal(format!(
                        "view {} already exists",
                        q.name
                    )));
                }
                views.insert(q.name.clone(), Arc::new(q.clone()));
                drop(views);

                // Materialized view: register cache slot (data is empty until REFRESH).
                if q.materialized {
                    use crate::storage::cache::result::{MaterializedViewDef, RefreshPolicy};
                    let def = MaterializedViewDef {
                        name: q.name.clone(),
                        query: format!("<parsed view {}>", q.name),
                        dependencies: collect_table_refs(&q.query),
                        refresh: RefreshPolicy::Manual,
                    };
                    self.inner.materialized_views.write().register(def);
                }
                // Plan cache may have cached a plan that didn't know about this
                // view — invalidate so future references pick up the new binding.
                // Result cache gets flushed too: OR REPLACE must not serve a
                // prior execution of the obsolete body.
                self.invalidate_plan_cache();
                self.invalidate_result_cache();

                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!(
                        "{}view {} created",
                        if q.materialized { "materialized " } else { "" },
                        q.name
                    ),
                    "create_view",
                ))
            }
            QueryExpr::DropView(ref q) => {
                let mut views = self.inner.views.write();
                let existed = views.remove(&q.name).is_some();
                drop(views);
                if q.materialized || existed {
                    // Try the materialised cache too — silent if absent.
                    self.inner.materialized_views.write().remove(&q.name);
                }
                // Drop any plan / result cache entries that baked the
                // view body into their QueryExpr.
                self.invalidate_plan_cache();
                self.invalidate_result_cache();
                if !existed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "view {} does not exist",
                        q.name
                    )));
                }
                self.invalidate_plan_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("view {} dropped", q.name),
                    "drop_view",
                ))
            }
            QueryExpr::RefreshMaterializedView(ref q) => {
                // Look up the view definition, execute its underlying query,
                // and stash the serialized result in the materialised cache.
                let view = {
                    let views = self.inner.views.read();
                    views.get(&q.name).cloned()
                };
                let view = match view {
                    Some(v) => v,
                    None => {
                        return Err(RedDBError::Internal(format!(
                            "view {} does not exist",
                            q.name
                        )))
                    }
                };
                if !view.materialized {
                    return Err(RedDBError::Internal(format!(
                        "view {} is not materialized — REFRESH requires \
                         CREATE MATERIALIZED VIEW",
                        q.name
                    )));
                }
                // Execute the underlying query fresh.
                let inner_result = self.execute_query_expr((*view.query).clone())?;
                // Cache data = JSON-serialised result (opaque blob; read path
                // returns it verbatim for now).
                let serialized = format!("{:?}", inner_result.result);
                self.inner
                    .materialized_views
                    .write()
                    .refresh(&q.name, serialized.into_bytes());
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("materialized view {} refreshed", q.name),
                    "refresh_materialized_view",
                ))
            }
            // Row Level Security (Phase 2.5 PG parity).
            //
            // Policies live in an in-memory registry keyed by (table, name).
            // Enforcement (AND-ing the policy's USING clause into every
            // query's WHERE for the table) arrives in Phase 2.5.2 via the
            // filter compiler; this dispatch only manages the catalog.
            QueryExpr::CreatePolicy(ref q) => {
                let key = (q.table.clone(), q.name.clone());
                self.inner
                    .rls_policies
                    .write()
                    .insert(key, Arc::new(q.clone()));
                self.invalidate_plan_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("policy {} on {} created", q.name, q.table),
                    "create_policy",
                ))
            }
            QueryExpr::DropPolicy(ref q) => {
                let removed = self
                    .inner
                    .rls_policies
                    .write()
                    .remove(&(q.table.clone(), q.name.clone()))
                    .is_some();
                if !removed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "policy {} on {} does not exist",
                        q.name, q.table
                    )));
                }
                self.invalidate_plan_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("policy {} on {} dropped", q.name, q.table),
                    "drop_policy",
                ))
            }
            // Foreign Data Wrappers (Phase 3.2 PG parity).
            //
            // CREATE SERVER / CREATE FOREIGN TABLE register into the shared
            // `ForeignTableRegistry`. The read path consults that registry
            // before dispatching a SELECT — when the table name matches a
            // registered foreign table, we forward the scan to the wrapper
            // and skip the normal collection lookup.
            //
            // Phase 3.2 is in-memory only; persistence across restarts is a
            // 3.2.2 follow-up that mirrors the view registry pattern.
            QueryExpr::CreateServer(ref q) => {
                use crate::storage::fdw::FdwOptions;
                let registry = Arc::clone(&self.inner.foreign_tables);
                if registry.server(&q.name).is_some() {
                    if q.if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("server {} already exists — skipped", q.name),
                            "create_server",
                        ));
                    }
                    return Err(RedDBError::Internal(format!(
                        "server {} already exists",
                        q.name
                    )));
                }
                let mut opts = FdwOptions::new();
                for (k, v) in &q.options {
                    opts.values.insert(k.clone(), v.clone());
                }
                registry
                    .create_server(&q.name, &q.wrapper, opts)
                    .map_err(|e| RedDBError::Internal(e.to_string()))?;
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("server {} created (wrapper {})", q.name, q.wrapper),
                    "create_server",
                ))
            }
            QueryExpr::DropServer(ref q) => {
                let existed = self.inner.foreign_tables.drop_server(&q.name);
                if !existed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "server {} does not exist",
                        q.name
                    )));
                }
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!(
                        "server {} dropped{}",
                        q.name,
                        if q.cascade { " (cascade)" } else { "" }
                    ),
                    "drop_server",
                ))
            }
            QueryExpr::CreateForeignTable(ref q) => {
                use crate::storage::fdw::{FdwOptions, ForeignColumn, ForeignTable};
                let registry = Arc::clone(&self.inner.foreign_tables);
                if registry.foreign_table(&q.name).is_some() {
                    if q.if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("foreign table {} already exists — skipped", q.name),
                            "create_foreign_table",
                        ));
                    }
                    return Err(RedDBError::Internal(format!(
                        "foreign table {} already exists",
                        q.name
                    )));
                }
                let mut opts = FdwOptions::new();
                for (k, v) in &q.options {
                    opts.values.insert(k.clone(), v.clone());
                }
                let columns: Vec<ForeignColumn> = q
                    .columns
                    .iter()
                    .map(|c| ForeignColumn {
                        name: c.name.clone(),
                        data_type: c.data_type.clone(),
                        not_null: c.not_null,
                    })
                    .collect();
                registry
                    .create_foreign_table(ForeignTable {
                        name: q.name.clone(),
                        server_name: q.server.clone(),
                        columns,
                        options: opts,
                    })
                    .map_err(|e| RedDBError::Internal(e.to_string()))?;
                self.invalidate_plan_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("foreign table {} created (server {})", q.name, q.server),
                    "create_foreign_table",
                ))
            }
            QueryExpr::DropForeignTable(ref q) => {
                let existed = self.inner.foreign_tables.drop_foreign_table(&q.name);
                if !existed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "foreign table {} does not exist",
                        q.name
                    )));
                }
                self.invalidate_plan_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("foreign table {} dropped", q.name),
                    "drop_foreign_table",
                ))
            }
            // COPY table FROM 'path' (Phase 1.5 PG parity).
            //
            // Stream CSV rows through the shared `CsvImporter`. The collection
            // is auto-created on first insert (via `insert_auto`-style path);
            // VACUUM/ANALYZE afterwards is up to the caller.
            QueryExpr::CopyFrom(ref q) => {
                use crate::storage::import::{CsvConfig, CsvImporter};
                let store = self.inner.db.store();
                let cfg = CsvConfig {
                    collection: q.table.clone(),
                    has_header: q.has_header,
                    delimiter: q.delimiter.map(|c| c as u8).unwrap_or(b','),
                    ..CsvConfig::default()
                };
                let importer = CsvImporter::new(cfg);
                let stats = importer
                    .import_file(&q.path, store.as_ref())
                    .map_err(|e| RedDBError::Internal(format!("COPY failed: {e}")))?;
                // Tables are written → invalidate cached plans / result cache.
                self.note_table_write(&q.table);
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!(
                        "COPY imported {} rows into {} ({} errors skipped, {}ms)",
                        stats.records_imported, q.table, stats.errors_skipped, stats.duration_ms
                    ),
                    "copy_from",
                ))
            }
            // Maintenance commands (Phase 1.2 PG parity).
            //
            // - VACUUM [FULL] [table]: refreshes planner stats for the target
            //   collection(s) and — when FULL — triggers a full pager persist
            //   (flushes dirty pages + fsync). Also invalidates the result cache
            //   so subsequent reads re-execute against the freshly compacted
            //   storage. RedDB's segment/btree GC runs continuously via the
            //   background lifecycle; explicit space reclamation for sealed
            //   segments arrives with Phase 2.3 (MVCC + dead-tuple reclamation).
            // - ANALYZE [table]: reruns `analyze_collection` +
            //   `persist_table_stats` via `refresh_table_planner_stats` so the
            //   planner has fresh histograms, distinct estimates, null counts.
            //
            // Both commands accept an optional target; omitting the target
            // iterates every collection in the store.
            QueryExpr::MaintenanceCommand(ref cmd) => {
                use crate::storage::query::ast::MaintenanceCommand as Mc;
                let store = self.inner.db.store();
                let (kind, msg) = match cmd {
                    Mc::Analyze { target } => {
                        let targets: Vec<String> = match target {
                            Some(t) => vec![t.clone()],
                            None => store.list_collections(),
                        };
                        for t in &targets {
                            self.refresh_table_planner_stats(t);
                        }
                        (
                            "analyze",
                            format!("ANALYZE refreshed stats for {} table(s)", targets.len()),
                        )
                    }
                    Mc::Vacuum { target, full } => {
                        let targets: Vec<String> = match target {
                            Some(t) => vec![t.clone()],
                            None => store.list_collections(),
                        };
                        // Stats refresh covers every target (same as ANALYZE).
                        for t in &targets {
                            self.refresh_table_planner_stats(t);
                        }
                        // FULL forces a pager persist (dirty-page flush + fsync).
                        // Regular VACUUM relies on the background writer / segment
                        // lifecycle so the command is non-blocking.
                        let persisted = if *full {
                            match store.persist() {
                                Ok(()) => true,
                                Err(e) => {
                                    return Err(RedDBError::Internal(format!(
                                        "VACUUM FULL persist failed: {e:?}"
                                    )));
                                }
                            }
                        } else {
                            false
                        };
                        // Result cache depended on pre-vacuum state.
                        self.invalidate_result_cache();
                        (
                            "vacuum",
                            format!(
                                "VACUUM{} processed {} table(s){}",
                                if *full { " FULL" } else { "" },
                                targets.len(),
                                if persisted {
                                    " (pages flushed to disk)"
                                } else {
                                    ""
                                }
                            ),
                        )
                    }
                };
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &msg,
                    kind,
                ))
            }
        };

        // Decrypt Value::Secret columns in-place before caching, so
        // cached results match the post-decrypt shape and repeat
        // queries skip the per-row AES-GCM pass.
        let mut query_result = query_result;
        if let Ok(ref mut result) = query_result {
            if result.statement_type == "select" {
                self.apply_secret_decryption(result);
            }
        }

        // Cache SELECT results for 30s.
        // Skip: pre-serialized JSON (large clone), and result sets > 5 rows.
        // Large multi-row results (range scans, filtered scans) are rarely
        // repeated with the same literal values so the cache hit rate is near
        // zero while the clone cost (100 records × ~16 fields each) is high.
        // Aggregations (1 row) and point lookups (1 row) still benefit.
        if let Ok(ref result) = query_result {
            if result.statement_type == "select"
                && result.result.pre_serialized_json.is_none()
                && result.result.records.len() <= 5
            {
                let mut cache = self.inner.result_cache.write();
                let (ref mut map, ref mut order) = *cache;
                // Use the tenant-aware cache key we computed at entry.
                if !map.contains_key(&cache_key_str) {
                    order.push_back(cache_key_str.clone());
                }
                map.insert(
                    cache_key_str.clone(),
                    RuntimeResultCacheEntry {
                        result: result.clone(),
                        cached_at: std::time::Instant::now(),
                        scopes: result_cache_scopes,
                    },
                );
                while map.len() > 1000 {
                    if let Some(oldest) = order.pop_front() {
                        map.remove(&oldest);
                    } else {
                        break;
                    }
                }
            }
        }

        query_result
    }

    /// Execute a pre-parsed `QueryExpr` directly, bypassing SQL parsing and the
    /// plan cache. Used by the prepared-statement fast path so that `execute_prepared`
    /// calls pay zero parse + cache overhead.
    ///
    /// Applies secret decryption on SELECT results, identical to `execute_query`.
    pub(crate) fn execute_query_expr(&self, expr: QueryExpr) -> RedDBResult<RuntimeQueryResult> {
        // View rewrite (Phase 2.1): substitute any `QueryExpr::Table(tq)`
        // whose `tq.table` matches a registered view with the view's
        // underlying query. Safe to call even when no views are registered.
        let expr = self.rewrite_view_refs(expr);

        let statement = query_expr_name(&expr);
        let mode = detect_mode(statement);
        let query_str = statement;

        let result = self.dispatch_expr(expr, query_str, mode)?;
        let mut r = result;
        if r.statement_type == "select" {
            self.apply_secret_decryption(&mut r);
        }
        Ok(r)
    }

    /// Walk a `QueryExpr` and replace `QueryExpr::Table(tq)` nodes whose
    /// `tq.table` matches a registered view name with the view's stored
    /// body. Recurses through joins so `SELECT ... FROM t JOIN myview ...`
    /// resolves correctly. Pure operation — no side effects.
    fn rewrite_view_refs(&self, expr: QueryExpr) -> QueryExpr {
        // Fast path: no views registered → return original expression.
        if self.inner.views.read().is_empty() {
            return expr;
        }
        self.rewrite_view_refs_inner(expr)
    }

    fn rewrite_view_refs_inner(&self, expr: QueryExpr) -> QueryExpr {
        use crate::storage::query::ast::{Filter, TableSource};
        match expr {
            QueryExpr::Table(mut tq) => {
                // 1. If the TableSource is a subquery, recurse into it so
                //    `SELECT ... FROM (SELECT ... FROM myview) t` expands.
                //    The legacy `table` field (set to a synthetic
                //    "__subq_NNNN" sentinel) stays as-is so callers that
                //    read it keep compiling.
                if let Some(TableSource::Subquery(body)) = tq.source.take() {
                    tq.source = Some(TableSource::Subquery(Box::new(
                        self.rewrite_view_refs_inner(*body),
                    )));
                    return QueryExpr::Table(tq);
                }

                // 2. Restore the source field (took it above for match).
                // When the source was `None` or `TableSource::Name(_)`, the
                // real lookup key is `tq.table` — check the view registry.
                let maybe_view = {
                    let views = self.inner.views.read();
                    views.get(&tq.table).cloned()
                };
                let Some(view) = maybe_view else {
                    return QueryExpr::Table(tq);
                };

                // Recurse into the view body — views may reference other
                // views. The recursion yields the final QueryExpr we need
                // to merge the outer's filter / limit / offset into.
                let inner_expr = self.rewrite_view_refs_inner((*view.query).clone());

                // Phase 5: when the body is a Table we merge the outer
                // TableQuery's WHERE / LIMIT / OFFSET into it so stacked
                // views filter recursively. Non-table bodies (Search,
                // Ask, Vector, Graph, Hybrid) can't meaningfully combine
                // with an outer Table query today — return the body
                // verbatim; outer predicates are lost. Full projection
                // merge lands in Phase 5.2.
                match inner_expr {
                    QueryExpr::Table(mut inner_tq) => {
                        if let Some(outer_filter) = tq.filter.take() {
                            inner_tq.filter = Some(match inner_tq.filter.take() {
                                Some(existing) => {
                                    Filter::And(Box::new(existing), Box::new(outer_filter))
                                }
                                None => outer_filter,
                            });
                        }
                        if let Some(outer_limit) = tq.limit {
                            inner_tq.limit = Some(match inner_tq.limit {
                                Some(existing) => existing.min(outer_limit),
                                None => outer_limit,
                            });
                        }
                        if let Some(outer_offset) = tq.offset {
                            inner_tq.offset = Some(match inner_tq.offset {
                                Some(existing) => existing + outer_offset,
                                None => outer_offset,
                            });
                        }
                        QueryExpr::Table(inner_tq)
                    }
                    other => other,
                }
            }
            QueryExpr::Join(mut jq) => {
                jq.left = Box::new(self.rewrite_view_refs_inner(*jq.left));
                jq.right = Box::new(self.rewrite_view_refs_inner(*jq.right));
                QueryExpr::Join(jq)
            }
            // Other variants don't carry nested QueryExpr that can reference
            // a view by table name. Return as-is.
            other => other,
        }
    }

    /// Internal dispatch: route a `QueryExpr` to the appropriate executor.
    /// Shared by `execute_query` (after parse/cache) and `execute_query_expr`
    /// (direct call from prepared-statement handler).
    fn dispatch_expr(
        &self,
        expr: QueryExpr,
        query_str: &str,
        mode: QueryMode,
    ) -> RedDBResult<RuntimeQueryResult> {
        let statement = query_expr_name(&expr);
        match expr {
            QueryExpr::Graph(_) | QueryExpr::Path(_) => {
                // Graph queries are not cacheable as prepared statements.
                return Err(RedDBError::Query(
                    "graph queries cannot be used as prepared statements".to_string(),
                ));
            }
            QueryExpr::Table(table) => Ok(RuntimeQueryResult {
                query: query_str.to_string(),
                mode,
                statement,
                engine: "runtime-table",
                result: execute_runtime_table_query(
                    &self.inner.db,
                    &table,
                    Some(&self.inner.index_store),
                )?,
                affected_rows: 0,
                statement_type: "select",
            }),
            QueryExpr::Join(join) => Ok(RuntimeQueryResult {
                query: query_str.to_string(),
                mode,
                statement,
                engine: "runtime-join",
                result: execute_runtime_join_query(&self.inner.db, &join)?,
                affected_rows: 0,
                statement_type: "select",
            }),
            QueryExpr::Vector(vector) => Ok(RuntimeQueryResult {
                query: query_str.to_string(),
                mode,
                statement,
                engine: "runtime-vector",
                result: execute_runtime_vector_query(&self.inner.db, &vector)?,
                affected_rows: 0,
                statement_type: "select",
            }),
            QueryExpr::Hybrid(hybrid) => Ok(RuntimeQueryResult {
                query: query_str.to_string(),
                mode,
                statement,
                engine: "runtime-hybrid",
                result: execute_runtime_hybrid_query(&self.inner.db, &hybrid)?,
                affected_rows: 0,
                statement_type: "select",
            }),
            _ => Err(RedDBError::Query(format!(
                "prepared-statement execution does not support {statement} statements"
            ))),
        }
    }

    /// Ultra-fast path: detect `SELECT * FROM table WHERE _entity_id = N` by string pattern
    /// and execute it without SQL parsing or planning. Returns None if pattern doesn't match.
    fn try_fast_entity_lookup(&self, query: &str) -> Option<RedDBResult<RuntimeQueryResult>> {
        // Pattern: "SELECT * FROM <table> WHERE _entity_id = <id>"
        // or "SELECT * FROM <table> WHERE _entity_id =<id>"
        let q = query.trim();
        if !q.starts_with("SELECT") && !q.starts_with("select") {
            return None;
        }

        // Find "WHERE _entity_id = " or "WHERE _entity_id ="
        let where_pos = q
            .find("WHERE _entity_id")
            .or_else(|| q.find("where _entity_id"))?;
        let after_field = &q[where_pos + 16..].trim_start(); // skip "WHERE _entity_id"
        let after_eq = after_field.strip_prefix('=')?.trim_start();

        // Parse the entity ID number
        let id_str = after_eq.trim();
        let entity_id: u64 = id_str.parse().ok()?;

        // Extract table name: between "FROM " and " WHERE"
        let from_pos = q.find("FROM ").or_else(|| q.find("from "))? + 5;
        let table = q[from_pos..where_pos].trim();
        if table.is_empty()
            || table.contains(' ') && !table.contains(" AS ") && !table.contains(" as ")
        {
            return None; // complex query, fall through
        }
        let table_name = table.split_whitespace().next()?;

        // Direct entity lookup — skips SQL parse, plan cache, result
        // cache, view rewriter, RLS gate. Safe because the gating in
        // `execute_query` guarantees no scope override / no
        // transaction context is active. MVCC visibility is still
        // honoured against the current snapshot.
        let store = self.inner.db.store();
        let entity = store
            .get(
                table_name,
                crate::storage::unified::EntityId::new(entity_id),
            )
            .filter(entity_visible_under_current_snapshot);

        let count = if entity.is_some() { 1u64 } else { 0 };

        // Materialize a record so downstream consumers that walk
        // `result.records` (embedded runtime API, decrypt pass, CLI)
        // see the row. Previously only `pre_serialized_json` was
        // filled, which caused those consumers to see zero rows and
        // skewed benchmarks.
        let records: Vec<crate::storage::query::unified::UnifiedRecord> = entity
            .as_ref()
            .and_then(|e| runtime_table_record_from_entity(e.clone()))
            .into_iter()
            .collect();

        let json = match entity {
            Some(ref e) => execute_runtime_serialize_single_entity(e),
            None => r#"{"columns":[],"record_count":0,"selection":{"scope":"any"},"records":[]}"#
                .to_string(),
        };

        Some(Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "select",
            engine: "fast-entity-lookup",
            result: crate::storage::query::unified::UnifiedResult {
                columns: Vec::new(),
                records,
                stats: crate::storage::query::unified::QueryStats {
                    rows_scanned: count,
                    ..Default::default()
                },
                pre_serialized_json: Some(json),
            },
            affected_rows: 0,
            statement_type: "select",
        }))
    }

    /// Invalidate the result cache (call after any write operation).
    /// Full clear — use for DDL (DROP TABLE, schema changes) or when table is unknown.
    pub fn invalidate_result_cache(&self) {
        let mut cache = self.inner.result_cache.write();
        cache.0.clear();
        cache.1.clear();
    }

    /// Invalidate only result cache entries that declared a dependency on `table`.
    /// Cheaper than a full clear: unrelated tables keep their cached results.
    pub(crate) fn invalidate_result_cache_for_table(&self, table: &str) {
        // Hot-path probe: with a read lock, see if any cache entry
        // even references this table. The bench's `bulk_update`
        // pattern fires N independent UPDATE statements; each used
        // to grab the result-cache write lock unconditionally,
        // serialising the writers on this single mutex even though
        // the cache is empty (no SELECT cached against the table).
        // Read lock first → if no match, skip the write entirely.
        {
            let cache = self.inner.result_cache.read();
            let (ref map, _) = *cache;
            if map.is_empty() || !map.values().any(|entry| entry.scopes.contains(table)) {
                return;
            }
        }
        let mut cache = self.inner.result_cache.write();
        let (ref mut map, ref mut order) = *cache;
        map.retain(|_, entry| !entry.scopes.contains(table));
        order.retain(|key| map.contains_key(key));
    }

    pub(crate) fn invalidate_plan_cache(&self) {
        self.inner.query_cache.write().clear();
    }

    pub(crate) fn clear_table_planner_stats(&self, table: &str) {
        let store = self.inner.db.store();
        crate::storage::query::planner::stats_catalog::clear_table_stats(store.as_ref(), table);
        self.invalidate_plan_cache();
    }

    /// Replay `tenant_tables.*.column` keys from red_config at boot so
    /// `CREATE TABLE ... TENANT BY (col)` declarations persist across
    /// restarts (Phase 2.5.4). Reads every row of the `red_config`
    /// collection, picks the keys matching the tenant-marker shape,
    /// and calls `register_tenant_table` for each.
    ///
    /// Safe no-op when `red_config` doesn't exist (first boot on a
    /// fresh datadir).
    pub(crate) fn rehydrate_tenant_tables(&self) {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return;
        };
        // Replay in insertion order (SegmentManager iteration). Multiple
        // toggles on the same table leave several rows behind — the
        // last one processed wins because each register/unregister
        // call overwrites the in-memory state.
        for entity in manager.query_all(|_| true) {
            let crate::storage::unified::entity::EntityData::Row(row) = &entity.data else {
                continue;
            };
            let Some(named) = &row.named else { continue };
            let Some(crate::storage::schema::Value::Text(key)) = named.get("key") else {
                continue;
            };
            // Shape: tenant_tables.{table}.column
            let Some(rest) = key.strip_prefix("tenant_tables.") else {
                continue;
            };
            let Some((table, suffix)) = rest.rsplit_once('.') else {
                continue;
            };
            if suffix != "column" {
                continue;
            }
            match named.get("value") {
                Some(crate::storage::schema::Value::Text(column)) => {
                    self.register_tenant_table(table, column);
                }
                // Null / missing value = DISABLE TENANCY marker.
                Some(crate::storage::schema::Value::Null) | None => {
                    self.unregister_tenant_table(table);
                }
                _ => {}
            }
        }
    }

    /// Register a table as tenant-scoped (Phase 2.5.4). Installs the
    /// in-memory column mapping, the implicit RLS policy, and enables
    /// row-level security on the table. Idempotent — re-registering
    /// the same `(table, column)` replaces the prior auto-policy.
    pub fn register_tenant_table(&self, table: &str, column: &str) {
        use crate::storage::query::ast::{
            CompareOp, CreatePolicyQuery, Expr, FieldRef, Filter, Span,
        };
        self.inner
            .tenant_tables
            .write()
            .insert(table.to_string(), column.to_string());

        // Build the policy: col = CURRENT_TENANT()
        // Uses CompareExpr so the comparison happens at runtime against
        // the thread-local tenant value read by the CURRENT_TENANT
        // scalar. Spans are synthetic — there's no source location for
        // an auto-generated policy.
        let lhs = Expr::Column {
            field: FieldRef::TableColumn {
                table: table.to_string(),
                column: column.to_string(),
            },
            span: Span::synthetic(),
        };
        let rhs = Expr::FunctionCall {
            name: "CURRENT_TENANT".to_string(),
            args: Vec::new(),
            span: Span::synthetic(),
        };
        let policy_filter = Filter::CompareExpr {
            lhs,
            op: CompareOp::Eq,
            rhs,
        };

        let policy = CreatePolicyQuery {
            name: "__tenant_iso".to_string(),
            table: table.to_string(),
            action: None, // None = ALL actions (SELECT/INSERT/UPDATE/DELETE)
            role: None,   // None = every role
            using: Box::new(policy_filter),
            // Auto-tenancy defaults to Table targets. Collections of
            // other kinds (graph / vector / queue / timeseries) that
            // opt in via `ALTER ... ENABLE TENANCY` should use the
            // matching kind — but for now we keep the auto-policy
            // kind-agnostic so the evaluator can apply it to any
            // entity living in the collection.
            target_kind: crate::storage::query::ast::PolicyTargetKind::Table,
        };

        // Replace any prior auto-policy for this table (column rename).
        self.inner.rls_policies.write().insert(
            (table.to_string(), "__tenant_iso".to_string()),
            Arc::new(policy),
        );
        self.inner
            .rls_enabled_tables
            .write()
            .insert(table.to_string());

        // Auto-build a hash index on the tenant column. Every read/write
        // against a tenant-scoped table carries an implicit
        // `col = CURRENT_TENANT()` predicate from the auto-policy, so an
        // index on that column is on the hot path of every query. Without
        // it, every SELECT/UPDATE/DELETE degrades to a full scan.
        self.ensure_tenant_index(table, column);
    }

    /// Auto-create the hash index that backs the tenant-iso RLS predicate.
    /// Skipped when:
    ///   * the column is dotted (nested path — flat secondary indices
    ///     don't cover those today; RLS still works via the policy)
    ///   * `__tenant_idx_{table}` already exists (idempotent on rehydrate)
    ///   * the user already registered an index whose first column matches
    ///     (avoids redundant duplicates of a user-defined composite)
    fn ensure_tenant_index(&self, table: &str, column: &str) {
        if column.contains('.') {
            return;
        }
        let index_name = format!("__tenant_idx_{table}");
        let registry = self.inner.index_store.list_indices(table);
        if registry.iter().any(|idx| idx.name == index_name) {
            return;
        }
        if registry
            .iter()
            .any(|idx| idx.columns.first().map(|c| c.as_str()) == Some(column))
        {
            return;
        }

        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(table) else {
            return;
        };
        let entities = manager.query_all(|_| true);
        let entity_fields: Vec<(
            crate::storage::unified::EntityId,
            Vec<(String, crate::storage::schema::Value)>,
        )> = entities
            .iter()
            .map(|e| {
                let fields = match &e.data {
                    crate::storage::EntityData::Row(row) => {
                        if let Some(ref named) = row.named {
                            named.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                        } else if let Some(ref schema) = row.schema {
                            schema
                                .iter()
                                .zip(row.columns.iter())
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect()
                        } else {
                            Vec::new()
                        }
                    }
                    crate::storage::EntityData::Node(node) => node
                        .properties
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    _ => Vec::new(),
                };
                (e.id, fields)
            })
            .collect();

        let columns = vec![column.to_string()];
        if self
            .inner
            .index_store
            .create_index(
                &index_name,
                table,
                &columns,
                super::index_store::IndexMethodKind::Hash,
                false,
                &entity_fields,
            )
            .is_err()
        {
            return;
        }
        self.inner
            .index_store
            .register(super::index_store::RegisteredIndex {
                name: index_name,
                collection: table.to_string(),
                columns,
                method: super::index_store::IndexMethodKind::Hash,
                unique: false,
            });
        self.invalidate_plan_cache();
    }

    /// Drop the auto-generated tenant index, if one exists. Called from
    /// `unregister_tenant_table` so DISABLE TENANCY / DROP TABLE clean up.
    fn drop_tenant_index(&self, table: &str) {
        let index_name = format!("__tenant_idx_{table}");
        self.inner.index_store.drop_index(&index_name, table);
    }

    /// Retrieve the tenant column for a table, if any (Phase 2.5.4).
    /// Used by the INSERT auto-fill path to know which column to
    /// populate with `current_tenant()` when the user didn't name it.
    pub fn tenant_column(&self, table: &str) -> Option<String> {
        self.inner.tenant_tables.read().get(table).cloned()
    }

    /// Remove a table's tenant registration (Phase 2.5.4). Called by
    /// DROP TABLE / ALTER TABLE DISABLE TENANCY. Removes the auto-policy
    /// but leaves any user-installed explicit policies intact.
    pub fn unregister_tenant_table(&self, table: &str) {
        self.inner.tenant_tables.write().remove(table);
        self.inner
            .rls_policies
            .write()
            .remove(&(table.to_string(), "__tenant_iso".to_string()));
        self.drop_tenant_index(table);
        // Only clear RLS enablement if no other policies remain.
        let has_other_policies = self
            .inner
            .rls_policies
            .read()
            .keys()
            .any(|(t, _)| t == table);
        if !has_other_policies {
            self.inner.rls_enabled_tables.write().remove(table);
        }
    }

    /// Record that the running transaction has marked `id` in `collection`
    /// for deletion (Phase 2.3.2b MVCC tombstones). `stamper_xid` is the
    /// xid that was written into `xmax` — either the parent txn xid or
    /// the innermost savepoint sub-xid. Savepoint rollback filters by
    /// this xid to revive only its own tombstones.
    pub(crate) fn record_pending_tombstone(
        &self,
        conn_id: u64,
        collection: &str,
        id: crate::storage::unified::entity::EntityId,
        stamper_xid: crate::storage::transaction::snapshot::Xid,
    ) {
        self.inner
            .pending_tombstones
            .write()
            .entry(conn_id)
            .or_default()
            .push((collection.to_string(), id, stamper_xid));
    }

    /// Flush tombstones on COMMIT — tuples are physically removed from
    /// storage. Safe to call with an empty list (no-op).
    pub(crate) fn finalize_pending_tombstones(&self, conn_id: u64) {
        let Some(pending) = self.inner.pending_tombstones.write().remove(&conn_id) else {
            return;
        };
        if pending.is_empty() {
            return;
        }

        // Group by collection so every batch issues a single `delete_batch`.
        let mut grouped: HashMap<String, Vec<crate::storage::unified::entity::EntityId>> =
            HashMap::new();
        for (collection, id, _xid) in pending {
            grouped.entry(collection).or_default().push(id);
        }

        let store = self.inner.db.store();
        for (collection, ids) in grouped {
            if let Err(err) = store.delete_batch(&collection, &ids) {
                // Best-effort: COMMIT already succeeded at the MVCC level
                // (xmax keeps the row hidden), so log and move on. A
                // later VACUUM will reclaim the storage.
                eprintln!(
                    "pending tombstone delete_batch failed for {collection}: {err}; \
                     rows stay xmax-stamped (reader-invisible) until VACUUM"
                );
                continue;
            }
            for id in &ids {
                store.context_index().remove_entity(*id);
                self.cdc_emit(
                    crate::replication::cdc::ChangeOperation::Delete,
                    &collection,
                    id.raw(),
                    "entity",
                );
            }
        }
    }

    /// Revive tombstones on ROLLBACK — reset `xmax` to 0 so the tuples
    /// become visible again to future snapshots. Best-effort: a row
    /// already reclaimed by a concurrent VACUUM stays gone, but VACUUM
    /// never reclaims tuples whose xmax is still referenced by any
    /// active snapshot, so this case is only reachable via external
    /// storage corruption.
    pub(crate) fn revive_pending_tombstones(&self, conn_id: u64) {
        let Some(pending) = self.inner.pending_tombstones.write().remove(&conn_id) else {
            return;
        };

        let store = self.inner.db.store();
        for (collection, id, _xid) in pending {
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            if let Some(mut entity) = manager.get(id) {
                entity.set_xmax(0);
                let _ = manager.update(entity);
            }
        }
    }

    /// Materialise the entire graph store while applying MVCC visibility
    /// AND per-collection RLS to each candidate node and edge. Mirrors
    /// `materialize_graph` but routes every entity through the same
    /// gate the SELECT path uses, with the correct `PolicyTargetKind`
    /// per entity kind (`Nodes` for graph nodes, `Edges` for graph
    /// edges). Returns the filtered `GraphStore` plus the
    /// `node_id → properties` map the executor needs for `RETURN n.*`
    /// projections.
    fn materialize_graph_with_rls(
        &self,
    ) -> RedDBResult<(
        crate::storage::engine::GraphStore,
        std::collections::HashMap<
            String,
            std::collections::HashMap<String, crate::storage::schema::Value>,
        >,
    )> {
        use crate::storage::engine::GraphStore;
        use crate::storage::query::ast::{PolicyAction, PolicyTargetKind};
        use crate::storage::unified::entity::{EntityData, EntityKind};
        use std::collections::{HashMap, HashSet};

        let store = self.inner.db.store();
        let snap_ctx = capture_current_snapshot();
        let role = current_auth_identity().map(|(_, r)| r.as_str().to_string());

        let graph = GraphStore::new();
        let mut node_properties: HashMap<String, HashMap<String, crate::storage::schema::Value>> =
            HashMap::new();
        let mut allowed_nodes: HashSet<String> = HashSet::new();

        // Per-collection cached compiled filters — Nodes-kind for
        // first pass, Edges-kind for the second. None entries mean
        // "RLS enabled, zero matching policy → deny all of this kind".
        let mut node_rls: HashMap<String, Option<crate::storage::query::ast::Filter>> =
            HashMap::new();
        let mut edge_rls: HashMap<String, Option<crate::storage::query::ast::Filter>> =
            HashMap::new();

        let collections = store.list_collections();

        // First pass — gather nodes.
        for collection in &collections {
            let Some(manager) = store.get_collection(collection) else {
                continue;
            };
            let entities = manager.query_all(|_| true);
            for entity in entities {
                if !entity_visible_with_context(snap_ctx.as_ref(), &entity) {
                    continue;
                }
                let EntityKind::GraphNode(ref node) = entity.kind else {
                    continue;
                };
                if !node_passes_rls(self, collection, role.as_deref(), &mut node_rls, &entity) {
                    continue;
                }
                let id_str = entity.id.raw().to_string();
                graph
                    .add_node(
                        &id_str,
                        &node.label,
                        super::graph_node_type(&node.node_type),
                    )
                    .map_err(|err| RedDBError::Query(err.to_string()))?;
                allowed_nodes.insert(id_str.clone());
                if let EntityData::Node(node_data) = &entity.data {
                    node_properties.insert(id_str, node_data.properties.clone());
                }
            }
        }

        // Second pass — gather edges. An edge appears only when both
        // endpoint nodes survived the RLS pass AND the edge itself
        // passes its own RLS gate.
        for collection in &collections {
            let Some(manager) = store.get_collection(collection) else {
                continue;
            };
            let entities = manager.query_all(|_| true);
            for entity in entities {
                if !entity_visible_with_context(snap_ctx.as_ref(), &entity) {
                    continue;
                }
                let EntityKind::GraphEdge(ref edge) = entity.kind else {
                    continue;
                };
                if !allowed_nodes.contains(&edge.from_node)
                    || !allowed_nodes.contains(&edge.to_node)
                {
                    continue;
                }
                if !edge_passes_rls(self, collection, role.as_deref(), &mut edge_rls, &entity) {
                    continue;
                }
                let weight = match &entity.data {
                    EntityData::Edge(e) => e.weight,
                    _ => edge.weight as f32 / 1000.0,
                };
                graph
                    .add_edge(
                        &edge.from_node,
                        &edge.to_node,
                        super::graph_edge_type(&edge.label),
                        weight,
                    )
                    .map_err(|err| RedDBError::Query(err.to_string()))?;
            }
        }

        // Suppress unused-PolicyAction/PolicyTargetKind warnings — both
        // are used inside the helper closures via the per-kind helpers
        // declared at the bottom of this file.
        let _ = (PolicyAction::Select, PolicyTargetKind::Nodes);

        Ok((graph, node_properties))
    }

    /// Phase 1.1 MVCC universal: post-save hook that stamps `xmin` on a
    /// freshly-inserted entity when the current connection holds an
    /// open transaction. Used by graph / vector / queue / timeseries
    /// write paths that go through the DevX builder API (`db.node(...)
    /// .save()` and friends) — those live in the storage crate and
    /// can't reach `current_xid()` without crossing layers, so the
    /// application layer calls this helper right after `save()` to
    /// finalise the MVCC stamp.
    ///
    /// Autocommit (outside BEGIN) is a no-op — no extra lookup or
    /// write, so the non-transactional hot path stays untouched.
    ///
    /// Best-effort: if the collection or entity disappears between
    /// the save and the stamp (concurrent DROP), we silently skip.
    pub(crate) fn stamp_xmin_if_in_txn(
        &self,
        collection: &str,
        id: crate::storage::unified::entity::EntityId,
    ) {
        let Some(xid) = self.current_xid() else {
            return;
        };
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(collection) else {
            return;
        };
        if let Some(mut entity) = manager.get(id) {
            entity.set_xmin(xid);
            let _ = manager.update(entity);
        }
    }

    /// Revive tombstones stamped by `stamper_xid` or any sub-xid
    /// allocated after it (Phase 2.3.2e savepoint rollback). Any
    /// pending entries with `xid < stamper_xid` stay queued because
    /// they belong to the enclosing scope — they'll either flush on
    /// COMMIT or revive on an outer ROLLBACK TO SAVEPOINT.
    ///
    /// Returns the number of tuples whose `xmax` was wiped back to 0.
    pub(crate) fn revive_tombstones_since(&self, conn_id: u64, stamper_xid: u64) -> usize {
        let mut guard = self.inner.pending_tombstones.write();
        let Some(pending) = guard.get_mut(&conn_id) else {
            return 0;
        };

        let store = self.inner.db.store();
        let mut revived = 0usize;
        pending.retain(|(collection, id, xid)| {
            if *xid < stamper_xid {
                // Stamped before the savepoint — keep in queue.
                return true;
            }
            if let Some(manager) = store.get_collection(collection) {
                if let Some(mut entity) = manager.get(*id) {
                    entity.set_xmax(0);
                    let _ = manager.update(entity);
                    revived += 1;
                }
            }
            false
        });
        if pending.is_empty() {
            guard.remove(&conn_id);
        }
        revived
    }

    /// Return the snapshot the current connection should use for visibility
    /// checks (Phase 2.3 PG parity).
    ///
    /// * If the connection is inside a BEGIN-wrapped transaction, reuse
    ///   the snapshot stored in its `TxnContext`.
    /// * Otherwise (autocommit), capture a fresh snapshot tied to an
    ///   implicit xid=0 — the read path treats pre-MVCC rows as always
    ///   visible so this degrades to "see everything committed".
    pub fn current_snapshot(&self) -> crate::storage::transaction::snapshot::Snapshot {
        let conn_id = current_connection_id();
        if let Some(ctx) = self.inner.tx_contexts.read().get(&conn_id).cloned() {
            return ctx.snapshot;
        }
        // Autocommit: take a fresh snapshot bounded by `peek_next_xid` so
        // every already-committed xid (which is strictly less) passes the
        // `xmin <= snap.xid` gate, while concurrently-active xids land in
        // the `in_progress` set and stay hidden until they commit. Using
        // xid=0 would incorrectly hide every MVCC-stamped tuple.
        let high_water = self.inner.snapshot_manager.peek_next_xid();
        self.inner.snapshot_manager.snapshot(high_water)
    }

    /// Xid of the current connection's active transaction, or `None` when
    /// running outside a BEGIN/COMMIT block. Write paths call this to
    /// decide whether to stamp `xmin`/`xmax` on tuples.
    /// Phase 2.3.2e: when a savepoint is open, `writer_xid` returns the
    /// sub-xid so new writes can be selectively rolled back. Otherwise
    /// the parent txn's xid is returned, matching pre-savepoint
    /// behaviour. Callers that need the enclosing *transaction* xid
    /// (e.g. VACUUM min-active calculations) should read `ctx.xid`
    /// directly.
    pub fn current_xid(&self) -> Option<crate::storage::transaction::snapshot::Xid> {
        let conn_id = current_connection_id();
        self.inner
            .tx_contexts
            .read()
            .get(&conn_id)
            .map(|ctx| ctx.writer_xid())
    }

    /// Access the shared `SnapshotManager` — useful for VACUUM to compute
    /// the oldest-active xid when reclaiming dead tuples.
    pub fn snapshot_manager(&self) -> Arc<crate::storage::transaction::snapshot::SnapshotManager> {
        Arc::clone(&self.inner.snapshot_manager)
    }

    /// Own-tx xids (parent + open savepoints) for the current
    /// connection. Transports + tests that build a `SnapshotContext`
    /// manually (outside the `execute_query` scope) need this set so
    /// the writer's own uncommitted tuples stay visible to self.
    pub fn current_txn_own_xids(
        &self,
    ) -> std::collections::HashSet<crate::storage::transaction::snapshot::Xid> {
        let mut set = std::collections::HashSet::new();
        if let Some(ctx) = self.inner.tx_contexts.read().get(&current_connection_id()) {
            set.insert(ctx.xid);
            for (_, sub) in &ctx.savepoints {
                set.insert(*sub);
            }
        }
        set
    }

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

    pub(crate) fn refresh_table_planner_stats(&self, table: &str) {
        let store = self.inner.db.store();
        if let Some(stats) =
            crate::storage::query::planner::stats_catalog::analyze_collection(store.as_ref(), table)
        {
            crate::storage::query::planner::stats_catalog::persist_table_stats(
                store.as_ref(),
                &stats,
            );
        } else {
            crate::storage::query::planner::stats_catalog::clear_table_stats(store.as_ref(), table);
        }
        self.invalidate_plan_cache();
    }

    pub(crate) fn note_table_write(&self, table: &str) {
        // Skip the write lock when the table is already marked
        // dirty. With single-row UPDATEs in a loop this used to
        // grab the planner_dirty_tables write lock N times even
        // though the first call already flipped the flag.
        let already_dirty = self.inner.planner_dirty_tables.read().contains(table);
        if !already_dirty {
            self.inner
                .planner_dirty_tables
                .write()
                .insert(table.to_string());
        }
        self.invalidate_result_cache_for_table(table);
    }
}
