//! DDL execution: CREATE TABLE, DROP TABLE, ALTER TABLE via SQL AST
//!
//! Translates DDL statements into collection-level operations on the
//! underlying `UnifiedStore`.  RedDB uses a flexible schema-on-read
//! model, so column definitions are advisory metadata rather than
//! rigid constraints.

use super::*;
use crate::catalog::CollectionModel;
use crate::runtime::audit_log::{AuditAuthSource, AuditEvent, AuditFieldEscaper, Outcome};
use crate::runtime::ddl::polymorphic_resolver;
use crate::storage::query::{analyze_create_table, resolve_declared_data_type, CreateColumnDef};
use std::collections::{BTreeSet, HashMap, HashSet};

fn vault_master_key_ref(collection: &str) -> String {
    format!("red.vault.{collection}.master_key")
}

impl RedDBRuntime {
    /// Execute CREATE TABLE
    ///
    /// Creates a new collection in the store.  Column definitions are
    /// recorded for introspection but do not enforce rigid schema
    /// constraints.
    pub fn execute_create_table(
        &self,
        raw_query: &str,
        query: &CreateTableQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        if query.collection_model != CollectionModel::Table {
            return self.execute_create_keyed_collection(raw_query, query);
        }
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        let store = self.inner.db.store();
        analyze_create_table(query).map_err(|err| RedDBError::Query(err.to_string()))?;
        crate::reserved_fields::ensure_no_reserved_public_item_fields(
            query.columns.iter().map(|column| column.name.as_str()),
            &format!("table '{}'", query.name),
        )?;
        // Check if the collection already exists.
        let exists = store.get_collection(&query.name).is_some();
        if exists {
            if query.if_not_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("table '{}' already exists", query.name),
                    "create",
                ));
            }
            return Err(RedDBError::Query(format!(
                "table '{}' already exists",
                query.name
            )));
        }

        // Build and validate the contract before mutating storage so invalid
        // SQL types / duplicate columns do not leave partial side effects.
        let contract = collection_contract_from_create_table(query)?;
        validate_event_subscriptions(self, &query.name, &contract.subscriptions)?;
        // Create the collection.
        store
            .create_collection(&query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        for subscription in &contract.subscriptions {
            ensure_event_target_queue(self, &subscription.target_queue)?;
        }
        if let Some(default_ttl_ms) = query.default_ttl_ms {
            self.inner
                .db
                .set_collection_default_ttl_ms(&query.name, default_ttl_ms);
        }
        self.inner
            .db
            .save_collection_contract(contract)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        if let Some(tenant_id) = crate::runtime::impl_core::current_tenant() {
            store.set_config_tree(
                &format!("red.collection_tenants.{}", query.name),
                &crate::serde_json::Value::String(tenant_id),
            );
        }
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.refresh_table_planner_stats(&query.name);
        self.invalidate_result_cache();
        // Issue #120 — feed the create into the schema-vocabulary
        // reverse index so AskPipeline (#121) sees this collection.
        let columns: Vec<String> = query.columns.iter().map(|col| col.name.clone()).collect();
        self.schema_vocabulary_apply(
            crate::runtime::schema_vocabulary::DdlEvent::CreateCollection {
                collection: query.name.clone(),
                columns,
                type_tags: Vec::new(),
                description: None,
            },
        );
        // Partition metadata (Phase 2.2 PG parity).
        //
        // When the CREATE TABLE carries a `PARTITION BY RANGE|LIST|HASH (col)`
        // clause, stamp the partition config into `red_config` under
        // `partition.{table}.{by,column}`. Children are registered separately
        // via `ALTER TABLE parent ATTACH PARTITION child ...`.
        if let Some(spec) = &query.partition_by {
            let kind_str = match spec.kind {
                crate::storage::query::ast::PartitionKind::Range => "range",
                crate::storage::query::ast::PartitionKind::List => "list",
                crate::storage::query::ast::PartitionKind::Hash => "hash",
            };
            store.set_config_tree(
                &format!("partition.{}.by", query.name),
                &crate::serde_json::Value::String(kind_str.to_string()),
            );
            store.set_config_tree(
                &format!("partition.{}.column", query.name),
                &crate::serde_json::Value::String(spec.column.clone()),
            );
        }

        // Table-scoped multi-tenancy (Phase 2.5.4).
        //
        // `CREATE TABLE t (...) TENANT BY (col)` declaration:
        //   1. Persists the `tenant_tables.{table}.column` marker so
        //      INSERTs can auto-fill and future opens re-hydrate.
        //   2. Registers the table in the in-memory `tenant_tables`
        //      HashMap used by the DML auto-fill path.
        //   3. Installs an implicit RLS policy equivalent to
        //      `USING (col = CURRENT_TENANT())` across all actions.
        //   4. Flips `rls_enabled_tables` on so the policy applies.
        if let Some(col) = &query.tenant_by {
            store.set_config_tree(
                &format!("tenant_tables.{}.column", query.name),
                &crate::serde_json::Value::String(col.clone()),
            );
            self.register_tenant_table(&query.name, col);
        }

        let ttl_suffix = query
            .default_ttl_ms
            .map(|ttl_ms| format!(" with default TTL {}ms", ttl_ms))
            .unwrap_or_default();

        let tenant_suffix = query
            .tenant_by
            .as_ref()
            .map(|col| format!(" (tenant-scoped by {col})"))
            .unwrap_or_default();

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!(
                "table '{}' created{}{}",
                query.name, ttl_suffix, tenant_suffix
            ),
            "create",
        ))
    }

    fn execute_create_keyed_collection(
        &self,
        raw_query: &str,
        query: &CreateTableQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        if is_system_schema_name(&query.name) {
            return Err(RedDBError::Query("system schema is read-only".to_string()));
        }
        let store = self.inner.db.store();
        let label = polymorphic_resolver::model_name(query.collection_model);
        if store.get_collection(&query.name).is_some() {
            if query.if_not_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("{label} '{}' already exists", query.name),
                    "create",
                ));
            }
            return Err(RedDBError::Query(format!(
                "{label} '{}' already exists",
                query.name
            )));
        }

        store
            .create_collection(&query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        if query.collection_model == CollectionModel::Vault {
            self.provision_vault_key_material(&query.name, query.vault_own_master_key)?;
            let key_scope = if query.vault_own_master_key {
                "own"
            } else {
                "cluster"
            };
            store.set_config_tree(
                &format!("red.vault.{}.key_scope", query.name),
                &crate::serde_json::Value::String(key_scope.to_string()),
            );
            store.set_config_tree(
                &format!("red.vault.{}.status", query.name),
                &crate::serde_json::Value::String("sealed".to_string()),
            );
        }
        if query.collection_model == CollectionModel::Metrics {
            for spec in &query.metrics_rollup_policies {
                let policy = crate::storage::timeseries::retention::DownsamplePolicy::parse(spec)
                    .ok_or_else(|| {
                    RedDBError::Query(format!("invalid metrics rollup policy '{}'", spec))
                })?;
                if policy.source != "raw" {
                    return Err(RedDBError::Query(format!(
                        "invalid metrics rollup policy '{}': metrics v0 rollups must use raw as source",
                        spec
                    )));
                }
                if !matches!(
                    policy.aggregation.as_str(),
                    "avg" | "sum" | "min" | "max" | "count"
                ) {
                    return Err(RedDBError::Query(format!(
                        "invalid metrics rollup policy '{}': supported aggregations are avg, sum, min, max, count",
                        spec
                    )));
                }
            }
            if let Some(raw_retention_ms) = query.default_ttl_ms {
                self.inner
                    .db
                    .set_collection_default_ttl_ms(&query.name, raw_retention_ms);
                store.set_config_tree(
                    &format!("red.metrics.{}.raw_retention_ms", query.name),
                    &crate::serde_json::Value::Number(raw_retention_ms as f64),
                );
            }
            let tenant_identity = query
                .tenant_by
                .clone()
                .unwrap_or_else(|| "current_tenant".to_string());
            store.set_config_tree(
                &format!("red.metrics.{}.tenant_identity", query.name),
                &crate::serde_json::Value::String(tenant_identity),
            );
            store.set_config_tree(
                &format!("red.metrics.{}.namespace", query.name),
                &crate::serde_json::Value::String("default".to_string()),
            );
            if !query.metrics_rollup_policies.is_empty() {
                store.set_config_tree(
                    &format!("red.metrics.{}.rollup_policies", query.name),
                    &crate::serde_json::Value::Array(
                        query
                            .metrics_rollup_policies
                            .iter()
                            .cloned()
                            .map(crate::serde_json::Value::String)
                            .collect(),
                    ),
                );
            }
        }
        let contract = if query.collection_model == CollectionModel::Metrics {
            metrics_collection_contract(query)
        } else {
            keyed_collection_contract(&query.name, query.collection_model)
        };
        self.inner
            .db
            .save_collection_contract(contract)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        if let Some(tenant_id) = crate::runtime::impl_core::current_tenant() {
            store.set_config_tree(
                &format!("red.collection_tenants.{}", query.name),
                &crate::serde_json::Value::String(tenant_id),
            );
        }
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.invalidate_result_cache();

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("{label} '{}' created", query.name),
            "create",
        ))
    }

    pub fn execute_create_collection(
        &self,
        raw_query: &str,
        query: &CreateCollectionQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let model = match query.kind.as_str() {
            "graph" => CollectionModel::Graph,
            "document" => CollectionModel::Document,
            "metrics" => CollectionModel::Metrics,
            // KIND blockchain — issue #523 foundation: stored on top of a
            // Table-shaped collection. The `chain` marker + reserved-column
            // discipline make the difference. Schema validation, conflict
            // retries, and `verify_chain` come in later iterations.
            "blockchain" => CollectionModel::Table,
            other => {
                return Err(RedDBError::Query(format!(
                    "NOT_YET_SUPPORTED: CREATE COLLECTION KIND {other} is not implemented"
                )));
            }
        };
        let create = CreateTableQuery {
            collection_model: model,
            name: query.name.clone(),
            columns: Vec::new(),
            if_not_exists: query.if_not_exists,
            default_ttl_ms: None,
            metrics_rollup_policies: Vec::new(),
            context_index_fields: Vec::new(),
            context_index_enabled: false,
            timestamps: false,
            partition_by: None,
            tenant_by: None,
            append_only: false,
            subscriptions: Vec::new(),
            vault_own_master_key: false,
        };
        let result = self.execute_create_table(raw_query, &create)?;
        if query.kind == "blockchain" {
            self.install_blockchain_kind(&query.name)?;
        }
        // Issue #522 — wire `SIGNED_BY (...)` into the runtime. The parser
        // already produces a validated 32-byte pubkey list; installing
        // the registry stamps the per-collection signer set into
        // `red_config` so the INSERT path can load it cheaply.
        if !query.allowed_signers.is_empty() {
            let actor = crate::runtime::impl_core::current_user_projected()
                .unwrap_or_else(|| "@system/create-collection".to_string());
            crate::runtime::signed_writes_kind::install(
                &*self.inner.db.store(),
                &query.name,
                &query.allowed_signers,
                &actor,
            );
        }
        Ok(result)
    }

    /// Stamp `red.collection.{name}.kind = "chain"` and append the genesis
    /// row. Idempotent against `IF NOT EXISTS`: if the collection already
    /// has a row at height 0 we leave it.
    fn install_blockchain_kind(&self, name: &str) -> RedDBResult<()> {
        use crate::runtime::blockchain_kind;
        use crate::storage::unified::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
        use std::sync::Arc;

        let store = self.inner.db.store();
        blockchain_kind::mark_as_chain(&*store, name);

        let existing_tip = blockchain_kind::chain_tip(&*store, name);
        if existing_tip.height.is_some() {
            return Ok(());
        }

        let fields = blockchain_kind::genesis_fields(blockchain_kind::now_ms());
        let named: std::collections::HashMap<String, crate::storage::schema::Value> =
            fields.into_iter().collect();
        let entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: Arc::from(name),
                row_id: 0,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(named),
                schema: None,
            }),
        );
        store
            .insert_auto(name, entity)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        // #524: prime the in-memory tip cache so the chain-tip endpoint and
        // subsequent INSERTs don't have to scan the collection to find genesis.
        if let Some(tip) = blockchain_kind::chain_tip_full(&*store, name) {
            self.inner
                .chain_tip_cache
                .lock()
                .insert(name.to_string(), tip);
        }
        Ok(())
    }

    pub fn execute_create_vector(
        &self,
        raw_query: &str,
        query: &CreateVectorQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        if is_system_schema_name(&query.name) {
            return Err(RedDBError::Query("system schema is read-only".to_string()));
        }
        let store = self.inner.db.store();
        if store.get_collection(&query.name).is_some() {
            if query.if_not_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("vector '{}' already exists", query.name),
                    "create",
                ));
            }
            return Err(RedDBError::Query(format!(
                "vector '{}' already exists",
                query.name
            )));
        }

        store
            .create_collection(&query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.inner
            .db
            .save_collection_contract(vector_collection_contract(query))
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        if let Some(tenant_id) = crate::runtime::impl_core::current_tenant() {
            store.set_config_tree(
                &format!("red.collection_tenants.{}", query.name),
                &crate::serde_json::Value::String(tenant_id),
            );
        }
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.invalidate_result_cache();

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("vector '{}' created", query.name),
            "create",
        ))
    }

    fn provision_vault_key_material(
        &self,
        collection: &str,
        own_master_key: bool,
    ) -> RedDBResult<()> {
        let auth_store = self.inner.auth_store.read().clone().ok_or_else(|| {
            RedDBError::Query("CREATE VAULT requires an enabled, unsealed vault".to_string())
        })?;
        if !auth_store.is_vault_backed() {
            return Err(RedDBError::Query(
                "CREATE VAULT requires an enabled, unsealed vault".to_string(),
            ));
        }

        if auth_store.vault_secret_key().is_none() {
            let key = crate::auth::store::random_bytes(32);
            auth_store
                .vault_kv_try_set("red.secret.aes_key".to_string(), hex::encode(key))
                .map_err(|err| RedDBError::Query(err.to_string()))?;
        }

        if own_master_key {
            let key = crate::auth::store::random_bytes(32);
            auth_store
                .vault_kv_try_set(vault_master_key_ref(collection), hex::encode(key))
                .map_err(|err| RedDBError::Query(err.to_string()))?;
        }

        Ok(())
    }

    /// Execute DROP TABLE
    ///
    /// Drops the collection and all its data from the store.
    pub fn execute_drop_table(
        &self,
        raw_query: &str,
        query: &DropTableQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        let store = self.inner.db.store();

        if is_system_schema_name(&query.name) {
            return Err(RedDBError::Query("system schema is read-only".to_string()));
        }

        let exists = store.get_collection(&query.name).is_some();
        if !exists {
            if query.if_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("table '{}' does not exist", query.name),
                    "drop",
                ));
            }
            return Err(RedDBError::NotFound(format!(
                "table '{}' not found",
                query.name
            )));
        }
        let actual =
            polymorphic_resolver::resolve(&query.name, &self.inner.db.catalog_model_snapshot())?;
        polymorphic_resolver::ensure_model_match(CollectionModel::Table, actual)?;

        // Emit 1 collection_dropped event before storage is wiped.
        // Queue is preserved; subscription is removed with the contract below.
        let final_count = store
            .get_collection(&query.name)
            .map(|manager| manager.query_all(|_| true).len() as u64)
            .unwrap_or(0);
        crate::runtime::mutation::emit_collection_dropped_event_for_collection(
            self,
            &query.name,
            final_count,
        )?;

        let orphaned_indices: Vec<String> = self
            .inner
            .index_store
            .list_indices(&query.name)
            .into_iter()
            .map(|index| index.name)
            .collect();
        for name in &orphaned_indices {
            self.inner.index_store.drop_index(name, &query.name);
        }

        store
            .drop_collection(&query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.inner.db.invalidate_vector_index(&query.name);
        self.inner.db.clear_collection_default_ttl_ms(&query.name);
        self.inner
            .db
            .remove_collection_contract(&query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.clear_table_planner_stats(&query.name);
        self.invalidate_result_cache();
        // Issue #119: a dropped collection vanishes from every
        // (tenant, role)'s visible-collections set. Auth is optional in
        // embedded mode so guard the call.
        if let Some(store) = self.inner.auth_store.read().clone() {
            store.invalidate_visible_collections_cache();
        }
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        // Issue #120 — drop both the collection entries *and* every
        // index entry that was scoped to this collection.  Dropping the
        // collection wipes columns + collection-name + type-tags +
        // index hits in one pass via `purge_collection_entries`, so
        // the explicit `DropIndex` calls would be redundant.
        self.schema_vocabulary_apply(
            crate::runtime::schema_vocabulary::DdlEvent::DropCollection {
                collection: query.name.clone(),
            },
        );

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("table '{}' dropped", query.name),
            "drop",
        ))
    }

    pub fn execute_drop_graph(
        &self,
        raw_query: &str,
        query: &DropGraphQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.execute_drop_typed_collection(
            raw_query,
            &query.name,
            query.if_exists,
            CollectionModel::Graph,
            "graph",
        )
    }

    pub fn execute_drop_vector(
        &self,
        raw_query: &str,
        query: &DropVectorQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.execute_drop_typed_collection(
            raw_query,
            &query.name,
            query.if_exists,
            CollectionModel::Vector,
            "vector",
        )
    }

    pub fn execute_drop_document(
        &self,
        raw_query: &str,
        query: &DropDocumentQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.execute_drop_typed_collection(
            raw_query,
            &query.name,
            query.if_exists,
            CollectionModel::Document,
            "document",
        )
    }

    pub fn execute_drop_kv(
        &self,
        raw_query: &str,
        query: &DropKvQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let label = polymorphic_resolver::model_name(query.model);
        self.execute_drop_typed_collection(
            raw_query,
            &query.name,
            query.if_exists,
            query.model,
            label,
        )
    }

    pub fn execute_drop_collection(
        &self,
        raw_query: &str,
        query: &DropCollectionQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        if is_system_schema_name(&query.name) {
            return Err(RedDBError::Query("system schema is read-only".to_string()));
        }
        let store = self.inner.db.store();
        if store.get_collection(&query.name).is_none() {
            if query.if_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("collection '{}' does not exist", query.name),
                    "drop",
                ));
            }
            return Err(RedDBError::NotFound(format!(
                "collection '{}' not found",
                query.name
            )));
        }

        let actual =
            polymorphic_resolver::resolve(&query.name, &self.inner.db.catalog_model_snapshot())?;
        if let Some(expected) = query.model {
            polymorphic_resolver::ensure_model_match(expected, actual)?;
        }

        match actual {
            CollectionModel::Table => self.execute_drop_table(
                raw_query,
                &DropTableQuery {
                    name: query.name.clone(),
                    if_exists: query.if_exists,
                },
            ),
            CollectionModel::TimeSeries => self.execute_drop_timeseries(
                raw_query,
                &DropTimeSeriesQuery {
                    name: query.name.clone(),
                    if_exists: query.if_exists,
                },
            ),
            CollectionModel::Queue => self.execute_drop_queue(
                raw_query,
                &DropQueueQuery {
                    name: query.name.clone(),
                    if_exists: query.if_exists,
                },
            ),
            CollectionModel::Graph => self.execute_drop_graph(
                raw_query,
                &DropGraphQuery {
                    name: query.name.clone(),
                    if_exists: query.if_exists,
                },
            ),
            CollectionModel::Vector => self.execute_drop_vector(
                raw_query,
                &DropVectorQuery {
                    name: query.name.clone(),
                    if_exists: query.if_exists,
                },
            ),
            CollectionModel::Document => self.execute_drop_document(
                raw_query,
                &DropDocumentQuery {
                    name: query.name.clone(),
                    if_exists: query.if_exists,
                },
            ),
            CollectionModel::Kv => self.execute_drop_kv(
                raw_query,
                &DropKvQuery {
                    name: query.name.clone(),
                    if_exists: query.if_exists,
                    model: CollectionModel::Kv,
                },
            ),
            CollectionModel::Config => self.execute_drop_kv(
                raw_query,
                &DropKvQuery {
                    name: query.name.clone(),
                    if_exists: query.if_exists,
                    model: CollectionModel::Config,
                },
            ),
            CollectionModel::Vault => self.execute_drop_kv(
                raw_query,
                &DropKvQuery {
                    name: query.name.clone(),
                    if_exists: query.if_exists,
                    model: CollectionModel::Vault,
                },
            ),
            CollectionModel::Hll => self.execute_probabilistic_command(
                raw_query,
                &ProbabilisticCommand::DropHll {
                    name: query.name.clone(),
                    if_exists: query.if_exists,
                },
            ),
            CollectionModel::Sketch => self.execute_probabilistic_command(
                raw_query,
                &ProbabilisticCommand::DropSketch {
                    name: query.name.clone(),
                    if_exists: query.if_exists,
                },
            ),
            CollectionModel::Filter => self.execute_probabilistic_command(
                raw_query,
                &ProbabilisticCommand::DropFilter {
                    name: query.name.clone(),
                    if_exists: query.if_exists,
                },
            ),
            CollectionModel::Metrics => self.execute_drop_typed_collection(
                raw_query,
                &query.name,
                query.if_exists,
                CollectionModel::Metrics,
                "metrics",
            ),
            CollectionModel::Mixed => self.execute_drop_typed_collection(
                raw_query,
                &query.name,
                query.if_exists,
                CollectionModel::Mixed,
                "collection",
            ),
        }
    }

    /// Execute ALTER TABLE
    ///
    /// In RedDB's schema-on-read model, ALTER TABLE operations are advisory.
    /// ADD COLUMN records the schema intent, DROP COLUMN removes it, and
    /// RENAME COLUMN is a metadata rename.  Existing data is not rewritten.
    pub fn execute_alter_table(
        &self,
        raw_query: &str,
        query: &AlterTableQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        let store = self.inner.db.store();

        // Verify the table exists.
        if store.get_collection(&query.name).is_none() {
            return Err(RedDBError::NotFound(format!(
                "table '{}' not found",
                query.name
            )));
        }

        let mut messages = Vec::new();

        // Collect column-level changes upfront for schema-change event emission below.
        let fields_added: Vec<String> = query
            .operations
            .iter()
            .filter_map(|op| {
                if let AlterOperation::AddColumn(col) = op {
                    Some(col.name.clone())
                } else {
                    None
                }
            })
            .collect();
        let fields_removed: Vec<String> = query
            .operations
            .iter()
            .filter_map(|op| {
                if let AlterOperation::DropColumn(name) = op {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect();

        for op in &query.operations {
            match op {
                AlterOperation::AddColumn(col) => {
                    // Schema-on-read: column will be available on next insert.
                    messages.push(format!("column '{}' added", col.name));
                }
                AlterOperation::DropColumn(name) => {
                    messages.push(format!("column '{}' dropped", name));
                }
                AlterOperation::RenameColumn { from, to } => {
                    messages.push(format!("column '{}' renamed to '{}'", from, to));
                }
                AlterOperation::AttachPartition { child, bound } => {
                    // Persist child → parent binding in red_config so the
                    // future planner-side pruner can enumerate children and
                    // evaluate their bounds.
                    store.set_config_tree(
                        &format!("partition.{}.children.{}", query.name, child),
                        &crate::serde_json::Value::String(bound.clone()),
                    );
                    messages.push(format!(
                        "partition '{child}' attached to '{}' ({bound})",
                        query.name
                    ));
                }
                AlterOperation::DetachPartition { child } => {
                    store.set_config_tree(
                        &format!("partition.{}.children.{}", query.name, child),
                        &crate::serde_json::Value::Null,
                    );
                    messages.push(format!(
                        "partition '{child}' detached from '{}'",
                        query.name
                    ));
                }
                AlterOperation::EnableRowLevelSecurity => {
                    self.inner
                        .rls_enabled_tables
                        .write()
                        .insert(query.name.clone());
                    // Persist flag so RLS survives restart via red_config.
                    store.set_config_tree(
                        &format!("rls.enabled.{}", query.name),
                        &crate::serde_json::Value::Bool(true),
                    );
                    self.invalidate_plan_cache();
                    messages.push(format!("row level security enabled on '{}'", query.name));
                }
                AlterOperation::DisableRowLevelSecurity => {
                    self.inner.rls_enabled_tables.write().remove(&query.name);
                    store.set_config_tree(
                        &format!("rls.enabled.{}", query.name),
                        &crate::serde_json::Value::Null,
                    );
                    self.invalidate_plan_cache();
                    messages.push(format!("row level security disabled on '{}'", query.name));
                }
                // Phase 2.5.4: retrofit tenancy onto an existing table.
                AlterOperation::EnableTenancy { column } => {
                    store.set_config_tree(
                        &format!("tenant_tables.{}.column", query.name),
                        &crate::serde_json::Value::String(column.clone()),
                    );
                    self.register_tenant_table(&query.name, column);
                    self.invalidate_plan_cache();
                    messages.push(format!(
                        "tenancy enabled on '{}' by column '{column}'",
                        query.name
                    ));
                }
                AlterOperation::DisableTenancy => {
                    store.set_config_tree(
                        &format!("tenant_tables.{}.column", query.name),
                        &crate::serde_json::Value::Null,
                    );
                    self.unregister_tenant_table(&query.name);
                    self.invalidate_plan_cache();
                    messages.push(format!("tenancy disabled on '{}'", query.name));
                }
                AlterOperation::SetAppendOnly(on) => {
                    // Contract is the single source of truth for the
                    // UPDATE/DELETE parse-time guard. The flag lands
                    // below via `apply_alter_operations_to_contract`;
                    // here we only publish the human-readable message.
                    messages.push(format!(
                        "append_only {} on '{}'",
                        if *on { "enabled" } else { "disabled" },
                        query.name
                    ));
                }
                AlterOperation::SetVersioned(on) => {
                    // Opt the collection into (or out of) Git-for-Data.
                    // Persists a row in red_vcs_settings; next AS OF /
                    // merge / diff against this table honours the
                    // flag. Retroactive: existing row versions whose
                    // xmins are still pinned by commits become
                    // reachable via AS OF COMMIT immediately.
                    self.vcs_set_versioned(&query.name, *on)?;
                    messages.push(format!(
                        "versioned {} on '{}'",
                        if *on { "enabled" } else { "disabled" },
                        query.name
                    ));
                }
                AlterOperation::EnableEvents(subscription) => {
                    let mut subscription = subscription.clone();
                    subscription.source = query.name.clone();
                    validate_event_subscriptions(
                        self,
                        &query.name,
                        std::slice::from_ref(&subscription),
                    )?;
                    ensure_event_target_queue(self, &subscription.target_queue)?;
                    messages.push(format!(
                        "events enabled on '{}' to '{}'",
                        query.name, subscription.target_queue
                    ));
                }
                AlterOperation::DisableEvents => {
                    messages.push(format!("events disabled on '{}'", query.name));
                }
                AlterOperation::AddSubscription { name, descriptor } => {
                    let mut sub = descriptor.clone();
                    sub.name = name.clone();
                    sub.source = query.name.clone();
                    validate_event_subscriptions(self, &query.name, std::slice::from_ref(&sub))?;
                    ensure_event_target_queue(self, &sub.target_queue)?;
                    messages.push(format!(
                        "subscription '{}' added on '{}' to '{}'",
                        name, query.name, sub.target_queue
                    ));
                }
                AlterOperation::DropSubscription { name } => {
                    messages.push(format!(
                        "subscription '{}' dropped on '{}'",
                        name, query.name
                    ));
                }
                AlterOperation::AddSigner { pubkey } => {
                    // Issue #522 — admin-gated registry mutation. The
                    // standard DDL `check_write` above gates by role; we
                    // additionally verify the collection actually has a
                    // signed-writes registry installed so this isn't a
                    // covert way to retrofit one (use `CREATE COLLECTION
                    // ... SIGNED_BY (...)` for that).
                    if !crate::runtime::signed_writes_kind::is_signed(&*store, &query.name)
                    {
                        return Err(RedDBError::Query(format!(
                            "ALTER COLLECTION ADD SIGNER: '{}' has no signer registry; \
                             recreate it with CREATE COLLECTION ... SIGNED_BY (...)",
                            query.name
                        )));
                    }
                    let actor = crate::runtime::impl_core::current_user_projected()
                        .unwrap_or_else(|| "@system/alter".to_string());
                    let changed = crate::runtime::signed_writes_kind::add_signer(
                        &*store, &query.name, *pubkey, &actor,
                    );
                    messages.push(format!(
                        "signer {} on '{}'",
                        if changed { "added" } else { "already present" },
                        query.name
                    ));
                }
                AlterOperation::RevokeSigner { pubkey } => {
                    if !crate::runtime::signed_writes_kind::is_signed(&*store, &query.name)
                    {
                        return Err(RedDBError::Query(format!(
                            "ALTER COLLECTION REVOKE SIGNER: '{}' has no signer registry",
                            query.name
                        )));
                    }
                    let actor = crate::runtime::impl_core::current_user_projected()
                        .unwrap_or_else(|| "@system/alter".to_string());
                    let changed = crate::runtime::signed_writes_kind::revoke_signer(
                        &*store, &query.name, pubkey, &actor,
                    );
                    messages.push(format!(
                        "signer {} on '{}'",
                        if changed { "revoked" } else { "already revoked" },
                        query.name
                    ));
                }
            }
        }

        let mut contract = self
            .inner
            .db
            .collection_contract(&query.name)
            .unwrap_or_else(|| default_collection_contract_for_existing_table(&query.name));
        apply_alter_operations_to_contract(&mut contract, &query.operations);
        contract.version = contract.version.saturating_add(1);
        contract.updated_at_unix_ms = current_unix_ms();
        self.inner
            .db
            .save_collection_contract(contract)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        // Issue #301 — emit OperatorEvent when column schema changes on a
        // collection that has active event subscriptions, so operators know
        // downstream consumers may see a different payload shape.
        if !fields_added.is_empty() || !fields_removed.is_empty() {
            let sub_names: Vec<String> = self
                .inner
                .db
                .collection_contract(&query.name)
                .map(|c| {
                    c.subscriptions
                        .iter()
                        .filter(|s| s.enabled)
                        .map(|s| s.name.clone())
                        .collect()
                })
                .unwrap_or_default();
            if !sub_names.is_empty() {
                crate::telemetry::operator_event::OperatorEvent::SubscriptionSchemaChange {
                    collection: query.name.clone(),
                    subscription_names: sub_names.join(", "),
                    fields_added: fields_added.join(", "),
                    fields_removed: fields_removed.join(", "),
                    lsn: self.cdc_current_lsn(),
                }
                .emit_global();
            }
        }

        self.clear_table_planner_stats(&query.name);
        self.invalidate_result_cache();
        // Issue #120 — refresh the schema-vocabulary entries from the
        // post-ALTER contract. Drop+recreate inside the index keeps
        // the invalidation guarantee complete (no stale columns from
        // before an ALTER ... DROP COLUMN).
        let post_alter_columns: Vec<String> = self
            .inner
            .db
            .collection_contract(&query.name)
            .map(|contract| {
                contract
                    .declared_columns
                    .iter()
                    .map(|col| col.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        self.schema_vocabulary_apply(
            crate::runtime::schema_vocabulary::DdlEvent::AlterCollection {
                collection: query.name.clone(),
                columns: post_alter_columns,
                type_tags: Vec::new(),
                description: None,
            },
        );

        let message = if messages.is_empty() {
            format!("table '{}' altered (no operations)", query.name)
        } else {
            format!("table '{}' altered: {}", query.name, messages.join(", "))
        };

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &message,
            "alter",
        ))
    }

    /// Execute EXPLAIN ALTER FOR CREATE TABLE
    ///
    /// Pure read: computes the schema diff between the target table's
    /// current `CollectionContract` and the embedded `CREATE TABLE` body,
    /// and returns it as SQL `ALTER TABLE` text (default) or structured
    /// JSON. Never mutates storage.
    pub fn execute_explain_alter(
        &self,
        raw_query: &str,
        query: &ExplainAlterQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        // Validate the target CREATE TABLE body so syntactically valid
        // but semantically broken targets (bad SQL types, duplicate
        // columns) are caught here rather than inside the diff engine.
        analyze_create_table(&query.target).map_err(|err| RedDBError::Query(err.to_string()))?;

        let current_contract = self.inner.db.collection_contract(&query.target.name);

        let current_columns: Vec<crate::physical::DeclaredColumnContract> = current_contract
            .as_ref()
            .map(|c| c.declared_columns.clone())
            .unwrap_or_default();

        let diff = super::schema_diff::compute_column_diff(
            &query.target.name,
            &current_columns,
            &query.target.columns,
        );

        let rendered = match query.format {
            ExplainFormat::Sql => super::schema_diff::format_as_sql(&diff),
            ExplainFormat::Json => super::schema_diff::format_as_json(&diff),
        };

        let format_label = match query.format {
            ExplainFormat::Sql => "sql",
            ExplainFormat::Json => "json",
        };

        let columns = vec![
            "table".to_string(),
            "format".to_string(),
            "diff".to_string(),
        ];
        let row = vec![
            ("table".to_string(), Value::text(query.target.name.clone())),
            ("format".to_string(), Value::text(format_label.to_string())),
            ("diff".to_string(), Value::text(rendered)),
        ];

        Ok(RuntimeQueryResult::ok_records(
            raw_query.to_string(),
            columns,
            vec![row],
            "explain",
        ))
    }

    /// Execute CREATE INDEX
    ///
    /// Registers a new index on a collection, builds it from existing data,
    /// and makes it available to the query executor for O(1) lookups.
    pub fn execute_create_index(
        &self,
        raw_query: &str,
        query: &CreateIndexQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        let store = self.inner.db.store();

        // Verify the table exists
        let manager = store
            .get_collection(&query.table)
            .ok_or_else(|| RedDBError::NotFound(format!("table '{}' not found", query.table)))?;

        let method_kind = match query.method {
            IndexMethod::Hash => super::index_store::IndexMethodKind::Hash,
            IndexMethod::BTree => super::index_store::IndexMethodKind::BTree,
            IndexMethod::Bitmap => super::index_store::IndexMethodKind::Bitmap,
            IndexMethod::RTree => super::index_store::IndexMethodKind::Spatial,
        };

        // Extract fields from existing entities for indexing. Row
        // entities may arrive in either the "named" HashMap layout
        // (gRPC `BulkInsertBinary` path) OR the columnar layout
        // (HTTP `POST /collections/X/bulk/rows` fast path, which uses
        // `schema: Some(Arc<Vec<String>>)` + `columns: Vec<Value>` and
        // leaves `named == None`). Prior to this commit the columnar
        // branch returned an empty field list, so `CREATE INDEX` built
        // a zero-entity index over HTTP-inserted data even though the
        // data was queryable via `SELECT`.
        let entities = manager.query_all(|_| true);
        let entity_fields: Vec<(crate::storage::unified::EntityId, Vec<(String, Value)>)> =
            entities
                .iter()
                .map(|e| {
                    let fields = match &e.data {
                        crate::storage::EntityData::Row(row) => {
                            if let Some(ref named) = row.named {
                                named.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                            } else if let Some(ref schema) = row.schema {
                                // Columnar layout — pair each column
                                // with its positional name from the
                                // shared schema Arc.
                                schema
                                    .iter()
                                    .zip(row.columns.iter())
                                    .map(|(k, v)| (k.clone(), v.clone()))
                                    .collect()
                            } else {
                                Vec::new()
                            }
                        }
                        crate::storage::EntityData::Node(node) => node
                            .properties
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        _ => Vec::new(),
                    };
                    (e.id, fields)
                })
                .collect();

        // Build the index
        let indexed_count = self
            .inner
            .index_store
            .create_index(
                &query.name,
                &query.table,
                &query.columns,
                method_kind,
                query.unique,
                &entity_fields,
            )
            .map_err(RedDBError::Internal)?;

        let analyzed = crate::storage::query::planner::stats_catalog::analyze_entity_fields(
            &query.table,
            &entity_fields,
        );
        crate::storage::query::planner::stats_catalog::persist_table_stats(&store, &analyzed);
        self.invalidate_plan_cache();

        // Register metadata
        self.inner
            .index_store
            .register(super::index_store::RegisteredIndex {
                name: query.name.clone(),
                collection: query.table.clone(),
                columns: query.columns.clone(),
                method: method_kind,
                unique: query.unique,
            });
        // Issue #120 — surface the index name + indexed columns in
        // the schema-vocabulary so AskPipeline (#121) can resolve
        // "the email index" back to its collection.
        self.schema_vocabulary_apply(crate::runtime::schema_vocabulary::DdlEvent::CreateIndex {
            collection: query.table.clone(),
            index: query.name.clone(),
            columns: query.columns.clone(),
        });

        let method_str = format!("{}", query.method);
        let unique_str = if query.unique { "unique " } else { "" };
        let cols = query.columns.join(", ");

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!(
                "{}index '{}' created on '{}' ({}) using {} ({} entities indexed)",
                unique_str, query.name, query.table, cols, method_str, indexed_count
            ),
            "create",
        ))
    }

    /// Execute DROP INDEX
    ///
    /// Removes an index from a collection.
    pub fn execute_drop_index(
        &self,
        raw_query: &str,
        query: &DropIndexQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        let store = self.inner.db.store();

        // Verify the table exists
        if store.get_collection(&query.table).is_none() {
            if query.if_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("table '{}' does not exist", query.table),
                    "drop",
                ));
            }
            return Err(RedDBError::NotFound(format!(
                "table '{}' not found",
                query.table
            )));
        }

        // Remove from IndexStore
        self.inner.index_store.drop_index(&query.name, &query.table);
        self.invalidate_plan_cache();
        // Issue #120 — keep the schema-vocabulary index entry in sync.
        self.schema_vocabulary_apply(crate::runtime::schema_vocabulary::DdlEvent::DropIndex {
            collection: query.table.clone(),
            index: query.name.clone(),
        });

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("index '{}' dropped from '{}'", query.name, query.table),
            "drop",
        ))
    }

    fn execute_drop_typed_collection(
        &self,
        raw_query: &str,
        name: &str,
        if_exists: bool,
        expected_model: CollectionModel,
        label: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        if is_system_schema_name(name) {
            return Err(RedDBError::Query("system schema is read-only".to_string()));
        }
        let store = self.inner.db.store();
        if store.get_collection(name).is_none() {
            if if_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("{label} '{name}' does not exist"),
                    "drop",
                ));
            }
            return Err(RedDBError::NotFound(format!("{label} '{name}' not found")));
        }

        let actual = polymorphic_resolver::resolve(name, &self.inner.db.catalog_model_snapshot())?;
        polymorphic_resolver::ensure_model_match(expected_model, actual)?;
        self.drop_collection_storage(raw_query, name, label)
    }

    pub fn execute_truncate(
        &self,
        raw_query: &str,
        query: &TruncateQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        if is_system_schema_name(&query.name) {
            return Err(RedDBError::Query("system schema is read-only".to_string()));
        }

        let label = query
            .model
            .map(polymorphic_resolver::model_name)
            .unwrap_or("collection");
        let store = self.inner.db.store();
        if store.get_collection(&query.name).is_none() {
            if query.if_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("{label} '{}' does not exist", query.name),
                    "truncate",
                ));
            }
            return Err(RedDBError::NotFound(format!(
                "{label} '{}' not found",
                query.name
            )));
        }

        let actual =
            polymorphic_resolver::resolve(&query.name, &self.inner.db.catalog_model_snapshot())?;
        if let Some(expected) = query.model {
            polymorphic_resolver::ensure_model_match(expected, actual)?;
        }

        if actual == CollectionModel::Queue {
            return self.execute_queue_command(
                raw_query,
                &QueueCommand::Purge {
                    queue: query.name.clone(),
                },
            );
        }

        // Count before wiping so we can emit the aggregated truncate event.
        let affected = self.truncate_collection_entities(&query.name)?;
        // Emit 1 truncate event (not N delete events) for event-enabled collections.
        crate::runtime::mutation::emit_truncate_event_for_collection(self, &query.name, affected)?;
        self.inner.db.invalidate_vector_index(&query.name);
        self.clear_table_planner_stats(&query.name);
        self.invalidate_result_cache();

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!(
                "{affected} entities truncated from {label} '{}'",
                query.name
            ),
            "truncate",
        ))
    }

    fn truncate_collection_entities(&self, name: &str) -> RedDBResult<u64> {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(name) else {
            return Ok(0);
        };
        let entities = manager.query_all(|_| true);
        if entities.is_empty() {
            return Ok(0);
        }

        for entity in &entities {
            let fields = entity_index_fields(&entity.data);
            self.inner
                .index_store
                .index_entity_delete(name, entity.id, &fields)
                .map_err(RedDBError::Internal)?;
        }

        let ids = entities.iter().map(|entity| entity.id).collect::<Vec<_>>();
        let deleted_ids = store
            .delete_batch(name, &ids)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        for id in &deleted_ids {
            store.context_index().remove_entity(*id);
        }
        Ok(deleted_ids.len() as u64)
    }

    fn drop_collection_storage(
        &self,
        raw_query: &str,
        name: &str,
        label: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();

        // Emit 1 collection_dropped event before storage is wiped.
        // Queue is preserved; subscription is removed with the contract below.
        let final_count = store
            .get_collection(name)
            .map(|manager| manager.query_all(|_| true).len() as u64)
            .unwrap_or(0);
        crate::runtime::mutation::emit_collection_dropped_event_for_collection(
            self,
            name,
            final_count,
        )?;

        let orphaned_indices: Vec<String> = self
            .inner
            .index_store
            .list_indices(name)
            .into_iter()
            .map(|index| index.name)
            .collect();
        for index_name in &orphaned_indices {
            self.inner.index_store.drop_index(index_name, name);
        }

        store
            .drop_collection(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.inner.db.invalidate_vector_index(name);
        self.inner.db.clear_collection_default_ttl_ms(name);
        self.inner
            .db
            .remove_collection_contract(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.clear_table_planner_stats(name);
        self.invalidate_result_cache();
        if let Some(store) = self.inner.auth_store.read().clone() {
            store.invalidate_visible_collections_cache();
        }
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.schema_vocabulary_apply(
            crate::runtime::schema_vocabulary::DdlEvent::DropCollection {
                collection: name.to_string(),
            },
        );

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("{label} '{name}' dropped"),
            "drop",
        ))
    }
}

