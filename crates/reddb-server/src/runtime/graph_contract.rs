//! Runtime ownership of optional graph contracts and projected-state validation.
use super::*;
use reddb_engine::catalog::GraphContract;
use reddb_engine::transaction::read::{entity_visibility_check, SnapshotContext};

impl RedDBRuntime {
    pub fn graph_contract(&self, collection: &str) -> RedDBResult<Option<GraphContract>> {
        self.check_collection_privilege(reddb_rql::ast::PolicyAction::Select, collection)?;
        let contract = self
            .db()
            .collection_contract(collection)
            .ok_or_else(|| RedDBError::NotFound(format!("collection '{collection}'")))?;
        Ok(contract.graph_contract)
    }

    pub(crate) fn has_graph_contract(&self, collection: &str) -> bool {
        self.db()
            .collection_contract_arc(collection)
            .is_some_and(|contract| contract.graph_contract.is_some())
    }

    pub fn set_graph_contract(
        &self,
        collection: &str,
        graph: Option<GraphContract>,
    ) -> RedDBResult<()> {
        self.check_write(super::write_gate::WriteKind::Ddl)?;
        if let (Some(auth), Some((username, role))) = (
            self.inner.auth_store.read().clone(),
            super::execution_context::current_auth_identity(),
        ) {
            let tenant = super::execution_context::current_tenant();
            let principal = crate::auth::UserId::from_parts(tenant.as_deref(), &username);
            self.check_ddl_collection_privilege(
                &auth,
                &principal,
                role,
                tenant.as_deref(),
                &username,
                "alter",
                collection,
            )
            .map_err(RedDBError::Query)?;
        }
        let store = self.db().store();
        let _publication = store.vault_publication_lock.lock();
        if self.snapshot_manager().oldest_active_xid().is_some() {
            return Err(RedDBError::InvalidOperation(
                "finish active transactions before changing a graph contract".into(),
            ));
        }
        let mut contract = self
            .db()
            .collection_contract(collection)
            .ok_or_else(|| RedDBError::NotFound(format!("collection '{collection}'")))?;
        if contract.declared_model != crate::catalog::CollectionModel::Graph {
            return Err(RedDBError::InvalidOperation(
                "graph contracts require a Graph collection".into(),
            ));
        }
        if let Some(graph) = &graph {
            graph.validate()?;
            for snapshot in self.vcs_live_graph_validation_contexts(collection)? {
                self.validate_graph_projection(collection, graph, &snapshot, &[], &[])?;
            }
            // Earlier readers must reject the file instead of ignoring a contract
            // they cannot enforce. V13 also carries branch provenance.
            if store.format_version() < reddb_file::native_store::STORE_VERSION_V13 {
                let previous = store.format_version();
                store.set_format_version(reddb_file::native_store::STORE_VERSION_V13);
                if store.pager().is_some() {
                    if let Err(error) = store.persist() {
                        store.set_format_version(previous);
                        return Err(error.into());
                    }
                }
            }
        }
        contract.graph_contract = graph;
        contract.version = contract.version.checked_add(1).ok_or_else(|| {
            RedDBError::InvalidOperation("collection contract version exhausted".into())
        })?;
        contract.updated_at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        self.db()
            .save_collection_contract(contract)
            .map_err(|error| RedDBError::Internal(error.to_string()))?;
        self.invalidate_result_cache();
        Ok(())
    }

    pub(crate) fn validate_graph_changes(
        &self,
        collection: &str,
        added: &[&UnifiedEntity],
        removed: &[EntityId],
    ) -> RedDBResult<()> {
        let Some(contract) = self.db().collection_contract_arc(collection) else {
            return Ok(());
        };
        let Some(graph) = &contract.graph_contract else {
            return Ok(());
        };
        let (mut snapshot, _retention) = self.native_read_snapshot()?;
        snapshot.own_xids = self.own_transaction_xids();
        if self.current_xid().is_none() {
            snapshot.snapshot = self.snapshot_manager().fresh_read_snapshot();
            if let Some(branch) = snapshot.branch.as_mut() {
                let branch = Arc::make_mut(branch);
                branch
                    .epochs
                    .insert(branch.writer_epoch, snapshot.snapshot.clone());
            }
        }
        self.validate_graph_projection(collection, graph, &snapshot, added, removed)
    }

