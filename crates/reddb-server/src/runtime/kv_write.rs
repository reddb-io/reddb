//! Keyed writes use a single durable replacement for ordinary autocommit KV.

use crate::application::collection_contract_enforcer::CollectionContractWriteEnforcer;
use crate::application::entity::{CreateEntityOutput, CreateKvInput};
use crate::catalog::CollectionModel;
use crate::storage::EntityId;
use crate::{RedDBResult, RedDBRuntime};
use reddb_types::Value;

pub(crate) fn atomic_available(runtime: &RedDBRuntime, collection: &str) -> bool {
    runtime.current_xid().is_none()
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
        engine = engine.for_keyed_write(previous);
    } else {
        assert!(
            previous.is_none(),
            "versioned and transactional writes retain their existing path"
        );
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
