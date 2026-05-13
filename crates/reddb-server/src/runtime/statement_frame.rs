use std::collections::HashSet;
use std::sync::Arc;

use super::impl_core::{
    collections_referenced, current_auth_identity, current_connection_id, current_tenant,
    has_with_prefix, intent_lock_modes_for, peek_top_level_as_of_with_table,
    query_has_volatile_builtin, ConfigSnapshotGuard, CurrentSnapshotGuard, SecretStoreGuard,
    SnapshotContext, TxLocalTenantGuard,
};
use super::{RedDBRuntime, RuntimeQueryResult, RuntimeResultCacheEntry};
use crate::api::{RedDBError, RedDBResult};
use crate::auth::Role;
use crate::storage::query::ast::QueryExpr;
use crate::storage::query::modes::{detect_mode, parse_multi, QueryMode};
use crate::storage::transaction::snapshot::{Snapshot, Xid};

/// Coarse privilege classification for a statement, computed once at
/// frame-build time from the SQL text. Mirrors the three-role auth
/// model (`Role::Read < Role::Write < Role::Admin`) so the frame can
/// answer "can this identity run this statement?" without re-walking
/// the parsed `QueryExpr` at every call site.
///
/// `None` means the statement does not touch the privilege gate at
/// all (transaction control, SET, SHOW). Such statements must remain
/// runnable under any authenticated identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Privilege {
    /// Read-only data access (SELECT, EXPLAIN, SHOW). Satisfied by
    /// any role from `Role::Read` upward.
    Read,
    /// Mutation of user data or schema author DDL (INSERT, UPDATE,
    /// DELETE, CREATE/ALTER/DROP TABLE, CREATE MIGRATION). Requires
    /// at least `Role::Write`.
    Write,
    /// Authority statements — GRANT, REVOKE, ALTER USER, APPLY /
    /// ROLLBACK MIGRATION, IAM policy mutation. Requires `Role::Admin`.
    Admin,
    /// Statement does not consult the privilege gate (BEGIN, COMMIT,
    /// ROLLBACK, SET, SHOW with no data exposure). Always permitted
    /// for any authenticated identity.
    None,
}

impl Privilege {
    /// `true` iff `role` is sufficient to execute a statement carrying
    /// this required privilege. Encodes the standard `Read ⊆ Write ⊆
    /// Admin` containment used by the auth fallback path.
    pub(crate) fn is_satisfied_by(self, role: Role) -> bool {
        match self {
            Self::None => true,
            Self::Read => role.can_read(),
            Self::Write => role.can_write(),
            Self::Admin => role.can_admin(),
        }
    }
}

/// Coarse lock intent for a statement, computed once at frame-build
/// time. Maps onto the storage-layer's `LockMode` matrix downstream
/// but stays decoupled here so the runtime can answer "does this
/// statement need the lock manager at all?" without a `use storage::`
/// at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockIntent {
    /// No collection-level lock needed (transaction control, SET,
    /// SHOW, EXPLAIN). The lock-acquisition path can short-circuit.
    None,
    /// Reader-style intent: SELECT, joins, graph / queue / search
    /// reads. Maps to `(IS, IS)` at the storage layer.
    Shared,
    /// Writer- or DDL-style intent: INSERT/UPDATE/DELETE (`(IX, IX)`)
    /// and CREATE/ALTER/DROP (`(IX, X)`). Both are surfaced as
    /// `Exclusive` at this granularity — call sites that need the
    /// finer distinction still consult `intent_lock_modes_for`.
    Exclusive,
}

/// Small, stable Interface that *represents* a read statement's
/// execution context. Every read caller that needs to know "under
/// what scope / identity / snapshot am I running, and is there an
/// AS OF floor in effect?" consults this trait — never the
/// underlying thread-locals or runtime fields directly.
///
/// The deletion test: removing this trait would force the four
/// concerns it exposes back into ad-hoc lookups at every read
/// callsite (`current_tenant()`, `current_auth_identity()`,
/// `capture_current_snapshot()`, AS OF re-parsing). The trait
/// concentrates them in one place so future changes (per-statement
/// logging, audit, scope policy) have a single seam to extend.
pub(crate) trait ReadFrame {
    /// Effective tenant scope for the statement after WITHIN /
    /// SET LOCAL TENANT / SET TENANT resolution. `None` means
    /// "no tenant bound" (RLS deny-default applies).
    fn effective_scope(&self) -> Option<&str>;

    /// Authenticated identity observed at frame-build time, if any.
    /// Returns `(username, role)` so callers can render audit lines
    /// or feed RLS policy lookups without re-reading thread-locals.
    fn identity(&self) -> Option<(&str, Role)>;

    /// MVCC snapshot the statement reads against. For autocommit
    /// this is a fresh snapshot; inside an active transaction it
    /// is the txn's snapshot; under AS OF it is the resolved
    /// historical xid.
    fn snapshot(&self) -> &Snapshot;

    /// AS OF xid floor when AS OF was applied for this statement,
    /// `None` for live reads. Useful for downstream callers that
    /// want to gate behaviour on historical-read mode without
    /// re-parsing the query.
    fn as_of_floor(&self) -> Option<Xid>;

    /// Stable result-cache key for the statement (already mixes
    /// effective tenant + identity).
    fn cache_key(&self) -> &str;

    /// Whether the statement is safe to serve from / populate the
    /// result cache. Combines two underlying signals:
    ///
    ///   * the query does not call a volatile builtin (e.g. `NOW()`,
    ///     `RANDOM()`, `UUID()`), which would change between calls,
    ///   * the connection is not inside an active transaction with
    ///     uncommitted writes that other readers shouldn't observe.
    ///
    /// SELECT cache callsites (read + write) consult this method
    /// instead of re-deriving safety from globals or poking the
    /// frame's private fields. Removing it would force every cache
    /// callsite to re-run `query_has_volatile_builtin` plus
    /// `result_cache_safe(conn_id)` inline.
    fn should_cache_result(&self) -> bool;

