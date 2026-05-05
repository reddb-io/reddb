use std::collections::HashSet;
use std::sync::Arc;

use super::impl_core::{
    collections_referenced, current_auth_identity, current_connection_id, current_tenant,
    intent_lock_modes_for, peek_top_level_as_of_with_table, query_has_volatile_builtin,
    ConfigSnapshotGuard, CurrentSnapshotGuard, SecretStoreGuard, SnapshotContext,
    TxLocalTenantGuard,
};
use super::{RedDBRuntime, RuntimeQueryResult};
use crate::api::{RedDBError, RedDBResult};
use crate::auth::Role;
use crate::storage::query::ast::QueryExpr;
use crate::storage::transaction::snapshot::{Snapshot, Xid};

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
}

pub(super) struct StatementFrameGuards {
    _tx_local_guard: TxLocalTenantGuard,
    _config_snapshot_guard: ConfigSnapshotGuard,
    _secret_store_guard: SecretStoreGuard,
    _snapshot_guard: CurrentSnapshotGuard,
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
            && result.result.pre_serialized_json.is_none()
            && result.result.records.len() <= 5
    }

    pub(super) fn check_query_privilege(
        &self,
        runtime: &RedDBRuntime,
        expr: &QueryExpr,
    ) -> RedDBResult<()> {
        runtime
            .check_query_privilege(expr)
            .map_err(|err| RedDBError::Query(format!("permission denied: {err}")))
    }

    pub(super) fn acquire_intent_locks(
        &self,
        runtime: &RedDBRuntime,
        expr: &QueryExpr,
    ) -> Option<crate::runtime::locking::LockerGuard> {
        if !runtime.config_bool("concurrency.locking.enabled", true) {
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
}

impl RedDBRuntime {
    fn own_transaction_xids(&self, conn_id: u64) -> HashSet<Xid> {
        let mut set = HashSet::new();
        if let Some(ctx) = self.inner.tx_contexts.read().get(&conn_id) {
            set.insert(ctx.xid);
            for (_, sub) in &ctx.savepoints {
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
    use crate::runtime::impl_core::{
        clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
        set_current_tenant,
    };
    use crate::api::RedDBOptions;
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

    #[test]
    fn as_of_on_red_collection_records_floor() {
        reset_thread_locals();
        let rt = fresh_runtime();

        // `red_*` collections always allow AS OF. The frame should
        // resolve to a concrete xid and surface it via the Interface.
        let frame = StatementExecutionFrame::build(
            &rt,
            "SELECT * FROM red_commits AS OF SNAPSHOT 1",
        )
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
}
