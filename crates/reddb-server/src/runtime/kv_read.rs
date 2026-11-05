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
    versioned: bool,
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
            versioned: is_versioned(runtime, model, collection)?,
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
            versioned: is_versioned(runtime, CollectionModel::Kv, collection)?,
            collection,
        })
    }

    pub(crate) fn record(&self, key: &str) -> RedDBResult<Option<KvReadRecord>> {
        self.project(key, |entity| KvReadRecord {
            id: entity.id,
            created_at: entity.created_at,
            updated_at: entity.updated_at,
            value: super::impl_kv::kv_value_from_entity(entity),
        })
    }

    pub(super) fn owned_entity(&self, key: &str) -> RedDBResult<Option<UnifiedEntity>> {
        self.project(key, Clone::clone)
    }

    fn project<T>(
        &self,
        key: &str,
        projection: impl FnMut(&UnifiedEntity) -> T,
    ) -> RedDBResult<Option<T>> {
        let store = self.runtime.db().store();
        let Some(manager) = store.get_collection(self.collection) else {
            return Ok(None);
        };
        let _publication = manager
            .has_backing()
            .then(|| store.vault_publication_lock.lock());
        let snapshot = if self.versioned {
            super::impl_core::capture_current_snapshot()
        } else {
            None
        };
        manager
            .try_project_key_entity(
                key,
                snapshot.as_ref(),
                self.versioned,
                usize::MAX,
                projection,
            )
            .map_err(RedDBError::from)
    }
}

pub(super) fn is_versioned(
    runtime: &RedDBRuntime,
    model: CollectionModel,
    collection: &str,
) -> RedDBResult<bool> {
    if model == CollectionModel::Kv {
        runtime.vcs_is_versioned(collection)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_keyed_reads_match_resident_and_propagate_decode_failures() {
        use crate::application::ports::RuntimeEntityPort;
        use crate::storage::engine::BTree;
        use reddb_engine::storage::collection_read::PagedCollection;
        use std::sync::Arc;

        let directory = tempfile::tempdir().expect("keyed fixture");
        let options = crate::RedDBOptions::persistent(directory.path().join("cold-keys.rdb"))
            .with_memory_budget(128 * 1024 * 1024)
            .with_storage_profile(
                crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
            )
            .expect("profile");
        let runtime = RedDBRuntime::with_options(options).expect("runtime");
        runtime
            .execute_query("KV PUT cold_keys.item = 7")
            .expect("seed");
        let expected = runtime
            .get_kv("cold_keys", "item")
            .expect("resident REST read");
        assert!(expected.is_some());
        let store = runtime.db().store();
        store.persist().expect("publish");
        let manager = store.get_collection("cold_keys").expect("keys");
        let root = store.collection_root_page("cold_keys").expect("root");
        let base = Arc::new(PagedCollection::new(Arc::new(BTree::with_root(
            Arc::clone(store.pager().expect("pager")),
            root,
        ))));
        let count = manager.count() as u64;
        manager.install_backing(
            base.clone(),
            root,
            store.format_version(),
            128 * 1024,
            count,
        );
        assert_eq!(
            runtime.get_kv("cold_keys", "item").expect("cold REST read"),
            expected
        );
        assert_eq!(runtime.get_kv("cold_keys", "absent").expect("absent"), None);
        // This must fail explicitly rather than report a missing key.
        manager.install_backing(base, root, store.format_version(), 1, count);
        assert!(runtime.get_kv("cold_keys", "item").is_err());
        assert!(runtime.execute_query("KV GET cold_keys.item").is_err());
    }

    #[test]
    fn cold_versioned_keys_keep_history_when_settings_are_paged() {
        use crate::application::ports::RuntimeEntityPort;
        use crate::application::vcs_collections;
        use crate::storage::engine::BTree;
        use reddb_engine::storage::collection_read::PagedCollection;
        use std::sync::Arc;

        let directory = tempfile::tempdir().expect("versioned keyed fixture");
        let options =
            crate::RedDBOptions::persistent(directory.path().join("cold-versioned-keys.rdb"))
                .with_memory_budget(128 * 1024 * 1024)
                .with_storage_profile(
                    crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
                )
                .expect("profile");
        let runtime = RedDBRuntime::with_options(options).expect("runtime");
        runtime
            .vcs_set_versioned("versioned_keys", true)
            .expect("retain history");
        runtime
            .execute_query("KV PUT versioned_keys.item = 7")
            .expect("first version");
        runtime
            .execute_query("KV PUT versioned_keys.item = 9")
            .expect("second version");
        let expected = runtime
            .get_kv("versioned_keys", "item")
            .expect("resident value");
        assert_eq!(
            expected.as_ref().map(|(value, _)| value),
            Some(&Value::Integer(9))
        );
        let store = runtime.db().store();
        store.persist().expect("publish history and settings");
        for collection in ["versioned_keys", vcs_collections::SETTINGS] {
            let manager = store.get_collection(collection).expect("collection");
            let root = store.collection_root_page(collection).expect("root");
            let base = PagedCollection::new(Arc::new(BTree::with_root(
                Arc::clone(store.pager().expect("pager")),
                root,
            )));
            manager.install_backing(
                Arc::new(base),
                root,
                store.format_version(),
                128 * 1024,
                manager.count() as u64,
            );
        }
        assert!(runtime
            .vcs_is_versioned("versioned_keys")
            .expect("cold VCS settings"));
        assert_eq!(
            runtime
                .get_kv("versioned_keys", "item")
                .expect("cold latest version"),
            expected
        );
        let result = runtime
            .execute_query("KV LIST versioned_keys")
            .expect("cold keyed list");
        assert_eq!(result.result.records.len(), 1);
        let manager = store
            .get_collection(vcs_collections::SETTINGS)
            .expect("settings");
        let (base, _) = manager
            .current_backing_overlay()
            .expect("settings generation");
        let root = store
            .collection_root_page(vcs_collections::SETTINGS)
            .expect("root");
        manager.install_backing(
            base,
            root,
            store.format_version(),
            1,
            manager.count() as u64,
        );
        assert!(runtime.vcs_is_versioned("versioned_keys").is_err());
        assert!(runtime.get_kv("versioned_keys", "item").is_err());
        assert!(runtime
            .execute_query("KV PUT versioned_keys.item = 11")
            .is_err());
        assert_eq!(
            store
                .get_collection("versioned_keys")
                .expect("keys")
                .count(),
            2,
            "failed settings reads must not silently turn versioned updates into replacements"
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
            .expect("validated read")
            .is_some());
        contract.origin = crate::physical::ContractOrigin::Explicit;
        runtime
            .db()
            .save_collection_contract(contract)
            .expect("explicit table");
        assert!(KvReadResolver::for_rest_kv(&runtime, "shared").is_err());
    }
}