pub(crate) fn is_system_schema_name(name: &str) -> bool {
    name == "red" || name.starts_with("red.") || name.starts_with("__red_schema_")
}

fn entity_index_fields(data: &EntityData) -> Vec<(String, Value)> {
    match data {
        EntityData::Row(row) => {
            if let Some(ref named) = row.named {
                named
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            } else if let Some(ref schema) = row.schema {
                schema
                    .iter()
                    .zip(row.columns.iter())
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            } else {
                Vec::new()
            }
        }
        EntityData::Node(node) => node
            .properties
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

fn collection_contract_from_create_table(
    query: &CreateTableQuery,
) -> RedDBResult<crate::physical::CollectionContract> {
    let now = current_unix_ms();
    let mut declared_columns: Vec<crate::physical::DeclaredColumnContract> = query
        .columns
        .iter()
        .map(declared_column_contract_from_ddl)
        .collect();
    if query.timestamps {
        // Opt-in `WITH timestamps = true` auto-adds two user-visible
        // columns that the write path populates from
        // UnifiedEntity::created_at/updated_at. BIGINT unix-ms, NOT NULL.
        declared_columns.push(crate::physical::DeclaredColumnContract {
            name: "created_at".to_string(),
            data_type: "BIGINT".to_string(),
            sql_type: Some(crate::storage::schema::SqlTypeName::simple("BIGINT")),
            not_null: true,
            default: None,
            compress: None,
            unique: false,
            primary_key: false,
            enum_variants: Vec::new(),
            array_element: None,
            decimal_precision: None,
        });
        declared_columns.push(crate::physical::DeclaredColumnContract {
            name: "updated_at".to_string(),
            data_type: "BIGINT".to_string(),
            sql_type: Some(crate::storage::schema::SqlTypeName::simple("BIGINT")),
            not_null: true,
            default: None,
            compress: None,
            unique: false,
            primary_key: false,
            enum_variants: Vec::new(),
            array_element: None,
            decimal_precision: None,
        });
    }
    Ok(crate::physical::CollectionContract {
        name: query.name.clone(),
        declared_model: crate::catalog::CollectionModel::Table,
        schema_mode: crate::catalog::SchemaMode::SemiStructured,
        origin: crate::physical::ContractOrigin::Explicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: query.default_ttl_ms,
        vector_dimension: None,
        vector_metric: None,
        context_index_fields: query.context_index_fields.clone(),
        declared_columns,
        table_def: Some(build_table_def_from_create_table(query)?),
        timestamps_enabled: query.timestamps,
        context_index_enabled: query.context_index_enabled
            || !query.context_index_fields.is_empty(),
        metrics_raw_retention_ms: None,
        metrics_rollup_policies: Vec::new(),
        metrics_tenant_identity: None,
        metrics_namespace: None,
        append_only: query.append_only,
        subscriptions: query.subscriptions.clone(),
        session_key: None,
        session_gap_ms: None,
    })
}

fn default_collection_contract_for_existing_table(
    name: &str,
) -> crate::physical::CollectionContract {
    let now = current_unix_ms();
    crate::physical::CollectionContract {
        name: name.to_string(),
        declared_model: crate::catalog::CollectionModel::Table,
        schema_mode: crate::catalog::SchemaMode::SemiStructured,
        origin: crate::physical::ContractOrigin::Explicit,
        version: 0,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: None,
        vector_dimension: None,
        vector_metric: None,
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def: Some(crate::storage::schema::TableDef::new(name.to_string())),
        timestamps_enabled: false,
        context_index_enabled: false,
        metrics_raw_retention_ms: None,
        metrics_rollup_policies: Vec::new(),
        metrics_tenant_identity: None,
        metrics_namespace: None,
        append_only: false,
        subscriptions: Vec::new(),
        session_key: None,
        session_gap_ms: None,
    }
}

fn keyed_collection_contract(
    name: &str,
    model: crate::catalog::CollectionModel,
) -> crate::physical::CollectionContract {
    let now = current_unix_ms();
    crate::physical::CollectionContract {
        name: name.to_string(),
        declared_model: model,
        schema_mode: crate::catalog::SchemaMode::Dynamic,
        origin: crate::physical::ContractOrigin::Explicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: None,
        vector_dimension: None,
        vector_metric: None,
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def: None,
        timestamps_enabled: false,
        context_index_enabled: false,
        metrics_raw_retention_ms: None,
        metrics_rollup_policies: Vec::new(),
        metrics_tenant_identity: None,
        metrics_namespace: None,
        append_only: false,
        subscriptions: Vec::new(),
        session_key: None,
        session_gap_ms: None,
    }
}

fn metrics_collection_contract(query: &CreateTableQuery) -> crate::physical::CollectionContract {
    let now = current_unix_ms();
    crate::physical::CollectionContract {
        name: query.name.clone(),
        declared_model: crate::catalog::CollectionModel::Metrics,
        schema_mode: crate::catalog::SchemaMode::SemiStructured,
        origin: crate::physical::ContractOrigin::Explicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: query.default_ttl_ms,
        vector_dimension: None,
        vector_metric: None,
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def: None,
        timestamps_enabled: false,
        context_index_enabled: false,
        metrics_raw_retention_ms: query.default_ttl_ms,
        metrics_rollup_policies: query.metrics_rollup_policies.clone(),
        metrics_tenant_identity: Some(
            query
                .tenant_by
                .clone()
                .unwrap_or_else(|| "current_tenant".to_string()),
        ),
        metrics_namespace: Some("default".to_string()),
        append_only: true,
        subscriptions: Vec::new(),
        session_key: None,
        session_gap_ms: None,
    }
}

fn vector_collection_contract(query: &CreateVectorQuery) -> crate::physical::CollectionContract {
    let now = current_unix_ms();
    crate::physical::CollectionContract {
        name: query.name.clone(),
        declared_model: crate::catalog::CollectionModel::Vector,
        schema_mode: crate::catalog::SchemaMode::Dynamic,
        origin: crate::physical::ContractOrigin::Explicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: None,
        vector_dimension: Some(query.dimension),
        vector_metric: Some(query.metric),
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def: None,
        timestamps_enabled: false,
        context_index_enabled: false,
        metrics_raw_retention_ms: None,
        metrics_rollup_policies: Vec::new(),
        metrics_tenant_identity: None,
        metrics_namespace: None,
        append_only: false,
        subscriptions: Vec::new(),
        session_key: None,
        session_gap_ms: None,
    }
}

fn declared_column_contract_from_ddl(
    column: &CreateColumnDef,
) -> crate::physical::DeclaredColumnContract {
    crate::physical::DeclaredColumnContract {
        name: column.name.clone(),
        data_type: column.data_type.clone(),
        sql_type: Some(column.sql_type.clone()),
        not_null: column.not_null,
        default: column.default.clone(),
        compress: column.compress,
        unique: column.unique,
        primary_key: column.primary_key,
        enum_variants: column.enum_variants.clone(),
        array_element: column.array_element.clone(),
        decimal_precision: column.decimal_precision,
    }
}

fn apply_alter_operations_to_contract(
    contract: &mut crate::physical::CollectionContract,
    operations: &[AlterOperation],
) {
    if contract.table_def.is_none() {
        contract.table_def = Some(crate::storage::schema::TableDef::new(contract.name.clone()));
    }
    for operation in operations {
        match operation {
            AlterOperation::AddColumn(column) => {
                if !contract
                    .declared_columns
                    .iter()
                    .any(|existing| existing.name == column.name)
                {
                    contract
                        .declared_columns
                        .push(declared_column_contract_from_ddl(column));
                }
                if let Some(table_def) = contract.table_def.as_mut() {
                    if table_def.get_column(&column.name).is_none() {
                        if let Ok(column_def) = column_def_from_ddl(column) {
                            if column.primary_key {
                                table_def.primary_key.push(column.name.clone());
                                table_def.constraints.push(
                                    crate::storage::schema::Constraint::new(
                                        format!("pk_{}", column.name),
                                        crate::storage::schema::ConstraintType::PrimaryKey,
                                    )
                                    .on_columns(vec![column.name.clone()]),
                                );
                            }
                            if column.unique {
                                table_def.constraints.push(
                                    crate::storage::schema::Constraint::new(
                                        format!("uniq_{}", column.name),
                                        crate::storage::schema::ConstraintType::Unique,
                                    )
                                    .on_columns(vec![column.name.clone()]),
                                );
                            }
                            if column.not_null {
                                table_def.constraints.push(
                                    crate::storage::schema::Constraint::new(
                                        format!("not_null_{}", column.name),
                                        crate::storage::schema::ConstraintType::NotNull,
                                    )
                                    .on_columns(vec![column.name.clone()]),
                                );
                            }
                            table_def.columns.push(column_def);
                        }
                    }
                }
            }
            AlterOperation::DropColumn(name) => {
                contract
                    .declared_columns
                    .retain(|column| column.name != *name);
                if let Some(table_def) = contract.table_def.as_mut() {
                    if let Some(index) = table_def.column_index(name) {
                        table_def.columns.remove(index);
                    }
                    table_def.primary_key.retain(|column| column != name);
                    table_def.constraints.retain(|constraint| {
                        !constraint.columns.iter().any(|column| column == name)
                    });
                    table_def
                        .indexes
                        .retain(|index| !index.columns.iter().any(|column| column == name));
                }
            }
            AlterOperation::RenameColumn { from, to } => {
                if contract
                    .declared_columns
                    .iter()
                    .any(|column| column.name == *to)
                {
                    continue;
                }
                if let Some(column) = contract
                    .declared_columns
                    .iter_mut()
                    .find(|column| column.name == *from)
                {
                    column.name = to.clone();
                }
                if let Some(table_def) = contract.table_def.as_mut() {
                    if let Some(column) = table_def
                        .columns
                        .iter_mut()
                        .find(|column| column.name == *from)
                    {
                        column.name = to.clone();
                    }
                    for primary_key in &mut table_def.primary_key {
                        if *primary_key == *from {
                            *primary_key = to.clone();
                        }
                    }
                    for constraint in &mut table_def.constraints {
                        for column in &mut constraint.columns {
                            if *column == *from {
                                *column = to.clone();
                            }
                        }
                        if let Some(ref_columns) = constraint.ref_columns.as_mut() {
                            for column in ref_columns {
                                if *column == *from {
                                    *column = to.clone();
                                }
                            }
                        }
                    }
                    for index in &mut table_def.indexes {
                        for column in &mut index.columns {
                            if *column == *from {
                                *column = to.clone();
                            }
                        }
                    }
                }
            }
            // Partition ops don't touch the column contract — metadata is
            // persisted separately via `red_config.partition.*`.
            AlterOperation::AttachPartition { .. } | AlterOperation::DetachPartition { .. } => {}
            // RLS toggles don't touch the column contract — flag is persisted
            // separately via `red_config.rls.enabled.{table}` and enforced
            // through the in-memory `rls_enabled_tables` set.
            AlterOperation::EnableRowLevelSecurity | AlterOperation::DisableRowLevelSecurity => {}
            // Phase 2.5.4: tenancy toggles persist via `red_config.tenant_tables.*`
            // and are enforced through `tenant_tables` + RLS auto-policy.
            AlterOperation::EnableTenancy { .. } | AlterOperation::DisableTenancy => {}
            AlterOperation::SetAppendOnly(on) => {
                contract.append_only = *on;
            }
            // VCS opt-in is persisted to red_vcs_settings by the
            // executor, not the contract — nothing to do here.
            AlterOperation::SetVersioned(_) => {}
            AlterOperation::EnableEvents(subscription) => {
                let mut subscription = subscription.clone();
                subscription.source = contract.name.clone();
                subscription.enabled = true;
                if let Some(existing) = contract
                    .subscriptions
                    .iter_mut()
                    .find(|existing| existing.target_queue == subscription.target_queue)
                {
                    *existing = subscription;
                } else {
                    contract.subscriptions.push(subscription);
                }
            }
            AlterOperation::DisableEvents => {
                for subscription in &mut contract.subscriptions {
                    subscription.enabled = false;
                }
            }
            AlterOperation::AddSubscription { name, descriptor } => {
                let mut sub = descriptor.clone();
                sub.name = name.clone();
                sub.source = contract.name.clone();
                sub.enabled = true;
                if let Some(existing) = contract.subscriptions.iter_mut().find(|s| s.name == *name)
                {
                    *existing = sub;
                } else {
                    contract.subscriptions.push(sub);
                }
            }
            AlterOperation::DropSubscription { name } => {
                contract.subscriptions.retain(|s| s.name != *name);
            }
            // Signer registry mutations live in `red_config` outside the
            // contract surface — the executor applied them directly via
            // `signed_writes_kind::{add,revoke}_signer`. Nothing to fold
            // into the column-shaped contract.
            AlterOperation::AddSigner { .. } | AlterOperation::RevokeSigner { .. } => {}
        }
    }
}

fn validate_event_subscriptions(
    runtime: &RedDBRuntime,
    source: &str,
    subscriptions: &[crate::catalog::SubscriptionDescriptor],
) -> RedDBResult<()> {
    for subscription in subscriptions
        .iter()
        .filter(|subscription| subscription.enabled)
    {
        if subscription.all_tenants && crate::runtime::impl_core::current_tenant().is_some() {
            return Err(RedDBError::Query(
                "cross-tenant subscription requires cluster-admin capability (events:cluster_subscribe)".to_string(),
            ));
        }
        validate_subscription_auth(runtime, source, subscription)?;
        if subscription.target_queue == source
            || subscription_would_create_cycle(
                &runtime.inner.db,
                source,
                &subscription.target_queue,
            )
        {
            return Err(RedDBError::Query(
                "subscription would create cycle".to_string(),
            ));
        }
        audit_subscription_redact_gap(runtime, source, subscription);
    }
    Ok(())
}

fn validate_subscription_auth(
    runtime: &RedDBRuntime,
    source: &str,
    subscription: &crate::catalog::SubscriptionDescriptor,
) -> RedDBResult<()> {
    let auth_store = match runtime.inner.auth_store.read().clone() {
        Some(store) => store,
        None => return Ok(()),
    };
    let (username, role) = match crate::runtime::impl_core::current_auth_identity() {
        Some(identity) => identity,
        None => return Ok(()),
    };
    let tenant = crate::runtime::impl_core::current_tenant();
    let principal = crate::auth::UserId::from_parts(tenant.as_deref(), &username);

    if auth_store.iam_authorization_enabled() {
        let ctx = crate::auth::policies::EvalContext {
            principal_tenant: tenant.clone(),
            current_tenant: tenant.clone(),
            peer_ip: None,
            mfa_present: false,
            now_ms: crate::auth::now_ms(),
            principal_is_admin_role: role == crate::auth::Role::Admin,
        };
        let mut source_resource = crate::auth::policies::ResourceRef::new("table", source);
        if let Some(t) = tenant.as_deref() {
            source_resource = source_resource.with_tenant(t.to_string());
        }
        if !auth_store.check_policy_authz(&principal, "select", &source_resource, &ctx) {
            return Err(RedDBError::Query(format!(
                "permission denied: principal=`{}` action=`select` resource=`{}:{}` denied by IAM policy",
                principal, source_resource.kind, source_resource.name
            )));
        }

        let mut target_resource =
            crate::auth::policies::ResourceRef::new("queue", subscription.target_queue.clone());
        if let Some(t) = tenant.as_deref() {
            target_resource = target_resource.with_tenant(t.to_string());
        }
        if !auth_store.check_policy_authz(&principal, "write", &target_resource, &ctx) {
            return Err(RedDBError::Query(format!(
                "permission denied: principal=`{}` action=`write` resource=`{}:{}` denied by IAM policy",
                principal, target_resource.kind, target_resource.name
            )));
        }
        return Ok(());
    }

    let ctx = crate::auth::privileges::AuthzContext {
        principal: &username,
        effective_role: role,
        tenant: tenant.as_deref(),
    };
    auth_store
        .check_grant(
            &ctx,
            crate::auth::privileges::Action::Select,
            &crate::auth::privileges::Resource::table_from_name(source),
        )
        .map_err(|err| RedDBError::Query(format!("permission denied: {err}")))?;
    auth_store
        .check_grant(
            &ctx,
            crate::auth::privileges::Action::Insert,
            &crate::auth::privileges::Resource::table_from_name(&subscription.target_queue),
        )
        .map_err(|err| RedDBError::Query(format!("permission denied: {err}")))?;
    Ok(())
}

fn audit_subscription_redact_gap(
    runtime: &RedDBRuntime,
    source: &str,
    subscription: &crate::catalog::SubscriptionDescriptor,
) {
    let auth_store = match runtime.inner.auth_store.read().clone() {
        Some(store) if store.iam_authorization_enabled() => store,
        _ => return,
    };
    let (username, role) = match crate::runtime::impl_core::current_auth_identity() {
        Some(identity) => identity,
        None => return,
    };
    let tenant = crate::runtime::impl_core::current_tenant();
    let principal = crate::auth::UserId::from_parts(tenant.as_deref(), &username);
    let missing = subscription_redact_gap_columns(&auth_store, &principal, source, subscription);
    if missing.is_empty() {
        return;
    }

    let columns = missing.into_iter().collect::<Vec<_>>().join(", ");
    tracing::warn!(
        target: "reddb::operator",
        "subscription_redact_gap: source={} target_queue={} columns=[{}]",
        source,
        subscription.target_queue,
        columns
    );
    let mut event = AuditEvent::builder("subscription_redact_gap")
        .principal(username)
        .source(AuditAuthSource::System)
        .resource(format!(
            "subscription:{}->{}",
            source, subscription.target_queue
        ))
        .outcome(Outcome::Success)
        .field(AuditFieldEscaper::field("source", source))
        .field(AuditFieldEscaper::field(
            "target_queue",
            subscription.target_queue.clone(),
        ))
        .field(AuditFieldEscaper::field(
            "subscription",
            subscription.name.clone(),
        ))
        .field(AuditFieldEscaper::field("columns", columns))
        .field(AuditFieldEscaper::field("role", role.as_str()));
    if let Some(t) = tenant {
        event = event.tenant(t);
    }
    runtime.inner.audit_log.record_event(event.build());
}

fn subscription_redact_gap_columns(
    auth_store: &crate::auth::store::AuthStore,
    principal: &crate::auth::UserId,
    source: &str,
    subscription: &crate::catalog::SubscriptionDescriptor,
) -> BTreeSet<String> {
    let redacted: HashSet<String> = subscription
        .redact_fields
        .iter()
        .map(|field| field.to_ascii_lowercase())
        .collect();
    auth_store
        .effective_policies(principal)
        .iter()
        .flat_map(|policy| policy.statements.iter())
        .filter(|statement| statement.effect == crate::auth::policies::Effect::Deny)
        .filter(|statement| statement.actions.iter().any(action_pattern_matches_select))
        .flat_map(|statement| statement.resources.iter())
        .filter_map(|resource| denied_column_for_source(resource, source))
        .filter(|column| !redact_covers_column(&redacted, source, column))
        .collect()
}

fn action_pattern_matches_select(pattern: &crate::auth::policies::ActionPattern) -> bool {
    match pattern {
        crate::auth::policies::ActionPattern::Wildcard => true,
        crate::auth::policies::ActionPattern::Exact(action) => action == "select",
        crate::auth::policies::ActionPattern::Prefix(prefix) => {
            "select".len() > prefix.len() + 1
                && "select".starts_with(prefix)
                && "select".as_bytes()[prefix.len()] == b':'
        }
    }
}

fn denied_column_for_source(
    resource: &crate::auth::policies::ResourcePattern,
    source: &str,
) -> Option<String> {
    let crate::auth::policies::ResourcePattern::Exact { kind, name } = resource else {
        return None;
    };
    if kind != "column" {
        return None;
    }
    let column = crate::auth::ColumnRef::parse_resource_name(name).ok()?;
    (column.table_resource_name() == source).then_some(column.column)
}

fn redact_covers_column(redacted: &HashSet<String>, source: &str, column: &str) -> bool {
    let column = column.to_ascii_lowercase();
    let qualified = format!("{}.{}", source.to_ascii_lowercase(), column);
    redacted.contains("*") || redacted.contains(&column) || redacted.contains(&qualified)
}

fn subscription_would_create_cycle(
    db: &crate::storage::unified::devx::RedDB,
    source: &str,
    target: &str,
) -> bool {
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    for contract in db.collection_contracts() {
        for subscription in contract
            .subscriptions
            .into_iter()
            .filter(|subscription| subscription.enabled)
        {
            graph
                .entry(subscription.source)
                .or_default()
                .push(subscription.target_queue);
        }
    }
    graph
        .entry(source.to_string())
        .or_default()
        .push(target.to_string());

    let mut stack = vec![target.to_string()];
    let mut seen = HashSet::new();
    while let Some(node) = stack.pop() {
        if node == source {
            return true;
        }
        if !seen.insert(node.clone()) {
            continue;
        }
        if let Some(next) = graph.get(&node) {
            stack.extend(next.iter().cloned());
        }
    }
    false
}

pub(crate) fn ensure_event_target_queue_pub(
    runtime: &RedDBRuntime,
    queue: &str,
) -> RedDBResult<()> {
    ensure_event_target_queue(runtime, queue)
}

fn ensure_event_target_queue(runtime: &RedDBRuntime, queue: &str) -> RedDBResult<()> {
    let store = runtime.inner.db.store();
    if store.get_collection(queue).is_some() {
        return Ok(());
    }
    store
        .create_collection(queue)
        .map_err(|err| RedDBError::Internal(err.to_string()))?;
    runtime
        .inner
        .db
        .save_collection_contract(event_queue_collection_contract(queue))
        .map_err(|err| RedDBError::Internal(err.to_string()))?;
    store.set_config_tree(
        &format!("queue.{queue}.mode"),
        &crate::serde_json::Value::String("fanout".to_string()),
    );
    Ok(())
}

fn event_queue_collection_contract(queue: &str) -> crate::physical::CollectionContract {
    let now = current_unix_ms();
    crate::physical::CollectionContract {
        name: queue.to_string(),
        declared_model: crate::catalog::CollectionModel::Queue,
        schema_mode: crate::catalog::SchemaMode::Dynamic,
        origin: crate::physical::ContractOrigin::Implicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: None,
        vector_dimension: None,
        vector_metric: None,
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def: None,
        timestamps_enabled: false,
        context_index_enabled: false,
        metrics_raw_retention_ms: None,
        metrics_rollup_policies: Vec::new(),
        metrics_tenant_identity: None,
        metrics_namespace: None,
        append_only: true,
        subscriptions: Vec::new(),
        session_key: None,
        session_gap_ms: None,
    }
}

fn build_table_def_from_create_table(
    query: &CreateTableQuery,
) -> RedDBResult<crate::storage::schema::TableDef> {
    let mut table = crate::storage::schema::TableDef::new(query.name.clone());
    for column in &query.columns {
        if column.primary_key {
            table.primary_key.push(column.name.clone());
            table.constraints.push(
                crate::storage::schema::Constraint::new(
                    format!("pk_{}", column.name),
                    crate::storage::schema::ConstraintType::PrimaryKey,
                )
                .on_columns(vec![column.name.clone()]),
            );
        }
        if column.unique {
            table.constraints.push(
                crate::storage::schema::Constraint::new(
                    format!("uniq_{}", column.name),
                    crate::storage::schema::ConstraintType::Unique,
                )
                .on_columns(vec![column.name.clone()]),
            );
        }
        if column.not_null {
            table.constraints.push(
                crate::storage::schema::Constraint::new(
                    format!("not_null_{}", column.name),
                    crate::storage::schema::ConstraintType::NotNull,
                )
                .on_columns(vec![column.name.clone()]),
            );
        }
        table.columns.push(column_def_from_ddl(column)?);
    }
    // WITH timestamps = true: append the two runtime-managed columns
    // to the schema so resolved_contract_columns exposes them to the
    // normalize/validate path. Declared as UnsignedInteger (unix-ms),
    // not-nullable; the write path auto-fills them.
    if query.timestamps {
        table.columns.push(
            crate::storage::schema::ColumnDef::new(
                "created_at".to_string(),
                crate::storage::schema::DataType::UnsignedInteger,
            )
            .not_null(),
        );
        table.columns.push(
            crate::storage::schema::ColumnDef::new(
                "updated_at".to_string(),
                crate::storage::schema::DataType::UnsignedInteger,
            )
            .not_null(),
        );
        table.constraints.push(
            crate::storage::schema::Constraint::new(
                "not_null_created_at".to_string(),
                crate::storage::schema::ConstraintType::NotNull,
            )
            .on_columns(vec!["created_at".to_string()]),
        );
        table.constraints.push(
            crate::storage::schema::Constraint::new(
                "not_null_updated_at".to_string(),
                crate::storage::schema::ConstraintType::NotNull,
            )
            .on_columns(vec!["updated_at".to_string()]),
        );
    }
    table
        .validate()
        .map_err(|err| RedDBError::Query(format!("invalid table definition: {err}")))?;
    Ok(table)
}

fn column_def_from_ddl(column: &CreateColumnDef) -> RedDBResult<crate::storage::schema::ColumnDef> {
    let data_type = resolve_declared_data_type(&column.data_type)
        .map_err(|err| RedDBError::Query(err.to_string()))?;
    let mut column_def = crate::storage::schema::ColumnDef::new(column.name.clone(), data_type);
    if column.not_null {
        column_def = column_def.not_null();
    }
    if let Some(default) = &column.default {
        column_def = column_def.with_default(default.as_bytes().to_vec());
    }
    if column.compress.unwrap_or(0) > 0 {
        column_def = column_def.compressed();
    }
    if !column.enum_variants.is_empty() {
        column_def = column_def.with_variants(column.enum_variants.clone());
    }
    if let Some(precision) = column.decimal_precision {
        column_def = column_def.with_precision(precision);
    }
    if let Some(element_type) = &column.array_element {
        column_def = column_def.with_element_type(
            resolve_declared_data_type(element_type)
                .map_err(|err| RedDBError::Query(err.to_string()))?,
        );
    }
    column_def = column_def.with_metadata("ddl_data_type", column.data_type.clone());
    if column.unique {
        column_def = column_def.with_metadata("unique", "true");
    }
    if column.primary_key {
        column_def = column_def.with_metadata("primary_key", "true");
    }
    Ok(column_def)
}

fn current_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use crate::auth::policies::{ActionPattern, Effect, Policy, ResourcePattern, Statement};
    use crate::auth::store::{AuthStore, PrincipalRef};
    use crate::auth::UserId;
    use crate::auth::{AuthConfig, Role};
    use crate::runtime::impl_core::{clear_current_auth_identity, set_current_auth_identity};
    use crate::storage::schema::Value;
    use crate::{RedDBOptions, RedDBRuntime};
    use std::sync::Arc;

    fn make_allow_policy(id: &str, action: &str, collection: &str) -> Policy {
        Policy {
            id: id.to_string(),
            version: 1,
            tenant: None,
            created_at: 0,
            updated_at: 0,
            statements: vec![Statement {
                sid: None,
                effect: Effect::Allow,
                actions: vec![ActionPattern::Exact(action.to_string())],
                resources: vec![ResourcePattern::Exact {
                    kind: "collection".to_string(),
                    name: collection.to_string(),
                }],
                condition: None,
            }],
        }
    }

    fn wire_auth_store(rt: &RedDBRuntime) -> Arc<AuthStore> {
        let store = Arc::new(AuthStore::new(AuthConfig::default()));
        *rt.inner.auth_store.write() = Some(store.clone());
        store
    }

    #[test]
    fn drop_denied_without_iam_policy() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE foo (id INT)").unwrap();
        let store = wire_auth_store(&rt);
        // Put a select-only policy so IAM mode activates, but give alice no drop policy.
        let select_only = Policy {
            id: "select-only".to_string(),
            version: 1,
            tenant: None,
            created_at: 0,
            updated_at: 0,
            statements: vec![Statement {
                sid: None,
                effect: Effect::Allow,
                actions: vec![ActionPattern::Exact("select".to_string())],
                resources: vec![ResourcePattern::Wildcard],
                condition: None,
            }],
        };
        store.put_policy_internal(select_only).unwrap();
        let alice = UserId::from_parts(None, "alice");
        store
            .attach_policy(PrincipalRef::User(alice), "select-only")
            .unwrap();
        set_current_auth_identity("alice".to_string(), Role::Write);
        let err = rt.execute_query("DROP TABLE foo").unwrap_err();
        clear_current_auth_identity();
        assert!(
            format!("{err}").contains("denied by IAM policy"),
            "got: {err}"
        );
    }

    #[test]
    fn drop_allowed_with_explicit_iam_policy() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE bar (id INT)").unwrap();
        let store = wire_auth_store(&rt);
        let policy = make_allow_policy("allow-drop-bar", "drop", "bar");
        store.put_policy_internal(policy).unwrap();
        let bob = UserId::from_parts(None, "bob");
        store
            .attach_policy(PrincipalRef::User(bob), "allow-drop-bar")
            .unwrap();
        set_current_auth_identity("bob".to_string(), Role::Write);
        rt.execute_query("DROP TABLE bar").unwrap();
        clear_current_auth_identity();
    }

    #[test]
    fn drop_allowed_with_wildcard_iam_policy() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE baz (id INT)").unwrap();
        let store = wire_auth_store(&rt);
        let policy = Policy {
            id: "allow-drop-all".to_string(),
            version: 1,
            tenant: None,
            created_at: 0,
            updated_at: 0,
            statements: vec![Statement {
                sid: None,
                effect: Effect::Allow,
                actions: vec![ActionPattern::Exact("drop".to_string())],
                resources: vec![ResourcePattern::Wildcard],
                condition: None,
            }],
        };
        store.put_policy_internal(policy).unwrap();
        let carl = UserId::from_parts(None, "carl");
        store
            .attach_policy(PrincipalRef::User(carl), "allow-drop-all")
            .unwrap();
        set_current_auth_identity("carl".to_string(), Role::Write);
        rt.execute_query("DROP TABLE baz").unwrap();
        clear_current_auth_identity();
    }

    #[test]
    fn truncate_denied_without_iam_policy() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE qux (id INT)").unwrap();
        let store = wire_auth_store(&rt);
        // A policy exists (IAM active) but gives no truncate right.
        let select_only = Policy {
            id: "select-only-2".to_string(),
            version: 1,
            tenant: None,
            created_at: 0,
            updated_at: 0,
            statements: vec![Statement {
                sid: None,
                effect: Effect::Allow,
                actions: vec![ActionPattern::Exact("select".to_string())],
                resources: vec![ResourcePattern::Wildcard],
                condition: None,
            }],
        };
        store.put_policy_internal(select_only).unwrap();
        let dana = UserId::from_parts(None, "dana");
        store
            .attach_policy(PrincipalRef::User(dana), "select-only-2")
            .unwrap();
        set_current_auth_identity("dana".to_string(), Role::Write);
        let err = rt.execute_query("TRUNCATE TABLE qux").unwrap_err();
        clear_current_auth_identity();
        assert!(
            format!("{err}").contains("denied by IAM policy"),
            "got: {err}"
        );
    }

    #[test]
    fn truncate_table_clears_rows_and_preserves_schema_and_indexes() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, name TEXT)")
            .unwrap();
        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'ana'), (2, 'bob')")
            .unwrap();
        rt.execute_query("CREATE INDEX idx_users_id ON users (id) USING HASH")
            .unwrap();

        let truncated = rt.execute_query("TRUNCATE TABLE users").unwrap();
        assert_eq!(truncated.statement_type, "truncate");
        assert_eq!(truncated.affected_rows, 0);

        let empty = rt.execute_query("SELECT id FROM users").unwrap();
        assert!(empty.result.records.is_empty());

        rt.execute_query("INSERT INTO users (id, name) VALUES (3, 'cy')")
            .unwrap();
        let selected = rt
            .execute_query("SELECT name FROM users WHERE id = 3")
            .unwrap();
        let name = selected.result.records[0].get("name").unwrap();
        assert_eq!(name, &Value::text("cy"));
        assert!(rt.db().collection_contract("users").is_some());
        assert!(rt
            .inner
            .index_store
            .list_indices("users")
            .iter()
            .any(|index| index.name == "idx_users_id"));
    }

    #[test]
    fn truncate_collection_is_polymorphic_and_typed_mismatch_fails() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE QUEUE tasks").unwrap();
        rt.execute_query("QUEUE PUSH tasks {'job':'a'}").unwrap();

        let err = rt.execute_query("TRUNCATE TABLE tasks").unwrap_err();
        assert!(format!("{err}").contains("model mismatch: expected table, got queue"));

        rt.execute_query("TRUNCATE COLLECTION tasks").unwrap();
        let len = rt.execute_query("QUEUE LEN tasks").unwrap();
        assert_eq!(
            len.result.records[0].get("len"),
            Some(&Value::UnsignedInteger(0))
        );
    }

    #[test]
    fn truncate_system_schema_is_read_only() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        let err = rt
            .execute_query("TRUNCATE COLLECTION red.collections")
            .unwrap_err();
        assert!(format!("{err}").contains("system schema is read-only"));
    }

    // ── #302 / #310: TRUNCATE / DROP single-event semantics ────────────────

    fn queue_payloads(rt: &RedDBRuntime, queue: &str) -> Vec<crate::json::Value> {
        let result = rt
            .execute_query(&format!("QUEUE PEEK {queue} 100"))
            .expect("peek queue");
        result
            .result
            .records
            .iter()
            .map(
                |record| match record.get("payload").expect("payload column") {
                    Value::Json(bytes) => crate::json::from_slice(bytes).expect("json payload"),
                    other => panic!("expected JSON queue payload, got {other:?}"),
                },
            )
            .collect()
    }

    /// `TRUNCATE users` on an event-enabled collection emits exactly 1
    /// `truncate` event, not one delete event per row.
    #[test]
    fn truncate_event_enabled_table_emits_single_truncate_event() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, name TEXT) WITH EVENTS TO users_events")
            .unwrap();
        rt.execute_query(
            "INSERT INTO users (id, name) VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
        )
        .unwrap();

        // Drain the 3 insert events so we start clean.
        rt.execute_query("QUEUE POP users_events COUNT 10").unwrap();

        rt.execute_query("TRUNCATE TABLE users").unwrap();

        let events = queue_payloads(&rt, "users_events");
        // Must be exactly 1 truncate event, not 3 delete events.
        assert_eq!(
            events.len(),
            1,
            "expected 1 truncate event, got {}",
            events.len()
        );
        let ev = events[0].as_object().expect("event is object");
        assert_eq!(
            ev.get("op").and_then(crate::json::Value::as_str),
            Some("truncate")
        );
        assert_eq!(
            ev.get("collection").and_then(crate::json::Value::as_str),
            Some("users")
        );
        assert_eq!(
            ev.get("entities_count")
                .and_then(crate::json::Value::as_u64),
            Some(3)
        );
        assert!(ev.get("ts").and_then(crate::json::Value::as_u64).is_some());
        assert!(ev.get("lsn").and_then(crate::json::Value::as_u64).is_some());
        assert!(ev
            .get("event_id")
            .and_then(crate::json::Value::as_str)
            .is_some_and(|s| !s.is_empty()));
    }

    /// `TRUNCATE users` on a collection without event subscription emits no events.
    #[test]
    fn truncate_no_events_collection_emits_nothing() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE plain (id INT, val TEXT)")
            .unwrap();
        rt.execute_query("INSERT INTO plain (id, val) VALUES (1, 'a'), (2, 'b')")
            .unwrap();
        // No EVENTS subscription — truncate must work without touching any queue.
        rt.execute_query("TRUNCATE TABLE plain").unwrap();
        // No crash, no queue to check. Just verify truncation happened.
        let rows = rt.execute_query("SELECT id FROM plain").unwrap();
        assert!(rows.result.records.is_empty());
    }

    /// `DROP TABLE users` on an event-enabled collection emits exactly 1
    /// `collection_dropped` event. The subscription is removed from the
    /// source contract but the target queue is preserved for consumer drain.
    #[test]
    fn drop_event_enabled_table_emits_single_collection_dropped_event() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, name TEXT) WITH EVENTS TO users_events")
            .unwrap();
        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'alice'), (2, 'bob')")
            .unwrap();

        // Drain insert events so we start clean.
        rt.execute_query("QUEUE POP users_events COUNT 10").unwrap();

        rt.execute_query("DROP TABLE users").unwrap();

        // Queue must still exist with 1 collection_dropped event.
        let events = queue_payloads(&rt, "users_events");
        assert_eq!(
            events.len(),
            1,
            "expected 1 collection_dropped event, got {}",
            events.len()
        );
        let ev = events[0].as_object().expect("event is object");
        assert_eq!(
            ev.get("op").and_then(crate::json::Value::as_str),
            Some("collection_dropped")
        );
        assert_eq!(
            ev.get("collection").and_then(crate::json::Value::as_str),
            Some("users")
        );
        assert_eq!(
            ev.get("final_entities_count")
                .and_then(crate::json::Value::as_u64),
            Some(2)
        );
        assert!(ev.get("ts").and_then(crate::json::Value::as_u64).is_some());
        assert!(ev.get("lsn").and_then(crate::json::Value::as_u64).is_some());
        assert!(ev
            .get("event_id")
            .and_then(crate::json::Value::as_str)
            .is_some_and(|s| !s.is_empty()));

        // Source collection is gone.
        let err = rt.execute_query("SELECT id FROM users").unwrap_err();
        assert!(
            format!("{err}").contains("users"),
            "expected not-found error"
        );
    }

    /// `DROP TABLE users` on a collection without event subscription works
    /// normally with no event emitted.
    #[test]
    fn drop_no_events_collection_emits_nothing() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE plain (id INT, val TEXT)")
            .unwrap();
        rt.execute_query("INSERT INTO plain (id, val) VALUES (1, 'a')")
            .unwrap();
        rt.execute_query("DROP TABLE plain").unwrap();
        // No crash and collection is gone.
        let err = rt.execute_query("SELECT id FROM plain").unwrap_err();
        assert!(format!("{err}").contains("plain"));
    }

    // ── #297: ops_filter + WHERE filter ────────────────────────────────────

    /// `WITH EVENTS (INSERT)` — UPDATE and DELETE events must NOT be emitted.
    #[test]
    fn ops_filter_insert_only_ignores_update_and_delete() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query(
            "CREATE TABLE items (id INT, val TEXT) WITH EVENTS (INSERT) TO items_events",
        )
        .unwrap();
        rt.execute_query("INSERT INTO items (id, val) VALUES (1, 'a')")
            .unwrap();
        rt.execute_query("UPDATE items SET val = 'b' WHERE id = 1")
            .unwrap();
        rt.execute_query("DELETE FROM items WHERE id = 1").unwrap();

        let events = queue_payloads(&rt, "items_events");
        // Only the INSERT should have fired.
        assert_eq!(
            events.len(),
            1,
            "expected 1 insert event, got {}",
            events.len()
        );
        assert_eq!(
            events[0]
                .as_object()
                .unwrap()
                .get("op")
                .and_then(crate::json::Value::as_str),
            Some("insert")
        );
    }

    /// `WITH EVENTS WHERE status = 'active'` — only rows matching the predicate generate events.
    #[test]
    fn where_filter_skips_rows_that_do_not_match() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query(
            "CREATE TABLE users (id INT, status TEXT) WITH EVENTS WHERE status = 'active' TO users_events",
        )
        .unwrap();

        // This row should generate an event.
        rt.execute_query("INSERT INTO users (id, status) VALUES (1, 'active')")
            .unwrap();
        // This row should NOT generate an event.
        rt.execute_query("INSERT INTO users (id, status) VALUES (2, 'inactive')")
            .unwrap();

        let events = queue_payloads(&rt, "users_events");
        assert_eq!(
            events.len(),
            1,
            "expected 1 event (only active), got {}",
            events.len()
        );
        let ev = events[0].as_object().unwrap();
        assert_eq!(
            ev.get("op").and_then(crate::json::Value::as_str),
            Some("insert")
        );
        let after = ev.get("after").unwrap().as_object().unwrap();
        assert_eq!(
            after.get("status").and_then(crate::json::Value::as_str),
            Some("active")
        );
    }

    /// `WITH EVENTS (INSERT, UPDATE) WHERE status = 'active'` — combination functional.
    #[test]
    fn ops_filter_and_where_filter_combined() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query(
            "CREATE TABLE items (id INT, status TEXT) WITH EVENTS (INSERT, UPDATE) WHERE status = 'active' TO items_events",
        )
        .unwrap();

        // INSERT active → event
        rt.execute_query("INSERT INTO items (id, status) VALUES (1, 'active')")
            .unwrap();
        // INSERT inactive → no event
        rt.execute_query("INSERT INTO items (id, status) VALUES (2, 'inactive')")
            .unwrap();
        // UPDATE row 1 to inactive → after = inactive, no event
        rt.execute_query("UPDATE items SET status = 'inactive' WHERE id = 1")
            .unwrap();
        // DELETE → ops_filter excludes it
        rt.execute_query("DELETE FROM items WHERE id = 2").unwrap();

        let events = queue_payloads(&rt, "items_events");
        // Only the first INSERT (active) fires; UPDATE result is inactive so skipped; DELETE excluded by ops_filter.
        assert_eq!(
            events.len(),
            1,
            "expected 1 event, got {}: {events:?}",
            events.len()
        );
        assert_eq!(
            events[0]
                .as_object()
                .unwrap()
                .get("op")
                .and_then(crate::json::Value::as_str),
            Some("insert")
        );
    }

    /// WHERE filter on DELETE events — the before-state (pre-image) is evaluated.
    #[test]
    fn where_filter_on_delete_checks_before_state() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query(
            "CREATE TABLE users (id INT, status TEXT) WITH EVENTS (DELETE) WHERE status = 'active' TO users_events",
        )
        .unwrap();

        rt.execute_query("INSERT INTO users (id, status) VALUES (1, 'active'), (2, 'inactive')")
            .unwrap();

        // Delete active row → event (before-state was active)
        rt.execute_query("DELETE FROM users WHERE id = 1").unwrap();
        // Delete inactive row → no event (before-state was inactive)
        rt.execute_query("DELETE FROM users WHERE id = 2").unwrap();

        let events = queue_payloads(&rt, "users_events");
        assert_eq!(
            events.len(),
            1,
            "expected 1 delete event, got {}",
            events.len()
        );
        let ev = events[0].as_object().unwrap();
        assert_eq!(
            ev.get("op").and_then(crate::json::Value::as_str),
            Some("delete")
        );
    }

    // ── #301: schema evolution OperatorEvent on ALTER ───────────────────────

    /// ADD COLUMN on event-enabled table must succeed (OperatorEvent is best-effort).
    #[test]
    fn alter_add_column_on_event_enabled_table_succeeds() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, name TEXT) WITH EVENTS TO users_events")
            .unwrap();
        // Must not error — OperatorEvent emission is best-effort (no global sink in tests).
        rt.execute_query("ALTER TABLE users ADD COLUMN phone TEXT")
            .unwrap();
        // The column is now in the contract.
        let contract = rt.db().collection_contract("users").unwrap();
        assert!(
            contract.declared_columns.iter().any(|c| c.name == "phone"),
            "phone column should be in contract"
        );
        // Subscription still enabled after the alter.
        assert!(
            contract.subscriptions.iter().any(|s| s.enabled),
            "subscription should remain enabled"
        );
    }

    /// DROP COLUMN on event-enabled table must succeed; non-column ALTERs
    /// (like ENABLE ROW LEVEL SECURITY) must also succeed without emitting.
    #[test]
    fn alter_drop_column_and_rls_on_event_enabled_table_succeeds() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query(
            "CREATE TABLE items (id INT, secret TEXT, status TEXT) WITH EVENTS TO items_events",
        )
        .unwrap();
        // DROP COLUMN — schema change event path exercises, must not error.
        rt.execute_query("ALTER TABLE items DROP COLUMN secret")
            .unwrap();
        let contract = rt.db().collection_contract("items").unwrap();
        assert!(
            !contract.declared_columns.iter().any(|c| c.name == "secret"),
            "secret column should be removed"
        );
        // ENABLE RLS — non-column op, no schema-change event (coverage).
        rt.execute_query("ALTER TABLE items ENABLE ROW LEVEL SECURITY")
            .unwrap();
        // Collection and subscription still intact.
        assert!(
            contract.subscriptions.iter().any(|s| s.enabled),
            "subscription should remain enabled"
        );
    }
}
