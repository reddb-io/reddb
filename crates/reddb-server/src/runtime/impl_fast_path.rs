//! Fast-path entity lookup, plan-cache invalidation, and DDL epoch.
//!
//! Extracted verbatim from `impl_core.rs` (impl_core slice 9/10, issue #1630).
//! Houses the string-pattern `try_fast_entity_lookup`, `invalidate_plan_cache`,
//! the monotonic `ddl_epoch` reader, and `clear_table_planner_stats`.
use super::execution_context::entity_visible_under_current_snapshot;
use super::*;

impl RedDBRuntime {
    /// Ultra-fast path: detect `SELECT * FROM table WHERE _entity_id = N` by string pattern
    /// and execute it without SQL parsing or planning. Returns None if pattern doesn't match.
    pub(crate) fn try_fast_entity_lookup(
        &self,
        query: &str,
    ) -> Option<RedDBResult<RuntimeQueryResult>> {
        // Pattern: "SELECT * FROM <table> WHERE _entity_id = <id>"
        // or "SELECT * FROM <table> WHERE _entity_id =<id>"
        let q = query.trim();
        if !q.starts_with("SELECT") && !q.starts_with("select") {
            return None;
        }

        // Find "WHERE _entity_id = " or "WHERE _entity_id ="
        let where_pos = q
            .find("WHERE _entity_id")
            .or_else(|| q.find("where _entity_id"))?;
        let after_field = &q[where_pos + 16..].trim_start(); // skip "WHERE _entity_id"
        let after_eq = after_field.strip_prefix('=')?.trim_start();

        // Parse the entity ID number
        let id_str = after_eq.trim();
        let entity_id: u64 = id_str.parse().ok()?;

        // Extract table name: between "FROM " and " WHERE"
        let from_pos = q.find("FROM ").or_else(|| q.find("from "))? + 5;
        let table = q[from_pos..where_pos].trim();
        if table.is_empty()
            || table.contains(' ') && !table.contains(" AS ") && !table.contains(" as ")
        {
            return None; // complex query, fall through
        }
        let table_name = table.split_whitespace().next()?;

        // Direct entity lookup — skips SQL parse, plan cache, result
        // cache, view rewriter, RLS gate. Safe because the gating in
        // `execute_query` guarantees no scope override / no
        // transaction context is active. MVCC visibility is still
        // honoured against the current snapshot.
        let store = self.inner.db.store();
        let entity = store
            .get(
                table_name,
                crate::storage::unified::EntityId::new(entity_id),
            )
            .filter(entity_visible_under_current_snapshot)
            .filter(|entity| {
                self.inner
                    .db
                    .replica_allows_entity_at_read(table_name, entity)
            });

        let count = if entity.is_some() { 1u64 } else { 0 };

        // Materialize a record so downstream consumers that walk
        // `result.records` (embedded runtime API, decrypt pass, CLI)
        // see the row. Previously only `pre_serialized_json` was
        // filled, which caused those consumers to see zero rows and
        // skewed benchmarks.
        let records: Vec<crate::storage::query::unified::UnifiedRecord> = entity
            .as_ref()
            .and_then(|e| runtime_table_record_from_entity(e.clone()))
            .into_iter()
            .collect();

        let json = match entity {
            Some(ref e) => execute_runtime_serialize_single_entity(e),
            None => r#"{"columns":[],"record_count":0,"selection":{"scope":"any"},"records":[]}"#
                .to_string(),
        };

        Some(Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "select",
            engine: "fast-entity-lookup",
            result: crate::storage::query::unified::UnifiedResult {
                columns: Vec::new(),
                records,
                stats: crate::storage::query::unified::QueryStats {
                    rows_scanned: count,
                    ..Default::default()
                },
                pre_serialized_json: Some(json),
            },
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
            notice: None,
        }))
    }

    pub(crate) fn invalidate_plan_cache(&self) {
        self.inner.query_cache.write().clear();
        self.inner
            .ddl_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    /// Read the monotonic DDL epoch counter. Bumped by every
    /// `invalidate_plan_cache` call so prepared-statement holders can
    /// detect schema drift between PREPARE and EXECUTE.
    pub fn ddl_epoch(&self) -> u64 {
        self.inner
            .ddl_epoch
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn clear_table_planner_stats(&self, table: &str) {
        let store = self.inner.db.store();
        crate::storage::query::planner::stats_catalog::clear_table_stats(store.as_ref(), table);
        self.invalidate_plan_cache();
    }
}
