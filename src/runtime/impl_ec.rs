use crate::ec::config::{EcFieldConfig, EcMode};
use crate::ec::consolidation;
use crate::ec::transactions::{create_transaction, EcOperation};
use crate::{RedDBError, RedDBResult};

use super::RedDBRuntime;

impl RedDBRuntime {
    pub fn ec_add(
        &self,
        collection: &str,
        field: &str,
        target_id: u64,
        value: f64,
        source: Option<&str>,
    ) -> RedDBResult<u64> {
        self.ec_mutate(
            collection,
            field,
            target_id,
            value,
            EcOperation::Add,
            source,
        )
    }

    pub fn ec_sub(
        &self,
        collection: &str,
        field: &str,
        target_id: u64,
        value: f64,
        source: Option<&str>,
    ) -> RedDBResult<u64> {
        self.ec_mutate(
            collection,
            field,
            target_id,
            value,
            EcOperation::Sub,
            source,
        )
    }

    pub fn ec_set(
        &self,
        collection: &str,
        field: &str,
        target_id: u64,
        value: f64,
        source: Option<&str>,
    ) -> RedDBResult<u64> {
        self.ec_mutate(
            collection,
            field,
            target_id,
            value,
            EcOperation::Set,
            source,
        )
    }

    fn ec_mutate(
        &self,
        collection: &str,
        field: &str,
        target_id: u64,
        value: f64,
        operation: EcOperation,
        source: Option<&str>,
    ) -> RedDBResult<u64> {
        let config = self.ec_config_or_default(collection, field);
        let tx_collection = config.tx_collection_name();

        let id = create_transaction(
            self.inner.db.store().as_ref(),
            &tx_collection,
            target_id,
            field,
            value,
            operation,
            source,
        )
        .map_err(|e| RedDBError::Internal(e))?;

        // Sync mode: consolidate immediately
        if config.mode == EcMode::Sync {
            consolidation::consolidate(self.inner.db.store().as_ref(), &config, Some(target_id))
                .map_err(|e| RedDBError::Internal(e))?;
        }

        Ok(id.raw())
    }

    pub fn ec_consolidate(
        &self,
        collection: &str,
        field: &str,
        target_id: Option<u64>,
    ) -> RedDBResult<consolidation::ConsolidationResult> {
        let config = self.ec_config_or_default(collection, field);
        consolidation::consolidate(self.inner.db.store().as_ref(), &config, target_id)
            .map_err(|e| RedDBError::Internal(e))
    }

    pub fn ec_status(
        &self,
        collection: &str,
        field: &str,
        target_id: u64,
    ) -> consolidation::EcStatus {
        let config = self.ec_config_or_default(collection, field);
        consolidation::get_ec_status(self.inner.db.store().as_ref(), &config, target_id)
    }

    pub fn ec_register_field(&self, config: EcFieldConfig) {
        self.inner.ec_registry.register(config);
        // Restart worker if needed
        if !self.inner.ec_worker.is_running() && !self.inner.ec_registry.async_configs().is_empty()
        {
            self.inner.ec_worker.start(
                std::sync::Arc::clone(&self.inner.ec_registry),
                std::sync::Arc::clone(&self.inner.db.store()),
            );
        }
    }

    pub fn ec_global_status(&self) -> Vec<crate::ec::consolidation::EcStatus> {
        let configs = self.inner.ec_registry.all_configs();
        let mut statuses = Vec::new();
        for config in configs {
            let tx_collection = config.tx_collection_name();
            let pending = crate::ec::transactions::query_pending_transactions(
                self.inner.db.store().as_ref(),
                &tx_collection,
                None,
            );
            let total_pending = pending.len() as u64;
            statuses.push(crate::ec::consolidation::EcStatus {
                consolidated: 0.0,
                pending_value: 0.0,
                pending_transactions: total_pending,
                has_pending_set: false,
                field: config.field.clone(),
                collection: config.collection.clone(),
                reducer: config.reducer.as_str().to_string(),
                mode: if config.mode == EcMode::Sync {
                    "sync"
                } else {
                    "async"
                }
                .to_string(),
            });
        }
        statuses
    }

    fn ec_config_or_default(&self, collection: &str, field: &str) -> EcFieldConfig {
        self.inner
            .ec_registry
            .get(collection, field)
            .unwrap_or_else(|| EcFieldConfig::new(collection, field))
    }
}
