//! Tenant-table registry and rehydration.
//!
//! Extracted verbatim from `impl_core.rs` (impl_core slice 9/10, issue #1630).
//! Houses tenant-table rehydration (`rehydrate_tenant_tables`,
//! `rehydrate_materialized_view_descriptors`, `rehydrate_declared_column_schemas`),
//! registration (`register_tenant_table`, `ensure_tenant_index`, `drop_tenant_index`,
//! `tenant_column`, `unregister_tenant_table`), and planner-stat maintenance
//! (`refresh_table_planner_stats`, `note_table_write`).
use super::*;

impl RedDBRuntime {
    /// Replay `tenant_tables.*.column` keys from red_config at boot so
    /// `CREATE TABLE ... TENANT BY (col)` declarations persist across
    /// restarts (Phase 2.5.4). Reads every row of the `red_config`
    /// collection, picks the keys matching the tenant-marker shape,
    /// and calls `register_tenant_table` for each.
    ///
    /// Safe no-op when `red_config` doesn't exist (first boot on a
    /// fresh datadir).
    pub(crate) fn rehydrate_tenant_tables(&self) {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return;
        };
        // Replay in insertion order (SegmentManager iteration). Multiple
        // toggles on the same table leave several rows behind — the
        // last one processed wins because each register/unregister
        // call overwrites the in-memory state.
        for entity in manager.query_all(|_| true) {
            let crate::storage::unified::entity::EntityData::Row(row) = &entity.data else {
                continue;
            };
            let Some(named) = &row.named else { continue };
            let Some(crate::storage::schema::Value::Text(key)) = named.get("key") else {
                continue;
            };
            // Shape: tenant_tables.{table}.column
            let Some(rest) = key.strip_prefix("tenant_tables.") else {
                continue;
            };
            let Some((table, suffix)) = rest.rsplit_once('.') else {
                // Issue #205 — a `tenant_tables.*` row that doesn't
                // split cleanly is a schema-shape regression: the
                // metadata writer must always emit the `.column`
                // suffix, so reaching this branch means an upgrade
                // with incompatible state or external tampering.
                crate::telemetry::operator_event::OperatorEvent::SchemaCorruption {
                    collection: "red_config".to_string(),
                    detail: format!("malformed tenant_tables key: {key}"),
                }
                .emit_global();
                continue;
            };
            if suffix != "column" {
                crate::telemetry::operator_event::OperatorEvent::SchemaCorruption {
                    collection: "red_config".to_string(),
                    detail: format!("unexpected tenant_tables suffix: {key}"),
                }
                .emit_global();
                continue;
            }
            match named.get("value") {
                Some(crate::storage::schema::Value::Text(column)) => {
                    self.register_tenant_table(table, column);
                }
                // Null / missing value = DISABLE TENANCY marker.
                Some(crate::storage::schema::Value::Null) | None => {
                    self.unregister_tenant_table(table);
                }
                _ => {}
            }
        }
    }

    /// Replay every persisted `MaterializedViewDescriptor` from the
    /// `red_materialized_view_defs` system collection (issue #593
    /// slice 9a). For each descriptor, re-parse the original SQL,
    /// extract the `QueryExpr::CreateView` it produced, and populate
    /// the in-memory registries (`inner.views` and
    /// `inner.materialized_views`) directly — no write paths run, so
    /// rehydrate does not re-persist what it just read.
    ///
    /// Malformed rows (missing `name`/`source_sql`, parse errors) are
    /// skipped with a `SchemaCorruption` operator event so a single
    /// bad entry does not block startup.
    pub(crate) fn rehydrate_materialized_view_descriptors(&self) {
        let store = self.inner.db.store();
        let descriptors = crate::runtime::continuous_materialized_view::load_all(store.as_ref());
        for descriptor in descriptors {
            let parsed = match crate::storage::query::parser::parse(&descriptor.source_sql) {
                Ok(qc) => qc,
                Err(err) => {
                    crate::telemetry::operator_event::OperatorEvent::SchemaCorruption {
                        collection:
                            crate::runtime::continuous_materialized_view::CATALOG_COLLECTION
                                .to_string(),
                        detail: format!(
                            "failed to re-parse materialized-view source for {}: {err}",
                            descriptor.name
                        ),
                    }
                    .emit_global();
                    continue;
                }
            };
            let crate::storage::query::ast::QueryExpr::CreateView(create) = parsed.query else {
                crate::telemetry::operator_event::OperatorEvent::SchemaCorruption {
                    collection: crate::runtime::continuous_materialized_view::CATALOG_COLLECTION
                        .to_string(),
                    detail: format!(
                        "materialized-view source for {} did not re-parse as CREATE VIEW",
                        descriptor.name
                    ),
                }
                .emit_global();
                continue;
            };
            // Populate in-memory view registry.
            let view_name = create.name.clone();
            self.inner
                .views
                .write()
                .insert(view_name.clone(), Arc::new(create));
            // Materialized cache slot (data empty until next REFRESH).
            use crate::storage::cache::result::{MaterializedViewDef, RefreshPolicy};
            let refresh = match descriptor.refresh_every_ms {
                Some(ms) => RefreshPolicy::Periodic(std::time::Duration::from_millis(ms)),
                None => RefreshPolicy::Manual,
            };
            let def = MaterializedViewDef {
                name: view_name.clone(),
                query: format!("<parsed view {}>", view_name),
                dependencies: descriptor.source_collections.clone(),
                refresh,
                retention_duration_ms: descriptor.retention_duration_ms,
            };
            self.inner.materialized_views.write().register(def);
            if let Err(err) = self.ensure_materialized_view_backing(&view_name) {
                crate::telemetry::operator_event::OperatorEvent::SchemaCorruption {
                    collection: crate::runtime::continuous_materialized_view::CATALOG_COLLECTION
                        .to_string(),
                    detail: format!(
                        "failed to rehydrate backing collection for materialized view {view_name}: {err}"
                    ),
                }
                .emit_global();
            }
        }
        // A rehydrated view shape may differ from any plans the cache
        // bootstrapped before this method ran — flush to be safe.
        self.invalidate_plan_cache();
    }

    pub(crate) fn rehydrate_declared_column_schemas(&self) {
        let store = self.inner.db.store();
        for contract in self.inner.db.collection_contracts() {
            let columns: Vec<String> = contract
                .declared_columns
                .iter()
                .map(|column| column.name.clone())
                .collect();
            let Some(manager) = store.get_collection(&contract.name) else {
                continue;
            };
            manager.set_column_schema_if_empty(columns);
        }
    }

    /// Register a table as tenant-scoped (Phase 2.5.4). Installs the
    /// in-memory column mapping, the implicit RLS policy, and enables
    /// row-level security on the table. Idempotent — re-registering
    /// the same `(table, column)` replaces the prior auto-policy.
    pub fn register_tenant_table(&self, table: &str, column: &str) {
        use crate::storage::query::ast::{
            CompareOp, CreatePolicyQuery, Expr, FieldRef, Filter, Span,
        };
        self.inner
            .tenant_tables
            .write()
            .insert(table.to_string(), column.to_string());

        // Build the policy: col = CURRENT_TENANT()
        // Uses CompareExpr so the comparison happens at runtime against
        // the thread-local tenant value read by the CURRENT_TENANT
        // scalar. Spans are synthetic — there's no source location for
        // an auto-generated policy.
        let lhs = Expr::Column {
            field: FieldRef::TableColumn {
                table: table.to_string(),
                column: column.to_string(),
            },
            span: Span::synthetic(),
        };
        let rhs = Expr::FunctionCall {
            name: "CURRENT_TENANT".to_string(),
            args: Vec::new(),
            span: Span::synthetic(),
        };
        let policy_filter = Filter::CompareExpr {
            lhs,
            op: CompareOp::Eq,
            rhs,
        };

        let policy = CreatePolicyQuery {
            name: "__tenant_iso".to_string(),
            table: table.to_string(),
            action: None, // None = ALL actions (SELECT/INSERT/UPDATE/DELETE)
            role: None,   // None = every role
            using: Box::new(policy_filter),
            // Auto-tenancy defaults to Table targets. Collections of
            // other kinds (graph / vector / queue / timeseries) that
            // opt in via `ALTER ... ENABLE TENANCY` should use the
            // matching kind — but for now we keep the auto-policy
            // kind-agnostic so the evaluator can apply it to any
            // entity living in the collection.
            target_kind: crate::storage::query::ast::PolicyTargetKind::Table,
        };

        // Replace any prior auto-policy for this table (column rename).
        self.inner.rls_policies.write().insert(
            (table.to_string(), "__tenant_iso".to_string()),
            Arc::new(policy),
        );
        self.inner
            .rls_enabled_tables
            .write()
            .insert(table.to_string());

        // Auto-build a hash index on the tenant column. Every read/write
        // against a tenant-scoped table carries an implicit
        // `col = CURRENT_TENANT()` predicate from the auto-policy, so an
        // index on that column is on the hot path of every query. Without
        // it, every SELECT/UPDATE/DELETE degrades to a full scan.
        self.ensure_tenant_index(table, column);
    }

    /// Auto-create the hash index that backs the tenant-iso RLS predicate.
    /// Skipped when:
    ///   * the column is dotted (nested path — flat secondary indices
    ///     don't cover those today; RLS still works via the policy)
    ///   * `__tenant_idx_{table}` already exists (idempotent on rehydrate)
    ///   * the user already registered an index whose first column matches
    ///     (avoids redundant duplicates of a user-defined composite)
    fn ensure_tenant_index(&self, table: &str, column: &str) {
        if column.contains('.') {
            return;
        }
        let index_name = format!("__tenant_idx_{table}");
        let registry = self.inner.index_store.list_indices(table);
        if registry.iter().any(|idx| idx.name == index_name) {
            return;
        }
        if registry
            .iter()
            .any(|idx| idx.columns.first().map(|c| c.as_str()) == Some(column))
        {
            return;
        }

        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(table) else {
            return;
        };
        let entities = manager.query_all(|_| true);
        let entity_fields: Vec<(
            crate::storage::unified::EntityId,
            Vec<(String, crate::storage::schema::Value)>,
        )> = entities
            .iter()
            .map(|e| {
                let fields = match &e.data {
                    crate::storage::EntityData::Row(row) => {
                        if let Some(ref named) = row.named {
                            named.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                        } else if let Some(ref schema) = row.schema {
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

        let columns = vec![column.to_string()];
        if self
            .inner
            .index_store
            .create_index(
                &index_name,
                table,
                &columns,
                super::index_store::IndexMethodKind::Hash,
                false,
                &entity_fields,
            )
            .is_err()
        {
            return;
        }
        self.inner
            .index_store
            .register(super::index_store::RegisteredIndex {
                name: index_name,
                collection: table.to_string(),
                columns,
                method: super::index_store::IndexMethodKind::Hash,
                unique: false,
            });
        self.invalidate_plan_cache();
    }

    /// Drop the auto-generated tenant index, if one exists. Called from
    /// `unregister_tenant_table` so DISABLE TENANCY / DROP TABLE clean up.
    fn drop_tenant_index(&self, table: &str) {
        let index_name = format!("__tenant_idx_{table}");
        self.inner.index_store.drop_index(&index_name, table);
    }

    /// Retrieve the tenant column for a table, if any (Phase 2.5.4).
    /// Used by the INSERT auto-fill path to know which column to
    /// populate with `current_tenant()` when the user didn't name it.
    pub fn tenant_column(&self, table: &str) -> Option<String> {
        self.inner.tenant_tables.read().get(table).cloned()
    }

    /// Remove a table's tenant registration (Phase 2.5.4). Called by
    /// DROP TABLE / ALTER TABLE DISABLE TENANCY. Removes the auto-policy
    /// but leaves any user-installed explicit policies intact.
    pub fn unregister_tenant_table(&self, table: &str) {
        self.inner.tenant_tables.write().remove(table);
        self.inner
            .rls_policies
            .write()
            .remove(&(table.to_string(), "__tenant_iso".to_string()));
        self.drop_tenant_index(table);
        // Only clear RLS enablement if no other policies remain.
        let has_other_policies = self
            .inner
            .rls_policies
            .read()
            .keys()
            .any(|(t, _)| t == table);
        if !has_other_policies {
            self.inner.rls_enabled_tables.write().remove(table);
        }
    }

    pub(crate) fn refresh_table_planner_stats(&self, table: &str) {
        let store = self.inner.db.store();
        if let Some(stats) =
            crate::storage::query::planner::stats_catalog::analyze_collection(store.as_ref(), table)
        {
            crate::storage::query::planner::stats_catalog::persist_table_stats(
                store.as_ref(),
                &stats,
            );
        } else {
            crate::storage::query::planner::stats_catalog::clear_table_stats(store.as_ref(), table);
        }
        self.invalidate_plan_cache();
    }

    pub(crate) fn note_table_write(&self, table: &str) {
        // Skip the write lock when the table is already marked
        // dirty. With single-row UPDATEs in a loop this used to
        // grab the planner_dirty_tables write lock N times even
        // though the first call already flipped the flag.
        let already_dirty = self.inner.planner_dirty_tables.read().contains(table);
        if !already_dirty {
            self.inner
                .planner_dirty_tables
                .write()
                .insert(table.to_string());
        }
        self.invalidate_result_cache_for_table(table);
    }
}
