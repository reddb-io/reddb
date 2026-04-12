use std::collections::HashMap;

use super::config::{EcFieldConfig, EcReducer};
use super::transactions::{
    mark_transactions_applied, query_pending_transactions, EcOperation, EcTransaction,
};
use crate::storage::schema::Value;
use crate::storage::unified::entity::{EntityData, EntityId};
use crate::storage::unified::store::UnifiedStore;

#[derive(Debug, Clone, Default)]
pub struct ConsolidationResult {
    pub records_consolidated: u64,
    pub transactions_applied: u64,
    pub errors: u64,
}

pub fn consolidate(
    store: &UnifiedStore,
    config: &EcFieldConfig,
    target_id: Option<u64>,
) -> Result<ConsolidationResult, String> {
    let tx_collection = config.tx_collection_name();
    let pending = query_pending_transactions(store, &tx_collection, target_id);

    if pending.is_empty() {
        return Ok(ConsolidationResult::default());
    }

    // Group by target_id
    let mut groups: HashMap<u64, Vec<(EntityId, EcTransaction)>> = HashMap::new();
    for (eid, tx) in pending {
        groups.entry(tx.target_id).or_default().push((eid, tx));
    }

    let mut result = ConsolidationResult::default();

    for (tid, transactions) in groups {
        match consolidate_record(store, config, tid, &transactions) {
            Ok(applied_count) => {
                result.records_consolidated += 1;
                result.transactions_applied += applied_count;

                let applied_ids: Vec<EntityId> = transactions.iter().map(|(eid, _)| *eid).collect();
                mark_transactions_applied(store, &tx_collection, &applied_ids);
            }
            Err(_) => {
                result.errors += 1;
            }
        }
    }

    Ok(result)
}

fn consolidate_record(
    store: &UnifiedStore,
    config: &EcFieldConfig,
    target_id: u64,
    transactions: &[(EntityId, EcTransaction)],
) -> Result<u64, String> {
    if transactions.is_empty() {
        return Ok(0);
    }

    // Find the last SET operation (if any)
    let last_set_idx = transactions
        .iter()
        .rposition(|(_, tx)| tx.operation == EcOperation::Set);

    // Determine base value
    let current_value = read_field_value(store, &config.collection, target_id, &config.field);
    let base_value = if let Some(idx) = last_set_idx {
        transactions[idx].1.value
    } else {
        current_value.unwrap_or(config.initial_value)
    };

    // Apply subsequent operations
    let start_idx = last_set_idx.map(|i| i + 1).unwrap_or(0);
    let mut new_value = base_value;
    let mut count = 0u64;

    for (_, tx) in &transactions[start_idx..] {
        match tx.operation {
            EcOperation::Add => {
                new_value = config.reducer.apply(new_value, tx.value, count);
                count += 1;
            }
            EcOperation::Sub => {
                let negated = match config.reducer {
                    EcReducer::Sum => new_value - tx.value,
                    EcReducer::Min => new_value.min(tx.value),
                    EcReducer::Max => new_value.max(tx.value),
                    _ => config.reducer.apply(new_value, -tx.value, count),
                };
                new_value = negated;
                count += 1;
            }
            EcOperation::Set => {
                new_value = tx.value;
                count = 0;
            }
        }
    }

    // Write the consolidated value back to the target entity
    write_field_value(
        store,
        &config.collection,
        target_id,
        &config.field,
        new_value,
    )?;

    Ok(transactions.len() as u64)
}

fn read_field_value(
    store: &UnifiedStore,
    collection: &str,
    entity_id: u64,
    field: &str,
) -> Option<f64> {
    let manager = store.get_collection(collection)?;
    let entity = manager.get(EntityId::new(entity_id))?;
    let row = entity.data.as_row()?;
    let value = row.get_field(field)?;
    match value {
        Value::Float(f) => Some(*f),
        Value::Integer(n) => Some(*n as f64),
        Value::UnsignedInteger(n) => Some(*n as f64),
        _ => None,
    }
}

fn write_field_value(
    store: &UnifiedStore,
    collection: &str,
    entity_id: u64,
    field: &str,
    value: f64,
) -> Result<(), String> {
    let manager = store
        .get_collection(collection)
        .ok_or_else(|| format!("collection '{}' not found", collection))?;

    let mut entity = manager
        .get(EntityId::new(entity_id))
        .ok_or_else(|| format!("entity {} not found in '{}'", entity_id, collection))?;

    if let EntityData::Row(ref mut row) = entity.data {
        if let Some(ref mut named) = row.named {
            named.insert(field.to_string(), Value::Float(value));
        }
    }

    manager
        .update(entity)
        .map_err(|e| format!("update failed: {:?}", e))?;

    Ok(())
}

pub fn get_ec_status(store: &UnifiedStore, config: &EcFieldConfig, target_id: u64) -> EcStatus {
    let consolidated = read_field_value(store, &config.collection, target_id, &config.field)
        .unwrap_or(config.initial_value);

    let tx_collection = config.tx_collection_name();
    let pending = query_pending_transactions(store, &tx_collection, Some(target_id));

    let pending_value: f64 = pending
        .iter()
        .map(|(_, tx)| match tx.operation {
            EcOperation::Add => tx.value,
            EcOperation::Sub => -tx.value,
            EcOperation::Set => 0.0,
        })
        .sum();

    let has_set = pending
        .iter()
        .any(|(_, tx)| tx.operation == EcOperation::Set);

    EcStatus {
        consolidated,
        pending_value,
        pending_transactions: pending.len() as u64,
        has_pending_set: has_set,
        field: config.field.clone(),
        collection: config.collection.clone(),
        reducer: config.reducer.as_str().to_string(),
        mode: if config.mode == super::config::EcMode::Sync {
            "sync"
        } else {
            "async"
        }
        .to_string(),
    }
}

#[derive(Debug, Clone)]
pub struct EcStatus {
    pub consolidated: f64,
    pub pending_value: f64,
    pub pending_transactions: u64,
    pub has_pending_set: bool,
    pub field: String,
    pub collection: String,
    pub reducer: String,
    pub mode: String,
}
