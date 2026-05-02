//! Native migration execution: CREATE / APPLY / ROLLBACK / EXPLAIN MIGRATION

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use crate::application::migration_collections as mc;
use crate::application::migration_graph;
use crate::application::migration_inference;
use crate::application::vcs::{Author, CreateCommitInput};
use crate::storage::query::ast::{
    ApplyMigrationQuery, ApplyMigrationTarget, CreateMigrationQuery, ExplainMigrationQuery,
    RollbackMigrationQuery,
};
use crate::storage::unified::entity::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn val_text(v: &Value) -> Option<&str> {
    if let Value::Text(s) = v {
        Some(s.as_ref())
    } else {
        None
    }
}

fn val_bool(v: &Value) -> Option<bool> {
    if let Value::Boolean(b) = v {
        Some(*b)
    } else {
        None
    }
}

fn migration_author(rt: &RedDBRuntime) -> Author {
    let store = rt.inner.db.store();
    let name = store
        .get_config("red.vcs.author.name")
        .and_then(|v| if let Value::Text(s) = v { Some(s.to_string()) } else { None })
        .unwrap_or_else(|| "reddb".to_string());
    let email = store
        .get_config("red.vcs.author.email")
        .and_then(|v| if let Value::Text(s) = v { Some(s.to_string()) } else { None })
        .unwrap_or_else(|| "reddb@localhost".to_string());
    Author { name, email }
}

fn insert_meta_row(
    store: &UnifiedStore,
    collection: &str,
    fields: HashMap<String, Value>,
) -> RedDBResult<EntityId> {
    let _ = store.get_or_create_collection(collection);
    store
        .insert_auto(
            collection,
            UnifiedEntity::new(
                EntityId::new(0),
                EntityKind::TableRow {
                    table: Arc::from(collection),
                    row_id: 0,
                },
                EntityData::Row(RowData {
                    columns: Vec::new(),
                    named: Some(fields),
                    schema: None,
                }),
            ),
        )
        .map_err(|e| RedDBError::Internal(e.to_string()))
}

/// Find a migration row by name. Returns the entity and its named fields.
fn find_migration(
    store: &UnifiedStore,
    name: &str,
) -> Option<(UnifiedEntity, HashMap<String, Value>)> {
    let manager = store.get_collection(mc::MIGRATIONS)?;
    let results = manager.query_all(|entity| {
        if let EntityData::Row(ref row) = entity.data {
            if let Some(ref named) = row.named {
                return named.get("name").and_then(|v| val_text(v)) == Some(name);
            }
        }
        false
    });
    results.into_iter().find_map(|entity| {
        if let EntityData::Row(ref row) = entity.data {
            if let Some(ref named) = row.named {
                return Some((entity.clone(), named.clone()));
            }
        }
        None
    })
}

/// Update a field on an existing migration row (by name).
fn update_migration_field(
    store: &UnifiedStore,
    name: &str,
    key: &str,
    value: Value,
) -> RedDBResult<()> {
    let manager = store
        .get_collection(mc::MIGRATIONS)
        .ok_or_else(|| RedDBError::Internal("red_migrations collection not found".to_string()))?;
    let results = manager.query_all(|entity| {
        if let EntityData::Row(ref row) = entity.data {
            if let Some(ref named) = row.named {
                return named.get("name").and_then(|v| val_text(v)) == Some(name);
            }
        }
        false
    });
    for mut entity in results {
        if let EntityData::Row(ref mut row) = entity.data {
            if let Some(ref mut named) = row.named {
                named.insert(key.to_string(), value.clone());
                manager
                    .update(entity)
                    .map_err(|e| RedDBError::Internal(e.to_string()))?;
                return Ok(());
            }
        }
    }
    Err(RedDBError::NotFound(format!(
        "migration '{name}' not found"
    )))
}

