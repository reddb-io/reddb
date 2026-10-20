//! Keyed writes use a single durable replacement for ordinary autocommit KV.

use crate::application::collection_contract_enforcer::CollectionContractWriteEnforcer;
use crate::application::entity::{CreateEntityOutput, CreateKvInput};
use crate::catalog::CollectionModel;
use crate::storage::EntityId;
use crate::{RedDBError, RedDBResult, RedDBRuntime};
use reddb_types::Value;

pub(crate) fn atomic_available(runtime: &RedDBRuntime, collection: &str) -> bool {
    runtime.db().store().pager().is_none()
        && runtime.current_xid().is_none()
        && !crate::storage::UnifiedStore::deferred_store_wal_capture_active()
        && !super::kv_read::is_versioned(runtime, CollectionModel::Kv, collection)
        && runtime
            .db()
            .collection_contract_arc(collection)
            .is_some_and(|contract| contract.declared_model == CollectionModel::Kv)
}

pub(crate) fn create(
    runtime: &RedDBRuntime,
    input: CreateKvInput,
    previous: Option<EntityId>,
) -> RedDBResult<CreateEntityOutput> {
    let db = runtime.db();
    let contract = CollectionContractWriteEnforcer::new(runtime, &db, &input.collection);
    let vault = db
        .collection_contract_arc(&input.collection)
        .is_some_and(|contract| contract.declared_model == CollectionModel::Vault);
    let value = if vault {
        contract.ensure_model(CollectionModel::Vault)?;
        runtime.seal_vault_value(&input.collection, input.value)?
    } else {
        contract.ensure_model(CollectionModel::Kv)?;
        input.value
    };
    let fields = vec![
        ("key".to_string(), Value::text(input.key)),
        ("value".to_string(), value),
    ];
    let collection = input.collection;
    let mut engine = runtime.mutation_engine();
    if !vault && atomic_available(runtime, &collection) {
        engine = engine.for_keyed_write(previous)?;
    } else if previous.is_some() {
        return Err(RedDBError::InvalidOperation(
            "collection contract changed during keyed replacement; retry the write".into(),
        ));
    }
    let result = engine.apply(
        collection.clone(),
        vec![super::mutation::MutationRow {
            fields,
            metadata: input.metadata,
            node_links: Vec::new(),
            vector_links: Vec::new(),
        }],
    )?;
    let id = result.ids[0];
    Ok(CreateEntityOutput {
        id,
        entity: db.store().get(&collection, id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::RuntimeEntityPort;

    #[test]
    fn replacement_refuses_changed_contract_without_deleting_previous() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("KV PUT changed.item = 'original'")
            .expect("seed");
        let (_, previous) = runtime
            .get_kv("changed", "item")
            .expect("read")
            .expect("previous");
        let mut contract = runtime
            .db()
            .collection_contract("changed")
            .expect("contract");
        contract.declared_model = CollectionModel::Mixed;
        runtime
            .db()
            .save_collection_contract(contract)
            .expect("change contract");
        let result = create(
            &runtime,
            CreateKvInput {
                collection: "changed".into(),
                key: "item".into(),
                value: Value::text("replacement"),
                metadata: Vec::new(),
            },
            Some(previous),
        );
        assert!(result.is_err());
        assert!(runtime.db().store().get("changed", previous).is_some());
        assert_eq!(
            runtime
                .db()
                .store()
                .get_collection("changed")
                .unwrap()
                .count(),
            1
        );
    }
}