    /// Coarse privilege class the statement requires, computed once
    /// at frame-build time from the SQL prefix. Read/write dispatch
    /// sites consult this instead of re-classifying the parsed
    /// `QueryExpr` inline at every callsite.
    ///
    /// Removing this method would force every privilege gate to
    /// recompute the (action, resource) classification from the
    /// parsed expression and re-check the role hierarchy inline.
    fn required_privilege(&self) -> Privilege;

    /// Coarse collection-level lock intent the statement implies.
    /// `None` lets the lock-acquisition path short-circuit without
    /// touching the lock manager.
    ///
    /// Removing this method would force the lock-acquisition path
    /// to always invoke `intent_lock_modes_for` (which itself walks
    /// the parsed expression) even for transaction-control / SET /
    /// SHOW statements that need no collection lock at all.
    fn lock_intent(&self) -> LockIntent;

    /// Set of collection ids the calling identity is allowed to
    /// observe under the active `(tenant, role)` scope. Computed once
    /// at frame-build time via the `AuthStore` visible-collections
    /// cache (see `auth::scope_cache`) and used by `AuthorizedSearch`
    /// to pre-filter SEARCH SIMILAR / SEARCH CONTEXT candidate sets
    /// before any similarity score is computed (issue #119).
    ///
    /// `None` means the frame was built without an auth store wired —
    /// embedded / single-tenant tests run that way. AI search call
    /// sites refuse to proceed with `None`, which is the deny-default
    /// the issue requires; pure SELECT paths fall back to the existing
    /// per-row RLS gate.
    fn visible_collections(&self) -> Option<&std::collections::HashSet<String>>;
}

/// Cheap first-word classification of a SQL statement, used at
/// frame-build time to derive `Privilege` + `LockIntent` without
/// re-parsing the query. Matches the keywords that the legacy
/// inline checks in `RedDBRuntime::check_query_privilege` and
/// `intent_lock_modes_for` already key on.
fn statement_kind(query: &str) -> &'static str {
    let trimmed = query.trim_start();
    // Skip a leading line / block comment so the classifier doesn't
    // misread `/* ... */ SELECT ...` as an unknown statement.
    let trimmed = if let Some(rest) = trimmed.strip_prefix("--") {
        rest.split_once('\n')
            .map(|(_, r)| r)
            .unwrap_or("")
            .trim_start()
    } else {
        trimmed
    };
    let first = trimmed
        .split(|c: char| c.is_whitespace() || c == '(' || c == ';')
        .next()
        .unwrap_or("");
    // ASCII-uppercase compare without allocating: SQL keywords are ASCII.
    let mut buf = [0u8; 16];
    let bytes = first.as_bytes();
    let n = bytes.len().min(buf.len());
    for i in 0..n {
        buf[i] = bytes[i].to_ascii_uppercase();
    }
    match &buf[..n] {
        b"SELECT" | b"WITH" | b"SHOW" | b"EXPLAIN" | b"DESCRIBE" | b"DESC" => "read",
        b"INSERT" | b"UPDATE" | b"DELETE" | b"UPSERT" | b"MERGE" | b"COPY" | b"TRUNCATE" => "write",
        b"CREATE" | b"ALTER" | b"DROP" | b"REINDEX" | b"VACUUM" | b"ANALYZE" => "ddl",
        b"GRANT" | b"REVOKE" => "admin",
        b"BEGIN" | b"START" | b"COMMIT" | b"ROLLBACK" | b"SAVEPOINT" | b"RELEASE" | b"END"
        | b"SET" | b"RESET" | b"PREPARE" | b"EXECUTE" | b"DEALLOCATE" | b"USE" => "control",
        _ => "unknown",
    }
}

fn classify_privilege(query: &str) -> Privilege {
    match statement_kind(query) {
        "read" => Privilege::Read,
        "write" => Privilege::Write,
        // DDL is gated at `Role::Write` in the legacy fallback (see
        // `RedDBRuntime::check_query_privilege` for CreateTable et al.),
        // so it classifies as Write here. APPLY / ROLLBACK MIGRATION and
        // GRANT / REVOKE upgrade to Admin via finer checks at the call
        // site — the frame surfaces only the coarse class.
        "ddl" => Privilege::Write,
        "admin" => Privilege::Admin,
        _ => Privilege::None,
    }
}

fn classify_lock_intent(query: &str) -> LockIntent {
    match statement_kind(query) {
        "read" => LockIntent::Shared,
        "write" | "ddl" => LockIntent::Exclusive,
        _ => LockIntent::None,
    }
}

pub(super) struct StatementExecutionFrame {
    tx_local_tenant: Option<Option<String>>,
    snapshot: Snapshot,
    own_xids: HashSet<Xid>,
    cache_key: String,
    is_volatile_query: bool,
    cache_safe: bool,
    /// Effective tenant captured at frame-build time after WITHIN /
    /// SET LOCAL TENANT / SET TENANT resolution. Stored on the frame
    /// so the `ReadFrame` Interface can return a borrow without
    /// re-touching the thread-local stack.
    effective_scope: Option<String>,
    /// Auth identity captured at frame-build time. `None` for
    /// embedded / anonymous callers.
    identity: Option<(String, Role)>,
    /// `Some(xid)` when AS OF resolved to a historical xid; `None`
    /// for live reads.
    as_of_floor: Option<Xid>,
    /// Privilege class required by the statement, derived from the
    /// SQL text at frame-build time. Read/write dispatch sites
    /// consult this instead of re-classifying the parsed expression.
    required_privilege: Privilege,
    /// Collection-level lock intent the statement implies. The
    /// lock-acquisition path short-circuits when this is `None`.
    lock_intent: LockIntent,
    /// Set of collection ids the active `(tenant, role)` scope is
    /// allowed to observe. Computed at frame-build time via the
    /// `AuthStore` visibility cache and consumed by `AuthorizedSearch`
    /// to gate SEARCH SIMILAR / SEARCH CONTEXT candidate sets before
    /// scoring (issue #119). `None` when no auth store is wired
    /// (embedded test mode) — AI search refuses on `None`.
    visible_collections: Option<HashSet<String>>,
}

