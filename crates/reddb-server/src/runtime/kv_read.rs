//! Validated keyed reads shared by runtime and application ports.
//!
//! Callers choose their existing validation policy at construction. Only typed
//! owned results leave this module; projection callbacks cannot be supplied by
//! callers or re-enter storage while the segment read guard is held.

use crate::catalog::CollectionModel;
use crate::storage::{EntityId, UnifiedEntity};
use crate::{RedDBError, RedDBResult, RedDBRuntime};
use reddb_types::Value;

pub(crate) struct KvReadRecord {
    pub(crate) id: EntityId,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    pub(crate) value: Option<Value>,
}

pub(crate) struct KvReadResolver<'a> {
    runtime: &'a RedDBRuntime,
    model: CollectionModel,
    collection: &'a str,
}

impl<'a> KvReadResolver<'a> {
    pub(crate) fn for_keyed_model(
        runtime: &'a RedDBRuntime,
        model: CollectionModel,
        collection: &'a str,
    ) -> RedDBResult<Self> {
        if let Some(contract) = runtime.db().collection_contract_arc(collection) {
            if contract.declared_model != model && contract.declared_model != CollectionModel::Mixed
            {
                return Err(RedDBError::InvalidOperation(format!(
                    "collection '{}' is declared as '{}' and does not allow '{}' operations",
                    collection,
                    super::impl_kv::keyed_model_name(contract.declared_model),
                    super::impl_kv::keyed_model_name(model)
                )));
            }
        }
        Ok(Self {
            runtime,
            model,
            collection,
        })
    }

    pub(crate) fn for_rest_kv(runtime: &'a RedDBRuntime, collection: &'a str) -> RedDBResult<Self> {
        crate::application::collection_contract_enforcer::ensure_collection_model_read(
            &runtime.db(),
            collection,
            CollectionModel::Kv,
        )?;
        Ok(Self {
            runtime,
            model: CollectionModel::Kv,
            collection,
        })
    }

    pub(crate) fn record(&self, key: &str) -> Option<KvReadRecord> {
        self.project(key, |entity| KvReadRecord {
            id: entity.id,
            created_at: entity.created_at,
            updated_at: entity.updated_at,
            value: super::impl_kv::kv_value_from_entity(entity),
        })
    }

    pub(super) fn owned_entity(&self, key: &str) -> Option<UnifiedEntity> {
        self.project(key, Clone::clone)
    }

    fn project<T>(&self, key: &str, projection: impl FnMut(&UnifiedEntity) -> T) -> Option<T> {
        let store = self.runtime.db().store();
        let manager = store.get_collection(self.collection)?;
        // SQL UPDATE creates MVCC versions even when KV PUT history is disabled.
        // Select the visible version for plain KV as well; this does not enable
        // versioned PUTs or change the config/vault read policy.
        let mvcc = self.model == CollectionModel::Kv;
        let snapshot = if mvcc {
            super::impl_core::capture_current_snapshot()
        } else {
            None
        };
        manager.project_key_entity(key, snapshot.as_ref(), mvcc, projection)
    }
}

pub(super) fn is_versioned(
    runtime: &RedDBRuntime,
    model: CollectionModel,
    collection: &str,
) -> bool {
    model == CollectionModel::Kv && runtime.vcs_is_versioned(collection).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_kv_reads_follow_sql_commit_and_rollback() {
        let directory = tempfile::tempdir().expect("temporary KV store");
        let options = crate::RedDBOptions::persistent(directory.path().join("kv-visibility.rdb"));
        let runtime = RedDBRuntime::with_options(options.clone()).expect("runtime");
        runtime
            .execute_query("KV PUT shared.item = 'before'")
            .expect("seed");
        for (sql, expected) in [
            ("BEGIN", "before"),
            (
                "UPDATE shared SET value='pending' WHERE key='item'",
                "pending",
            ),
            ("ROLLBACK", "before"),
            ("BEGIN", "before"),
            ("UPDATE shared SET value='after' WHERE key='item'", "after"),
            ("COMMIT", "after"),
        ] {
            runtime.execute_query(sql).expect("SQL transaction");
            let result = runtime
                .execute_query("KV GET shared.item")
                .expect("keyed read");
            assert_eq!(
                result.result.records[0].get("value"),
                Some(&Value::text(expected)),
                "after {sql}"
            );
        }
        runtime.checkpoint().expect("checkpoint");
        drop(runtime);
        let reopened = RedDBRuntime::with_options(options).expect("reopen");
        let result = reopened
            .execute_query("KV GET shared.item")
            .expect("recovered keyed read");
        assert_eq!(
            result.result.records[0].get("value"),
            Some(&Value::text("after"))
        );
    }

    #[test]
    fn kv_read_resolver_requires_validation_and_preserves_legacy_policy_difference() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("KV PUT shared.item = 7")
            .expect("seed");
        let mut contract = runtime
            .db()
            .collection_contract("shared")
            .expect("contract");
        contract.declared_model = CollectionModel::Table;
        runtime
            .db()
            .save_collection_contract(contract.clone())
            .expect("implicit table");
        assert!(KvReadResolver::for_keyed_model(&runtime, CollectionModel::Kv, "shared").is_err());
        assert!(KvReadResolver::for_rest_kv(&runtime, "shared")
            .expect("legacy REST read")
            .record("item")
            .is_some());
        contract.origin = crate::physical::ContractOrigin::Explicit;
        runtime
            .db()
            .save_collection_contract(contract)
            .expect("explicit table");
        assert!(KvReadResolver::for_rest_kv(&runtime, "shared").is_err());
    }
}
