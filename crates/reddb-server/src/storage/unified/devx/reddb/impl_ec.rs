use super::*;

use crate::ec::config::{EcFieldConfig, EcMode};
use crate::ec::consolidation::{self, ConsolidationResult, EcStatus};
use crate::ec::transactions::{self, EcOperation};

impl RedDB {
    /// Add a value to a field via eventual consistency.
    /// In Sync mode, consolidates immediately. In Async mode, queues for background consolidation.
    pub fn ec_add(
        &self,
        collection: &str,
        field: &str,
        target_id: EntityId,
        value: f64,
    ) -> Result<EntityId, Box<dyn std::error::Error>> {
        self.ec_mutate(collection, field, target_id, value, EcOperation::Add)
    }

    /// Subtract a value from a field via eventual consistency.
    pub fn ec_sub(
        &self,
        collection: &str,
        field: &str,
        target_id: EntityId,
        value: f64,
    ) -> Result<EntityId, Box<dyn std::error::Error>> {
        self.ec_mutate(collection, field, target_id, value, EcOperation::Sub)
    }

    /// Set a field to a specific value via eventual consistency (overrides previous adds/subs).
    pub fn ec_set(
        &self,
        collection: &str,
        field: &str,
        target_id: EntityId,
        value: f64,
    ) -> Result<EntityId, Box<dyn std::error::Error>> {
        self.ec_mutate(collection, field, target_id, value, EcOperation::Set)
    }

    fn ec_mutate(
        &self,
        collection: &str,
        field: &str,
        target_id: EntityId,
        value: f64,
        operation: EcOperation,
    ) -> Result<EntityId, Box<dyn std::error::Error>> {
        let config = self.ec_config_or_default(collection, field);
        let tx_collection = config.tx_collection_name();

        let id = transactions::create_transaction(
            &self.store,
            &tx_collection,
            target_id.raw(),
            field,
            value,
            operation,
            None,
        )?;

        if config.mode == EcMode::Sync {
            consolidation::consolidate(&self.store, &config, Some(target_id.raw()))
                .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        }

        Ok(id)
    }

    /// Consolidate all pending transactions for a field (or a specific entity).
    pub fn ec_consolidate(
        &self,
        collection: &str,
        field: &str,
        target_id: Option<u64>,
    ) -> Result<ConsolidationResult, Box<dyn std::error::Error>> {
        let config = self.ec_config_or_default(collection, field);
        consolidation::consolidate(&self.store, &config, target_id)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })
    }

    /// Consolidate ALL registered EC fields. Useful before flush().
    pub fn ec_consolidate_all(&self) -> Result<u64, Box<dyn std::error::Error>> {
        let configs = self.ec_registry.all_configs();
        let mut total = 0u64;
        for config in configs {
            if let Ok(result) = consolidation::consolidate(&self.store, &config, None) {
                total += result.transactions_applied;
            }
        }
        Ok(total)
    }

    /// Get the consolidation status for a specific entity's field.
    pub fn ec_status(&self, collection: &str, field: &str, target_id: u64) -> EcStatus {
        let config = self.ec_config_or_default(collection, field);
        consolidation::get_ec_status(&self.store, &config, target_id)
    }

    /// Register a field for eventual consistency.
    pub fn ec_register(&self, config: EcFieldConfig) {
        self.ec_registry.register(config);
    }

    fn ec_config_or_default(&self, collection: &str, field: &str) -> EcFieldConfig {
        self.ec_registry
            .get(collection, field)
            .unwrap_or_else(|| EcFieldConfig::new(collection, field))
    }
}