pub(super) struct StatementFrameGuards {
    _tx_local_guard: TxLocalTenantGuard,
    _config_snapshot_guard: ConfigSnapshotGuard,
    _secret_store_guard: SecretStoreGuard,
    _snapshot_guard: CurrentSnapshotGuard,
}

pub(super) struct PreparedStatement {
    pub(super) expr: QueryExpr,
    pub(super) mode: QueryMode,
}

impl StatementExecutionFrame {
    pub(super) fn build(runtime: &RedDBRuntime, query: &str) -> RedDBResult<Self> {
        let conn_id = current_connection_id();
        let tx_local_tenant = runtime.inner.tx_local_tenants.read().get(&conn_id).cloned();
        let own_xids = runtime.own_transaction_xids(conn_id);
        let (snapshot, as_of_floor) = runtime.statement_snapshot(query)?;
        let cache_key = result_cache_key(query);
        let is_volatile_query = query_has_volatile_builtin(query);
        let cache_safe = runtime.result_cache_safe(conn_id);
        // Capture identity + effective scope under the same
        // thread-local view that the cache key was built from, so
        // the Interface and the cache key agree on what "this
        // statement" means.
        let effective_scope = current_tenant();
        let identity = current_auth_identity();

        // Coarse classification of the statement, computed once from
        // the SQL prefix so downstream callers don't re-derive it
        // from the parsed `QueryExpr` at every privilege / lock site.
        let required_privilege = classify_privilege(query);
        let lock_intent = classify_lock_intent(query);

        // Issue #119: resolve the visible-collections set for the
        // active (tenant, role) scope. Only meaningful when an auth
        // store is wired *and* an identity was captured — embedded
        // anonymous callers fall back to `None`, and AI search call
        // sites refuse on `None`.
        let visible_collections = match (runtime.inner.auth_store.read().clone(), identity.as_ref())
        {
            (Some(store), Some((principal, role))) => {
                let collections = runtime.inner.db.store().list_collections();
                Some(store.visible_collections_for_scope(
                    effective_scope.as_deref(),
                    *role,
                    principal,
                    &collections,
                ))
            }
            _ => None,
        };

        Ok(Self {
            tx_local_tenant,
            snapshot,
            own_xids,
            cache_key,
            is_volatile_query,
            cache_safe,
            effective_scope,
            identity,
            as_of_floor,
            required_privilege,
            lock_intent,
            visible_collections,
        })
    }

    pub(super) fn install(&self, runtime: &RedDBRuntime) -> StatementFrameGuards {
        StatementFrameGuards {
            _tx_local_guard: TxLocalTenantGuard::install(self.tx_local_tenant.clone()),
            _config_snapshot_guard: ConfigSnapshotGuard::install(Arc::clone(&runtime.inner.db)),
            _secret_store_guard: SecretStoreGuard::install(runtime.inner.auth_store.read().clone()),
            _snapshot_guard: CurrentSnapshotGuard::install(SnapshotContext {
                snapshot: self.snapshot.clone(),
                manager: Arc::clone(&runtime.inner.snapshot_manager),
                own_xids: self.own_xids.clone(),
            }),
        }
    }

    pub(super) fn cache_key(&self) -> &str {
        &self.cache_key
    }

    pub(super) fn can_read_result_cache(&self) -> bool {
        // Delegates to the `ReadFrame` Interface so the volatile +
        // active-tx safety decision lives in exactly one place.
        <Self as ReadFrame>::should_cache_result(self)
    }

    pub(super) fn should_write_result_cache(&self, result: &RuntimeQueryResult) -> bool {
        // Cache-safety (volatile builtin, active-tx writes) comes from
        // the Interface; the rest are write-side payload heuristics
        // (statement shape, result size) that aren't part of the
        // safety contract.
        <Self as ReadFrame>::should_cache_result(self)
            && result.statement_type == "select"
            && result.engine != "vault"
            && result.result.pre_serialized_json.is_none()
            && result.result.records.len() <= 5
    }

    pub(super) fn read_result_cache(&self, runtime: &RedDBRuntime) -> Option<RuntimeQueryResult> {
        if self.can_read_result_cache() {
            runtime.get_result_cache_entry(self.cache_key())
        } else {
            None
        }
    }

    pub(super) fn write_result_cache(
        &self,
        runtime: &RedDBRuntime,
        result: &RuntimeQueryResult,
        scopes: HashSet<String>,
    ) {
        if self.should_write_result_cache(result) {
            runtime.put_result_cache_entry(
                self.cache_key(),
                RuntimeResultCacheEntry {
                    result: result.clone(),
                    cached_at: std::time::Instant::now(),
                    scopes,
                },
            );
        }
    }

    pub(super) fn prepare_cte(&self, query: &str) -> RedDBResult<Option<QueryExpr>> {
        // Detected via cheap prefix check so non-CTE queries skip the
        // full parse here. CTE-bearing queries bypass the plan cache
        // and result cache (rare workload — perf optimization is a
        // follow-up). Inlining substitutes every CTE reference with
        // its body as a subquery in FROM, after which the existing
        // subquery-in-FROM machinery handles execution. Recursive
        // CTEs are rejected explicitly until fixpoint execution wires
        // through the runtime.
        if !has_with_prefix(query) {
            return Ok(None);
        }
        let parsed = crate::storage::query::parser::parse(query)
            .map_err(|err| RedDBError::Query(err.to_string()))?;
        if parsed.with_clause.is_some() {
            let rewritten = crate::storage::query::executors::inline_ctes(parsed)
                .map_err(|err| RedDBError::Query(err.to_string()))?;
            return Ok(Some(rewritten));
        }
        // No WITH after parse (the prefix matched something else like
        // `WITHIN` that already routed elsewhere) — fall through to
        // the normal path with the original query.
        Ok(None)
    }

