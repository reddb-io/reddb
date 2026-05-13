use super::*;
use crate::application::entity::metadata_to_json;
use crate::auth::column_policy_gate::ColumnAccessRequest;
use crate::auth::UserId;
use crate::replication::cdc::ChangeRecord;
use crate::replication::logical::{ApplyMode, LogicalChangeApplier};
use crate::storage::query::ast::TableSource;

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

    /// Cheap presence flag for `CURRENT_SNAPSHOT`. Scan hot paths
    /// poll this instead of `borrow()`-ing the RefCell on every
    /// row — the common case (autocommit / no MVCC session) reads
    /// one atomic `Cell<bool>` and short-circuits, saving ~10ns × N
    /// rows on aggregate_group / select_range scans.
    static HAS_SNAPSHOT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };

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

    /// Statement-local config resolver. SQL expressions materialize the
    /// `red_config` snapshot lazily on the first `$config.*`/`CONFIG()`
    /// access, keeping ordinary statements on the zero-scan path.
    static CURRENT_CONFIG_RESOLVER: std::cell::RefCell<Option<ConfigResolver>> =
        const { std::cell::RefCell::new(None) };

    /// Statement-local secret resolver. SQL expressions materialize the
    /// vault KV snapshot lazily on first `$secret.*` access, then use
    /// lock-free map reads for the rest of the statement.
    static CURRENT_SECRET_RESOLVER: std::cell::RefCell<Option<SecretResolver>> =
        const { std::cell::RefCell::new(None) };
}

fn secret_sql_value_to_string(value: &Value) -> RedDBResult<String> {
    match value {
        Value::Text(s) => Ok(s.to_string()),
        Value::Integer(n) => Ok(n.to_string()),
        Value::UnsignedInteger(n) => Ok(n.to_string()),
        Value::Float(n) => Ok(n.to_string()),
        Value::Boolean(b) => Ok(b.to_string()),
        Value::Null => Err(RedDBError::Query(
            "SET SECRET key = NULL deletes the secret; use DELETE SECRET for explicit deletes"
                .to_string(),
        )),
        Value::Password(_) | Value::Secret(_) => Err(RedDBError::Query(
            "SET SECRET accepts plain scalar literals; PASSWORD() and SECRET() are for typed columns"
                .to_string(),
        )),
        _ => Err(RedDBError::Query(format!(
            "SET SECRET does not support value type {:?} yet",
            value.data_type()
        ))),
    }
}

fn system_keyed_collection_contract(
    name: &str,
    model: crate::catalog::CollectionModel,
) -> crate::physical::CollectionContract {
    let now = crate::utils::now_unix_millis() as u128;
    crate::physical::CollectionContract {
        name: name.to_string(),
        declared_model: model,
        schema_mode: crate::catalog::SchemaMode::Dynamic,
        origin: crate::physical::ContractOrigin::Implicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: None,
        vector_dimension: None,
        vector_metric: None,
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def: None,
        timestamps_enabled: false,
        context_index_enabled: false,
        append_only: false,
        subscriptions: Vec::new(),
    }
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
/// connection's transaction — the parent xid plus open and released
/// savepoint sub-xids. The visibility rule promotes rows stamped with
/// these xids to "always visible (unless aborted)" so the writer sees
/// its own nested-savepoint writes even though their xids exceed
/// `snapshot.xid`.
#[derive(Clone)]
pub struct SnapshotContext {
    pub snapshot: crate::storage::transaction::snapshot::Snapshot,
    pub manager: Arc<crate::storage::transaction::snapshot::SnapshotManager>,
    pub own_xids: std::collections::HashSet<crate::storage::transaction::snapshot::Xid>,
    pub requires_index_fallback: bool,
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

pub(crate) fn current_secret_value(path: &str) -> Option<String> {
    let key = path.to_ascii_lowercase();
    CURRENT_SECRET_RESOLVER.with(|cell| {
        let mut resolver = cell.borrow_mut();
        let resolver = resolver.as_mut()?;
        if resolver.values.is_none() {
            resolver.values = resolver
                .store
                .as_ref()
                .map(|store| store.vault_kv_snapshot());
        }
        let values = resolver.values.as_ref()?;
        values.get(&key).cloned().or_else(|| {
            key.strip_prefix("red.vault/").and_then(|rest| {
                values
                    .get(rest)
                    .cloned()
                    .or_else(|| values.get(&format!("red.secret.{rest}")).cloned())
            })
        })
    })
}

struct SecretResolver {
    store: Option<Arc<crate::auth::store::AuthStore>>,
    values: Option<HashMap<String, String>>,
}

pub(super) struct SecretStoreGuard {
    previous: Option<SecretResolver>,
}

impl SecretStoreGuard {
    pub(super) fn install(store: Option<Arc<crate::auth::store::AuthStore>>) -> Self {
        let previous = CURRENT_SECRET_RESOLVER.with(|cell| {
            cell.replace(Some(SecretResolver {
                store,
                values: None,
            }))
        });
        Self { previous }
    }
}

impl Drop for SecretStoreGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT_SECRET_RESOLVER.with(|cell| {
            cell.replace(previous);
        });
    }
}

pub(crate) fn current_config_value(path: &str) -> Option<Value> {
    let key = path.to_ascii_lowercase();
    CURRENT_CONFIG_RESOLVER.with(|cell| {
        let mut resolver = cell.borrow_mut();
        let resolver = resolver.as_mut()?;
        if resolver.values.is_none() {
            resolver.values = Some(latest_config_snapshot(&resolver.db));
        }
        let values = resolver.values.as_ref()?;
        values.get(&key).cloned().or_else(|| {
            key.strip_prefix("red.config/")
                .and_then(|rest| values.get(&format!("red.config.{rest}")).cloned())
        })
    })
}

fn update_current_config_value(path: &str, value: Value) {
    let key = path.to_ascii_lowercase();
    CURRENT_CONFIG_RESOLVER.with(|cell| {
        if let Some(resolver) = cell.borrow_mut().as_mut() {
            if let Some(values) = resolver.values.as_mut() {
                values.insert(key, value);
            }
        }
    });
}

fn update_current_secret_value(path: &str, value: Option<String>) {
    let key = path.to_ascii_lowercase();
    CURRENT_SECRET_RESOLVER.with(|cell| {
        if let Some(resolver) = cell.borrow_mut().as_mut() {
            let Some(values) = resolver.values.as_mut() else {
                return;
            };
            match value {
                Some(value) => {
                    values.insert(key, value);
                }
                None => {
                    values.remove(&key);
                }
            }
        }
    });
}

fn latest_config_snapshot(db: &RedDB) -> HashMap<String, Value> {
    let mut latest: HashMap<String, (u64, Value)> = HashMap::new();

    if let Some(manager) = db.store().get_collection("red_config") {
        manager.for_each_entity(|entity| {
            let Some(row) = entity.data.as_row() else {
                return true;
            };
            let Some(Value::Text(key)) = row.get_field("key") else {
                return true;
            };
            let value = row.get_field("value").cloned().unwrap_or(Value::Null);
            let id = entity.id.raw();
            let key = key.to_ascii_lowercase();
            insert_latest_config_value(&mut latest, key.clone(), id, value.clone());
            if let Some(rest) = key.strip_prefix("red.config.") {
                insert_latest_config_value(&mut latest, format!("red.config/{rest}"), id, value);
            }
            true
        });
    }

    if let Some(manager) = db.store().get_collection("red.config") {
        manager.for_each_entity(|entity| {
            let Some(row) = entity.data.as_row() else {
                return true;
            };
            if matches!(row.get_field("tombstone"), Some(Value::Boolean(true))) {
                return true;
            }
            let Some(Value::Text(key)) = row.get_field("key") else {
                return true;
            };
            let value = row.get_field("value").cloned().unwrap_or(Value::Null);
            insert_latest_config_value(
                &mut latest,
                format!("red.config/{}", key.to_ascii_lowercase()),
                entity.id.raw(),
                value,
            );
            true
        });
    }

    latest
        .into_iter()
        .map(|(key, (_, value))| (key, value))
        .collect()
}

fn insert_latest_config_value(
    latest: &mut HashMap<String, (u64, Value)>,
    key: String,
    id: u64,
    value: Value,
) {
    match latest.get(&key) {
        Some((prev_id, _)) if *prev_id > id => {}
        _ => {
            latest.insert(key, (id, value));
        }
    }
}

struct ConfigResolver {
    db: Arc<RedDB>,
    values: Option<HashMap<String, Value>>,
}

pub(super) struct ConfigSnapshotGuard {
    previous: Option<ConfigResolver>,
}

impl ConfigSnapshotGuard {
    pub(super) fn install(db: Arc<RedDB>) -> Self {
        let previous = CURRENT_CONFIG_RESOLVER
            .with(|cell| cell.replace(Some(ConfigResolver { db, values: None })));
        Self { previous }
    }
}

impl Drop for ConfigSnapshotGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT_CONFIG_RESOLVER.with(|cell| {
            cell.replace(previous);
        });
    }
}

/// Install the MVCC snapshot used by the current thread for the duration
/// of one statement. Paired with `clear_current_snapshot()` — callers
/// should prefer the `CurrentSnapshotGuard` RAII wrapper so early returns
/// still clean up.
pub fn set_current_snapshot(ctx: SnapshotContext) {
    CURRENT_SNAPSHOT.with(|cell| *cell.borrow_mut() = Some(ctx));
    HAS_SNAPSHOT.with(|c| c.set(true));
}

pub fn clear_current_snapshot() {
    CURRENT_SNAPSHOT.with(|cell| *cell.borrow_mut() = None);
    HAS_SNAPSHOT.with(|c| c.set(false));
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
        let prev = self.previous.take();
        let has = prev.is_some();
        CURRENT_SNAPSHOT.with(|cell| *cell.borrow_mut() = prev);
        HAS_SNAPSHOT.with(|c| c.set(has));
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
    // Fast path — one `Cell<bool>` read, no RefCell borrow. Autocommit
    // reads (no active MVCC transaction) still hide superseded physical
    // versions while avoiding a full snapshot-context lookup.
    // This runs on every row of every scan; the slow path only fires
    // inside an explicit transaction.
    if !HAS_SNAPSHOT.with(|c| c.get()) {
        return entity.xmax == 0;
    }
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
    if !HAS_SNAPSHOT.with(|c| c.get()) {
        return true;
    }
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

/// Whether the active read snapshot may need historical tuple versions
/// that the current secondary indexes cannot prove. Index paths can still
/// recheck visible candidates, but only a heap scan can discover versions
/// whose indexed value was changed or deleted after this snapshot.
pub(crate) fn current_snapshot_requires_index_fallback() -> bool {
    if !HAS_SNAPSHOT.with(|c| c.get()) {
        return false;
    }
    CURRENT_SNAPSHOT.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|ctx| ctx.requires_index_fallback)
    })
}

/// Frozen MVCC + identity context for callers that need to reinstall
/// the same view across thread-local boundaries — long-lived cursors,
/// background batchers, anything that detaches from the dispatch path
/// and re-enters later.
///
/// The bundle bakes in the three thread-locals every read path
/// consults: `SnapshotContext` (MVCC visibility), the auth identity
/// (RLS policy gate), and the tenant id (RLS scope). A FETCH that
/// reinstalls the bundle sees exactly the same rows as the DECLARE
/// would have, regardless of writes that landed in between.
///
/// Cheap to clone — `SnapshotContext` is a clone of three
/// `Arc`-backed fields, identity is a `(String, Role)`, tenant is a
/// `String`. None of these contend with the read path.
#[derive(Clone, Default)]
pub struct SnapshotBundle {
    pub snapshot: Option<SnapshotContext>,
    pub auth: Option<(String, crate::auth::Role)>,
    pub tenant: Option<String>,
}

/// Capture the three read-path thread-locals into a `SnapshotBundle`.
/// Pairs with `with_snapshot_bundle` for re-entry.
pub fn snapshot_bundle() -> SnapshotBundle {
    SnapshotBundle {
        snapshot: capture_current_snapshot(),
        auth: current_auth_identity(),
        tenant: CURRENT_TENANT_ID.with(|cell| cell.borrow().clone()),
    }
}

/// Reinstall a captured `SnapshotBundle` for the duration of `f`.
/// Restores the caller's previous thread-locals on exit (panic-safe via
/// the explicit guard struct so a panic in `f` cannot leak the
/// installed identity into the worker's next request).
pub fn with_snapshot_bundle<R>(bundle: &SnapshotBundle, f: impl FnOnce() -> R) -> R {
    struct Guard {
        prev_snapshot: Option<SnapshotContext>,
        prev_auth: Option<(String, crate::auth::Role)>,
        prev_tenant: Option<String>,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            let snap = self.prev_snapshot.take();
            let has = snap.is_some();
            CURRENT_SNAPSHOT.with(|cell| *cell.borrow_mut() = snap);
            HAS_SNAPSHOT.with(|c| c.set(has));
            CURRENT_AUTH_IDENTITY.with(|cell| *cell.borrow_mut() = self.prev_auth.take());
            CURRENT_TENANT_ID.with(|cell| *cell.borrow_mut() = self.prev_tenant.take());
        }
    }

    let _guard = {
        let prev_snapshot = CURRENT_SNAPSHOT.with(|cell| cell.borrow().clone());
        let prev_auth = CURRENT_AUTH_IDENTITY.with(|cell| cell.borrow().clone());
        let prev_tenant = CURRENT_TENANT_ID.with(|cell| cell.borrow().clone());

        match bundle.snapshot.clone() {
            Some(ctx) => set_current_snapshot(ctx),
            None => clear_current_snapshot(),
        }
        CURRENT_AUTH_IDENTITY.with(|cell| *cell.borrow_mut() = bundle.auth.clone());
        CURRENT_TENANT_ID.with(|cell| *cell.borrow_mut() = bundle.tenant.clone());

        Guard {
            prev_snapshot,
            prev_auth,
            prev_tenant,
        }
    };
    f()
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