/// Load all dependency edges from red_migration_deps.
fn load_all_edges(store: &UnifiedStore) -> Vec<(String, String)> {
    let Some(manager) = store.get_collection(mc::MIGRATION_DEPS) else {
        return Vec::new();
    };
    manager
        .query_all(|_| true)
        .into_iter()
        .filter_map(|entity| {
            if let EntityData::Row(ref row) = entity.data {
                if let Some(ref named) = row.named {
                    let from = named.get("migration_id").and_then(|v| val_text(v))?.to_string();
                    let to = named.get("depends_on_id").and_then(|v| val_text(v))?.to_string();
                    return Some((from, to));
                }
            }
            None
        })
        .collect()
}

/// Load all migration names from red_migrations.
fn load_all_migration_names(store: &UnifiedStore) -> Vec<String> {
    let Some(manager) = store.get_collection(mc::MIGRATIONS) else {
        return Vec::new();
    };
    manager
        .query_all(|_| true)
        .into_iter()
        .filter_map(|entity| {
            if let EntityData::Row(ref row) = entity.data {
                if let Some(ref named) = row.named {
                    return named.get("name").and_then(|v| val_text(v)).map(|s| s.to_string());
                }
            }
            None
        })
        .collect()
}

impl RedDBRuntime {
    /// CREATE MIGRATION — register a migration definition with status=pending.
    pub fn execute_create_migration(
        &self,
        raw_query: &str,
        q: &CreateMigrationQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;

        if find_migration(store, &q.name).is_some() {
            return Err(RedDBError::Query(format!(
                "migration '{}' already exists",
                q.name
            )));
        }

        for dep in &q.depends_on {
            if find_migration(store, dep).is_none() {
                return Err(RedDBError::Query(format!(
                    "migration '{dep}' referenced in DEPENDS ON does not exist"
                )));
            }
        }

        // Cycle detection: check that adding name → dep edges wouldn't create a cycle.
        let existing_edges = load_all_edges(store);
        for dep in &q.depends_on {
            if migration_graph::would_create_cycle(&existing_edges, q.name.as_str(), dep) {
                return Err(RedDBError::Query(format!(
                    "adding DEPENDS ON '{dep}' to migration '{}' would create a dependency cycle",
                    q.name
                )));
            }
        }

        let mut fields: HashMap<String, Value> = HashMap::new();
        fields.insert("name".to_string(), Value::text(q.name.as_str()));
        fields.insert("status".to_string(), Value::text("pending"));
        fields.insert(
            "kind".to_string(),
            Value::text(if q.no_rollback { "data" } else { "ddl" }),
        );
        fields.insert("body".to_string(), Value::text(q.body.as_str()));
        fields.insert(
            "author".to_string(),
            Value::text(migration_author(self).name.as_str()),
        );
        fields.insert("created_at".to_string(), Value::TimestampMs(now_ms()));
        fields.insert("applied_at".to_string(), Value::Null);
        fields.insert("rows_total".to_string(), Value::Null);
        fields.insert("rows_processed".to_string(), Value::UnsignedInteger(0));
        fields.insert("vcs_commit_hash".to_string(), Value::Null);
        fields.insert("no_rollback".to_string(), Value::Boolean(q.no_rollback));
        fields.insert(
            "batch_size".to_string(),
            q.batch_size
                .map(Value::UnsignedInteger)
                .unwrap_or(Value::Null),
        );
        insert_meta_row(store, mc::MIGRATIONS, fields)?;

        for dep in &q.depends_on {
            let mut dep_fields: HashMap<String, Value> = HashMap::new();
            dep_fields.insert("migration_id".to_string(), Value::text(q.name.as_str()));
            dep_fields.insert("depends_on_id".to_string(), Value::text(dep.as_str()));
            dep_fields.insert("inferred".to_string(), Value::Boolean(false));
            insert_meta_row(store, mc::MIGRATION_DEPS, dep_fields)?;
        }

        // Auto-infer additional dependency edges from static analysis of the body.
        let existing_migrations: Vec<(String, String)> = store
            .get_collection(mc::MIGRATIONS)
            .map(|manager| {
                manager
                    .query_all(|_| true)
                    .into_iter()
                    .filter_map(|entity| {
                        if let EntityData::Row(ref row) = entity.data {
                            if let Some(ref named) = row.named {
                                let name =
                                    named.get("name").and_then(|v| val_text(v))?.to_string();
                                let body =
                                    named.get("body").and_then(|v| val_text(v))?.to_string();
                                return Some((name, body));
                            }
                        }
                        None
                    })
                    .collect()
            })
            .unwrap_or_default();
        let explicit_deps: std::collections::HashSet<String> = q.depends_on.iter().cloned().collect();
        let inferred_edges = migration_inference::infer_dependencies(
            q.name.as_str(),
            q.body.as_str(),
            &existing_migrations,
        );
        for (_, dep) in inferred_edges {
            if explicit_deps.contains(&dep) {
                continue; // already stored as explicit
            }
            // Only store if it wouldn't create a cycle (re-check with updated edge set).
            let current_edges = load_all_edges(store);
            if !migration_graph::would_create_cycle(&current_edges, q.name.as_str(), &dep) {
                let mut dep_fields: HashMap<String, Value> = HashMap::new();
                dep_fields.insert("migration_id".to_string(), Value::text(q.name.as_str()));
                dep_fields.insert("depends_on_id".to_string(), Value::text(dep.as_str()));
                dep_fields.insert("inferred".to_string(), Value::Boolean(true));
                let _ = insert_meta_row(store, mc::MIGRATION_DEPS, dep_fields);
            }
        }

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("migration '{}' registered (pending)", q.name),
            "create_migration",
        ))
    }

    /// APPLY MIGRATION name [FOR TENANT id] | APPLY MIGRATION * [FOR TENANT id]
    pub fn execute_apply_migration(
        &self,
        raw_query: &str,
        q: &ApplyMigrationQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        // FOR TENANT * fans out to every known tenant.
        if let Some(tenant) = &q.for_tenant {
            if tenant == "*" {
                return self.apply_migration_all_tenants(raw_query, q);
            }
            // FOR TENANT <specific_id>: set tenant context for this apply.
            crate::runtime::impl_core::set_current_tenant(tenant.clone());
        }

        let result = match &q.target {
            ApplyMigrationTarget::Named(name) => self.apply_single_migration(raw_query, name),
            ApplyMigrationTarget::All => self.apply_all_pending(raw_query),
        };

        // Clear tenant override after apply so it doesn't leak.
        if q.for_tenant.is_some() {
            crate::runtime::impl_core::clear_current_tenant();
        }

        result
    }

    fn apply_all_pending(&self, raw_query: &str) -> RedDBResult<RuntimeQueryResult> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let pending = self.collect_pending_migrations(store);
        if pending.is_empty() {
            return Ok(RuntimeQueryResult::ok_message(
                raw_query.to_string(),
                "no pending migrations",
                "apply_migration",
            ));
        }
        let mut applied = 0u32;
        let mut messages: Vec<String> = Vec::new();
        for name in pending {
            match self.apply_single_migration(raw_query, &name) {
                Ok(_) => {
                    applied += 1;
                    messages.push(format!("applied: {name}"));
                }
                Err(e) => {
                    messages.push(format!("failed: {name} — {e}"));
                    break;
                }
            }
        }
        let summary = messages.join("; ");
        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("applied {applied} migration(s): {summary}"),
            "apply_migration",
        ))
    }

    /// Fan out APPLY MIGRATION * to every known tenant in the auth store.
    fn apply_migration_all_tenants(
        &self,
        raw_query: &str,
        q: &ApplyMigrationQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let tenant_ids = self.list_known_tenants();
        if tenant_ids.is_empty() {
            return Ok(RuntimeQueryResult::ok_message(
                raw_query.to_string(),
                "no tenants found — nothing applied",
                "apply_migration",
            ));
        }
        let mut results: Vec<String> = Vec::new();
        for tenant in &tenant_ids {
            crate::runtime::impl_core::set_current_tenant(tenant.clone());
            let inner_q = ApplyMigrationQuery {
                target: q.target.clone(),
                for_tenant: None,
            };
            match self.execute_apply_migration(raw_query, &inner_q) {
                Ok(r) => results.push(format!(
                    "tenant={tenant}: {}",
                    r.result
                        .records
                        .first()
                        .and_then(|rec| rec.get("message"))
                        .and_then(|v| val_text(v))
                        .unwrap_or("ok")
                )),
                Err(e) => results.push(format!("tenant={tenant}: error — {e}")),
            }
            crate::runtime::impl_core::clear_current_tenant();
        }
        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &results.join("; "),
            "apply_migration",
        ))
    }

    /// Collect distinct tenant IDs from the auth store.
    fn list_known_tenants(&self) -> Vec<String> {
        let auth_store = match self.inner.auth_store.read().clone() {
            Some(s) => s,
            None => return Vec::new(),
        };
        let users = auth_store.list_users_scoped(None);
        let mut tenants: std::collections::HashSet<String> = std::collections::HashSet::new();
        for u in users {
            if let Some(ref t) = u.tenant_id {
                tenants.insert(t.clone());
            }
        }
        let mut out: Vec<String> = tenants.into_iter().collect();
        out.sort();
        out
    }

    fn collect_pending_migrations(&self, store: &UnifiedStore) -> Vec<String> {
        // Collect only pending migrations.
        let Some(manager) = store.get_collection(mc::MIGRATIONS) else {
            return Vec::new();
        };
        let pending: Vec<String> = manager
            .query_all(|entity| {
                if let EntityData::Row(ref row) = entity.data {
                    if let Some(ref named) = row.named {
                        return named.get("status").and_then(|v| val_text(v)) == Some("pending");
                    }
                }
                false
            })
            .into_iter()
            .filter_map(|entity| {
                if let EntityData::Row(ref row) = entity.data {
                    if let Some(ref named) = row.named {
                        return named
                            .get("name")
                            .and_then(|v| val_text(v))
                            .map(|s| s.to_string());
                    }
                }
                None
            })
            .collect();

        // Sort topologically using the full edge set (includes applied migrations
        // as anchors — only pending nodes end up in the output).
        let all_edges = load_all_edges(store);
        // Filter edges to only those between pending migrations.
        let pending_set: std::collections::HashSet<&str> =
            pending.iter().map(|s| s.as_str()).collect();
        let relevant_edges: Vec<(String, String)> = all_edges
            .into_iter()
            .filter(|(m, d)| pending_set.contains(m.as_str()) && pending_set.contains(d.as_str()))
            .collect();

        match migration_graph::topological_sort(&pending, &relevant_edges) {
            Ok(sorted) => sorted,
            Err(_) => pending, // cycle shouldn't happen (guarded at CREATE time); fall back
        }
    }

    fn apply_single_migration(
        &self,
        raw_query: &str,
        name: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;

        let (_, fields) = find_migration(store, name)
            .ok_or_else(|| RedDBError::NotFound(format!("migration '{name}' not found")))?;

        let status = fields
            .get("status")
            .and_then(|v| val_text(v))
            .unwrap_or("");

        if status == "applied" {
            return Ok(RuntimeQueryResult::ok_message(
                raw_query.to_string(),
                &format!("migration '{name}' is already applied"),
                "apply_migration",
            ));
        }

        // Verify all dependencies are applied.
        let deps = self.load_migration_deps(store, name);
        for dep in &deps {
            match find_migration(store, dep) {
                Some((_, dep_fields)) => {
                    let dep_status = dep_fields
                        .get("status")
                        .and_then(|v| val_text(v))
                        .unwrap_or("");
                    if dep_status != "applied" {
                        return Err(RedDBError::Query(format!(
                            "migration '{name}' depends on '{dep}' which is not yet applied"
                        )));
                    }
                }
                None => {
                    return Err(RedDBError::Query(format!(
                        "migration '{name}' depends on '{dep}' which does not exist"
                    )));
                }
            }
        }

        let body = fields
            .get("body")
            .and_then(|v| val_text(v))
            .unwrap_or("")
            .to_string();
        let batch_size = match fields.get("batch_size") {
            Some(Value::UnsignedInteger(n)) => Some(*n),
            _ => None,
        };
        let no_rollback = fields
            .get("no_rollback")
            .and_then(|v| val_bool(v))
            .unwrap_or(false);
        let rows_processed_start = match fields.get("rows_processed") {
            Some(Value::UnsignedInteger(n)) => *n,
            _ => 0,
        };

        let apply_result = if let Some(batch) = batch_size {
            self.apply_batched(store, name, &body, batch, rows_processed_start)
        } else {
            self.apply_statements(name, &body)
        };

        match apply_result {
            Err(e) => {
                let err_msg = e.to_string();
                let _ = update_migration_field(store, name, "status", Value::text("failed"));
                let _ =
                    update_migration_field(store, name, "error", Value::text(err_msg.as_str()));
                Err(RedDBError::Query(format!(
                    "migration '{name}' failed: {err_msg}"
                )))
            }
            Ok(rows_processed) => {
                let author = migration_author(self);
                let commit_hash = self
                    .vcs_commit(CreateCommitInput {
                        connection_id: 0,
                        message: format!("migration: {name}"),
                        author,
                        committer: None,
                        amend: false,
                        allow_empty: true,
                    })
                    .map(|c| c.hash)
                    .unwrap_or_default();

                let _ = update_migration_field(store, name, "status", Value::text("applied"));
                let _ =
                    update_migration_field(store, name, "applied_at", Value::TimestampMs(now_ms()));
                let _ = update_migration_field(
                    store,
                    name,
                    "vcs_commit_hash",
                    Value::text(commit_hash.as_str()),
                );
                if batch_size.is_some() {
                    let _ = update_migration_field(
                        store,
                        name,
                        "rows_processed",
                        Value::UnsignedInteger(rows_processed),
                    );
                }
                let msg = if no_rollback {
                    format!(
                        "migration '{name}' applied — {rows_processed} rows (no rollback, commit: {commit_hash})"
                    )
                } else {
                    format!("migration '{name}' applied (commit: {commit_hash})")
                };
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &msg,
                    "apply_migration",
                ))
            }
        }
    }

    /// Execute a (possibly multi-statement) DDL body by splitting on `;`.
    /// Returns Ok(0) — row count not tracked for DDL.
    fn apply_statements(&self, name: &str, body: &str) -> RedDBResult<u64> {
        let statements: Vec<&str> = body
            .split(';')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        for stmt in statements {
            self.execute_query(stmt).map_err(|e| {
                RedDBError::Query(format!(
                    "statement in migration '{name}' failed: {e}"
                ))
            })?;
        }
        Ok(0)
    }

    /// Execute a data migration body in batches of `batch_size` rows,
    /// persisting a checkpoint (`rows_processed`) after each batch.
    /// Appends `LIMIT {batch_size}` to the body on each iteration;
    /// stops when the engine reports fewer rows affected than the batch size.
    fn apply_batched(
        &self,
        store: &UnifiedStore,
        name: &str,
        body: &str,
        batch_size: u64,
        initial_processed: u64,
    ) -> RedDBResult<u64> {
        let mut total = initial_processed;
        loop {
            let batch_body = format!("{body} LIMIT {batch_size}");
            let result = self.execute_query(&batch_body).map_err(|e| {
                RedDBError::Query(format!("batch in migration '{name}' failed: {e}"))
            })?;
            let affected = result.affected_rows;
            total += affected;
            // Persist checkpoint so a crash can resume from here.
            let _ = update_migration_field(
                store,
                name,
                "rows_processed",
                Value::UnsignedInteger(total),
            );
            if affected < batch_size {
                break;
            }
        }
        Ok(total)
    }

    fn load_migration_deps(&self, store: &UnifiedStore, name: &str) -> Vec<String> {
        let Some(manager) = store.get_collection(mc::MIGRATION_DEPS) else {
            return Vec::new();
        };
        manager
            .query_all(|entity| {
                if let EntityData::Row(ref row) = entity.data {
                    if let Some(ref named) = row.named {
                        return named.get("migration_id").and_then(|v| val_text(v)) == Some(name);
                    }
                }
                false
            })
            .into_iter()
            .filter_map(|entity| {
                if let EntityData::Row(ref row) = entity.data {
                    if let Some(ref named) = row.named {
                        return named
                            .get("depends_on_id")
                            .and_then(|v| val_text(v))
                            .map(|s| s.to_string());
                    }
                }
                None
            })
            .collect()
    }

    /// ROLLBACK MIGRATION name
    pub fn execute_rollback_migration(
        &self,
        raw_query: &str,
        q: &RollbackMigrationQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;

        let (_, fields) = find_migration(store, &q.name)
            .ok_or_else(|| RedDBError::NotFound(format!("migration '{}' not found", q.name)))?;

        if fields
            .get("no_rollback")
            .and_then(|v| val_bool(v))
            .unwrap_or(false)
        {
            return Err(RedDBError::Query(format!(
                "migration '{}' was declared NO ROLLBACK and cannot be rolled back",
                q.name
            )));
        }

        let status = fields
            .get("status")
            .and_then(|v| val_text(v))
            .unwrap_or("");

        if status != "applied" {
            return Err(RedDBError::Query(format!(
                "migration '{}' has status '{status}' — only applied migrations can be rolled back",
                q.name
            )));
        }

        let commit_hash = fields
            .get("vcs_commit_hash")
            .and_then(|v| val_text(v))
            .unwrap_or("")
            .to_string();

        if !commit_hash.is_empty() {
            let author = migration_author(self);
            let _ = self.vcs_revert(0, &commit_hash, author);
        }

        let _ = update_migration_field(store, &q.name, "status", Value::text("pending"));
        let _ = update_migration_field(store, &q.name, "applied_at", Value::Null);
        let _ = update_migration_field(store, &q.name, "vcs_commit_hash", Value::Null);

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("migration '{}' rolled back (status: pending)", q.name),
            "rollback_migration",
        ))
    }

    /// EXPLAIN MIGRATION name
    pub fn execute_explain_migration(
        &self,
        raw_query: &str,
        q: &ExplainMigrationQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;

        let (_, fields) = find_migration(store, &q.name)
            .ok_or_else(|| RedDBError::NotFound(format!("migration '{}' not found", q.name)))?;

        let status = fields
            .get("status")
            .and_then(|v| val_text(v))
            .unwrap_or("unknown")
            .to_string();
        let body = fields
            .get("body")
            .and_then(|v| val_text(v))
            .unwrap_or("")
            .to_string();
        let kind = fields
            .get("kind")
            .and_then(|v| val_text(v))
            .unwrap_or("ddl")
            .to_string();

        let columns = vec![
            "migration".to_string(),
            "status".to_string(),
            "kind".to_string(),
            "body".to_string(),
            "estimated_rows".to_string(),
            "lock_duration_ms".to_string(),
        ];

        let row: Vec<(String, Value)> = vec![
            ("migration".to_string(), Value::text(q.name.as_str())),
            ("status".to_string(), Value::text(status.as_str())),
            ("kind".to_string(), Value::text(kind.as_str())),
            ("body".to_string(), Value::text(body.as_str())),
            ("estimated_rows".to_string(), Value::Null),
            ("lock_duration_ms".to_string(), Value::UnsignedInteger(0)),
        ];

        Ok(RuntimeQueryResult::ok_records(
            raw_query.to_string(),
            columns,
            vec![row],
            "explain_migration",
        ))
    }
}
