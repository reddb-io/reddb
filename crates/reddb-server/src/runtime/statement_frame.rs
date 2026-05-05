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
use crate::storage::query::ast::QueryExpr;
use crate::storage::transaction::snapshot::{Snapshot, Xid};

pub(super) struct StatementExecutionFrame {
    tx_local_tenant: Option<Option<String>>,
    snapshot: Snapshot,
    own_xids: HashSet<Xid>,
    cache_key: String,
    is_volatile_query: bool,
    cache_safe: bool,
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
        let snapshot = runtime.statement_snapshot(query)?;
        let cache_key = result_cache_key(query);
        let is_volatile_query = query_has_volatile_builtin(query);
        let cache_safe = runtime.result_cache_safe(conn_id);

        Ok(Self {
            tx_local_tenant,
            snapshot,
            own_xids,
            cache_key,
            is_volatile_query,
            cache_safe,
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
        !self.is_volatile_query && self.cache_safe
    }

    pub(super) fn should_write_result_cache(&self, result: &RuntimeQueryResult) -> bool {
        result.statement_type == "select"
            && !self.is_volatile_query
            && self.cache_safe
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

    fn statement_snapshot(&self, query: &str) -> RedDBResult<Snapshot> {
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
                Ok(Snapshot {
                    xid,
                    in_progress: HashSet::new(),
                })
            }
            Some((spec, None)) => {
                let xid = self.vcs_resolve_as_of(spec)?;
                Ok(Snapshot {
                    xid,
                    in_progress: HashSet::new(),
                })
            }
            None => Ok(self.current_snapshot()),
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