    pub(super) fn prepare_statement(
        &self,
        runtime: &RedDBRuntime,
        query: &str,
    ) -> RedDBResult<PreparedStatement> {
        let mode = detect_mode(query);
        if matches!(mode, QueryMode::Unknown) {
            return Err(RedDBError::Query("unable to detect query mode".to_string()));
        }

        // ── Plan cache: reuse only exact-query ASTs ──
        //
        // DML statements (INSERT/UPDATE/DELETE) almost always have unique literal
        // values, so caching them burns CPU on eviction bookkeeping (Vec::remove(0)
        // shifts the entire LRU list) with zero hit rate. Skip the cache entirely
        // Plan cache applies to statements whose shape can be
        // normalised + rebound (`UPDATE t SET x=? WHERE _entity_id=?`
        // reuses the same plan across thousands of varying literals).
        // INSERT is still bypassed — its shape changes per column set
        // and bulk paths don't go through here anyway.
        let first_word = query
            .trim()
            .split_ascii_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        let is_insert = first_word == "INSERT";

        // Fused normalize+extract: one byte-scan produces both the
        // cache_key AND the literal bindings. Saves a second Lexer
        // pass over the query text on every cache hit — dominant
        // cost on tight UPDATE loops that hit the same shape
        // thousands of times with varying literals.
        let (cache_key, prescan_binds) = if is_insert {
            (String::new(), Vec::new())
        } else {
            crate::storage::query::planner::cache_key::normalize_and_extract(query)
        };

        let expr = if is_insert {
            // Bypass plan cache for INSERT — shape varies per query.
            parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?
        } else {
            // ── Hot path: read lock only (no writer serialization on cache hits) ──
            //
            // peek() is a non-mutating probe: no LRU promotion, no touch().
            // This lets concurrent readers proceed without blocking each other.
            // On hit we bind literals if needed and return immediately.
            // Only on miss do we drop to a write lock to parse + insert.
            let hit = {
                let plan_cache = runtime.inner.query_cache.read();
                plan_cache.peek(&cache_key).map(|cached| {
                    let parameter_count = cached.parameter_count;
                    let optimized = cached.plan.optimized.clone();
                    let exact_query = cached.exact_query.clone();
                    (parameter_count, optimized, exact_query)
                })
            };

            if let Some((parameter_count, optimized, exact_query)) = hit {
                if parameter_count > 0 {
                    // Shape hit: use the binds extracted during normalise.
                    let shape_binds = prescan_binds.clone();
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
                    let mut pc = runtime.inner.query_cache.write();
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
        // appears in the expression with its stored body. Runs after
        // parse and before dispatch so the SQL entrypoint gets the
        // same view resolution `execute_query_expr` already does.
        let expr = runtime.rewrite_view_refs(expr);

        Ok(PreparedStatement { expr, mode })
    }

    pub(super) fn check_query_privilege(
        &self,
        runtime: &RedDBRuntime,
        expr: &QueryExpr,
    ) -> RedDBResult<()> {
        // Frame-level coarse gate. We consult `required_privilege()`
        // (computed once at frame-build) against the captured identity
        // before the deep grant engine walks the parsed expression.
        // The coarse gate cannot ALLOW anything the grant engine would
        // deny — it only short-circuits the obvious "Role::Read tries
        // INSERT" case so a downstream caller never has to redo this
        // check inline. `Privilege::None` (transaction control / SET /
        // SHOW) flows through unchanged; the grant engine treats those
        // as bypass too.
        if let Some((username, role)) = <Self as ReadFrame>::identity(self) {
            let needed = <Self as ReadFrame>::required_privilege(self);
            if !needed.is_satisfied_by(role) {
                // Issue #205 — when the deep grant engine *also*
                // denies, we treat this as an ordinary permission
                // failure. But when an Admin-only statement reaches
                // this gate without an auth_store wired (so the deep
                // engine can't double-check), the coarse rejection is
                // the only line of defence — emit an OperatorEvent so
                // the operator notices an Admin-class statement was
                // attempted with insufficient role.
                if matches!(needed, Privilege::Admin) && runtime.inner.auth_store.read().is_none() {
                    crate::telemetry::operator_event::OperatorEvent::AuthBypass {
                        principal: username.to_string(),
                        resource: format!("statement requiring {needed:?}"),
                        detail: format!(
                            "auth_store not wired; coarse gate is sole defence (role={role:?})"
                        ),
                    }
                    .emit_global();
                }
                return Err(RedDBError::Query(format!(
                    "permission denied: principal=`{username}` role=`{role:?}` lacks {needed:?} privilege"
                )));
            }
        }
        runtime
            .check_query_privilege(expr)
            .map_err(|err| RedDBError::Query(format!("permission denied: {err}")))
    }

    pub(super) fn prepare_dispatch(
        &self,
        runtime: &RedDBRuntime,
        expr: &QueryExpr,
    ) -> RedDBResult<Option<crate::runtime::locking::LockerGuard>> {
        runtime.validate_model_operations_before_auth(expr)?;
        self.check_query_privilege(runtime, expr)?;
        Ok(self.acquire_intent_locks(runtime, expr))
    }

    pub(super) fn acquire_intent_locks(
        &self,
        runtime: &RedDBRuntime,
        expr: &QueryExpr,
    ) -> Option<crate::runtime::locking::LockerGuard> {
        if !runtime.config_bool("concurrency.locking.enabled", true) {
            return None;
        }
        // Frame-level short-circuit: if the statement carries no lock
        // intent (transaction control, SET, SHOW), skip the lock
        // manager entirely instead of letting `intent_lock_modes_for`
        // walk the parsed expression to reach the same conclusion.
        if matches!(<Self as ReadFrame>::lock_intent(self), LockIntent::None) {
            return None;
        }
        intent_lock_modes_for(expr).map(|(global_mode, coll_mode)| {
            let mut guard =
                crate::runtime::locking::LockerGuard::new(runtime.inner.lock_manager.clone());
            let _ = guard.acquire(crate::runtime::locking::Resource::Global, global_mode);
            for collection in collections_referenced(expr) {
                let _ = guard.acquire(
                    crate::runtime::locking::Resource::Collection(collection),
                    coll_mode,
                );
            }
            guard
        })
    }
}

impl ReadFrame for StatementExecutionFrame {
    fn effective_scope(&self) -> Option<&str> {
        self.effective_scope.as_deref()
    }

    fn identity(&self) -> Option<(&str, Role)> {
        self.identity.as_ref().map(|(u, r)| (u.as_str(), *r))
    }

    fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    fn as_of_floor(&self) -> Option<Xid> {
        self.as_of_floor
    }

    fn cache_key(&self) -> &str {
        &self.cache_key
    }

    fn should_cache_result(&self) -> bool {
        !self.is_volatile_query && self.cache_safe
    }

    fn required_privilege(&self) -> Privilege {
        self.required_privilege
    }

    fn lock_intent(&self) -> LockIntent {
        self.lock_intent
    }

    fn visible_collections(&self) -> Option<&HashSet<String>> {
        self.visible_collections.as_ref()
    }
}

/// Lightweight `ReadFrame` carrier used by AI command entry points
/// (`SEARCH SIMILAR`, `SEARCH CONTEXT`, `ASK`).
///
/// Issue #119 calls this struct `EffectiveScope`. It bundles the
/// `(tenant, identity, role, visible_collections, snapshot)` tuple so
/// every AI runtime entry can pass *one* value to `AuthorizedSearch`
/// instead of re-reading thread-locals at every call site.
///
/// Built via `RedDBRuntime::ai_scope()` which sources tenant + identity
/// from the per-statement thread-locals (identical to how
/// `StatementExecutionFrame::build` derives them) and resolves
/// `visible_collections` via the `AuthStore` cache.
pub struct EffectiveScope {
    pub(crate) tenant: Option<String>,
    pub(crate) identity: Option<(String, Role)>,
    pub(crate) snapshot: Snapshot,
    pub(crate) visible_collections: Option<HashSet<String>>,
}

impl EffectiveScope {
    /// Capability check used by the AI runtime (`runtime/ai/ner.rs`)
    /// to gate LLM-backed NER calls behind `ai:ner:read`.
    ///
    /// Placeholder for now: always returns `false`. The auth engine's
    /// capability matrix is future work; until it lands, every routed
    /// LLM-NER call denies at the gate and `extract_tokens_routed`'s
    /// heuristic fallback fires (see `ask_pipeline::extract_tokens_routed`).
    /// Documented in code so the wire-up is a one-line change once
    /// the auth engine learns capabilities.
    pub fn has_capability(&self, _capability: &str) -> bool {
        false
    }
}

impl ReadFrame for EffectiveScope {
    fn effective_scope(&self) -> Option<&str> {
        self.tenant.as_deref()
    }
    fn identity(&self) -> Option<(&str, Role)> {
        self.identity.as_ref().map(|(u, r)| (u.as_str(), *r))
    }
    fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    fn as_of_floor(&self) -> Option<Xid> {
        None
    }
    fn cache_key(&self) -> &str {
        ""
    }
    fn should_cache_result(&self) -> bool {
        false
    }
    fn required_privilege(&self) -> Privilege {
        Privilege::Read
    }
    fn lock_intent(&self) -> LockIntent {
        LockIntent::Shared
    }
    fn visible_collections(&self) -> Option<&HashSet<String>> {
        self.visible_collections.as_ref()
    }
}

impl RedDBRuntime {
    /// Build the AI command `EffectiveScope` from the current
    /// statement thread-locals + auth store.
    ///
    /// Returns `None` for embedded callers (no auth store, no
    /// identity) — `AuthorizedSearch` treats `None` as deny-default.
    pub(crate) fn ai_scope(&self) -> EffectiveScope {
        let tenant = super::impl_core::current_tenant();
        let identity = super::impl_core::current_auth_identity();
        let snapshot = self.current_snapshot();
        let visible_collections = match (self.inner.auth_store.read().clone(), identity.as_ref()) {
            (Some(store), Some((principal, role))) => {
                let collections = self.inner.db.store().list_collections();
                Some(store.visible_collections_for_scope(
                    tenant.as_deref(),
                    *role,
                    principal,
                    &collections,
                ))
            }
            _ => None,
        };
        EffectiveScope {
            tenant,
            identity,
            snapshot,
            visible_collections,
        }
    }
}

/// Test fixtures for callers that need to drive `ReadFrame` without
/// booting a runtime. Lives behind `cfg(test)` and `pub(crate)` so it
/// only leaks across module boundaries inside the crate.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{LockIntent, Privilege, ReadFrame};
    use crate::auth::Role;
    use crate::storage::transaction::snapshot::{Snapshot, Xid};
    use std::collections::HashSet;

    /// A `ReadFrame` impl with hand-set fields. Used by
    /// `authorized_search` tests to assert the deny-default and
    /// scope-trim behaviour without going through frame construction.
    pub(crate) struct FakeReadFrame {
        pub tenant: Option<String>,
        pub identity: Option<(String, Role)>,
        pub snapshot: Snapshot,
        pub visible: Option<HashSet<String>>,
    }

    impl FakeReadFrame {
        pub(crate) fn without_scope() -> Self {
            Self {
                tenant: None,
                identity: None,
                snapshot: Snapshot {
                    xid: 0,
                    in_progress: HashSet::new(),
                },
                visible: None,
            }
        }

        pub(crate) fn with_visible(visible: HashSet<String>) -> Self {
            Self {
                tenant: Some("acme".to_string()),
                identity: Some(("alice".to_string(), Role::Read)),
                snapshot: Snapshot {
                    xid: 0,
                    in_progress: HashSet::new(),
                },
                visible: Some(visible),
            }
        }
    }

    impl ReadFrame for FakeReadFrame {
        fn effective_scope(&self) -> Option<&str> {
            self.tenant.as_deref()
        }
        fn identity(&self) -> Option<(&str, Role)> {
            self.identity.as_ref().map(|(u, r)| (u.as_str(), *r))
        }
        fn snapshot(&self) -> &Snapshot {
            &self.snapshot
        }
        fn as_of_floor(&self) -> Option<Xid> {
            None
        }
        fn cache_key(&self) -> &str {
            ""
        }
        fn should_cache_result(&self) -> bool {
            false
        }
        fn required_privilege(&self) -> Privilege {
            Privilege::Read
        }
        fn lock_intent(&self) -> LockIntent {
            LockIntent::Shared
        }
        fn visible_collections(&self) -> Option<&HashSet<String>> {
            self.visible.as_ref()
        }
    }
}

impl RedDBRuntime {
    fn own_transaction_xids(&self, conn_id: u64) -> HashSet<Xid> {
        let mut set = HashSet::new();
        if let Some(ctx) = self.inner.tx_contexts.read().get(&conn_id) {
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

    /// Resolve the snapshot for the current statement, returning
    /// the snapshot itself and (when AS OF is in effect) the
    /// resolved xid floor. The floor is the same xid carried inside
    /// `Snapshot.xid` for AS OF reads — exposing it separately lets
    /// the `ReadFrame` Interface tell "live read" from "historical
    /// read" without inferring from `in_progress.is_empty()`.
    fn statement_snapshot(&self, query: &str) -> RedDBResult<(Snapshot, Option<Xid>)> {
        match peek_top_level_as_of_with_table(query) {
            Some((spec, Some(table))) => {
                if !table.starts_with("red_") && !self.vcs_is_versioned(&table)? {
                    return Err(RedDBError::InvalidConfig(format!(
                        "AS OF requires a versioned collection — \
                         `{table}` has not opted in. \
                         Call vcs.set_versioned(\"{table}\", true) first."
                    )));
                }
                let xid = self.vcs_resolve_as_of(spec)?;
                Ok((
                    Snapshot {
                        xid,
                        in_progress: HashSet::new(),
                    },
                    Some(xid),
                ))
            }
            Some((spec, None)) => {
                let xid = self.vcs_resolve_as_of(spec)?;
                Ok((
                    Snapshot {
                        xid,
                        in_progress: HashSet::new(),
                    },
                    Some(xid),
                ))
            }
            None => Ok((self.current_snapshot(), None)),
        }
    }

    fn result_cache_safe(&self, conn_id: u64) -> bool {
        let has_active_xids = self.inner.snapshot_manager.oldest_active_xid().is_some();
        let in_own_tx = self.inner.tx_contexts.read().contains_key(&conn_id);
        !has_active_xids && !in_own_tx
    }
}

fn result_cache_key(query: &str) -> String {
    let tenant = current_tenant().unwrap_or_default();
    let auth = current_auth_identity()
        .map(|(user, role)| format!("{}|{:?}", user, role))
        .unwrap_or_default();
    if tenant.is_empty() && auth.is_empty() {
        query.to_string()
    } else {
        format!("{query}\u{001e}{tenant}\u{001e}{auth}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::RedDBOptions;
    use crate::runtime::impl_core::{
        clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
        set_current_tenant,
    };
    use crate::runtime::RedDBRuntime;

    fn fresh_runtime() -> RedDBRuntime {
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
    }

    /// Ensure thread-local state from a prior test can't leak into
    /// the next one — tests in the same binary share the thread.
    fn reset_thread_locals() {
        clear_current_tenant();
        clear_current_auth_identity();
    }

    #[test]
    fn autocommit_select_takes_live_snapshot() {
        reset_thread_locals();
        let rt = fresh_runtime();
        let frame =
            StatementExecutionFrame::build(&rt, "SELECT 1").expect("frame builds for SELECT 1");

        // Live reads: no AS OF floor, snapshot bounded by the
        // manager's `peek_next_xid` so committed tuples are visible.
        let f: &dyn ReadFrame = &frame;
        assert!(f.as_of_floor().is_none(), "live read has no AS OF floor");
        assert!(
            f.snapshot().xid >= 1,
            "autocommit snapshot xid is bounded by peek_next_xid"
        );
    }

    #[test]
    fn frame_captures_identity_and_scope() {
        reset_thread_locals();
        set_current_tenant("acme".to_string());
        set_current_auth_identity("alice".to_string(), Role::Write);

        let rt = fresh_runtime();
        let frame = StatementExecutionFrame::build(&rt, "SELECT 1").expect("frame builds");
        let f: &dyn ReadFrame = &frame;

        assert_eq!(f.effective_scope(), Some("acme"));
        let id = f.identity().expect("identity captured");
        assert_eq!(id.0, "alice");
        assert!(matches!(id.1, Role::Write));

        // Cache key mixes scope + identity so two callers under
        // different tenants never share a cache slot.
        assert!(
            f.cache_key().contains("acme") && f.cache_key().contains("alice"),
            "cache key folds in scope + identity, got {:?}",
            f.cache_key()
        );

        reset_thread_locals();
    }

    #[test]
    fn as_of_rejects_non_versioned_user_collection() {
        reset_thread_locals();
        let rt = fresh_runtime();

        // `not_versioned` is a plain user collection — the frame
        // builder must reject AS OF until the caller opts in via
        // `vcs.set_versioned`.
        let err = match StatementExecutionFrame::build(
            &rt,
            "SELECT * FROM not_versioned AS OF COMMIT 'deadbeef'",
        ) {
            Err(e) => e,
            Ok(_) => panic!("AS OF on non-versioned user collection rejected"),
        };

        let msg = format!("{err}");
        assert!(
            msg.contains("AS OF requires a versioned collection"),
            "expected AS OF rejection, got: {msg}"
        );
    }

    /// End-to-end proof that the SELECT path consumes a `ReadFrame`.
    ///
    /// Sets a tenant + identity via the public thread-local API the
    /// runtime uses for ambient scope, drives a real `SELECT` through
    /// `execute_query`, then inspects the result cache that the SELECT
    /// path populates via `frame.cache_key()`. The key only carries
    /// the tenant + identity *because* it was built through the frame —
    /// reverting the wiring to inline `current_tenant()` /
    /// `current_auth_identity()` reads would still pass this test, but
    /// dropping the frame entirely (so the SELECT path stopped touching
    /// `cache_key`) would break it.
    #[test]
    fn select_path_routes_through_frame_cache_key() {
        reset_thread_locals();
        set_current_tenant("acme".to_string());
        set_current_auth_identity("alice".to_string(), Role::Read);

        let rt = fresh_runtime();
        let result = rt
            .execute_query("SELECT 1")
            .expect("SELECT 1 executes under tenant=acme/identity=alice");
        assert_eq!(result.statement_type, "select");

        // The SELECT path (in `execute_query_expr`) builds a frame and
        // writes its result through `frame.cache_key()`. That key folds
        // tenant + identity in via `result_cache_key`, so finding "acme"
        // and "alice" inside any cached key proves the frame was the
        // seam used.
        let cache = rt.inner.result_cache.read();
        let any_keyed_with_scope = cache
            .0
            .keys()
            .any(|k| k.contains("acme") && k.contains("alice"));
        assert!(
            any_keyed_with_scope,
            "expected at least one result-cache key carrying tenant+identity, \
             got keys: {:?}",
            cache.0.keys().collect::<Vec<_>>()
        );

        reset_thread_locals();
    }

    /// A SELECT that calls a volatile builtin (here:
    /// `pg_advisory_unlock`, the volatile token the runtime currently
    /// recognises in `query_has_volatile_builtin`) must NOT populate
    /// the result cache. Any caller hitting the cache after this would
    /// see a stale answer for an inherently-volatile query, so the
    /// SELECT path gates writes through `frame.should_cache_result()`.
    ///
    /// Deletion test: removing `ReadFrame::should_cache_result`, or
    /// reverting the SELECT path to skip its safety gate, would let
    /// the result cache silently absorb this statement and break the
    /// assertion below.
    #[test]
    fn volatile_select_does_not_populate_result_cache() {
        reset_thread_locals();
        let rt = fresh_runtime();

        // Frame-level invariant: the volatile-builtin signal collapses
        // `should_cache_result` to false even for an autocommit /
        // out-of-tx connection.
        let frame =
            StatementExecutionFrame::build(&rt, "SELECT pg_advisory_unlock(1)").expect("frame");
        let f: &dyn ReadFrame = &frame;
        assert!(
            !f.should_cache_result(),
            "volatile builtin must disable result-cache safety"
        );

        // End-to-end: drive the volatile SELECT through `execute_query`
        // and confirm no entry was stamped under its cache key. Other
        // entries from prior tests sharing the binary may exist, so we
        // assert specifically on this query's key.
        let _ = rt
            .execute_query("SELECT pg_advisory_unlock(1)")
            .expect("volatile SELECT executes");
        let cache = rt.inner.result_cache.read();
        let key = result_cache_key("SELECT pg_advisory_unlock(1)");
        assert!(
            !cache.0.contains_key(&key),
            "volatile SELECT must not populate result cache, found key {key:?} in {:?}",
            cache.0.keys().collect::<Vec<_>>()
        );

        reset_thread_locals();
    }

    #[test]
    fn blob_cache_backend_populates_blob_path_without_legacy_write() {
        reset_thread_locals();
        let rt = fresh_runtime();
        rt.inner
            .db
            .store()
            .set_config_tree("runtime.result_cache.backend", &crate::json!("blob_cache"));

        let result = rt.execute_query("SELECT 1").expect("SELECT 1 executes");
        assert_eq!(result.statement_type, "select");

        let key = result_cache_key("SELECT 1");
        assert!(
            rt.inner
                .result_blob_cache
                .get("runtime.result_cache", &key)
                .is_some(),
            "blob backend should stamp the Blob Cache path"
        );
        assert!(rt.inner.result_blob_entries.read().0.contains_key(&key));
        assert!(
            !rt.inner.result_cache.read().0.contains_key(&key),
            "blob backend should not write the legacy map"
        );
    }

    #[test]
    fn blob_cache_backend_keeps_volatile_select_out_of_blob_path() {
        reset_thread_locals();
        let rt = fresh_runtime();
        rt.inner
            .db
            .store()
            .set_config_tree("runtime.result_cache.backend", &crate::json!("blob_cache"));

        let _ = rt
            .execute_query("SELECT pg_advisory_unlock(1)")
            .expect("volatile SELECT executes");
        let key = result_cache_key("SELECT pg_advisory_unlock(1)");
        assert!(
            rt.inner
                .result_blob_cache
                .get("runtime.result_cache", &key)
                .is_none(),
            "volatile SELECT must not populate blob result cache"
        );
        assert!(!rt.inner.result_blob_entries.read().0.contains_key(&key));
    }

    #[test]
    fn shadow_backend_dual_writes_and_reports_no_divergence_on_equal_results() {
        reset_thread_locals();
        let rt = fresh_runtime();
        rt.inner
            .db
            .store()
            .set_config_tree("runtime.result_cache.backend", &crate::json!("shadow"));

        let first = rt.execute_query("SELECT 1").expect("first SELECT");
        let second = rt.execute_query("SELECT 1").expect("cached SELECT");
        assert_eq!(first.result.len(), second.result.len());

        let key = result_cache_key("SELECT 1");
        assert!(rt.inner.result_cache.read().0.contains_key(&key));
        assert!(rt.inner.result_blob_entries.read().0.contains_key(&key));
        assert_eq!(rt.result_cache_shadow_divergences(), 0);
        assert_eq!(
            crate::runtime::METRIC_CACHE_SHADOW_DIVERGENCE_TOTAL,
            "cache_shadow_divergence_total"
        );
    }

    #[test]
    fn as_of_on_red_collection_records_floor() {
        reset_thread_locals();
        let rt = fresh_runtime();

        // `red_*` collections always allow AS OF. The frame should
        // resolve to a concrete xid and surface it via the Interface.
        let frame =
            StatementExecutionFrame::build(&rt, "SELECT * FROM red_commits AS OF SNAPSHOT 1")
                .expect("AS OF SNAPSHOT 1 on red_commits resolves");

        let f: &dyn ReadFrame = &frame;
        assert_eq!(
            f.as_of_floor(),
            Some(1),
            "AS OF SNAPSHOT 1 records xid=1 as the floor"
        );
        assert_eq!(f.snapshot().xid, 1);
        assert!(
            f.snapshot().in_progress.is_empty(),
            "historical reads have no in-progress set"
        );
    }

    /// The frame classifies common SQL prefixes into the coarse
    /// `Privilege` / `LockIntent` buckets at build time. This test
    /// pins the mapping so a regression that silently re-routes
    /// (e.g. INSERT classified as Read) surfaces here, not at a
    /// downstream privilege gate.
    #[test]
    fn frame_classifies_privilege_and_lock_intent_from_prefix() {
        reset_thread_locals();
        let rt = fresh_runtime();

        let cases = [
            ("SELECT 1", Privilege::Read, LockIntent::Shared),
            (
                "INSERT INTO t (id) VALUES (1)",
                Privilege::Write,
                LockIntent::Exclusive,
            ),
            (
                "UPDATE t SET x = 1 WHERE id = 1",
                Privilege::Write,
                LockIntent::Exclusive,
            ),
            (
                "DELETE FROM t WHERE id = 1",
                Privilege::Write,
                LockIntent::Exclusive,
            ),
            (
                "CREATE TABLE foo (id INT)",
                Privilege::Write,
                LockIntent::Exclusive,
            ),
            ("BEGIN", Privilege::None, LockIntent::None),
            ("COMMIT", Privilege::None, LockIntent::None),
            ("SET timezone = 'UTC'", Privilege::None, LockIntent::None),
        ];

        for (q, want_priv, want_lock) in cases {
            let frame = StatementExecutionFrame::build(&rt, q)
                .unwrap_or_else(|e| panic!("frame builds for {q:?}: {e}"));
            let f: &dyn ReadFrame = &frame;
            assert_eq!(f.required_privilege(), want_priv, "privilege for {q:?}");
            assert_eq!(f.lock_intent(), want_lock, "lock intent for {q:?}");
        }
    }

    /// Deletion-test for `ReadFrame::required_privilege`: a SELECT
    /// driven through `execute_query` under an identity whose role
    /// doesn't satisfy the frame's coarse `Read` privilege gets
    /// denied with the frame's signal.
    ///
    /// We test the gate by classifying an INSERT (which the frame
    /// reports as `Privilege::Write`) under `Role::Read` — the only
    /// pair the legacy fallback would also reject, but here the
    /// rejection comes through `frame.check_query_privilege` BEFORE
    /// the parsed-expression walker runs. Removing
    /// `required_privilege` (or the `is_satisfied_by` consult inside
    /// `check_query_privilege`) would force the deny path back to the
    /// inline `RedDBRuntime::check_query_privilege` walker — but the
    /// auth_store gate up there is bypassed when no auth_store is
    /// wired (embedded test mode), so this test would FLIP from
    /// denied to permitted and break the assertion below.
    #[test]
    fn insert_under_read_role_denied_via_frame_privilege() {
        reset_thread_locals();
        set_current_auth_identity("alice".to_string(), Role::Read);

        let rt = fresh_runtime();
        // Bypass parser by reaching into the frame directly: the
        // frame derives privilege from the SQL prefix without
        // needing an auth_store wired up. Driving end-to-end via
        // `execute_query` would also reject (no table `t`), but for
        // a different reason — we want to pin the privilege seam.
        let frame = StatementExecutionFrame::build(&rt, "INSERT INTO t (id) VALUES (1)")
            .expect("frame builds for INSERT");
        let f: &dyn ReadFrame = &frame;
        assert_eq!(
            f.required_privilege(),
            Privilege::Write,
            "INSERT classified as Write"
        );
        let id = f.identity().expect("identity captured");
        assert!(
            !f.required_privilege().is_satisfied_by(id.1),
            "Role::Read does not satisfy Privilege::Write — frame must deny"
        );

        // End-to-end: the frame's `check_query_privilege` sees the
        // (Read role, Write privilege) mismatch and denies before
        // dispatch. We drive a synthetic `QueryExpr::Table` because
        // the SELECT/INSERT parser would happen to also fail, and we
        // want the failure to come from the privilege seam.
        use crate::storage::query::ast::{QueryExpr, TableQuery};
        let expr = QueryExpr::Table(TableQuery::new("t"));
        let err = frame
            .check_query_privilege(&rt, &expr)
            .expect_err("denied via frame's coarse privilege gate");
        let msg = format!("{err}");
        assert!(
            msg.contains("permission denied") && msg.contains("Write"),
            "expected frame-level Write deny, got: {msg}"
        );

        reset_thread_locals();
    }

    /// Deletion-test for `ReadFrame::lock_intent`: a transaction
    /// control statement carries `LockIntent::None` and the
    /// `acquire_intent_locks` path returns `None` without consulting
    /// `intent_lock_modes_for`. Removing the method (or its consult
    /// site in `acquire_intent_locks`) would force the lock-mode
    /// helper to walk a fabricated parsed expression to reach the
    /// same conclusion — but the assertion that no guard is allocated
    /// for a `BEGIN` frame would still hold, so we additionally pin
    /// the classifier mapping above to make the deletion observable.
    #[test]
    fn control_statement_skips_intent_locks_via_frame() {
        reset_thread_locals();
        let rt = fresh_runtime();

        let frame = StatementExecutionFrame::build(&rt, "BEGIN").expect("frame builds for BEGIN");
        let f: &dyn ReadFrame = &frame;
        assert_eq!(f.lock_intent(), LockIntent::None);

        // Drive `acquire_intent_locks` against a fabricated SELECT
        // expression that WOULD normally yield `(IS, IS)`; the frame's
        // `lock_intent() == None` short-circuit must still suppress
        // the guard.
        use crate::storage::query::ast::{QueryExpr, TableQuery};
        let expr = QueryExpr::Table(TableQuery::new("t"));
        let guard = frame.acquire_intent_locks(&rt, &expr);
        assert!(
            guard.is_none(),
            "BEGIN frame's lock_intent=None must short-circuit lock acquisition"
        );
    }
}