fn table_row_index_fields(
    entity: &crate::storage::unified::entity::UnifiedEntity,
) -> Vec<(String, crate::storage::schema::Value)> {
    let crate::storage::EntityData::Row(row) = &entity.data else {
        return Vec::new();
    };
    if let Some(named) = &row.named {
        return named
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
    }
    if let Some(schema) = &row.schema {
        return schema
            .iter()
            .zip(row.columns.iter())
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
    }
    Vec::new()
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
        QueryExpr::CreateCollection(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::CreateVector(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropTable(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropGraph(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropVector(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropDocument(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropKv(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropCollection(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::Truncate(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::AlterTable(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::CreateIndex(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::DropIndex(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::CreateTimeSeries(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropTimeSeries(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::CreateQueue(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::AlterQueue(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropQueue(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::QueueSelect(query) => cache_scope_insert(scopes, &query.queue),
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
            QueueCommand::Move {
                source,
                destination,
                ..
            } => {
                cache_scope_insert(scopes, source);
                cache_scope_insert(scopes, destination);
            }
        },
        QueryExpr::EventsBackfill(query) => {
            cache_scope_insert(scopes, &query.collection);
            cache_scope_insert(scopes, &query.target_queue);
        }
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
        | QueryExpr::SetSecret { .. }
        | QueryExpr::DeleteSecret { .. }
        | QueryExpr::ShowSecrets { .. }
        | QueryExpr::SetTenant(_)
        | QueryExpr::ShowTenant
        | QueryExpr::TransactionControl(_)
        | QueryExpr::CreateSchema(_)
        | QueryExpr::DropSchema(_)
        | QueryExpr::CreateSequence(_)
        | QueryExpr::DropSequence(_)
        | QueryExpr::Grant(_)
        | QueryExpr::Revoke(_)
        | QueryExpr::AlterUser(_)
        | QueryExpr::CreateIamPolicy { .. }
        | QueryExpr::DropIamPolicy { .. }
        | QueryExpr::AttachPolicy { .. }
        | QueryExpr::DetachPolicy { .. }
        | QueryExpr::ShowPolicies { .. }
        | QueryExpr::ShowEffectivePermissions { .. }
        | QueryExpr::SimulatePolicy { .. }
        | QueryExpr::CreateMigration(_)
        | QueryExpr::ApplyMigration(_)
        | QueryExpr::RollbackMigration(_)
        | QueryExpr::ExplainMigration(_)
        | QueryExpr::EventsBackfillStatus { .. } => {}
        QueryExpr::KvCommand(cmd) => {
            use crate::storage::query::ast::KvCommand;
            match cmd {
                KvCommand::Put { collection, .. }
                | KvCommand::InvalidateTags { collection, .. }
                | KvCommand::Get { collection, .. }
                | KvCommand::Unseal { collection, .. }
                | KvCommand::Rotate { collection, .. }
                | KvCommand::History { collection, .. }
                | KvCommand::List { collection, .. }
                | KvCommand::Purge { collection, .. }
                | KvCommand::Watch { collection, .. }
                | KvCommand::Delete { collection, .. }
                | KvCommand::Incr { collection, .. }
                | KvCommand::Cas { collection, .. } => cache_scope_insert(scopes, collection),
            }
        }
        QueryExpr::ConfigCommand(cmd) => {
            use crate::storage::query::ast::ConfigCommand;
            match cmd {
                ConfigCommand::Put { collection, .. }
                | ConfigCommand::Get { collection, .. }
                | ConfigCommand::Resolve { collection, .. }
                | ConfigCommand::Rotate { collection, .. }
                | ConfigCommand::Delete { collection, .. }
                | ConfigCommand::History { collection, .. }
                | ConfigCommand::List { collection, .. }
                | ConfigCommand::Watch { collection, .. }
                | ConfigCommand::InvalidVolatileOperation { collection, .. } => {
                    cache_scope_insert(scopes, collection)
                }
            }
        }
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

const RESULT_CACHE_BACKEND_KEY: &str = "runtime.result_cache.backend";
const RESULT_CACHE_DEFAULT_BACKEND: &str = "legacy";
const RESULT_CACHE_BLOB_NAMESPACE: &str = "runtime.result_cache";
const RESULT_CACHE_TTL_SECS: u64 = 30;
const RESULT_CACHE_MAX_ENTRIES: usize = 1000;
const RESULT_CACHE_PAYLOAD_MAGIC: &[u8; 8] = b"RDRC0001";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeResultCacheBackend {
    Legacy,
    BlobCache,
    Shadow,
}

fn trim_result_cache(
    map: &mut HashMap<String, RuntimeResultCacheEntry>,
    order: &mut std::collections::VecDeque<String>,
) {
    while map.len() > RESULT_CACHE_MAX_ENTRIES {
        if let Some(oldest) = order.pop_front() {
            map.remove(&oldest);
        } else {
            break;
        }
    }
}

fn result_cache_fingerprint(result: &RuntimeQueryResult) -> String {
    format!(
        "{:?}|{}|{}|{}|{}|{:?}",
        result.result,
        result.query,
        result.statement,
        result.engine,
        result.affected_rows,
        result.statement_type
    )
}

fn mode_to_byte(mode: crate::storage::query::modes::QueryMode) -> u8 {
    match mode {
        crate::storage::query::modes::QueryMode::Sql => 0,
        crate::storage::query::modes::QueryMode::Gremlin => 1,
        crate::storage::query::modes::QueryMode::Cypher => 2,
        crate::storage::query::modes::QueryMode::Sparql => 3,
        crate::storage::query::modes::QueryMode::Path => 4,
        crate::storage::query::modes::QueryMode::Natural => 5,
        crate::storage::query::modes::QueryMode::Unknown => 255,
    }
}

fn mode_from_byte(byte: u8) -> Option<crate::storage::query::modes::QueryMode> {
    match byte {
        0 => Some(crate::storage::query::modes::QueryMode::Sql),
        1 => Some(crate::storage::query::modes::QueryMode::Gremlin),
        2 => Some(crate::storage::query::modes::QueryMode::Cypher),
        3 => Some(crate::storage::query::modes::QueryMode::Sparql),
        4 => Some(crate::storage::query::modes::QueryMode::Path),
        5 => Some(crate::storage::query::modes::QueryMode::Natural),
        255 => Some(crate::storage::query::modes::QueryMode::Unknown),
        _ => None,
    }
}

fn result_cache_static_str(value: &str) -> Option<&'static str> {
    match value {
        "select" => Some("select"),
        "materialized-graph" => Some("materialized-graph"),
        "runtime-red-schema" => Some("runtime-red-schema"),
        "runtime-fdw" => Some("runtime-fdw"),
        "runtime-table-rls" => Some("runtime-table-rls"),
        "runtime-table" => Some("runtime-table"),
        "runtime-join-rls" => Some("runtime-join-rls"),
        "runtime-join" => Some("runtime-join"),
        "runtime-vector" => Some("runtime-vector"),
        "runtime-hybrid" => Some("runtime-hybrid"),
        "runtime-secret" => Some("runtime-secret"),
        "runtime-config" => Some("runtime-config"),
        "runtime-tenant" => Some("runtime-tenant"),
        "runtime-explain" => Some("runtime-explain"),
        "runtime-tree" => Some("runtime-tree"),
        "runtime-kv" => Some("runtime-kv"),
        "runtime-queue" => Some("runtime-queue"),
        _ => None,
    }
}

fn write_u32(out: &mut Vec<u8>, value: usize) -> Option<()> {
    let value = u32::try_from(value).ok()?;
    out.extend_from_slice(&value.to_le_bytes());
    Some(())
}

fn write_string(out: &mut Vec<u8>, value: &str) -> Option<()> {
    write_u32(out, value.len())?;
    out.extend_from_slice(value.as_bytes());
    Some(())
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) -> Option<()> {
    write_u32(out, value.len())?;
    out.extend_from_slice(value);
    Some(())
}

fn read_u8(input: &mut &[u8]) -> Option<u8> {
    let (&value, rest) = input.split_first()?;
    *input = rest;
    Some(value)
}

fn read_u32(input: &mut &[u8]) -> Option<usize> {
    if input.len() < 4 {
        return None;
    }
    let value = u32::from_le_bytes(input[..4].try_into().ok()?) as usize;
    *input = &input[4..];
    Some(value)
}

fn read_u64(input: &mut &[u8]) -> Option<u64> {
    if input.len() < 8 {
        return None;
    }
    let value = u64::from_le_bytes(input[..8].try_into().ok()?);
    *input = &input[8..];
    Some(value)
}

fn read_string(input: &mut &[u8]) -> Option<String> {
    let len = read_u32(input)?;
    if input.len() < len {
        return None;
    }
    let value = String::from_utf8(input[..len].to_vec()).ok()?;
    *input = &input[len..];
    Some(value)
}

fn read_bytes<'a>(input: &mut &'a [u8]) -> Option<&'a [u8]> {
    let len = read_u32(input)?;
    if input.len() < len {
        return None;
    }
    let value = &input[..len];
    *input = &input[len..];
    Some(value)
}

fn encode_result_cache_payload(entry: &RuntimeResultCacheEntry) -> Option<Vec<u8>> {
    let result = &entry.result;
    if result.result.pre_serialized_json.is_some()
        || result_cache_static_str(result.statement).is_none()
        || result_cache_static_str(result.engine).is_none()
        || result_cache_static_str(result.statement_type).is_none()
        || result.result.records.iter().any(|record| {
            !record.nodes.is_empty()
                || !record.edges.is_empty()
                || !record.paths.is_empty()
                || !record.vector_results.is_empty()
        })
    {
        return None;
    }

    let mut out = Vec::new();
    out.extend_from_slice(RESULT_CACHE_PAYLOAD_MAGIC);
    write_string(&mut out, &result.query)?;
    out.push(mode_to_byte(result.mode));
    write_string(&mut out, result.statement)?;
    write_string(&mut out, result.engine)?;
    out.extend_from_slice(&result.affected_rows.to_le_bytes());
    write_string(&mut out, result.statement_type)?;

    write_u32(&mut out, result.result.columns.len())?;
    for column in &result.result.columns {
        write_string(&mut out, column)?;
    }
    out.extend_from_slice(&result.result.stats.nodes_scanned.to_le_bytes());
    out.extend_from_slice(&result.result.stats.edges_scanned.to_le_bytes());
    out.extend_from_slice(&result.result.stats.rows_scanned.to_le_bytes());
    out.extend_from_slice(&result.result.stats.exec_time_us.to_le_bytes());

    write_u32(&mut out, result.result.records.len())?;
    for record in &result.result.records {
        let fields = record.iter_fields().collect::<Vec<_>>();
        write_u32(&mut out, fields.len())?;
        for (name, value) in fields {
            write_string(&mut out, name)?;
            let mut encoded = Vec::new();
            crate::storage::schema::value_codec::encode(value, &mut encoded);
            write_bytes(&mut out, &encoded)?;
        }
    }

    write_u32(&mut out, entry.scopes.len())?;
    for scope in &entry.scopes {
        write_string(&mut out, scope)?;
    }
    Some(out)
}

fn decode_result_cache_payload(mut input: &[u8]) -> Option<(RuntimeQueryResult, HashSet<String>)> {
    if input.len() < RESULT_CACHE_PAYLOAD_MAGIC.len()
        || &input[..RESULT_CACHE_PAYLOAD_MAGIC.len()] != RESULT_CACHE_PAYLOAD_MAGIC
    {
        return None;
    }
    input = &input[RESULT_CACHE_PAYLOAD_MAGIC.len()..];

    let query = read_string(&mut input)?;
    let mode = mode_from_byte(read_u8(&mut input)?)?;
    let statement = result_cache_static_str(&read_string(&mut input)?)?;
    let engine = result_cache_static_str(&read_string(&mut input)?)?;
    let affected_rows = read_u64(&mut input)?;
    let statement_type = result_cache_static_str(&read_string(&mut input)?)?;

    let mut columns = Vec::new();
    for _ in 0..read_u32(&mut input)? {
        columns.push(read_string(&mut input)?);
    }
    let stats = crate::storage::query::unified::QueryStats {
        nodes_scanned: read_u64(&mut input)?,
        edges_scanned: read_u64(&mut input)?,
        rows_scanned: read_u64(&mut input)?,
        exec_time_us: read_u64(&mut input)?,
    };

    let mut records = Vec::new();
    for _ in 0..read_u32(&mut input)? {
        let mut record = crate::storage::query::unified::UnifiedRecord::new();
        for _ in 0..read_u32(&mut input)? {
            let name = read_string(&mut input)?;
            let bytes = read_bytes(&mut input)?;
            let (value, used) = crate::storage::schema::value_codec::decode(bytes).ok()?;
            if used != bytes.len() {
                return None;
            }
            record.set_owned(name, value);
        }
        records.push(record);
    }

    let mut scopes = HashSet::new();
    for _ in 0..read_u32(&mut input)? {
        scopes.insert(read_string(&mut input)?);
    }
    if !input.is_empty() {
        return None;
    }

    Some((
        RuntimeQueryResult {
            query,
            mode,
            statement,
            engine,
            result: crate::storage::query::unified::UnifiedResult {
                columns,
                records,
                stats,
                pre_serialized_json: None,
            },
            affected_rows,
            statement_type,
        },
        scopes,
    ))
}

/// Heuristic: does the raw SQL reference a built-in whose output
/// varies by connection, clock, or randomness? Such queries must
/// skip the 30s result cache — see the call site for rationale.
///
/// ASCII case-insensitive substring match. False positives (the
/// token appears in a quoted string) only skip caching, which is
/// the conservative direction.
/// If `sql` starts with `EXPLAIN` followed by a non-`ALTER` token,
/// return the trimmed inner statement; otherwise `None`.
///
/// `EXPLAIN ALTER FOR CREATE TABLE ...` is a separate schema-diff
/// command handled inside the normal SQL parser, so we leave it
/// alone here.
fn strip_explain_prefix(sql: &str) -> Option<&str> {
    let trimmed = sql.trim_start();
    let (head, rest) = trimmed.split_at(
        trimmed
            .find(|c: char| c.is_whitespace())
            .unwrap_or(trimmed.len()),
    );
    if !head.eq_ignore_ascii_case("EXPLAIN") {
        return None;
    }
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }
    // Peek the next token — if ALTER or ASK, defer to the normal parser.
    // `EXPLAIN ASK` is an executable read path: it runs retrieval and
    // provider selection, then short-circuits before the LLM call.
    let next_head_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    if rest[..next_head_end].eq_ignore_ascii_case("ALTER")
        || rest[..next_head_end].eq_ignore_ascii_case("ASK")
    {
        return None;
    }
    Some(rest)
}

/// Cheap prefix check for a leading `WITH` keyword. Used to gate the
/// CTE-aware parse in `execute_query` without paying for a full
/// lexer pass on every statement. Treats `WITHIN` as not-a-CTE so
/// `WITHIN TENANT '...' SELECT ...` doesn't mis-route.
pub(super) fn has_with_prefix(sql: &str) -> bool {
    let trimmed = sql.trim_start();
    let head_end = trimmed
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(trimmed.len());
    trimmed[..head_end].eq_ignore_ascii_case("WITH")
}

/// If the query is a plain SELECT whose top-level `TableQuery`
/// carries an `AS OF` clause, return a typed spec that the runtime
/// can feed to `vcs_resolve_as_of`. Returns `None` for any other
/// shape — joins, DML, EXPLAIN, or parse failures — so callers fall
/// back to the connection's regular MVCC snapshot. A cheap textual
/// prefilter skips the parse entirely when the source doesn't
/// mention `AS OF` / `as of`, keeping the autocommit hot path free.
fn peek_top_level_as_of(sql: &str) -> Option<crate::application::vcs::AsOfSpec> {
    peek_top_level_as_of_with_table(sql).map(|(spec, _)| spec)
}

/// Same as `peek_top_level_as_of` but also returns the table name
/// targeted by the AS OF clause (when the FROM clause names a
/// concrete table). `None` for the table slot means scalar SELECT
/// or a subquery source — callers treat those as "no enforcement".
pub(super) fn peek_top_level_as_of_with_table(
    sql: &str,
) -> Option<(crate::application::vcs::AsOfSpec, Option<String>)> {
    if !sql
        .as_bytes()
        .windows(5)
        .any(|w| w.eq_ignore_ascii_case(b"as of"))
    {
        return None;
    }
    let parsed = crate::storage::query::parser::parse(sql).ok()?;
    let crate::storage::query::ast::QueryExpr::Table(table) = parsed.query else {
        return None;
    };
    let clause = table.as_of?;
    let table_name = if table.table.is_empty() || table.table == "any" {
        None
    } else {
        Some(table.table.clone())
    };
    let spec = match clause {
        crate::storage::query::ast::AsOfClause::Commit(h) => {
            crate::application::vcs::AsOfSpec::Commit(h)
        }
        crate::storage::query::ast::AsOfClause::Branch(b) => {
            crate::application::vcs::AsOfSpec::Branch(b)
        }
        crate::storage::query::ast::AsOfClause::Tag(t) => crate::application::vcs::AsOfSpec::Tag(t),
        crate::storage::query::ast::AsOfClause::TimestampMs(ts) => {
            crate::application::vcs::AsOfSpec::TimestampMs(ts)
        }
        crate::storage::query::ast::AsOfClause::Snapshot(x) => {
            crate::application::vcs::AsOfSpec::Snapshot(x)
        }
    };
    Some((spec, table_name))
}

pub(super) fn query_has_volatile_builtin(sql: &str) -> bool {
    // Lowercase the bytes up to the first null/newline into a small
    // stack buffer for cheap contains() checks. Most SQL fits in the
    // buffer; longer queries fall back to owned lowercase.
    const VOLATILE_TOKENS: &[&str] = &[
        "pg_advisory_lock",
        "pg_try_advisory_lock",
        "pg_advisory_unlock",
        "random()",
        // NOW() / CURRENT_TIMESTAMP / CURRENT_DATE intentionally
        // omitted for now — they ARE volatile but today's tests rely
        // on caching them. Revisit once a tighter volatility story
        // lands.
    ];
    let lowered = sql.to_ascii_lowercase();
    VOLATILE_TOKENS.iter().any(|t| lowered.contains(t))
}

pub(super) fn query_is_ask_statement(sql: &str) -> bool {
    let trimmed = sql.trim_start();
    let head_end = trimmed
        .find(|c: char| c.is_whitespace() || c == '(' || c == ';')
        .unwrap_or(trimmed.len());
    trimmed[..head_end].eq_ignore_ascii_case("ASK")
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
pub(super) fn intent_lock_modes_for(
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
        | QueryExpr::QueueSelect(_) => Some((IntentShared, IntentShared)),

        // Writes — IX / IX. Non-tabular mutations (vector insert,
        // graph node insert, queue push, timeseries point insert)
        // don't carry their own dispatch arm here; they ride through
        // the Insert variant or a command variant covered by the
        // read-side arm above. P1.T4 expands only the TableQuery-ish
        // writes; non-tabular kinds inherit when their DML variants
        // land in later phases.
        QueryExpr::Insert(_)
        | QueryExpr::Update(_)
        | QueryExpr::Delete(_)
        | QueryExpr::QueueCommand(QueueCommand::Move { .. }) => {
            Some((IntentExclusive, IntentExclusive))
        }
        QueryExpr::QueueCommand(_) => Some((IntentShared, IntentShared)),

        // DDL — IX / X. A DDL against collection `c` blocks all
        // other writers + readers on `c` but leaves other collections
        // running (because Global stays IX, not X).
        QueryExpr::CreateTable(_)
        | QueryExpr::CreateCollection(_)
        | QueryExpr::CreateVector(_)
        | QueryExpr::DropTable(_)
        | QueryExpr::DropGraph(_)
        | QueryExpr::DropVector(_)
        | QueryExpr::DropDocument(_)
        | QueryExpr::DropKv(_)
        | QueryExpr::DropCollection(_)
        | QueryExpr::Truncate(_)
        | QueryExpr::AlterTable(_)
        | QueryExpr::CreateIndex(_)
        | QueryExpr::DropIndex(_)
        | QueryExpr::CreateTimeSeries(_)
        | QueryExpr::DropTimeSeries(_)
        | QueryExpr::CreateQueue(_)
        | QueryExpr::AlterQueue(_)
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
pub(super) fn collections_referenced(expr: &QueryExpr) -> Vec<String> {
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
        QueryExpr::QueueSelect(q) => out.push(q.queue.clone()),

        // DDL — include the target collection so DDL takes
        // `(Collection, X)` and blocks concurrent readers / writers
        // on the same collection. Other collections stay live
        // because Global is still IX.
        QueryExpr::CreateTable(q) => out.push(q.name.clone()),
        QueryExpr::CreateCollection(q) => out.push(q.name.clone()),
        QueryExpr::CreateVector(q) => out.push(q.name.clone()),
        QueryExpr::DropTable(q) => out.push(q.name.clone()),
        QueryExpr::DropGraph(q) => out.push(q.name.clone()),
        QueryExpr::DropVector(q) => out.push(q.name.clone()),
        QueryExpr::DropDocument(q) => out.push(q.name.clone()),
        QueryExpr::DropKv(q) => out.push(q.name.clone()),
        QueryExpr::DropCollection(q) => out.push(q.name.clone()),
        QueryExpr::Truncate(q) => out.push(q.name.clone()),
        QueryExpr::AlterTable(q) => out.push(q.name.clone()),
        QueryExpr::CreateIndex(q) => out.push(q.table.clone()),
        QueryExpr::DropIndex(q) => out.push(q.table.clone()),
        QueryExpr::CreateTimeSeries(q) => out.push(q.name.clone()),
        QueryExpr::DropTimeSeries(q) => out.push(q.name.clone()),
        QueryExpr::CreateQueue(q) => out.push(q.name.clone()),
        QueryExpr::AlterQueue(q) => out.push(q.name.clone()),
        QueryExpr::DropQueue(q) => out.push(q.name.clone()),
        QueryExpr::QueueCommand(QueueCommand::Move {
            source,
            destination,
            ..
        }) => {
            out.push(source.clone());
            out.push(destination.clone());
        }
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
        // PLAN.md Phase 9.1 — capture wall-clock before storage
        // open so the cold-start phase markers can be backfilled
        // once Lifecycle is constructed below. Storage open
        // encapsulates auto-restore + WAL replay; we treat the
        // whole window as one combined "restore" + "wal_replay"
        // phase split at the same boundary because the storage
        // layer doesn't yet emit a finer signal.
        let boot_open_start_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let db = Arc::new(
            RedDB::open_with_options(&options)
                .map_err(|err| RedDBError::Internal(err.to_string()))?,
        );
        let result_blob_cache = crate::storage::cache::BlobCache::open_with_l2(
            crate::storage::cache::BlobCacheConfig::default().with_l2_path(
                options
                    .resolved_path("data.rdb")
                    .with_extension("result-cache.l2"),
            ),
        )
        .map_err(|err| {
            RedDBError::Internal(format!("open result Blob Cache L2 failed: {err:?}"))
        })?;
        let storage_ready_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

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
                result_blob_cache,
                result_blob_entries: parking_lot::RwLock::new((
                    HashMap::new(),
                    std::collections::VecDeque::new(),
                )),
                ask_answer_cache_entries: parking_lot::RwLock::new((
                    HashSet::new(),
                    std::collections::VecDeque::new(),
                )),
                result_cache_shadow_divergences: std::sync::atomic::AtomicU64::new(0),
                ask_daily_spend: parking_lot::RwLock::new(HashMap::new()),
                queue_message_locks: parking_lot::RwLock::new(HashMap::new()),
                planner_dirty_tables: parking_lot::RwLock::new(HashSet::new()),
                ec_registry: Arc::new(crate::ec::config::EcRegistry::new()),
                ec_worker: crate::ec::worker::EcWorker::new(),
                auth_store: parking_lot::RwLock::new(None),
                oauth_validator: parking_lot::RwLock::new(None),
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
                pending_versioned_updates: parking_lot::RwLock::new(HashMap::new()),
                pending_kv_watch_events: parking_lot::RwLock::new(HashMap::new()),
                pending_store_wal_actions: parking_lot::RwLock::new(HashMap::new()),
                tenant_tables: parking_lot::RwLock::new(HashMap::new()),
                ddl_epoch: std::sync::atomic::AtomicU64::new(0),
                write_gate: Arc::new(crate::runtime::write_gate::WriteGate::from_options(
                    &options,
                )),
                lifecycle: crate::runtime::lifecycle::Lifecycle::new(),
                resource_limits: crate::runtime::resource_limits::ResourceLimits::from_env(),
                audit_log: {
                    // Default audit-log path for the in-memory case
                    // sits in the system temp dir; persistent runs
                    // place it next to data.rdb.
                    let data_path = options
                        .data_path
                        .clone()
                        .unwrap_or_else(|| std::env::temp_dir().join("reddb"));
                    Arc::new(crate::runtime::audit_log::AuditLogger::for_data_path(
                        &data_path,
                    ))
                },
                lease_lifecycle: std::sync::OnceLock::new(),
                replica_apply_metrics: crate::replication::logical::ReplicaApplyMetrics::default(),
                quota_bucket: crate::runtime::quota_bucket::QuotaBucket::from_env(),
                schema_vocabulary: parking_lot::RwLock::new(
                    crate::runtime::schema_vocabulary::SchemaVocabulary::new(),
                ),
                slow_query_logger: {
                    // Issue #205 — slow-query sink lives in the same
                    // directory the audit log uses, so backup/restore
                    // ships them together. Threshold + sample-pct
                    // default conservatively (1 s, 100% sampling) so
                    // emitted lines are rare and complete. Operators
                    // tune via env / config matrix in a follow-up.
                    //
                    // `data_path` points at the primary `.rdb` *file*
                    // (mirrors AuditLogger::for_data_path), so we
                    // anchor the slow log at its parent directory.
                    let log_dir = options
                        .data_path
                        .as_ref()
                        .and_then(|p| p.parent().map(std::path::PathBuf::from))
                        .unwrap_or_else(|| std::env::temp_dir().join("reddb"));
                    let threshold_ms = std::env::var("RED_SLOW_QUERY_THRESHOLD_MS")
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(1000);
                    let sample_pct = std::env::var("RED_SLOW_QUERY_SAMPLE_PCT")
                        .ok()
                        .and_then(|s| s.parse::<u8>().ok())
                        .unwrap_or(100);
                    crate::telemetry::slow_query_logger::SlowQueryLogger::new(
                        crate::telemetry::slow_query_logger::SlowQueryOpts {
                            log_dir,
                            threshold_ms,
                            sample_pct,
                        },
                    )
                },
                kv_stats: crate::runtime::KvStatsCounters::default(),
                kv_tag_index: crate::runtime::KvTagIndex::default(),
            }),
        };

        // Issue #205 — install the process-wide OperatorEvent sink so
        // emit sites buried in storage / replication / signal handlers
        // can record without threading an `&AuditLogger` through every
        // call stack. First registration wins; subsequent in-memory
        // runtimes (test harnesses) fall through to tracing+eprintln.
        crate::telemetry::operator_event::install_global_audit_sink(Arc::clone(
            &runtime.inner.audit_log,
        ));

        // PLAN.md Phase 9.1 — backfill cold-start phase markers
        // from the wall-clock captured before storage open. The
        // entire `RedDB::open_with_options` call covers both
        // auto-restore (when configured) and WAL replay. We
        // record both phases against the same boundary today;
        // a follow-up will split them once the storage layer
        // surfaces a finer-grained event.
        runtime
            .inner
            .lifecycle
            .set_restore_started_at_ms(boot_open_start_ms);
        runtime
            .inner
            .lifecycle
            .set_restore_ready_at_ms(storage_ready_ms);
        runtime
            .inner
            .lifecycle
            .set_wal_replay_started_at_ms(boot_open_start_ms);
        runtime
            .inner
            .lifecycle
            .set_wal_replay_ready_at_ms(storage_ready_ms);

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
        runtime.rehydrate_snapshot_xid_floor();
        runtime.bootstrap_system_keyed_collections()?;

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

        // VCS ("Git for Data") — create the `red_*` metadata
        // collections on first boot. Idempotent: `get_or_create_collection`
        // is a no-op if the collection already exists.
        {
            let store = runtime.inner.db.store();
            for name in crate::application::vcs_collections::ALL {
                let _ = store.get_or_create_collection(*name);
            }
            // Seed VCS config namespace with sensible defaults on first
            // boot, matching the pattern used by red.ai / red.storage.
            store.set_config_tree(
                crate::application::vcs_collections::CONFIG_NAMESPACE,
                &crate::json!({
                    "default_branch": "main",
                    "author": crate::json!({
                        "name": "reddb",
                        "email": "reddb@localhost"
                    }),
                    "protected_branches": crate::json!(["main"]),
                    "closure": crate::json!({
                        "enabled": true,
                        "lazy": true
                    }),
                    "merge": crate::json!({
                        "default_strategy": "auto",
                        "fast_forward": true
                    })
                }),
            );
        }

        // Migrations — create the `red_migrations` / `red_migration_deps`
        // system collections on first boot. Idempotent.
        {
            let store = runtime.inner.db.store();
            for name in crate::application::migration_collections::ALL {
                let _ = store.get_or_create_collection(*name);
            }
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
                            crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                            _ => None,
                        });
                        let val = row.get_field("value");
                        if key == Some("red.config.backup.enabled") {
                            backup_enabled = match val {
                                Some(crate::storage::schema::Value::Boolean(true)) => true,
                                Some(crate::storage::schema::Value::Text(s)) => &**s == "true",
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

        // PLAN.md Phase 1 — Lifecycle Contract. Mark Ready once every
        // boot stage above has completed (WAL replay, restore-from-
        // remote, replica-loop spawn). Health probes flip from 503 to
        // 200 here; shutdown begins from this state.
        runtime.inner.lifecycle.mark_ready();

        Ok(runtime)
    }

    fn rehydrate_snapshot_xid_floor(&self) {
        let store = self.inner.db.store();
        for collection in store.list_collections() {
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            for entity in manager.query_all(|_| true) {
                self.inner
                    .snapshot_manager
                    .observe_committed_xid(entity.xmin);
                self.inner
                    .snapshot_manager
                    .observe_committed_xid(entity.xmax);
            }
        }
    }

    fn bootstrap_system_keyed_collections(&self) -> RedDBResult<()> {
        let mut changed = false;
        for (name, model) in [
            ("red.config", crate::catalog::CollectionModel::Config),
            ("red.vault", crate::catalog::CollectionModel::Vault),
        ] {
            if self.inner.db.store().get_collection(name).is_none() {
                self.inner.db.store().get_or_create_collection(name);
                changed = true;
            }
            if self.inner.db.collection_contract(name).is_none() {
                self.inner
                    .db
                    .save_collection_contract(system_keyed_collection_contract(name, model))
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
                changed = true;
            }
        }
        if changed {
            self.inner
                .db
                .persist_metadata()
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        Ok(())
    }

    pub fn db(&self) -> Arc<RedDB> {
        Arc::clone(&self.inner.db)
    }

    /// Direct access to the runtime's secondary-index store.
    /// Used by bulk-insert entry points (gRPC binary bulk, HTTP bulk,
    /// wire bulk) that need to push new rows through the per-index
    /// maintenance hook after `store.bulk_insert` returns.
    pub fn index_store_ref(&self) -> &super::index_store::IndexStore {
        &self.inner.index_store
    }

    /// Apply a DDL event to the schema-vocabulary reverse index
    /// (issue #120). Called by DDL execution paths after the catalog
    /// mutation has succeeded so the index never holds entries for
    /// half-applied DDL.
    pub(crate) fn schema_vocabulary_apply(
        &self,
        event: crate::runtime::schema_vocabulary::DdlEvent,
    ) {
        self.inner.schema_vocabulary.write().on_ddl(event);
    }

    /// Lookup `token` in the schema-vocabulary reverse index. Returns
    /// an owned `Vec<VocabHit>` because the underlying read lock
    /// cannot be borrowed across the call boundary; the slice from
    /// `SchemaVocabulary::lookup` is cloned per hit.
    pub fn schema_vocabulary_lookup(
        &self,
        token: &str,
    ) -> Vec<crate::runtime::schema_vocabulary::VocabHit> {
        self.inner.schema_vocabulary.read().lookup(token).to_vec()
    }

    /// Inject an AuthStore into the runtime. Called by server boot
    /// after the vault has been bootstrapped, so that `Value::Secret`
    /// auto-encrypt/decrypt can reach the vault AES key.
    pub fn set_auth_store(&self, store: Arc<crate::auth::store::AuthStore>) {
        *self.inner.auth_store.write() = Some(store);
    }

    /// Read a vault KV secret from the configured AuthStore, if present.
    pub fn vault_kv_get(&self, key: &str) -> Option<String> {
        self.inner
            .auth_store
            .read()
            .as_ref()
            .and_then(|store| store.vault_kv_get(key))
    }

    /// Write a vault KV secret and fail if the encrypted vault write is
    /// unavailable or cannot be made durable.
    pub fn vault_kv_try_set(&self, key: String, value: String) -> RedDBResult<()> {
        let store = self.inner.auth_store.read().clone().ok_or_else(|| {
            RedDBError::Query("secret storage requires an enabled, unsealed vault".to_string())
        })?;
        store
            .vault_kv_try_set(key, value)
            .map_err(|err| RedDBError::Query(err.to_string()))
    }

    /// Inject an `OAuthValidator` into the runtime. When set, HTTP and
    /// wire transports try OAuth JWT validation before falling back to
    /// the local AuthStore lookup. Pass `None` to disable.
    pub fn set_oauth_validator(&self, validator: Option<Arc<crate::auth::oauth::OAuthValidator>>) {
        *self.inner.oauth_validator.write() = validator;
    }

    /// Returns a clone of the configured `OAuthValidator` Arc, if any.
    /// Hot path: called per HTTP request when an Authorization header
    /// is present, so we hand back a cheap Arc clone.
    pub fn oauth_validator(&self) -> Option<Arc<crate::auth::oauth::OAuthValidator>> {
        self.inner.oauth_validator.read().clone()
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
                    crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        result = match row.get_field("value") {
                            Some(crate::storage::schema::Value::Boolean(b)) => *b,
                            Some(crate::storage::schema::Value::Text(s)) => {
                                matches!(s.as_ref(), "true" | "TRUE" | "True" | "1")
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
                    crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
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

    pub(crate) fn config_f64(&self, key: &str, default: f64) -> f64 {
        if let Some(raw) = self.inner.env_config_overrides.get(key) {
            if let Ok(n) = raw.parse::<f64>() {
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
                    crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        result = match row.get_field("value") {
                            Some(crate::storage::schema::Value::Float(n)) => *n,
                            Some(crate::storage::schema::Value::Integer(n)) => *n as f64,
                            Some(crate::storage::schema::Value::UnsignedInteger(n)) => *n as f64,
                            Some(crate::storage::schema::Value::Text(s)) => {
                                s.parse::<f64>().unwrap_or(default)
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
                    crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        if let Some(crate::storage::schema::Value::Text(value)) =
                            row.get_field("value")
                        {
                            result = value.to_string();
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
            for value in record.values_mut() {
                if let Value::Secret(ref bytes) = value {
                    if let Some(plain) =
                        super::impl_dml::decrypt_secret_payload(&key, bytes.as_slice())
                    {
                        if let Ok(text) = String::from_utf8(plain) {
                            *value = Value::text(text);
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

    /// Public-mutation gate snapshot (PLAN.md W1).
    ///
    /// Surfaces that accept untrusted client requests (SQL DML/DDL,
    /// gRPC mutating RPCs, HTTP/native wire mutations, admin
    /// maintenance, serverless lifecycle) call `check_write` before
    /// dispatching to storage. Returns `RedDBError::ReadOnly` on any
    /// instance running as a replica or with `options.read_only =
    /// true`. The replica internal logical-WAL apply path reaches into
    /// the store directly and never calls this method, so legitimate
    /// replica catch-up still works.
    pub fn check_write(&self, kind: crate::runtime::write_gate::WriteKind) -> RedDBResult<()> {
        self.inner.write_gate.check(kind)
    }

    /// Read-only handle to the gate, useful for transports that want
    /// to surface the policy in health/status output without taking on
    /// a dependency on the concrete enum.
    pub fn write_gate(&self) -> &crate::runtime::write_gate::WriteGate {
        &self.inner.write_gate
    }

    /// Process lifecycle handle (PLAN.md Phase 1). Health probes,
    /// admin/shutdown, and signal handlers consult this single
    /// state machine.
    pub fn lifecycle(&self) -> &crate::runtime::lifecycle::Lifecycle {
        &self.inner.lifecycle
    }

    /// Operator-imposed resource limits (PLAN.md Phase 4.1).
    pub fn resource_limits(&self) -> &crate::runtime::resource_limits::ResourceLimits {
        &self.inner.resource_limits
    }

    /// Append-only audit log for admin mutations (PLAN.md Phase 6.5).
    pub fn audit_log(&self) -> &crate::runtime::audit_log::AuditLogger {
        &self.inner.audit_log
    }

    /// Shared `Arc` to the audit logger — used by collaborators (the
    /// lease lifecycle, future request-context plumbing) that need to
    /// keep the logger alive past the runtime's stack frame.
    pub fn audit_log_arc(&self) -> Arc<crate::runtime::audit_log::AuditLogger> {
        Arc::clone(&self.inner.audit_log)
    }

    /// Shared `Arc` to the write gate. Same rationale as
    /// `audit_log_arc`: collaborators (lease lifecycle, refresh
    /// thread) need a clone-cheap handle they can move into a
    /// background thread.
    pub fn write_gate_arc(&self) -> Arc<crate::runtime::write_gate::WriteGate> {
        Arc::clone(&self.inner.write_gate)
    }

    /// Serverless writer-lease state machine. `None` when the operator
    /// did not opt into lease fencing (`RED_LEASE_REQUIRED` unset).
    pub fn lease_lifecycle(&self) -> Option<&Arc<crate::runtime::lease_lifecycle::LeaseLifecycle>> {
        self.inner.lease_lifecycle.get()
    }

    /// Install the lease lifecycle. Idempotent; subsequent calls
    /// return the previously stored value untouched.
    pub fn set_lease_lifecycle(
        &self,
        lifecycle: Arc<crate::runtime::lease_lifecycle::LeaseLifecycle>,
    ) -> Result<(), Arc<crate::runtime::lease_lifecycle::LeaseLifecycle>> {
        self.inner.lease_lifecycle.set(lifecycle)
    }

    /// Reject the call when the requested batch size exceeds
    /// `RED_MAX_BATCH_SIZE`. Returns `RedDBError::QuotaExceeded`
    /// shaped so the HTTP layer can map it to 413 Payload Too
    /// Large (PLAN.md Phase 4.1).
    pub fn check_batch_size(&self, requested: usize) -> RedDBResult<()> {
        if self.inner.resource_limits.batch_size_exceeded(requested) {
            let max = self.inner.resource_limits.max_batch_size.unwrap_or(0);
            return Err(RedDBError::QuotaExceeded(format!(
                "max_batch_size:{requested}:{max}"
            )));
        }
        Ok(())
    }

    /// Reject the call when the local DB file exceeds
    /// `RED_MAX_DB_SIZE_BYTES`. Reads file metadata once per call —
    /// the cost is a single `stat()` syscall, negligible against the
    /// I/O the caller is about to do. Returns `QuotaExceeded` shaped
    /// for HTTP 507 Insufficient Storage.
    pub fn check_db_size(&self) -> RedDBResult<()> {
        let Some(limit) = self.inner.resource_limits.max_db_size_bytes else {
            return Ok(());
        };
        if limit == 0 {
            return Ok(());
        }
        let Some(path) = self.inner.db.path() else {
            return Ok(());
        };
        let current = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if current > limit {
            return Err(RedDBError::QuotaExceeded(format!(
                "max_db_size_bytes:{current}:{limit}"
            )));
        }
        Ok(())
    }

    /// Graceful shutdown coordinator (PLAN.md Phase 1.1).
    ///
    /// Steps, in order, all idempotent across re-entrant calls:
    ///   1. Move lifecycle into `ShuttingDown` (concurrent callers
    ///      observe `Stopped` after first finishes).
    ///   2. Flush WAL + run final checkpoint via `db.flush()` so
    ///      every acked write is durable on disk.
    ///   3. If `backup_on_shutdown == true` and a remote backend is
    ///      configured, run a synchronous `trigger_backup()` so the
    ///      remote head reflects the final state.
    ///   4. Stamp the report and move to `Stopped`. Subsequent calls
    ///      return the cached report without re-running anything.
    ///
    /// On any error, the runtime is still marked `Stopped` so the
    /// process can exit; the caller logs the error context but does
    /// not retry the same shutdown — the operator can inspect the
    /// report fields to see which step failed.
    pub fn graceful_shutdown(
        &self,
        backup_on_shutdown: bool,
    ) -> RedDBResult<crate::runtime::lifecycle::ShutdownReport> {
        if !self.inner.lifecycle.begin_shutdown() {
            // Someone else already shut down (or is in flight). Return
            // the cached report so the HTTP caller and SIGTERM handler
            // get the same idempotent answer.
            return Ok(self.inner.lifecycle.shutdown_report().unwrap_or_default());
        }

        let started_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut report = crate::runtime::lifecycle::ShutdownReport {
            started_at_ms: started_ms,
            ..Default::default()
        };

        // Flush WAL + run any pending checkpoint. Local fsync is
        // unconditional — even a lease-lost replica needs its WAL on
        // disk before exit so a future restore has the latest tail.
        // The remote upload is gated separately so a lost-lease writer
        // doesn't clobber the new holder's state on its way out.
        let flush_res = self.inner.db.flush_local_only();
        report.flushed_wal = flush_res.is_ok();
        report.final_checkpoint = flush_res.is_ok();
        if let Err(err) = &flush_res {
            tracing::error!(
                target: "reddb::lifecycle",
                error = %err,
                "graceful_shutdown: local flush failed"
            );
        } else if let Err(lease_err) =
            self.assert_remote_write_allowed("shutdown/checkpoint_upload")
        {
            tracing::warn!(
                target: "reddb::serverless::lease",
                error = %lease_err,
                "graceful_shutdown: remote upload skipped — lease not held"
            );
        } else if let Err(err) = self.inner.db.upload_to_remote_backend() {
            tracing::error!(
                target: "reddb::lifecycle",
                error = %err,
                "graceful_shutdown: remote upload failed"
            );
        }

        // Optional final backup. Skipped silently when no remote
        // backend is configured — `trigger_backup()` returns Err
        // anyway in that case, but logging it as a shutdown failure
        // would be misleading on a standalone (no-backend) runtime.
        if backup_on_shutdown && self.inner.db.remote_backend.is_some() {
            // The trigger_backup gate now reads `WriteKind::Backup`,
            // which a replica/read_only instance refuses. That's
            // intentional — replicas don't drive backups; only the
            // primary does. We still want shutdown to flush its WAL
            // even if the backup branch is gated off.
            match self.trigger_backup() {
                Ok(result) => {
                    report.backup_uploaded = result.uploaded;
                }
                Err(err) => {
                    tracing::warn!(
                        target: "reddb::lifecycle",
                        error = %err,
                        "graceful_shutdown: final backup skipped"
                    );
                }
            }
        }

        let completed_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(started_ms);
        report.completed_at_ms = completed_ms;
        report.duration_ms = completed_ms.saturating_sub(started_ms);

        self.inner.lifecycle.finish_shutdown(report.clone());
        Ok(report)
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
    ) -> u64 {
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
        lsn
    }

    pub(crate) fn cdc_emit_insert_batch_no_cache_invalidate(
        &self,
        collection: &str,
        ids: &[EntityId],
        entity_kind: &str,
    ) -> Vec<u64> {
        if ids.is_empty() {
            return Vec::new();
        }

        // Without logical replication, CDC only needs the in-memory event
        // ring. Reserve all LSNs and push the batch under one mutex instead
        // of taking the ring lock once per inserted row.
        if self.inner.db.replication.is_none() {
            return self.inner.cdc.emit_batch_same_collection(
                crate::replication::cdc::ChangeOperation::Insert,
                collection,
                entity_kind,
                ids.iter().map(|id| id.raw()),
            );
        }

        // Replication needs one logical-WAL record per entity with the
        // serialized entity bytes, so keep the existing per-row path.
        ids.iter()
            .map(|id| {
                self.cdc_emit_no_cache_invalidate(
                    crate::replication::cdc::ChangeOperation::Insert,
                    collection,
                    id.raw(),
                    entity_kind,
                )
            })
            .collect()
    }

    pub fn cdc_emit(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        entity_id: u64,
        entity_kind: &str,
    ) -> u64 {
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
        lsn
    }

    pub(crate) fn cdc_emit_kv(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        key: &str,
        entity_id: u64,
        before: Option<crate::json::Value>,
        after: Option<crate::json::Value>,
    ) -> u64 {
        let lsn = self
            .inner
            .cdc
            .emit_kv(operation, collection, key, entity_id, before, after);
        self.inner.kv_stats.incr_watch_events_emitted();
        self.invalidate_result_cache_for_table(collection);
        lsn
    }

    pub(crate) fn record_kv_watch_event(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        key: &str,
        entity_id: u64,
        before: Option<crate::json::Value>,
        after: Option<crate::json::Value>,
    ) {
        if self.current_xid().is_some() {
            let conn_id = current_connection_id();
            let event = crate::replication::cdc::KvWatchEvent {
                collection: collection.to_string(),
                key: key.to_string(),
                op: operation,
                before,
                after,
                lsn: 0,
                committed_at: 0,
                dropped_event_count: 0,
            };
            self.inner
                .pending_kv_watch_events
                .write()
                .entry(conn_id)
                .or_default()
                .push(event);
            return;
        }

        self.cdc_emit_kv(operation, collection, key, entity_id, before, after);
    }

    pub(crate) fn cdc_emit_prebuilt(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        entity: &UnifiedEntity,
        entity_kind: &str,
        metadata: Option<&crate::storage::Metadata>,
        invalidate_cache: bool,
    ) -> u64 {
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
    ) -> u64 {
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

        lsn
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

            // PLAN.md Phase 11.5 — stateful applier guards LSN
            // monotonicity across pulls. Seed with the persisted
            // `last_applied_lsn` so reboots don't lose the chain
            // pointer.
            let applier = crate::replication::logical::LogicalChangeApplier::new(since_lsn);

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
                                    self.inner.replica_apply_metrics.record(
                                        crate::replication::logical::ApplyErrorKind::Decode,
                                    );
                                    self.persist_replication_health(
                                        "apply_error",
                                        "failed to decode WAL record hex payload",
                                        current_lsn,
                                        oldest_available_lsn,
                                    );
                                    continue;
                                };
                                let Ok(change) = ChangeRecord::decode(&data) else {
                                    self.inner.replica_apply_metrics.record(
                                        crate::replication::logical::ApplyErrorKind::Decode,
                                    );
                                    self.persist_replication_health(
                                        "apply_error",
                                        "failed to decode logical WAL record",
                                        current_lsn,
                                        oldest_available_lsn,
                                    );
                                    continue;
                                };
                                match applier.apply(
                                    self.inner.db.as_ref(),
                                    &change,
                                    ApplyMode::Replica,
                                ) {
                                    Ok(crate::replication::logical::ApplyOutcome::Applied) => {
                                        since_lsn = since_lsn.max(change.lsn);
                                        self.persist_replica_lsn(since_lsn);
                                    }
                                    Ok(_) => {
                                        // Idempotent / Skipped: no advance, no error.
                                    }
                                    Err(err) => {
                                        self.inner.replica_apply_metrics.record(err.kind());
                                        // Issue #205 — emit operator-grade event
                                        // for the two replication-fatal kinds. `Gap`
                                        // / `Apply` / `Decode` already persist via
                                        // `persist_replication_health`; the
                                        // OperatorEvent variants only cover the
                                        // two "stream is broken" / "follower
                                        // diverged" conditions an operator must act
                                        // on out-of-band.
                                        match &err {
                                            crate::replication::logical::LogicalApplyError::Divergence { lsn, expected: _, got: _ } => {
                                                crate::telemetry::operator_event::OperatorEvent::Divergence {
                                                    peer: "primary".to_string(),
                                                    leader_lsn: *lsn,
                                                    follower_lsn: since_lsn,
                                                }
                                                .emit_global();
                                            }
                                            crate::replication::logical::LogicalApplyError::Gap { last, next } => {
                                                crate::telemetry::operator_event::OperatorEvent::ReplicationBroken {
                                                    peer: "primary".to_string(),
                                                    reason: format!("stalled gap last={last} next={next}"),
                                                }
                                                .emit_global();
                                            }
                                            _ => {}
                                        }
                                        let kind = match &err {
                                            crate::replication::logical::LogicalApplyError::Gap { .. } => "stalled_gap",
                                            crate::replication::logical::LogicalApplyError::Divergence { .. } => "divergence",
                                            _ => "apply_error",
                                        };
                                        self.persist_replication_health(
                                            kind,
                                            &format!("replica apply rejected: {err}"),
                                            current_lsn,
                                            oldest_available_lsn,
                                        );
                                        // Stop applying this batch. The
                                        // outer loop will retry on next
                                        // pull, which on a real Gap will
                                        // not magically heal — operator
                                        // must rebootstrap. For
                                        // Divergence, we explicitly do
                                        // not advance; this keeps the
                                        // replica visibly unhealthy
                                        // instead of silently swallowing
                                        // corruption.
                                        break;
                                    }
                                }
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

    /// PLAN.md Phase 11.4 — current CDC LSN. Public mutation
    /// surfaces (HTTP query, gRPC entity ops) call this immediately
    /// after a successful write to feed `enforce_commit_policy`.
    pub fn cdc_current_lsn(&self) -> u64 {
        self.inner.cdc.current_lsn()
    }

    pub fn kv_watch_events_since(
        &self,
        collection: &str,
        key: &str,
        since_lsn: u64,
        max_count: usize,
    ) -> Vec<crate::replication::cdc::KvWatchEvent> {
        self.inner
            .cdc
            .poll(since_lsn, max_count)
            .into_iter()
            .filter_map(|event| event.kv)
            .filter(|event| event.collection == collection && event.key == key)
            .collect()
    }

    pub fn kv_watch_events_since_prefix(
        &self,
        collection: &str,
        prefix: &str,
        since_lsn: u64,
        max_count: usize,
    ) -> Vec<crate::replication::cdc::KvWatchEvent> {
        self.inner
            .cdc
            .poll(since_lsn, max_count)
            .into_iter()
            .filter_map(|event| event.kv)
            .filter(|event| event.collection == collection && event.key.starts_with(prefix))
            .collect()
    }

    pub(crate) fn kv_watch_subscribe<'a>(
        &'a self,
        collection: impl Into<String>,
        key: impl Into<String>,
        from_lsn: Option<u64>,
    ) -> crate::runtime::kv_watch::KvWatchStream<'a> {
        crate::runtime::kv_watch::KvWatchStream::subscribe(
            &self.inner.cdc,
            &self.inner.kv_stats,
            collection,
            key,
            from_lsn,
            self.kv_watch_idle_timeout_ms(),
        )
    }

    pub(crate) fn kv_watch_subscribe_prefix<'a>(
        &'a self,
        collection: impl Into<String>,
        prefix: impl Into<String>,
        from_lsn: Option<u64>,
    ) -> crate::runtime::kv_watch::KvWatchStream<'a> {
        crate::runtime::kv_watch::KvWatchStream::subscribe_prefix(
            &self.inner.cdc,
            &self.inner.kv_stats,
            collection,
            prefix,
            from_lsn,
            self.kv_watch_idle_timeout_ms(),
        )
    }

    pub(crate) fn kv_watch_idle_timeout_ms(&self) -> u64 {
        self.config_u64("red.config.kv.watch.idle_timeout_ms", 60_000)
    }

    /// Get backup scheduler status.
    pub fn backup_status(&self) -> crate::replication::scheduler::BackupStatus {
        self.inner.backup_scheduler.status()
    }

    /// Borrow the runtime's result Blob Cache.
    ///
    /// Wired for the `/admin/blob_cache/sweep` and
    /// `/admin/blob_cache/flush_namespace` HTTP handlers (issue #148
    /// follow-up): both delegate to
    /// `crate::storage::cache::sweeper::BlobCacheSweeper`, which takes a
    /// `&BlobCache`. Also used by `trigger_backup` when
    /// `red.config.backup.include_blob_cache=true` to locate the L2
    /// directory for archival.
    pub fn result_blob_cache(&self) -> &crate::storage::cache::BlobCache {
        &self.inner.result_blob_cache
    }

    /// PLAN.md Phase 11.4 — owned snapshot of every registered
    /// replica's state on this primary. Returns empty vec on
    /// non-primary instances or when no replicas are registered yet.
    pub fn primary_replica_snapshots(&self) -> Vec<crate::replication::primary::ReplicaState> {
        self.inner
            .db
            .replication
            .as_ref()
            .map(|repl| repl.replica_snapshots())
            .unwrap_or_default()
    }

    /// PLAN.md Phase 11.4 — active commit policy. Reads
    /// `RED_PRIMARY_COMMIT_POLICY` once at runtime construction;
    /// future env reloads will need a reload endpoint. Default is
    /// `Local` — current behavior, no replica blocking.
    pub fn commit_policy(&self) -> crate::replication::CommitPolicy {
        crate::replication::CommitPolicy::from_env()
    }

    /// PLAN.md Phase 11.5 — accessor for replica-side apply error
    /// counters (gap / divergence / apply / decode). Returned
    /// snapshot is consistent across the four counters; the labels
    /// match `reddb_replica_apply_errors_total{kind}`.
    pub fn replica_apply_error_counts(
        &self,
    ) -> [(crate::replication::logical::ApplyErrorKind, u64); 4] {
        self.inner.replica_apply_metrics.snapshot()
    }

    /// PLAN.md Phase 4.4 — per-caller quota bucket. Always
    /// returned; `is_configured()` lets callers short-circuit.
    pub fn quota_bucket(&self) -> &crate::runtime::quota_bucket::QuotaBucket {
        &self.inner.quota_bucket
    }

    /// PLAN.md Phase 11.4 — observability snapshot of every
    /// replica's durable LSN as known to the commit waiter. Empty
    /// vec on non-primary instances or when no replica has acked.
    pub fn commit_waiter_snapshot(&self) -> Vec<(String, u64)> {
        self.inner
            .db
            .replication
            .as_ref()
            .map(|repl| repl.commit_waiter.snapshot())
            .unwrap_or_default()
    }

    /// PLAN.md Phase 11.4 — `(reached, timed_out, not_required, last_micros)`
    /// counters for /metrics. Always-zero on non-primary instances.
    pub fn commit_waiter_metrics_snapshot(&self) -> (u64, u64, u64, u64) {
        self.inner
            .db
            .replication
            .as_ref()
            .map(|repl| repl.commit_waiter.metrics_snapshot())
            .unwrap_or((0, 0, 0, 0))
    }

    /// PLAN.md Phase 11.4 — block until at least `count` replicas
    /// have durably applied through `target_lsn`, or `timeout`
    /// elapses. Returns the `AwaitOutcome` so the caller can decide
    /// whether to surface a timeout error to the client or continue
    /// (the policy mapping lives in the commit dispatcher).
    ///
    /// Foundation only — the write commit path doesn't yet call
    /// this. Wiring it is a per-surface task gated on the operator
    /// flipping `RED_PRIMARY_COMMIT_POLICY` away from `local`.
    pub fn await_replica_acks(
        &self,
        target_lsn: u64,
        count: u32,
        timeout: std::time::Duration,
    ) -> crate::replication::AwaitOutcome {
        match &self.inner.db.replication {
            Some(repl) => repl.commit_waiter.await_acks(target_lsn, count, timeout),
            None => {
                // No replication configured: policy must be `Local`.
                // Treat as immediate `NotRequired` so callers don't
                // block on a degenerate setup.
                crate::replication::AwaitOutcome::NotRequired
            }
        }
    }

    /// PLAN.md Phase 11.4 — enforce the configured commit policy
    /// against `post_lsn` (the LSN of the just-completed write).
    /// Returns `Ok(AwaitOutcome)` on every successful enforcement
    /// (including `Reached` and `TimedOut` when fail-on-timeout is
    /// off). Returns `Err(ReadOnly)` only when:
    ///   * policy is `AckN(n)` with `n > 0`
    ///   * the wait timed out
    ///   * `RED_COMMIT_FAIL_ON_TIMEOUT=true` is set
    ///
    /// The HTTP / gRPC / wire surfaces map the error to 504 / wire
    /// backoff. Default behaviour (env unset) logs warn and returns
    /// success — matches PLAN.md "default v1 stays local" semantics
    /// while still letting the operator opt into hard-blocking.
    pub fn enforce_commit_policy(
        &self,
        post_lsn: u64,
    ) -> RedDBResult<crate::replication::AwaitOutcome> {
        let n = match self.commit_policy() {
            crate::replication::CommitPolicy::AckN(n) if n > 0 => n,
            _ => return Ok(crate::replication::AwaitOutcome::NotRequired),
        };
        let timeout_ms = std::env::var("RED_REPLICATION_ACK_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(5_000);
        let outcome =
            self.await_replica_acks(post_lsn, n, std::time::Duration::from_millis(timeout_ms));
        if let crate::replication::AwaitOutcome::TimedOut { observed, required } = &outcome {
            tracing::warn!(
                target: "reddb::commit",
                post_lsn,
                observed = *observed,
                required = *required,
                timeout_ms,
                "ack_n: timed out waiting for replicas"
            );
            let fail = std::env::var("RED_COMMIT_FAIL_ON_TIMEOUT")
                .ok()
                .map(|v| {
                    let t = v.trim();
                    t.eq_ignore_ascii_case("true") || t == "1" || t.eq_ignore_ascii_case("yes")
                })
                .unwrap_or(false);
            if fail {
                return Err(RedDBError::ReadOnly(format!(
                    "commit policy timed out at lsn {post_lsn}: observed={observed} required={required} (RED_COMMIT_FAIL_ON_TIMEOUT=true)"
                )));
            }
        }
        Ok(outcome)
    }

    /// PLAN.md Phase 6.3 — whether at-rest encryption is configured.
    /// Reads `RED_ENCRYPTION_KEY` / `RED_ENCRYPTION_KEY_FILE` lazily;
    /// returns `("enabled", None)` when a key is loadable, `("error", Some(msg))`
    /// when the operator set the env but it doesn't parse, and
    /// `("disabled", None)` when no key is configured. The pager
    /// hookup is deferred — this accessor surfaces the operator's
    /// intent for /admin/status without yet using the key in writes.
    pub fn encryption_at_rest_status(&self) -> (&'static str, Option<String>) {
        match crate::crypto::page_encryption::key_from_env() {
            Ok(Some(_)) => ("enabled", None),
            Ok(None) => ("disabled", None),
            Err(err) => ("error", Some(err)),
        }
    }

    /// PLAN.md Phase 11.5 — current replica apply health label
    /// (`ok`, `gap`, `divergence`, `apply_error`, `connecting`,
    /// `stalled_gap`). Read from the persisted `red.replication.state`
    /// config key updated by the replica loop. Returns `None` on
    /// non-replica instances or when no apply has run yet.
    pub fn replica_apply_health(&self) -> Option<String> {
        let state = self.config_string("red.replication.state", "");
        if state.is_empty() {
            None
        } else {
            Some(state)
        }
    }

    /// Current local LSN paired with the LSN of the most recently
    /// archived WAL segment. The difference is the replication /
    /// archive lag operators alert on (PLAN.md Phase 5.1). Returns
    /// `(0, 0)` when neither replication nor archiving is configured.
    pub fn wal_archive_progress(&self) -> (u64, u64) {
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
        (current_lsn, last_archived_lsn)
    }

    /// Trigger an immediate backup.
    pub fn trigger_backup(&self) -> RedDBResult<crate::replication::scheduler::BackupResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Backup)?;
        // Defense in depth — check_write above already rejects when
        // the lease is NotHeld, but log + audit the lease angle here
        // explicitly so dashboards distinguish "lease lost" from a
        // generic read-only refusal.
        self.assert_remote_write_allowed("admin/backup")?;
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
            // Hash the local snapshot bytes so the manifest can carry
            // the digest for restore-side verification (PLAN.md
            // Phase 4). Failure to hash is non-fatal — we still
            // publish the manifest, just without a checksum, so a
            // future fix can backfill rather than losing the backup.
            let snapshot_sha256 =
                crate::storage::wal::SnapshotManifest::compute_snapshot_sha256(path)
                    .map_err(|err| {
                        tracing::warn!(
                            target: "reddb::backup",
                            error = %err,
                            snapshot_id = snapshot.snapshot_id,
                            "snapshot hash failed; manifest will lack checksum"
                        );
                    })
                    .ok();
            let manifest = crate::storage::wal::SnapshotManifest {
                timeline_id: timeline_id.clone(),
                snapshot_key: snapshot_key.clone(),
                snapshot_id: snapshot.snapshot_id,
                snapshot_time: snapshot.created_at_unix_ms as u64,
                base_lsn: current_lsn,
                schema_version: crate::api::REDDB_FORMAT_VERSION,
                format_version: crate::api::REDDB_FORMAT_VERSION,
                snapshot_sha256,
            };
            crate::storage::wal::publish_snapshot_manifest(backend.as_ref(), &manifest)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;

            // PLAN.md Phase 11.3 — read the head of the WAL hash chain
            // so the new segment can link back. `None` means we're
            // starting a fresh timeline (after a clean restore or on
            // first archive ever); the segment's `prev_hash` will be
            // `None` and restore-side validation accepts that only for
            // the first segment in `plan.wal_segments`.
            let prev_segment_hash = self.config_string("red.config.timeline.last_segment_hash", "");
            let prev_hash_arg = if prev_segment_hash.is_empty() {
                None
            } else {
                Some(prev_segment_hash)
            };

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
                    prev_hash_arg,
                )
                .map_err(|err| RedDBError::Internal(err.to_string()))?
                {
                    if let Some(spool) = &primary.logical_wal_spool {
                        let _ = spool.prune_through(meta.lsn_end);
                    }
                    // Advance the chain head so the next archive call
                    // links to this segment's hash. If the segment has
                    // no sha256 (legacy / hashing failed) we leave the
                    // head as-is — the next segment then carries the
                    // prior chain head, preserving continuity.
                    if let Some(sha) = &meta.sha256 {
                        self.inner.db.store().set_config_tree(
                            "red.config.timeline",
                            &crate::json!({ "last_segment_hash": sha }),
                        );
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

            // PLAN.md Phase 2.4 — refresh the unified `MANIFEST.json`
            // at the prefix root so external tooling sees a single
            // catalog of every snapshot + WAL segment with their
            // checksums. Best-effort: a manifest publish failure
            // doesn't fail the backup (the per-artifact sidecars
            // already give restore-side integrity), but it does log
            // so dashboards can flag stale catalogs.
            if let Err(err) = crate::storage::wal::publish_unified_manifest_for_prefix(
                backend.as_ref(),
                &snapshot_prefix,
            ) {
                tracing::warn!(
                    target: "reddb::backup",
                    error = %err,
                    snapshot_prefix = %snapshot_prefix,
                    "unified MANIFEST.json refresh failed; per-artifact sidecars unaffected"
                );
            }

            // PLAN.md Phase 11.4 — when the operator picked a
            // commit policy that demands replica durability, block
            // until the configured count of replicas has acked the
            // archived LSN (or the timeout fires). For backup the
            // policy decides the *DR posture* — `local` returns
            // immediately, `ack_n` ensures at least N replicas saw
            // the new tail before we report success to the
            // operator. A `TimedOut` is logged but does NOT fail
            // the backup: the local WAL + remote upload are durable
            // regardless; the missing acks are reported via
            // /metrics and /admin/status so the operator can decide.
            match self.commit_policy() {
                crate::replication::CommitPolicy::AckN(n) if n > 0 => {
                    let timeout = std::env::var("RED_REPLICATION_ACK_TIMEOUT_MS")
                        .ok()
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(5_000);
                    let outcome = self.await_replica_acks(
                        archived_lsn,
                        n,
                        std::time::Duration::from_millis(timeout),
                    );
                    match outcome {
                        crate::replication::AwaitOutcome::Reached(count) => {
                            tracing::debug!(
                                target: "reddb::backup",
                                archived_lsn,
                                n,
                                count,
                                "ack_n: replicas synced before backup return"
                            );
                        }
                        crate::replication::AwaitOutcome::TimedOut { observed, required } => {
                            tracing::warn!(
                                target: "reddb::backup",
                                archived_lsn,
                                observed,
                                required,
                                timeout_ms = timeout,
                                "ack_n: timed out waiting for replicas; backup uploaded but DR posture degraded"
                            );
                        }
                        crate::replication::AwaitOutcome::NotRequired => {}
                    }
                }
                _ => {} // Local / RemoteWal / Quorum: no blocking yet
            }

            // Issue #148 follow-up — opt-in archive of the L2 Blob Cache
            // directory tree. Default off so a standard backup stays
            // small; flip via `red.config.backup.include_blob_cache=true`
            // when warm-cache restore is required (per
            // docs/operations/blob-cache-backup-restore.md §1).
            //
            // The L2 tree is *derived* state (ADR 0006) — its absence
            // never causes data loss; it only affects post-restore
            // p99 latency until the cache re-warms. We therefore log
            // (not fail) on per-file upload errors so a partial L2
            // upload never aborts a healthy snapshot+WAL backup.
            if self.config_bool("red.config.backup.include_blob_cache", false) {
                let blob_cache_prefix = self.config_string(
                    "red.config.backup.blob_cache_prefix",
                    &format!("{snapshot_prefix}blob_cache/"),
                );
                if let Some(l2_path) = self.inner.result_blob_cache.l2_path() {
                    match crate::storage::cache::archive_blob_cache_l2(
                        backend.as_ref(),
                        l2_path,
                        &blob_cache_prefix,
                    ) {
                        Ok(count) => {
                            tracing::info!(
                                target: "reddb::backup",
                                files_uploaded = count,
                                blob_cache_prefix = %blob_cache_prefix,
                                "include_blob_cache: archived L2 directory"
                            );
                        }
                        Err(err) => {
                            tracing::warn!(
                                target: "reddb::backup",
                                error = %err,
                                blob_cache_prefix = %blob_cache_prefix,
                                "include_blob_cache: L2 archive failed; backup proceeding (cache is derived state)"
                            );
                        }
                    }
                } else {
                    tracing::debug!(
                        target: "reddb::backup",
                        "include_blob_cache=true but no L2 path configured; nothing to archive"
                    );
                }
            }

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
        // Local fsync always allowed — losing the lease shouldn't
        // prevent us from durably persisting what's already in memory.
        // The remote upload is the side-effect that risks clobbering a
        // peer's state, so it's behind the lease gate.
        self.inner.db.flush_local_only().map_err(|err| {
            // Issue #205 — local flush failure is a CheckpointFailed
            // operator-grade event. The local-flush path also covers
            // the WAL fsync we depend on, so a failure here doubles as
            // the WalFsyncFailed signal for the runtime entry point.
            let msg = err.to_string();
            crate::telemetry::operator_event::OperatorEvent::CheckpointFailed {
                lsn: 0,
                error: msg.clone(),
            }
            .emit_global();
            crate::telemetry::operator_event::OperatorEvent::WalFsyncFailed {
                path: "<flush_local_only>".to_string(),
                error: msg.clone(),
            }
            .emit_global();
            RedDBError::Engine(msg)
        })?;
        if let Err(err) = self.assert_remote_write_allowed("checkpoint") {
            tracing::warn!(
                target: "reddb::serverless::lease",
                error = %err,
                "checkpoint: skipping remote upload — lease not held"
            );
            return Ok(());
        }
        self.inner
            .db
            .upload_to_remote_backend()
            .map_err(|err| RedDBError::Engine(err.to_string()))
    }

    /// Guard remote-mutating operations on the writer lease.
    /// Returns `Ok(())` when no remote backend is configured (the
    /// lease is irrelevant) or the lease state is `NotRequired` /
    /// `Held`. Returns `RedDBError::ReadOnly` when the lease is
    /// `NotHeld`, with an audit-friendly action label so the caller
    /// can record the rejection.
    pub(crate) fn assert_remote_write_allowed(&self, action: &str) -> RedDBResult<()> {
        if self.inner.db.remote_backend.is_none() {
            return Ok(());
        }
        match self.inner.write_gate.lease_state() {
            crate::runtime::write_gate::LeaseGateState::NotHeld => {
                self.inner.audit_log.record(
                    action,
                    "system",
                    "remote_backend",
                    "err: writer lease not held",
                    crate::json::Value::Null,
                );
                Err(RedDBError::ReadOnly(format!(
                    "writer lease not held — {action} blocked (serverless fence)"
                )))
            }
            _ => Ok(()),
        }
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
            result_blob_cache: self.inner.result_blob_cache.stats(),
            kv: self.inner.kv_stats.snapshot(),
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

    /// Issue #205 — single lifecycle exit for slow-query logging.
    ///
    /// `execute_query_inner` does the real work; this wrapper times it
    /// and, if elapsed exceeds the configured threshold, hands the
    /// triple `(QueryKind, elapsed_ms, sql_redacted, scope)` to the
    /// SlowQueryLogger. The threshold + sample_pct were captured at
    /// SlowQueryLogger construction (runtime startup), so the per-call
    /// cost on below-threshold paths is one relaxed atomic load.
    pub fn execute_query(&self, query: &str) -> RedDBResult<RuntimeQueryResult> {
        let started = std::time::Instant::now();
        let result = self.execute_query_inner(query);
        let elapsed_ms = started.elapsed().as_millis() as u64;

        // Build EffectiveScope from the same thread-locals frame-build
        // consults — keeps the slow-log row consistent with the audit /
        // RLS view of "this statement". `ai_scope()` is the canonical
        // builder.
        let scope = self.ai_scope();
        let kind = match result
            .as_ref()
            .map(|r| r.statement_type)
            .unwrap_or("select")
        {
            "select" => crate::telemetry::slow_query_logger::QueryKind::Select,
            "insert" => crate::telemetry::slow_query_logger::QueryKind::Insert,
            "update" => crate::telemetry::slow_query_logger::QueryKind::Update,
            "delete" => crate::telemetry::slow_query_logger::QueryKind::Delete,
            _ => crate::telemetry::slow_query_logger::QueryKind::Internal,
        };
        // SQL redaction: pass the raw query through. The slow-query
        // logger writes structured JSON so embedded literals stay
        // escape-safe at the JSON boundary (proven by
        // `adversarial_sql_is_escape_safe` in slow_query_logger.rs).
        // PII redaction (e.g. literal masking) is a follow-up.
        self.inner
            .slow_query_logger
            .record(kind, elapsed_ms, query.to_string(), &scope);

        result
    }

    #[inline(never)]
    fn execute_query_inner(&self, query: &str) -> RedDBResult<RuntimeQueryResult> {
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
                // Re-enter the inner path, NOT `execute_query`, so the
                // slow-query lifecycle hook records exactly one row per
                // top-level statement (the WITHIN-stripped form would
                // double-record).
                return self.execute_query_inner(inner);
            }
            Ok(None) => {}
            Err(msg) => return Err(RedDBError::Query(msg)),
        }

        // `EXPLAIN <stmt>` — introspection. Runs the planner on the
        // inner statement (WITHOUT executing it) and returns the
        // CanonicalLogicalNode tree as rows so the caller can see the
        // operator shape and estimated cost. `EXPLAIN ALTER FOR ...`
        // is a distinct schema-diff command and continues down the
        // regular SQL path.
        if let Some(inner) = strip_explain_prefix(query) {
            return self.explain_as_rows(query, inner);
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

        if super::red_schema::is_system_schema_write(query) {
            return Err(RedDBError::Query(
                super::red_schema::READ_ONLY_ERROR.to_string(),
            ));
        }

        let rewritten_query = super::red_schema::rewrite_virtual_names(query);
        let execution_query = rewritten_query.as_deref().unwrap_or(query);

        let frame = super::statement_frame::StatementExecutionFrame::build(self, execution_query)?;
        let _frame_guards = frame.install(self);

        // Phase 6 logging: enter a span stamped with conn_id / tenant
        // / query_len. Every downstream tracing::info!/warn!/error!
        // inherits these fields — no need to thread them manually
        // through storage/scan layers. Entered AFTER the WITHIN /
        // SET LOCAL TENANT resolution above so the span reflects the
        // effective scope for this statement.
        let _log_span = crate::telemetry::span::query_span(query).entered();

        // ── CTE prelude (#41) — `WITH x AS (...) SELECT ... FROM x` ──
        if let Some(rewritten) = frame.prepare_cte(execution_query)? {
            return self.execute_query_expr(rewritten);
        }

        // ── TURBO: bypass SQL parse for SELECT * FROM x WHERE _entity_id = N ──
        if let Some(result) = self.try_fast_entity_lookup(execution_query) {
            return result;
        }

        // ── Result cache: return cached result if still fresh (30s TTL) ──
        if let Some(result) = frame.read_result_cache(self) {
            return Ok(result);
        }

        let prepared = frame.prepare_statement(self, execution_query)?;
        let mode = prepared.mode;
        let expr = prepared.expr;

        let statement = query_expr_name(&expr);
        let result_cache_scopes = query_expr_result_cache_scopes(&expr);

        let _lock_guard = frame.prepare_dispatch(self, &expr)?;
        let frame_iface: &dyn super::statement_frame::ReadFrame = &frame;

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
                if super::red_schema::is_virtual_table(&table.table) {
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-red-schema",
                        result: super::red_schema::red_query(
                            self,
                            &table.table,
                            &table,
                            &frame as &dyn super::statement_frame::ReadFrame,
                        )?,
                        affected_rows: 0,
                        statement_type: "select",
                    });
                }

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
                let Some(table_with_rls) = self.authorize_relational_table_select(
                    table,
                    &frame as &dyn super::statement_frame::ReadFrame,
                )?
                else {
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
                let join_with_rls = match self.authorize_relational_join_select(
                    join,
                    &frame as &dyn super::statement_frame::ReadFrame,
                )? {
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
            QueryExpr::Insert(ref insert) if super::red_schema::is_virtual_table(&insert.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Update(ref update) if super::red_schema::is_virtual_table(&update.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Delete(ref delete) if super::red_schema::is_virtual_table(&delete.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Insert(ref insert) => {
                self.with_deferred_store_wal_if_transaction(|| self.execute_insert(query, insert))
            }
            QueryExpr::Update(ref update) => {
                self.with_deferred_store_wal_if_transaction(|| self.execute_update(query, update))
            }
            QueryExpr::Delete(ref delete) => {
                self.with_deferred_store_wal_if_transaction(|| self.execute_delete(query, delete))
            }
            // DDL execution
            QueryExpr::CreateTable(ref create) => self.execute_create_table(query, create),
            QueryExpr::CreateCollection(ref create) => {
                self.execute_create_collection(query, create)
            }
            QueryExpr::CreateVector(ref create) => self.execute_create_vector(query, create),
            QueryExpr::DropTable(ref drop_tbl) => self.execute_drop_table(query, drop_tbl),
            QueryExpr::DropGraph(ref drop_graph) => self.execute_drop_graph(query, drop_graph),
            QueryExpr::DropVector(ref drop_vector) => self.execute_drop_vector(query, drop_vector),
            QueryExpr::DropDocument(ref drop_document) => {
                self.execute_drop_document(query, drop_document)
            }
            QueryExpr::DropKv(ref drop_kv) => self.execute_drop_kv(query, drop_kv),
            QueryExpr::DropCollection(ref drop_collection) => {
                self.execute_drop_collection(query, drop_collection)
            }
            QueryExpr::Truncate(ref truncate) => self.execute_truncate(query, truncate),
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
            QueryExpr::AlterQueue(ref q) => self.execute_alter_queue(query, q),
            QueryExpr::DropQueue(ref q) => self.execute_drop_queue(query, q),
            QueryExpr::QueueSelect(ref q) => self.execute_queue_select(query, q),
            QueryExpr::QueueCommand(ref cmd) => self.execute_queue_command(query, cmd),
            QueryExpr::EventsBackfill(ref backfill) => {
                self.execute_events_backfill(query, backfill)
            }
            QueryExpr::EventsBackfillStatus { ref collection } => Err(RedDBError::Query(format!(
                "EVENTS BACKFILL STATUS for '{collection}' is not implemented in this slice"
            ))),
            QueryExpr::KvCommand(ref cmd) => self.execute_kv_command(query, cmd),
            QueryExpr::ConfigCommand(ref cmd) => self.execute_config_command(query, cmd),
            QueryExpr::CreateTree(ref tree) => self.execute_create_tree(query, tree),
            QueryExpr::DropTree(ref tree) => self.execute_drop_tree(query, tree),
            QueryExpr::TreeCommand(ref cmd) => self.execute_tree_command(query, cmd),
            // SET CONFIG key = value
            QueryExpr::SetConfig { ref key, ref value } => {
                if key.starts_with("red.secret.") {
                    return Err(RedDBError::Query(
                        "red.secret.* is reserved for vault secrets; use SET SECRET".to_string(),
                    ));
                }
                let store = self.inner.db.store();
                let json_val = match value {
                    Value::Text(s) => crate::serde_json::Value::String(s.to_string()),
                    Value::Integer(n) => crate::serde_json::Value::Number(*n as f64),
                    Value::Float(n) => crate::serde_json::Value::Number(*n),
                    Value::Boolean(b) => crate::serde_json::Value::Bool(*b),
                    _ => crate::serde_json::Value::String(value.to_string()),
                };
                store.set_config_tree(key, &json_val);
                update_current_config_value(key, value.clone());
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
            // SET SECRET key = value
            QueryExpr::SetSecret { ref key, ref value } => {
                if key.starts_with("red.config.") {
                    return Err(RedDBError::Query(
                        "red.config.* is reserved for config; use SET CONFIG".to_string(),
                    ));
                }
                let auth_store = self.inner.auth_store.read().clone().ok_or_else(|| {
                    RedDBError::Query("SET SECRET requires an enabled, unsealed vault".to_string())
                })?;
                if matches!(value, Value::Null) {
                    auth_store
                        .vault_kv_try_delete(key)
                        .map_err(|err| RedDBError::Query(err.to_string()))?;
                    update_current_secret_value(key, None);
                    self.invalidate_result_cache();
                    return Ok(RuntimeQueryResult::ok_message(
                        query.to_string(),
                        &format!("secret deleted: {key}"),
                        "delete_secret",
                    ));
                }
                let value = secret_sql_value_to_string(value)?;
                auth_store
                    .vault_kv_try_set(key.clone(), value.clone())
                    .map_err(|err| RedDBError::Query(err.to_string()))?;
                update_current_secret_value(key, Some(value));
                self.invalidate_result_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("secret set: {key}"),
                    "set_secret",
                ))
            }
            // DELETE SECRET key
            QueryExpr::DeleteSecret { ref key } => {
                let auth_store = self.inner.auth_store.read().clone().ok_or_else(|| {
                    RedDBError::Query(
                        "DELETE SECRET requires an enabled, unsealed vault".to_string(),
                    )
                })?;
                let deleted = auth_store
                    .vault_kv_try_delete(key)
                    .map_err(|err| RedDBError::Query(err.to_string()))?;
                if deleted {
                    update_current_secret_value(key, None);
                }
                self.invalidate_result_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("secret deleted: {key}"),
                    if deleted {
                        "delete_secret"
                    } else {
                        "delete_secret_not_found"
                    },
                ))
            }
            // SHOW SECRET[S] [prefix]
            QueryExpr::ShowSecrets { ref prefix } => {
                let auth_store = self.inner.auth_store.read().clone().ok_or_else(|| {
                    RedDBError::Query("SHOW SECRET requires an enabled, unsealed vault".to_string())
                })?;
                if !auth_store.is_vault_backed() {
                    return Err(RedDBError::Query(
                        "SHOW SECRET requires an enabled, unsealed vault".to_string(),
                    ));
                }
                let mut keys = auth_store.vault_kv_keys();
                keys.sort();
                let mut result = UnifiedResult::with_columns(vec![
                    "key".into(),
                    "value".into(),
                    "status".into(),
                ]);
                for key in keys {
                    if let Some(ref pfx) = prefix {
                        if !key.starts_with(pfx) {
                            continue;
                        }
                    }
                    let mut record = UnifiedRecord::new();
                    record.set("key", Value::text(key));
                    record.set("value", Value::text("***"));
                    record.set("status", Value::text("active"));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement: "show_secrets",
                    engine: "runtime-secret",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
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
                let mut latest = std::collections::BTreeMap::<String, (u64, Value, Value)>::new();
                for entity in entities {
                    if let EntityData::Row(ref row) = entity.data {
                        if let Some(ref named) = row.named {
                            let key_val = named.get("key").cloned().unwrap_or(Value::Null);
                            let val = named.get("value").cloned().unwrap_or(Value::Null);
                            let key_str = match &key_val {
                                Value::Text(s) => s.as_ref(),
                                _ => continue,
                            };
                            if let Some(ref pfx) = prefix {
                                if !key_str.starts_with(pfx.as_str()) {
                                    continue;
                                }
                            }
                            let entity_id = entity.id.raw();
                            match latest.get(key_str) {
                                Some((prev_id, _, _)) if *prev_id > entity_id => {}
                                _ => {
                                    latest.insert(key_str.to_string(), (entity_id, key_val, val));
                                }
                            }
                        }
                    }
                }
                let mut result = UnifiedResult::with_columns(vec!["key".into(), "value".into()]);
                for (_, key_val, val) in latest.into_values() {
                    let mut record = UnifiedRecord::new();
                    record.set("key", key_val);
                    record.set("value", val);
                    result.push(record);
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
                    current_tenant().map(Value::text).unwrap_or(Value::Null),
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
                                let mut own_xids = std::collections::HashSet::new();
                                own_xids.insert(ctx.xid);
                                for (_, sub) in &ctx.savepoints {
                                    own_xids.insert(*sub);
                                }
                                for sub in &ctx.released_sub_xids {
                                    own_xids.insert(*sub);
                                }
                                if let Err(err) = self.check_table_row_write_conflicts(
                                    conn_id,
                                    &ctx.snapshot,
                                    &own_xids,
                                ) {
                                    for (_, sub) in &ctx.savepoints {
                                        self.inner.snapshot_manager.rollback(*sub);
                                    }
                                    for sub in &ctx.released_sub_xids {
                                        self.inner.snapshot_manager.rollback(*sub);
                                    }
                                    self.inner.snapshot_manager.rollback(ctx.xid);
                                    self.revive_pending_versioned_updates(conn_id);
                                    self.revive_pending_tombstones(conn_id);
                                    self.discard_pending_kv_watch_events(conn_id);
                                    self.discard_pending_store_wal_actions(conn_id);
                                    return Err(err);
                                }
                                self.restore_pending_write_stamps(conn_id);
                                if let Err(err) = self.flush_pending_store_wal_actions(conn_id) {
                                    for (_, sub) in &ctx.savepoints {
                                        self.inner.snapshot_manager.rollback(*sub);
                                    }
                                    for sub in &ctx.released_sub_xids {
                                        self.inner.snapshot_manager.rollback(*sub);
                                    }
                                    self.inner.snapshot_manager.rollback(ctx.xid);
                                    self.revive_pending_versioned_updates(conn_id);
                                    self.revive_pending_tombstones(conn_id);
                                    self.discard_pending_kv_watch_events(conn_id);
                                    return Err(err);
                                }
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
                                self.finalize_pending_versioned_updates(conn_id);
                                self.finalize_pending_tombstones(conn_id);
                                self.finalize_pending_kv_watch_events(conn_id);
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
                                self.revive_pending_versioned_updates(conn_id);
                                self.revive_pending_tombstones(conn_id);
                                self.discard_pending_kv_watch_events(conn_id);
                                self.discard_pending_store_wal_actions(conn_id);
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
                                let reverted_updates =
                                    self.revive_versioned_updates_since(conn_id, savepoint_xid);
                                let revived = self.revive_tombstones_since(conn_id, savepoint_xid);
                                (
                                    "rollback_to_savepoint",
                                    format!(
                                        "ROLLBACK TO SAVEPOINT {name} — aborted {} sub_xid(s), reverted {reverted_updates} update(s), revived {revived} tombstone(s)",
                                        aborted.len(),
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
                // Issue #120 — surface policy names in the
                // schema-vocabulary so AskPipeline (#121) can resolve
                // a policy reference back to its table.
                self.schema_vocabulary_apply(
                    crate::runtime::schema_vocabulary::DdlEvent::CreatePolicy {
                        collection: q.table.clone(),
                        policy: q.name.clone(),
                    },
                );
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
                // Issue #120 — keep the schema-vocabulary policy
                // entry in sync.
                self.schema_vocabulary_apply(
                    crate::runtime::schema_vocabulary::DdlEvent::DropPolicy {
                        collection: q.table.clone(),
                        policy: q.name.clone(),
                    },
                );
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
                        let cutoff_xid = self.mvcc_vacuum_cutoff_xid();
                        let mut vacuum_stats =
                            crate::storage::unified::store::MvccVacuumStats::default();
                        for t in &targets {
                            let stats = store.vacuum_mvcc_history(t, cutoff_xid).map_err(|e| {
                                RedDBError::Internal(format!(
                                    "VACUUM MVCC history failed for {t}: {e}"
                                ))
                            })?;
                            if stats.reclaimed_versions > 0 {
                                self.rebuild_runtime_indexes_for_table(t)?;
                            }
                            vacuum_stats.add(&stats);
                        }
                        self.inner.snapshot_manager.prune_aborted(cutoff_xid);
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
                                "VACUUM{} processed {} table(s): scanned_versions={}, retained_versions={}, reclaimed_versions={}, retained_history_versions={}, reclaimed_history_versions={}, retained_tombstones={}, reclaimed_tombstones={}{}",
                                if *full { " FULL" } else { "" },
                                targets.len(),
                                vacuum_stats.scanned_versions,
                                vacuum_stats.retained_versions,
                                vacuum_stats.reclaimed_versions,
                                vacuum_stats.retained_history_versions,
                                vacuum_stats.reclaimed_history_versions,
                                vacuum_stats.retained_tombstones,
                                vacuum_stats.reclaimed_tombstones,
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
            // GRANT / REVOKE / ALTER USER (RBAC milestone).
            //
            // These hit the AuthStore directly. The privilege-check
            // gate at the top of `execute_query_expr` already decided
            // whether the caller may even run the statement; here we
            // just translate the AST into AuthStore calls.
            QueryExpr::Grant(ref g) => self.execute_grant_statement(query, g),
            QueryExpr::Revoke(ref r) => self.execute_revoke_statement(query, r),
            QueryExpr::AlterUser(ref a) => self.execute_alter_user_statement(query, a),
            QueryExpr::CreateIamPolicy { ref id, ref json } => {
                self.execute_create_iam_policy(query, id, json)
            }
            QueryExpr::DropIamPolicy { ref id } => self.execute_drop_iam_policy(query, id),
            QueryExpr::AttachPolicy {
                ref policy_id,
                ref principal,
            } => self.execute_attach_policy(query, policy_id, principal),
            QueryExpr::DetachPolicy {
                ref policy_id,
                ref principal,
            } => self.execute_detach_policy(query, policy_id, principal),
            QueryExpr::ShowPolicies { ref filter } => {
                self.execute_show_policies(query, filter.as_ref())
            }
            QueryExpr::ShowEffectivePermissions {
                ref user,
                ref resource,
            } => self.execute_show_effective_permissions(query, user, resource.as_ref()),
            QueryExpr::SimulatePolicy {
                ref user,
                ref action,
                ref resource,
            } => self.execute_simulate_policy(query, user, action, resource),
            QueryExpr::CreateMigration(ref q) => self.execute_create_migration(query, q),
            QueryExpr::ApplyMigration(ref q) => self.execute_apply_migration(query, q),
            QueryExpr::RollbackMigration(ref q) => self.execute_rollback_migration(query, q),
            QueryExpr::ExplainMigration(ref q) => self.execute_explain_migration(query, q),
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
            frame.write_result_cache(self, result, result_cache_scopes);
        }

        query_result
    }

    /// Execute a pre-parsed `QueryExpr` directly, bypassing SQL parsing and the
    /// plan cache. Used by the prepared-statement fast path so that `execute_prepared`
    /// calls pay zero parse + cache overhead.
    ///
    /// Applies secret decryption on SELECT results, identical to `execute_query`.
    pub fn execute_query_expr(&self, expr: QueryExpr) -> RedDBResult<RuntimeQueryResult> {
        let _config_snapshot_guard = ConfigSnapshotGuard::install(Arc::clone(&self.inner.db));
        let _secret_store_guard = SecretStoreGuard::install(self.inner.auth_store.read().clone());
        // View rewrite (Phase 2.1): substitute any `QueryExpr::Table(tq)`
        // whose `tq.table` matches a registered view with the view's
        // underlying query. Safe to call even when no views are registered.
        let expr = self.rewrite_view_refs(expr);

        self.validate_model_operations_before_auth(&expr)?;
        // Granular RBAC privilege check. Runs before dispatch so a
        // denied caller never reaches storage. Fail-closed: any error
        // resolving the action / resource produces PermissionDenied.
        if let Err(err) = self.check_query_privilege(&expr) {
            return Err(RedDBError::Query(format!("permission denied: {err}")));
        }

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

    pub(super) fn validate_model_operations_before_auth(
        &self,
        expr: &QueryExpr,
    ) -> RedDBResult<()> {
        use crate::catalog::CollectionModel;
        use crate::runtime::ddl::polymorphic_resolver;
        use crate::storage::query::ast::KvCommand;

        let system_schema_target = match expr {
            QueryExpr::DropTable(q) => Some(q.name.as_str()),
            QueryExpr::DropGraph(q) => Some(q.name.as_str()),
            QueryExpr::DropVector(q) => Some(q.name.as_str()),
            QueryExpr::DropDocument(q) => Some(q.name.as_str()),
            QueryExpr::DropKv(q) => Some(q.name.as_str()),
            QueryExpr::DropCollection(q) => Some(q.name.as_str()),
            QueryExpr::Truncate(q) => Some(q.name.as_str()),
            _ => None,
        };
        if system_schema_target.is_some_and(crate::runtime::impl_ddl::is_system_schema_name) {
            return Err(RedDBError::Query("system schema is read-only".to_string()));
        }

        let expected = match expr {
            QueryExpr::DropTable(q) => Some((q.name.as_str(), CollectionModel::Table)),
            QueryExpr::DropGraph(q) => Some((q.name.as_str(), CollectionModel::Graph)),
            QueryExpr::DropVector(q) => Some((q.name.as_str(), CollectionModel::Vector)),
            QueryExpr::DropDocument(q) => Some((q.name.as_str(), CollectionModel::Document)),
            QueryExpr::DropKv(q) => Some((q.name.as_str(), q.model)),
            QueryExpr::Truncate(q) => q.model.map(|model| (q.name.as_str(), model)),
            QueryExpr::KvCommand(cmd) => {
                let (collection, model) = match cmd {
                    KvCommand::Put {
                        collection, model, ..
                    }
                    | KvCommand::Get {
                        collection, model, ..
                    }
                    | KvCommand::Incr {
                        collection, model, ..
                    }
                    | KvCommand::Cas {
                        collection, model, ..
                    }
                    | KvCommand::Delete {
                        collection, model, ..
                    } => (collection.as_str(), *model),
                    KvCommand::Rotate { collection, .. }
                    | KvCommand::History { collection, .. }
                    | KvCommand::List { collection, .. }
                    | KvCommand::Purge { collection, .. } => {
                        (collection.as_str(), CollectionModel::Vault)
                    }
                    KvCommand::InvalidateTags { collection, .. } => {
                        (collection.as_str(), CollectionModel::Kv)
                    }
                    KvCommand::Watch {
                        collection, model, ..
                    } => (collection.as_str(), *model),
                    KvCommand::Unseal { collection, .. } => {
                        (collection.as_str(), CollectionModel::Vault)
                    }
                };
                Some((collection, model))
            }
            QueryExpr::ConfigCommand(cmd) => {
                self.validate_config_command_before_auth(cmd)?;
                None
            }
            _ => None,
        };

        let Some((name, expected_model)) = expected else {
            return Ok(());
        };
        let snapshot = self.inner.db.catalog_model_snapshot();
        let Some(actual_model) = snapshot
            .collections
            .iter()
            .find(|collection| collection.name == name)
            .map(|collection| collection.declared_model.unwrap_or(collection.model))
        else {
            return Ok(());
        };
        polymorphic_resolver::ensure_model_match(expected_model, actual_model)
    }

    /// Walk a `QueryExpr` and replace `QueryExpr::Table(tq)` nodes whose
    /// `tq.table` matches a registered view name with the view's stored
    /// body. Recurses through joins so `SELECT ... FROM t JOIN myview ...`
    /// resolves correctly. Pure operation — no side effects.
    pub(super) fn rewrite_view_refs(&self, expr: QueryExpr) -> QueryExpr {
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
    fn authorize_relational_table_select(
        &self,
        mut table: TableQuery,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<Option<TableQuery>> {
        if let Some(TableSource::Subquery(inner)) = table.source.take() {
            let authorized_inner = self.authorize_relational_select_expr(*inner, frame)?;
            table.source = Some(TableSource::Subquery(Box::new(authorized_inner)));
            return Ok(Some(table));
        }

        self.check_table_column_projection_authz(&table, frame)?;

        if self.inner.rls_enabled_tables.read().contains(&table.table) {
            return Ok(inject_rls_filters(self, frame, table));
        }

        Ok(Some(table))
    }

    fn authorize_relational_join_select(
        &self,
        mut join: JoinQuery,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<Option<JoinQuery>> {
        self.check_join_column_projection_authz(&join, frame)?;
        join.left = Box::new(self.authorize_relational_join_child(*join.left, frame)?);
        join.right = Box::new(self.authorize_relational_join_child(*join.right, frame)?);
        Ok(inject_rls_into_join(self, frame, join))
    }

    fn authorize_relational_join_child(
        &self,
        expr: QueryExpr,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<QueryExpr> {
        match expr {
            QueryExpr::Table(mut table) => {
                if let Some(TableSource::Subquery(inner)) = table.source.take() {
                    let authorized_inner = self.authorize_relational_select_expr(*inner, frame)?;
                    table.source = Some(TableSource::Subquery(Box::new(authorized_inner)));
                }
                Ok(QueryExpr::Table(table))
            }
            QueryExpr::Join(join) => self
                .authorize_relational_join_select(join, frame)?
                .map(QueryExpr::Join)
                .ok_or_else(|| {
                    RedDBError::Query("permission denied: RLS denied relational subquery".into())
                }),
            other => Ok(other),
        }
    }

    fn authorize_relational_select_expr(
        &self,
        expr: QueryExpr,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<QueryExpr> {
        match expr {
            QueryExpr::Table(table) => self
                .authorize_relational_table_select(table, frame)?
                .map(QueryExpr::Table)
                .ok_or_else(|| {
                    RedDBError::Query("permission denied: RLS denied relational subquery".into())
                }),
            QueryExpr::Join(join) => self
                .authorize_relational_join_select(join, frame)?
                .map(QueryExpr::Join)
                .ok_or_else(|| {
                    RedDBError::Query("permission denied: RLS denied relational subquery".into())
                }),
            other => Ok(other),
        }
    }

    fn check_table_column_projection_authz(
        &self,
        table: &TableQuery,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<()> {
        let Some((username, role)) = frame.identity() else {
            return Ok(());
        };
        let Some(auth_store) = self.inner.auth_store.read().clone() else {
            return Ok(());
        };

        let columns = self.resolved_table_projection_columns(table)?;
        let request = ColumnAccessRequest::select(table.table.clone(), columns);
        let principal = UserId::from_parts(frame.effective_scope(), username);
        let ctx = runtime_iam_context(role, frame.effective_scope());
        let outcome = auth_store.check_column_projection_authz(&principal, &request, &ctx);
        if outcome.allowed() {
            return Ok(());
        }

        if let Some(denied) = outcome.first_denied_column() {
            return Err(RedDBError::Query(format!(
                "permission denied: principal=`{username}` cannot select column `{}`",
                denied.resource.name
            )));
        }
        Err(RedDBError::Query(format!(
            "permission denied: principal=`{username}` cannot select table `{}`",
            table.table
        )))
    }

    fn check_join_column_projection_authz(
        &self,
        join: &JoinQuery,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<()> {
        let mut by_table: HashMap<String, BTreeSet<String>> = HashMap::new();
        let projections = crate::storage::query::sql_lowering::effective_join_projections(join);
        self.collect_join_projection_columns(join, &projections, &mut by_table)?;

        for (table, columns) in by_table {
            let query = TableQuery {
                table,
                source: None,
                alias: None,
                select_items: Vec::new(),
                columns: columns.into_iter().map(Projection::Column).collect(),
                where_expr: None,
                filter: None,
                group_by_exprs: Vec::new(),
                group_by: Vec::new(),
                having_expr: None,
                having: None,
                order_by: Vec::new(),
                limit: None,
                limit_param: None,
                offset: None,
                offset_param: None,
                expand: None,
                as_of: None,
            };
            self.check_table_column_projection_authz(&query, frame)?;
        }
        Ok(())
    }

    fn collect_join_projection_columns(
        &self,
        join: &JoinQuery,
        projections: &[Projection],
        out: &mut HashMap<String, BTreeSet<String>>,
    ) -> RedDBResult<()> {
        let left = table_side_context(join.left.as_ref());
        let right = table_side_context(join.right.as_ref());

        if projections
            .iter()
            .any(|projection| matches!(projection, Projection::All))
        {
            for side in [left.as_ref(), right.as_ref()].into_iter().flatten() {
                out.entry(side.table.clone())
                    .or_default()
                    .extend(self.table_all_projection_columns(&side.table)?);
            }
            return Ok(());
        }

        for projection in projections {
            collect_projection_columns_for_join_side(
                projection,
                left.as_ref(),
                right.as_ref(),
                out,
            )?;
        }
        Ok(())
    }

    fn resolved_table_projection_columns(&self, table: &TableQuery) -> RedDBResult<Vec<String>> {
        let projections = crate::storage::query::sql_lowering::effective_table_projections(table);
        if projections
            .iter()
            .any(|projection| matches!(projection, Projection::All))
        {
            return self.table_all_projection_columns(&table.table);
        }

        let mut columns = BTreeSet::new();
        for projection in &projections {
            collect_projection_columns_for_table(
                projection,
                &table.table,
                table.alias.as_deref(),
                &mut columns,
            );
        }
        Ok(columns.into_iter().collect())
    }

    fn table_all_projection_columns(&self, table: &str) -> RedDBResult<Vec<String>> {
        if let Some(contract) = self.inner.db.collection_contract_arc(table) {
            let columns: Vec<String> = contract
                .declared_columns
                .iter()
                .map(|column| column.name.clone())
                .collect();
            if !columns.is_empty() {
                return Ok(columns);
            }
        }

        let records = scan_runtime_table_source_records_limited(&self.inner.db, table, Some(1))?;
        Ok(records
            .first()
            .map(|record| {
                record
                    .column_names()
                    .into_iter()
                    .map(|column| column.to_string())
                    .collect()
            })
            .unwrap_or_default())
    }

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
                Err(RedDBError::Query(
                    "graph queries cannot be used as prepared statements".to_string(),
                ))
            }
            QueryExpr::Table(table) => {
                let scope = self.ai_scope();
                if super::red_schema::is_virtual_table(&table.table) {
                    return Ok(RuntimeQueryResult {
                        query: query_str.to_string(),
                        mode,
                        statement,
                        engine: "runtime-red-schema",
                        result: super::red_schema::red_query(
                            self,
                            &table.table,
                            &table,
                            &scope as &dyn super::statement_frame::ReadFrame,
                        )?,
                        affected_rows: 0,
                        statement_type: "select",
                    });
                }
                let Some(table_with_rls) = self.authorize_relational_table_select(
                    table,
                    &scope as &dyn super::statement_frame::ReadFrame,
                )?
                else {
                    return Ok(RuntimeQueryResult {
                        query: query_str.to_string(),
                        mode,
                        statement,
                        engine: "runtime-table-rls",
                        result: crate::storage::query::unified::UnifiedResult::empty(),
                        affected_rows: 0,
                        statement_type: "select",
                    });
                };
                Ok(RuntimeQueryResult {
                    query: query_str.to_string(),
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
                let scope = self.ai_scope();
                let Some(join_with_rls) = self.authorize_relational_join_select(
                    join,
                    &scope as &dyn super::statement_frame::ReadFrame,
                )?
                else {
                    return Ok(RuntimeQueryResult {
                        query: query_str.to_string(),
                        mode,
                        statement,
                        engine: "runtime-join-rls",
                        result: crate::storage::query::unified::UnifiedResult::empty(),
                        affected_rows: 0,
                        statement_type: "select",
                    });
                };
                Ok(RuntimeQueryResult {
                    query: query_str.to_string(),
                    mode,
                    statement,
                    engine: "runtime-join",
                    result: execute_runtime_join_query(&self.inner.db, &join_with_rls)?,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
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
            QueryExpr::Insert(ref insert) if super::red_schema::is_virtual_table(&insert.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Update(ref update) if super::red_schema::is_virtual_table(&update.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Delete(ref delete) if super::red_schema::is_virtual_table(&delete.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Insert(ref insert) => self
                .with_deferred_store_wal_if_transaction(|| self.execute_insert(query_str, insert)),
            QueryExpr::Update(ref update) => self
                .with_deferred_store_wal_if_transaction(|| self.execute_update(query_str, update)),
            QueryExpr::Delete(ref delete) => self
                .with_deferred_store_wal_if_transaction(|| self.execute_delete(query_str, delete)),
            QueryExpr::SearchCommand(ref cmd) => self.execute_search_command(query_str, cmd),
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

    fn result_cache_backend(&self) -> RuntimeResultCacheBackend {
        match self
            .config_string(RESULT_CACHE_BACKEND_KEY, RESULT_CACHE_DEFAULT_BACKEND)
            .as_str()
        {
            "blob_cache" => RuntimeResultCacheBackend::BlobCache,
            "shadow" => RuntimeResultCacheBackend::Shadow,
            _ => RuntimeResultCacheBackend::Legacy,
        }
    }

    pub(super) fn get_result_cache_entry(&self, key: &str) -> Option<RuntimeQueryResult> {
        match self.result_cache_backend() {
            RuntimeResultCacheBackend::Legacy => self.get_legacy_result_cache_entry(key),
            RuntimeResultCacheBackend::BlobCache => self.get_blob_result_cache_entry(key),
            RuntimeResultCacheBackend::Shadow => {
                let legacy = self.get_legacy_result_cache_entry(key);
                let blob = self.get_blob_result_cache_entry(key);
                if let (Some(ref legacy), Some(ref blob)) = (&legacy, &blob) {
                    if result_cache_fingerprint(legacy) != result_cache_fingerprint(blob) {
                        self.inner
                            .result_cache_shadow_divergences
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        tracing::warn!(
                            key,
                            metric = crate::runtime::METRIC_CACHE_SHADOW_DIVERGENCE_TOTAL,
                            "result cache shadow backend diverged from legacy"
                        );
                    }
                }
                legacy
            }
        }
    }

    fn get_legacy_result_cache_entry(&self, key: &str) -> Option<RuntimeQueryResult> {
        let cache = self.inner.result_cache.read();
        cache.0.get(key).and_then(|entry| {
            if entry.cached_at.elapsed().as_secs() < RESULT_CACHE_TTL_SECS {
                Some(entry.result.clone())
            } else {
                None
            }
        })
    }

    fn get_blob_result_cache_entry(&self, key: &str) -> Option<RuntimeQueryResult> {
        let hit = self
            .inner
            .result_blob_cache
            .get(RESULT_CACHE_BLOB_NAMESPACE, key)?;
        {
            let cache = self.inner.result_blob_entries.read();
            if let Some(entry) = cache.0.get(key) {
                return Some(entry.result.clone());
            }
        }

        let (result, scopes) = decode_result_cache_payload(hit.value())?;
        let mut cache = self.inner.result_blob_entries.write();
        let (ref mut map, ref mut order) = *cache;
        if !map.contains_key(key) {
            order.push_back(key.to_string());
        }
        map.insert(
            key.to_string(),
            RuntimeResultCacheEntry {
                result: result.clone(),
                cached_at: std::time::Instant::now(),
                scopes,
            },
        );
        trim_result_cache(map, order);
        Some(result)
    }

    pub(super) fn put_result_cache_entry(&self, key: &str, entry: RuntimeResultCacheEntry) {
        match self.result_cache_backend() {
            RuntimeResultCacheBackend::Legacy => self.put_legacy_result_cache_entry(key, entry),
            RuntimeResultCacheBackend::BlobCache => self.put_blob_result_cache_entry(key, entry),
            RuntimeResultCacheBackend::Shadow => {
                self.put_legacy_result_cache_entry(key, entry.clone());
                self.put_blob_result_cache_entry(key, entry);
            }
        }
    }

    fn put_legacy_result_cache_entry(&self, key: &str, entry: RuntimeResultCacheEntry) {
        let mut cache = self.inner.result_cache.write();
        let (ref mut map, ref mut order) = *cache;
        if !map.contains_key(key) {
            order.push_back(key.to_string());
        }
        map.insert(key.to_string(), entry);
        trim_result_cache(map, order);
    }

    fn put_blob_result_cache_entry(&self, key: &str, entry: RuntimeResultCacheEntry) {
        let policy = crate::storage::cache::BlobCachePolicy::default()
            .ttl_ms(RESULT_CACHE_TTL_SECS * 1000)
            .priority(200);
        let dependencies = entry.scopes.iter().cloned().collect::<Vec<_>>();
        let bytes = encode_result_cache_payload(&entry)
            .unwrap_or_else(|| result_cache_fingerprint(&entry.result).into_bytes());
        let put = crate::storage::cache::BlobCachePut::new(bytes)
            .with_dependencies(dependencies)
            .with_policy(policy);
        if self
            .inner
            .result_blob_cache
            .put(RESULT_CACHE_BLOB_NAMESPACE, key, put)
            .is_err()
        {
            return;
        }

        let mut cache = self.inner.result_blob_entries.write();
        let (ref mut map, ref mut order) = *cache;
        if !map.contains_key(key) {
            order.push_back(key.to_string());
        }
        map.insert(key.to_string(), entry);
        trim_result_cache(map, order);
    }

    pub fn result_cache_shadow_divergences(&self) -> u64 {
        self.inner
            .result_cache_shadow_divergences
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Invalidate the result cache (call after any write operation).
    /// Full clear — use for DDL (DROP TABLE, schema changes) or when table is unknown.
    pub fn invalidate_result_cache(&self) {
        let mut cache = self.inner.result_cache.write();
        cache.0.clear();
        cache.1.clear();
        let mut blob_entries = self.inner.result_blob_entries.write();
        blob_entries.0.clear();
        blob_entries.1.clear();
        self.inner
            .result_blob_cache
            .invalidate_namespace(RESULT_CACHE_BLOB_NAMESPACE);
        let mut ask_entries = self.inner.ask_answer_cache_entries.write();
        ask_entries.0.clear();
        ask_entries.1.clear();
        self.inner
            .result_blob_cache
            .invalidate_namespace(ASK_ANSWER_CACHE_NAMESPACE);
    }

    /// Invalidate only result cache entries that declared a dependency on `table`.
    /// Cheaper than a full clear: unrelated tables keep their cached results.
    pub(crate) fn invalidate_result_cache_for_table(&self, table: &str) {
        // Hot-path probe both backends before taking write locks. The blob
        // backend is node-local, same as the legacy result cache.
        let legacy_has_match = {
            let cache = self.inner.result_cache.read();
            let (ref map, _) = *cache;
            !map.is_empty() && map.values().any(|entry| entry.scopes.contains(table))
        };
        let blob_has_match = {
            let cache = self.inner.result_blob_entries.read();
            let (ref map, _) = *cache;
            !map.is_empty() && map.values().any(|entry| entry.scopes.contains(table))
        };
        if legacy_has_match {
            let mut cache = self.inner.result_cache.write();
            let (ref mut map, ref mut order) = *cache;
            map.retain(|_, entry| !entry.scopes.contains(table));
            order.retain(|key| map.contains_key(key));
        }

        if matches!(
            self.result_cache_backend(),
            RuntimeResultCacheBackend::BlobCache | RuntimeResultCacheBackend::Shadow
        ) {
            let mut blob_entries = self.inner.result_blob_entries.write();
            let (ref mut blob_map, ref mut blob_order) = *blob_entries;
            blob_map.clear();
            blob_order.clear();
            self.inner
                .result_blob_cache
                .invalidate_namespace(RESULT_CACHE_BLOB_NAMESPACE);
        } else if blob_has_match {
            let mut blob_entries = self.inner.result_blob_entries.write();
            let (ref mut blob_map, ref mut blob_order) = *blob_entries;
            blob_map.retain(|_, entry| !entry.scopes.contains(table));
            blob_order.retain(|key| blob_map.contains_key(key));
        }
        let mut ask_entries = self.inner.ask_answer_cache_entries.write();
        ask_entries.0.clear();
        ask_entries.1.clear();
        self.inner
            .result_blob_cache
            .invalidate_namespace(ASK_ANSWER_CACHE_NAMESPACE);
    }

    pub(crate) fn invalidate_plan_cache(&self) {
        self.inner.query_cache.write().clear();
        self.inner
            .ddl_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    /// Read the monotonic DDL epoch counter. Bumped by every
    /// `invalidate_plan_cache` call so prepared-statement holders can
    /// detect schema drift between PREPARE and EXECUTE.
    pub fn ddl_epoch(&self) -> u64 {
        self.inner
            .ddl_epoch
            .load(std::sync::atomic::Ordering::Acquire)
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
                // Issue #205 — a `tenant_tables.*` row that doesn't
                // split cleanly is a schema-shape regression: the
                // metadata writer must always emit the `.column`
                // suffix, so reaching this branch means an upgrade
                // with incompatible state or external tampering.
                crate::telemetry::operator_event::OperatorEvent::SchemaCorruption {
                    collection: "red_config".to_string(),
                    detail: format!("malformed tenant_tables key: {key}"),
                }
                .emit_global();
                continue;
            };
            if suffix != "column" {
                crate::telemetry::operator_event::OperatorEvent::SchemaCorruption {
                    collection: "red_config".to_string(),
                    detail: format!("unexpected tenant_tables suffix: {key}"),
                }
                .emit_global();
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
        previous_xmax: crate::storage::transaction::snapshot::Xid,
    ) {
        self.inner
            .pending_tombstones
            .write()
            .entry(conn_id)
            .or_default()
            .push((collection.to_string(), id, stamper_xid, previous_xmax));
    }

    pub(crate) fn record_pending_versioned_update(
        &self,
        conn_id: u64,
        collection: &str,
        old_id: crate::storage::unified::entity::EntityId,
        new_id: crate::storage::unified::entity::EntityId,
        stamper_xid: crate::storage::transaction::snapshot::Xid,
        previous_xmax: crate::storage::transaction::snapshot::Xid,
    ) {
        self.inner
            .pending_versioned_updates
            .write()
            .entry(conn_id)
            .or_default()
            .push((
                collection.to_string(),
                old_id,
                new_id,
                stamper_xid,
                previous_xmax,
            ));
    }

    fn with_deferred_store_wal_if_transaction<T>(
        &self,
        f: impl FnOnce() -> RedDBResult<T>,
    ) -> RedDBResult<T> {
        let conn_id = current_connection_id();
        if !self.inner.tx_contexts.read().contains_key(&conn_id) {
            return f();
        }

        crate::storage::UnifiedStore::begin_deferred_store_wal_capture();
        let result = f();
        let captured = crate::storage::UnifiedStore::take_deferred_store_wal_capture();
        match result {
            Ok(value) => {
                self.record_pending_store_wal_actions(conn_id, captured);
                Ok(value)
            }
            Err(err) => Err(err),
        }
    }

    fn record_pending_store_wal_actions(
        &self,
        conn_id: u64,
        actions: crate::storage::unified::DeferredStoreWalActions,
    ) {
        if actions.is_empty() {
            return;
        }
        let mut guard = self.inner.pending_store_wal_actions.write();
        guard.entry(conn_id).or_default().extend(actions);
    }

    fn flush_pending_store_wal_actions(&self, conn_id: u64) -> RedDBResult<()> {
        let Some(actions) = self
            .inner
            .pending_store_wal_actions
            .write()
            .remove(&conn_id)
        else {
            return Ok(());
        };
        self.inner
            .db
            .store()
            .append_deferred_store_wal_actions(actions)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    fn discard_pending_store_wal_actions(&self, conn_id: u64) {
        self.inner
            .pending_store_wal_actions
            .write()
            .remove(&conn_id);
    }

    fn xid_conflicts_with_snapshot(
        &self,
        xid: crate::storage::transaction::snapshot::Xid,
        snapshot: &crate::storage::transaction::snapshot::Snapshot,
        own_xids: &std::collections::HashSet<crate::storage::transaction::snapshot::Xid>,
    ) -> bool {
        xid != 0
            && !own_xids.contains(&xid)
            && !self.inner.snapshot_manager.is_aborted(xid)
            && !self.inner.snapshot_manager.is_active(xid)
            && (xid > snapshot.xid || snapshot.in_progress.contains(&xid))
    }

    fn conflict_error(
        collection: &str,
        logical_id: crate::storage::unified::entity::EntityId,
        xid: crate::storage::transaction::snapshot::Xid,
    ) -> RedDBError {
        RedDBError::Query(format!(
            "serialization conflict: table row {collection}/{} was modified by concurrent transaction {xid}",
            logical_id.raw()
        ))
    }

    fn check_logical_row_conflict(
        &self,
        collection: &str,
        logical_id: crate::storage::unified::entity::EntityId,
        excluded_ids: &[crate::storage::unified::entity::EntityId],
        snapshot: &crate::storage::transaction::snapshot::Snapshot,
        own_xids: &std::collections::HashSet<crate::storage::transaction::snapshot::Xid>,
    ) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(());
        };

        for candidate in manager.query_all(|_| true) {
            if excluded_ids.contains(&candidate.id) || candidate.logical_id() != logical_id {
                continue;
            }
            if self.xid_conflicts_with_snapshot(candidate.xmin, snapshot, own_xids) {
                return Err(Self::conflict_error(collection, logical_id, candidate.xmin));
            }
            if self.xid_conflicts_with_snapshot(candidate.xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(collection, logical_id, candidate.xmax));
            }
        }
        Ok(())
    }

    pub(crate) fn check_table_row_write_conflicts(
        &self,
        conn_id: u64,
        snapshot: &crate::storage::transaction::snapshot::Snapshot,
        own_xids: &std::collections::HashSet<crate::storage::transaction::snapshot::Xid>,
    ) -> RedDBResult<()> {
        let versioned_updates = self
            .inner
            .pending_versioned_updates
            .read()
            .get(&conn_id)
            .cloned()
            .unwrap_or_default();
        let tombstones = self
            .inner
            .pending_tombstones
            .read()
            .get(&conn_id)
            .cloned()
            .unwrap_or_default();

        let store = self.inner.db.store();
        for (collection, old_id, new_id, xid, previous_xmax) in versioned_updates {
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            let Some(old) = manager.get(old_id) else {
                continue;
            };
            let logical_id = old.logical_id();
            if self.xid_conflicts_with_snapshot(previous_xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(&collection, logical_id, previous_xmax));
            }
            if old.xmax != xid && self.xid_conflicts_with_snapshot(old.xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(&collection, logical_id, old.xmax));
            }
            self.check_logical_row_conflict(
                &collection,
                logical_id,
                &[old_id, new_id],
                snapshot,
                own_xids,
            )?;
        }

        for (collection, id, xid, previous_xmax) in tombstones {
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            let Some(entity) = manager.get(id) else {
                continue;
            };
            let logical_id = entity.logical_id();
            if self.xid_conflicts_with_snapshot(previous_xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(&collection, logical_id, previous_xmax));
            }
            if entity.xmax != xid
                && self.xid_conflicts_with_snapshot(entity.xmax, snapshot, own_xids)
            {
                return Err(Self::conflict_error(&collection, logical_id, entity.xmax));
            }
            self.check_logical_row_conflict(&collection, logical_id, &[id], snapshot, own_xids)?;
        }

        Ok(())
    }

    pub(crate) fn restore_pending_write_stamps(&self, conn_id: u64) {
        let versioned_updates = self
            .inner
            .pending_versioned_updates
            .read()
            .get(&conn_id)
            .cloned()
            .unwrap_or_default();
        let tombstones = self
            .inner
            .pending_tombstones
            .read()
            .get(&conn_id)
            .cloned()
            .unwrap_or_default();

        let store = self.inner.db.store();
        for (collection, old_id, _new_id, xid, _previous_xmax) in versioned_updates {
            if let Some(manager) = store.get_collection(&collection) {
                if let Some(mut entity) = manager.get(old_id) {
                    entity.set_xmax(xid);
                    let _ = manager.update(entity);
                }
            }
        }
        for (collection, id, xid, _previous_xmax) in tombstones {
            if let Some(manager) = store.get_collection(&collection) {
                if let Some(mut entity) = manager.get(id) {
                    entity.set_xmax(xid);
                    let _ = manager.update(entity);
                }
            }
        }
    }

    pub(crate) fn finalize_pending_versioned_updates(&self, conn_id: u64) {
        self.inner
            .pending_versioned_updates
            .write()
            .remove(&conn_id);
    }

    pub(crate) fn revive_pending_versioned_updates(&self, conn_id: u64) {
        let Some(pending) = self
            .inner
            .pending_versioned_updates
            .write()
            .remove(&conn_id)
        else {
            return;
        };

        let store = self.inner.db.store();
        for (collection, old_id, new_id, xid, previous_xmax) in pending {
            if let Some(manager) = store.get_collection(&collection) {
                if let Some(mut old) = manager.get(old_id) {
                    if old.xmax == xid {
                        old.set_xmax(previous_xmax);
                        let _ = manager.update(old);
                    }
                }
            }
            let _ = store.delete_batch(&collection, &[new_id]);
        }
    }

    pub(crate) fn revive_versioned_updates_since(&self, conn_id: u64, stamper_xid: u64) -> usize {
        let mut guard = self.inner.pending_versioned_updates.write();
        let Some(pending) = guard.get_mut(&conn_id) else {
            return 0;
        };

        let store = self.inner.db.store();
        let mut reverted = 0usize;
        pending.retain(|(collection, old_id, new_id, xid, previous_xmax)| {
            if *xid < stamper_xid {
                return true;
            }
            if let Some(manager) = store.get_collection(collection) {
                if let Some(mut old) = manager.get(*old_id) {
                    if old.xmax == *xid {
                        old.set_xmax(*previous_xmax);
                        let _ = manager.update(old);
                    }
                }
            }
            let _ = store.delete_batch(collection, &[*new_id]);
            reverted += 1;
            false
        });
        if pending.is_empty() {
            guard.remove(&conn_id);
        }
        reverted
    }

    /// Flush tombstones on COMMIT. The xmax stamp is already the durable
    /// delete marker; commit only drops the rollback journal and emits
    /// side effects. Physical reclamation is left for VACUUM so old
    /// snapshots can still resolve the pre-delete row version.
    pub(crate) fn finalize_pending_tombstones(&self, conn_id: u64) {
        let Some(pending) = self.inner.pending_tombstones.write().remove(&conn_id) else {
            return;
        };
        if pending.is_empty() {
            return;
        }

        let store = self.inner.db.store();
        for (collection, id, _xid, _previous_xmax) in pending {
            store.context_index().remove_entity(id);
            self.cdc_emit(
                crate::replication::cdc::ChangeOperation::Delete,
                &collection,
                id.raw(),
                "entity",
            );
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
        for (collection, id, xid, previous_xmax) in pending {
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            if let Some(mut entity) = manager.get(id) {
                if entity.xmax == xid {
                    entity.set_xmax(previous_xmax);
                    let _ = manager.update(entity);
                }
            }
        }
    }

    pub(crate) fn finalize_pending_kv_watch_events(&self, conn_id: u64) {
        let Some(pending) = self.inner.pending_kv_watch_events.write().remove(&conn_id) else {
            return;
        };
        for event in pending {
            self.cdc_emit_kv(
                event.op,
                &event.collection,
                &event.key,
                0,
                event.before,
                event.after,
            );
        }
    }

    pub(crate) fn discard_pending_kv_watch_events(&self, conn_id: u64) {
        self.inner.pending_kv_watch_events.write().remove(&conn_id);
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
                    .add_node_with_label(
                        &id_str,
                        &node.label,
                        &super::graph_node_label(&node.node_type),
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
                    .add_edge_with_label(
                        &edge.from_node,
                        &edge.to_node,
                        &super::graph_edge_label(&edge.label),
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
        pending.retain(|(collection, id, xid, previous_xmax)| {
            if *xid < stamper_xid {
                // Stamped before the savepoint — keep in queue.
                return true;
            }
            if let Some(manager) = store.get_collection(collection) {
                if let Some(mut entity) = manager.get(*id) {
                    if entity.xmax == *xid {
                        entity.set_xmax(*previous_xmax);
                        let _ = manager.update(entity);
                        revived += 1;
                    }
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

    fn mvcc_vacuum_cutoff_xid(&self) -> crate::storage::transaction::snapshot::Xid {
        let manager = &self.inner.snapshot_manager;
        let next_xid = manager.peek_next_xid();
        let mut cutoff = next_xid;
        if let Some(oldest_active) = manager.oldest_active_xid() {
            cutoff = cutoff.min(oldest_active);
        }
        if let Some(oldest_pinned) = manager.oldest_pinned_xid() {
            cutoff = cutoff.min(oldest_pinned);
        }
        let retention_xids = self.config_u64("runtime.mvcc.vacuum_retention_xids", 0);
        if retention_xids > 0 {
            cutoff = cutoff.min(next_xid.saturating_sub(retention_xids));
        }
        cutoff
    }

    fn rebuild_runtime_indexes_for_table(&self, table: &str) -> RedDBResult<()> {
        let registered = self.inner.index_store.list_indices(table);
        if registered.is_empty() {
            return Ok(());
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(table) else {
            return Ok(());
        };
        let entity_fields = manager
            .query_all(|entity| matches!(entity.kind, crate::storage::EntityKind::TableRow { .. }))
            .into_iter()
            .map(|entity| (entity.id, table_row_index_fields(&entity)))
            .collect::<Vec<_>>();

        for index in registered {
            self.inner.index_store.drop_index(&index.name, table);
            self.inner
                .index_store
                .create_index(
                    &index.name,
                    table,
                    &index.columns,
                    index.method,
                    index.unique,
                    &entity_fields,
                )
                .map_err(RedDBError::Internal)?;
            self.inner.index_store.register(index);
        }
        self.invalidate_plan_cache();
        Ok(())
    }

    /// Own-tx xids (parent + open/released savepoints) for the current
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
            for sub in &ctx.released_sub_xids {
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

    /// Wrap the planner's `RuntimeQueryExplain` as rows on a
    /// `RuntimeQueryResult` so callers over the SQL interface see the
    /// plan tree in the same shape a SELECT produces.
    ///
    /// Columns: `op`, `source`, `est_rows`, `est_cost`, `depth`.
    /// Nodes are walked depth-first; `depth` counts from 0 at the
    /// root so a text renderer can indent without re-walking.
    fn explain_as_rows(&self, raw_query: &str, inner_sql: &str) -> RedDBResult<RuntimeQueryResult> {
        let explain = self.explain_query(inner_sql)?;

        let columns = vec![
            "op".to_string(),
            "source".to_string(),
            "est_rows".to_string(),
            "est_cost".to_string(),
            "depth".to_string(),
        ];

        let mut records: Vec<crate::storage::query::unified::UnifiedRecord> = Vec::new();

        // Prepend `CteScan` markers when the query carried a leading
        // WITH clause. The CTE bodies are already inlined into the
        // main plan tree, but operators reading EXPLAIN need to see
        // which named CTEs were resolved — without this row the plan
        // would look indistinguishable from a hand-inlined query.
        for name in &explain.cte_materializations {
            use std::sync::Arc;
            let mut rec = crate::storage::query::unified::UnifiedRecord::default();
            rec.set_arc(Arc::from("op"), Value::text("CteScan".to_string()));
            rec.set_arc(Arc::from("source"), Value::text(name.clone()));
            rec.set_arc(Arc::from("est_rows"), Value::Float(0.0));
            rec.set_arc(Arc::from("est_cost"), Value::Float(0.0));
            rec.set_arc(Arc::from("depth"), Value::Integer(0));
            records.push(rec);
        }

        walk_plan_node(&explain.logical_plan.root, 0, &mut records);

        let result = crate::storage::query::unified::UnifiedResult {
            columns,
            records,
            stats: Default::default(),
            pre_serialized_json: None,
        };

        Ok(RuntimeQueryResult {
            query: raw_query.to_string(),
            mode: explain.mode,
            statement: "explain",
            engine: "runtime-explain",
            result,
            affected_rows: 0,
            statement_type: "select",
        })
    }

    // -----------------------------------------------------------------
    // Granular RBAC — privilege gate + GRANT/REVOKE/ALTER USER dispatch
    // -----------------------------------------------------------------

    /// Project a `QueryExpr` to the (action, resource) pair the
    /// privilege engine cares about. Returns `Ok(())` for statements
    /// that don't touch user data (transaction control, SHOW, SET, etc.).
    pub(super) fn check_query_privilege(
        &self,
        expr: &crate::storage::query::ast::QueryExpr,
    ) -> Result<(), String> {
        use crate::auth::privileges::{Action, AuthzContext, Resource};
        use crate::auth::UserId;
        use crate::storage::query::ast::QueryExpr;

        // No auth store wired (embedded mode / fresh DB / tests) → bypass.
        // The bootstrap path itself goes through `execute_query` so this
        // is the only sensible default; once auth is wired, the gate
        // becomes active.
        let auth_store = match self.inner.auth_store.read().clone() {
            Some(s) => s,
            None => return Ok(()),
        };

        // Resolve principal + role from the thread-local identity.
        // Anonymous (no identity) is allowed to read the bootstrap path
        // only when auth_store says so; we treat missing identity as
        // platform-admin-equivalent here so embedded test harnesses
        // continue to work without setting an identity.
        let (username, role) = match current_auth_identity() {
            Some(p) => p,
            None => return Ok(()),
        };
        let tenant = current_tenant();

        let ctx = AuthzContext {
            principal: &username,
            effective_role: role,
            tenant: tenant.as_deref(),
        };
        let principal_id = UserId::from_parts(tenant.as_deref(), &username);

        // Map QueryExpr → (Action, Resource).
        let (action, resource) = match expr {
            QueryExpr::Table(t) => (Action::Select, Resource::table_from_name(&t.table)),
            QueryExpr::QueueSelect(q) => (Action::Select, Resource::table_from_name(&q.queue)),
            QueryExpr::Graph(g) => {
                if auth_store.iam_authorization_enabled() {
                    self.check_graph_property_projection_privilege(
                        &auth_store,
                        &principal_id,
                        role,
                        tenant.as_deref(),
                        g,
                    )?;
                    return Ok(());
                }
                return Ok(());
            }
            QueryExpr::Vector(v) => {
                if auth_store.iam_authorization_enabled() {
                    self.check_table_like_column_projection_privilege(
                        &auth_store,
                        &principal_id,
                        role,
                        tenant.as_deref(),
                        &v.collection,
                        &["content".to_string()],
                    )?;
                    return Ok(());
                }
                return Ok(());
            }
            QueryExpr::Insert(i) => (Action::Insert, Resource::table_from_name(&i.table)),
            QueryExpr::Update(u) => (Action::Update, Resource::table_from_name(&u.table)),
            QueryExpr::Delete(d) => (Action::Delete, Resource::table_from_name(&d.table)),
            // Joins inherit the read privilege from any constituent
            // table — for now we emit a single Select on the database
            // (admins bypass; non-admins need a Database/Schema grant).
            QueryExpr::Join(_) => (Action::Select, Resource::Database),
            // GRANT / REVOKE / ALTER USER are authority statements;
            // require Admin (the helper methods enforce).
            QueryExpr::Grant(_) | QueryExpr::Revoke(_) | QueryExpr::AlterUser(_) => {
                return if role == crate::auth::Role::Admin {
                    Ok(())
                } else {
                    Err(format!(
                        "principal=`{}` role=`{:?}` cannot issue ACL/auth DDL",
                        username, role
                    ))
                };
            }
            QueryExpr::CreateIamPolicy { id, .. } => {
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:put",
                    "policy",
                    id,
                );
            }
            QueryExpr::DropIamPolicy { id } => {
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:drop",
                    "policy",
                    id,
                );
            }
            QueryExpr::AttachPolicy { policy_id, .. } => {
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:attach",
                    "policy",
                    policy_id,
                );
            }
            QueryExpr::DetachPolicy { policy_id, .. } => {
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:detach",
                    "policy",
                    policy_id,
                );
            }
            QueryExpr::ShowPolicies { .. } | QueryExpr::ShowEffectivePermissions { .. } => {
                return Ok(());
            }
            QueryExpr::SimulatePolicy { .. } => {
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:simulate",
                    "policy",
                    "*",
                );
            }
            // DROP and TRUNCATE — Write-role gate + per-collection IAM policy
            // when IAM mode is active. Other DDL stays role-only for now.
            QueryExpr::DropTable(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::DropGraph(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::DropVector(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::DropDocument(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::DropKv(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::DropCollection(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::Truncate(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "truncate",
                    &q.name,
                );
            }
            // Remaining DDL — gate on Write role. Fine-grained grants TBD.
            QueryExpr::CreateTable(_)
            | QueryExpr::CreateCollection(_)
            | QueryExpr::CreateVector(_)
            | QueryExpr::AlterTable(_)
            | QueryExpr::CreateIndex(_)
            | QueryExpr::DropIndex(_)
            | QueryExpr::CreateSchema(_)
            | QueryExpr::DropSchema(_)
            | QueryExpr::CreateSequence(_)
            | QueryExpr::DropSequence(_)
            | QueryExpr::CreateView(_)
            | QueryExpr::DropView(_)
            | QueryExpr::RefreshMaterializedView(_)
            | QueryExpr::CreatePolicy(_)
            | QueryExpr::DropPolicy(_)
            | QueryExpr::CreateServer(_)
            | QueryExpr::DropServer(_)
            | QueryExpr::CreateForeignTable(_)
            | QueryExpr::DropForeignTable(_)
            | QueryExpr::CreateTimeSeries(_)
            | QueryExpr::DropTimeSeries(_)
            | QueryExpr::CreateQueue(_)
            | QueryExpr::AlterQueue(_)
            | QueryExpr::DropQueue(_)
            | QueryExpr::CreateTree(_)
            | QueryExpr::DropTree(_) => {
                return if role >= crate::auth::Role::Write {
                    Ok(())
                } else {
                    Err(format!(
                        "principal=`{}` role=`{:?}` cannot issue DDL",
                        username, role
                    ))
                };
            }
            // Migration DDL — CREATE MIGRATION requires Write role (schema author).
            QueryExpr::CreateMigration(_) => {
                return if role >= crate::auth::Role::Write {
                    Ok(())
                } else {
                    Err(format!(
                        "principal=`{}` role=`{:?}` cannot issue CREATE MIGRATION",
                        username, role
                    ))
                };
            }
            // APPLY / ROLLBACK change data and schema — require Admin.
            QueryExpr::ApplyMigration(_) | QueryExpr::RollbackMigration(_) => {
                return if role == crate::auth::Role::Admin {
                    Ok(())
                } else {
                    Err(format!(
                        "principal=`{}` role=`{:?}` cannot issue APPLY/ROLLBACK MIGRATION",
                        username, role
                    ))
                };
            }
            // EXPLAIN MIGRATION is read-only — any authenticated principal.
            QueryExpr::ExplainMigration(_) => return Ok(()),
            // Everything else (SET, SHOW, transaction control, graph
            // commands, queue/tree commands, MaintenanceCommand …)
            // is allowed for any authenticated principal.
            _ => return Ok(()),
        };

        if auth_store.iam_authorization_enabled() {
            let iam_action = legacy_action_to_iam(action);
            let iam_resource = legacy_resource_to_iam(&resource, tenant.as_deref());
            let iam_ctx = runtime_iam_context(role, tenant.as_deref());
            if !auth_store.check_policy_authz(&principal_id, iam_action, &iam_resource, &iam_ctx) {
                return Err(format!(
                    "principal=`{}` action=`{}` resource=`{}:{}` denied by IAM policy",
                    username, iam_action, iam_resource.kind, iam_resource.name
                ));
            }

            if let QueryExpr::Table(table) = expr {
                self.check_table_column_projection_privilege(
                    &auth_store,
                    &principal_id,
                    &iam_ctx,
                    table,
                )?;
            }

            if let QueryExpr::Update(update) = expr {
                let columns = update_set_target_columns(update);
                if !columns.is_empty() {
                    let request = column_access_request_for_table_update(&update.table, columns);
                    let outcome =
                        auth_store.check_column_projection_authz(&principal_id, &request, &iam_ctx);
                    if let Some(denied) = outcome.first_denied_column() {
                        return Err(format!(
                            "principal=`{}` action=`{}` resource=`{}:{}` denied by IAM column policy",
                            username, iam_action, denied.resource.kind, denied.resource.name
                        ));
                    }
                    if !outcome.allowed() {
                        return Err(format!(
                            "principal=`{}` action=`{}` resource=`{}:{}` denied by IAM policy",
                            username,
                            iam_action,
                            outcome.table_resource.kind,
                            outcome.table_resource.name
                        ));
                    }
                }
            }

            Ok(())
        } else {
            auth_store
                .check_grant(&ctx, action, &resource)
                .map_err(|e| e.to_string())
        }
    }

    fn check_table_column_projection_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        ctx: &crate::auth::policies::EvalContext,
        table: &crate::storage::query::ast::TableQuery,
    ) -> Result<(), String> {
        use crate::auth::{ColumnAccessRequest, ColumnDecisionEffect};

        let columns = requested_table_columns_for_policy(table);
        if columns.is_empty() {
            return Ok(());
        }

        let request = ColumnAccessRequest::select(table.table.clone(), columns);
        let outcome = auth_store.check_column_projection_authz(principal, &request, ctx);
        if outcome.allowed() {
            return Ok(());
        }

        if !matches!(
            outcome.table_decision,
            crate::auth::policies::Decision::Allow { .. }
                | crate::auth::policies::Decision::AdminBypass
        ) {
            return Err(format!(
                "principal=`{}` action=`select` resource=`{}:{}` denied by IAM policy",
                principal, outcome.table_resource.kind, outcome.table_resource.name
            ));
        }

        let denied = outcome
            .first_denied_column()
            .filter(|decision| decision.effective == ColumnDecisionEffect::Denied);
        match denied {
            Some(decision) => Err(format!(
                "principal=`{}` action=`select` resource=`{}:{}` denied by IAM policy",
                principal, decision.resource.kind, decision.resource.name
            )),
            None => Ok(()),
        }
    }

    fn check_graph_property_projection_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        query: &crate::storage::query::ast::GraphQuery,
    ) -> Result<(), String> {
        let columns = explicit_graph_projection_properties(query);
        if columns.is_empty() {
            return Ok(());
        }
        self.check_table_like_column_projection_privilege(
            auth_store, principal, role, tenant, "graph", &columns,
        )
    }

    fn check_table_like_column_projection_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        table: &str,
        columns: &[String],
    ) -> Result<(), String> {
        let iam_ctx = runtime_iam_context(role, tenant);
        let request =
            crate::auth::ColumnAccessRequest::select(table.to_string(), columns.iter().cloned());
        let outcome = auth_store.check_column_projection_authz(principal, &request, &iam_ctx);
        if outcome.allowed() {
            return Ok(());
        }
        let denied = outcome
            .first_denied_column()
            .map(|d| d.resource.name.clone())
            .unwrap_or_else(|| format!("{table}.<unknown>"));
        Err(format!(
            "principal=`{}` action=`select` resource=`column:{}` denied by IAM policy",
            principal, denied
        ))
    }

    fn check_policy_management_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        action: &str,
        resource_kind: &str,
        resource_name: &str,
    ) -> Result<(), String> {
        if !auth_store.iam_authorization_enabled() {
            return if role == crate::auth::Role::Admin {
                Ok(())
            } else {
                Err(format!(
                    "principal=`{}` role=`{:?}` cannot issue ACL/auth DDL",
                    principal, role
                ))
            };
        }

        let mut resource = crate::auth::policies::ResourceRef::new(
            resource_kind.to_string(),
            resource_name.to_string(),
        );
        if let Some(t) = tenant {
            resource = resource.with_tenant(t.to_string());
        }
        let ctx = runtime_iam_context(role, tenant);
        if auth_store.check_policy_authz(principal, action, &resource, &ctx) {
            Ok(())
        } else {
            Err(format!(
                "principal=`{}` action=`{}` resource=`{}:{}` denied by IAM policy",
                principal, action, resource.kind, resource.name
            ))
        }
    }

    /// IAM privilege check for DROP / TRUNCATE on a named collection.
    ///
    /// In legacy mode (IAM not enabled): requires Write role.
    /// In IAM mode: requires an explicit `drop` / `truncate` policy on
    /// `collection:<name>` (Admin role auto-passes via AdminBypass).
    /// Records an audit log entry for both allow and deny outcomes.
    fn check_ddl_collection_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        username: &str,
        action: &str,
        collection: &str,
    ) -> Result<(), String> {
        if role < crate::auth::Role::Write {
            let msg = format!(
                "principal=`{}` role=`{:?}` cannot issue DDL",
                username, role
            );
            self.inner.audit_log.record(
                action,
                username,
                collection,
                "denied",
                crate::json::Value::Null,
            );
            return Err(msg);
        }

        if !auth_store.iam_authorization_enabled() {
            self.inner.audit_log.record(
                action,
                username,
                collection,
                "ok",
                crate::json::Value::Null,
            );
            return Ok(());
        }

        let resource_name = collection.to_string();
        let mut resource = crate::auth::policies::ResourceRef::new(
            "collection".to_string(),
            resource_name.clone(),
        );
        if let Some(t) = tenant {
            resource = resource.with_tenant(t.to_string());
        }
        let ctx = runtime_iam_context(role, tenant);
        if auth_store.check_policy_authz(principal, action, &resource, &ctx) {
            self.inner.audit_log.record(
                action,
                username,
                &resource_name,
                "ok",
                crate::json::Value::Null,
            );
            Ok(())
        } else {
            self.inner.audit_log.record(
                action,
                username,
                &resource_name,
                "denied",
                crate::json::Value::Null,
            );
            Err(format!(
                "principal=`{}` action=`{}` resource=`collection:{}` denied by IAM policy",
                username, action, resource_name
            ))
        }
    }

    /// Translate the parsed [`GrantStmt`] into AuthStore mutations.
    fn execute_grant_statement(
        &self,
        query: &str,
        stmt: &crate::storage::query::ast::GrantStmt,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::privileges::{Action, GrantPrincipal, Resource};
        use crate::auth::UserId;
        use crate::storage::query::ast::{GrantObjectKind, GrantPrincipalRef};

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        // Granter identity + role.
        let (gname, grole) = current_auth_identity().ok_or_else(|| {
            RedDBError::Query("GRANT requires an authenticated principal".to_string())
        })?;
        let granter = UserId::from_parts(current_tenant().as_deref(), &gname);
        let granter_role = grole;

        // Build the action set.
        let mut actions: Vec<Action> = Vec::new();
        if stmt.all {
            actions.push(Action::All);
        } else {
            for kw in &stmt.actions {
                let a = Action::from_keyword(kw).ok_or_else(|| {
                    RedDBError::Query(format!("unknown privilege keyword `{}`", kw))
                })?;
                actions.push(a);
            }
        }

        // Audit emit (printed; structured emission is Agent #4's lane).
        let mut applied = 0usize;
        for obj in &stmt.objects {
            let resource = match stmt.object_kind {
                GrantObjectKind::Table => Resource::Table {
                    schema: obj.schema.clone(),
                    table: obj.name.clone(),
                },
                GrantObjectKind::Schema => Resource::Schema(obj.name.clone()),
                GrantObjectKind::Database => Resource::Database,
                GrantObjectKind::Function => Resource::Function {
                    schema: obj.schema.clone(),
                    name: obj.name.clone(),
                },
            };
            for principal in &stmt.principals {
                let p = match principal {
                    GrantPrincipalRef::Public => GrantPrincipal::Public,
                    GrantPrincipalRef::Group(g) => GrantPrincipal::Group(g.clone()),
                    GrantPrincipalRef::User { tenant, name } => {
                        GrantPrincipal::User(UserId::from_parts(tenant.as_deref(), name))
                    }
                };
                // Tenant of the grant follows the granter's tenant
                // (cross-tenant guard inside `AuthStore::grant`).
                let tenant = granter.tenant.clone();
                auth_store
                    .grant(
                        &granter,
                        granter_role,
                        p.clone(),
                        resource.clone(),
                        actions.clone(),
                        stmt.with_grant_option,
                        tenant.clone(),
                    )
                    .map_err(|e| RedDBError::Query(e.to_string()))?;

                // IAM policy translation: every GRANT also lands as a
                // synthetic `_grant_<id>` policy attached to the
                // principal so the new evaluator sees it.
                if let Some(policy) =
                    grant_to_iam_policy(&p, &resource, &actions, tenant.as_deref())
                {
                    let pid = policy.id.clone();
                    auth_store
                        .put_policy_internal(policy)
                        .map_err(|e| RedDBError::Query(e.to_string()))?;
                    let attachment = match &p {
                        GrantPrincipal::User(uid) => {
                            crate::auth::store::PrincipalRef::User(uid.clone())
                        }
                        GrantPrincipal::Group(group) => {
                            crate::auth::store::PrincipalRef::Group(group.clone())
                        }
                        GrantPrincipal::Public => crate::auth::store::PrincipalRef::Group(
                            crate::auth::store::PUBLIC_IAM_GROUP.to_string(),
                        ),
                    };
                    auth_store
                        .attach_policy(attachment, &pid)
                        .map_err(|e| RedDBError::Query(e.to_string()))?;
                }
                applied += 1;
                tracing::info!(
                    target: "audit",
                    principal = %granter,
                    action = "grant",
                    "GRANT applied"
                );
            }
        }

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("GRANT applied to {} target(s)", applied),
            "grant",
        ))
    }

    /// Translate the parsed [`RevokeStmt`] into AuthStore mutations.
    fn execute_revoke_statement(
        &self,
        query: &str,
        stmt: &crate::storage::query::ast::RevokeStmt,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::privileges::{Action, GrantPrincipal, Resource};
        use crate::auth::UserId;
        use crate::storage::query::ast::{GrantObjectKind, GrantPrincipalRef};

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        let (_gname, grole) = current_auth_identity().ok_or_else(|| {
            RedDBError::Query("REVOKE requires an authenticated principal".to_string())
        })?;
        let granter_role = grole;

        let actions: Vec<Action> = if stmt.all {
            vec![Action::All]
        } else {
            stmt.actions
                .iter()
                .map(|kw| Action::from_keyword(kw).unwrap_or(Action::Select))
                .collect()
        };

        let mut total_removed = 0usize;
        for obj in &stmt.objects {
            let resource = match stmt.object_kind {
                GrantObjectKind::Table => Resource::Table {
                    schema: obj.schema.clone(),
                    table: obj.name.clone(),
                },
                GrantObjectKind::Schema => Resource::Schema(obj.name.clone()),
                GrantObjectKind::Database => Resource::Database,
                GrantObjectKind::Function => Resource::Function {
                    schema: obj.schema.clone(),
                    name: obj.name.clone(),
                },
            };
            for principal in &stmt.principals {
                let p = match principal {
                    GrantPrincipalRef::Public => GrantPrincipal::Public,
                    GrantPrincipalRef::Group(g) => GrantPrincipal::Group(g.clone()),
                    GrantPrincipalRef::User { tenant, name } => {
                        GrantPrincipal::User(UserId::from_parts(tenant.as_deref(), name))
                    }
                };
                let removed = auth_store
                    .revoke(granter_role, &p, &resource, &actions)
                    .map_err(|e| RedDBError::Query(e.to_string()))?;
                let _removed_policies =
                    auth_store.delete_synthetic_grant_policies(&p, &resource, &actions);
                total_removed += removed;
            }
        }

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("REVOKE removed {} grant(s)", total_removed),
            "revoke",
        ))
    }

    /// Translate the parsed [`AlterUserStmt`] into AuthStore mutations.
    fn execute_alter_user_statement(
        &self,
        query: &str,
        stmt: &crate::storage::query::ast::AlterUserStmt,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::privileges::UserAttributes;
        use crate::auth::UserId;
        use crate::storage::query::ast::AlterUserAttribute;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        let (_gname, grole) = current_auth_identity().ok_or_else(|| {
            RedDBError::Query("ALTER USER requires an authenticated principal".to_string())
        })?;
        if grole != crate::auth::Role::Admin {
            return Err(RedDBError::Query(
                "ALTER USER requires Admin role".to_string(),
            ));
        }

        let target = UserId::from_parts(stmt.tenant.as_deref(), &stmt.username);

        // Apply attributes incrementally — each one reads the current
        // record, mutates the relevant field, writes back.
        let mut attrs = auth_store.user_attributes(&target);
        let mut enable_change: Option<bool> = None;

        for a in &stmt.attributes {
            match a {
                AlterUserAttribute::ValidUntil(ts) => {
                    // Parse ISO-ish timestamp → ms since epoch. Fall
                    // back to integer-ms parsing for callers that pass
                    // `'1234567890123'`.
                    let ms = parse_timestamp_to_ms(ts).ok_or_else(|| {
                        RedDBError::Query(format!("invalid VALID UNTIL timestamp `{ts}`"))
                    })?;
                    attrs.valid_until = Some(ms);
                }
                AlterUserAttribute::ConnectionLimit(n) => {
                    if *n < 0 {
                        return Err(RedDBError::Query(
                            "CONNECTION LIMIT must be non-negative".to_string(),
                        ));
                    }
                    attrs.connection_limit = Some(*n as u32);
                }
                AlterUserAttribute::SetSearchPath(p) => {
                    attrs.search_path = Some(p.clone());
                }
                AlterUserAttribute::AddGroup(g) => {
                    if !attrs.groups.iter().any(|existing| existing == g) {
                        attrs.groups.push(g.clone());
                        attrs.groups.sort();
                    }
                }
                AlterUserAttribute::DropGroup(g) => {
                    attrs.groups.retain(|existing| existing != g);
                }
                AlterUserAttribute::Enable => enable_change = Some(true),
                AlterUserAttribute::Disable => enable_change = Some(false),
                AlterUserAttribute::Password(_) => {
                    // Out of scope — accept the AST but no-op so the
                    // parser stays compatible with future password
                    // rotation work.
                }
            }
        }

        auth_store
            .set_user_attributes(&target, attrs)
            .map_err(|e| RedDBError::Query(e.to_string()))?;
        if let Some(en) = enable_change {
            auth_store
                .set_user_enabled(&target, en)
                .map_err(|e| RedDBError::Query(e.to_string()))?;
        }
        self.invalidate_result_cache();
        tracing::info!(
            target: "audit",
            principal = %target,
            action = "alter_user",
            "ALTER USER applied"
        );

        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("ALTER USER {} applied", target),
            "alter_user",
        ))
    }

    // -----------------------------------------------------------------
    // IAM policy executors
    // -----------------------------------------------------------------

    fn execute_create_iam_policy(
        &self,
        query: &str,
        id: &str,
        json: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::policies::Policy;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        // Parse + validate. The kernel rejects oversize / bad shape /
        // bad action keywords. If the supplied id differs from the JSON
        // id, override it with the SQL-provided id (the JSON id is
        // optional context — the SQL DDL form is authoritative).
        let mut policy = Policy::from_json_str(json)
            .map_err(|e| RedDBError::Query(format!("policy parse: {e}")))?;
        if policy.id != id {
            policy.id = id.to_string();
        }
        let pid = policy.id.clone();
        auth_store
            .put_policy(policy)
            .map_err(|e| RedDBError::Query(e.to_string()))?;

        let principal = current_auth_identity()
            .map(|(u, _)| u)
            .unwrap_or_else(|| "anonymous".into());
        tracing::info!(
            target: "audit",
            principal = %principal,
            action = "iam:policy.put",
            matched_policy_id = %pid,
            "CREATE POLICY applied"
        );
        self.inner.audit_log.record(
            "iam/policy.put",
            &principal,
            &pid,
            "ok",
            crate::json::Value::Null,
        );

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("policy `{pid}` stored"),
            "create_iam_policy",
        ))
    }

    fn execute_drop_iam_policy(&self, query: &str, id: &str) -> RedDBResult<RuntimeQueryResult> {
        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;
        auth_store
            .delete_policy(id)
            .map_err(|e| RedDBError::Query(e.to_string()))?;

        let principal = current_auth_identity()
            .map(|(u, _)| u)
            .unwrap_or_else(|| "anonymous".into());
        tracing::info!(
            target: "audit",
            principal = %principal,
            action = "iam:policy.drop",
            matched_policy_id = %id,
            "DROP POLICY applied"
        );
        self.inner.audit_log.record(
            "iam/policy.drop",
            &principal,
            id,
            "ok",
            crate::json::Value::Null,
        );

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("policy `{id}` dropped"),
            "drop_iam_policy",
        ))
    }

    fn execute_attach_policy(
        &self,
        query: &str,
        policy_id: &str,
        principal: &crate::storage::query::ast::PolicyPrincipalRef,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::store::PrincipalRef;
        use crate::auth::UserId;
        use crate::storage::query::ast::PolicyPrincipalRef;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;
        let p = match principal {
            PolicyPrincipalRef::User(u) => {
                PrincipalRef::User(UserId::from_parts(u.tenant.as_deref(), &u.username))
            }
            PolicyPrincipalRef::Group(g) => PrincipalRef::Group(g.clone()),
        };
        let pretty_target = principal_label(principal);
        auth_store
            .attach_policy(p, policy_id)
            .map_err(|e| RedDBError::Query(e.to_string()))?;

        let principal_str = current_auth_identity()
            .map(|(u, _)| u)
            .unwrap_or_else(|| "anonymous".into());
        tracing::info!(
            target: "audit",
            principal = %principal_str,
            action = "iam:policy.attach",
            matched_policy_id = %policy_id,
            target = %pretty_target,
            "ATTACH POLICY applied"
        );
        self.inner.audit_log.record(
            "iam/policy.attach",
            &principal_str,
            &pretty_target,
            "ok",
            crate::json::Value::Null,
        );

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("policy `{policy_id}` attached to {pretty_target}"),
            "attach_policy",
        ))
    }

    fn execute_detach_policy(
        &self,
        query: &str,
        policy_id: &str,
        principal: &crate::storage::query::ast::PolicyPrincipalRef,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::store::PrincipalRef;
        use crate::auth::UserId;
        use crate::storage::query::ast::PolicyPrincipalRef;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;
        let p = match principal {
            PolicyPrincipalRef::User(u) => {
                PrincipalRef::User(UserId::from_parts(u.tenant.as_deref(), &u.username))
            }
            PolicyPrincipalRef::Group(g) => PrincipalRef::Group(g.clone()),
        };
        let pretty_target = principal_label(principal);
        auth_store
            .detach_policy(p, policy_id)
            .map_err(|e| RedDBError::Query(e.to_string()))?;

        let principal_str = current_auth_identity()
            .map(|(u, _)| u)
            .unwrap_or_else(|| "anonymous".into());
        tracing::info!(
            target: "audit",
            principal = %principal_str,
            action = "iam:policy.detach",
            matched_policy_id = %policy_id,
            target = %pretty_target,
            "DETACH POLICY applied"
        );
        self.inner.audit_log.record(
            "iam/policy.detach",
            &principal_str,
            &pretty_target,
            "ok",
            crate::json::Value::Null,
        );

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("policy `{policy_id}` detached from {pretty_target}"),
            "detach_policy",
        ))
    }

    fn execute_show_policies(
        &self,
        query: &str,
        filter: Option<&crate::storage::query::ast::PolicyPrincipalRef>,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::UserId;
        use crate::storage::query::ast::PolicyPrincipalRef;
        use crate::storage::query::unified::UnifiedRecord;
        use crate::storage::schema::Value as SchemaValue;
        use std::sync::Arc;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        let pols = match filter {
            None => auth_store.list_policies(),
            Some(PolicyPrincipalRef::User(u)) => {
                let id = UserId::from_parts(u.tenant.as_deref(), &u.username);
                auth_store.effective_policies(&id)
            }
            Some(PolicyPrincipalRef::Group(g)) => auth_store.group_policies(g),
        };

        let mut records = Vec::with_capacity(pols.len());
        for p in pols.iter() {
            let mut rec = UnifiedRecord::default();
            rec.set_arc(Arc::from("id"), SchemaValue::text(p.id.clone()));
            rec.set_arc(
                Arc::from("statements"),
                SchemaValue::Integer(p.statements.len() as i64),
            );
            rec.set_arc(
                Arc::from("tenant"),
                p.tenant
                    .as_deref()
                    .map(|t| SchemaValue::text(t.to_string()))
                    .unwrap_or(SchemaValue::Null),
            );
            rec.set_arc(Arc::from("json"), SchemaValue::text(p.to_json_string()));
            records.push(rec);
        }
        let mut result = crate::storage::query::unified::UnifiedResult::empty();
        result.records = records;
        Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "show_policies",
            engine: "iam-policies",
            result,
            affected_rows: 0,
            statement_type: "select",
        })
    }

    fn execute_show_effective_permissions(
        &self,
        query: &str,
        user: &crate::storage::query::ast::PolicyUserRef,
        resource: Option<&crate::storage::query::ast::PolicyResourceRef>,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::UserId;
        use crate::storage::query::unified::UnifiedRecord;
        use crate::storage::schema::Value as SchemaValue;
        use std::sync::Arc;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;
        let id = UserId::from_parts(user.tenant.as_deref(), &user.username);
        let pols = auth_store.effective_policies(&id);

        // Show one row per (policy, statement) tuple, plus any
        // resource-level filter passed by the caller.
        let mut records = Vec::new();
        for p in pols.iter() {
            for (idx, st) in p.statements.iter().enumerate() {
                if let Some(_r) = resource {
                    // Naive filter: render statement targets to strings
                    // and skip if no match. Conservative default = include
                    // (the simulator handles fine-grained matching).
                }
                let mut rec = UnifiedRecord::default();
                rec.set_arc(Arc::from("policy_id"), SchemaValue::text(p.id.clone()));
                rec.set_arc(
                    Arc::from("statement_index"),
                    SchemaValue::Integer(idx as i64),
                );
                rec.set_arc(
                    Arc::from("sid"),
                    st.sid
                        .as_deref()
                        .map(|s| SchemaValue::text(s.to_string()))
                        .unwrap_or(SchemaValue::Null),
                );
                rec.set_arc(
                    Arc::from("effect"),
                    SchemaValue::text(match st.effect {
                        crate::auth::policies::Effect::Allow => "allow",
                        crate::auth::policies::Effect::Deny => "deny",
                    }),
                );
                rec.set_arc(
                    Arc::from("actions"),
                    SchemaValue::Integer(st.actions.len() as i64),
                );
                rec.set_arc(
                    Arc::from("resources"),
                    SchemaValue::Integer(st.resources.len() as i64),
                );
                records.push(rec);
            }
        }
        let mut result = crate::storage::query::unified::UnifiedResult::empty();
        result.records = records;
        Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "show_effective_permissions",
            engine: "iam-policies",
            result,
            affected_rows: 0,
            statement_type: "select",
        })
    }

    fn execute_simulate_policy(
        &self,
        query: &str,
        user: &crate::storage::query::ast::PolicyUserRef,
        action: &str,
        resource: &crate::storage::query::ast::PolicyResourceRef,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::policies::ResourceRef;
        use crate::auth::store::SimCtx;
        use crate::auth::UserId;
        use crate::storage::query::unified::UnifiedRecord;
        use crate::storage::schema::Value as SchemaValue;
        use std::sync::Arc;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;
        let id = UserId::from_parts(user.tenant.as_deref(), &user.username);
        let r = ResourceRef::new(resource.kind.clone(), resource.name.clone());
        let outcome = auth_store.simulate(&id, action, &r, SimCtx::default());

        let principal_str = current_auth_identity()
            .map(|(u, _)| u)
            .unwrap_or_else(|| "anonymous".into());
        let (decision_str, matched_pid, matched_sid) = decision_to_strings(&outcome.decision);
        tracing::info!(
            target: "audit",
            principal = %principal_str,
            action = "iam:policy.simulate",
            decision = %decision_str,
            matched_policy_id = ?matched_pid,
            matched_sid = ?matched_sid,
            "SIMULATE issued"
        );
        self.inner.audit_log.record(
            "iam/policy.simulate",
            &principal_str,
            &id.to_string(),
            "ok",
            crate::json::Value::Null,
        );

        let mut rec = UnifiedRecord::default();
        rec.set_arc(Arc::from("decision"), SchemaValue::text(decision_str));
        rec.set_arc(
            Arc::from("matched_policy_id"),
            matched_pid
                .map(SchemaValue::text)
                .unwrap_or(SchemaValue::Null),
        );
        rec.set_arc(
            Arc::from("matched_sid"),
            matched_sid
                .map(SchemaValue::text)
                .unwrap_or(SchemaValue::Null),
        );
        rec.set_arc(Arc::from("reason"), SchemaValue::text(outcome.reason));
        rec.set_arc(
            Arc::from("trail_len"),
            SchemaValue::Integer(outcome.trail.len() as i64),
        );
        let mut result = crate::storage::query::unified::UnifiedResult::empty();
        result.records = vec![rec];
        Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "simulate_policy",
            engine: "iam-policies",
            result,
            affected_rows: 0,
            statement_type: "select",
        })
    }
}

/// Translate a parsed GRANT into a synthetic IAM policy whose id
/// starts with `_grant_<unique>`. PUBLIC is represented as an
/// implicit IAM group; legacy GROUP grants are still rejected by the
/// grant store and are not translated here.
fn grant_to_iam_policy(
    principal: &crate::auth::privileges::GrantPrincipal,
    resource: &crate::auth::privileges::Resource,
    actions: &[crate::auth::privileges::Action],
    tenant: Option<&str>,
) -> Option<crate::auth::policies::Policy> {
    use crate::auth::policies::{
        compile_action, ActionPattern, Effect, Policy, ResourcePattern, Statement,
    };
    use crate::auth::privileges::{Action, GrantPrincipal, Resource};

    if matches!(principal, GrantPrincipal::Group(_)) {
        return None;
    }

    let now = crate::auth::now_ms();
    let id = format!("_grant_{:x}_{:x}", now, std::process::id());

    let resource_str = match resource {
        Resource::Database => "table:*".to_string(),
        Resource::Schema(s) => format!("table:{s}.*"),
        Resource::Table { schema, table } => match schema {
            Some(s) => format!("table:{s}.{table}"),
            None => format!("table:{table}"),
        },
        Resource::Function { schema, name } => match schema {
            Some(s) => format!("function:{s}.{name}"),
            None => format!("function:{name}"),
        },
    };

    // Compile actions — fall back to `*` only when the grant included
    // `Action::All`. Map every other action keyword to its lowercase
    // form so it lines up with the kernel's allowlist.
    let action_patterns: Vec<ActionPattern> = if actions.contains(&Action::All) {
        vec![ActionPattern::Wildcard]
    } else {
        actions
            .iter()
            .map(|a| compile_action(&a.as_str().to_ascii_lowercase()))
            .collect()
    };
    if action_patterns.is_empty() {
        return None;
    }

    // Inline resource compilation matching the kernel's `compile_resource`:
    //   * `*` → wildcard
    //   * contains `*` → glob
    //   * `kind:name` → exact
    let resource_patterns = if resource_str == "*" {
        vec![ResourcePattern::Wildcard]
    } else if resource_str.contains('*') {
        vec![ResourcePattern::Glob(resource_str.clone())]
    } else if let Some((kind, name)) = resource_str.split_once(':') {
        vec![ResourcePattern::Exact {
            kind: kind.to_string(),
            name: name.to_string(),
        }]
    } else {
        vec![ResourcePattern::Wildcard]
    };

    let policy = Policy {
        id,
        version: 1,
        tenant: tenant.map(|t| t.to_string()),
        created_at: now,
        updated_at: now,
        statements: vec![Statement {
            sid: None,
            effect: Effect::Allow,
            actions: action_patterns,
            resources: resource_patterns,
            condition: None,
        }],
    };
    if policy.validate().is_err() {
        return None;
    }
    Some(policy)
}

fn legacy_action_to_iam(action: crate::auth::privileges::Action) -> &'static str {
    use crate::auth::privileges::Action;
    match action {
        Action::Select => "select",
        Action::Insert => "insert",
        Action::Update => "update",
        Action::Delete => "delete",
        Action::Truncate => "truncate",
        Action::References => "references",
        Action::Execute => "execute",
        Action::Usage => "usage",
        Action::All => "*",
    }
}

fn update_set_target_columns(query: &crate::storage::query::ast::UpdateQuery) -> Vec<String> {
    let mut columns = Vec::new();
    for (column, _) in &query.assignment_exprs {
        if !columns.iter().any(|seen| seen == column) {
            columns.push(column.clone());
        }
    }
    columns
}

fn column_access_request_for_table_update(
    table_name: &str,
    columns: Vec<String>,
) -> crate::auth::ColumnAccessRequest {
    match table_name.split_once('.') {
        Some((schema, table)) => {
            crate::auth::ColumnAccessRequest::update(table.to_string(), columns)
                .with_schema(schema.to_string())
        }
        None => crate::auth::ColumnAccessRequest::update(table_name.to_string(), columns),
    }
}

fn requested_table_columns_for_policy(
    table: &crate::storage::query::ast::TableQuery,
) -> Vec<String> {
    use crate::storage::query::sql_lowering::{
        effective_table_filter, effective_table_group_by_exprs, effective_table_having_filter,
        effective_table_projections,
    };

    let table_name = table.table.as_str();
    let table_alias = table.alias.as_deref();
    let mut columns = std::collections::BTreeSet::new();

    for projection in effective_table_projections(table) {
        collect_projection_columns(&projection, table_name, table_alias, &mut columns);
    }
    if let Some(filter) = effective_table_filter(table) {
        collect_filter_columns(&filter, table_name, table_alias, &mut columns);
    }
    for expr in effective_table_group_by_exprs(table) {
        collect_expr_columns(&expr, table_name, table_alias, &mut columns);
    }
    if let Some(filter) = effective_table_having_filter(table) {
        collect_filter_columns(&filter, table_name, table_alias, &mut columns);
    }
    for order in &table.order_by {
        if let Some(expr) = order.expr.as_ref() {
            collect_expr_columns(expr, table_name, table_alias, &mut columns);
        } else {
            collect_field_ref_column(&order.field, table_name, table_alias, &mut columns);
        }
    }

    columns.into_iter().collect()
}

fn collect_projection_columns(
    projection: &crate::storage::query::ast::Projection,
    table_name: &str,
    table_alias: Option<&str>,
    columns: &mut std::collections::BTreeSet<String>,
) {
    use crate::storage::query::ast::Projection;
    match projection {
        Projection::All => {
            columns.insert("*".to_string());
        }
        Projection::Column(column) | Projection::Alias(column, _) => {
            if column != "*" {
                columns.insert(column.clone());
            }
        }
        Projection::Function(_, args) => {
            for arg in args {
                collect_projection_columns(arg, table_name, table_alias, columns);
            }
        }
        Projection::Expression(filter, _) => {
            collect_filter_columns(filter, table_name, table_alias, columns);
        }
        Projection::Field(field, _) => {
            collect_field_ref_column(field, table_name, table_alias, columns);
        }
    }
}

fn collect_filter_columns(
    filter: &crate::storage::query::ast::Filter,
    table_name: &str,
    table_alias: Option<&str>,
    columns: &mut std::collections::BTreeSet<String>,
) {
    use crate::storage::query::ast::Filter;
    match filter {
        Filter::Compare { field, .. }
        | Filter::IsNull(field)
        | Filter::IsNotNull(field)
        | Filter::In { field, .. }
        | Filter::Between { field, .. }
        | Filter::Like { field, .. }
        | Filter::StartsWith { field, .. }
        | Filter::EndsWith { field, .. }
        | Filter::Contains { field, .. } => {
            collect_field_ref_column(field, table_name, table_alias, columns);
        }
        Filter::CompareFields { left, right, .. } => {
            collect_field_ref_column(left, table_name, table_alias, columns);
            collect_field_ref_column(right, table_name, table_alias, columns);
        }
        Filter::CompareExpr { lhs, rhs, .. } => {
            collect_expr_columns(lhs, table_name, table_alias, columns);
            collect_expr_columns(rhs, table_name, table_alias, columns);
        }
        Filter::And(left, right) | Filter::Or(left, right) => {
            collect_filter_columns(left, table_name, table_alias, columns);
            collect_filter_columns(right, table_name, table_alias, columns);
        }
        Filter::Not(inner) => collect_filter_columns(inner, table_name, table_alias, columns),
    }
}

fn collect_expr_columns(
    expr: &crate::storage::query::ast::Expr,
    table_name: &str,
    table_alias: Option<&str>,
    columns: &mut std::collections::BTreeSet<String>,
) {
    use crate::storage::query::ast::Expr;
    match expr {
        Expr::Column { field, .. } => {
            collect_field_ref_column(field, table_name, table_alias, columns);
        }
        Expr::Literal { .. } | Expr::Parameter { .. } => {}
        Expr::UnaryOp { operand, .. } | Expr::Cast { inner: operand, .. } => {
            collect_expr_columns(operand, table_name, table_alias, columns);
        }
        Expr::BinaryOp { lhs, rhs, .. } => {
            collect_expr_columns(lhs, table_name, table_alias, columns);
            collect_expr_columns(rhs, table_name, table_alias, columns);
        }
        Expr::FunctionCall { args, .. } => {
            for arg in args {
                collect_expr_columns(arg, table_name, table_alias, columns);
            }
        }
        Expr::Case {
            branches, else_, ..
        } => {
            for (condition, value) in branches {
                collect_expr_columns(condition, table_name, table_alias, columns);
                collect_expr_columns(value, table_name, table_alias, columns);
            }
            if let Some(value) = else_ {
                collect_expr_columns(value, table_name, table_alias, columns);
            }
        }
        Expr::IsNull { operand, .. } => {
            collect_expr_columns(operand, table_name, table_alias, columns);
        }
        Expr::InList { target, values, .. } => {
            collect_expr_columns(target, table_name, table_alias, columns);
            for value in values {
                collect_expr_columns(value, table_name, table_alias, columns);
            }
        }
        Expr::Between {
            target, low, high, ..
        } => {
            collect_expr_columns(target, table_name, table_alias, columns);
            collect_expr_columns(low, table_name, table_alias, columns);
            collect_expr_columns(high, table_name, table_alias, columns);
        }
    }
}

fn collect_field_ref_column(
    field: &crate::storage::query::ast::FieldRef,
    table_name: &str,
    table_alias: Option<&str>,
    columns: &mut std::collections::BTreeSet<String>,
) {
    if let Some(column) = policy_column_name_from_field_ref(field, table_name, table_alias) {
        if column != "*" {
            columns.insert(column);
        }
    }
}

fn policy_column_name_from_field_ref(
    field: &crate::storage::query::ast::FieldRef,
    table_name: &str,
    table_alias: Option<&str>,
) -> Option<String> {
    match field {
        crate::storage::query::ast::FieldRef::TableColumn { table, column } => {
            if column == "*" {
                return Some("*".to_string());
            }
            if table.is_empty() || table == table_name || Some(table.as_str()) == table_alias {
                Some(column.clone())
            } else {
                Some(format!("{table}.{column}"))
            }
        }
        _ => None,
    }
}

fn legacy_resource_to_iam(
    resource: &crate::auth::privileges::Resource,
    tenant: Option<&str>,
) -> crate::auth::policies::ResourceRef {
    use crate::auth::privileges::Resource;

    let (kind, name) = match resource {
        Resource::Database => ("database".to_string(), "*".to_string()),
        Resource::Schema(s) => ("schema".to_string(), format!("{s}.*")),
        Resource::Table { schema, table } => (
            "table".to_string(),
            match schema {
                Some(s) => format!("{s}.{table}"),
                None => table.clone(),
            },
        ),
        Resource::Function { schema, name } => (
            "function".to_string(),
            match schema {
                Some(s) => format!("{s}.{name}"),
                None => name.clone(),
            },
        ),
    };

    let mut out = crate::auth::policies::ResourceRef::new(kind, name);
    if let Some(t) = tenant {
        out = out.with_tenant(t.to_string());
    }
    out
}

#[derive(Debug)]
struct JoinTableSide {
    table: String,
    alias: String,
}

fn table_side_context(expr: &QueryExpr) -> Option<JoinTableSide> {
    match expr {
        QueryExpr::Table(table) => Some(JoinTableSide {
            table: table.table.clone(),
            alias: table.alias.clone().unwrap_or_else(|| table.table.clone()),
        }),
        _ => None,
    }
}

fn collect_projection_columns_for_table(
    projection: &Projection,
    table: &str,
    alias: Option<&str>,
    out: &mut BTreeSet<String>,
) {
    match projection {
        Projection::Column(column) | Projection::Alias(column, _) => {
            match split_qualified_column(column) {
                Some((qualifier, column))
                    if qualifier == table || alias.is_some_and(|alias| qualifier == alias) =>
                {
                    push_policy_column(column, out);
                }
                Some(_) => {}
                None => push_policy_column(column, out),
            }
        }
        Projection::Field(
            FieldRef::TableColumn {
                table: qualifier,
                column,
            },
            _,
        ) => {
            if qualifier.is_empty()
                || qualifier == table
                || alias.is_some_and(|alias| qualifier == alias)
            {
                push_policy_column(column, out);
            }
        }
        Projection::Field(
            FieldRef::NodeProperty {
                alias: qualifier,
                property,
            },
            _,
        )
        | Projection::Field(
            FieldRef::EdgeProperty {
                alias: qualifier,
                property,
            },
            _,
        ) => {
            if qualifier == table || alias.is_some_and(|alias| qualifier == alias) {
                push_policy_column(property, out);
            }
        }
        Projection::Function(_, args) => {
            for arg in args {
                collect_projection_columns_for_table(arg, table, alias, out);
            }
        }
        Projection::Expression(_, _) | Projection::All | Projection::Field(_, _) => {}
    }
}

fn collect_projection_columns_for_join_side(
    projection: &Projection,
    left: Option<&JoinTableSide>,
    right: Option<&JoinTableSide>,
    out: &mut HashMap<String, BTreeSet<String>>,
) -> RedDBResult<()> {
    match projection {
        Projection::Column(column) | Projection::Alias(column, _) => {
            if let Some((qualifier, column)) = split_qualified_column(column) {
                push_qualified_join_column(qualifier, column, left, right, out);
            } else {
                push_unqualified_join_column(column, left, right, out);
            }
        }
        Projection::Field(FieldRef::TableColumn { table, column }, _) => {
            if table.is_empty() {
                push_unqualified_join_column(column, left, right, out);
            } else if let Some(side) = [left, right]
                .into_iter()
                .flatten()
                .find(|side| table == side.table.as_str() || table == side.alias.as_str())
            {
                push_join_column(&side.table, column, out);
            }
        }
        Projection::Field(FieldRef::NodeProperty { alias, property }, _)
        | Projection::Field(FieldRef::EdgeProperty { alias, property }, _) => {
            push_qualified_join_column(alias, property, left, right, out);
        }
        Projection::Function(_, args) => {
            for arg in args {
                collect_projection_columns_for_join_side(arg, left, right, out)?;
            }
        }
        Projection::Expression(_, _) | Projection::All | Projection::Field(_, _) => {}
    }
    Ok(())
}

fn split_qualified_column(column: &str) -> Option<(&str, &str)> {
    let (qualifier, column) = column.split_once('.')?;
    if qualifier.is_empty() || column.is_empty() || column.contains('.') {
        return None;
    }
    Some((qualifier, column))
}

fn push_qualified_join_column(
    qualifier: &str,
    column: &str,
    left: Option<&JoinTableSide>,
    right: Option<&JoinTableSide>,
    out: &mut HashMap<String, BTreeSet<String>>,
) {
    if let Some(side) = [left, right]
        .into_iter()
        .flatten()
        .find(|side| qualifier == side.table.as_str() || qualifier == side.alias.as_str())
    {
        push_join_column(&side.table, column, out);
    }
}

fn push_unqualified_join_column(
    column: &str,
    left: Option<&JoinTableSide>,
    right: Option<&JoinTableSide>,
    out: &mut HashMap<String, BTreeSet<String>>,
) {
    for side in [left, right].into_iter().flatten() {
        push_join_column(&side.table, column, out);
    }
}

fn push_join_column(table: &str, column: &str, out: &mut HashMap<String, BTreeSet<String>>) {
    if is_policy_column_name(column) {
        out.entry(table.to_string())
            .or_default()
            .insert(column.to_string());
    }
}

fn push_policy_column(column: &str, out: &mut BTreeSet<String>) {
    if is_policy_column_name(column) {
        out.insert(column.to_string());
    }
}

fn is_policy_column_name(column: &str) -> bool {
    !column.is_empty()
        && column != "*"
        && !column.starts_with("LIT:")
        && !column.starts_with("TYPE:")
}

fn runtime_iam_context(
    role: crate::auth::Role,
    tenant: Option<&str>,
) -> crate::auth::policies::EvalContext {
    crate::auth::policies::EvalContext {
        principal_tenant: tenant.map(|t| t.to_string()),
        current_tenant: tenant.map(|t| t.to_string()),
        peer_ip: None,
        mfa_present: false,
        now_ms: crate::auth::now_ms(),
        principal_is_admin_role: role == crate::auth::Role::Admin,
    }
}

fn explicit_table_projection_columns(
    query: &crate::storage::query::ast::TableQuery,
) -> Vec<String> {
    use crate::storage::query::ast::{FieldRef, Projection};

    let mut columns = Vec::new();
    for projection in crate::storage::query::sql_lowering::effective_table_projections(query) {
        match projection {
            Projection::Column(column) | Projection::Alias(column, _) => {
                push_unique(&mut columns, column)
            }
            Projection::Field(FieldRef::TableColumn { column, .. }, _) => {
                push_unique(&mut columns, column)
            }
            // SELECT * and expression/function projections need the
            // executor-wide column-policy context mapped in
            // docs/security/select-relational-column-policy-audit-2026-05-08.md.
            _ => {}
        }
    }
    columns
}

fn explicit_graph_projection_properties(
    query: &crate::storage::query::ast::GraphQuery,
) -> Vec<String> {
    use crate::storage::query::ast::{FieldRef, Projection};

    let mut columns = Vec::new();
    for projection in &query.return_ {
        match projection {
            Projection::Field(FieldRef::NodeProperty { property, .. }, _)
            | Projection::Field(FieldRef::EdgeProperty { property, .. }, _) => {
                push_unique(&mut columns, property.clone())
            }
            _ => {}
        }
    }
    columns
}

fn push_unique(columns: &mut Vec<String>, column: String) {
    if !columns.iter().any(|existing| existing == &column) {
        columns.push(column);
    }
}

fn principal_label(p: &crate::storage::query::ast::PolicyPrincipalRef) -> String {
    use crate::storage::query::ast::PolicyPrincipalRef;
    match p {
        PolicyPrincipalRef::User(u) => match &u.tenant {
            Some(t) => format!("user:{t}/{}", u.username),
            None => format!("user:{}", u.username),
        },
        PolicyPrincipalRef::Group(g) => format!("group:{g}"),
    }
}

/// Render a `Decision` into the (decision, matched_policy_id, matched_sid)
/// shape used by every audit emit + the simulator response.
pub(crate) fn decision_to_strings(
    d: &crate::auth::policies::Decision,
) -> (String, Option<String>, Option<String>) {
    use crate::auth::policies::Decision;
    match d {
        Decision::Allow {
            matched_policy_id,
            matched_sid,
        } => (
            "allow".into(),
            Some(matched_policy_id.clone()),
            matched_sid.clone(),
        ),
        Decision::Deny {
            matched_policy_id,
            matched_sid,
        } => (
            "deny".into(),
            Some(matched_policy_id.clone()),
            matched_sid.clone(),
        ),
        Decision::DefaultDeny => ("default_deny".into(), None, None),
        Decision::AdminBypass => ("admin_bypass".into(), None, None),
    }
}

fn parse_timestamp_to_ms(s: &str) -> Option<u128> {
    // Bare integer ms.
    if let Ok(n) = s.parse::<u128>() {
        return Some(n);
    }
    // Fallback: ISO-8601 like 2030-01-02 03:04:05 — accept the date
    // portion only (midnight UTC). Full RFC3339 parsing is a stretch
    // goal; the common case is `'2030-01-01'`.
    if let Some(date) = s.split_whitespace().next() {
        let parts: Vec<&str> = date.split('-').collect();
        if parts.len() == 3 {
            let (y, m, d) = (parts[0], parts[1], parts[2]);
            if let (Ok(y), Ok(m), Ok(d)) = (y.parse::<i64>(), m.parse::<u32>(), d.parse::<u32>()) {
                // Days since 1970-01-01 — simple Julian arithmetic
                // suitable for years 1970-2100. Good enough for test
                // fixtures; precise parsing lands when we wire chrono.
                let days_in = days_from_civil(y, m, d);
                return Some((days_in as u128) * 86_400_000u128);
            }
        }
    }
    None
}

/// Days from Unix epoch using H. Hinnant's civil-from-days algorithm.
/// Robust for the entire Gregorian range; used by `parse_timestamp_to_ms`.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as u64 + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i64 - 719468
}

fn walk_plan_node(
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    depth: usize,
    out: &mut Vec<crate::storage::query::unified::UnifiedRecord>,
) {
    use std::sync::Arc;
    let mut rec = crate::storage::query::unified::UnifiedRecord::default();
    rec.set_arc(Arc::from("op"), Value::text(node.operator.clone()));
    rec.set_arc(
        Arc::from("source"),
        node.source.clone().map(Value::text).unwrap_or(Value::Null),
    );
    rec.set_arc(Arc::from("est_rows"), Value::Float(node.estimated_rows));
    rec.set_arc(Arc::from("est_cost"), Value::Float(node.operator_cost));
    rec.set_arc(Arc::from("depth"), Value::Integer(depth as i64));
    out.push(rec);
    for child in &node.children {
        walk_plan_node(child, depth + 1, out);
    }
}