    /// Called under the publication fence, before committing the transaction.
    /// Refresh only the writer epoch; inherited branches retain their fork view.
    pub(crate) fn validate_graph_contracts_at_commit(&self) -> RedDBResult<()> {
        let (mut context, _retention) = self.native_read_snapshot()?;
        context.snapshot = self.snapshot_manager().fresh_read_snapshot();
        context.own_xids = self.own_transaction_xids();
        if let Some(branch) = context.branch.as_mut() {
            let branch = Arc::make_mut(branch);
            branch
                .epochs
                .insert(branch.writer_epoch, context.snapshot.clone());
        }
        for contract in self.db().collection_contracts() {
            if let Some(graph) = &contract.graph_contract {
                self.validate_graph_projection(&contract.name, graph, &context, &[], &[])?;
            }
        }
        Ok(())
    }

    pub(crate) fn validate_graph_entities(
        &self,
        collection: &str,
        graph: &GraphContract,
        entities: &[&UnifiedEntity],
    ) -> RedDBResult<()> {
        let Some(column) = self.tenant_column(collection) else {
            graph.validate_entities(entities)?;
            return Ok(());
        };
        // Integrity is checked across all tenants, but a tenant's edge may only
        // resolve endpoints in its own graph. RLS cannot hide invalid endpoints
        // from admission or let another tenant satisfy an endpoint reference.
        let mut tenants: HashMap<String, Vec<&UnifiedEntity>> = HashMap::new();
        for &entity in entities {
            let properties = match &entity.data {
                EntityData::Node(node) => &node.properties,
                EntityData::Edge(edge) => &edge.properties,
                _ => continue,
            };
            let tenant = if let Some((root, tail)) = column.split_once('.') {
                super::impl_dml_support::dotted_tenant_text(
                    properties.get(root).unwrap_or(&Value::Null),
                    tail,
                )?
            } else {
                properties.get(&column).and_then(|value| match value {
                    Value::Text(value) => Some(value.to_string()),
                    _ => None,
                })
            }
            .ok_or_else(|| {
                RedDBError::Query("graph contract requires a text tenant discriminator".into())
            })?;
            tenants.entry(tenant).or_default().push(entity);
        }
        for entities in tenants.values() {
            graph.validate_entities(entities)?;
        }
        Ok(())
    }

    fn validate_graph_projection(
        &self,
        collection: &str,
        graph: &GraphContract,
        snapshot: &SnapshotContext,
        added: &[&UnifiedEntity],
        removed: &[EntityId],
    ) -> RedDBResult<()> {
        let store = self.db().store();
        let manager = store
            .get_collection(collection)
            .ok_or_else(|| RedDBError::NotFound(format!("collection '{collection}'")))?;
        let (_scan_memory, read_bytes_max) = self.reserve_index_scan(&manager)?;
        let mut rows = Vec::new();
        let mut reservations = Vec::new();
        super::function_budget::try_scan_result(
            |visit| manager.try_for_each_entity(read_bytes_max, visit),
            |entity| {
                if !removed.contains(&entity.id)
                    && entity_visibility_check(snapshot, entity)
                    && matches!(
                        entity.kind,
                        EntityKind::GraphNode(_) | EntityKind::GraphEdge(_)
                    )
                {
                    let bytes = crate::storage::unified::memory_size::entity_bytes(entity)
                        .saturating_mul(2)
                        .saturating_add(512);
                    reservations.push(
                        self.try_reserve_query_buffer_from_sample(bytes)
                            .ok_or_else(|| {
                                RedDBError::Query(
                                    "graph contract validation exceeds the query memory budget"
                                        .into(),
                                )
                            })?,
                    );
                    rows.push(entity.clone());
                }
                Ok(true)
            },
        )?;
        let entities: Vec<_> = rows.iter().chain(added.iter().copied()).collect();
        self.validate_graph_entities(collection, graph, &entities)?;
        Ok(())
    }
}
