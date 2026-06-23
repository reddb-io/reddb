use super::*;
use crate::auth::column_policy_gate::ColumnAccessRequest;
use crate::auth::UserId;
use crate::replication::cdc::ChangeRecord;
use crate::storage::query::ast::TableSource;

/// Read a numeric score column out of a result record as `f64`, matching
/// the column name case-insensitively. Used by the leaderboard-rank head
/// walk (#918) to compare scores; non-numeric / missing columns yield
/// `None` so a row with no comparable score never shifts a rank.
fn record_column_f64(
    rec: &crate::storage::query::unified::UnifiedRecord,
    column: &str,
) -> Option<f64> {
    let value = rec
        .get(column)
        .or_else(|| rec.get(&column.to_lowercase()))?;
    match value {
        Value::Integer(n) => Some(*n as f64),
        Value::UnsignedInteger(n) => Some(*n as f64),
        Value::Float(n) => Some(*n),
        Value::Timestamp(n) | Value::Duration(n) => Some(*n as f64),
        _ => None,
    }
}

fn record_rid_u64(rec: &crate::storage::query::unified::UnifiedRecord) -> Option<u64> {
    match rec.get("rid") {
        Some(Value::UnsignedInteger(n)) => Some(*n),
        Some(Value::Integer(n)) if *n >= 0 => Some(*n as u64),
        _ => None,
    }
}

fn seed_storage_deploy_config(
    store: &crate::storage::UnifiedStore,
    selection: crate::storage::StorageProfileSelection,
) {
    store.set_config_tree(
        "storage.deploy",
        &crate::json!({
            "profile": selection.deploy_profile.as_str(),
            "packaging": selection.packaging.as_str(),
            "preset": selection.preset_name(),
            "replica_count": selection.replica_count,
            "managed_backup": selection.managed_backup,
            "wal_retention": selection.wal_retention,
        }),
    );
}

struct RankedHeadEntry {
    rank: u64,
    record: crate::storage::query::unified::UnifiedRecord,
}

fn secret_sql_value_to_string(value: &Value) -> RedDBResult<String> {
    match value {
        Value::Text(s) => Ok(s.to_string()),
        Value::Integer(n) => Ok(n.to_string()),
        Value::UnsignedInteger(n) => Ok(n.to_string()),
        Value::Float(n) => Ok(n.to_string()),
        Value::Boolean(b) => Ok(b.to_string()),
        Value::Null => Err(RedDBError::Query(
            "SET SECRET key = NULL deletes the secret; use DELETE SECRET for explicit deletes"
                .to_string(),
        )),
        Value::Password(_) | Value::Secret(_) => Err(RedDBError::Query(
            "SET SECRET accepts plain scalar literals; PASSWORD() and SECRET() are for typed columns"
                .to_string(),
        )),
        _ => Err(RedDBError::Query(format!(
            "SET SECRET does not support value type {:?} yet",
            value.data_type()
        ))),
    }
}

fn insert_config_json_path(
    root: &mut crate::serde_json::Value,
    path: &str,
    value: crate::serde_json::Value,
) {
    let segments: Vec<&str> = path
        .split('.')
        .filter(|segment| !segment.is_empty())
        .collect();
    insert_config_json_segments(root, &segments, value);
}

fn insert_config_json_segments(
    root: &mut crate::serde_json::Value,
    segments: &[&str],
    value: crate::serde_json::Value,
) {
    if segments.is_empty() {
        *root = value;
        return;
    }

    if !matches!(root, crate::serde_json::Value::Object(_)) {
        *root = crate::serde_json::Value::Object(crate::serde_json::Map::new());
    }

    let crate::serde_json::Value::Object(map) = root else {
        return;
    };
    if segments.len() == 1 {
        map.insert(segments[0].to_string(), value);
        return;
    }
    let entry = map
        .entry(segments[0].to_string())
        .or_insert_with(|| crate::serde_json::Value::Object(crate::serde_json::Map::new()));
    insert_config_json_segments(entry, &segments[1..], value);
}

fn show_config_json_result(
    query: &str,
    mode: crate::storage::query::modes::QueryMode,
    prefix: &Option<String>,
    value: crate::serde_json::Value,
) -> RuntimeQueryResult {
    let mut result = UnifiedResult::with_columns(vec!["key".into(), "value".into()]);
    let mut record = UnifiedRecord::new();
    record.set(
        "key",
        prefix
            .as_ref()
            .map(|key| Value::text(key.clone()))
            .unwrap_or(Value::Null),
    );
    record.set("value", Value::Json(value.to_string_compact().into_bytes()));
    result.push(record);
    RuntimeQueryResult {
        query: query.to_string(),
        mode,
        statement: "show_config_json",
        engine: "runtime-config",
        result,
        affected_rows: 0,
        statement_type: "select",
        bookmark: None,
    }
}

#[derive(Clone)]
struct QueryControlEventSpec {
    kind: crate::runtime::control_events::EventKind,
    action: &'static str,
    resource: Option<String>,
    fields: Vec<(String, crate::runtime::control_events::Sensitivity)>,
}

#[derive(Clone)]
struct QueryAuditPlan {
    statement_kind: &'static str,
    collections: Vec<String>,
}

fn query_audit_plan(expr: &QueryExpr) -> Option<QueryAuditPlan> {
    let mut collections = Vec::new();
    let statement_kind = match expr {
        QueryExpr::Table(table) => {
            push_query_audit_collection(&mut collections, &table.table);
            "select"
        }
        QueryExpr::Join(join) => {
            collect_query_audit_collections(&join.left, &mut collections);
            collect_query_audit_collections(&join.right, &mut collections);
            "select"
        }
        QueryExpr::Insert(insert) => {
            push_query_audit_collection(&mut collections, &insert.table);
            "insert"
        }
        QueryExpr::Update(update) => {
            push_query_audit_collection(&mut collections, &update.table);
            "update"
        }
        QueryExpr::Delete(delete) => {
            push_query_audit_collection(&mut collections, &delete.table);
            "delete"
        }
        _ => return None,
    };
    if collections.is_empty() {
        None
    } else {
        Some(QueryAuditPlan {
            statement_kind,
            collections,
        })
    }
}

fn collect_query_audit_collections(expr: &QueryExpr, collections: &mut Vec<String>) {
    match expr {
        QueryExpr::Table(table) => push_query_audit_collection(collections, &table.table),
        QueryExpr::Join(join) => {
            collect_query_audit_collections(&join.left, collections);
            collect_query_audit_collections(&join.right, collections);
        }
        _ => {}
    }
}

fn push_query_audit_collection(collections: &mut Vec<String>, name: &str) {
    if name == "red" || name.starts_with("red.") || name.starts_with("__red_schema_") {
        return;
    }
    if !collections.iter().any(|existing| existing == name) {
        collections.push(name.to_string());
    }
}

const RUNTIME_INDEX_REGISTRY_COLLECTION: &str = "red_index_registry";

impl RedDBRuntime {
    fn execute_create_metric(
        &self,
        raw_query: &str,
        query: &crate::storage::query::ast::CreateMetricQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        let store = self.inner.db.store();
        super::metric_descriptor_catalog::create(
            store.as_ref(),
            &query.path,
            &query.kind,
            &query.role,
            super::metric_descriptor_catalog::DerivedSpec {
                source: query.source.clone(),
                query: query.query.clone(),
                window_ms: query.window_ms,
                time_field: query.time_field.clone(),
            },
        )?;
        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("metric descriptor '{}' created", query.path),
            "create",
        ))
    }

    /// `CREATE RANKING <name> ON <table> (<column> [ASC|DESC]) [TOP <k>]`
    /// — declare a Ranking capability over an ordinary table's score
    /// column (issue #918 / ADR 0035). Persists a WAL-backed catalog
    /// record; no new Collection model is introduced. Authorized through
    /// the same DDL write gate as `CREATE METRIC`/`CREATE INDEX`.
    fn execute_create_ranking(
        &self,
        raw_query: &str,
        req: super::ranking_descriptor_catalog::CreateRankingRequest,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        let store = self.inner.db.store();
        let descriptor = super::ranking_descriptor_catalog::create(store.as_ref(), &req)?;
        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!(
                "ranking '{}' created on {}({})",
                descriptor.name, descriptor.table, descriptor.column
            ),
            "create",
        ))
    }

    /// `SHOW RANKINGS` — project the declared Ranking capabilities back as
    /// rows, so a declared capability is observable (the Analytics
    /// "prefer SELECT over admin verbs" rule).
    fn execute_show_rankings(&self, raw_query: &str) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();
        let entries = super::ranking_descriptor_catalog::list(store.as_ref());
        let columns = vec![
            "name".to_string(),
            "table".to_string(),
            "column".to_string(),
            "direction".to_string(),
            "top_k".to_string(),
        ];
        let rows = entries
            .into_iter()
            .map(|e| {
                vec![
                    ("name".to_string(), Value::text(e.name)),
                    ("table".to_string(), Value::text(e.table)),
                    ("column".to_string(), Value::text(e.column)),
                    (
                        "direction".to_string(),
                        Value::text(if e.descending { "DESC" } else { "ASC" }.to_string()),
                    ),
                    ("top_k".to_string(), Value::UnsignedInteger(e.top_k)),
                ]
            })
            .collect();
        let mut result =
            RuntimeQueryResult::ok_records(raw_query.to_string(), columns, rows, "select");
        result.statement = "rank_of";
        result.engine = "runtime-rank";
        Ok(result)
    }

    /// `RANK OF <id> IN <name>` — exact, MVCC-correct rank of a specific
    /// row within the capability's bounded top-K head (issue #918).
    ///
    /// Returns a single `rank` row when the row is visible *and* falls
    /// inside the exact head; an empty result otherwise (not visible, or
    /// in the approximate tail — a separate slice). The computation runs
    /// entirely over the regular read pipeline so it inherits MVCC
    /// visibility, RLS/policy, and tenant scope from ordinary reads.
    fn execute_rank_of(
        &self,
        raw_query: &str,
        req: &crate::storage::query::ast::RankOfQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();
        let descriptor = super::ranking_descriptor_catalog::get(store.as_ref(), &req.ranking)
            .ok_or_else(|| {
                RedDBError::Query(format!("ranking '{}' does not exist", req.ranking))
            })?;
        let rank = self.compute_exact_head_rank(&descriptor, req.entity_id)?;
        let columns = vec!["rank".to_string()];
        let rows = match rank {
            Some(rank) => vec![vec![("rank".to_string(), Value::UnsignedInteger(rank))]],
            None => Vec::new(),
        };
        let mut result =
            RuntimeQueryResult::ok_records(raw_query.to_string(), columns, rows, "select");
        result.statement = "rank_range";
        result.engine = "runtime-rank";
        Ok(result)
    }

    /// `RANK RANGE <lo> TO <hi> IN <name>` — exact, MVCC-correct entries
    /// occupying a contiguous rank range within the bounded top-K head.
    ///
    /// The output is in leaderboard order and includes `rank` plus the
    /// row columns returned by the canonical exact-head SQL read.
    fn execute_rank_range(
        &self,
        raw_query: &str,
        req: &crate::storage::query::ast::RankRangeQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();
        let descriptor = super::ranking_descriptor_catalog::get(store.as_ref(), &req.ranking)
            .ok_or_else(|| {
                RedDBError::Query(format!("ranking '{}' does not exist", req.ranking))
            })?;
        let (head_columns, entries) = self.compute_ranked_head_entries(&descriptor)?;

        let mut columns = Vec::with_capacity(head_columns.len() + 1);
        columns.push("rank".to_string());
        for column in &head_columns {
            if column != "rank" {
                columns.push(column.clone());
            }
        }

        let rows = entries
            .into_iter()
            .filter(|entry| entry.rank >= req.lo && entry.rank <= req.hi)
            .map(|entry| {
                let mut row = Vec::with_capacity(columns.len());
                row.push(("rank".to_string(), Value::UnsignedInteger(entry.rank)));
                for column in &head_columns {
                    if column == "rank" {
                        continue;
                    }
                    if let Some(value) = entry.record.get(column) {
                        row.push((column.clone(), value.clone()));
                    }
                }
                row
            })
            .collect();
        let mut result =
            RuntimeQueryResult::ok_records(raw_query.to_string(), columns, rows, "select");
        result.statement = "approx_rank_of";
        result.engine = "runtime-rank";
        Ok(result)
    }

    /// Compute the exact rank of `target_id` within the descriptor's
    /// bounded top-K head, or `None` if the row is invisible to the
    /// querying snapshot or beyond the exact head.
    ///
    /// Faithful to ADR 0035: it walks the sorted index head
    /// (`ORDER BY <col> {DESC|ASC} LIMIT k`, served by
    /// `try_sorted_index_lookup` + the per-row MVCC visibility re-check)
    /// and counts only rows visible to the current snapshot. Running the
    /// head scan through `execute_query_inner` keeps it on the same
    /// snapshot/tenant/policy frame as ordinary reads, so the rank agrees
    /// with `ORDER BY <col> {DESC|ASC} LIMIT` under that snapshot by
    /// construction. RANK semantics: tied scores share a rank, so the
    /// rank is `1 + (number of strictly-better visible rows)`.
    fn compute_exact_head_rank(
        &self,
        descriptor: &super::ranking_descriptor_catalog::RankingDescriptor,
        target_id: u64,
    ) -> RedDBResult<Option<u64>> {
        let (_columns, entries) = self.compute_ranked_head_entries(descriptor)?;
        Ok(entries
            .into_iter()
            .find(|entry| record_rid_u64(&entry.record) == Some(target_id))
            .map(|entry| entry.rank))
    }

    /// Return the exact head rows in deterministic rank order.
    fn compute_ranked_head_entries(
        &self,
        descriptor: &super::ranking_descriptor_catalog::RankingDescriptor,
    ) -> RedDBResult<(Vec<String>, Vec<RankedHeadEntry>)> {
        let table = &descriptor.table;
        let column = &descriptor.column;

        // The exact head: top-K rows in rank order. Each row here already
        // passed MVCC visibility *and* RLS/tenant filtering during the
        // scan, so identifying the target *within* this result (rather
        // than via a separate `rid` lookup, which takes the
        // direct entity-fetch path that bypasses the RLS gate) is what
        // makes the rank honor policy/tenant scope (criterion 5).
        let dir = if descriptor.descending { "DESC" } else { "ASC" };
        let head_sql = format!(
            "SELECT * FROM {table} ORDER BY {column} {dir}, rid ASC LIMIT {}",
            descriptor.top_k
        );
        let head_result = self.execute_query_inner(&head_sql)?;

        let mut entries = Vec::with_capacity(head_result.result.records.len());
        let mut row_position = 0u64;
        let mut current_rank = 0u64;
        let mut previous_score: Option<f64> = None;
        for rec in &head_result.result.records {
            let Some(score) = record_column_f64(rec, column) else {
                continue;
            };
            row_position += 1;
            current_rank = if previous_score == Some(score) {
                current_rank
            } else {
                row_position
            };
            previous_score = Some(score);
            entries.push(RankedHeadEntry {
                rank: current_rank,
                record: rec.clone(),
            });
        }
        Ok((head_result.result.columns, entries))
    }

    /// `APPROX RANK OF <id> IN <name>` — the *approximate tail* read
    /// (issue #923 / ADR 0035). Serves an explicitly-approximate
    /// percentile / rank for an entry below the exact top-K head from a
    /// per-`(table, column)` score sketch.
    ///
    /// The result is always labeled approximate (`approximate = true`,
    /// distinct from the exact `RANK OF` surface which returns only a bare
    /// `rank`) so a caller never reads a tail estimate as an exact head
    /// position. An invisible / non-existent row yields no row, exactly
    /// like the exact surface.
    fn execute_approx_rank_of(
        &self,
        raw_query: &str,
        req: &crate::storage::query::ast::RankOfQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();
        let descriptor = super::ranking_descriptor_catalog::get(store.as_ref(), &req.ranking)
            .ok_or_else(|| {
                RedDBError::Query(format!("ranking '{}' does not exist", req.ranking))
            })?;

        let approx = self.compute_approx_rank(&descriptor, req.entity_id)?;
        let columns = vec![
            "rank".to_string(),
            "percentile".to_string(),
            "approximate".to_string(),
        ];
        let rows = match approx {
            Some(approx) => vec![vec![
                ("rank".to_string(), Value::UnsignedInteger(approx.rank)),
                ("percentile".to_string(), Value::Float(approx.percentile)),
                ("approximate".to_string(), Value::Boolean(true)),
            ]],
            None => Vec::new(),
        };
        let mut result =
            RuntimeQueryResult::ok_records(raw_query.to_string(), columns, rows, "select");
        result.statement = "approx_rank_of";
        // Tag as `runtime-rank` so the 30s result cache skips this read
        // (see `should_write_result_cache`). The approximate rank is rebuilt
        // from a live full scan on every call (criterion 4: it must track
        // score changes); a cached entry, scoped only to the ranking name and
        // never the underlying table, would otherwise survive inserts into
        // that table and serve a stale rank.
        result.engine = "runtime-rank";
        Ok(result)
    }

    /// Refresh the per-`(table, column)` score sketch from the rows visible
    /// to the current snapshot and return the target's approximate rank, or
    /// `None` if the target row is invisible to this snapshot / tenant.
    ///
    /// The sketch is rebuilt from the live column on each read and persisted
    /// back to `red_config` keyed by `(table, column)` — so it is maintained
    /// per `(collection, score column)` and stays current as scores change
    /// (criterion 4). The scan runs through `execute_query_inner`, inheriting
    /// the same MVCC snapshot, RLS/tenant scope, and policy as ordinary
    /// reads. The *approximation* is the histogram bucketing in
    /// [`super::score_sketch::ScoreSketch`], not the data freshness, so the
    /// estimate carries the documented error band even though it is built
    /// from a full scan in this v0 (incremental maintenance is an ADR-0035
    /// implementation detail, left open and reversible).
    fn compute_approx_rank(
        &self,
        descriptor: &super::ranking_descriptor_catalog::RankingDescriptor,
        target_id: u64,
    ) -> RedDBResult<Option<super::score_sketch::ApproxRank>> {
        let table = &descriptor.table;
        let column = &descriptor.column;

        // Scan the visible rows once: it both feeds the sketch and locates
        // the target's score under the same snapshot/tenant/policy frame.
        let scan_sql = format!("SELECT * FROM {table}");
        let scan = self.execute_query_inner(&scan_sql)?;
        let records = &scan.result.records;

        let mut scores: Vec<f64> = Vec::with_capacity(records.len());
        let mut target_score: Option<f64> = None;
        for rec in records {
            let Some(score) = record_column_f64(rec, column) else {
                continue;
            };
            scores.push(score);
            let rid = match rec.get("rid") {
                Some(Value::UnsignedInteger(n)) => Some(*n),
                Some(Value::Integer(n)) if *n >= 0 => Some(*n as u64),
                _ => None,
            };
            if rid == Some(target_id) {
                target_score = Some(score);
            }
        }

        let sketch = super::score_sketch::ScoreSketch::from_scores(&scores);
        // Persist the refreshed sketch per (table, column).
        super::ranking_descriptor_catalog::save_sketch(
            self.inner.db.store().as_ref(),
            table,
            column,
            &sketch,
        );

        let Some(target_score) = target_score else {
            // Not visible to this snapshot/tenant ⇒ no rank (matches exact).
            return Ok(None);
        };
        Ok(sketch.approx_rank(target_score, descriptor.descending))
    }

    fn execute_alter_metric(
        &self,
        raw_query: &str,
        query: &crate::storage::query::ast::AlterMetricQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        let store = self.inner.db.store();
        super::metric_descriptor_catalog::update(
            store.as_ref(),
            &query.path,
            query.set_role.as_deref(),
            query.attempted_kind.as_deref(),
            query.attempted_path.as_deref(),
        )?;
        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("metric descriptor '{}' updated", query.path),
            "alter",
        ))
    }

    fn execute_create_slo(
        &self,
        raw_query: &str,
        query: &crate::storage::query::ast::CreateSloQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        let store = self.inner.db.store();
        super::slo_descriptor_catalog::create(
            store.as_ref(),
            &query.path,
            &query.metric_path,
            query.target,
            query.window_ms,
        )?;
        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("SLO descriptor '{}' created", query.path),
            "create",
        ))
    }

    fn execute_create_analytics_source(
        &self,
        raw_query: &str,
        query: super::analytics_source_catalog::CreateAnalyticsSourceProfile,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        let store = self.inner.db.store();
        let profile = super::analytics_source_catalog::create(
            store.as_ref(),
            &self.inner.db.collection_contracts(),
            query,
        )?;
        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("analytics source '{}' created", profile.name),
            "create",
        ))
    }
}

fn query_control_event_specs(expr: &QueryExpr) -> Vec<QueryControlEventSpec> {
    use crate::runtime::control_events::{EventKind, Sensitivity};

    let mut specs = Vec::new();
    let mut schema = |action: &'static str, resource: Option<String>| {
        specs.push(QueryControlEventSpec {
            kind: EventKind::SchemaDdl,
            action,
            resource,
            fields: Vec::new(),
        });
    };
    match expr {
        QueryExpr::CreateTable(q) => {
            schema("create_table", Some(format!("table:{}", q.name)));
            if let Some(column) = &q.tenant_by {
                specs.push(QueryControlEventSpec {
                    kind: EventKind::TenantGovernance,
                    action: "create_table_tenant_by",
                    resource: Some(format!("table:{}", q.name)),
                    fields: vec![("tenant_column".to_string(), Sensitivity::raw(column))],
                });
            }
        }
        QueryExpr::CreateCollection(q) => {
            schema("create_collection", Some(format!("collection:{}", q.name)));
        }
        QueryExpr::CreateVector(q) => schema("create_vector", Some(format!("vector:{}", q.name))),
        QueryExpr::DropTable(q) => schema("drop_table", Some(format!("table:{}", q.name))),
        QueryExpr::DropGraph(q) => schema("drop_graph", Some(format!("graph:{}", q.name))),
        QueryExpr::DropVector(q) => schema("drop_vector", Some(format!("vector:{}", q.name))),
        QueryExpr::DropDocument(q) => {
            schema("drop_document", Some(format!("document:{}", q.name)));
        }
        QueryExpr::DropKv(q) => schema("drop_kv", Some(format!("kv:{}", q.name))),
        QueryExpr::DropCollection(q) => {
            schema("drop_collection", Some(format!("collection:{}", q.name)));
        }
        QueryExpr::Truncate(q) => schema("truncate", Some(format!("collection:{}", q.name))),
        QueryExpr::AlterTable(q) => {
            schema("alter_table", Some(format!("table:{}", q.name)));
            for op in &q.operations {
                match op {
                    crate::storage::query::ast::AlterOperation::EnableRowLevelSecurity => {
                        specs.push(QueryControlEventSpec {
                            kind: EventKind::RlsGovernance,
                            action: "enable_rls",
                            resource: Some(format!("table:{}", q.name)),
                            fields: Vec::new(),
                        });
                    }
                    crate::storage::query::ast::AlterOperation::DisableRowLevelSecurity => {
                        specs.push(QueryControlEventSpec {
                            kind: EventKind::RlsGovernance,
                            action: "disable_rls",
                            resource: Some(format!("table:{}", q.name)),
                            fields: Vec::new(),
                        });
                    }
                    crate::storage::query::ast::AlterOperation::EnableTenancy { column } => {
                        specs.push(QueryControlEventSpec {
                            kind: EventKind::TenantGovernance,
                            action: "enable_tenancy",
                            resource: Some(format!("table:{}", q.name)),
                            fields: vec![("tenant_column".to_string(), Sensitivity::raw(column))],
                        });
                    }
                    crate::storage::query::ast::AlterOperation::DisableTenancy => {
                        specs.push(QueryControlEventSpec {
                            kind: EventKind::TenantGovernance,
                            action: "disable_tenancy",
                            resource: Some(format!("table:{}", q.name)),
                            fields: Vec::new(),
                        });
                    }
                    _ => {}
                }
            }
        }
        QueryExpr::CreateIndex(q) => {
            schema(
                "create_index",
                Some(format!("index:{}:{}", q.table, q.name)),
            );
        }
        QueryExpr::DropIndex(q) => {
            schema("drop_index", Some(format!("index:{}:{}", q.table, q.name)));
        }
        QueryExpr::CreateTimeSeries(q) => {
            schema("create_timeseries", Some(format!("timeseries:{}", q.name)));
        }
        QueryExpr::CreateMetric(q) => {
            schema("create_metric", Some(format!("metric:{}", q.path)));
        }
        QueryExpr::AlterMetric(q) => {
            schema("alter_metric", Some(format!("metric:{}", q.path)));
        }
        QueryExpr::CreateSlo(q) => {
            schema("create_slo", Some(format!("slo:{}", q.path)));
        }
        QueryExpr::DropTimeSeries(q) => {
            schema("drop_timeseries", Some(format!("timeseries:{}", q.name)));
        }
        QueryExpr::CreateQueue(q) => schema("create_queue", Some(format!("queue:{}", q.name))),
        QueryExpr::AlterQueue(q) => schema("alter_queue", Some(format!("queue:{}", q.name))),
        QueryExpr::DropQueue(q) => schema("drop_queue", Some(format!("queue:{}", q.name))),
        QueryExpr::CreateTree(q) => {
            schema(
                "create_tree",
                Some(format!("tree:{}:{}", q.collection, q.name)),
            );
        }
        QueryExpr::DropTree(q) => {
            schema(
                "drop_tree",
                Some(format!("tree:{}:{}", q.collection, q.name)),
            );
        }
        QueryExpr::CreateSchema(q) => schema("create_schema", Some(format!("schema:{}", q.name))),
        QueryExpr::DropSchema(q) => schema("drop_schema", Some(format!("schema:{}", q.name))),
        QueryExpr::CreateSequence(q) => {
            schema("create_sequence", Some(format!("sequence:{}", q.name)));
        }
        QueryExpr::DropSequence(q) => schema("drop_sequence", Some(format!("sequence:{}", q.name))),
        QueryExpr::CreateView(q) => schema("create_view", Some(format!("view:{}", q.name))),
        QueryExpr::DropView(q) => schema("drop_view", Some(format!("view:{}", q.name))),
        QueryExpr::RefreshMaterializedView(q) => {
            schema(
                "refresh_materialized_view",
                Some(format!("view:{}", q.name)),
            );
        }
        QueryExpr::CreatePolicy(q) => {
            specs.push(QueryControlEventSpec {
                kind: EventKind::RlsGovernance,
                action: "create_policy",
                resource: Some(format!("table:{}:policy:{}", q.table, q.name)),
                fields: vec![(
                    "target_kind".to_string(),
                    Sensitivity::raw(q.target_kind.as_ident()),
                )],
            });
        }
        QueryExpr::DropPolicy(q) => {
            specs.push(QueryControlEventSpec {
                kind: EventKind::RlsGovernance,
                action: "drop_policy",
                resource: Some(format!("table:{}:policy:{}", q.table, q.name)),
                fields: Vec::new(),
            });
        }
        QueryExpr::SetTenant(value) => {
            let mut fields = Vec::new();
            if let Some(value) = value {
                fields.push(("tenant".to_string(), Sensitivity::raw(value)));
            }
            specs.push(QueryControlEventSpec {
                kind: EventKind::TenantGovernance,
                action: "set_tenant",
                resource: Some("tenant:session".to_string()),
                fields,
            });
        }
        QueryExpr::SetConfig { key, .. } => {
            specs.push(QueryControlEventSpec {
                kind: EventKind::ConfigWrite,
                action: "config:write",
                resource: Some(format!("config:{key}")),
                fields: vec![("key".to_string(), Sensitivity::raw(key))],
            });
        }
        QueryExpr::ConfigCommand(cmd) => match cmd {
            crate::storage::query::ast::ConfigCommand::Put {
                collection, key, ..
            }
            | crate::storage::query::ast::ConfigCommand::Rotate {
                collection, key, ..
            } => {
                let target = format!("{collection}/{key}");
                specs.push(QueryControlEventSpec {
                    kind: EventKind::ConfigWrite,
                    action: "config:write",
                    resource: Some(format!("config:{target}")),
                    fields: vec![
                        ("collection".to_string(), Sensitivity::raw(collection)),
                        ("key".to_string(), Sensitivity::raw(key)),
                    ],
                });
            }
            crate::storage::query::ast::ConfigCommand::Delete { collection, key } => {
                let target = format!("{collection}/{key}");
                specs.push(QueryControlEventSpec {
                    kind: EventKind::ConfigDelete,
                    action: "config:write",
                    resource: Some(format!("config:{target}")),
                    fields: vec![
                        ("collection".to_string(), Sensitivity::raw(collection)),
                        ("key".to_string(), Sensitivity::raw(key)),
                    ],
                });
            }
            _ => {}
        },
        QueryExpr::AlterUser(stmt) => {
            let disables = stmt.attributes.iter().any(|attr| {
                matches!(
                    attr,
                    crate::storage::query::ast::AlterUserAttribute::Disable
                )
            });
            specs.push(QueryControlEventSpec {
                kind: if disables {
                    EventKind::UserDisable
                } else {
                    EventKind::UserUpdate
                },
                action: "alter_user",
                resource: Some(format!("user:{}", stmt.username)),
                fields: Vec::new(),
            });
        }
        QueryExpr::CreateUser(stmt) => {
            specs.push(QueryControlEventSpec {
                kind: EventKind::UserCreate,
                action: "create_user",
                resource: Some(format!("user:{}", stmt.username)),
                fields: Vec::new(),
            });
        }
        _ => {}
    }
    specs
}

pub(crate) fn control_event_outcome_for_error(
    err: &RedDBError,
) -> crate::runtime::control_events::Outcome {
    match err {
        RedDBError::ReadOnly(_) => crate::runtime::control_events::Outcome::Denied,
        RedDBError::Query(msg)
            if msg.contains("permission denied")
                || msg.contains("cannot issue")
                || msg.contains("lacks") =>
        {
            crate::runtime::control_events::Outcome::Denied
        }
        _ => crate::runtime::control_events::Outcome::Error,
    }
}

/// Convert the rows produced by a materialized-view body into
/// `UnifiedEntity` table rows targeting the backing collection.
/// Issue #595 slice 9c — feeds `UnifiedStore::refresh_collection`.
///
/// Graph fragments and vector hits are ignored: a materialized view
/// is a relational result set (SELECT-shaped); slices 11+ may extend
/// this once we have a richer view body shape. Each row materialises
/// the union of its schema-bound columns + overflow.
fn view_records_to_entities(
    table: &str,
    records: &[crate::storage::query::unified::UnifiedRecord],
) -> Vec<crate::storage::UnifiedEntity> {
    use std::collections::HashMap;
    let table_arc: std::sync::Arc<str> = std::sync::Arc::from(table);
    let mut out = Vec::with_capacity(records.len());
    for record in records {
        let mut named: HashMap<String, crate::storage::schema::Value> = HashMap::new();
        for (name, value) in record.iter_fields() {
            named.insert(name.to_string(), value.clone());
        }
        let entity = crate::storage::UnifiedEntity::new(
            crate::storage::EntityId::new(0),
            crate::storage::EntityKind::TableRow {
                table: std::sync::Arc::clone(&table_arc),
                row_id: 0,
            },
            crate::storage::EntityData::Row(crate::storage::RowData {
                columns: Vec::new(),
                named: Some(named),
                schema: None,
            }),
        );
        out.push(entity);
    }
    out
}

fn system_keyed_collection_contract(
    name: &str,
    model: crate::catalog::CollectionModel,
) -> crate::physical::CollectionContract {
    let now = crate::utils::now_unix_millis() as u128;
    crate::physical::CollectionContract {
        name: name.to_string(),
        declared_model: model,
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
        append_only: false,
        subscriptions: Vec::new(),
        analytics_config: Vec::new(),
        session_key: None,
        session_gap_ms: None,
        retention_duration_ms: None,
        analytical_storage: None,

        ai_policy: None,
    }
}

pub use super::execution_context::{
    capture_current_snapshot, clear_current_auth_identity, clear_current_connection_id,
    clear_current_snapshot, clear_current_tenant, current_auth_identity_for_audit,
    current_connection_id, current_tenant, entity_visible_under_current_snapshot,
    entity_visible_with_context, set_current_auth_identity, set_current_connection_id,
    set_current_snapshot, set_current_tenant, snapshot_bundle, with_snapshot_bundle,
    SnapshotBundle, SnapshotContext,
};
pub(crate) use super::execution_context::{
    current_auth_identity, current_config_value, current_role_projected, current_scope_override,
    current_secret_value, current_snapshot_requires_index_fallback, current_user_projected,
    has_scope_override_active, parse_set_local_tenant, update_current_config_value,
    update_current_secret_value, xids_visible_under_current_snapshot, ConfigSnapshotGuard,
    CurrentSnapshotGuard, ScopeOverrideGuard, SecretStoreGuard, TxLocalTenantGuard,
};

fn table_row_index_fields(
    entity: &crate::storage::unified::entity::UnifiedEntity,
) -> Vec<(String, crate::storage::schema::Value)> {
    let crate::storage::EntityData::Row(row) = &entity.data else {
        return Vec::new();
    };
    if let Some(named) = &row.named {
        return named
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
    }
    if let Some(schema) = &row.schema {
        return schema
            .iter()
            .zip(row.columns.iter())
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
    }
    Vec::new()
}

fn named_text(
    named: &std::collections::HashMap<String, crate::storage::schema::Value>,
    key: &str,
) -> Option<String> {
    match named.get(key) {
        Some(crate::storage::schema::Value::Text(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn named_bool(
    named: &std::collections::HashMap<String, crate::storage::schema::Value>,
    key: &str,
) -> Option<bool> {
    match named.get(key) {
        Some(crate::storage::schema::Value::Boolean(value)) => Some(*value),
        _ => None,
    }
}

fn index_method_kind_as_str(method: super::index_store::IndexMethodKind) -> &'static str {
    match method {
        super::index_store::IndexMethodKind::Hash => "hash",
        super::index_store::IndexMethodKind::Bitmap => "bitmap",
        super::index_store::IndexMethodKind::Spatial => "spatial",
        super::index_store::IndexMethodKind::BTree => "btree",
    }
}

fn index_method_kind_from_str(raw: &str) -> Option<super::index_store::IndexMethodKind> {
    match raw {
        "hash" => Some(super::index_store::IndexMethodKind::Hash),
        "bitmap" => Some(super::index_store::IndexMethodKind::Bitmap),
        "spatial" | "rtree" => Some(super::index_store::IndexMethodKind::Spatial),
        "btree" => Some(super::index_store::IndexMethodKind::BTree),
        _ => None,
    }
}

fn runtime_pool_lock(runtime: &RedDBRuntime) -> std::sync::MutexGuard<'_, PoolState> {
    runtime
        .inner
        .pool
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The graph-analytics table-valued functions recognized in FROM position.
/// Both the graph-collection form and the inline `nodes => / edges =>` form
/// (issue #799) accept these names.
fn is_graph_tvf_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("components")
        || name.eq_ignore_ascii_case("louvain")
        || name.eq_ignore_ascii_case("degree_centrality")
        || name.eq_ignore_ascii_case("shortest_path")
        || name.eq_ignore_ascii_case("betweenness")
        || name.eq_ignore_ascii_case("eigenvector")
        || name.eq_ignore_ascii_case("pagerank")
}

/// Map a declared `WITH ANALYTICS` view to the concrete graph algorithm name
/// and named-argument list that [`RedDBRuntime::dispatch_graph_algorithm`]
/// consumes (issue #800). The `using` option selects the algorithm inside the
/// output family; unsupported algorithms and the options that do not apply to
/// the chosen algorithm are rejected so a view never silently ignores a
/// declared parameter.
fn analytics_view_algorithm(
    graph: &str,
    view: &crate::catalog::AnalyticsViewDescriptor,
) -> RedDBResult<(String, Vec<(String, f64)>)> {
    use crate::catalog::AnalyticsOutput;

    let mut named_args: Vec<(String, f64)> = Vec::new();
    let algorithm = match view.output {
        AnalyticsOutput::Communities => {
            let algo = view.algorithm.as_deref().unwrap_or("louvain");
            if !algo.eq_ignore_ascii_case("louvain") {
                return Err(RedDBError::Query(format!(
                    "analytics output 'communities' on graph '{graph}' has unsupported algorithm '{algo}' (expected louvain)"
                )));
            }
            if let Some(resolution) = view.resolution {
                named_args.push(("resolution".to_string(), resolution));
            }
            "louvain".to_string()
        }
        AnalyticsOutput::Components => {
            if let Some(algo) = view.algorithm.as_deref() {
                if !algo.eq_ignore_ascii_case("components")
                    && !algo.eq_ignore_ascii_case("connected_components")
                {
                    return Err(RedDBError::Query(format!(
                        "analytics output 'components' on graph '{graph}' has unsupported algorithm '{algo}' (expected connected_components)"
                    )));
                }
            }
            "components".to_string()
        }
        AnalyticsOutput::Centrality => {
            let algo = view
                .algorithm
                .as_deref()
                .unwrap_or("pagerank")
                .to_ascii_lowercase();
            match algo.as_str() {
                "pagerank" => {
                    if let Some(max_iterations) = view.max_iterations {
                        named_args.push(("max_iterations".to_string(), max_iterations as f64));
                    }
                }
                "eigenvector" => {
                    if let Some(max_iterations) = view.max_iterations {
                        named_args.push(("max_iterations".to_string(), max_iterations as f64));
                    }
                    if let Some(tolerance) = view.tolerance {
                        named_args.push(("tolerance".to_string(), tolerance));
                    }
                }
                "betweenness" => {}
                other => {
                    return Err(RedDBError::Query(format!(
                        "analytics output 'centrality' on graph '{graph}' has unsupported algorithm '{other}' (expected pagerank, betweenness, or eigenvector)"
                    )));
                }
            }
            algo
        }
    };
    Ok((algorithm, named_args))
}

/// Reject any named arguments for a TVF that accepts none.
fn reject_named_args(name: &str, named_args: &[(String, f64)]) -> RedDBResult<()> {
    if let Some((key, _)) = named_args.first() {
        return Err(RedDBError::Query(format!(
            "table function '{name}' has no named argument '{key}'"
        )));
    }
    Ok(())
}

/// Resolve louvain's optional `resolution` named arg (γ, default 1.0). Any
/// other named key, or a non-finite / non-positive resolution, is rejected.
fn louvain_resolution(named_args: &[(String, f64)]) -> RedDBResult<f64> {
    let mut resolution = 1.0_f64;
    for (key, value) in named_args {
        if key.eq_ignore_ascii_case("resolution") {
            if !value.is_finite() || *value <= 0.0 {
                return Err(RedDBError::Query(format!(
                    "table function 'louvain' resolution must be > 0, got {value}"
                )));
            }
            resolution = *value;
        } else {
            return Err(RedDBError::Query(format!(
                "table function 'louvain' has no named argument '{key}' (expected 'resolution')"
            )));
        }
    }
    Ok(resolution)
}

/// Undirected degree centrality over abstract inputs: each edge contributes
/// 1 to both of its endpoints. Returns `(node_id, degree)` deterministically
/// in ascending node-id order, so identical input always yields identical
/// rows.
fn abstract_degree_centrality(
    nodes: &[String],
    edges: &[(
        String,
        String,
        crate::storage::engine::graph_algorithms::Weight,
    )],
) -> Vec<(String, usize)> {
    let mut degree: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for n in nodes {
        degree.entry(n.clone()).or_insert(0);
    }
    for (a, b, _w) in edges {
        *degree.entry(a.clone()).or_insert(0) += 1;
        *degree.entry(b.clone()).or_insert(0) += 1;
    }
    degree.into_iter().collect()
}

/// Ordered column names for a materialized subquery result: the projection
/// columns when present, else the first record's field order.
fn ordered_result_columns(result: &crate::storage::query::unified::UnifiedResult) -> Vec<String> {
    if !result.columns.is_empty() {
        return result.columns.clone();
    }
    result
        .records
        .first()
        .map(|record| {
            record
                .column_names()
                .iter()
                .map(|column| column.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Canonical node-id string for a cell value, so the node universe (from the
/// `nodes` subquery) and the edge endpoints (from the `edges` subquery)
/// compare equal regardless of integer-vs-text typing. `Null` is not a node.
fn value_to_node_id(value: &crate::storage::schema::Value) -> Option<String> {
    use crate::storage::schema::Value;
    match value {
        Value::Null => None,
        Value::Text(s) => Some(s.to_string()),
        Value::Integer(n) => Some(n.to_string()),
        Value::UnsignedInteger(n) => Some(n.to_string()),
        Value::NodeRef(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// Numeric edge weight from a cell value (the optional third `edges` column).
fn value_to_weight(value: &crate::storage::schema::Value) -> Option<f32> {
    use crate::storage::schema::Value;
    match value {
        Value::Float(f) => Some(*f as f32),
        Value::Integer(n) => Some(*n as f32),
        Value::UnsignedInteger(n) => Some(*n as f32),
        _ => None,
    }
}

/// Build the node universe from a materialized `nodes` subquery result: the
/// first projected column of each row is the node id (issue #799). Zero rows
/// is a valid empty node set; a row set with no columns is a shape error.
fn inline_node_ids(
    name: &str,
    result: &crate::storage::query::unified::UnifiedResult,
) -> RedDBResult<Vec<String>> {
    if result.records.is_empty() {
        return Ok(Vec::new());
    }
    let columns = ordered_result_columns(result);
    let Some(first_col) = columns.first() else {
        return Err(RedDBError::Query(format!(
            "table function '{name}' inline form: `nodes` subquery must project at least one column (the node id)"
        )));
    };
    let mut ids = Vec::with_capacity(result.records.len());
    for record in &result.records {
        if let Some(id) = record.get(first_col).and_then(value_to_node_id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Build the edge list from a materialized `edges` subquery result: the first
/// two projected columns are `(source, target)` and an optional third column
/// is the numeric weight (defaulting to 1.0). Fewer than two columns is a
/// shape error (issue #799).
fn inline_edges(
    name: &str,
    result: &crate::storage::query::unified::UnifiedResult,
) -> RedDBResult<
    Vec<(
        String,
        String,
        crate::storage::engine::graph_algorithms::Weight,
    )>,
> {
    if result.records.is_empty() {
        return Ok(Vec::new());
    }
    let columns = ordered_result_columns(result);
    if columns.len() < 2 {
        return Err(RedDBError::Query(format!(
            "table function '{name}' inline form: `edges` subquery must project at least two columns (source, target), got {}",
            columns.len()
        )));
    }
    let src_col = &columns[0];
    let dst_col = &columns[1];
    let weight_col = columns.get(2);
    let mut edges = Vec::with_capacity(result.records.len());
    for record in &result.records {
        let (Some(src), Some(dst)) = (
            record.get(src_col).and_then(value_to_node_id),
            record.get(dst_col).and_then(value_to_node_id),
        ) else {
            // A null/absent endpoint is not a valid edge; skip it.
            continue;
        };
        let weight = match weight_col {
            Some(col) => match record.get(col) {
                None | Some(crate::storage::schema::Value::Null) => 1.0,
                Some(value) => value_to_weight(value).ok_or_else(|| {
                    RedDBError::Query(format!(
                        "table function '{name}' inline form: `edges` weight column must be numeric"
                    ))
                })?,
            },
            None => 1.0,
        };
        edges.push((src, dst, weight));
    }
    Ok(edges)
}

fn cache_scope_insert(scopes: &mut HashSet<String>, name: &str) {
    if name.is_empty() || name.starts_with("__subq_") || is_universal_query_source(name) {
        return;
    }
    scopes.insert(name.to_string());
}

fn collect_table_source_scopes(scopes: &mut HashSet<String>, query: &TableQuery) {
    match query.source.as_ref() {
        Some(crate::storage::query::ast::TableSource::Name(name)) => {
            cache_scope_insert(scopes, name)
        }
        Some(crate::storage::query::ast::TableSource::Subquery(subquery)) => {
            collect_query_expr_result_cache_scopes(scopes, subquery);
        }
        // Graph-collection TVFs (e.g. `louvain(g)`) read the graph store
        // read-only. The result is now cached (issue #802) and scoped to the
        // graph collection named in the first argument, so any mutation on
        // that collection (`INSERT INTO g NODE/EDGE …`) invalidates the
        // entry via `invalidate_result_cache_for_table`. Non-graph or
        // zero-arg functions contribute no scope.
        Some(crate::storage::query::ast::TableSource::Function { name, args, .. }) => {
            if is_graph_tvf_name(name) {
                if let Some(graph) = args.first() {
                    cache_scope_insert(scopes, graph);
                }
            }
        }
        // The inline-graph form reads ordinary tables/docs through its
        // `nodes`/`edges` subqueries, so its result cache must be scoped to
        // those source collections — mutating any of them invalidates the
        // cached result (issue #799).
        Some(crate::storage::query::ast::TableSource::InlineGraphFunction {
            nodes, edges, ..
        }) => {
            collect_query_expr_result_cache_scopes(scopes, nodes);
            collect_query_expr_result_cache_scopes(scopes, edges);
        }
        None => cache_scope_insert(scopes, &query.table),
    }
}

fn collect_vector_source_scopes(
    scopes: &mut HashSet<String>,
    source: &crate::storage::query::ast::VectorSource,
) {
    match source {
        crate::storage::query::ast::VectorSource::Reference { collection, .. } => {
            cache_scope_insert(scopes, collection);
        }
        crate::storage::query::ast::VectorSource::Subquery(subquery) => {
            collect_query_expr_result_cache_scopes(scopes, subquery);
        }
        crate::storage::query::ast::VectorSource::Literal(_)
        | crate::storage::query::ast::VectorSource::Text(_) => {}
    }
}

fn collect_path_selector_scopes(
    scopes: &mut HashSet<String>,
    selector: &crate::storage::query::ast::NodeSelector,
) {
    if let crate::storage::query::ast::NodeSelector::ByRow { table, .. } = selector {
        cache_scope_insert(scopes, table);
    }
}

fn collect_query_expr_result_cache_scopes(scopes: &mut HashSet<String>, expr: &QueryExpr) {
    match expr {
        QueryExpr::Table(query) => collect_table_source_scopes(scopes, query),
        QueryExpr::Join(query) => {
            collect_query_expr_result_cache_scopes(scopes, &query.left);
            collect_query_expr_result_cache_scopes(scopes, &query.right);
        }
        QueryExpr::Path(query) => {
            collect_path_selector_scopes(scopes, &query.from);
            collect_path_selector_scopes(scopes, &query.to);
        }
        QueryExpr::Vector(query) => {
            cache_scope_insert(scopes, &query.collection);
            collect_vector_source_scopes(scopes, &query.query_vector);
        }
        QueryExpr::Hybrid(query) => {
            collect_query_expr_result_cache_scopes(scopes, &query.structured);
            cache_scope_insert(scopes, &query.vector.collection);
            collect_vector_source_scopes(scopes, &query.vector.query_vector);
        }
        QueryExpr::Insert(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::Update(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::Delete(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::CreateTable(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::CreateCollection(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::CreateVector(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropTable(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropGraph(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropVector(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropDocument(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropKv(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropCollection(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::Truncate(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::AlterTable(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::CreateIndex(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::DropIndex(query) => cache_scope_insert(scopes, &query.table),
        QueryExpr::CreateTimeSeries(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::CreateMetric(query) => cache_scope_insert(scopes, &query.path),
        QueryExpr::AlterMetric(query) => cache_scope_insert(scopes, &query.path),
        QueryExpr::CreateSlo(query) => cache_scope_insert(scopes, &query.path),
        QueryExpr::DropTimeSeries(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::CreateQueue(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::AlterQueue(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::DropQueue(query) => cache_scope_insert(scopes, &query.name),
        QueryExpr::QueueSelect(query) => cache_scope_insert(scopes, &query.queue),
        QueryExpr::QueueCommand(query) => match query {
            QueueCommand::Push { queue, .. }
            | QueueCommand::Pop { queue, .. }
            | QueueCommand::Peek { queue, .. }
            | QueueCommand::Len { queue }
            | QueueCommand::Purge { queue }
            | QueueCommand::GroupCreate { queue, .. }
            | QueueCommand::GroupRead { queue, .. }
            | QueueCommand::Pending { queue, .. }
            | QueueCommand::Claim { queue, .. }
            | QueueCommand::Ack { queue, .. }
            | QueueCommand::Nack { queue, .. } => cache_scope_insert(scopes, queue),
            QueueCommand::Move {
                source,
                destination,
                ..
            } => {
                cache_scope_insert(scopes, source);
                cache_scope_insert(scopes, destination);
            }
        },
        QueryExpr::EventsBackfill(query) => {
            cache_scope_insert(scopes, &query.collection);
            cache_scope_insert(scopes, &query.target_queue);
        }
        QueryExpr::CreateTree(query) => cache_scope_insert(scopes, &query.collection),
        QueryExpr::DropTree(query) => cache_scope_insert(scopes, &query.collection),
        QueryExpr::TreeCommand(query) => match query {
            TreeCommand::Insert { collection, .. }
            | TreeCommand::Move { collection, .. }
            | TreeCommand::Delete { collection, .. }
            | TreeCommand::Validate { collection, .. }
            | TreeCommand::Rebalance { collection, .. } => cache_scope_insert(scopes, collection),
        },
        QueryExpr::SearchCommand(query) => match query {
            SearchCommand::Similar { collection, .. }
            | SearchCommand::Hybrid { collection, .. }
            | SearchCommand::SpatialRadius { collection, .. }
            | SearchCommand::SpatialBbox { collection, .. }
            | SearchCommand::SpatialNearest { collection, .. } => {
                cache_scope_insert(scopes, collection);
            }
            SearchCommand::Text { collection, .. }
            | SearchCommand::Multimodal { collection, .. }
            | SearchCommand::Index { collection, .. }
            | SearchCommand::Context { collection, .. } => {
                if let Some(collection) = collection.as_deref() {
                    cache_scope_insert(scopes, collection);
                }
            }
        },
        QueryExpr::Ask(query) => {
            if let Some(collection) = query.collection.as_deref() {
                cache_scope_insert(scopes, collection);
            }
        }
        QueryExpr::ExplainAlter(query) => cache_scope_insert(scopes, &query.target.name),
        QueryExpr::MaintenanceCommand(cmd) => match cmd {
            crate::storage::query::ast::MaintenanceCommand::Vacuum { target, .. }
            | crate::storage::query::ast::MaintenanceCommand::Analyze { target } => {
                if let Some(t) = target {
                    cache_scope_insert(scopes, t);
                }
            }
        },
        QueryExpr::CopyFrom(cmd) => cache_scope_insert(scopes, &cmd.table),
        QueryExpr::CreateView(cmd) => {
            cache_scope_insert(scopes, &cmd.name);
            // Invalidating the view should also invalidate its dependencies.
            collect_query_expr_result_cache_scopes(scopes, &cmd.query);
        }
        QueryExpr::DropView(cmd) => cache_scope_insert(scopes, &cmd.name),
        QueryExpr::RefreshMaterializedView(cmd) => cache_scope_insert(scopes, &cmd.name),
        QueryExpr::CreatePolicy(cmd) => cache_scope_insert(scopes, &cmd.table),
        QueryExpr::DropPolicy(cmd) => cache_scope_insert(scopes, &cmd.table),
        QueryExpr::CreateServer(_) | QueryExpr::DropServer(_) => {}
        QueryExpr::CreateForeignTable(cmd) => cache_scope_insert(scopes, &cmd.name),
        QueryExpr::DropForeignTable(cmd) => cache_scope_insert(scopes, &cmd.name),
        QueryExpr::Graph(_)
        | QueryExpr::GraphCommand(_)
        | QueryExpr::ProbabilisticCommand(_)
        | QueryExpr::SetConfig { .. }
        | QueryExpr::ShowConfig { .. }
        | QueryExpr::SetSecret { .. }
        | QueryExpr::DeleteSecret { .. }
        | QueryExpr::ShowSecrets { .. }
        | QueryExpr::SetTenant(_)
        | QueryExpr::ShowTenant
        | QueryExpr::TransactionControl(_)
        | QueryExpr::CreateSchema(_)
        | QueryExpr::DropSchema(_)
        | QueryExpr::CreateSequence(_)
        | QueryExpr::DropSequence(_)
        | QueryExpr::Grant(_)
        | QueryExpr::Revoke(_)
        | QueryExpr::AlterUser(_)
        | QueryExpr::CreateUser(_)
        | QueryExpr::CreateIamPolicy { .. }
        | QueryExpr::DropIamPolicy { .. }
        | QueryExpr::AttachPolicy { .. }
        | QueryExpr::DetachPolicy { .. }
        | QueryExpr::ShowPolicies { .. }
        | QueryExpr::ShowEffectivePermissions { .. }
        | QueryExpr::RankOf(_)
        | QueryExpr::ApproxRankOf(_)
        | QueryExpr::RankRange(_)
        | QueryExpr::SimulatePolicy { .. }
        | QueryExpr::LintPolicy { .. }
        | QueryExpr::MigratePolicyMode { .. }
        | QueryExpr::CreateMigration(_)
        | QueryExpr::ApplyMigration(_)
        | QueryExpr::RollbackMigration(_)
        | QueryExpr::ExplainMigration(_)
        | QueryExpr::EventsBackfillStatus { .. } => {}
        QueryExpr::KvCommand(cmd) => {
            use crate::storage::query::ast::KvCommand;
            match cmd {
                KvCommand::Put { collection, .. }
                | KvCommand::InvalidateTags { collection, .. }
                | KvCommand::Get { collection, .. }
                | KvCommand::Unseal { collection, .. }
                | KvCommand::Rotate { collection, .. }
                | KvCommand::History { collection, .. }
                | KvCommand::List { collection, .. }
                | KvCommand::Purge { collection, .. }
                | KvCommand::Watch { collection, .. }
                | KvCommand::Delete { collection, .. }
                | KvCommand::Incr { collection, .. }
                | KvCommand::Cas { collection, .. } => cache_scope_insert(scopes, collection),
            }
        }
        QueryExpr::ConfigCommand(cmd) => {
            use crate::storage::query::ast::ConfigCommand;
            match cmd {
                ConfigCommand::Put { collection, .. }
                | ConfigCommand::Get { collection, .. }
                | ConfigCommand::Resolve { collection, .. }
                | ConfigCommand::Rotate { collection, .. }
                | ConfigCommand::Delete { collection, .. }
                | ConfigCommand::History { collection, .. }
                | ConfigCommand::List { collection, .. }
                | ConfigCommand::Watch { collection, .. }
                | ConfigCommand::InvalidVolatileOperation { collection, .. } => {
                    cache_scope_insert(scopes, collection)
                }
            }
        }
    }
}

/// Combine matching RLS policies for a table + action into a single
/// `Filter` suitable for AND-ing into a caller's `WHERE` clause.
///
/// Returns `None` when RLS is disabled or no policy admits the caller's
/// role — callers use that to short-circuit the mutation (for DELETE /
/// UPDATE we simply skip the operation, which PG expresses as "no rows
/// match the policy + predicate combination").
pub(crate) fn rls_policy_filter(
    runtime: &RedDBRuntime,
    table: &str,
    action: crate::storage::query::ast::PolicyAction,
) -> Option<crate::storage::query::ast::Filter> {
    rls_policy_filter_for_kind(
        runtime,
        table,
        action,
        crate::storage::query::ast::PolicyTargetKind::Table,
    )
}

/// Kind-aware policy filter combiner (Phase 2.5.5 RLS universal).
/// Graph / vector / queue / timeseries scans pass the concrete kind;
/// policies targeting other kinds are ignored. Legacy Table-scoped
/// policies still apply cross-kind — callers register auto-tenancy
/// policies as Table today.
pub(crate) fn rls_policy_filter_for_kind(
    runtime: &RedDBRuntime,
    table: &str,
    action: crate::storage::query::ast::PolicyAction,
    kind: crate::storage::query::ast::PolicyTargetKind,
) -> Option<crate::storage::query::ast::Filter> {
    use crate::storage::query::ast::Filter;

    if !runtime.inner.rls_enabled_tables.read().contains(table) {
        return None;
    }
    let role = current_auth_identity().map(|(_, role)| role);
    let role_str = role.map(|r| r.as_str().to_string());
    let policies = runtime.matching_rls_policies_for_kind(table, role_str.as_deref(), action, kind);
    if policies.is_empty() {
        return None;
    }
    policies
        .into_iter()
        .reduce(|acc, f| Filter::Or(Box::new(acc), Box::new(f)))
}

/// Returns true when the table has RLS enforcement enabled. Convenience
/// shortcut so DML paths can gate the AND-combine work without reaching
/// into `runtime.inner.rls_enabled_tables` directly.
pub(crate) fn rls_is_enabled(runtime: &RedDBRuntime, table: &str) -> bool {
    runtime.inner.rls_enabled_tables.read().contains(table)
}

/// Per-entity gate used by the graph materialiser for `GraphNode`
/// entities. RLS is checked against the source collection with
/// `kind = Nodes`, which `matching_rls_policies_for_kind` resolves to
/// either `Nodes`-targeted policies or legacy `Table`-targeted ones
/// (for back-compat with auto-tenancy declarations). Cached per
/// collection so big graphs only resolve the policy chain once.
fn node_passes_rls(
    runtime: &RedDBRuntime,
    collection: &str,
    role: Option<&str>,
    cache: &mut std::collections::HashMap<String, Option<crate::storage::query::ast::Filter>>,
    entity: &crate::storage::unified::entity::UnifiedEntity,
) -> bool {
    use crate::storage::query::ast::{Filter, PolicyAction, PolicyTargetKind};

    if !runtime.inner.rls_enabled_tables.read().contains(collection) {
        return true;
    }
    let filter = cache.entry(collection.to_string()).or_insert_with(|| {
        let policies = runtime.matching_rls_policies_for_kind(
            collection,
            role,
            PolicyAction::Select,
            PolicyTargetKind::Nodes,
        );
        if policies.is_empty() {
            None
        } else {
            policies
                .into_iter()
                .reduce(|acc, f| Filter::Or(Box::new(acc), Box::new(f)))
        }
    });
    let Some(filter) = filter else {
        return false;
    };
    crate::runtime::query_exec::evaluate_entity_filter_with_db(
        Some(&runtime.inner.db),
        entity,
        filter,
        collection,
        collection,
    )
}

/// Edge counterpart of `node_passes_rls`. Same caching strategy with
/// `kind = Edges`.
fn edge_passes_rls(
    runtime: &RedDBRuntime,
    collection: &str,
    role: Option<&str>,
    cache: &mut std::collections::HashMap<String, Option<crate::storage::query::ast::Filter>>,
    entity: &crate::storage::unified::entity::UnifiedEntity,
) -> bool {
    use crate::storage::query::ast::{Filter, PolicyAction, PolicyTargetKind};

    if !runtime.inner.rls_enabled_tables.read().contains(collection) {
        return true;
    }
    let filter = cache.entry(collection.to_string()).or_insert_with(|| {
        let policies = runtime.matching_rls_policies_for_kind(
            collection,
            role,
            PolicyAction::Select,
            PolicyTargetKind::Edges,
        );
        if policies.is_empty() {
            None
        } else {
            policies
                .into_iter()
                .reduce(|acc, f| Filter::Or(Box::new(acc), Box::new(f)))
        }
    });
    let Some(filter) = filter else {
        return false;
    };
    crate::runtime::query_exec::evaluate_entity_filter_with_db(
        Some(&runtime.inner.db),
        entity,
        filter,
        collection,
        collection,
    )
}

/// RLS policy injection (Phase 2.5.2 PG parity).
///
/// Fetch every matching policy for the current thread-local role and
/// fold them into the query's filter. Semantics mirror PostgreSQL:
///
/// * Multiple policies on the same table combine with **OR** — a row is
///   visible if *any* policy admits it.
/// * The combined policy predicate is **AND**-ed into the caller's
///   existing `WHERE` clause so explicit predicates continue to trim
///   the policy-allowed set.
/// * No matching policies + RLS enabled = zero rows (PG's
///   restrictive-default). Callers get `None` and return an empty
///   `UnifiedResult` without ever dispatching the scan.
///
/// This runs only when `RuntimeInner::rls_enabled_tables` already
/// contains the table name — callers gate the hot path upfront to
/// avoid the lock acquisition on tables without RLS.
///
/// Returns `None` when no policy admits the current role; returns
/// `Some(mutated_table)` with policy filters folded in otherwise.
fn inject_rls_filters(
    runtime: &RedDBRuntime,
    frame: &dyn super::statement_frame::ReadFrame,
    mut table: crate::storage::query::ast::TableQuery,
) -> Option<crate::storage::query::ast::TableQuery> {
    use crate::storage::query::ast::{Filter, PolicyAction};

    // `None` role falls through to policies with no `TO role` clause.
    let role = frame.identity().map(|(_, role)| role);
    let role_str = role.map(|r| r.as_str().to_string());
    let policies =
        runtime.matching_rls_policies(&table.table, role_str.as_deref(), PolicyAction::Select);

    if policies.is_empty() {
        // RLS enabled + no policy match = deny everything. Signal the
        // caller to short-circuit with an empty result set.
        return None;
    }

    // Combine policy predicates with OR (PG's permissive default).
    let combined = policies
        .into_iter()
        .reduce(|acc, f| Filter::Or(Box::new(acc), Box::new(f)))
        .expect("policies non-empty");

    // AND into the caller's existing predicate. The predicate may live
    // in `where_expr` rather than `filter`: `resolve_table_expr_subqueries`
    // nulls `filter` whenever `where_expr` is present (the case for a
    // view body rewritten into `SELECT … WHERE …`). Folding only into
    // `filter` here would silently drop that `where_expr` predicate at
    // eval time because `effective_table_filter` prefers `filter` —
    // e.g. `WITHIN TENANT … SELECT * FROM <view>` would apply the
    // tenant policy but lose the view's own WHERE (#635).
    use crate::storage::query::sql_lowering::{expr_to_filter, filter_to_expr};
    let had_where_expr = table.where_expr.is_some();
    let existing = table
        .filter
        .take()
        .or_else(|| table.where_expr.as_ref().map(expr_to_filter));
    let new_filter = match existing {
        Some(existing) => Filter::And(Box::new(existing), Box::new(combined)),
        None => combined,
    };
    // Keep `where_expr` in lock-step with the merged `filter` so
    // whichever the executor consults sees the full predicate.
    if had_where_expr {
        table.where_expr = Some(filter_to_expr(&new_filter));
    }
    table.filter = Some(new_filter);
    Some(table)
}

/// Apply per-table RLS to a `JoinQuery` by folding each side's policy
/// predicate into the join's outer filter. Walking the merged record
/// at the join layer (rather than mutating the per-side scan filter)
/// keeps the planner's strategy choice and per-side index selection
/// undisturbed — the policy predicate uses the qualified `t.col` form
/// that resolves cleanly against the merged record's keys.
///
/// Returns `None` when any leaf has RLS enabled and no policy admits
/// the caller — the join short-circuits to an empty result.
fn inject_rls_into_join(
    runtime: &RedDBRuntime,
    frame: &dyn super::statement_frame::ReadFrame,
    mut join: crate::storage::query::ast::JoinQuery,
) -> Option<crate::storage::query::ast::JoinQuery> {
    use crate::storage::query::ast::Filter;

    let mut policy_filters: Vec<Filter> = Vec::new();
    if !collect_join_side_policy(runtime, frame, join.left.as_ref(), &mut policy_filters) {
        return None;
    }
    if !collect_join_side_policy(runtime, frame, join.right.as_ref(), &mut policy_filters) {
        return None;
    }

    if policy_filters.is_empty() {
        return Some(join);
    }

    let combined = policy_filters
        .into_iter()
        .reduce(|acc, f| Filter::And(Box::new(acc), Box::new(f)))
        .expect("policy_filters non-empty");

    join.filter = Some(match join.filter.take() {
        Some(existing) => Filter::And(Box::new(existing), Box::new(combined)),
        None => combined,
    });

    Some(join)
}

/// For each `Table` leaf reachable through nested joins, append the
/// RLS-policy filter (combined with OR across that side's matching
/// policies) into `out`. Returns `false` when a side has RLS enabled
/// but no policy admits the caller — the join must short-circuit.
fn collect_join_side_policy(
    runtime: &RedDBRuntime,
    frame: &dyn super::statement_frame::ReadFrame,
    expr: &crate::storage::query::ast::QueryExpr,
    out: &mut Vec<crate::storage::query::ast::Filter>,
) -> bool {
    use crate::storage::query::ast::{Filter, PolicyAction, QueryExpr};
    match expr {
        QueryExpr::Table(t) => {
            if !runtime.inner.rls_enabled_tables.read().contains(&t.table) {
                return true;
            }
            let role = frame.identity().map(|(_, role)| role);
            let role_str = role.map(|r| r.as_str().to_string());
            let policies =
                runtime.matching_rls_policies(&t.table, role_str.as_deref(), PolicyAction::Select);
            if policies.is_empty() {
                return false;
            }
            let combined = policies
                .into_iter()
                .reduce(|acc, f| Filter::Or(Box::new(acc), Box::new(f)))
                .expect("policies non-empty");
            out.push(combined);
            true
        }
        QueryExpr::Join(inner) => {
            collect_join_side_policy(runtime, frame, inner.left.as_ref(), out)
                && collect_join_side_policy(runtime, frame, inner.right.as_ref(), out)
        }
        _ => true,
    }
}

/// Foreign-table post-scan filter application (Phase 3.2.2 PG parity).
///
/// Phase 3.2 FDW wrappers don't advertise filter pushdown, so the runtime
/// applies `WHERE` / `ORDER BY` / `LIMIT` / `OFFSET` after the wrapper
/// materialises all rows. Projections are best-effort — when the query
/// lists explicit columns we keep only those; a `SELECT *` keeps every
/// wrapper-emitted field verbatim.
///
/// When a wrapper later opts into pushdown (`supports_pushdown = true`)
/// the runtime will pass the compiled filter down instead of post-filtering.
fn apply_foreign_table_filters(
    records: Vec<crate::storage::query::unified::UnifiedRecord>,
    query: &crate::storage::query::ast::TableQuery,
) -> crate::storage::query::unified::UnifiedResult {
    use crate::storage::query::sql_lowering::{
        effective_table_filter, effective_table_projections,
    };
    use crate::storage::query::unified::UnifiedResult;

    let filter = effective_table_filter(query);
    let projections = effective_table_projections(query);

    // Step 1 — WHERE. Reuse the cross-store evaluator so the semantics
    // match native-collection queries (same operators, same NULL handling).
    let mut filtered: Vec<_> = records
        .into_iter()
        .filter(|record| match &filter {
            Some(f) => {
                super::join_filter::evaluate_runtime_filter_with_db(None, record, f, None, None)
            }
            None => true,
        })
        .collect();

    // Step 2 — LIMIT / OFFSET. Applied after filter to match SQL semantics.
    if let Some(offset) = query.offset {
        let offset = offset as usize;
        if offset >= filtered.len() {
            filtered.clear();
        } else {
            filtered.drain(0..offset);
        }
    }
    if let Some(limit) = query.limit {
        filtered.truncate(limit as usize);
    }

    // Step 3 — columns list. `SELECT *` (no explicit projections) keeps
    // the wrapper's column set; an explicit list trims to those names.
    let columns: Vec<String> = if projections.is_empty() {
        filtered
            .first()
            .map(|r| r.column_names().iter().map(|k| k.to_string()).collect())
            .unwrap_or_default()
    } else {
        projections
            .iter()
            .map(super::join_filter::projection_name)
            .collect()
    };

    let mut result = UnifiedResult::empty();
    result.columns = columns;
    result.records = filtered;
    result
}

/// Collect every concrete table reference inside a `QueryExpr`.
///
/// Used by view bookkeeping (dependency tracking for materialised
/// invalidation) and any other rewriter that needs to know the base
/// tables a query pulls from. Does not descend into projections/filters;
/// only the `FROM` side.
pub(crate) fn collect_table_refs(expr: &QueryExpr) -> Vec<String> {
    let mut scopes: HashSet<String> = HashSet::new();
    collect_query_expr_result_cache_scopes(&mut scopes, expr);
    scopes.into_iter().collect()
}

fn query_expr_result_cache_scopes(expr: &QueryExpr) -> HashSet<String> {
    let mut scopes = HashSet::new();
    collect_query_expr_result_cache_scopes(&mut scopes, expr);
    scopes
}

/// Heuristic: does the raw SQL reference a built-in whose output
/// varies by connection, clock, or randomness? Such queries must
/// skip the 30s result cache — see the call site for rationale.
///
/// ASCII case-insensitive substring match. False positives (the
/// token appears in a quoted string) only skip caching, which is
/// the conservative direction.
/// If `sql` starts with `EXPLAIN` followed by a non-`ALTER` token,
/// return the trimmed inner statement; otherwise `None`.
///
/// `EXPLAIN ALTER FOR CREATE TABLE ...` is a separate schema-diff
/// command handled inside the normal SQL parser, so we leave it
/// alone here.
fn strip_explain_prefix(sql: &str) -> Option<&str> {
    let trimmed = sql.trim_start();
    let (head, rest) = trimmed.split_at(
        trimmed
            .find(|c: char| c.is_whitespace())
            .unwrap_or(trimmed.len()),
    );
    if !head.eq_ignore_ascii_case("EXPLAIN") {
        return None;
    }
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }
    // Peek the next token — if ALTER or ASK, defer to the normal parser.
    // `EXPLAIN ASK` is an executable read path: it runs retrieval and
    // provider selection, then short-circuits before the LLM call.
    let next_head_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    if rest[..next_head_end].eq_ignore_ascii_case("ALTER")
        || rest[..next_head_end].eq_ignore_ascii_case("ASK")
    {
        return None;
    }
    Some(rest)
}

/// Cheap prefix check for a leading `WITH` keyword. Used to gate the
/// CTE-aware parse in `execute_query` without paying for a full
/// lexer pass on every statement. Treats `WITHIN` as not-a-CTE so
/// `WITHIN TENANT '...' SELECT ...` doesn't mis-route.
pub(super) fn has_with_prefix(sql: &str) -> bool {
    let trimmed = sql.trim_start();
    let head_end = trimmed
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(trimmed.len());
    trimmed[..head_end].eq_ignore_ascii_case("WITH")
}

/// If the query is a plain SELECT whose top-level `TableQuery`
/// carries an `AS OF` clause, return a typed spec that the runtime
/// can feed to `vcs_resolve_as_of`. Returns `None` for any other
/// shape — joins, DML, EXPLAIN, or parse failures — so callers fall
/// back to the connection's regular MVCC snapshot. A cheap textual
/// prefilter skips the parse entirely when the source doesn't
/// mention `AS OF` / `as of`, keeping the autocommit hot path free.
fn peek_top_level_as_of(sql: &str) -> Option<crate::application::vcs::AsOfSpec> {
    peek_top_level_as_of_with_table(sql).map(|(spec, _)| spec)
}

/// Same as `peek_top_level_as_of` but also returns the table name
/// targeted by the AS OF clause (when the FROM clause names a
/// concrete table). `None` for the table slot means scalar SELECT
/// or a subquery source — callers treat those as "no enforcement".
pub(super) fn peek_top_level_as_of_with_table(
    sql: &str,
) -> Option<(crate::application::vcs::AsOfSpec, Option<String>)> {
    if !sql
        .as_bytes()
        .windows(5)
        .any(|w| w.eq_ignore_ascii_case(b"as of"))
    {
        return None;
    }
    let parsed = crate::storage::query::parser::parse(sql).ok()?;
    let crate::storage::query::ast::QueryExpr::Table(table) = parsed.query else {
        return None;
    };
    let clause = table.as_of?;
    let table_name = if table.table.is_empty() || table.table == "any" {
        None
    } else {
        Some(table.table.clone())
    };
    let spec = match clause {
        crate::storage::query::ast::AsOfClause::Commit(h) => {
            crate::application::vcs::AsOfSpec::Commit(h)
        }
        crate::storage::query::ast::AsOfClause::Branch(b) => {
            crate::application::vcs::AsOfSpec::Branch(b)
        }
        crate::storage::query::ast::AsOfClause::Tag(t) => crate::application::vcs::AsOfSpec::Tag(t),
        crate::storage::query::ast::AsOfClause::TimestampMs(ts) => {
            crate::application::vcs::AsOfSpec::TimestampMs(ts)
        }
        crate::storage::query::ast::AsOfClause::Snapshot(x) => {
            crate::application::vcs::AsOfSpec::Snapshot(x)
        }
    };
    Some((spec, table_name))
}

pub(super) fn query_has_volatile_builtin(sql: &str) -> bool {
    // Lowercase the bytes up to the first null/newline into a small
    // stack buffer for cheap contains() checks. Most SQL fits in the
    // buffer; longer queries fall back to owned lowercase.
    const VOLATILE_TOKENS: &[&str] = &[
        "pg_advisory_lock",
        "pg_try_advisory_lock",
        "pg_advisory_unlock",
        "random()",
        // `$config.<path>` / `$secret.<path>` resolve mutable runtime config /
        // vault state at execution time (#1370). A cached result would serve a
        // stale value after a later `SET CONFIG` / `SET SECRET`, so treat any
        // query referencing them as volatile (never result-cache it).
        "$config",
        "$secret",
        // NOW() / CURRENT_TIMESTAMP / CURRENT_DATE intentionally
        // omitted for now — they ARE volatile but today's tests rely
        // on caching them. Revisit once a tighter volatility story
        // lands.
    ];
    let lowered = sql.to_ascii_lowercase();
    VOLATILE_TOKENS.iter().any(|t| lowered.contains(t))
}

pub(super) fn query_is_ask_statement(sql: &str) -> bool {
    let trimmed = sql.trim_start();
    let head_end = trimmed
        .find(|c: char| c.is_whitespace() || c == '(' || c == ';')
        .unwrap_or(trimmed.len());
    trimmed[..head_end].eq_ignore_ascii_case("ASK")
}

/// Pick the `(global_mode, collection_mode)` pair for an expression,
/// or `None` for variants that opt out of intent-locking entirely
/// (admin statements like `SHOW CONFIG`, transaction control, tenant
/// toggles).
///
/// Phase-1 contract:
/// - Reads  — `(IX-compatible) (Global, IS) → (Collection, IS)`
/// - Writes — `(IX-compatible) (Global, IX) → (Collection, IX)`
/// - DDL    — `(strong)        (Global, IX) → (Collection, X)`
pub(super) fn intent_lock_modes_for(
    expr: &QueryExpr,
) -> Option<(
    crate::storage::transaction::lock::LockMode,
    crate::storage::transaction::lock::LockMode,
)> {
    use crate::storage::transaction::lock::LockMode::{Exclusive, IntentExclusive, IntentShared};

    match expr {
        // Reads — IS / IS.
        QueryExpr::Table(_)
        | QueryExpr::Join(_)
        | QueryExpr::Vector(_)
        | QueryExpr::Hybrid(_)
        | QueryExpr::Graph(_)
        | QueryExpr::Path(_)
        | QueryExpr::Ask(_)
        | QueryExpr::SearchCommand(_)
        | QueryExpr::GraphCommand(_)
        | QueryExpr::RankOf(_)
        | QueryExpr::ApproxRankOf(_)
        | QueryExpr::RankRange(_)
        | QueryExpr::QueueSelect(_) => Some((IntentShared, IntentShared)),

        // Writes — IX / IX. Non-tabular mutations (vector insert,
        // graph node insert, queue push, timeseries point insert)
        // don't carry their own dispatch arm here; they ride through
        // the Insert variant or a command variant covered by the
        // read-side arm above. P1.T4 expands only the TableQuery-ish
        // writes; non-tabular kinds inherit when their DML variants
        // land in later phases.
        QueryExpr::Insert(_)
        | QueryExpr::Update(_)
        | QueryExpr::Delete(_)
        | QueryExpr::QueueCommand(QueueCommand::Move { .. }) => {
            Some((IntentExclusive, IntentExclusive))
        }
        QueryExpr::QueueCommand(_) => Some((IntentShared, IntentShared)),

        // DDL — IX / X. A DDL against collection `c` blocks all
        // other writers + readers on `c` but leaves other collections
        // running (because Global stays IX, not X).
        QueryExpr::CreateTable(_)
        | QueryExpr::CreateCollection(_)
        | QueryExpr::CreateVector(_)
        | QueryExpr::DropTable(_)
        | QueryExpr::DropGraph(_)
        | QueryExpr::DropVector(_)
        | QueryExpr::DropDocument(_)
        | QueryExpr::DropKv(_)
        | QueryExpr::DropCollection(_)
        | QueryExpr::Truncate(_)
        | QueryExpr::AlterTable(_)
        | QueryExpr::CreateIndex(_)
        | QueryExpr::DropIndex(_)
        | QueryExpr::CreateTimeSeries(_)
        | QueryExpr::CreateMetric(_)
        | QueryExpr::AlterMetric(_)
        | QueryExpr::CreateSlo(_)
        | QueryExpr::DropTimeSeries(_)
        | QueryExpr::CreateQueue(_)
        | QueryExpr::AlterQueue(_)
        | QueryExpr::DropQueue(_)
        | QueryExpr::CreateTree(_)
        | QueryExpr::DropTree(_)
        | QueryExpr::CreatePolicy(_)
        | QueryExpr::DropPolicy(_)
        | QueryExpr::CreateView(_)
        | QueryExpr::DropView(_)
        | QueryExpr::RefreshMaterializedView(_)
        | QueryExpr::CreateSchema(_)
        | QueryExpr::DropSchema(_)
        | QueryExpr::CreateSequence(_)
        | QueryExpr::DropSequence(_)
        | QueryExpr::CreateServer(_)
        | QueryExpr::DropServer(_)
        | QueryExpr::CreateForeignTable(_)
        | QueryExpr::DropForeignTable(_) => Some((IntentExclusive, Exclusive)),

        // Admin / control — skip intent locks. `SET TENANT`,
        // `BEGIN / COMMIT / ROLLBACK`, `SET CONFIG`, `SHOW CONFIG`,
        // `VACUUM`, etc. don't touch collection data the same way
        // and the existing transaction layer already serialises the
        // pieces that matter.
        _ => None,
    }
}

/// Best-effort collection inventory for an expression. Used to pick
/// `Collection(...)` resources for the intent-lock guard. Overshoots
/// are fine (take an extra IS, benign); undershoots leak writes past
/// DDL X locks, so err on the side of listing more names.
pub(super) fn collections_referenced(expr: &QueryExpr) -> Vec<String> {
    let mut out = Vec::new();
    walk_collections(expr, &mut out);
    out.sort();
    out.dedup();
    out
}

fn walk_collections(expr: &QueryExpr, out: &mut Vec<String>) {
    match expr {
        QueryExpr::Table(t) => out.push(t.table.clone()),
        QueryExpr::Join(j) => {
            walk_collections(&j.left, out);
            walk_collections(&j.right, out);
        }
        QueryExpr::Insert(i) => out.push(i.table.clone()),
        QueryExpr::Update(u) => out.push(u.table.clone()),
        QueryExpr::Delete(d) => out.push(d.table.clone()),
        QueryExpr::QueueSelect(q) => out.push(q.queue.clone()),

        // DDL — include the target collection so DDL takes
        // `(Collection, X)` and blocks concurrent readers / writers
        // on the same collection. Other collections stay live
        // because Global is still IX.
        QueryExpr::CreateTable(q) => out.push(q.name.clone()),
        QueryExpr::CreateCollection(q) => out.push(q.name.clone()),
        QueryExpr::CreateVector(q) => out.push(q.name.clone()),
        QueryExpr::DropTable(q) => out.push(q.name.clone()),
        QueryExpr::DropGraph(q) => out.push(q.name.clone()),
        QueryExpr::DropVector(q) => out.push(q.name.clone()),
        QueryExpr::DropDocument(q) => out.push(q.name.clone()),
        QueryExpr::DropKv(q) => out.push(q.name.clone()),
        QueryExpr::DropCollection(q) => out.push(q.name.clone()),
        QueryExpr::Truncate(q) => out.push(q.name.clone()),
        QueryExpr::AlterTable(q) => out.push(q.name.clone()),
        QueryExpr::CreateIndex(q) => out.push(q.table.clone()),
        QueryExpr::DropIndex(q) => out.push(q.table.clone()),
        QueryExpr::CreateTimeSeries(q) => out.push(q.name.clone()),
        QueryExpr::CreateMetric(q) => out.push(q.path.clone()),
        QueryExpr::AlterMetric(q) => out.push(q.path.clone()),
        QueryExpr::CreateSlo(q) => out.push(q.path.clone()),
        QueryExpr::DropTimeSeries(q) => out.push(q.name.clone()),
        QueryExpr::CreateQueue(q) => out.push(q.name.clone()),
        QueryExpr::AlterQueue(q) => out.push(q.name.clone()),
        QueryExpr::DropQueue(q) => out.push(q.name.clone()),
        QueryExpr::QueueCommand(QueueCommand::Move {
            source,
            destination,
            ..
        }) => {
            out.push(source.clone());
            out.push(destination.clone());
        }
        QueryExpr::CreatePolicy(q) => out.push(q.table.clone()),
        QueryExpr::CreateView(q) => out.push(q.name.clone()),
        QueryExpr::DropView(q) => out.push(q.name.clone()),
        QueryExpr::RefreshMaterializedView(q) => out.push(q.name.clone()),

        // Vector / Hybrid / Graph / Path / commands reference
        // collections through fields whose shape varies; without a
        // uniform accessor we fall back to the global lock only —
        // benign because every runtime path still holds the global
        // mode.
        _ => {}
    }
}

impl RedDBRuntime {
    pub fn in_memory() -> RedDBResult<Self> {
        Self::with_options(RedDBOptions::in_memory())
    }

    pub fn flush(&self) -> RedDBResult<()> {
        self.inner
            .db
            .flush()
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    /// Handle to the intent-lock manager for tests + introspection.
    /// Production code acquires via `LockerGuard::new(rt.lock_manager())`
    /// rather than touching the manager directly.
    pub fn lock_manager(&self) -> std::sync::Arc<crate::storage::transaction::lock::LockManager> {
        self.inner.lock_manager.clone()
    }

    /// Process-local governance registry for managed policy/config guardrails.
    pub fn config_registry(&self) -> std::sync::Arc<crate::auth::registry::ConfigRegistry> {
        self.inner.config_registry.clone()
    }

    pub fn query_audit(&self) -> std::sync::Arc<crate::runtime::query_audit::QueryAuditStream> {
        self.inner.query_audit.clone()
    }

    pub fn control_events_require_persistence(&self) -> bool {
        self.inner.control_event_config.require_persistence()
    }

    pub fn control_event_config(&self) -> crate::runtime::control_events::ControlEventConfig {
        self.inner.control_event_config
    }

    pub fn control_event_ledger(
        &self,
    ) -> Arc<dyn crate::runtime::control_events::ControlEventLedger> {
        self.inner.control_event_ledger.read().clone()
    }

    #[doc(hidden)]
    pub fn replace_control_event_ledger_for_tests(
        &self,
        ledger: Arc<dyn crate::runtime::control_events::ControlEventLedger>,
    ) {
        *self.inner.control_event_ledger.write() = ledger;
    }

    #[inline(never)]
    pub fn with_options(options: RedDBOptions) -> RedDBResult<Self> {
        Self::with_pool(options, ConnectionPoolConfig::default())
    }

    pub fn with_pool(
        options: RedDBOptions,
        pool_config: ConnectionPoolConfig,
    ) -> RedDBResult<Self> {
        // PLAN.md Phase 9.1 — capture wall-clock before storage
        // open so the cold-start phase markers can be backfilled
        // once Lifecycle is constructed below. Storage open
        // encapsulates auto-restore + WAL replay; we treat the
        // whole window as one combined "restore" + "wal_replay"
        // phase split at the same boundary because the storage
        // layer doesn't yet emit a finer signal.
        let boot_open_start_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let embedded_single_file = options.storage_profile.deploy_profile
            == crate::storage::DeployProfile::Embedded
            && options.storage_profile.packaging == crate::storage::StoragePackaging::SingleFile;
        let db = Arc::new(
            RedDB::open_with_options(&options)
                .map_err(|err| RedDBError::Internal(err.to_string()))?,
        );
        let result_blob_cache_config = if embedded_single_file {
            crate::storage::cache::BlobCacheConfig::default()
        } else {
            crate::storage::cache::BlobCacheConfig::default().with_l2_path(
                reddb_file::layout::result_cache_l2_path(
                    &options.resolved_path(reddb_file::default_database_path()),
                ),
            )
        };
        let result_blob_cache =
            crate::storage::cache::BlobCache::open_with_l2(result_blob_cache_config).map_err(
                |err| RedDBError::Internal(format!("open result Blob Cache L2 failed: {err:?}")),
            )?;
        let storage_ready_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let runtime = Self {
            inner: Arc::new(RuntimeInner {
                db: db.clone(),
                layout: PhysicalLayout::from_options(&options),
                embedded_single_file,
                indices: IndexCatalog::register_default_vector_graph(
                    options.has_capability(crate::api::Capability::Table),
                    options.has_capability(crate::api::Capability::Graph),
                ),
                pool_config,
                pool: Mutex::new(PoolState::default()),
                started_at_unix_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
                probabilistic: super::probabilistic_store::ProbabilisticStore::new(),
                index_store: super::index_store::IndexStore::new(),
                cdc: crate::replication::cdc::CdcBuffer::new(100_000),
                backup_scheduler: crate::replication::scheduler::BackupScheduler::new(3600),
                query_cache: parking_lot::RwLock::new(
                    crate::storage::query::planner::cache::PlanCache::new(1000),
                ),
                result_cache: parking_lot::RwLock::new((
                    HashMap::new(),
                    std::collections::VecDeque::new(),
                )),
                result_blob_cache,
                result_blob_entries: parking_lot::RwLock::new((
                    HashMap::new(),
                    std::collections::VecDeque::new(),
                )),
                ask_answer_cache_entries: parking_lot::RwLock::new((
                    HashSet::new(),
                    std::collections::VecDeque::new(),
                )),
                result_cache_shadow_divergences: std::sync::atomic::AtomicU64::new(0),
                result_cache_hits: std::sync::atomic::AtomicU64::new(0),
                result_cache_misses: std::sync::atomic::AtomicU64::new(0),
                result_cache_evictions: std::sync::atomic::AtomicU64::new(0),
                ask_daily_spend: parking_lot::RwLock::new(HashMap::new()),
                queue_message_locks: parking_lot::RwLock::new(HashMap::new()),
                rmw_locks: RmwLockTable::new(),
                planner_dirty_tables: parking_lot::RwLock::new(HashSet::new()),
                ec_registry: Arc::new(crate::ec::config::EcRegistry::new()),
                config_registry: Arc::new(crate::auth::registry::ConfigRegistry::new()),
                ec_worker: crate::ec::worker::EcWorker::new(),
                auth_store: parking_lot::RwLock::new(None),
                oauth_validator: parking_lot::RwLock::new(None),
                browser_token_authority: parking_lot::RwLock::new(None),
                views: parking_lot::RwLock::new(HashMap::new()),
                materialized_views: parking_lot::RwLock::new(
                    crate::storage::cache::result::MaterializedViewCache::new(),
                ),
                retention_sweeper: parking_lot::RwLock::new(
                    crate::runtime::retention_sweeper::RetentionSweeperState::new(),
                ),
                snapshot_manager: Arc::new(
                    crate::storage::transaction::snapshot::SnapshotManager::new(),
                ),
                tx_contexts: parking_lot::RwLock::new(HashMap::new()),
                tx_local_tenants: parking_lot::RwLock::new(HashMap::new()),
                env_config_overrides: crate::runtime::config_overlay::collect_env_overrides(),
                lock_manager: Arc::new({
                    // Sourced from the matrix: Tier B key
                    // `concurrency.locking.deadlock_timeout_ms`
                    // (default 5000). Env var wins at boot so
                    // operators can tune without touching red_config.
                    let env = crate::runtime::config_overlay::collect_env_overrides();
                    let timeout_ms = env
                        .get("concurrency.locking.deadlock_timeout_ms")
                        .and_then(|raw| raw.parse::<u64>().ok())
                        .unwrap_or_else(|| {
                            match crate::runtime::config_matrix::default_for(
                                "concurrency.locking.deadlock_timeout_ms",
                            ) {
                                Some(crate::serde_json::Value::Number(n)) => n as u64,
                                _ => 5000,
                            }
                        });
                    let cfg = crate::storage::transaction::lock::LockConfig {
                        default_timeout: std::time::Duration::from_millis(timeout_ms),
                        ..Default::default()
                    };
                    crate::storage::transaction::lock::LockManager::new(cfg)
                }),
                rls_policies: parking_lot::RwLock::new(HashMap::new()),
                rls_enabled_tables: parking_lot::RwLock::new(HashSet::new()),
                foreign_tables: Arc::new(crate::storage::fdw::ForeignTableRegistry::with_builtins()),
                pending_tombstones: parking_lot::RwLock::new(HashMap::new()),
                pending_versioned_updates: parking_lot::RwLock::new(HashMap::new()),
                pending_kv_watch_events: parking_lot::RwLock::new(HashMap::new()),
                pending_store_wal_actions: parking_lot::RwLock::new(HashMap::new()),
                queue_wait_registry: std::sync::Arc::new(
                    crate::runtime::queue_wait_registry::QueueWaitRegistry::new(),
                ),
                pending_queue_wakes: parking_lot::RwLock::new(HashMap::new()),
                tenant_tables: parking_lot::RwLock::new(HashMap::new()),
                ddl_epoch: std::sync::atomic::AtomicU64::new(0),
                write_gate: Arc::new(crate::runtime::write_gate::WriteGate::from_options(
                    &options,
                )),
                lifecycle: crate::runtime::lifecycle::Lifecycle::new(),
                resource_limits: crate::runtime::resource_limits::ResourceLimits::from_env(),
                audit_log: {
                    // Default audit-log path for the in-memory case
                    // sits in the system temp dir; persistent runs
                    // place it next to the resolved data file.
                    //
                    // gh-471 iter 2: route through the resolved
                    // `LogDestination`. Performance/Max tiers emit a
                    // file-backed log destination under the file-owned
                    // support-directory logs tier;
                    // lower tiers / ephemeral runs report `Stderr`
                    // and we keep the legacy file-next-to-data sink.
                    // #1375 — single-file embedded mode keeps the data
                    // directory to exactly the `.rdb` artifact, so the audit
                    // log must NOT land as a sibling. Route it to a
                    // process-unique temp location even when a data path is
                    // set; only the non-embedded case uses the data dir.
                    let data_path = if embedded_single_file {
                        std::env::temp_dir()
                            .join("reddb-embedded-runtime")
                            .join(format!("audit-{}", std::process::id()))
                    } else {
                        options
                            .data_path
                            .clone()
                            .unwrap_or_else(|| std::env::temp_dir().join("reddb"))
                    };
                    if !embedded_single_file
                        && options
                            .metadata
                            .contains_key(crate::api::EPHEMERAL_RUNTIME_METADATA_KEY)
                    {
                        // Ephemeral (in-memory) runtimes all live in the
                        // system temp dir, so the fixed `.audit.log` sibling
                        // that `legacy_audit_log_path` returns collides across
                        // instances. Under nextest's process-per-test model
                        // many ephemeral runtimes run concurrently and truncate
                        // each other's audit log, flaking audit assertions.
                        // Derive the sink from the unique ephemeral data-file
                        // stem so every runtime gets its own file.
                        let audit_path = reddb_file::layout::sibling_path(
                            &data_path,
                            &reddb_file::layout::sidecar_file_name(&data_path, "audit.log"),
                        );
                        Arc::new(crate::runtime::audit_log::AuditLogger::with_path(
                            audit_path,
                        ))
                    } else {
                        let (audit_dest, _) = crate::api::tier_wiring::current_log_destinations();
                        Arc::new(crate::runtime::audit_log::AuditLogger::for_destination(
                            &audit_dest,
                            &data_path,
                        ))
                    }
                },
                control_event_ledger: parking_lot::RwLock::new(Arc::new(
                    crate::runtime::control_events::RuntimeLedger::new(db.store()),
                )),
                control_event_config: options.control_events,
                query_audit: Arc::new(crate::runtime::query_audit::QueryAuditStream::new(
                    db.store(),
                    options.query_audit.clone(),
                )),
                lease_lifecycle: std::sync::OnceLock::new(),
                replica_apply_metrics: std::sync::Arc::new(
                    crate::replication::logical::ReplicaApplyMetrics::default(),
                ),
                quota_bucket: crate::runtime::quota_bucket::QuotaBucket::from_env(),
                schema_vocabulary: parking_lot::RwLock::new(
                    crate::runtime::schema_vocabulary::SchemaVocabulary::new(),
                ),
                slow_query_logger: {
                    // Issue #205 — slow-query sink lives in the same
                    // directory the audit log uses, so backup/restore
                    // ships them together. Threshold + sample-pct
                    // default conservatively (1 s, 100% sampling) so
                    // emitted lines are rare and complete. Operators
                    // tune via env / config matrix in a follow-up.
                    //
                    // gh-471 iter 2: same routing as the audit log —
                    // `LogDestination::File(...)` for Performance/Max
                    // lands under the file-owned support-directory logs tier;
                    // lower tiers fall back to `red-slow.log` in the
                    // data directory.
                    // #1375 — see the audit-log note above: single-file mode
                    // never writes the slow-query log as a sibling of the
                    // `.rdb`. Route to a process-unique temp dir when embedded,
                    // regardless of the data path.
                    let fallback_dir = if embedded_single_file {
                        std::env::temp_dir()
                            .join("reddb-embedded-runtime")
                            .join(format!("slow-{}", std::process::id()))
                    } else {
                        options
                            .data_path
                            .as_ref()
                            .and_then(|p| p.parent().map(std::path::PathBuf::from))
                            .unwrap_or_else(|| std::env::temp_dir().join("reddb"))
                    };
                    let threshold_ms = std::env::var("RED_SLOW_QUERY_THRESHOLD_MS")
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(1000);
                    let sample_pct = std::env::var("RED_SLOW_QUERY_SAMPLE_PCT")
                        .ok()
                        .and_then(|s| s.parse::<u8>().ok())
                        .unwrap_or(100);
                    let (_, slow_dest) = crate::api::tier_wiring::current_log_destinations();
                    crate::telemetry::slow_query_logger::SlowQueryLogger::for_destination(
                        &slow_dest,
                        &fallback_dir,
                        threshold_ms,
                        sample_pct,
                    )
                },
                slow_query_store: crate::telemetry::slow_query_store::SlowQueryStore::new(
                    crate::telemetry::slow_query_store::DEFAULT_CAP,
                ),
                kv_stats: crate::runtime::KvStatsCounters::default(),
                metrics_ingest_stats: crate::runtime::MetricsIngestCounters::default(),
                metrics_tenant_activity_stats:
                    crate::runtime::MetricsTenantActivityCounters::default(),
                queue_telemetry: Arc::new(
                    crate::runtime::queue_telemetry::QueueTelemetryCounters::default(),
                ),
                query_latency_telemetry: Arc::new(
                    crate::runtime::query_latency_telemetry::QueryLatencyTelemetry::default(),
                ),
                queue_presence: Arc::new(
                    crate::storage::queue::presence::ConsumerPresenceRegistry::new(),
                ),
                vector_introspection: Arc::new(
                    crate::storage::vector::introspection::VectorIntrospectionRegistry::new(),
                ),
                kv_tag_index: crate::runtime::KvTagIndex::default(),
                chain_tip_cache: parking_lot::Mutex::new(HashMap::new()),
                chain_integrity_broken: parking_lot::Mutex::new(HashMap::new()),
                integrity_tombstones: parking_lot::Mutex::new(Vec::new()),
                integrity_tombstones_state: std::sync::atomic::AtomicU8::new(0),
            }),
        };

        // Issue #205 — install the process-wide OperatorEvent sink so
        // emit sites buried in storage / replication / signal handlers
        // can record without threading an `&AuditLogger` through every
        // call stack. First registration wins; subsequent in-memory
        // runtimes (test harnesses) fall through to tracing+eprintln.
        crate::telemetry::operator_event::install_global_audit_sink(Arc::clone(
            &runtime.inner.audit_log,
        ));

        // Issue #1238 — wire the slow-query telemetry substrate (ADR 0060).
        // The logger dual-writes: file sink (existing) + ring store (new).
        runtime
            .inner
            .slow_query_logger
            .attach_store(Arc::clone(&runtime.inner.slow_query_store));

        // PLAN.md Phase 9.1 — backfill cold-start phase markers
        // from the wall-clock captured before storage open. The
        // entire `RedDB::open_with_options` call covers both
        // auto-restore (when configured) and WAL replay. We
        // record both phases against the same boundary today;
        // a follow-up will split them once the storage layer
        // surfaces a finer-grained event.
        runtime
            .inner
            .lifecycle
            .set_restore_started_at_ms(boot_open_start_ms);
        runtime
            .inner
            .lifecycle
            .set_restore_ready_at_ms(storage_ready_ms);
        runtime
            .inner
            .lifecycle
            .set_wal_replay_started_at_ms(boot_open_start_ms);
        runtime
            .inner
            .lifecycle
            .set_wal_replay_ready_at_ms(storage_ready_ms);

        let restored_cdc_lsn = runtime
            .inner
            .db
            .replication
            .as_ref()
            .map(|repl| {
                repl.logical_wal_spool
                    .as_ref()
                    .map(|spool| spool.current_lsn())
                    .unwrap_or(0)
            })
            .unwrap_or(0)
            .max(runtime.config_u64("red.config.timeline.last_archived_lsn", 0));
        runtime.inner.cdc.set_current_lsn(restored_cdc_lsn);
        runtime.rehydrate_snapshot_xid_floor();
        runtime
            .bootstrap_system_keyed_collections()
            .map_err(|err| RedDBError::Internal(format!("bootstrap system collections: {err}")))?;
        runtime.rehydrate_declared_column_schemas();
        runtime.rehydrate_runtime_index_registry()?;
        runtime
            .load_probabilistic_state()
            .map_err(|err| RedDBError::Internal(format!("load probabilistic state: {err}")))?;

        // Phase 2.5.4: replay `tenant_tables.{table}.column` markers so
        // tables declared via `TENANT BY (col)` survive restart. Each
        // entry re-registers the auto-policy and flips RLS on again.
        runtime.rehydrate_tenant_tables();
        // Issue #593 slice 9a — replay persisted materialized-view
        // descriptors so `CREATE MATERIALIZED VIEW v AS …` survives a
        // restart. Runs after the system-keyed collections bootstrap
        // and before the API opens.
        runtime.rehydrate_materialized_view_descriptors();
        if let Some(repl) = &runtime.inner.db.replication {
            repl.wal_buffer.set_current_lsn(restored_cdc_lsn);
        }

        // Save system info to red_config on boot
        {
            let sys = SystemInfo::collect();
            runtime.inner.db.store().set_config_tree(
                "red.system",
                &crate::serde_json::json!({
                    "pid": sys.pid,
                    "cpu_cores": sys.cpu_cores,
                    "total_memory_bytes": sys.total_memory_bytes,
                    "available_memory_bytes": sys.available_memory_bytes,
                    "os": sys.os,
                    "arch": sys.arch,
                    "hostname": sys.hostname,
                    "started_at": SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64
                }),
            );

            // Seed defaults on first boot (only if red_config is empty or missing defaults)
            let store = runtime.inner.db.store();
            if store
                .get_collection("red_config")
                .map(|m| m.query_all(|_| true).len())
                .unwrap_or(0)
                <= 10
            {
                store.set_config_tree("red.ai", &crate::json!({
                    "default": crate::json!({
                        "provider": "openai",
                        "model": crate::ai::DEFAULT_OPENAI_PROMPT_MODEL
                    }),
                    "max_embedding_inputs": 256,
                    "max_prompt_batch": 256,
                    "timeout": crate::json!({ "connect_secs": 10, "read_secs": 90, "write_secs": 30 })
                }));
                store.set_config_tree(
                    "red.server",
                    &crate::json!({
                        "max_scan_limit": 1000,
                        "max_body_size": 1048576,
                        "read_timeout_ms": 5000,
                        "write_timeout_ms": 5000
                    }),
                );
                store.set_config_tree(
                    "red.storage",
                    &crate::json!({
                        "page_size": 4096,
                        "page_cache_capacity": 100000,
                        "auto_checkpoint_pages": 1000,
                        "snapshot_retention": 16,
                        "verify_checksums": true,
                        "segment": crate::json!({
                            "max_entities": 100000,
                            "max_bytes": 268435456_u64,
                            "compression_level": 6
                        }),
                        "hnsw": crate::json!({ "m": 16, "ef_construction": 100, "ef_search": 50 }),
                        "ivf": crate::json!({ "n_lists": 100, "n_probes": 10 }),
                        "bm25": crate::json!({ "k1": 1.2, "b": 0.75 })
                    }),
                );
                store.set_config_tree(
                    "red.search",
                    &crate::json!({
                        "rag": crate::json!({
                            "max_chunks_per_source": 10,
                            "max_total_chunks": 25,
                            "similarity_threshold": 0.8,
                            "graph_depth": 2,
                            "min_relevance": 0.3
                        }),
                        "fusion": crate::json!({
                            "vector_weight": 0.5,
                            "graph_weight": 0.3,
                            "table_weight": 0.2,
                            "dedup_threshold": 0.85
                        })
                    }),
                );
                store.set_config_tree(
                    "red.auth",
                    &crate::json!({
                        "enabled": false,
                        "session_ttl_secs": 3600,
                        "require_auth": false
                    }),
                );
                store.set_config_tree(
                    "red.query",
                    &crate::json!({
                        "connection_pool": crate::json!({ "max_connections": 64, "max_idle": 16 }),
                        "max_recursion_depth": 1000
                    }),
                );
                store.set_config_tree(
                    "red.indexes",
                    &crate::json!({
                        "auto_select": true,
                        "bloom_filter": crate::json!({
                            "enabled": true,
                            "false_positive_rate": 0.01,
                            "prune_on_scan": true
                        }),
                        "hash": crate::json!({ "enabled": true }),
                        "bitmap": crate::json!({ "enabled": true, "max_cardinality": 1000 }),
                        "spatial": crate::json!({ "enabled": true })
                    }),
                );
                store.set_config_tree(
                    "red.memtable",
                    &crate::json!({
                        "enabled": true,
                        "max_bytes": 67108864_u64,
                        "flush_threshold": 0.75
                    }),
                );
                store.set_config_tree(
                    "red.probabilistic",
                    &crate::json!({
                        "hll_registers": 16384,
                        "sketch_default_width": 1000,
                        "sketch_default_depth": 5,
                        "filter_default_capacity": 100000
                    }),
                );
                store.set_config_tree(
                    "red.timeseries",
                    &crate::json!({
                        "default_chunk_size": 1024,
                        "compression": crate::json!({
                            "timestamps": "delta_of_delta",
                            "values": "gorilla_xor"
                        }),
                        "default_retention_days": 0
                    }),
                );
                store.set_config_tree(
                    "red.queue",
                    &crate::json!({
                        "default_max_size": 0,
                        "default_max_attempts": 3,
                        "visibility_timeout_ms": 30000,
                        "consumer_idle_timeout_ms": 60000
                    }),
                );
                store.set_config_tree(
                    "red.backup",
                    &crate::json!({
                        "enabled": false,
                        "interval_secs": 3600,
                        "retention_count": 24,
                        "upload": false,
                        "backend": "local"
                    }),
                );
                store.set_config_tree(
                    "red.wal",
                    &crate::json!({
                        "archive": crate::json!({
                            "enabled": false,
                            "retention_hours": 168,
                            "prefix": reddb_file::backup_wal_prefix("")
                        })
                    }),
                );
                store.set_config_tree(
                    "red.cdc",
                    &crate::json!({
                        "enabled": true,
                        "buffer_size": 100000
                    }),
                );
                store.set_config_tree(
                    "red.config.secret",
                    &crate::json!({
                        "auto_encrypt": true,
                        "auto_decrypt": true
                    }),
                );
            }

            // Perf-parity config matrix: heal the Tier A (critical)
            // keys unconditionally on every boot. Idempotent — only
            // writes the default when the key is missing. Keeps
            // `SHOW CONFIG` showing every guarantee the operator has
            // (durability.mode, concurrency.locking.enabled, …) even
            // on long-running datadirs that predate the matrix.
            crate::runtime::config_matrix::heal_critical_keys(store.as_ref());
            seed_storage_deploy_config(store.as_ref(), options.storage_profile);

            // Phase 5 — Lehman-Yao runtime flag. Read the Tier A
            // `storage.btree.lehman_yao` value from the matrix (env
            // > file > red_config > default) and publish it to the
            // storage layer's atomic so the B-tree read / split
            // paths can branch without re-reading the config on
            // every hot-path call.
            let lehman_yao = runtime.config_bool("storage.btree.lehman_yao", true);
            crate::storage::engine::btree::lehman_yao::set_enabled(lehman_yao);
            if lehman_yao {
                tracing::info!(
                    "storage.btree.lehman_yao=true — lock-free concurrent descent enabled"
                );
            }

            // Config file overlay — mounted `/etc/reddb/config.json`
            // (override path via REDDB_CONFIG_FILE). Writes keys with
            // write-if-absent semantics so a later user `SET CONFIG`
            // always wins. Missing file = silent no-op.
            let overlay_path = crate::runtime::config_overlay::config_file_path();
            let _ =
                crate::runtime::config_overlay::apply_config_file(store.as_ref(), &overlay_path);
        }

        // VCS ("Git for Data") — create the `red_*` metadata
        // collections on first boot. Idempotent: `get_or_create_collection`
        // is a no-op if the collection already exists.
        {
            let store = runtime.inner.db.store();
            for name in crate::application::vcs_collections::ALL {
                let _ = store.get_or_create_collection(*name);
            }
            // Seed VCS config namespace with sensible defaults on first
            // boot, matching the pattern used by red.ai / red.storage.
            store.set_config_tree(
                crate::application::vcs_collections::CONFIG_NAMESPACE,
                &crate::json!({
                    "default_branch": "main",
                    "author": crate::json!({
                        "name": "reddb",
                        "email": "reddb@localhost"
                    }),
                    "protected_branches": crate::json!(["main"]),
                    "closure": crate::json!({
                        "enabled": true,
                        "lazy": true
                    }),
                    "merge": crate::json!({
                        "default_strategy": "auto",
                        "fast_forward": true
                    })
                }),
            );
        }

        // Migrations — create the `red_migrations` / `red_migration_deps`
        // system collections on first boot. Idempotent.
        {
            let store = runtime.inner.db.store();
            for name in crate::application::migration_collections::ALL {
                let _ = store.get_or_create_collection(*name);
            }
        }

        // Topology graph (#803) — ensure the built-in `red.topology.cluster`
        // graph collection (declared WITH ANALYTICS) and its metadata sidecar
        // exist. Idempotent and survives restarts via the WAL-backed contract.
        let _ = crate::application::topology_collections::ensure(&runtime);

        // #1369 — reserve a fixed internal-id floor so the first user-inserted
        // entity always receives a stable, documented `rid` (FIRST_USER_ENTITY_ID),
        // independent of how many internal collection-descriptor / config-default
        // entities the boot sequence seeded above. `register_entity_id` only ever
        // raises the allocator, so a database that already holds user data
        // (counter past the floor) is untouched; a freshly-seeded database jumps
        // straight to the floor.
        runtime
            .inner
            .db
            .store()
            .register_entity_id(crate::storage::EntityId::new(
                crate::storage::FIRST_USER_ENTITY_ID - 1,
            ));

        // Start background maintenance thread (context index refresh +
        // session purge). Held by a WEAK reference to `RuntimeInner`
        // so dropping the last `RedDBRuntime` handle actually releases
        // the underlying Arc<Pager> (and its file lock). Polling at
        // 200ms means shutdown latency is bounded; the real 60-second
        // work cadence is tracked independently via a `last_work`
        // timestamp.
        //
        // The previous version captured `rt = runtime.clone()` by
        // strong reference and ran an unterminated `loop`, which held
        // Arc<RuntimeInner> forever — reopening a persistent database
        // in the same process failed with "Database is locked" because
        // the pager could never drop. See the regression test
        // `finding_1_select_after_bulk_insert_persistent_reopen`.
        {
            let weak = Arc::downgrade(&runtime.inner);
            std::thread::Builder::new()
                .name("reddb-maintenance".into())
                .spawn(move || {
                    let tick = std::time::Duration::from_millis(200);
                    let work_interval = std::time::Duration::from_secs(60);
                    let mut last_work = std::time::Instant::now();
                    loop {
                        std::thread::sleep(tick);
                        let Some(inner) = weak.upgrade() else {
                            // All strong references dropped — the
                            // runtime is gone, exit cleanly.
                            break;
                        };
                        if last_work.elapsed() >= work_interval {
                            let _stats = inner.db.store().context_index().stats();
                            last_work = std::time::Instant::now();
                        }
                    }
                })
                .ok();
        }

        // Start backup scheduler if enabled via red_config
        {
            let store = runtime.inner.db.store();
            let mut backup_enabled = false;
            let mut backup_interval = 3600u64;

            if let Some(manager) = store.get_collection("red_config") {
                manager.for_each_entity(|entity| {
                    if let Some(row) = entity.data.as_row() {
                        let key = row.get_field("key").and_then(|v| match v {
                            crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                            _ => None,
                        });
                        let val = row.get_field("value");
                        if key == Some("red.config.backup.enabled") {
                            backup_enabled = match val {
                                Some(crate::storage::schema::Value::Boolean(true)) => true,
                                Some(crate::storage::schema::Value::Text(s)) => &**s == "true",
                                _ => false,
                            };
                        } else if key == Some("red.config.backup.interval_secs") {
                            if let Some(crate::storage::schema::Value::Integer(n)) = val {
                                backup_interval = *n as u64;
                            }
                        }
                    }
                    true
                });
            }

            if backup_enabled {
                runtime.inner.backup_scheduler.set_interval(backup_interval);
                let rt = runtime.clone();
                runtime
                    .inner
                    .backup_scheduler
                    .start(move || rt.trigger_backup().map_err(|e| format!("{}", e)));
            }
        }

        // Load EC registry from red_config and start worker
        {
            runtime
                .inner
                .ec_registry
                .load_from_config_store(runtime.inner.db.store().as_ref());
            if !runtime.inner.ec_registry.async_configs().is_empty() {
                runtime.inner.ec_worker.start(
                    Arc::clone(&runtime.inner.ec_registry),
                    Arc::clone(&runtime.inner.db.store()),
                );
            }
        }

        if let crate::replication::ReplicationRole::Replica { primary_addr } =
            runtime.inner.db.options().replication.role.clone()
        {
            let rt = runtime.clone();
            std::thread::Builder::new()
                .name("reddb-replica".into())
                .spawn(move || rt.run_replica_loop(primary_addr))
                .ok();
        }

        // PLAN.md Phase 1 — Lifecycle Contract. Mark Ready once every
        // boot stage above has completed (WAL replay, restore-from-
        // remote, replica-loop spawn). Health probes flip from 503 to
        // 200 here; shutdown begins from this state.
        runtime.inner.lifecycle.mark_ready();

        // Issue #583 slice 10 — ContinuousMaterializedView scheduler.
        // Low-priority background ticker that drains the cache's
        // `claim_due_at` set every ~50ms. Holds only a Weak<RuntimeInner>
        // so the thread exits cleanly when the runtime drops (≤50ms
        // latency between drop and exit). Materialized views without
        // a `REFRESH EVERY` clause stay on the manual-refresh path
        // and are skipped by `claim_due_at`, so the loop is a no-op
        // when no scheduled views exist.
        {
            let weak_inner = Arc::downgrade(&runtime.inner);
            std::thread::Builder::new()
                .name("reddb-mv-scheduler".into())
                .spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    let Some(inner) = weak_inner.upgrade() else {
                        break;
                    };
                    let rt = RedDBRuntime { inner };
                    rt.refresh_due_materialized_views();
                })
                .ok();
        }

        // Issue #584 slice 12 — DeclarativeRetention background sweeper.
        // Low-priority ticker that physically reclaims rows whose
        // timestamp has fallen beyond the retention window. Holds a
        // `Weak<RuntimeInner>` so the thread exits within one tick of
        // the runtime drop (graceful shutdown leaves storage consistent
        // because each tick goes through the standard DELETE path —
        // there is no half-finished mutation state to clean up). The
        // tick interval is intentionally longer than the MV scheduler
        // (500ms) because retention is order-of-seconds at minimum.
        if !runtime.write_gate().is_read_only() {
            let weak_inner = Arc::downgrade(&runtime.inner);
            std::thread::Builder::new()
                .name("reddb-retention-sweeper".into())
                .spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    let Some(inner) = weak_inner.upgrade() else {
                        break;
                    };
                    let rt = RedDBRuntime { inner };
                    rt.sweep_retention_tick(
                        crate::runtime::retention_sweeper::DEFAULT_SWEEPER_BATCH,
                    );
                })
                .ok();
        }

        Ok(runtime)
    }

    fn rehydrate_snapshot_xid_floor(&self) {
        let store = self.inner.db.store();
        for collection in store.list_collections() {
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            for entity in manager.query_all(|_| true) {
                self.inner
                    .snapshot_manager
                    .observe_committed_xid(entity.xmin);
                self.inner
                    .snapshot_manager
                    .observe_committed_xid(entity.xmax);
            }
        }
    }

    /// Provision an empty Table-shaped collection that backs a
    /// `CREATE MATERIALIZED VIEW v` (issue #594 slice 9b of #575).
    /// `SELECT FROM v` reads this collection directly; the rewriter is
    /// configured to skip materialized views so the body is no longer
    /// substituted. REFRESH still writes to the cache slot — wiring it
    /// into this backing collection is the job of slice 9c.
    ///
    /// Idempotent: re-running for the same name leaves the existing
    /// collection in place (mirrors `CREATE TABLE IF NOT EXISTS`
    /// semantics). This keeps `CREATE OR REPLACE MATERIALIZED VIEW v`
    /// cheap — the body change does not invalidate already-buffered
    /// rows. Until 9c lands the backing is always empty anyway.
    pub(crate) fn ensure_materialized_view_backing(&self, name: &str) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let mut changed = false;
        if store.get_collection(name).is_none() {
            store.get_or_create_collection(name);
            changed = true;
        }
        if self.inner.db.collection_contract(name).is_none() {
            self.inner
                .db
                .save_collection_contract(system_keyed_collection_contract(
                    name,
                    crate::catalog::CollectionModel::Table,
                ))
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            changed = true;
        }
        if changed {
            self.inner
                .db
                .persist_metadata()
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        Ok(())
    }

    /// Inverse of [`ensure_materialized_view_backing`] — drops the
    /// backing collection on `DROP MATERIALIZED VIEW v`. No-op when
    /// the collection was never created (e.g. a `DROP MATERIALIZED
    /// VIEW IF EXISTS v` against an unknown name).
    pub(crate) fn drop_materialized_view_backing(&self, name: &str) -> RedDBResult<()> {
        let store = self.inner.db.store();
        if store.get_collection(name).is_none() {
            return Ok(());
        }
        store
            .drop_collection(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        // The contract may have been dropped already (DROP TABLE path)
        // — ignore "not found" errors by checking presence first.
        if self.inner.db.collection_contract(name).is_some() {
            self.inner
                .db
                .remove_collection_contract(name)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        self.invalidate_result_cache();
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        Ok(())
    }

    fn bootstrap_system_keyed_collections(&self) -> RedDBResult<()> {
        let mut changed = false;
        for (name, model) in [
            ("red.config", crate::catalog::CollectionModel::Config),
            ("red.vault", crate::catalog::CollectionModel::Vault),
            // Issue #593 — materialized-view catalog. One row per
            // `CREATE MATERIALIZED VIEW`; rehydrated at boot before
            // the API opens.
            (
                crate::runtime::continuous_materialized_view::CATALOG_COLLECTION,
                crate::catalog::CollectionModel::Config,
            ),
        ] {
            if self.inner.db.store().get_collection(name).is_none() {
                self.inner.db.store().get_or_create_collection(name);
                changed = true;
            }
            if self.inner.db.collection_contract(name).is_none() {
                self.inner
                    .db
                    .save_collection_contract(system_keyed_collection_contract(name, model))
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
                changed = true;
            }
        }
        if changed {
            self.inner
                .db
                .persist_metadata()
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        Ok(())
    }

    pub fn db(&self) -> Arc<RedDB> {
        Arc::clone(&self.inner.db)
    }

    /// Direct access to the runtime's secondary-index store.
    /// Used by bulk-insert entry points (gRPC binary bulk, HTTP bulk,
    /// wire bulk) that need to push new rows through the per-index
    /// maintenance hook after `store.bulk_insert` returns.
    pub fn index_store_ref(&self) -> &super::index_store::IndexStore {
        &self.inner.index_store
    }

    /// Apply a DDL event to the schema-vocabulary reverse index
    /// (issue #120). Called by DDL execution paths after the catalog
    /// mutation has succeeded so the index never holds entries for
    /// half-applied DDL.
    pub(crate) fn schema_vocabulary_apply(
        &self,
        event: crate::runtime::schema_vocabulary::DdlEvent,
    ) {
        self.inner.schema_vocabulary.write().on_ddl(event);
    }

    /// Lookup `token` in the schema-vocabulary reverse index. Returns
    /// an owned `Vec<VocabHit>` because the underlying read lock
    /// cannot be borrowed across the call boundary; the slice from
    /// `SchemaVocabulary::lookup` is cloned per hit.
    pub fn schema_vocabulary_lookup(
        &self,
        token: &str,
    ) -> Vec<crate::runtime::schema_vocabulary::VocabHit> {
        self.inner.schema_vocabulary.read().lookup(token).to_vec()
    }

    /// Inject an AuthStore into the runtime. Called by server boot
    /// after the vault has been bootstrapped, so that `Value::Secret`
    /// auto-encrypt/decrypt can reach the vault AES key.
    pub fn set_auth_store(&self, store: Arc<crate::auth::store::AuthStore>) {
        *self.inner.auth_store.write() = Some(store);
    }

    /// Snapshot the current AuthStore (if any). Used by the wire listener
    /// to validate bearer tokens issued via HTTP `/auth/login`.
    pub fn auth_store(&self) -> Option<Arc<crate::auth::store::AuthStore>> {
        self.inner.auth_store.read().clone()
    }

    /// Read a vault KV secret from the configured AuthStore, if present.
    pub fn vault_kv_get(&self, key: &str) -> Option<String> {
        self.inner
            .auth_store
            .read()
            .as_ref()
            .and_then(|store| store.vault_kv_get(key))
    }

    /// Write a vault KV secret and fail if the encrypted vault write is
    /// unavailable or cannot be made durable.
    pub fn vault_kv_try_set(&self, key: String, value: String) -> RedDBResult<()> {
        let store = self.inner.auth_store.read().clone().ok_or_else(|| {
            RedDBError::Query("secret storage requires an enabled, unsealed vault".to_string())
        })?;
        store
            .vault_kv_try_set(key, value)
            .map_err(|err| RedDBError::Query(err.to_string()))
    }

    /// Inject an `OAuthValidator` into the runtime. When set, HTTP and
    /// wire transports try OAuth JWT validation before falling back to
    /// the local AuthStore lookup. Pass `None` to disable.
    pub fn set_oauth_validator(&self, validator: Option<Arc<crate::auth::oauth::OAuthValidator>>) {
        *self.inner.oauth_validator.write() = validator;
    }

    /// Returns a clone of the configured `OAuthValidator` Arc, if any.
    /// Hot path: called per HTTP request when an Authorization header
    /// is present, so we hand back a cheap Arc clone.
    pub fn oauth_validator(&self) -> Option<Arc<crate::auth::oauth::OAuthValidator>> {
        self.inner.oauth_validator.read().clone()
    }

    /// Inject the browser-token authority (issue #936). When set, the
    /// RedWire WS handshake accepts the short-lived access JWT it mints
    /// (alongside, and tried before, the federated OAuth validator), and
    /// the `/auth/browser/*` HTTP endpoints can issue/rotate the pair.
    /// `None` leaves the browser credential flow inert.
    pub fn set_browser_token_authority(
        &self,
        authority: Option<Arc<crate::auth::browser_token::BrowserTokenAuthority>>,
    ) {
        *self.inner.browser_token_authority.write() = authority;
    }

    /// Snapshot the browser-token authority, if wired. Read on the WS
    /// handshake path and by the `/auth/browser/*` handlers; a cheap Arc
    /// clone keeps the lock hold short.
    pub fn browser_token_authority(
        &self,
    ) -> Option<Arc<crate::auth::browser_token::BrowserTokenAuthority>> {
        self.inner.browser_token_authority.read().clone()
    }

    /// Returns the vault AES key (`red.secret.aes_key`) if an auth
    /// store is wired and a key has been generated. Used by the
    /// `Value::Secret` encrypt/decrypt pipeline.
    pub(crate) fn secret_aes_key(&self) -> Option<[u8; 32]> {
        let guard = self.inner.auth_store.read();
        guard.as_ref().and_then(|s| s.vault_secret_key())
    }

    /// Resolve a boolean flag from `red_config`. Defaults to `default`
    /// when the key is missing or not coercible. If the same key has
    /// been written multiple times (SET CONFIG appends new rows), the
    /// most recent entity wins. Env-var overrides
    /// (`REDDB_<UP_DOTTED>`) take highest precedence.
    pub(crate) fn config_bool(&self, key: &str, default: bool) -> bool {
        if let Some(raw) = self.inner.env_config_overrides.get(key) {
            if let Some(crate::storage::schema::Value::Boolean(b)) =
                crate::runtime::config_overlay::coerce_env_value(key, raw)
            {
                return b;
            }
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return default;
        };
        let mut result = default;
        let mut latest_id: u64 = 0;
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let entry_key = row.get_field("key").and_then(|v| match v {
                    crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        result = match row.get_field("value") {
                            Some(crate::storage::schema::Value::Boolean(b)) => *b,
                            Some(crate::storage::schema::Value::Text(s)) => {
                                matches!(s.as_ref(), "true" | "TRUE" | "True" | "1")
                            }
                            Some(crate::storage::schema::Value::Integer(n)) => *n != 0,
                            _ => default,
                        };
                    }
                }
            }
            true
        });
        result
    }

    pub(crate) fn config_u64(&self, key: &str, default: u64) -> u64 {
        if let Some(raw) = self.inner.env_config_overrides.get(key) {
            if let Some(crate::storage::schema::Value::UnsignedInteger(n)) =
                crate::runtime::config_overlay::coerce_env_value(key, raw)
            {
                return n;
            }
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return default;
        };
        let mut result = default;
        let mut latest_id: u64 = 0;
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let entry_key = row.get_field("key").and_then(|v| match v {
                    crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        result = match row.get_field("value") {
                            Some(crate::storage::schema::Value::Integer(n)) => *n as u64,
                            Some(crate::storage::schema::Value::UnsignedInteger(n)) => *n,
                            Some(crate::storage::schema::Value::Text(s)) => {
                                s.parse::<u64>().unwrap_or(default)
                            }
                            _ => default,
                        };
                    }
                }
            }
            true
        });
        result
    }

    pub(crate) fn config_f64(&self, key: &str, default: f64) -> f64 {
        if let Some(raw) = self.inner.env_config_overrides.get(key) {
            if let Ok(n) = raw.parse::<f64>() {
                return n;
            }
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return default;
        };
        let mut result = default;
        let mut latest_id: u64 = 0;
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let entry_key = row.get_field("key").and_then(|v| match v {
                    crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        result = match row.get_field("value") {
                            Some(crate::storage::schema::Value::Float(n)) => *n,
                            Some(crate::storage::schema::Value::Integer(n)) => *n as f64,
                            Some(crate::storage::schema::Value::UnsignedInteger(n)) => *n as f64,
                            Some(crate::storage::schema::Value::Text(s)) => {
                                s.parse::<f64>().unwrap_or(default)
                            }
                            _ => default,
                        };
                    }
                }
            }
            true
        });
        result
    }

    pub(crate) fn config_string(&self, key: &str, default: &str) -> String {
        if let Some(raw) = self.inner.env_config_overrides.get(key) {
            return raw.clone();
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return default.to_string();
        };
        let mut result = default.to_string();
        let mut latest_id: u64 = 0;
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let entry_key = row.get_field("key").and_then(|v| match v {
                    crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        if let Some(crate::storage::schema::Value::Text(value)) =
                            row.get_field("value")
                        {
                            result = value.to_string();
                        }
                    }
                }
            }
            true
        });
        result
    }

    /// Whether `SECRET('...')` literals should be encrypted with the
    /// vault AES key on INSERT. Default `true`.
    pub(crate) fn secret_auto_encrypt(&self) -> bool {
        self.config_bool("red.config.secret.auto_encrypt", true)
    }

    /// Whether `Value::Secret` columns should be decrypted back to
    /// plaintext on SELECT when the vault is unsealed. Default `true`.
    /// Turning this off keeps secrets masked as `***` even while the
    /// vault is open — useful for audit trails or read-only exports.
    pub(crate) fn secret_auto_decrypt(&self) -> bool {
        self.config_bool("red.config.secret.auto_decrypt", true)
    }

    /// Walk every record in `result` and swap `Value::Secret(bytes)`
    /// for the decrypted plaintext when the runtime has the vault
    /// AES key AND `red.config.secret.auto_decrypt = true`. If the
    /// key is missing, the vault is sealed, or auto_decrypt is off,
    /// secrets are left as `Value::Secret` which every formatter
    /// (Display, JSON) already masks as `***`.
    pub(crate) fn apply_secret_decryption(&self, result: &mut RuntimeQueryResult) {
        if !self.secret_auto_decrypt() {
            return;
        }
        let Some(key) = self.secret_aes_key() else {
            return;
        };
        for record in result.result.records.iter_mut() {
            for value in record.values_mut() {
                if let Value::Secret(ref bytes) = value {
                    if let Some(plain) =
                        super::impl_dml::decrypt_secret_payload(&key, bytes.as_slice())
                    {
                        if let Ok(text) = String::from_utf8(plain) {
                            *value = Value::text(text);
                        }
                    }
                }
            }
        }
    }

    /// Emit a CDC change event and replicate to WAL buffer.
    /// Create a `MutationEngine` bound to this runtime.
    ///
    /// The engine is cheap to construct (no allocation) and should be
    /// dropped after `apply` returns. Use this from application-layer
    /// `create_row` / `create_rows_batch` instead of calling
    /// `bulk_insert` + `index_entity_insert` + `cdc_emit` separately.
    pub(crate) fn mutation_engine(&self) -> crate::runtime::mutation::MutationEngine<'_> {
        crate::runtime::mutation::MutationEngine::new(self)
    }

    /// Public-mutation gate snapshot (PLAN.md W1).
    ///
    /// Surfaces that accept untrusted client requests (SQL DML/DDL,
    /// gRPC mutating RPCs, HTTP/native wire mutations, admin
    /// maintenance, serverless lifecycle) call `check_write` before
    /// dispatching to storage. Returns `RedDBError::ReadOnly` on any
    /// instance running as a replica or with `options.read_only =
    /// true`. The replica internal logical-WAL apply path reaches into
    /// the store directly and never calls this method, so legitimate
    /// replica catch-up still works.
    pub fn check_write(&self, kind: crate::runtime::write_gate::WriteKind) -> RedDBResult<()> {
        self.inner.write_gate.check(kind)
    }

    /// Read-only handle to the gate, useful for transports that want
    /// to surface the policy in health/status output without taking on
    /// a dependency on the concrete enum.
    pub fn write_gate(&self) -> &crate::runtime::write_gate::WriteGate {
        &self.inner.write_gate
    }

    /// Process lifecycle handle (PLAN.md Phase 1). Health probes,
    /// admin/shutdown, and signal handlers consult this single
    /// state machine.
    pub fn lifecycle(&self) -> &crate::runtime::lifecycle::Lifecycle {
        &self.inner.lifecycle
    }

    /// Operator-imposed resource limits (PLAN.md Phase 4.1).
    pub fn resource_limits(&self) -> &crate::runtime::resource_limits::ResourceLimits {
        &self.inner.resource_limits
    }

    /// Append-only audit log for admin mutations (PLAN.md Phase 6.5).
    pub fn audit_log(&self) -> &crate::runtime::audit_log::AuditLogger {
        &self.inner.audit_log
    }

    /// Shared `Arc` to the audit logger — used by collaborators (the
    /// lease lifecycle, future request-context plumbing) that need to
    /// keep the logger alive past the runtime's stack frame.
    pub fn audit_log_arc(&self) -> Arc<crate::runtime::audit_log::AuditLogger> {
        Arc::clone(&self.inner.audit_log)
    }

    pub(crate) fn emit_control_event(
        &self,
        kind: crate::runtime::control_events::EventKind,
        outcome: crate::runtime::control_events::Outcome,
        action: &'static str,
        resource: Option<String>,
        reason: Option<String>,
        extra_fields: Vec<(String, crate::runtime::control_events::Sensitivity)>,
    ) -> RedDBResult<()> {
        use crate::runtime::control_events::{
            ActorRef, ControlEvent, ControlEventCtx, ControlEventLedger, Sensitivity,
        };

        let tenant = current_tenant();
        let principal = current_auth_identity();
        let actor_user = principal
            .as_ref()
            .map(|(principal, _)| UserId::from_parts(tenant.as_deref(), principal));
        let actor = actor_user
            .as_ref()
            .map(ActorRef::User)
            .unwrap_or(ActorRef::Anonymous);
        let ctx = ControlEventCtx {
            actor,
            scope: tenant
                .as_ref()
                .map(|scope| std::borrow::Cow::Borrowed(scope.as_str())),
            request_id: Some(std::borrow::Cow::Owned(format!(
                "conn-{}",
                current_connection_id()
            ))),
            trace_id: None,
        };
        let mut fields = std::collections::HashMap::new();
        fields.insert(
            "connection_id".to_string(),
            Sensitivity::raw(current_connection_id().to_string()),
        );
        if let Some((_, role)) = principal {
            fields.insert("actor_role".to_string(), Sensitivity::raw(role.as_str()));
        }
        for (key, value) in extra_fields {
            fields.insert(key, value);
        }
        let event = ControlEvent {
            kind,
            outcome,
            action: std::borrow::Cow::Borrowed(action),
            resource,
            reason,
            matched_policy_id: None,
            fields,
        };
        let ledger = self.inner.control_event_ledger.read();
        match ledger.emit(&ctx, event) {
            Ok(_) => Ok(()),
            Err(err) if self.inner.control_event_config.require_persistence() => {
                Err(RedDBError::Internal(err.to_string()))
            }
            Err(_) => Ok(()),
        }
    }

    fn policy_mutation_control_ctx<'a>(
        &self,
        actor: &'a crate::auth::UserId,
        tenant: Option<&'a str>,
    ) -> crate::runtime::control_events::ControlEventCtx<'a> {
        crate::runtime::control_events::ControlEventCtx {
            actor: crate::runtime::control_events::ActorRef::User(actor),
            scope: tenant.map(std::borrow::Cow::Borrowed),
            request_id: Some(std::borrow::Cow::Owned(format!(
                "conn-{}",
                current_connection_id()
            ))),
            trace_id: None,
        }
    }

    fn emit_query_audit(
        &self,
        query: &str,
        plan: &QueryAuditPlan,
        duration_ms: u64,
        result: &RuntimeQueryResult,
    ) {
        if !self.inner.query_audit.has_rules() {
            return;
        }
        let actor = current_auth_identity().map(|(principal, _)| principal);
        let tenant = current_tenant();
        let row_count = if result.statement_type == "select" {
            result.result.records.len() as u64
        } else {
            result.affected_rows
        };
        self.inner
            .query_audit
            .emit(crate::runtime::query_audit::QueryAuditEvent {
                actor,
                tenant,
                statement_kind: plan.statement_kind,
                touched_collections: plan.collections.clone(),
                duration_ms,
                row_count,
                request_id: Some(crate::crypto::uuid::Uuid::new_v7().to_string()),
                query_hash: Some(blake3::hash(query.as_bytes()).to_hex().to_string()),
            });
    }

    /// Shared queue telemetry counters (delivered/acked/nacked).
    pub(crate) fn queue_telemetry(
        &self,
    ) -> &crate::runtime::queue_telemetry::QueueTelemetryCounters {
        &self.inner.queue_telemetry
    }

    /// Snapshots of the queue telemetry counters in label-deterministic
    /// order for `/metrics` rendering and the integration test.
    pub fn queue_telemetry_snapshot(
        &self,
    ) -> crate::runtime::queue_telemetry::QueueTelemetrySnapshot {
        crate::runtime::queue_telemetry::QueueTelemetrySnapshot {
            delivered: self.inner.queue_telemetry.delivered_snapshot(),
            acked: self.inner.queue_telemetry.acked_snapshot(),
            nacked: self.inner.queue_telemetry.nacked_snapshot(),
            wait_started: self.inner.queue_telemetry.wait_started_snapshot(),
            wait_woken: self.inner.queue_telemetry.wait_woken_snapshot(),
            wait_timed_out: self.inner.queue_telemetry.wait_timed_out_snapshot(),
            wait_cancelled: self.inner.queue_telemetry.wait_cancelled_snapshot(),
            wait_duration: self.inner.queue_telemetry.wait_duration_snapshot(),
        }
    }

    /// Per-`kind` query latency histograms for `/metrics` (only kinds with
    /// a real sample are present — empty kinds are absent, not zero-filled).
    pub fn query_latency_snapshot(
        &self,
    ) -> Vec<crate::runtime::query_latency_telemetry::QueryLatencyHistogram> {
        self.inner.query_latency_telemetry.snapshot()
    }

    /// Cross-kind query latency rollup for `/cluster/status` and the
    /// red-ui percentile panels. `count == 0` until a real sample exists.
    pub fn query_latency_rollup(
        &self,
    ) -> crate::runtime::query_latency_telemetry::QueryLatencyHistogram {
        self.inner.query_latency_telemetry.rollup()
    }

    /// Issue #742 — consumer presence registry. Heartbeats land here
    /// from `QUEUE READ` (and, in a follow-up slice, an explicit
    /// `QUEUE HEARTBEAT` command); Red UI and `red.queue_consumers`
    /// read snapshots through `queue_consumer_presence_snapshot`.
    pub(crate) fn queue_presence(
        &self,
    ) -> &std::sync::Arc<crate::storage::queue::presence::ConsumerPresenceRegistry> {
        &self.inner.queue_presence
    }

    /// Issue #742 — point-in-time presence snapshot, classifying each
    /// `(queue, group, consumer)` as active/stale/expired against the
    /// supplied TTL. Wall-clock is read once here so the lifecycle
    /// flags inside the snapshot are internally consistent.
    pub fn queue_consumer_presence_snapshot(
        &self,
        ttl_ms: u64,
    ) -> Vec<crate::storage::queue::presence::ConsumerPresence> {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        self.inner.queue_presence.snapshot(now_ns, ttl_ms)
    }

    /// Issue #742 — active-consumer count per `(queue, group)` for the
    /// queue-metadata surface. Stale/expired entries are excluded by
    /// definition; they are still visible in the per-row snapshot.
    pub fn queue_active_consumer_counts(
        &self,
        ttl_ms: u64,
    ) -> std::collections::HashMap<(String, String), u32> {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        self.inner
            .queue_presence
            .count_active_by_group(now_ns, ttl_ms)
    }

    /// Issue #743 — vector + TurboQuant introspection registry. Engine
    /// publish points (collection create, artifact build start /
    /// finish, fallback toggle, drop) update this; Red UI and
    /// `red.*` vector virtual tables read snapshots through
    /// `vector_introspection_snapshot` / `vector_introspection_get`.
    pub(crate) fn vector_introspection_registry(
        &self,
    ) -> &std::sync::Arc<crate::storage::vector::introspection::VectorIntrospectionRegistry> {
        &self.inner.vector_introspection
    }

    /// Issue #743 — full snapshot of every tracked vector collection's
    /// `(VectorMetadata, ArtifactMetadata)`. Deterministically ordered
    /// by collection name so Red UI tables and tests both see a
    /// stable shape.
    pub fn vector_introspection_snapshot(
        &self,
    ) -> Vec<crate::storage::vector::introspection::VectorIntrospection> {
        self.inner.vector_introspection.snapshot()
    }

    /// Issue #743 — single-collection lookup, for the per-collection
    /// metadata endpoint Red UI hits when an operator opens one
    /// vector's toolbar.
    pub fn vector_introspection_get(
        &self,
        collection: &str,
    ) -> Option<crate::storage::vector::introspection::VectorIntrospection> {
        self.inner.vector_introspection.get(collection)
    }

    /// Issue #1238 — ADR 0060 read-model accessor for slow-query telemetry.
    ///
    /// Returns a reference to the bounded ring store so HTTP handlers and
    /// the red-ui read model can call `store.read(filter)` without
    /// touching `red-slow.log` directly.
    pub fn slow_query_store(&self) -> &Arc<crate::telemetry::slow_query_store::SlowQueryStore> {
        &self.inner.slow_query_store
    }

    /// Slice 10 of issue #527 — render-time scan of pending entries
    /// per (queue, group) for the `queue_pending_gauge` exposition.
    /// Walks `red_queue_meta` live so the gauge cannot drift from
    /// the source of truth.
    pub fn queue_pending_counts(&self) -> Vec<((String, String), u64)> {
        let store = self.inner.db.store();
        crate::runtime::impl_queue::pending_counts_by_group(store.as_ref())
            .into_iter()
            .collect()
    }

    /// Shared `Arc` to the write gate. Same rationale as
    /// `audit_log_arc`: collaborators (lease lifecycle, refresh
    /// thread) need a clone-cheap handle they can move into a
    /// background thread.
    pub fn write_gate_arc(&self) -> Arc<crate::runtime::write_gate::WriteGate> {
        Arc::clone(&self.inner.write_gate)
    }

    /// Serverless writer-lease state machine. `None` when the operator
    /// did not opt into lease fencing (`RED_LEASE_REQUIRED` unset).
    pub fn lease_lifecycle(&self) -> Option<&Arc<crate::runtime::lease_lifecycle::LeaseLifecycle>> {
        self.inner.lease_lifecycle.get()
    }

    /// Install the lease lifecycle. Idempotent; subsequent calls
    /// return the previously stored value untouched.
    pub fn set_lease_lifecycle(
        &self,
        lifecycle: Arc<crate::runtime::lease_lifecycle::LeaseLifecycle>,
    ) -> Result<(), Arc<crate::runtime::lease_lifecycle::LeaseLifecycle>> {
        self.inner.lease_lifecycle.set(lifecycle)
    }

    /// Reject the call when the requested batch size exceeds
    /// `RED_MAX_BATCH_SIZE`. Returns `RedDBError::QuotaExceeded`
    /// shaped so the HTTP layer can map it to 413 Payload Too
    /// Large (PLAN.md Phase 4.1).
    pub fn check_batch_size(&self, requested: usize) -> RedDBResult<()> {
        if self.inner.resource_limits.batch_size_exceeded(requested) {
            let max = self.inner.resource_limits.max_batch_size.unwrap_or(0);
            return Err(RedDBError::QuotaExceeded(format!(
                "max_batch_size:{requested}:{max}"
            )));
        }
        Ok(())
    }

    /// Reject the call when the local DB file exceeds
    /// `RED_MAX_DB_SIZE_BYTES`. Reads file metadata once per call —
    /// the cost is a single `stat()` syscall, negligible against the
    /// I/O the caller is about to do. Returns `QuotaExceeded` shaped
    /// for HTTP 507 Insufficient Storage.
    pub fn check_db_size(&self) -> RedDBResult<()> {
        let Some(limit) = self.inner.resource_limits.max_db_size_bytes else {
            return Ok(());
        };
        if limit == 0 {
            return Ok(());
        }
        let Some(path) = self.inner.db.path() else {
            return Ok(());
        };
        let current = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if current > limit {
            return Err(RedDBError::QuotaExceeded(format!(
                "max_db_size_bytes:{current}:{limit}"
            )));
        }
        Ok(())
    }

    /// Graceful shutdown coordinator (PLAN.md Phase 1.1).
    ///
    /// Steps, in order, all idempotent across re-entrant calls:
    ///   1. Move lifecycle into `ShuttingDown` (concurrent callers
    ///      observe `Stopped` after first finishes).
    ///   2. Flush WAL + run final checkpoint via `db.flush()` so
    ///      every acked write is durable on disk.
    ///   3. If `backup_on_shutdown == true` and a remote backend is
    ///      configured, run a synchronous `trigger_backup()` so the
    ///      remote head reflects the final state.
    ///   4. Stamp the report and move to `Stopped`. Subsequent calls
    ///      return the cached report without re-running anything.
    ///
    /// On any error, the runtime is still marked `Stopped` so the
    /// process can exit; the caller logs the error context but does
    /// not retry the same shutdown — the operator can inspect the
    /// report fields to see which step failed.
    pub fn graceful_shutdown(
        &self,
        backup_on_shutdown: bool,
    ) -> RedDBResult<crate::runtime::lifecycle::ShutdownReport> {
        if !self.inner.lifecycle.begin_shutdown() {
            // Someone else already shut down (or is in flight). Return
            // the cached report so the HTTP caller and SIGTERM handler
            // get the same idempotent answer.
            return Ok(self.inner.lifecycle.shutdown_report().unwrap_or_default());
        }

        let started_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut report = crate::runtime::lifecycle::ShutdownReport {
            started_at_ms: started_ms,
            ..Default::default()
        };

        // Flush WAL + run any pending checkpoint. Local fsync is
        // unconditional — even a lease-lost replica needs its WAL on
        // disk before exit so a future restore has the latest tail.
        // The remote upload is gated separately so a lost-lease writer
        // doesn't clobber the new holder's state on its way out.
        let flush_res = self.inner.db.flush_local_only();
        report.flushed_wal = flush_res.is_ok();
        report.final_checkpoint = flush_res.is_ok();
        if let Err(err) = &flush_res {
            tracing::error!(
                target: "reddb::lifecycle",
                error = %err,
                "graceful_shutdown: local flush failed"
            );
        } else if let Err(lease_err) =
            self.assert_remote_write_allowed("shutdown/checkpoint_upload")
        {
            tracing::warn!(
                target: "reddb::serverless::lease",
                error = %lease_err,
                "graceful_shutdown: remote upload skipped — lease not held"
            );
        } else if let Err(err) = self.inner.db.upload_to_remote_backend() {
            tracing::error!(
                target: "reddb::lifecycle",
                error = %err,
                "graceful_shutdown: remote upload failed"
            );
        }

        // Optional final backup. Skipped silently when no remote
        // backend is configured — `trigger_backup()` returns Err
        // anyway in that case, but logging it as a shutdown failure
        // would be misleading on a standalone (no-backend) runtime.
        if backup_on_shutdown && self.inner.db.remote_backend.is_some() {
            // The trigger_backup gate now reads `WriteKind::Backup`,
            // which a replica/read_only instance refuses. That's
            // intentional — replicas don't drive backups; only the
            // primary does. We still want shutdown to flush its WAL
            // even if the backup branch is gated off.
            match self.trigger_backup() {
                Ok(result) => {
                    report.backup_uploaded = result.uploaded;
                }
                Err(err) => {
                    tracing::warn!(
                        target: "reddb::lifecycle",
                        error = %err,
                        "graceful_shutdown: final backup skipped"
                    );
                }
            }
        }

        let completed_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(started_ms);
        report.completed_at_ms = completed_ms;
        report.duration_ms = completed_ms.saturating_sub(started_ms);

        self.inner.lifecycle.finish_shutdown(report.clone());
        Ok(report)
    }

    /// PLAN.md Phase 4.4 — per-caller quota bucket. Always
    /// returned; `is_configured()` lets callers short-circuit.
    pub fn quota_bucket(&self) -> &crate::runtime::quota_bucket::QuotaBucket {
        &self.inner.quota_bucket
    }

    /// PLAN.md Phase 6.3 — whether at-rest encryption is configured.
    /// Reads `RED_ENCRYPTION_KEY` / `RED_ENCRYPTION_KEY_FILE` lazily;
    /// returns `("enabled", None)` when a key is loadable, `("error", Some(msg))`
    /// when the operator set the env but it doesn't parse, and
    /// `("disabled", None)` when no key is configured. The pager
    /// hookup is deferred — this accessor surfaces the operator's
    /// intent for /admin/status without yet using the key in writes.
    pub fn encryption_at_rest_status(&self) -> (&'static str, Option<String>) {
        match crate::crypto::page_encryption::key_from_env() {
            Ok(Some(_)) => ("enabled", None),
            Ok(None) => ("disabled", None),
            Err(err) => ("error", Some(err)),
        }
    }

    /// PLAN.md Phase 11.5 — current replica apply health label
    /// (`ok`, `gap`, `divergence`, `apply_error`, `connecting`,
    /// `stalled_gap`). Read from the persisted `red.replication.state`
    /// config key updated by the replica loop. Returns `None` on
    /// non-replica instances or when no apply has run yet.
    pub fn replica_apply_health(&self) -> Option<String> {
        let state = self.config_string("red.replication.state", "");
        if state.is_empty() {
            None
        } else {
            Some(state)
        }
    }

    pub fn acquire(&self) -> RedDBResult<RuntimeConnection> {
        let mut pool = self
            .inner
            .pool
            .lock()
            .map_err(|e| RedDBError::Internal(format!("connection pool lock poisoned: {e}")))?;
        if pool.active >= self.inner.pool_config.max_connections {
            return Err(RedDBError::Internal(
                "connection pool exhausted".to_string(),
            ));
        }

        let id = if let Some(id) = pool.idle.pop() {
            id
        } else {
            let id = pool.next_id;
            pool.next_id += 1;
            id
        };
        pool.active += 1;
        pool.total_checkouts += 1;
        drop(pool);

        Ok(RuntimeConnection {
            id,
            inner: Arc::clone(&self.inner),
        })
    }

    pub fn checkpoint(&self) -> RedDBResult<()> {
        // Local fsync always allowed — losing the lease shouldn't
        // prevent us from durably persisting what's already in memory.
        // The remote upload is the side-effect that risks clobbering a
        // peer's state, so it's behind the lease gate.
        self.inner.db.flush_local_only().map_err(|err| {
            // Issue #205 — local flush failure is a CheckpointFailed
            // operator-grade event. The local-flush path also covers
            // the WAL fsync we depend on, so a failure here doubles as
            // the WalFsyncFailed signal for the runtime entry point.
            let msg = err.to_string();
            crate::telemetry::operator_event::OperatorEvent::CheckpointFailed {
                lsn: 0,
                error: msg.clone(),
            }
            .emit_global();
            crate::telemetry::operator_event::OperatorEvent::WalFsyncFailed {
                path: "<flush_local_only>".to_string(),
                error: msg.clone(),
            }
            .emit_global();
            RedDBError::Engine(msg)
        })?;
        if let Err(err) = self.assert_remote_write_allowed("checkpoint") {
            tracing::warn!(
                target: "reddb::serverless::lease",
                error = %err,
                "checkpoint: skipping remote upload — lease not held"
            );
            return Ok(());
        }
        self.inner
            .db
            .upload_to_remote_backend()
            .map_err(|err| RedDBError::Engine(err.to_string()))
    }

    /// Guard remote-mutating operations on the writer lease.
    /// Returns `Ok(())` when no remote backend is configured (the
    /// lease is irrelevant) or the lease state is `NotRequired` /
    /// `Held`. Returns `RedDBError::ReadOnly` when the lease is
    /// `NotHeld`, with an audit-friendly action label so the caller
    /// can record the rejection.
    pub(crate) fn assert_remote_write_allowed(&self, action: &str) -> RedDBResult<()> {
        if self.inner.db.remote_backend.is_none() {
            return Ok(());
        }
        match self.inner.write_gate.lease_state() {
            crate::runtime::write_gate::LeaseGateState::NotHeld => {
                self.inner.audit_log.record(
                    action,
                    "system",
                    "remote_backend",
                    "err: writer lease not held",
                    crate::json::Value::Null,
                );
                Err(RedDBError::ReadOnly(format!(
                    "writer lease not held — {action} blocked (serverless fence)"
                )))
            }
            _ => Ok(()),
        }
    }

    pub fn run_maintenance(&self) -> RedDBResult<()> {
        self.inner
            .db
            .run_maintenance()
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn scan_collection(
        &self,
        collection: &str,
        cursor: Option<ScanCursor>,
        limit: usize,
    ) -> RedDBResult<ScanPage> {
        let store = self.inner.db.store();
        let manager = store
            .get_collection(collection)
            .ok_or_else(|| RedDBError::NotFound(collection.to_string()))?;

        let mut entities = manager.query_all(|_| true);
        entities.sort_by_key(|entity| entity.id.raw());

        let offset = cursor.map(|cursor| cursor.offset).unwrap_or(0);
        let total = entities.len();
        let end = total.min(offset.saturating_add(limit.max(1)));
        let items = if offset >= total {
            Vec::new()
        } else {
            entities[offset..end].to_vec()
        };
        let next = (end < total).then_some(ScanCursor { offset: end });

        Ok(ScanPage {
            collection: collection.to_string(),
            items,
            next,
            total,
        })
    }

    pub fn catalog(&self) -> CatalogModelSnapshot {
        self.inner.db.catalog_model_snapshot()
    }

    pub fn catalog_consistency_report(&self) -> crate::catalog::CatalogConsistencyReport {
        self.inner.db.catalog_consistency_report()
    }

    pub fn catalog_attention_summary(&self) -> CatalogAttentionSummary {
        crate::catalog::attention_summary(&self.catalog())
    }

    pub fn collection_attention(&self) -> Vec<CollectionDescriptor> {
        crate::catalog::collection_attention(&self.catalog())
    }

    pub fn index_attention(&self) -> Vec<CatalogIndexStatus> {
        crate::catalog::index_attention(&self.catalog())
    }

    pub fn graph_projection_attention(&self) -> Vec<CatalogGraphProjectionStatus> {
        crate::catalog::graph_projection_attention(&self.catalog())
    }

    pub fn analytics_job_attention(&self) -> Vec<CatalogAnalyticsJobStatus> {
        crate::catalog::analytics_job_attention(&self.catalog())
    }

    pub fn stats(&self) -> RuntimeStats {
        let pool = runtime_pool_lock(self);
        RuntimeStats {
            active_connections: pool.active,
            idle_connections: pool.idle.len(),
            total_checkouts: pool.total_checkouts,
            paged_mode: self.inner.db.is_paged(),
            started_at_unix_ms: self.inner.started_at_unix_ms,
            store: self.inner.db.stats(),
            system: SystemInfo::collect(),
            result_blob_cache: self.inner.result_blob_cache.stats(),
            kv: self.inner.kv_stats.snapshot(),
            metrics_ingest: self.inner.metrics_ingest_stats.snapshot(),
        }
    }

    pub(crate) fn record_metrics_ingest(
        &self,
        accepted_samples: u64,
        accepted_series: u64,
        rejected_samples: u64,
        rejected_series: u64,
    ) {
        self.inner.metrics_ingest_stats.record(
            accepted_samples,
            accepted_series,
            rejected_samples,
            rejected_series,
        );
    }

    pub(crate) fn record_metrics_cardinality_budget_rejections(&self, rejected_series: u64) {
        self.inner
            .metrics_ingest_stats
            .record_cardinality_budget_rejections(rejected_series);
    }

    pub(crate) fn record_metrics_tenant_activity(
        &self,
        tenant: &str,
        namespace: &str,
        operation: &str,
    ) {
        self.inner
            .metrics_tenant_activity_stats
            .record(tenant, namespace, operation);
    }

    pub(crate) fn metrics_tenant_activity_snapshot(
        &self,
    ) -> Vec<crate::runtime::MetricsTenantActivityStats> {
        self.inner.metrics_tenant_activity_stats.snapshot()
    }

    /// Execute a query under a typed scope override without embedding
    /// the tenant / user / role values into the SQL string. Use this
    /// from transport middleware (HTTP / gRPC / worker loops) where the
    /// scope is resolved from auth claims and the SQL is a parameterised
    /// template — avoids the string-concat injection risk of building
    /// `WITHIN TENANT '<id>' …` manually, and is drop-in compatible with
    /// prepared statements that didn't know about tenancy.
    ///
    /// Precedence matches the `WITHIN` clause: the passed `scope`
    /// overrides `SET LOCAL TENANT`, which overrides `SET TENANT`.
    /// The override is pushed on the thread-local scope stack for the
    /// duration of the call and popped on return — pool-shared
    /// connections cannot leak it across requests.
    pub fn execute_query_with_scope(
        &self,
        query: &str,
        scope: crate::runtime::within_clause::ScopeOverride,
    ) -> RedDBResult<RuntimeQueryResult> {
        if scope.is_empty() {
            return self.execute_query(query);
        }
        let _scope_guard = ScopeOverrideGuard::install(scope);
        self.execute_query(query)
    }

    /// Issue #205 — single lifecycle exit for slow-query logging.
    ///
    /// `execute_query_inner` does the real work; this wrapper times it
    /// and, if elapsed exceeds the configured threshold, hands the
    /// triple `(QueryKind, elapsed_ms, sql_redacted, scope)` to the
    /// SlowQueryLogger. The threshold + sample_pct were captured at
    /// SlowQueryLogger construction (runtime startup), so the per-call
    /// cost on below-threshold paths is one relaxed atomic load.
    pub fn execute_query(&self, query: &str) -> RedDBResult<RuntimeQueryResult> {
        let started = std::time::Instant::now();
        let result = self.execute_query_inner(query);
        self.finish_query_lifecycle(query, started, result)
    }

    /// Execute a SQL statement with already-decoded positional bind
    /// parameters. Transports should call this instead of parsing +
    /// binding on their side and then reaching for `execute_query_expr`:
    /// this entry keeps parameterized statements inside the same
    /// statement lifecycle as textual SQL (snapshot guard, config/secret
    /// guards, coarse auth, intent locks, slow-query logging, integrity
    /// tombstone filtering, and causal bookmarks).
    pub fn execute_query_with_params(
        &self,
        query: &str,
        params: &[Value],
    ) -> RedDBResult<RuntimeQueryResult> {
        if params.is_empty() {
            return self.execute_query(query);
        }
        let started = std::time::Instant::now();
        let result = self.execute_query_with_params_inner(query, params);
        self.finish_query_lifecycle(query, started, result)
    }

    fn finish_query_lifecycle(
        &self,
        query: &str,
        started: std::time::Instant,
        mut result: RedDBResult<RuntimeQueryResult>,
    ) -> RedDBResult<RuntimeQueryResult> {
        // Issue #765 / S6 — filter integrity-tombstoned rows out of SELECT
        // results before they reach any consumer. Fast no-op (one relaxed
        // atomic load) unless an input-stream digest mismatch has tombstoned
        // a RID range on this store.
        if let Ok(ref mut query_result) = result {
            if query_result.statement_type == "select" {
                self.filter_integrity_tombstoned(&mut query_result.result);
            }
        }
        let elapsed_ms = started.elapsed().as_millis() as u64;

        // Build EffectiveScope from the same thread-locals frame-build
        // consults — keeps the slow-log row consistent with the audit /
        // RLS view of "this statement". `ai_scope()` is the canonical
        // builder.
        let scope = self.ai_scope();
        let kind = match result
            .as_ref()
            .map(|r| r.statement_type)
            .unwrap_or("select")
        {
            "select" => crate::telemetry::slow_query_logger::QueryKind::Select,
            "insert" => crate::telemetry::slow_query_logger::QueryKind::Insert,
            "update" => crate::telemetry::slow_query_logger::QueryKind::Update,
            "delete" => crate::telemetry::slow_query_logger::QueryKind::Delete,
            _ => crate::telemetry::slow_query_logger::QueryKind::Internal,
        };
        // SQL redaction: pass the raw query through. The slow-query
        // logger writes structured JSON so embedded literals stay
        // escape-safe at the JSON boundary (proven by
        // `adversarial_sql_is_escape_safe` in slow_query_logger.rs).
        // PII redaction (e.g. literal masking) is a follow-up.
        self.inner
            .slow_query_logger
            .record(kind, elapsed_ms, query.to_string(), &scope);

        // Issue #1241 — record latency into the bounded per-`kind`
        // histogram substrate (always, not only above the slow-query
        // threshold). `started.elapsed()` is re-read here for sub-ms
        // resolution; the cost is one `Instant::now` plus a handful of
        // relaxed atomic adds (see `query_latency_telemetry` docs).
        self.inner
            .query_latency_telemetry
            .observe(kind, started.elapsed().as_secs_f64());

        if let Ok(ref mut query_result) = result {
            if matches!(query_result.statement_type, "insert" | "update" | "delete") {
                let bookmark = crate::replication::CausalBookmark::new(
                    self.current_replication_term(),
                    self.cdc_current_lsn(),
                );
                query_result.bookmark = Some(bookmark.encode());
            }
        }

        result
    }

    fn execute_query_with_params_inner(
        &self,
        query: &str,
        params: &[Value],
    ) -> RedDBResult<RuntimeQueryResult> {
        let parsed = parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?;
        let bound = crate::storage::query::user_params::bind(&parsed, params).map_err(|err| {
            RedDBError::Validation {
                message: err.to_string(),
                validation: crate::json!({
                    "code": "INVALID_PARAMS",
                    "surface": "query.params",
                }),
            }
        })?;
        self.execute_bound_query_expr_in_frame(query, bound)
    }

    fn execute_bound_query_expr_in_frame(
        &self,
        query: &str,
        expr: QueryExpr,
    ) -> RedDBResult<RuntimeQueryResult> {
        let rewritten_query = super::red_schema::rewrite_virtual_names(query);
        let execution_query = rewritten_query.as_deref().unwrap_or(query);
        let frame = super::statement_frame::StatementExecutionFrame::build(self, execution_query)?;
        let _frame_guards = frame.install(self);
        let _log_span = crate::telemetry::span::query_span(query).entered();

        let expr = self.rewrite_view_refs(expr);
        let mode = detect_mode(execution_query);
        let control_event_specs = query_control_event_specs(&expr);
        let _lock_guard = match frame.prepare_dispatch(self, &expr) {
            Ok(guard) => guard,
            Err(err) => {
                let outcome = control_event_outcome_for_error(&err);
                for spec in &control_event_specs {
                    self.emit_control_event(
                        spec.kind,
                        outcome,
                        spec.action,
                        spec.resource.clone(),
                        Some(err.to_string()),
                        spec.fields.clone(),
                    )?;
                }
                return Err(err);
            }
        };

        let mut result = self.dispatch_expr(expr, query, mode)?;
        if result.statement_type == "select" {
            self.apply_secret_decryption(&mut result);
        }
        Ok(result)
    }

    pub fn causal_session(&self) -> crate::runtime::CausalSession {
        crate::runtime::CausalSession {
            runtime: self.clone(),
            bookmark: None,
            wait_timeout: std::time::Duration::from_secs(5),
        }
    }

    pub fn wait_for_bookmark(
        &self,
        bookmark: &crate::replication::CausalBookmark,
        timeout: std::time::Duration,
    ) -> RedDBResult<()> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let applied_lsn = self.local_contiguous_applied_lsn();
            if applied_lsn >= bookmark.commit_lsn() {
                return Ok(());
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(RedDBError::InvalidOperation(format!(
                    "timed out waiting for causal bookmark lsn {}; applied={}",
                    bookmark.commit_lsn(),
                    applied_lsn
                )));
            }
            let remaining = deadline.saturating_duration_since(now);
            std::thread::sleep(remaining.min(std::time::Duration::from_millis(5)));
        }
    }

    fn local_contiguous_applied_lsn(&self) -> u64 {
        match self.inner.db.options().replication.role {
            crate::replication::ReplicationRole::Replica { .. } => {
                self.config_u64("red.replication.last_applied_lsn", 0)
            }
            _ => self.cdc_current_lsn(),
        }
    }

    #[inline(never)]
    fn execute_query_inner(&self, query: &str) -> RedDBResult<RuntimeQueryResult> {
        // ── ULTRA-TURBO: autocommit `SELECT * FROM t WHERE _entity_id = N` ──
        //
        // Moved above every boot-cost the normal path pays (WITHIN
        // strip, SET LOCAL parse, tx_local_tenants read, snapshot
        // guard, tracing span, tx_contexts read) because the bench's
        // `select_point` scenario was observed at 28× vs PostgreSQL —
        // the dominant cost wasn't the entity fetch but the ceremony
        // before it. Only fires when there's no ambient transaction
        // context or WITHIN override, so the snapshot install we skip
        // truly is a no-op for this query.
        if !has_scope_override_active()
            && !query.trim_start().starts_with("WITHIN")
            && !query.trim_start().starts_with("within")
            && !self.inner.query_audit.has_rules()
            && !self
                .inner
                .tx_contexts
                .read()
                .contains_key(&current_connection_id())
        {
            if let Some(result) = self.try_fast_entity_lookup(query) {
                return result;
            }
        }

        // `WITHIN TENANT '<id>' [USER '<u>'] [AS ROLE '<r>'] <stmt>` —
        // strip the prefix, push a stack-scoped override, recurse on
        // the inner statement, pop on return. Stack lives in a
        // thread-local but is balanced by the RAII guard, so a
        // pool-shared connection cannot leak the override across
        // requests and an early `?` return still pops cleanly.
        match crate::runtime::within_clause::try_strip_within_prefix(query) {
            Ok(Some((scope, inner))) => {
                let _scope_guard = ScopeOverrideGuard::install(scope);
                // Re-enter the inner path, NOT `execute_query`, so the
                // slow-query lifecycle hook records exactly one row per
                // top-level statement (the WITHIN-stripped form would
                // double-record).
                return self.execute_query_inner(inner);
            }
            Ok(None) => {}
            Err(msg) => return Err(RedDBError::Query(msg)),
        }

        // `EXPLAIN <stmt>` — introspection. Runs the planner on the
        // inner statement (WITHOUT executing it) and returns the
        // CanonicalLogicalNode tree as rows so the caller can see the
        // operator shape and estimated cost. `EXPLAIN ALTER FOR ...`
        // is a distinct schema-diff command and continues down the
        // regular SQL path.
        if let Some(inner) = strip_explain_prefix(query) {
            return self.explain_as_rows(query, inner);
        }

        // `SET LOCAL TENANT '<id>'` — write the per-transaction tenant
        // override and return. Outside a transaction the statement is
        // an error (matches PG semantics: SET LOCAL only takes effect
        // within an active transaction).
        if let Some(value) = parse_set_local_tenant(query)? {
            let conn_id = current_connection_id();
            if !self.inner.tx_contexts.read().contains_key(&conn_id) {
                return Err(RedDBError::Query(
                    "SET LOCAL TENANT requires an active transaction".to_string(),
                ));
            }
            self.inner
                .tx_local_tenants
                .write()
                .insert(conn_id, value.clone());
            return Ok(RuntimeQueryResult::ok_message(
                query.to_string(),
                &match &value {
                    Some(id) => format!("local tenant set: {id}"),
                    None => "local tenant cleared".to_string(),
                },
                "set_local_tenant",
            ));
        }

        if super::red_schema::is_system_schema_write(query) {
            return Err(RedDBError::Query(
                super::red_schema::READ_ONLY_ERROR.to_string(),
            ));
        }

        if let Some(create_source) = super::analytics_source_catalog::parse_create_statement(query)?
        {
            return self.execute_create_analytics_source(query, create_source);
        }

        // Issue #790 — `READ METRIC <path>` is intentionally rejected at
        // v0. The descriptor itself is readable through
        // `red.analytics.metrics`; the *output* read returns a
        // structured error so callers can tell "execution engine not yet
        // built" apart from "metric does not exist".
        if let Some(path) = super::metric_descriptor_catalog::parse_read_metric_statement(query) {
            return Err(super::metric_descriptor_catalog::read_output_unsupported(
                &path,
            ));
        }

        // Issue #918 / ADR 0035 — leaderboard rank capability catalog
        // declarations are still recognised before the general parser.
        // Rank reads themselves are parser AST nodes, including Redis-flavor
        // Z* sugar that desugars to the same canonical rank shapes.
        if let Some(parsed) = super::ranking_descriptor_catalog::parse_create_ranking(query) {
            return self.execute_create_ranking(query, parsed?);
        }
        if super::ranking_descriptor_catalog::parse_show_rankings(query) {
            return self.execute_show_rankings(query);
        }

        let rewritten_query = super::red_schema::rewrite_virtual_names(query);
        let execution_query = rewritten_query.as_deref().unwrap_or(query);

        let frame = super::statement_frame::StatementExecutionFrame::build(self, execution_query)?;
        let _frame_guards = frame.install(self);

        // Phase 6 logging: enter a span stamped with conn_id / tenant
        // / query_len. Every downstream tracing::info!/warn!/error!
        // inherits these fields — no need to thread them manually
        // through storage/scan layers. Entered AFTER the WITHIN /
        // SET LOCAL TENANT resolution above so the span reflects the
        // effective scope for this statement.
        let _log_span = crate::telemetry::span::query_span(query).entered();

        // ── CTE prelude (#41) — `WITH x AS (...) SELECT ... FROM x` ──
        if let Some(rewritten) = frame.prepare_cte(execution_query)? {
            return self.execute_query_expr(rewritten);
        }

        // ── TURBO: bypass SQL parse for SELECT * FROM x WHERE _entity_id = N ──
        if !self.inner.query_audit.has_rules() {
            if let Some(result) = self.try_fast_entity_lookup(execution_query) {
                return result;
            }
        }

        // ── Result cache: return cached result if still fresh (30s TTL) ──
        if !self.inner.query_audit.has_rules() {
            if let Some(result) = frame.read_result_cache(self) {
                return Ok(result);
            }
        }

        let prepared = frame.prepare_statement(self, execution_query)?;
        let mode = prepared.mode;
        let expr = prepared.expr;

        let statement = query_expr_name(&expr);
        let result_cache_scopes = query_expr_result_cache_scopes(&expr);
        let control_event_specs = query_control_event_specs(&expr);
        let query_audit_plan = query_audit_plan(&expr);

        let _lock_guard = match frame.prepare_dispatch(self, &expr) {
            Ok(guard) => guard,
            Err(err) => {
                let outcome = control_event_outcome_for_error(&err);
                for spec in &control_event_specs {
                    self.emit_control_event(
                        spec.kind,
                        outcome,
                        spec.action,
                        spec.resource.clone(),
                        Some(err.to_string()),
                        spec.fields.clone(),
                    )?;
                }
                return Err(err);
            }
        };
        let frame_iface: &dyn super::statement_frame::ReadFrame = &frame;
        let query_audit_started = std::time::Instant::now();

        let query_result = match expr {
            QueryExpr::Graph(_) | QueryExpr::Path(_) => {
                // Apply MVCC visibility + RLS gate while materialising the
                // graph: every node entity is screened against the source
                // collection's policy chain (basic and `Nodes`-targeted)
                // and dropped when the caller's tenant / role doesn't
                // admit it. Edges are pruned automatically because the
                // graph builder skips edges whose endpoints aren't in
                // `allowed_nodes`.
                let (graph, node_properties, edge_properties) =
                    self.materialize_graph_with_rls()?;
                let result =
                    crate::storage::query::unified::UnifiedExecutor::execute_on_with_graph_properties(
                        &graph,
                        &expr,
                        node_properties,
                        edge_properties,
                    )
                        .map_err(|err| RedDBError::Query(err.to_string()))?;

                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement,
                    engine: "materialized-graph",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                })
            }
            QueryExpr::Table(table) => {
                let table = self.resolve_table_expr_subqueries(
                    table,
                    &frame as &dyn super::statement_frame::ReadFrame,
                )?;
                // Table-valued functions (e.g. components(g)) dispatch to a
                // read-only executor before any catalog/virtual-table routing
                // (issue #795).
                if let Some(TableSource::Function {
                    name,
                    args,
                    named_args,
                }) = table.source.clone()
                {
                    // The graph-collection form is cacheable (issue #802): the
                    // result-cache read at the top of this function keys on the
                    // query string, and `result_cache_scopes` carries the graph
                    // collection (see `collect_table_source_scopes`) so a write
                    // to it invalidates the entry. Deterministic algorithm
                    // output is worth caching at any row count, so the write
                    // bypasses the generic ≤5-row payload heuristic.
                    let tvf_result = RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-graph-tvf",
                        result: self.execute_table_function(&name, &args, &named_args)?,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    };
                    frame.write_result_cache(self, &tvf_result, result_cache_scopes.clone());
                    return Ok(tvf_result);
                }
                // Inline-graph TVF (issue #799): the graph is supplied by two
                // subqueries instead of a collection reference. Unlike the
                // graph-collection form, the result IS cacheable — its cache
                // key is the query string (the result-cache read at the top of
                // `execute_query_inner` keys on it) and `result_cache_scopes`
                // already carries the `nodes`/`edges` source collections, so a
                // write to any of them invalidates the entry.
                if let Some(TableSource::InlineGraphFunction {
                    name,
                    nodes,
                    edges,
                    named_args,
                }) = table.source.clone()
                {
                    let inline_result = RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-graph-tvf-inline",
                        result: self.execute_inline_graph_function(
                            &name,
                            &nodes,
                            &edges,
                            &named_args,
                        )?,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    };
                    frame.write_result_cache(self, &inline_result, result_cache_scopes);
                    return Ok(inline_result);
                }
                if super::red_schema::is_virtual_table(&table.table) {
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-red-schema",
                        result: super::red_schema::red_query(
                            self,
                            &table.table,
                            &table,
                            &frame as &dyn super::statement_frame::ReadFrame,
                        )?,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    });
                }

                // `<graph>.<output>` analytics virtual view (issue #800).
                // Recomputed on demand — intentionally not result-cached, so it
                // always reflects the current graph data.
                if let Some(view_result) = self.try_resolve_analytics_view(
                    &table,
                    &frame as &dyn super::statement_frame::ReadFrame,
                )? {
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-graph-analytics-view",
                        result: view_result,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    });
                }

                if let Some(result) = self.execute_probabilistic_select(&table)? {
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-probabilistic",
                        result,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    });
                }

                // Foreign-table intercept (Phase 3.2.2 PG parity).
                //
                // When the referenced table matches a `CREATE FOREIGN TABLE`
                // registration, short-circuit into the FDW scan. Phase 3.2
                // wrappers don't yet support pushdown, so filters/projections
                // apply post-scan via `apply_foreign_table_filters` — good
                // enough for correctness; perf work lands in 3.2.3.
                if self.inner.foreign_tables.is_foreign_table(&table.table) {
                    let records = self
                        .inner
                        .foreign_tables
                        .scan(&table.table)
                        .map_err(|e| RedDBError::Internal(e.to_string()))?;
                    let result = apply_foreign_table_filters(records, &table);
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-fdw",
                        result,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    });
                }

                // Row-Level Security enforcement (Phase 2.5.2 PG parity).
                //
                // When RLS is enabled on this table, fetch every policy
                // that applies to the current (role, SELECT) pair and
                // fold them into the query's WHERE clause: policies
                // OR-combine (any of them admitting the row is enough),
                // then AND into the caller's existing filter.
                //
                // Anonymous callers (no thread-local identity) pass
                // `role = None`; policies with a specific `TO role`
                // clause skip, but `TO PUBLIC` policies still apply.
                //
                // When `inject_rls_filters` returns `None` the table has
                // RLS enabled but no policy admits the caller's role —
                // short-circuit with an empty result set instead of
                // synthesising a contradiction filter.
                let Some(table_with_rls) = self.authorize_relational_table_select(
                    table,
                    &frame as &dyn super::statement_frame::ReadFrame,
                )?
                else {
                    let empty = crate::storage::query::unified::UnifiedResult::empty();
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement,
                        engine: "runtime-table-rls",
                        result: empty,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    });
                };
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement,
                    engine: "runtime-table",
                    // #885: lend the frame-owned row-buffer arena to the
                    // streaming path so chunk buffers are reused across
                    // this statement's chunk-fetches instead of allocated
                    // fresh per chunk. This is the table-query dispatch
                    // that runs under a `StatementExecutionFrame`; the
                    // frameless prepared/subquery paths keep `None`.
                    result: execute_runtime_table_query_in(
                        &self.inner.db,
                        &table_with_rls,
                        Some(&self.inner.index_store),
                        Some(frame.row_arena()),
                    )?,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                })
            }
            QueryExpr::Join(join) => {
                // Fold per-table RLS filters into each `QueryExpr::Table`
                // leaf of the join tree before executing. Without this
                // the join executor scans both tables raw and ignores
                // policies — a `WITHIN TENANT 'x'` against a join of
                // two tenant-scoped tables would leak cross-tenant rows.
                // When any leaf has RLS enabled and zero matching policy,
                // short-circuit to an empty join result instead of
                // emitting a contradiction filter.
                let join_with_rls = match self.authorize_relational_join_select(
                    join,
                    &frame as &dyn super::statement_frame::ReadFrame,
                )? {
                    Some(j) => j,
                    None => {
                        return Ok(RuntimeQueryResult {
                            query: query.to_string(),
                            mode,
                            statement,
                            engine: "runtime-join-rls",
                            result: crate::storage::query::unified::UnifiedResult::empty(),
                            affected_rows: 0,
                            statement_type: "select",
                            bookmark: None,
                        });
                    }
                };
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement,
                    engine: "runtime-join",
                    result: execute_runtime_join_query(&self.inner.db, &join_with_rls)?,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                })
            }
            QueryExpr::Vector(vector) => Ok(RuntimeQueryResult {
                query: query.to_string(),
                mode,
                statement,
                engine: "runtime-vector",
                result: execute_runtime_vector_query(&self.inner.db, &vector)?,
                affected_rows: 0,
                statement_type: "select",
                bookmark: None,
            }),
            QueryExpr::Hybrid(hybrid) => Ok(RuntimeQueryResult {
                query: query.to_string(),
                mode,
                statement,
                engine: "runtime-hybrid",
                result: execute_runtime_hybrid_query(&self.inner.db, &hybrid)?,
                affected_rows: 0,
                statement_type: "select",
                bookmark: None,
            }),
            QueryExpr::RankOf(ref rank) => self.execute_rank_of(query, rank),
            QueryExpr::ApproxRankOf(ref rank) => self.execute_approx_rank_of(query, rank),
            QueryExpr::RankRange(ref range) => self.execute_rank_range(query, range),
            // DML execution
            QueryExpr::Insert(ref insert) if super::red_schema::is_virtual_table(&insert.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Update(ref update) if super::red_schema::is_virtual_table(&update.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Delete(ref delete) if super::red_schema::is_virtual_table(&delete.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Insert(ref insert) => self
                .with_deferred_store_wal_for_dml(self.insert_may_emit_events(insert), || {
                    self.execute_insert(query, insert)
                }),
            QueryExpr::Update(ref update) => self
                .with_deferred_store_wal_for_dml(self.update_may_emit_events(update), || {
                    self.execute_update(query, update)
                }),
            QueryExpr::Delete(ref delete) => self
                .with_deferred_store_wal_for_dml(self.delete_may_emit_events(delete), || {
                    self.execute_delete(query, delete)
                }),
            // DDL execution
            QueryExpr::CreateTable(ref create) => self.execute_create_table(query, create),
            QueryExpr::CreateCollection(ref create) => {
                self.execute_create_collection(query, create)
            }
            QueryExpr::CreateVector(ref create) => self.execute_create_vector(query, create),
            QueryExpr::DropTable(ref drop_tbl) => self.execute_drop_table(query, drop_tbl),
            QueryExpr::DropGraph(ref drop_graph) => self.execute_drop_graph(query, drop_graph),
            QueryExpr::DropVector(ref drop_vector) => self.execute_drop_vector(query, drop_vector),
            QueryExpr::DropDocument(ref drop_document) => {
                self.execute_drop_document(query, drop_document)
            }
            QueryExpr::DropKv(ref drop_kv) => self.execute_drop_kv(query, drop_kv),
            QueryExpr::DropCollection(ref drop_collection) => {
                self.execute_drop_collection(query, drop_collection)
            }
            QueryExpr::Truncate(ref truncate) => self.execute_truncate(query, truncate),
            QueryExpr::AlterTable(ref alter) => self.execute_alter_table(query, alter),
            QueryExpr::ExplainAlter(ref explain) => self.execute_explain_alter(query, explain),
            // Graph analytics commands
            QueryExpr::GraphCommand(ref cmd) => self.execute_graph_command(query, cmd),
            // Search commands
            QueryExpr::SearchCommand(ref cmd) => self.execute_search_command(query, cmd),
            // ASK: RAG query with LLM synthesis
            QueryExpr::Ask(ref ask) => self.execute_ask(query, ask),
            QueryExpr::CreateIndex(ref create_idx) => self.execute_create_index(query, create_idx),
            QueryExpr::DropIndex(ref drop_idx) => self.execute_drop_index(query, drop_idx),
            QueryExpr::ProbabilisticCommand(ref cmd) => {
                self.execute_probabilistic_command(query, cmd)
            }
            // Time-series DDL
            QueryExpr::CreateTimeSeries(ref ts) => self.execute_create_timeseries(query, ts),
            QueryExpr::CreateMetric(ref metric) => self.execute_create_metric(query, metric),
            QueryExpr::AlterMetric(ref alter) => self.execute_alter_metric(query, alter),
            QueryExpr::CreateSlo(ref slo) => self.execute_create_slo(query, slo),
            QueryExpr::DropTimeSeries(ref ts) => self.execute_drop_timeseries(query, ts),
            // Queue DDL and commands
            QueryExpr::CreateQueue(ref q) => self.execute_create_queue(query, q),
            QueryExpr::AlterQueue(ref q) => self.execute_alter_queue(query, q),
            QueryExpr::DropQueue(ref q) => self.execute_drop_queue(query, q),
            QueryExpr::QueueSelect(ref q) => self.execute_queue_select(query, q),
            QueryExpr::QueueCommand(ref cmd) => self.execute_queue_command(query, cmd),
            QueryExpr::EventsBackfill(ref backfill) => {
                self.execute_events_backfill(query, backfill)
            }
            QueryExpr::EventsBackfillStatus { ref collection } => Err(RedDBError::Query(format!(
                "EVENTS BACKFILL STATUS for '{collection}' is not implemented in this slice"
            ))),
            QueryExpr::KvCommand(ref cmd) => self.execute_kv_command(query, cmd),
            QueryExpr::ConfigCommand(ref cmd) => self.execute_config_command(query, cmd),
            QueryExpr::CreateTree(ref tree) => self.execute_create_tree(query, tree),
            QueryExpr::DropTree(ref tree) => self.execute_drop_tree(query, tree),
            QueryExpr::TreeCommand(ref cmd) => self.execute_tree_command(query, cmd),
            // SET CONFIG key = value
            QueryExpr::SetConfig { ref key, ref value } => {
                if key.starts_with("red.secret.") {
                    return Err(RedDBError::Query(
                        "red.secret.* is reserved for vault secrets; use SET SECRET".to_string(),
                    ));
                }
                if key.starts_with("red.secrets.") {
                    return Err(RedDBError::Query(
                        "red.secrets.* is reserved for vault secrets; use SET SECRET".to_string(),
                    ));
                }
                match self.check_managed_config_write_for_set_config(key) {
                    Err(err) => Err(err),
                    Ok(()) => {
                        let store = self.inner.db.store();
                        let json_val = match value {
                            Value::Text(s) => crate::serde_json::Value::String(s.to_string()),
                            Value::Integer(n) => crate::serde_json::Value::Number(*n as f64),
                            Value::Float(n) => crate::serde_json::Value::Number(*n),
                            Value::Boolean(b) => crate::serde_json::Value::Bool(*b),
                            _ => crate::serde_json::Value::String(value.to_string()),
                        };
                        store.set_config_tree(key, &json_val);
                        update_current_config_value(key, value.clone());
                        // Config changes can flip runtime behavior mid-session
                        // (auto_decrypt, auto_encrypt, etc.) — invalidate the
                        // result cache so subsequent reads re-execute against
                        // the new config.
                        self.invalidate_result_cache();
                        Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("config set: {key}"),
                            "set",
                        ))
                    }
                }
            }
            // SET SECRET key = value
            QueryExpr::SetSecret { ref key, ref value } => {
                if key.starts_with("red.config.") {
                    return Err(RedDBError::Query(
                        "red.config.* is reserved for config; use SET CONFIG".to_string(),
                    ));
                }
                let auth_store = self.inner.auth_store.read().clone().ok_or_else(|| {
                    RedDBError::Query("SET SECRET requires an enabled, unsealed vault".to_string())
                })?;
                if matches!(value, Value::Null) {
                    auth_store
                        .vault_kv_try_delete(key)
                        .map_err(|err| RedDBError::Query(err.to_string()))?;
                    update_current_secret_value(key, None);
                    self.invalidate_result_cache();
                    return Ok(RuntimeQueryResult::ok_message(
                        query.to_string(),
                        &format!("secret deleted: {key}"),
                        "delete_secret",
                    ));
                }
                let value = secret_sql_value_to_string(value)?;
                auth_store
                    .vault_kv_try_set(key.clone(), value.clone())
                    .map_err(|err| RedDBError::Query(err.to_string()))?;
                update_current_secret_value(key, Some(value));
                self.invalidate_result_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("secret set: {key}"),
                    "set_secret",
                ))
            }
            // DELETE SECRET key
            QueryExpr::DeleteSecret { ref key } => {
                let auth_store = self.inner.auth_store.read().clone().ok_or_else(|| {
                    RedDBError::Query(
                        "DELETE SECRET requires an enabled, unsealed vault".to_string(),
                    )
                })?;
                let deleted = auth_store
                    .vault_kv_try_delete(key)
                    .map_err(|err| RedDBError::Query(err.to_string()))?;
                if deleted {
                    update_current_secret_value(key, None);
                }
                self.invalidate_result_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("secret deleted: {key}"),
                    if deleted {
                        "delete_secret"
                    } else {
                        "delete_secret_not_found"
                    },
                ))
            }
            // SHOW SECRET[S] [prefix]
            QueryExpr::ShowSecrets { ref prefix } => {
                let auth_store = self.inner.auth_store.read().clone().ok_or_else(|| {
                    RedDBError::Query("SHOW SECRET requires an enabled, unsealed vault".to_string())
                })?;
                if !auth_store.is_vault_backed() {
                    return Err(RedDBError::Query(
                        "SHOW SECRET requires an enabled, unsealed vault".to_string(),
                    ));
                }
                let mut keys = auth_store.vault_kv_keys();
                keys.sort();
                let mut result = UnifiedResult::with_columns(vec![
                    "key".into(),
                    "value".into(),
                    "status".into(),
                ]);
                for key in keys {
                    if let Some(ref pfx) = prefix {
                        if !key.starts_with(pfx) {
                            continue;
                        }
                    }
                    let mut record = UnifiedRecord::new();
                    record.set("key", Value::text(key));
                    record.set("value", Value::text("***"));
                    record.set("status", Value::text("active"));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement: "show_secrets",
                    engine: "runtime-secret",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                })
            }
            // SHOW CONFIG [prefix] [AS JSON|FORMAT JSON]
            QueryExpr::ShowConfig {
                ref prefix,
                as_json,
            } => {
                let store = self.inner.db.store();
                let all_collections = store.list_collections();
                if !all_collections.contains(&"red_config".to_string()) {
                    if as_json {
                        return Ok(show_config_json_result(
                            query,
                            mode,
                            prefix,
                            crate::serde_json::Value::Object(crate::serde_json::Map::new()),
                        ));
                    }
                    let result = UnifiedResult::with_columns(vec!["key".into(), "value".into()]);
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement: "show_config",
                        engine: "runtime-config",
                        result,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    });
                }
                let manager = store
                    .get_collection("red_config")
                    .ok_or_else(|| RedDBError::NotFound("red_config".to_string()))?;
                let entities = manager.query_all(|_| true);
                let mut latest = std::collections::BTreeMap::<String, (u64, Value, Value)>::new();
                for entity in entities {
                    if let EntityData::Row(ref row) = entity.data {
                        if let Some(ref named) = row.named {
                            let key_val = named.get("key").cloned().unwrap_or(Value::Null);
                            let val = named.get("value").cloned().unwrap_or(Value::Null);
                            let key_str = match &key_val {
                                Value::Text(s) => s.as_ref(),
                                _ => continue,
                            };
                            if let Some(ref pfx) = prefix {
                                if !key_str.starts_with(pfx.as_str()) {
                                    continue;
                                }
                            }
                            let entity_id = entity.id.raw();
                            match latest.get(key_str) {
                                Some((prev_id, _, _)) if *prev_id > entity_id => {}
                                _ => {
                                    latest.insert(key_str.to_string(), (entity_id, key_val, val));
                                }
                            }
                        }
                    }
                }
                if as_json {
                    let mut tree = crate::serde_json::Value::Object(crate::serde_json::Map::new());
                    for (key, (_, _, val)) in latest {
                        let relative = match prefix {
                            Some(pfx) if key == *pfx => "",
                            Some(pfx) => key
                                .strip_prefix(pfx.as_str())
                                .and_then(|tail| tail.strip_prefix('.'))
                                .unwrap_or(key.as_str()),
                            None => key.as_str(),
                        };
                        insert_config_json_path(
                            &mut tree,
                            relative,
                            crate::presentation::entity_json::storage_value_to_json(&val),
                        );
                    }
                    return Ok(show_config_json_result(query, mode, prefix, tree));
                }
                let mut result = UnifiedResult::with_columns(vec!["key".into(), "value".into()]);
                for (_, key_val, val) in latest.into_values() {
                    let mut record = UnifiedRecord::new();
                    record.set("key", key_val);
                    record.set("value", val);
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement: "show_config",
                    engine: "runtime-config",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                })
            }
            // Session-local multi-tenancy handle (Phase 2.5.3).
            //
            // SET TENANT 'id' / SET TENANT NULL / RESET TENANT — writes
            // the thread-local; SHOW TENANT returns it. Paired with the
            // CURRENT_TENANT() scalar for use in RLS policies.
            QueryExpr::SetTenant(ref value) => {
                match value {
                    Some(id) => set_current_tenant(id.clone()),
                    None => clear_current_tenant(),
                }
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &match value {
                        Some(id) => format!("tenant set: {id}"),
                        None => "tenant cleared".to_string(),
                    },
                    "set_tenant",
                ))
            }
            QueryExpr::ShowTenant => {
                let mut result = UnifiedResult::with_columns(vec!["tenant".into()]);
                let mut record = UnifiedRecord::new();
                record.set(
                    "tenant",
                    current_tenant().map(Value::text).unwrap_or(Value::Null),
                );
                result.push(record);
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement: "show_tenant",
                    engine: "runtime-tenant",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                })
            }
            // Transaction control (Phase 2.3 PG parity).
            //
            // BEGIN allocates a real `Xid` and stores a `TxnContext` keyed by
            // the current connection's id. COMMIT/ROLLBACK release it through
            // the `SnapshotManager` so future snapshots see the correct set of
            // active/aborted transactions.
            //
            // Tuple stamping (xmin/xmax) and read-path visibility filtering
            // land in Phase 2.3.2 — this dispatch only manages the snapshot
            // registry. Statements running outside a TxnContext still behave
            // as autocommit (xid=0 → visible to every snapshot).
            QueryExpr::TransactionControl(ref ctl) => {
                use crate::storage::query::ast::TxnControl;
                use crate::storage::transaction::snapshot::{TxnContext, Xid};
                use crate::storage::transaction::IsolationLevel;

                // Phase 2.3 keys transactions by a thread-local connection id.
                // The stdio/gRPC paths wire a real per-connection id later;
                // for embedded use (one RedDBRuntime per process-ish caller)
                // we fall back to a deterministic placeholder.
                let conn_id = current_connection_id();

                let (kind, msg) = match ctl {
                    TxnControl::Begin => {
                        let mgr = Arc::clone(&self.inner.snapshot_manager);
                        let xid = mgr.begin();
                        let snapshot = mgr.snapshot(xid);
                        let ctx = TxnContext {
                            xid,
                            isolation: IsolationLevel::SnapshotIsolation,
                            snapshot,
                            savepoints: Vec::new(),
                            released_sub_xids: Vec::new(),
                        };
                        self.inner.tx_contexts.write().insert(conn_id, ctx);
                        ("begin", format!("BEGIN — xid={xid} (snapshot isolation)"))
                    }
                    TxnControl::Commit => {
                        // SET LOCAL TENANT ends with the transaction.
                        self.inner.tx_local_tenants.write().remove(&conn_id);
                        let ctx = self.inner.tx_contexts.write().remove(&conn_id);
                        match ctx {
                            Some(ctx) => {
                                let mut own_xids = std::collections::HashSet::new();
                                own_xids.insert(ctx.xid);
                                for (_, sub) in &ctx.savepoints {
                                    own_xids.insert(*sub);
                                }
                                for sub in &ctx.released_sub_xids {
                                    own_xids.insert(*sub);
                                }
                                if let Err(err) = self.check_table_row_write_conflicts(
                                    conn_id,
                                    &ctx.snapshot,
                                    &own_xids,
                                ) {
                                    for (_, sub) in &ctx.savepoints {
                                        self.inner.snapshot_manager.rollback(*sub);
                                    }
                                    for sub in &ctx.released_sub_xids {
                                        self.inner.snapshot_manager.rollback(*sub);
                                    }
                                    self.inner.snapshot_manager.rollback(ctx.xid);
                                    self.revive_pending_versioned_updates(conn_id);
                                    self.revive_pending_tombstones(conn_id);
                                    self.discard_pending_kv_watch_events(conn_id);
                                    self.discard_pending_queue_wakes(conn_id);
                                    self.discard_pending_store_wal_actions(conn_id);
                                    return Err(err);
                                }
                                self.restore_pending_write_stamps(conn_id);
                                if let Err(err) = self.flush_pending_store_wal_actions(conn_id) {
                                    for (_, sub) in &ctx.savepoints {
                                        self.inner.snapshot_manager.rollback(*sub);
                                    }
                                    for sub in &ctx.released_sub_xids {
                                        self.inner.snapshot_manager.rollback(*sub);
                                    }
                                    self.inner.snapshot_manager.rollback(ctx.xid);
                                    self.revive_pending_versioned_updates(conn_id);
                                    self.revive_pending_tombstones(conn_id);
                                    self.discard_pending_kv_watch_events(conn_id);
                                    return Err(err);
                                }
                                // Phase 2.3.2e: commit every open sub-xid
                                // so they also become visible. Their
                                // work is promoted to the parent txn's
                                // result exactly like a RELEASE would
                                // have done.
                                for (_, sub) in &ctx.savepoints {
                                    self.inner.snapshot_manager.commit(*sub);
                                }
                                for sub in &ctx.released_sub_xids {
                                    self.inner.snapshot_manager.commit(*sub);
                                }
                                self.inner.snapshot_manager.commit(ctx.xid);
                                self.finalize_pending_versioned_updates(conn_id);
                                self.finalize_pending_tombstones(conn_id);
                                self.finalize_pending_kv_watch_events(conn_id);
                                self.finalize_pending_queue_wakes(conn_id);
                                ("commit", format!("COMMIT — xid={} committed", ctx.xid))
                            }
                            None => (
                                "commit",
                                "COMMIT outside transaction — no-op (autocommit)".to_string(),
                            ),
                        }
                    }
                    TxnControl::Rollback => {
                        self.inner.tx_local_tenants.write().remove(&conn_id);
                        let ctx = self.inner.tx_contexts.write().remove(&conn_id);
                        match ctx {
                            Some(ctx) => {
                                // Phase 2.3.2e: abort every open sub-xid
                                // too so their writes stay hidden.
                                for (_, sub) in &ctx.savepoints {
                                    self.inner.snapshot_manager.rollback(*sub);
                                }
                                for sub in &ctx.released_sub_xids {
                                    self.inner.snapshot_manager.rollback(*sub);
                                }
                                self.inner.snapshot_manager.rollback(ctx.xid);
                                // Phase 2.3.2b: tuples that the txn had
                                // xmax-stamped become live again — wipe xmax
                                // back to 0 so later snapshots see them.
                                self.revive_pending_versioned_updates(conn_id);
                                self.revive_pending_tombstones(conn_id);
                                self.discard_pending_kv_watch_events(conn_id);
                                self.discard_pending_queue_wakes(conn_id);
                                self.discard_pending_store_wal_actions(conn_id);
                                ("rollback", format!("ROLLBACK — xid={} aborted", ctx.xid))
                            }
                            None => (
                                "rollback",
                                "ROLLBACK outside transaction — no-op (autocommit)".to_string(),
                            ),
                        }
                    }
                    // Phase 2.3.2e: savepoints map onto sub-xids. Each
                    // SAVEPOINT allocates a fresh xid and pushes it
                    // onto the per-txn stack so subsequent writes can
                    // be selectively rolled back. RELEASE pops without
                    // aborting; ROLLBACK TO aborts the sub-xid (and
                    // any nested ones) + revives their tombstones.
                    TxnControl::Savepoint(name) => {
                        let mgr = Arc::clone(&self.inner.snapshot_manager);
                        let mut guard = self.inner.tx_contexts.write();
                        match guard.get_mut(&conn_id) {
                            Some(ctx) => {
                                let sub = mgr.begin();
                                ctx.savepoints.push((name.clone(), sub));
                                ("savepoint", format!("SAVEPOINT {name} — sub_xid={sub}"))
                            }
                            None => (
                                "savepoint",
                                "SAVEPOINT outside transaction — no-op".to_string(),
                            ),
                        }
                    }
                    TxnControl::ReleaseSavepoint(name) => {
                        let mut guard = self.inner.tx_contexts.write();
                        match guard.get_mut(&conn_id) {
                            Some(ctx) => {
                                let pos = ctx
                                    .savepoints
                                    .iter()
                                    .position(|(n, _)| n == name)
                                    .ok_or_else(|| {
                                        RedDBError::Internal(format!(
                                            "savepoint {name} does not exist"
                                        ))
                                    })?;
                                // RELEASE pops the named savepoint and
                                // any nested ones. Their sub-xids move
                                // to `released_sub_xids` so they commit
                                // (or roll back) alongside the parent
                                // xid — PG semantics: released
                                // savepoints still contribute their
                                // work, but their names are gone.
                                let released = ctx.savepoints.len() - pos;
                                let popped: Vec<Xid> = ctx
                                    .savepoints
                                    .split_off(pos)
                                    .into_iter()
                                    .map(|(_, x)| x)
                                    .collect();
                                ctx.released_sub_xids.extend(popped);
                                (
                                    "release_savepoint",
                                    format!("RELEASE SAVEPOINT {name} — {released} level(s)"),
                                )
                            }
                            None => (
                                "release_savepoint",
                                "RELEASE outside transaction — no-op".to_string(),
                            ),
                        }
                    }
                    TxnControl::RollbackToSavepoint(name) => {
                        let mgr = Arc::clone(&self.inner.snapshot_manager);
                        // Splice out the savepoint + nested ones under
                        // a narrow lock, then run the snapshot-manager
                        // + tombstone side-effects without the tx map
                        // held so nothing re-enters.
                        let drop_result: Option<(Xid, Vec<Xid>)> = {
                            let mut guard = self.inner.tx_contexts.write();
                            if let Some(ctx) = guard.get_mut(&conn_id) {
                                let pos = ctx
                                    .savepoints
                                    .iter()
                                    .position(|(n, _)| n == name)
                                    .ok_or_else(|| {
                                        RedDBError::Internal(format!(
                                            "savepoint {name} does not exist"
                                        ))
                                    })?;
                                let savepoint_xid = ctx.savepoints[pos].1;
                                let aborted: Vec<Xid> = ctx
                                    .savepoints
                                    .split_off(pos)
                                    .into_iter()
                                    .map(|(_, x)| x)
                                    .collect();
                                Some((savepoint_xid, aborted))
                            } else {
                                None
                            }
                        };

                        match drop_result {
                            Some((savepoint_xid, aborted)) => {
                                for x in &aborted {
                                    mgr.rollback(*x);
                                }
                                let reverted_updates =
                                    self.revive_versioned_updates_since(conn_id, savepoint_xid);
                                let revived = self.revive_tombstones_since(conn_id, savepoint_xid);
                                (
                                    "rollback_to_savepoint",
                                    format!(
                                        "ROLLBACK TO SAVEPOINT {name} — aborted {} sub_xid(s), reverted {reverted_updates} update(s), revived {revived} tombstone(s)",
                                        aborted.len(),
                                    ),
                                )
                            }
                            None => (
                                "rollback_to_savepoint",
                                "ROLLBACK TO outside transaction — no-op".to_string(),
                            ),
                        }
                    }
                };
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &msg,
                    kind,
                ))
            }
            // Schema + Sequence DDL (Phase 1.3 PG parity).
            //
            // Schemas are lightweight logical namespaces: a CREATE SCHEMA call
            // just registers the name in `red_config` under `schema.{name}`.
            // Table lookups still happen by collection name; clients using
            // `schema.table` qualified names collapse to collection `schema.table`.
            //
            // Sequences persist a 64-bit counter + metadata (start, increment)
            // in `red_config` under `sequence.{name}.*`. Scalar callers
            // `nextval('name')` / `currval('name')` arrive with the MVCC phase
            // once we have a proper mutating-function dispatch path; for now the
            // DDL just establishes the catalog entry so clients don't error.
            QueryExpr::CreateSchema(ref q) => {
                let store = self.inner.db.store();
                let key = format!("schema.{}", q.name);
                if store.get_config(&key).is_some() {
                    if q.if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("schema {} already exists — skipped", q.name),
                            "create_schema",
                        ));
                    }
                    return Err(RedDBError::Internal(format!(
                        "schema {} already exists",
                        q.name
                    )));
                }
                store.set_config_tree(&key, &crate::serde_json::Value::Bool(true));
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("schema {} created", q.name),
                    "create_schema",
                ))
            }
            QueryExpr::DropSchema(ref q) => {
                let store = self.inner.db.store();
                let key = format!("schema.{}", q.name);
                let existed = store.get_config(&key).is_some();
                if !existed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "schema {} does not exist",
                        q.name
                    )));
                }
                // Remove marker from red_config via set to null.
                store.set_config_tree(&key, &crate::serde_json::Value::Null);
                let suffix = if q.cascade {
                    " (CASCADE accepted — tables untouched)"
                } else {
                    ""
                };
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("schema {} dropped{}", q.name, suffix),
                    "drop_schema",
                ))
            }
            QueryExpr::CreateSequence(ref q) => {
                let store = self.inner.db.store();
                let base = format!("sequence.{}", q.name);
                let start_key = format!("{base}.start");
                let incr_key = format!("{base}.increment");
                let curr_key = format!("{base}.current");
                if store.get_config(&start_key).is_some() {
                    if q.if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("sequence {} already exists — skipped", q.name),
                            "create_sequence",
                        ));
                    }
                    return Err(RedDBError::Internal(format!(
                        "sequence {} already exists",
                        q.name
                    )));
                }
                // Persist start + increment, and set current so the first
                // nextval returns `start`.
                let initial_current = q.start - q.increment;
                store.set_config_tree(
                    &start_key,
                    &crate::serde_json::Value::Number(q.start as f64),
                );
                store.set_config_tree(
                    &incr_key,
                    &crate::serde_json::Value::Number(q.increment as f64),
                );
                store.set_config_tree(
                    &curr_key,
                    &crate::serde_json::Value::Number(initial_current as f64),
                );
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!(
                        "sequence {} created (start={}, increment={})",
                        q.name, q.start, q.increment
                    ),
                    "create_sequence",
                ))
            }
            QueryExpr::DropSequence(ref q) => {
                let store = self.inner.db.store();
                let base = format!("sequence.{}", q.name);
                let existed = store.get_config(&format!("{base}.start")).is_some();
                if !existed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "sequence {} does not exist",
                        q.name
                    )));
                }
                for k in ["start", "increment", "current"] {
                    store.set_config_tree(&format!("{base}.{k}"), &crate::serde_json::Value::Null);
                }
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("sequence {} dropped", q.name),
                    "drop_sequence",
                ))
            }
            // Views — CREATE [MATERIALIZED] VIEW (Phase 2.1 PG parity).
            //
            // The view definition is stored in-memory on RuntimeInner (not
            // persisted). SELECTs that reference the view name will substitute
            // the stored `QueryExpr` via `resolve_view_reference` during
            // planning (same entry point used by table-name resolution).
            //
            // Materialized views additionally allocate a slot in
            // `MaterializedViewCache`; a REFRESH repopulates that slot.
            QueryExpr::CreateView(ref q) => {
                let mut views = self.inner.views.write();
                if views.contains_key(&q.name) && !q.or_replace {
                    if q.if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("view {} already exists — skipped", q.name),
                            "create_view",
                        ));
                    }
                    return Err(RedDBError::Internal(format!(
                        "view {} already exists",
                        q.name
                    )));
                }
                views.insert(q.name.clone(), Arc::new(q.clone()));
                drop(views);

                // Materialized view: register cache slot (data is empty until REFRESH).
                if q.materialized {
                    use crate::storage::cache::result::{MaterializedViewDef, RefreshPolicy};
                    let refresh = match q.refresh_every_ms {
                        Some(ms) => RefreshPolicy::Periodic(std::time::Duration::from_millis(ms)),
                        None => RefreshPolicy::Manual,
                    };
                    let dependencies = collect_table_refs(&q.query);
                    let def = MaterializedViewDef {
                        name: q.name.clone(),
                        query: format!("<parsed view {}>", q.name),
                        dependencies: dependencies.clone(),
                        refresh,
                        retention_duration_ms: q.retention_duration_ms,
                    };
                    self.inner.materialized_views.write().register(def);

                    // Issue #593 slice 9a — persist the descriptor to
                    // the system catalog so the definition survives a
                    // restart. Upsert semantics (delete-then-insert by
                    // name) keep the catalog free of duplicate rows
                    // across `CREATE OR REPLACE` churn.
                    let descriptor =
                        crate::runtime::continuous_materialized_view::MaterializedViewDescriptor {
                            name: q.name.clone(),
                            source_sql: query.to_string(),
                            source_collections: dependencies,
                            refresh_every_ms: q.refresh_every_ms,
                            retention_duration_ms: q.retention_duration_ms,
                        };
                    let store = self.inner.db.store();
                    crate::runtime::continuous_materialized_view::persist_descriptor(
                        store.as_ref(),
                        &descriptor,
                    )?;

                    // Issue #594 slice 9b — provision a Table-shaped
                    // backing collection named after the view. The
                    // rewriter skips materialized views (see
                    // `rewrite_view_refs_inner`) so `SELECT FROM v`
                    // resolves to this collection directly. Empty
                    // until REFRESH wires through it in 9c.
                    self.ensure_materialized_view_backing(&q.name)?;
                }
                // Plan cache may have cached a plan that didn't know about this
                // view — invalidate so future references pick up the new binding.
                // Result cache gets flushed too: OR REPLACE must not serve a
                // prior execution of the obsolete body.
                self.invalidate_plan_cache();
                self.invalidate_result_cache();

                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!(
                        "{}view {} created",
                        if q.materialized { "materialized " } else { "" },
                        q.name
                    ),
                    "create_view",
                ))
            }
            QueryExpr::DropView(ref q) => {
                let mut views = self.inner.views.write();
                let removed = views.remove(&q.name);
                let existed = removed.is_some();
                let removed_materialized =
                    removed.as_ref().map(|v| v.materialized).unwrap_or(false);
                drop(views);
                if q.materialized || existed {
                    // Try the materialised cache too — silent if absent.
                    self.inner.materialized_views.write().remove(&q.name);
                    // Issue #593 slice 9a — remove any persisted
                    // catalog row. Idempotent: a no-op when the view
                    // was never materialized (no row was ever written).
                    let store = self.inner.db.store();
                    crate::runtime::continuous_materialized_view::remove_by_name(
                        store.as_ref(),
                        &q.name,
                    )?;
                }
                // Issue #594 slice 9b — drop the backing collection
                // that was provisioned at CREATE time. Only mat views
                // ever had one; regular views never did.
                if removed_materialized || q.materialized {
                    self.drop_materialized_view_backing(&q.name)?;
                }
                // Drop any plan / result cache entries that baked the
                // view body into their QueryExpr.
                self.invalidate_plan_cache();
                self.invalidate_result_cache();
                if !existed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "view {} does not exist",
                        q.name
                    )));
                }
                self.invalidate_plan_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("view {} dropped", q.name),
                    "drop_view",
                ))
            }
            QueryExpr::RefreshMaterializedView(ref q) => {
                // Look up the view definition, execute its underlying query,
                // and stash the serialized result in the materialised cache.
                let view = {
                    let views = self.inner.views.read();
                    views.get(&q.name).cloned()
                };
                let view = match view {
                    Some(v) => v,
                    None => {
                        return Err(RedDBError::Internal(format!(
                            "view {} does not exist",
                            q.name
                        )))
                    }
                };
                if !view.materialized {
                    return Err(RedDBError::Internal(format!(
                        "view {} is not materialized — REFRESH requires \
                         CREATE MATERIALIZED VIEW",
                        q.name
                    )));
                }
                // Execute the underlying query fresh.
                let started = std::time::Instant::now();
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                match self.execute_query_expr((*view.query).clone()) {
                    Ok(inner_result) => {
                        // Issue #595 slice 9c — atomically replace the
                        // backing collection's contents under a single
                        // WAL group. Concurrent SELECT from the view
                        // sees either the prior or new contents, never
                        // partial. A crash before the WAL commit lands
                        // leaves the prior contents intact on recovery.
                        let entities =
                            view_records_to_entities(&q.name, &inner_result.result.records);
                        let row_count = entities.len() as u64;
                        let store = self.inner.db.store();
                        let serialized_records = match store.refresh_collection(&q.name, entities) {
                            Ok(records) => records,
                            Err(err) => {
                                let duration_ms = started.elapsed().as_millis() as u64;
                                let msg = err.to_string();
                                self.inner
                                    .materialized_views
                                    .write()
                                    .record_refresh_failure(
                                        &q.name,
                                        msg.clone(),
                                        duration_ms,
                                        now_ms,
                                    );
                                return Err(RedDBError::Internal(format!(
                                    "REFRESH MATERIALIZED VIEW {}: {msg}",
                                    q.name
                                )));
                            }
                        };

                        // Issue #596 slice 9d — emit a Refresh
                        // ChangeRecord into the logical-WAL spool so
                        // replicas deterministically replay the same
                        // backing-collection contents via
                        // `LogicalChangeApplier::apply_record`.
                        if let Some(ref primary) = self.inner.db.replication {
                            let lsn = self.inner.cdc.emit(
                                crate::replication::cdc::ChangeOperation::Refresh,
                                &q.name,
                                0,
                                "refresh",
                            );
                            self.invalidate_result_cache_for_table(&q.name);
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;
                            let record = ChangeRecord::for_refresh(
                                lsn,
                                timestamp,
                                q.name.clone(),
                                serialized_records,
                            )
                            .with_term(self.current_replication_term());
                            let encoded = record.encode();
                            primary.append_logical_record(record.lsn, encoded);
                        }

                        let duration_ms = started.elapsed().as_millis() as u64;
                        let serialized = format!("{:?}", inner_result.result);
                        self.inner
                            .materialized_views
                            .write()
                            .record_refresh_success(
                                &q.name,
                                serialized.into_bytes(),
                                row_count,
                                duration_ms,
                                now_ms,
                            );
                        // SELECT FROM v now reads through the rewriter
                        // skip into the backing collection — drop the
                        // result cache so prior empty-backing reads
                        // don't shadow the new contents.
                        self.invalidate_result_cache();
                        Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("materialized view {} refreshed", q.name),
                            "refresh_materialized_view",
                        ))
                    }
                    Err(err) => {
                        let duration_ms = started.elapsed().as_millis() as u64;
                        let msg = err.to_string();
                        self.inner
                            .materialized_views
                            .write()
                            .record_refresh_failure(&q.name, msg.clone(), duration_ms, now_ms);
                        Err(err)
                    }
                }
            }
            // Row Level Security (Phase 2.5 PG parity).
            //
            // Policies live in an in-memory registry keyed by (table, name).
            // Enforcement (AND-ing the policy's USING clause into every
            // query's WHERE for the table) arrives in Phase 2.5.2 via the
            // filter compiler; this dispatch only manages the catalog.
            QueryExpr::CreatePolicy(ref q) => {
                let key = (q.table.clone(), q.name.clone());
                self.inner
                    .rls_policies
                    .write()
                    .insert(key, Arc::new(q.clone()));
                self.invalidate_plan_cache();
                // Issue #120 — surface policy names in the
                // schema-vocabulary so AskPipeline (#121) can resolve
                // a policy reference back to its table.
                self.schema_vocabulary_apply(
                    crate::runtime::schema_vocabulary::DdlEvent::CreatePolicy {
                        collection: q.table.clone(),
                        policy: q.name.clone(),
                    },
                );
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("policy {} on {} created", q.name, q.table),
                    "create_policy",
                ))
            }
            QueryExpr::DropPolicy(ref q) => {
                let removed = self
                    .inner
                    .rls_policies
                    .write()
                    .remove(&(q.table.clone(), q.name.clone()))
                    .is_some();
                if !removed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "policy {} on {} does not exist",
                        q.name, q.table
                    )));
                }
                self.invalidate_plan_cache();
                // Issue #120 — keep the schema-vocabulary policy
                // entry in sync.
                self.schema_vocabulary_apply(
                    crate::runtime::schema_vocabulary::DdlEvent::DropPolicy {
                        collection: q.table.clone(),
                        policy: q.name.clone(),
                    },
                );
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("policy {} on {} dropped", q.name, q.table),
                    "drop_policy",
                ))
            }
            // Foreign Data Wrappers (Phase 3.2 PG parity).
            //
            // CREATE SERVER / CREATE FOREIGN TABLE register into the shared
            // `ForeignTableRegistry`. The read path consults that registry
            // before dispatching a SELECT — when the table name matches a
            // registered foreign table, we forward the scan to the wrapper
            // and skip the normal collection lookup.
            //
            // Phase 3.2 is in-memory only; persistence across restarts is a
            // 3.2.2 follow-up that mirrors the view registry pattern.
            QueryExpr::CreateServer(ref q) => {
                use crate::storage::fdw::FdwOptions;
                let registry = Arc::clone(&self.inner.foreign_tables);
                if registry.server(&q.name).is_some() {
                    if q.if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("server {} already exists — skipped", q.name),
                            "create_server",
                        ));
                    }
                    return Err(RedDBError::Internal(format!(
                        "server {} already exists",
                        q.name
                    )));
                }
                let mut opts = FdwOptions::new();
                for (k, v) in &q.options {
                    opts.values.insert(k.clone(), v.clone());
                }
                registry
                    .create_server(&q.name, &q.wrapper, opts)
                    .map_err(|e| RedDBError::Internal(e.to_string()))?;
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("server {} created (wrapper {})", q.name, q.wrapper),
                    "create_server",
                ))
            }
            QueryExpr::DropServer(ref q) => {
                let existed = self.inner.foreign_tables.drop_server(&q.name);
                if !existed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "server {} does not exist",
                        q.name
                    )));
                }
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!(
                        "server {} dropped{}",
                        q.name,
                        if q.cascade { " (cascade)" } else { "" }
                    ),
                    "drop_server",
                ))
            }
            QueryExpr::CreateForeignTable(ref q) => {
                use crate::storage::fdw::{FdwOptions, ForeignColumn, ForeignTable};
                let registry = Arc::clone(&self.inner.foreign_tables);
                if registry.foreign_table(&q.name).is_some() {
                    if q.if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            query.to_string(),
                            &format!("foreign table {} already exists — skipped", q.name),
                            "create_foreign_table",
                        ));
                    }
                    return Err(RedDBError::Internal(format!(
                        "foreign table {} already exists",
                        q.name
                    )));
                }
                let mut opts = FdwOptions::new();
                for (k, v) in &q.options {
                    opts.values.insert(k.clone(), v.clone());
                }
                let columns: Vec<ForeignColumn> = q
                    .columns
                    .iter()
                    .map(|c| ForeignColumn {
                        name: c.name.clone(),
                        data_type: c.data_type.clone(),
                        not_null: c.not_null,
                    })
                    .collect();
                registry
                    .create_foreign_table(ForeignTable {
                        name: q.name.clone(),
                        server_name: q.server.clone(),
                        columns,
                        options: opts,
                    })
                    .map_err(|e| RedDBError::Internal(e.to_string()))?;
                self.invalidate_plan_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("foreign table {} created (server {})", q.name, q.server),
                    "create_foreign_table",
                ))
            }
            QueryExpr::DropForeignTable(ref q) => {
                let existed = self.inner.foreign_tables.drop_foreign_table(&q.name);
                if !existed && !q.if_exists {
                    return Err(RedDBError::Internal(format!(
                        "foreign table {} does not exist",
                        q.name
                    )));
                }
                self.invalidate_plan_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("foreign table {} dropped", q.name),
                    "drop_foreign_table",
                ))
            }
            // COPY table FROM 'path' (Phase 1.5 PG parity).
            //
            // Stream CSV rows through the shared `CsvImporter`. The collection
            // is auto-created on first insert (via `insert_auto`-style path);
            // VACUUM/ANALYZE afterwards is up to the caller.
            QueryExpr::CopyFrom(ref q) => {
                use crate::storage::import::{CsvConfig, CsvImporter};
                let store = self.inner.db.store();
                let cfg = CsvConfig {
                    collection: q.table.clone(),
                    has_header: q.has_header,
                    delimiter: q.delimiter.map(|c| c as u8).unwrap_or(b','),
                    ..CsvConfig::default()
                };
                let importer = CsvImporter::new(cfg);
                let stats = importer
                    .import_file(&q.path, store.as_ref())
                    .map_err(|e| RedDBError::Internal(format!("COPY failed: {e}")))?;
                // Tables are written → invalidate cached plans / result cache.
                self.note_table_write(&q.table);
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!(
                        "COPY imported {} rows into {} ({} errors skipped, {}ms)",
                        stats.records_imported, q.table, stats.errors_skipped, stats.duration_ms
                    ),
                    "copy_from",
                ))
            }
            // Maintenance commands (Phase 1.2 PG parity).
            //
            // - VACUUM [FULL] [table]: refreshes planner stats for the target
            //   collection(s) and — when FULL — triggers a full pager persist
            //   (flushes dirty pages + fsync). Also invalidates the result cache
            //   so subsequent reads re-execute against the freshly compacted
            //   storage. RedDB's segment/btree GC runs continuously via the
            //   background lifecycle; explicit space reclamation for sealed
            //   segments arrives with Phase 2.3 (MVCC + dead-tuple reclamation).
            // - ANALYZE [table]: reruns `analyze_collection` +
            //   `persist_table_stats` via `refresh_table_planner_stats` so the
            //   planner has fresh histograms, distinct estimates, null counts.
            //
            // Both commands accept an optional target; omitting the target
            // iterates every collection in the store.
            QueryExpr::MaintenanceCommand(ref cmd) => {
                use crate::storage::query::ast::MaintenanceCommand as Mc;
                let store = self.inner.db.store();
                let (kind, msg) = match cmd {
                    Mc::Analyze { target } => {
                        let targets: Vec<String> = match target {
                            Some(t) => vec![t.clone()],
                            None => store.list_collections(),
                        };
                        for t in &targets {
                            self.refresh_table_planner_stats(t);
                        }
                        (
                            "analyze",
                            format!("ANALYZE refreshed stats for {} table(s)", targets.len()),
                        )
                    }
                    Mc::Vacuum { target, full } => {
                        let targets: Vec<String> = match target {
                            Some(t) => vec![t.clone()],
                            None => store.list_collections(),
                        };
                        let cutoff_xid = self.mvcc_vacuum_cutoff_xid();
                        let mut vacuum_stats =
                            crate::storage::unified::store::MvccVacuumStats::default();
                        for t in &targets {
                            let stats = store.vacuum_mvcc_history(t, cutoff_xid).map_err(|e| {
                                RedDBError::Internal(format!(
                                    "VACUUM MVCC history failed for {t}: {e}"
                                ))
                            })?;
                            if stats.reclaimed_versions > 0 {
                                self.rebuild_runtime_indexes_for_table(t)?;
                            }
                            vacuum_stats.add(&stats);
                        }
                        self.inner.snapshot_manager.prune_aborted(cutoff_xid);
                        // Stats refresh covers every target (same as ANALYZE).
                        for t in &targets {
                            self.refresh_table_planner_stats(t);
                        }
                        // FULL forces a pager persist (dirty-page flush + fsync).
                        // Regular VACUUM relies on the background writer / segment
                        // lifecycle so the command is non-blocking.
                        let persisted = if *full {
                            match store.persist() {
                                Ok(()) => true,
                                Err(e) => {
                                    return Err(RedDBError::Internal(format!(
                                        "VACUUM FULL persist failed: {e:?}"
                                    )));
                                }
                            }
                        } else {
                            false
                        };
                        // Result cache depended on pre-vacuum state.
                        self.invalidate_result_cache();
                        (
                            "vacuum",
                            format!(
                                "VACUUM{} processed {} table(s): scanned_versions={}, retained_versions={}, reclaimed_versions={}, retained_history_versions={}, reclaimed_history_versions={}, retained_tombstones={}, reclaimed_tombstones={}{}",
                                if *full { " FULL" } else { "" },
                                targets.len(),
                                vacuum_stats.scanned_versions,
                                vacuum_stats.retained_versions,
                                vacuum_stats.reclaimed_versions,
                                vacuum_stats.retained_history_versions,
                                vacuum_stats.reclaimed_history_versions,
                                vacuum_stats.retained_tombstones,
                                vacuum_stats.reclaimed_tombstones,
                                if persisted {
                                    " (pages flushed to disk)"
                                } else {
                                    ""
                                }
                            ),
                        )
                    }
                };
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &msg,
                    kind,
                ))
            }
            // GRANT / REVOKE / ALTER USER (RBAC milestone).
            //
            // These hit the AuthStore directly. The statement frame /
            // privilege gate has already decided whether the caller may
            // even run the statement; here we just translate the AST into
            // AuthStore calls.
            QueryExpr::Grant(ref g) => self.execute_grant_statement(query, g),
            QueryExpr::Revoke(ref r) => self.execute_revoke_statement(query, r),
            QueryExpr::AlterUser(ref a) => self.execute_alter_user_statement(query, a),
            QueryExpr::CreateUser(ref u) => self.execute_create_user_statement(query, u),
            QueryExpr::CreateIamPolicy { ref id, ref json } => {
                self.execute_create_iam_policy(query, id, json)
            }
            QueryExpr::DropIamPolicy { ref id } => self.execute_drop_iam_policy(query, id),
            QueryExpr::AttachPolicy {
                ref policy_id,
                ref principal,
            } => self.execute_attach_policy(query, policy_id, principal),
            QueryExpr::DetachPolicy {
                ref policy_id,
                ref principal,
            } => self.execute_detach_policy(query, policy_id, principal),
            QueryExpr::ShowPolicies { ref filter } => {
                self.execute_show_policies(query, filter.as_ref())
            }
            QueryExpr::ShowEffectivePermissions {
                ref user,
                ref resource,
            } => self.execute_show_effective_permissions(query, user, resource.as_ref()),
            QueryExpr::SimulatePolicy {
                ref user,
                ref action,
                ref resource,
            } => self.execute_simulate_policy(query, user, action, resource),
            QueryExpr::LintPolicy { ref source } => self.execute_lint_policy(query, source),
            QueryExpr::MigratePolicyMode {
                ref target,
                dry_run,
            } => self.execute_migrate_policy_mode(query, target, dry_run),
            QueryExpr::CreateMigration(ref q) => self.execute_create_migration(query, q),
            QueryExpr::ApplyMigration(ref q) => self.execute_apply_migration(query, q),
            QueryExpr::RollbackMigration(ref q) => self.execute_rollback_migration(query, q),
            QueryExpr::ExplainMigration(ref q) => self.execute_explain_migration(query, q),
        };

        if !control_event_specs.is_empty() {
            let (outcome, reason) = match &query_result {
                Ok(_) => (crate::runtime::control_events::Outcome::Allowed, None),
                Err(err) => (control_event_outcome_for_error(err), Some(err.to_string())),
            };
            for spec in &control_event_specs {
                self.emit_control_event(
                    spec.kind,
                    outcome,
                    spec.action,
                    spec.resource.clone(),
                    reason.clone(),
                    spec.fields.clone(),
                )?;
            }
        }

        if let (Some(plan), Ok(result)) = (&query_audit_plan, &query_result) {
            self.emit_query_audit(
                query,
                plan,
                query_audit_started.elapsed().as_millis() as u64,
                result,
            );
        }

        // Decrypt Value::Secret columns in-place before caching, so
        // cached results match the post-decrypt shape and repeat
        // queries skip the per-row AES-GCM pass.
        let mut query_result = query_result;
        if let Ok(ref mut result) = query_result {
            if result.statement_type == "select" {
                self.apply_secret_decryption(result);
            }
        }

        // Cache SELECT results for 30s.
        // Skip: pre-serialized JSON (large clone), and result sets > 5 rows.
        // Large multi-row results (range scans, filtered scans) are rarely
        // repeated with the same literal values so the cache hit rate is near
        // zero while the clone cost (100 records × ~16 fields each) is high.
        // Aggregations (1 row) and point lookups (1 row) still benefit.
        if let Ok(ref result) = query_result {
            frame.write_result_cache(self, result, result_cache_scopes);
        }

        query_result
    }

    /// Snapshot of every registered materialized view's runtime
    /// state — feeds the `red.materialized_views` virtual table.
    /// Issue #583 slice 10.
    pub fn materialized_view_metadata(
        &self,
    ) -> Vec<crate::storage::cache::result::MaterializedViewMetadata> {
        // Issue #595 slice 9c — `current_row_count` is now scraped
        // live from the backing collection rather than read from the
        // cache slot. Mirrors the slice-10 invariant on
        // `queue_pending_gauge` in #527: the live store is the source
        // of truth, the cache slot only carries last-refresh telemetry
        // (timing, error, refresh cadence).
        let store = self.inner.db.store();
        let mut entries = self.inner.materialized_views.read().metadata();
        for entry in &mut entries {
            if let Some(manager) = store.get_collection(&entry.name) {
                entry.current_row_count = manager.count() as u64;
            }
        }
        entries
    }

    /// Drive scheduled refreshes for materialized views with a
    /// `REFRESH EVERY <duration>` clause. Called from the background
    /// scheduler thread (and from unit tests with a fake clock via
    /// `claim_due_at`). Each invocation atomically claims the set of
    /// due views (so two concurrent ticks never double-fire the same
    /// view) and runs each refresh through the standard execution
    /// path — failures are captured in `last_error` and the prior
    /// content stays intact. Issue #583 slice 10.
    /// Snapshot of every tracked retention sweeper state — feeds the
    /// three extra columns on `red.retention`. Issue #584 slice 12.
    pub(crate) fn retention_sweeper_snapshot(
        &self,
    ) -> Vec<(String, crate::runtime::retention_sweeper::SweeperState)> {
        self.inner.retention_sweeper.read().snapshot()
    }

    /// Drive one tick of the retention sweeper. Iterates collections
    /// with a retention policy set, physically deletes at most
    /// `batch_size` expired rows per collection, and records the
    /// `last_sweep_at_ms` / `rows_swept_total` / pending estimate that
    /// `red.retention` exposes. Called from the background sweeper
    /// thread; safe to invoke directly from tests with a small batch
    /// size to drain rows deterministically. Issue #584 slice 12.
    ///
    /// Deletes are issued as `DELETE FROM <collection> WHERE
    /// <ts_column> < <cutoff>` through the standard `execute_query`
    /// chokepoint so WAL participation and snapshot guards apply
    /// exactly as for a user-issued DELETE — replicas replay the
    /// sweeper's deletes via the same WAL stream with no special
    /// handling on the replication side.
    ///
    /// Batching is enforced by tightening the cutoff: if more than
    /// `batch_size` rows are expired, the cutoff is dropped to the
    /// `batch_size`-th oldest expired timestamp + 1 so the predicate
    /// matches roughly `batch_size` rows; the remainder is reported
    /// as `current_rows_pending_sweep_estimate` and drained on the
    /// next tick.
    pub fn sweep_retention_tick(&self, batch_size: usize) {
        if batch_size == 0 {
            return;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let store = self.inner.db.store();
        let collections = store.list_collections();
        for name in collections {
            let Some(contract) = self.inner.db.collection_contract(&name) else {
                continue;
            };
            let Some(retention_ms) = contract.retention_duration_ms else {
                continue;
            };
            let Some(ts_column) =
                crate::runtime::retention_filter::resolve_timestamp_column(&contract)
            else {
                continue;
            };
            let Some(manager) = store.get_collection(&name) else {
                continue;
            };
            let cutoff = (now_ms as i64).saturating_sub(retention_ms as i64);

            // Single pass: collect expired timestamps. We keep the
            // full Vec rather than a bounded heap because the partial
            // sort below is the simplest correct way to find the
            // batch-th oldest; for the slice's "1000-row default
            // batch" target this is bounded enough for production
            // operation, and the alternative (in-place heap of size
            // batch+1) is a follow-up optimisation.
            let mut expired_ts: Vec<i64> = Vec::new();
            manager.for_each_entity(|entity| {
                let ts = match ts_column.as_str() {
                    "created_at" => Some(entity.created_at as i64),
                    "updated_at" => Some(entity.updated_at as i64),
                    other => entity
                        .data
                        .as_row()
                        .and_then(|row| row.get_field(other))
                        .and_then(|v| match v {
                            crate::storage::schema::Value::TimestampMs(t) => Some(*t),
                            crate::storage::schema::Value::Timestamp(t) => {
                                Some(t.saturating_mul(1_000))
                            }
                            crate::storage::schema::Value::BigInt(t) => Some(*t),
                            crate::storage::schema::Value::UnsignedInteger(t) => {
                                i64::try_from(*t).ok()
                            }
                            crate::storage::schema::Value::Integer(t) => Some(*t),
                            _ => None,
                        }),
                };
                if let Some(t) = ts {
                    if t < cutoff {
                        expired_ts.push(t);
                    }
                }
                true
            });

            let total_expired = expired_ts.len() as u64;
            if total_expired == 0 {
                self.inner
                    .retention_sweeper
                    .write()
                    .record_tick(&name, 0, 0, now_ms);
                continue;
            }

            let (effective_cutoff, pending) = if (total_expired as usize) <= batch_size {
                (cutoff, 0u64)
            } else {
                // Tighten the cutoff to the (batch_size)-th oldest
                // expired timestamp + 1 so DELETE matches roughly
                // `batch_size` rows.
                expired_ts.sort_unstable();
                let nth = expired_ts[batch_size - 1];
                (
                    nth.saturating_add(1),
                    total_expired.saturating_sub(batch_size as u64),
                )
            };

            let stmt = format!(
                "DELETE FROM {} WHERE {} < {}",
                name, ts_column, effective_cutoff
            );
            let deleted = match self.execute_query(&stmt) {
                Ok(r) => r.affected_rows,
                Err(_) => 0,
            };

            self.inner
                .retention_sweeper
                .write()
                .record_tick(&name, deleted, pending, now_ms);
        }
    }

    pub fn refresh_due_materialized_views(&self) {
        let due = {
            let mut cache = self.inner.materialized_views.write();
            cache.claim_due_at(std::time::Instant::now())
        };
        for name in due {
            // Round-trip through `execute_query` (rather than the
            // prepared-statement `execute_query_expr` fast path, which
            // explicitly rejects DDL/maintenance statements). Failures
            // are captured inside the RefreshMaterializedView handler
            // via `record_refresh_failure`; the scheduler ignores the
            // Result so one bad view doesn't halt the loop.
            let stmt = format!("REFRESH MATERIALIZED VIEW {}", name);
            let _ = self.execute_query(&stmt);
        }
    }

    /// Execute a pre-parsed `QueryExpr` directly, bypassing SQL parsing and the
    /// plan cache. Used by the prepared-statement fast path so that `execute_prepared`
    /// calls pay zero parse + cache overhead.
    ///
    /// Applies secret decryption on SELECT results, identical to `execute_query`.
    pub fn execute_query_expr(&self, expr: QueryExpr) -> RedDBResult<RuntimeQueryResult> {
        let _config_snapshot_guard = ConfigSnapshotGuard::install(Arc::clone(&self.inner.db));
        let _secret_store_guard = SecretStoreGuard::install(self.inner.auth_store.read().clone());
        // View rewrite (Phase 2.1): substitute any `QueryExpr::Table(tq)`
        // whose `tq.table` matches a registered view with the view's
        // underlying query. Safe to call even when no views are registered.
        let expr = self.rewrite_view_refs(expr);

        self.validate_model_operations_before_auth(&expr)?;
        // Granular RBAC privilege check. Runs before dispatch so a
        // denied caller never reaches storage. Fail-closed: any error
        // resolving the action / resource produces PermissionDenied.
        if let Err(err) = self.check_query_privilege(&expr) {
            return Err(RedDBError::Query(format!("permission denied: {err}")));
        }

        let statement = query_expr_name(&expr);
        let mode = detect_mode(statement);
        let query_str = statement;

        let result = self.dispatch_expr(expr, query_str, mode)?;
        let mut r = result;
        if r.statement_type == "select" {
            self.apply_secret_decryption(&mut r);
        }
        Ok(r)
    }

    pub(super) fn validate_model_operations_before_auth(
        &self,
        expr: &QueryExpr,
    ) -> RedDBResult<()> {
        use crate::catalog::CollectionModel;
        use crate::runtime::ddl::polymorphic_resolver;
        use crate::storage::query::ast::KvCommand;

        let system_schema_target = match expr {
            QueryExpr::DropTable(q) => Some(q.name.as_str()),
            QueryExpr::DropGraph(q) => Some(q.name.as_str()),
            QueryExpr::DropVector(q) => Some(q.name.as_str()),
            QueryExpr::DropDocument(q) => Some(q.name.as_str()),
            QueryExpr::DropKv(q) => Some(q.name.as_str()),
            QueryExpr::DropCollection(q) => Some(q.name.as_str()),
            QueryExpr::Truncate(q) => Some(q.name.as_str()),
            _ => None,
        };
        if system_schema_target.is_some_and(crate::runtime::impl_ddl::is_system_schema_name) {
            return Err(RedDBError::Query("system schema is read-only".to_string()));
        }

        let expected = match expr {
            QueryExpr::DropTable(q) => Some((q.name.as_str(), CollectionModel::Table)),
            QueryExpr::DropGraph(q) => Some((q.name.as_str(), CollectionModel::Graph)),
            QueryExpr::DropVector(q) => Some((q.name.as_str(), CollectionModel::Vector)),
            QueryExpr::DropDocument(q) => Some((q.name.as_str(), CollectionModel::Document)),
            QueryExpr::DropKv(q) => Some((q.name.as_str(), q.model)),
            QueryExpr::DropCollection(q) => q.model.map(|model| (q.name.as_str(), model)),
            QueryExpr::Truncate(q) => q.model.map(|model| (q.name.as_str(), model)),
            QueryExpr::KvCommand(cmd) => {
                let (collection, model) = match cmd {
                    KvCommand::Put {
                        collection, model, ..
                    }
                    | KvCommand::Get {
                        collection, model, ..
                    }
                    | KvCommand::Incr {
                        collection, model, ..
                    }
                    | KvCommand::Cas {
                        collection, model, ..
                    }
                    | KvCommand::List {
                        collection, model, ..
                    }
                    | KvCommand::Delete {
                        collection, model, ..
                    } => (collection.as_str(), *model),
                    KvCommand::Rotate { collection, .. }
                    | KvCommand::History { collection, .. }
                    | KvCommand::Purge { collection, .. } => {
                        (collection.as_str(), CollectionModel::Vault)
                    }
                    KvCommand::InvalidateTags { collection, .. } => {
                        (collection.as_str(), CollectionModel::Kv)
                    }
                    KvCommand::Watch {
                        collection, model, ..
                    } => (collection.as_str(), *model),
                    KvCommand::Unseal { collection, .. } => {
                        (collection.as_str(), CollectionModel::Vault)
                    }
                };
                Some((collection, model))
            }
            QueryExpr::ConfigCommand(cmd) => {
                self.validate_config_command_before_auth(cmd)?;
                None
            }
            _ => None,
        };

        let Some((name, expected_model)) = expected else {
            return Ok(());
        };
        let snapshot = self.inner.db.catalog_model_snapshot();
        let Some(actual_model) = snapshot
            .collections
            .iter()
            .find(|collection| collection.name == name)
            .map(|collection| collection.declared_model.unwrap_or(collection.model))
        else {
            return Ok(());
        };
        polymorphic_resolver::ensure_model_match(expected_model, actual_model)
    }

    /// Walk a `QueryExpr` and replace `QueryExpr::Table(tq)` nodes whose
    /// `tq.table` matches a registered view name with the view's stored
    /// body. Recurses through joins so `SELECT ... FROM t JOIN myview ...`
    /// resolves correctly. Pure operation — no side effects.
    pub(super) fn rewrite_view_refs(&self, expr: QueryExpr) -> QueryExpr {
        // Fast path: no views registered → return original expression.
        if self.inner.views.read().is_empty() {
            return expr;
        }
        self.rewrite_view_refs_inner(expr)
    }

    fn rewrite_view_refs_inner(&self, expr: QueryExpr) -> QueryExpr {
        use crate::storage::query::ast::{Filter, TableSource};
        match expr {
            QueryExpr::Table(mut tq) => {
                // 1. If the TableSource is a subquery, recurse into it so
                //    `SELECT ... FROM (SELECT ... FROM myview) t` expands.
                //    The legacy `table` field (set to a synthetic
                //    "__subq_NNNN" sentinel) stays as-is so callers that
                //    read it keep compiling.
                if let Some(TableSource::Subquery(body)) = tq.source.take() {
                    tq.source = Some(TableSource::Subquery(Box::new(
                        self.rewrite_view_refs_inner(*body),
                    )));
                    return QueryExpr::Table(tq);
                }

                // 2. Restore the source field (took it above for match).
                // When the source was `None` or `TableSource::Name(_)`, the
                // real lookup key is `tq.table` — check the view registry.
                let maybe_view = {
                    let views = self.inner.views.read();
                    views.get(&tq.table).cloned()
                };
                let Some(view) = maybe_view else {
                    return QueryExpr::Table(tq);
                };

                // Issue #594 slice 9b — materialized views are read
                // from their backing collection, not by substituting
                // the body. Returning the TableQuery as-is lets the
                // normal table-read path resolve `SELECT FROM v`
                // against the collection provisioned at CREATE time.
                if view.materialized {
                    return QueryExpr::Table(tq);
                }

                // Recurse into the view body — views may reference other
                // views. The recursion yields the final QueryExpr we need
                // to merge the outer's filter / limit / offset into.
                let inner_expr = self.rewrite_view_refs_inner((*view.query).clone());

                // Phase 5: when the body is a Table we merge the outer
                // TableQuery's WHERE / LIMIT / OFFSET into it so stacked
                // views filter recursively. Non-table bodies (Search,
                // Ask, Vector, Graph, Hybrid) can't meaningfully combine
                // with an outer Table query today — return the body
                // verbatim; outer predicates are lost. Full projection
                // merge lands in Phase 5.2.
                match inner_expr {
                    QueryExpr::Table(mut inner_tq) => {
                        if let Some(outer_filter) = tq.filter.take() {
                            inner_tq.filter = Some(match inner_tq.filter.take() {
                                Some(existing) => {
                                    Filter::And(Box::new(existing), Box::new(outer_filter))
                                }
                                None => outer_filter,
                            });
                            // Keep the `Expr` form in lock-step with the
                            // merged `Filter`. The executor prefers
                            // `where_expr` and nulls `filter` when it is
                            // present (see `execute_query_inner`), so a
                            // stacked view whose outer predicate was only
                            // merged into `filter` would silently drop that
                            // predicate at eval time (#635).
                            inner_tq.where_expr = inner_tq
                                .filter
                                .as_ref()
                                .map(crate::storage::query::sql_lowering::filter_to_expr);
                        }
                        if let Some(outer_limit) = tq.limit {
                            inner_tq.limit = Some(match inner_tq.limit {
                                Some(existing) => existing.min(outer_limit),
                                None => outer_limit,
                            });
                        }
                        if let Some(outer_offset) = tq.offset {
                            inner_tq.offset = Some(match inner_tq.offset {
                                Some(existing) => existing + outer_offset,
                                None => outer_offset,
                            });
                        }
                        QueryExpr::Table(inner_tq)
                    }
                    other => other,
                }
            }
            QueryExpr::Join(mut jq) => {
                jq.left = Box::new(self.rewrite_view_refs_inner(*jq.left));
                jq.right = Box::new(self.rewrite_view_refs_inner(*jq.right));
                QueryExpr::Join(jq)
            }
            // Other variants don't carry nested QueryExpr that can reference
            // a view by table name. Return as-is.
            other => other,
        }
    }

    /// Apply table-level read authorization and RLS rewriting for a
    /// relational SELECT leaf.
    fn authorize_relational_table_select(
        &self,
        mut table: TableQuery,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<Option<TableQuery>> {
        if let Some(TableSource::Subquery(inner)) = table.source.take() {
            let authorized_inner = self.authorize_relational_select_expr(*inner, frame)?;
            table.source = Some(TableSource::Subquery(Box::new(authorized_inner)));
            return Ok(Some(table));
        }

        self.check_table_column_projection_authz(&table, frame)?;

        if self.inner.rls_enabled_tables.read().contains(&table.table) {
            return Ok(inject_rls_filters(self, frame, table));
        }

        Ok(Some(table))
    }

    fn authorize_relational_join_select(
        &self,
        mut join: JoinQuery,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<Option<JoinQuery>> {
        self.check_join_column_projection_authz(&join, frame)?;
        join.left = Box::new(self.authorize_relational_join_child(*join.left, frame)?);
        join.right = Box::new(self.authorize_relational_join_child(*join.right, frame)?);
        Ok(inject_rls_into_join(self, frame, join))
    }

    fn authorize_relational_join_child(
        &self,
        expr: QueryExpr,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<QueryExpr> {
        match expr {
            QueryExpr::Table(mut table) => {
                if let Some(TableSource::Subquery(inner)) = table.source.take() {
                    let authorized_inner = self.authorize_relational_select_expr(*inner, frame)?;
                    table.source = Some(TableSource::Subquery(Box::new(authorized_inner)));
                }
                Ok(QueryExpr::Table(table))
            }
            QueryExpr::Join(join) => self
                .authorize_relational_join_select(join, frame)?
                .map(QueryExpr::Join)
                .ok_or_else(|| {
                    RedDBError::Query("permission denied: RLS denied relational subquery".into())
                }),
            other => Ok(other),
        }
    }

    fn authorize_relational_select_expr(
        &self,
        expr: QueryExpr,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<QueryExpr> {
        match expr {
            QueryExpr::Table(table) => self
                .authorize_relational_table_select(table, frame)?
                .map(QueryExpr::Table)
                .ok_or_else(|| {
                    RedDBError::Query("permission denied: RLS denied relational subquery".into())
                }),
            QueryExpr::Join(join) => self
                .authorize_relational_join_select(join, frame)?
                .map(QueryExpr::Join)
                .ok_or_else(|| {
                    RedDBError::Query("permission denied: RLS denied relational subquery".into())
                }),
            other => Ok(other),
        }
    }

    fn check_table_column_projection_authz(
        &self,
        table: &TableQuery,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<()> {
        let Some((username, role)) = frame.identity() else {
            return Ok(());
        };
        let Some(auth_store) = self.inner.auth_store.read().clone() else {
            return Ok(());
        };

        let columns = self.resolved_table_projection_columns(table)?;
        let request = ColumnAccessRequest::select(table.table.clone(), columns);
        let principal = UserId::from_parts(frame.effective_scope(), username);
        let ctx = runtime_iam_context(role, frame.effective_scope());
        let outcome = auth_store.check_column_projection_authz(&principal, &request, &ctx);
        if outcome.allowed() {
            return Ok(());
        }

        if let Some(denied) = outcome.first_denied_column() {
            return Err(RedDBError::Query(format!(
                "permission denied: principal=`{username}` cannot select column `{}`",
                denied.resource.name
            )));
        }
        Err(RedDBError::Query(format!(
            "permission denied: principal=`{username}` cannot select table `{}`",
            table.table
        )))
    }

    fn check_join_column_projection_authz(
        &self,
        join: &JoinQuery,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<()> {
        let mut by_table: HashMap<String, BTreeSet<String>> = HashMap::new();
        let projections = crate::storage::query::sql_lowering::effective_join_projections(join);
        self.collect_join_projection_columns(join, &projections, &mut by_table)?;

        for (table, columns) in by_table {
            let query = TableQuery {
                table,
                source: None,
                alias: None,
                select_items: Vec::new(),
                columns: columns.into_iter().map(Projection::Column).collect(),
                where_expr: None,
                filter: None,
                group_by_exprs: Vec::new(),
                group_by: Vec::new(),
                having_expr: None,
                having: None,
                order_by: Vec::new(),
                limit: None,
                limit_param: None,
                offset: None,
                offset_param: None,
                expand: None,
                as_of: None,
                sessionize: None,
                distinct: false,
            };
            self.check_table_column_projection_authz(&query, frame)?;
        }
        Ok(())
    }

    fn collect_join_projection_columns(
        &self,
        join: &JoinQuery,
        projections: &[Projection],
        out: &mut HashMap<String, BTreeSet<String>>,
    ) -> RedDBResult<()> {
        let left = table_side_context(join.left.as_ref());
        let right = table_side_context(join.right.as_ref());

        if projections
            .iter()
            .any(|projection| matches!(projection, Projection::All))
        {
            for side in [left.as_ref(), right.as_ref()].into_iter().flatten() {
                out.entry(side.table.clone())
                    .or_default()
                    .extend(self.table_all_projection_columns(&side.table)?);
            }
            return Ok(());
        }

        for projection in projections {
            collect_projection_columns_for_join_side(
                projection,
                left.as_ref(),
                right.as_ref(),
                out,
            )?;
        }
        Ok(())
    }

    fn resolved_table_projection_columns(&self, table: &TableQuery) -> RedDBResult<Vec<String>> {
        let projections = crate::storage::query::sql_lowering::effective_table_projections(table);
        if projections
            .iter()
            .any(|projection| matches!(projection, Projection::All))
        {
            return self.table_all_projection_columns(&table.table);
        }

        let mut columns = BTreeSet::new();
        for projection in &projections {
            collect_projection_columns_for_table(
                projection,
                &table.table,
                table.alias.as_deref(),
                &mut columns,
            );
        }
        Ok(columns.into_iter().collect())
    }

    fn table_all_projection_columns(&self, table: &str) -> RedDBResult<Vec<String>> {
        if let Some(contract) = self.inner.db.collection_contract_arc(table) {
            let columns: Vec<String> = contract
                .declared_columns
                .iter()
                .map(|column| column.name.clone())
                .collect();
            if !columns.is_empty() {
                return Ok(columns);
            }
        }

        let records = scan_runtime_table_source_records_limited(&self.inner.db, table, Some(1))?;
        Ok(records
            .first()
            .map(|record| {
                record
                    .column_names()
                    .into_iter()
                    .map(|column| column.to_string())
                    .collect()
            })
            .unwrap_or_default())
    }

    fn resolve_table_expr_subqueries(
        &self,
        mut table: TableQuery,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<TableQuery> {
        // Only a `Subquery` source needs recursive resolution. `.take()`
        // would otherwise drop a `Name` / `Function` source on the floor
        // (the `if let` skips the body but the take already cleared it),
        // which silently broke `SELECT * FROM components(g)` — the TVF
        // dispatch downstream keys off `TableSource::Function` and never
        // fired. Restore any non-subquery source unchanged (issue #795).
        match table.source.take() {
            Some(TableSource::Subquery(inner)) => {
                let inner = self.resolve_select_expr_subqueries(*inner, frame)?;
                table.source = Some(TableSource::Subquery(Box::new(inner)));
            }
            other => table.source = other,
        }

        let outer_scopes = relation_scopes_for_query(&QueryExpr::Table(table.clone()));
        for item in &mut table.select_items {
            if let crate::storage::query::ast::SelectItem::Expr { expr, .. } = item {
                *expr = self.resolve_expr_subqueries(expr.clone(), &outer_scopes, frame)?;
            }
        }
        if let Some(where_expr) = table.where_expr.take() {
            table.where_expr =
                Some(self.resolve_expr_subqueries(where_expr, &outer_scopes, frame)?);
            table.filter = None;
        }
        if let Some(having_expr) = table.having_expr.take() {
            table.having_expr =
                Some(self.resolve_expr_subqueries(having_expr, &outer_scopes, frame)?);
            table.having = None;
        }
        for expr in &mut table.group_by_exprs {
            *expr = self.resolve_expr_subqueries(expr.clone(), &outer_scopes, frame)?;
        }
        for clause in &mut table.order_by {
            if let Some(expr) = clause.expr.take() {
                clause.expr = Some(self.resolve_expr_subqueries(expr, &outer_scopes, frame)?);
            }
        }
        Ok(table)
    }

    fn resolve_select_expr_subqueries(
        &self,
        expr: QueryExpr,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<QueryExpr> {
        match expr {
            QueryExpr::Table(table) => self
                .resolve_table_expr_subqueries(table, frame)
                .map(QueryExpr::Table),
            QueryExpr::Join(mut join) => {
                join.left = Box::new(self.resolve_select_expr_subqueries(*join.left, frame)?);
                join.right = Box::new(self.resolve_select_expr_subqueries(*join.right, frame)?);
                Ok(QueryExpr::Join(join))
            }
            other => Ok(other),
        }
    }

    fn resolve_expr_subqueries(
        &self,
        expr: crate::storage::query::ast::Expr,
        outer_scopes: &[String],
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<crate::storage::query::ast::Expr> {
        use crate::storage::query::ast::Expr;

        match expr {
            Expr::Subquery { query, span } => {
                let values = self.execute_expr_subquery_values(query, outer_scopes, frame)?;
                if values.len() > 1 {
                    return Err(RedDBError::Query(
                        "scalar subquery returned more than one row".to_string(),
                    ));
                }
                Ok(Expr::Literal {
                    value: values.into_iter().next().unwrap_or(Value::Null),
                    span,
                })
            }
            Expr::BinaryOp { op, lhs, rhs, span } => Ok(Expr::BinaryOp {
                op,
                lhs: Box::new(self.resolve_expr_subqueries(*lhs, outer_scopes, frame)?),
                rhs: Box::new(self.resolve_expr_subqueries(*rhs, outer_scopes, frame)?),
                span,
            }),
            Expr::UnaryOp { op, operand, span } => Ok(Expr::UnaryOp {
                op,
                operand: Box::new(self.resolve_expr_subqueries(*operand, outer_scopes, frame)?),
                span,
            }),
            Expr::Cast {
                inner,
                target,
                span,
            } => Ok(Expr::Cast {
                inner: Box::new(self.resolve_expr_subqueries(*inner, outer_scopes, frame)?),
                target,
                span,
            }),
            Expr::FunctionCall { name, args, span } => {
                let args = args
                    .into_iter()
                    .map(|arg| self.resolve_expr_subqueries(arg, outer_scopes, frame))
                    .collect::<RedDBResult<Vec<_>>>()?;
                Ok(Expr::FunctionCall { name, args, span })
            }
            Expr::Case {
                branches,
                else_,
                span,
            } => {
                let branches = branches
                    .into_iter()
                    .map(|(cond, value)| {
                        Ok((
                            self.resolve_expr_subqueries(cond, outer_scopes, frame)?,
                            self.resolve_expr_subqueries(value, outer_scopes, frame)?,
                        ))
                    })
                    .collect::<RedDBResult<Vec<_>>>()?;
                let else_ = else_
                    .map(|expr| self.resolve_expr_subqueries(*expr, outer_scopes, frame))
                    .transpose()?
                    .map(Box::new);
                Ok(Expr::Case {
                    branches,
                    else_,
                    span,
                })
            }
            Expr::IsNull {
                operand,
                negated,
                span,
            } => Ok(Expr::IsNull {
                operand: Box::new(self.resolve_expr_subqueries(*operand, outer_scopes, frame)?),
                negated,
                span,
            }),
            Expr::InList {
                target,
                values,
                negated,
                span,
            } => {
                let target =
                    Box::new(self.resolve_expr_subqueries(*target, outer_scopes, frame)?);
                let mut resolved = Vec::new();
                for value in values {
                    if let Expr::Subquery { query, .. } = value {
                        resolved.extend(
                            self.execute_expr_subquery_values(query, outer_scopes, frame)?
                                .into_iter()
                                .map(Expr::lit),
                        );
                    } else {
                        resolved.push(self.resolve_expr_subqueries(value, outer_scopes, frame)?);
                    }
                }
                Ok(Expr::InList {
                    target,
                    values: resolved,
                    negated,
                    span,
                })
            }
            Expr::Between {
                target,
                low,
                high,
                negated,
                span,
            } => Ok(Expr::Between {
                target: Box::new(self.resolve_expr_subqueries(*target, outer_scopes, frame)?),
                low: Box::new(self.resolve_expr_subqueries(*low, outer_scopes, frame)?),
                high: Box::new(self.resolve_expr_subqueries(*high, outer_scopes, frame)?),
                negated,
                span,
            }),
            other => Ok(other),
        }
    }

    fn execute_expr_subquery_values(
        &self,
        subquery: crate::storage::query::ast::ExprSubquery,
        outer_scopes: &[String],
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<Vec<Value>> {
        let query = *subquery.query;
        if query_references_outer_scope(&query, outer_scopes) {
            return Err(RedDBError::Query(
                "NOT_YET_SUPPORTED: correlated subqueries are not supported yet; track follow-up issue #470-correlated-subqueries".to_string(),
            ));
        }
        let query = self.rewrite_view_refs(query);
        let query = self.resolve_select_expr_subqueries(query, frame)?;
        let query = self.authorize_relational_select_expr(query, frame)?;
        let result = match query {
            QueryExpr::Table(table) => {
                execute_runtime_table_query(&self.inner.db, &table, Some(&self.inner.index_store))?
            }
            QueryExpr::Join(join) => execute_runtime_join_query(&self.inner.db, &join)?,
            other => {
                return Err(RedDBError::Query(format!(
                    "expression subquery must be a SELECT query, got {}",
                    query_expr_name(&other)
                )))
            }
        };
        first_column_values(result)
    }

    fn dispatch_expr(
        &self,
        expr: QueryExpr,
        query_str: &str,
        mode: QueryMode,
    ) -> RedDBResult<RuntimeQueryResult> {
        let statement = query_expr_name(&expr);
        match expr {
            QueryExpr::Graph(_) | QueryExpr::Path(_) => {
                // Graph queries are not cacheable as prepared statements.
                Err(RedDBError::Query(
                    "graph queries cannot be used as prepared statements".to_string(),
                ))
            }
            QueryExpr::Table(table) => {
                let scope = self.ai_scope();
                let table = self.resolve_table_expr_subqueries(
                    table,
                    &scope as &dyn super::statement_frame::ReadFrame,
                )?;
                // Table-valued functions (e.g. components(g)) dispatch to a
                // read-only executor before any catalog/virtual-table routing
                // (issue #795).
                if let Some(TableSource::Function {
                    name,
                    args,
                    named_args,
                }) = table.source.clone()
                {
                    return Ok(RuntimeQueryResult {
                        query: query_str.to_string(),
                        mode,
                        statement,
                        engine: "runtime-graph-tvf",
                        result: self.execute_table_function(&name, &args, &named_args)?,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    });
                }
                // Inline-graph TVF (issue #799) on the prepared-statement /
                // direct-expr path. Result caching is wired on the
                // `execute_query_inner` path; here we just compute and return.
                if let Some(TableSource::InlineGraphFunction {
                    name,
                    nodes,
                    edges,
                    named_args,
                }) = table.source.clone()
                {
                    return Ok(RuntimeQueryResult {
                        query: query_str.to_string(),
                        mode,
                        statement,
                        engine: "runtime-graph-tvf-inline",
                        result: self.execute_inline_graph_function(
                            &name,
                            &nodes,
                            &edges,
                            &named_args,
                        )?,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    });
                }
                if super::red_schema::is_virtual_table(&table.table) {
                    return Ok(RuntimeQueryResult {
                        query: query_str.to_string(),
                        mode,
                        statement,
                        engine: "runtime-red-schema",
                        result: super::red_schema::red_query(
                            self,
                            &table.table,
                            &table,
                            &scope as &dyn super::statement_frame::ReadFrame,
                        )?,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    });
                }
                // `<graph>.<output>` analytics virtual view (issue #800).
                if let Some(view_result) = self.try_resolve_analytics_view(
                    &table,
                    &scope as &dyn super::statement_frame::ReadFrame,
                )? {
                    return Ok(RuntimeQueryResult {
                        query: query_str.to_string(),
                        mode,
                        statement,
                        engine: "runtime-graph-analytics-view",
                        result: view_result,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    });
                }
                let Some(table_with_rls) = self.authorize_relational_table_select(
                    table,
                    &scope as &dyn super::statement_frame::ReadFrame,
                )?
                else {
                    return Ok(RuntimeQueryResult {
                        query: query_str.to_string(),
                        mode,
                        statement,
                        engine: "runtime-table-rls",
                        result: crate::storage::query::unified::UnifiedResult::empty(),
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    });
                };
                Ok(RuntimeQueryResult {
                    query: query_str.to_string(),
                    mode,
                    statement,
                    engine: "runtime-table",
                    result: execute_runtime_table_query(
                        &self.inner.db,
                        &table_with_rls,
                        Some(&self.inner.index_store),
                    )?,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                })
            }
            QueryExpr::Join(join) => {
                let scope = self.ai_scope();
                let Some(join_with_rls) = self.authorize_relational_join_select(
                    join,
                    &scope as &dyn super::statement_frame::ReadFrame,
                )?
                else {
                    return Ok(RuntimeQueryResult {
                        query: query_str.to_string(),
                        mode,
                        statement,
                        engine: "runtime-join-rls",
                        result: crate::storage::query::unified::UnifiedResult::empty(),
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    });
                };
                Ok(RuntimeQueryResult {
                    query: query_str.to_string(),
                    mode,
                    statement,
                    engine: "runtime-join",
                    result: execute_runtime_join_query(&self.inner.db, &join_with_rls)?,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                })
            }
            QueryExpr::Vector(vector) => Ok(RuntimeQueryResult {
                query: query_str.to_string(),
                mode,
                statement,
                engine: "runtime-vector",
                result: execute_runtime_vector_query(&self.inner.db, &vector)?,
                affected_rows: 0,
                statement_type: "select",
                bookmark: None,
            }),
            QueryExpr::Hybrid(hybrid) => Ok(RuntimeQueryResult {
                query: query_str.to_string(),
                mode,
                statement,
                engine: "runtime-hybrid",
                result: execute_runtime_hybrid_query(&self.inner.db, &hybrid)?,
                affected_rows: 0,
                statement_type: "select",
                bookmark: None,
            }),
            QueryExpr::Insert(ref insert) if super::red_schema::is_virtual_table(&insert.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Update(ref update) if super::red_schema::is_virtual_table(&update.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Delete(ref delete) if super::red_schema::is_virtual_table(&delete.table) => {
                Err(RedDBError::Query(
                    super::red_schema::READ_ONLY_ERROR.to_string(),
                ))
            }
            QueryExpr::Insert(ref insert) => self
                .with_deferred_store_wal_for_dml(self.insert_may_emit_events(insert), || {
                    self.execute_insert(query_str, insert)
                }),
            QueryExpr::Update(ref update) => self
                .with_deferred_store_wal_for_dml(self.update_may_emit_events(update), || {
                    self.execute_update(query_str, update)
                }),
            QueryExpr::Delete(ref delete) => self
                .with_deferred_store_wal_for_dml(self.delete_may_emit_events(delete), || {
                    self.execute_delete(query_str, delete)
                }),
            QueryExpr::SearchCommand(ref cmd) => self.execute_search_command(query_str, cmd),
            QueryExpr::Ask(ref ask) => self.execute_ask(query_str, ask),
            _ => Err(RedDBError::Query(format!(
                "prepared-statement execution does not support {statement} statements"
            ))),
        }
    }

    /// Dispatch a graph-collection table-valued function call in FROM
    /// position (e.g. `SELECT * FROM components(g)`).
    ///
    /// Validates the function name and arity here, materializes the whole
    /// active graph read-only, then runs the algorithm via the shared
    /// `dispatch_graph_algorithm` path. Never mutates the catalog or store.
    fn execute_table_function(
        &self,
        name: &str,
        args: &[String],
        named_args: &[(String, f64)],
    ) -> RedDBResult<crate::storage::query::unified::UnifiedResult> {
        if !is_graph_tvf_name(name) {
            return Err(RedDBError::Query(format!("unknown table function: {name}")));
        }
        // Every graph-collection TVF takes exactly one graph argument.
        if args.len() != 1 {
            return Err(RedDBError::Query(format!(
                "table function '{name}' takes exactly 1 graph argument, got {}",
                args.len()
            )));
        }

        // Read-only materialization of the full active graph. Passing `None`
        // for the projection uses the full graph store. Like #795/#796, the
        // v0 form runs over the whole graph store regardless of the collection
        // argument value. Materialization never mutates any store.
        let (nodes, edges) = self.materialize_whole_graph_abstract()?;
        self.dispatch_graph_algorithm(name, nodes, edges, named_args)
    }

    /// Dispatch an inline-graph table-valued function call in FROM position
    /// (e.g. `SELECT * FROM components(nodes => (…), edges => (…))`, issue
    /// #799).
    ///
    /// Materializes the two subqueries through the normal read path (so RLS,
    /// column authz, and MVCC visibility all apply), constructs the abstract
    /// graph — the first column of `nodes` is the node id; the first two-or-
    /// three columns of `edges` are `(source, target [, weight])` — then runs
    /// the same algorithm path used by the graph-collection form. Read-only.
    fn execute_inline_graph_function(
        &self,
        name: &str,
        nodes_query: &QueryExpr,
        edges_query: &QueryExpr,
        named_args: &[(String, f64)],
    ) -> RedDBResult<crate::storage::query::unified::UnifiedResult> {
        if !is_graph_tvf_name(name) {
            return Err(RedDBError::Query(format!("unknown table function: {name}")));
        }

        let node_result = self.execute_query_expr(nodes_query.clone())?.result;
        let nodes = inline_node_ids(name, &node_result)?;

        let edge_result = self.execute_query_expr(edges_query.clone())?.result;
        let edges = inline_edges(name, &edge_result)?;

        self.dispatch_graph_algorithm(name, nodes, edges, named_args)
    }

    /// Materialize the whole active graph read-only into the abstract
    /// `(nodes, edges)` inputs the pure graph algorithms consume.
    fn materialize_whole_graph_abstract(
        &self,
    ) -> RedDBResult<(
        Vec<String>,
        Vec<(
            String,
            String,
            crate::storage::engine::graph_algorithms::Weight,
        )>,
    )> {
        use crate::storage::engine::graph_algorithms;

        let graph = super::graph_dsl::materialize_graph_with_projection(
            self.inner.db.store().as_ref(),
            None,
        )?;
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();
        let edges: Vec<(String, String, graph_algorithms::Weight)> = graph
            .iter_all_edges()
            .into_iter()
            .map(|e| (e.source_id, e.target_id, e.weight))
            .collect();
        Ok((nodes, edges))
    }

    /// Resolve a `<graph>.<output>` analytics virtual view (issue #800).
    ///
    /// Returns `Ok(None)` when `table` is not an analytics view — either the
    /// name is not dotted, a real collection of that exact name exists (a real
    /// collection always wins; no shadowing), the suffix is not a recognised
    /// analytics output, or the parent is not a graph. Returns `Ok(Some(_))`
    /// with the freshly computed result when it does resolve, and an error when
    /// the parent graph exists but the output is not enabled, a declared
    /// algorithm is unsupported, or the parent collection's policy denies the
    /// read.
    ///
    /// The view is recomputed on every call (no result-cache write) so it
    /// always reflects the current graph data, satisfying the on-demand
    /// recompute contract for this slice.
    fn try_resolve_analytics_view(
        &self,
        table: &TableQuery,
        frame: &dyn super::statement_frame::ReadFrame,
    ) -> RedDBResult<Option<crate::storage::query::unified::UnifiedResult>> {
        let full = table.table.as_str();
        let Some(dot) = full.rfind('.') else {
            return Ok(None);
        };
        // A real collection literally named `g.communities` always wins.
        if self.inner.db.store().get_collection(full).is_some() {
            return Ok(None);
        }
        let graph_name = &full[..dot];
        let output_name = &full[dot + 1..];
        let Some(output) = crate::catalog::AnalyticsOutput::from_str(output_name) else {
            return Ok(None);
        };

        let contracts = self.inner.db.collection_contracts();
        let Some(contract) = contracts.iter().find(|c| c.name == graph_name) else {
            return Ok(None);
        };
        if contract.declared_model != crate::catalog::CollectionModel::Graph {
            return Ok(None);
        }
        let Some(view) = contract
            .analytics_config
            .iter()
            .find(|view| view.output == output)
        else {
            // The parent graph exists but this output was not declared — a
            // clear error beats the misleading "collection not found".
            return Err(RedDBError::Query(format!(
                "analytics output '{output_name}' is not enabled on graph '{graph_name}'; declare it with WITH ANALYTICS (...)"
            )));
        };

        // Policy inheritance (AC5): route through the parent graph collection's
        // read authorization. A policy or RLS rule that denies the parent
        // denies its analytics views transitively.
        let parent_query = TableQuery::new(graph_name);
        if self
            .authorize_relational_table_select(parent_query, frame)?
            .is_none()
        {
            return Err(RedDBError::Query(format!(
                "permission denied: policy on graph '{graph_name}' denies analytics view '{output_name}'"
            )));
        }

        let (algorithm, named_args) = analytics_view_algorithm(graph_name, view)?;
        let (nodes, edges) = self.materialize_whole_graph_abstract()?;
        let result = self.dispatch_graph_algorithm(&algorithm, nodes, edges, &named_args)?;
        Ok(Some(result))
    }

    /// Shared algorithm dispatch over abstract `(nodes, edges)` inputs.
    ///
    /// Both the graph-collection form and the inline-graph form route here so
    /// named-argument validation and the projected row shape stay identical
    /// across the two signatures (issue #799). Projects each algorithm's
    /// native output shape.
    fn dispatch_graph_algorithm(
        &self,
        name: &str,
        nodes: Vec<String>,
        edges: Vec<(
            String,
            String,
            crate::storage::engine::graph_algorithms::Weight,
        )>,
        named_args: &[(String, f64)],
    ) -> RedDBResult<crate::storage::query::unified::UnifiedResult> {
        use crate::storage::engine::graph_algorithms;
        use crate::storage::query::unified::UnifiedResult;
        use crate::storage::schema::Value;

        if name.eq_ignore_ascii_case("components") {
            reject_named_args(name, named_args)?;
            let assignment = graph_algorithms::connected_components(&nodes, &edges);
            let mut result =
                UnifiedResult::with_columns(vec!["node_id".into(), "island_id".into()]);
            for (node_id, island_id) in assignment {
                let mut record = UnifiedRecord::new();
                record.set("node_id", Value::text(node_id));
                record.set("island_id", Value::Integer(island_id as i64));
                result.push(record);
            }
            return Ok(result);
        }

        if name.eq_ignore_ascii_case("louvain") {
            // The only supported named argument is `resolution` (γ). It
            // defaults to 1.0 (classic modularity) and must be a finite,
            // strictly positive number — a non-positive (or NaN/inf)
            // resolution has no sensible meaning.
            let resolution = louvain_resolution(named_args)?;
            let assignment = graph_algorithms::louvain(&nodes, &edges, resolution);
            let mut result =
                UnifiedResult::with_columns(vec!["node_id".into(), "community_id".into()]);
            for (node_id, community_id) in assignment {
                let mut record = UnifiedRecord::new();
                record.set("node_id", Value::text(node_id));
                record.set("community_id", Value::Integer(community_id as i64));
                result.push(record);
            }
            return Ok(result);
        }

        if name.eq_ignore_ascii_case("degree_centrality") {
            reject_named_args(name, named_args)?;
            let assignment = abstract_degree_centrality(&nodes, &edges);
            let mut result = UnifiedResult::with_columns(vec!["node_id".into(), "degree".into()]);
            for (node_id, degree) in assignment {
                let mut record = UnifiedRecord::new();
                record.set("node_id", Value::text(node_id));
                record.set("degree", Value::Integer(degree as i64));
                result.push(record);
            }
            return Ok(result);
        }

        if name.eq_ignore_ascii_case("shortest_path") {
            // Scalar named arguments: `src` and `dst` are required node ids,
            // `max_hops` is an optional non-negative edge-count cap. Node ids
            // in the graph store are integer entity ids rendered as strings, so
            // each id arg must be a non-negative whole number; reject anything
            // else (fractional, negative, NaN/inf) with a clear message.
            let mut src: Option<String> = None;
            let mut dst: Option<String> = None;
            let mut max_hops: Option<usize> = None;
            let as_node_id = |key: &str, value: f64| -> RedDBResult<String> {
                if !value.is_finite() || value < 0.0 || value.fract() != 0.0 {
                    return Err(RedDBError::Query(format!(
                        "table function 'shortest_path' argument '{key}' must be a non-negative integer node id, got {value}"
                    )));
                }
                Ok((value as i64).to_string())
            };
            for (key, value) in named_args {
                if key.eq_ignore_ascii_case("src") {
                    src = Some(as_node_id("src", *value)?);
                } else if key.eq_ignore_ascii_case("dst") {
                    dst = Some(as_node_id("dst", *value)?);
                } else if key.eq_ignore_ascii_case("max_hops") {
                    if !value.is_finite() || *value < 0.0 || value.fract() != 0.0 {
                        return Err(RedDBError::Query(format!(
                            "table function 'shortest_path' max_hops must be a non-negative integer, got {value}"
                        )));
                    }
                    max_hops = Some(*value as usize);
                } else {
                    return Err(RedDBError::Query(format!(
                        "table function 'shortest_path' has no named argument '{key}' (expected 'src', 'dst', 'max_hops')"
                    )));
                }
            }
            let src = src.ok_or_else(|| {
                RedDBError::Query(
                    "table function 'shortest_path' requires named argument 'src'".to_string(),
                )
            })?;
            let dst = dst.ok_or_else(|| {
                RedDBError::Query(
                    "table function 'shortest_path' requires named argument 'dst'".to_string(),
                )
            })?;

            // Columns are always present; an unreachable pair (within the
            // optional `max_hops` budget) simply yields zero rows — never an
            // error. `hop` is the 0-based index from the source;
            // `cumulative_weight` is the running path weight (0 at the source,
            // the total at the destination). Edges are treated as undirected,
            // consistent with `components` / `louvain`.
            let mut result = UnifiedResult::with_columns(vec![
                "hop".into(),
                "node_id".into(),
                "cumulative_weight".into(),
            ]);
            if let Some(path) =
                graph_algorithms::shortest_path(&nodes, &edges, &src, &dst, max_hops)
            {
                for (hop, (node_id, cumulative_weight)) in path.into_iter().enumerate() {
                    let mut record = UnifiedRecord::new();
                    record.set("hop", Value::Integer(hop as i64));
                    record.set("node_id", Value::text(node_id));
                    record.set("cumulative_weight", Value::Float(cumulative_weight));
                    result.push(record);
                }
            }
            return Ok(result);
        }
        // ── Centrality family (issue #797): each returns rows `(node_id,
        // score)` over the abstract `(nodes, edges)` graph. Like the other
        // graph TVFs the graph is treated as undirected and scores are
        // deterministic; the inline-graph form shares this dispatch. ──
        if name.eq_ignore_ascii_case("betweenness") {
            reject_named_args(name, named_args)?;
            return Ok(Self::centrality_result(graph_algorithms::betweenness(
                &nodes, &edges,
            )));
        }
        if name.eq_ignore_ascii_case("eigenvector") {
            // Optional `max_iterations` (positive integer, default 100) and
            // `tolerance` (finite, strictly positive, default 1e-6).
            let mut max_iterations = 100_usize;
            let mut tolerance = 1e-6_f64;
            for (key, value) in named_args {
                if key.eq_ignore_ascii_case("max_iterations") {
                    max_iterations = parse_positive_iterations("eigenvector", value)?;
                } else if key.eq_ignore_ascii_case("tolerance") {
                    if !value.is_finite() || *value <= 0.0 {
                        return Err(RedDBError::Query(format!(
                            "table function 'eigenvector' tolerance must be > 0, got {value}"
                        )));
                    }
                    tolerance = *value;
                } else {
                    return Err(RedDBError::Query(format!(
                        "table function 'eigenvector' has no named argument '{key}' (expected 'max_iterations' or 'tolerance')"
                    )));
                }
            }
            return Ok(Self::centrality_result(graph_algorithms::eigenvector(
                &nodes,
                &edges,
                max_iterations,
                tolerance,
            )));
        }
        if name.eq_ignore_ascii_case("pagerank") {
            // Optional `damping` (in (0, 1), default 0.85) and `max_iterations`
            // (positive integer, default 100).
            let mut damping = 0.85_f64;
            let mut max_iterations = 100_usize;
            for (key, value) in named_args {
                if key.eq_ignore_ascii_case("damping") {
                    if !value.is_finite() || *value <= 0.0 || *value >= 1.0 {
                        return Err(RedDBError::Query(format!(
                            "table function 'pagerank' damping must be in (0, 1), got {value}"
                        )));
                    }
                    damping = *value;
                } else if key.eq_ignore_ascii_case("max_iterations") {
                    max_iterations = parse_positive_iterations("pagerank", value)?;
                } else {
                    return Err(RedDBError::Query(format!(
                        "table function 'pagerank' has no named argument '{key}' (expected 'damping' or 'max_iterations')"
                    )));
                }
            }
            return Ok(Self::centrality_result(graph_algorithms::pagerank(
                &nodes,
                &edges,
                damping,
                max_iterations,
            )));
        }
        Err(RedDBError::Query(format!("unknown table function: {name}")))
    }

    /// `components(<graph_collection>)` — returns rows `(node_id, island_id)`.
    ///
    /// Materializes the active graph (nodes + weighted edges) read-only and
    /// runs the pure `graph_algorithms::connected_components`. Edges are
    /// treated as undirected; island ids are deterministic (ascending order of
    /// each component's smallest node).
    fn execute_components_tvf(
        &self,
        _collection: &str,
    ) -> RedDBResult<crate::storage::query::unified::UnifiedResult> {
        use crate::storage::engine::graph_algorithms;
        use crate::storage::query::unified::UnifiedResult;
        use crate::storage::schema::Value;

        // Read-only materialization of the full active graph. The named
        // collection identifies the active graph scope; passing `None` for the
        // projection uses the full graph store (the same result
        // `active_graph_projection` yields when no projection is registered).
        // Materialization never mutates any store.
        let graph = super::graph_dsl::materialize_graph_with_projection(
            self.inner.db.store().as_ref(),
            None,
        )?;

        // Materialize abstract inputs for the pure algorithm.
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();
        let edges: Vec<(String, String, graph_algorithms::Weight)> = graph
            .iter_all_edges()
            .into_iter()
            .map(|e| (e.source_id, e.target_id, e.weight))
            .collect();

        let assignment = graph_algorithms::connected_components(&nodes, &edges);

        // Project into a UnifiedResult with columns ["node_id", "island_id"].
        let mut result = UnifiedResult::with_columns(vec!["node_id".into(), "island_id".into()]);
        for (node_id, island_id) in assignment {
            let mut record = UnifiedRecord::new();
            record.set("node_id", Value::text(node_id));
            record.set("island_id", Value::Integer(island_id as i64));
            result.push(record);
        }
        Ok(result)
    }

    /// `louvain(<graph> [, resolution => <f64>])` — returns rows
    /// `(node_id, community_id)` (issue #796).
    ///
    /// Materializes the active graph (nodes + weighted edges) read-only and
    /// runs the pure, deterministic `graph_algorithms::louvain`. Edges are
    /// treated as undirected; community ids are assigned in ascending order of
    /// each community's smallest node, so identical input + resolution always
    /// yields identical rows. Like `components`, the v0 form runs over the
    /// whole graph store regardless of the collection argument value.
    fn execute_louvain_tvf(
        &self,
        _collection: &str,
        resolution: f64,
    ) -> RedDBResult<crate::storage::query::unified::UnifiedResult> {
        use crate::storage::engine::graph_algorithms;
        use crate::storage::query::unified::UnifiedResult;
        use crate::storage::schema::Value;

        let graph = super::graph_dsl::materialize_graph_with_projection(
            self.inner.db.store().as_ref(),
            None,
        )?;

        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();
        let edges: Vec<(String, String, graph_algorithms::Weight)> = graph
            .iter_all_edges()
            .into_iter()
            .map(|e| (e.source_id, e.target_id, e.weight))
            .collect();

        let assignment = graph_algorithms::louvain(&nodes, &edges, resolution);

        // Project into a UnifiedResult with columns ["node_id", "community_id"].
        let mut result = UnifiedResult::with_columns(vec!["node_id".into(), "community_id".into()]);
        for (node_id, community_id) in assignment {
            let mut record = UnifiedRecord::new();
            record.set("node_id", Value::text(node_id));
            record.set("community_id", Value::Integer(community_id as i64));
            result.push(record);
        }
        Ok(result)
    }

    /// Project `(node_id, score)` centrality rows into a `UnifiedResult` with
    /// columns `["node_id", "score"]`; scores are `Value::Float`.
    fn centrality_result(
        rows: Vec<(String, f64)>,
    ) -> crate::storage::query::unified::UnifiedResult {
        use crate::storage::query::unified::UnifiedResult;
        use crate::storage::schema::Value;
        let mut result = UnifiedResult::with_columns(vec!["node_id".into(), "score".into()]);
        for (node_id, score) in rows {
            let mut record = UnifiedRecord::new();
            record.set("node_id", Value::text(node_id));
            record.set("score", Value::Float(score));
            result.push(record);
        }
        result
    }

    /// Ultra-fast path: detect `SELECT * FROM table WHERE _entity_id = N` by string pattern
    /// and execute it without SQL parsing or planning. Returns None if pattern doesn't match.
    fn try_fast_entity_lookup(&self, query: &str) -> Option<RedDBResult<RuntimeQueryResult>> {
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

    /// Record that the running transaction has marked `id` in `collection`
    /// for deletion (Phase 2.3.2b MVCC tombstones). `stamper_xid` is the
    /// xid that was written into `xmax` — either the parent txn xid or
    /// the innermost savepoint sub-xid. Savepoint rollback filters by
    /// this xid to revive only its own tombstones.
    pub(crate) fn record_pending_tombstone(
        &self,
        conn_id: u64,
        collection: &str,
        id: crate::storage::unified::entity::EntityId,
        stamper_xid: crate::storage::transaction::snapshot::Xid,
        previous_xmax: crate::storage::transaction::snapshot::Xid,
    ) {
        self.inner
            .pending_tombstones
            .write()
            .entry(conn_id)
            .or_default()
            .push((collection.to_string(), id, stamper_xid, previous_xmax));
    }

    pub(crate) fn record_pending_versioned_update(
        &self,
        conn_id: u64,
        collection: &str,
        old_id: crate::storage::unified::entity::EntityId,
        new_id: crate::storage::unified::entity::EntityId,
        stamper_xid: crate::storage::transaction::snapshot::Xid,
        previous_xmax: crate::storage::transaction::snapshot::Xid,
    ) {
        self.inner
            .pending_versioned_updates
            .write()
            .entry(conn_id)
            .or_default()
            .push((
                collection.to_string(),
                old_id,
                new_id,
                stamper_xid,
                previous_xmax,
            ));
    }

    fn with_deferred_store_wal_if_transaction<T>(
        &self,
        f: impl FnOnce() -> RedDBResult<T>,
    ) -> RedDBResult<T> {
        let conn_id = current_connection_id();
        if !self.inner.tx_contexts.read().contains_key(&conn_id) {
            return f();
        }

        crate::storage::UnifiedStore::begin_deferred_store_wal_capture();
        let result = f();
        let captured = crate::storage::UnifiedStore::take_deferred_store_wal_capture();
        match result {
            Ok(value) => {
                self.record_pending_store_wal_actions(conn_id, captured);
                Ok(value)
            }
            Err(err) => Err(err),
        }
    }

    fn with_deferred_store_wal_for_dml<T>(
        &self,
        capture_autocommit_events: bool,
        f: impl FnOnce() -> RedDBResult<T>,
    ) -> RedDBResult<T> {
        let conn_id = current_connection_id();
        if self.inner.tx_contexts.read().contains_key(&conn_id) {
            return self.with_deferred_store_wal_if_transaction(f);
        }
        if !capture_autocommit_events {
            return f();
        }

        crate::storage::UnifiedStore::begin_deferred_store_wal_capture();
        let result = f();
        let captured = crate::storage::UnifiedStore::take_deferred_store_wal_capture();
        self.inner
            .db
            .store()
            .append_deferred_store_wal_actions(captured)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        result
    }

    fn insert_may_emit_events(&self, query: &InsertQuery) -> bool {
        !query.suppress_events
            && self.collection_has_event_subscriptions_for_operation(
                &query.table,
                crate::catalog::SubscriptionOperation::Insert,
            )
    }

    fn update_may_emit_events(&self, query: &UpdateQuery) -> bool {
        !query.suppress_events
            && self.collection_has_event_subscriptions_for_operation(
                &query.table,
                crate::catalog::SubscriptionOperation::Update,
            )
    }

    fn delete_may_emit_events(&self, query: &DeleteQuery) -> bool {
        !query.suppress_events
            && self.collection_has_event_subscriptions_for_operation(
                &query.table,
                crate::catalog::SubscriptionOperation::Delete,
            )
    }

    fn collection_has_event_subscriptions_for_operation(
        &self,
        collection: &str,
        operation: crate::catalog::SubscriptionOperation,
    ) -> bool {
        let Some(contract) = self.db().collection_contract_arc(collection) else {
            return false;
        };
        contract.subscriptions.iter().any(|subscription| {
            subscription.enabled
                && (subscription.ops_filter.is_empty()
                    || subscription.ops_filter.contains(&operation))
        })
    }

    fn record_pending_store_wal_actions(
        &self,
        conn_id: u64,
        actions: crate::storage::unified::DeferredStoreWalActions,
    ) {
        if actions.is_empty() {
            return;
        }
        let mut guard = self.inner.pending_store_wal_actions.write();
        guard.entry(conn_id).or_default().extend(actions);
    }

    fn flush_pending_store_wal_actions(&self, conn_id: u64) -> RedDBResult<()> {
        let Some(actions) = self
            .inner
            .pending_store_wal_actions
            .write()
            .remove(&conn_id)
        else {
            return Ok(());
        };
        self.inner
            .db
            .store()
            .append_deferred_store_wal_actions(actions)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    fn discard_pending_store_wal_actions(&self, conn_id: u64) {
        self.inner
            .pending_store_wal_actions
            .write()
            .remove(&conn_id);
    }

    fn xid_conflicts_with_snapshot(
        &self,
        xid: crate::storage::transaction::snapshot::Xid,
        snapshot: &crate::storage::transaction::snapshot::Snapshot,
        own_xids: &std::collections::HashSet<crate::storage::transaction::snapshot::Xid>,
    ) -> bool {
        xid != 0
            && !own_xids.contains(&xid)
            && !self.inner.snapshot_manager.is_aborted(xid)
            && !self.inner.snapshot_manager.is_active(xid)
            && (xid > snapshot.xid || snapshot.in_progress.contains(&xid))
    }

    fn conflict_error(
        collection: &str,
        logical_id: crate::storage::unified::entity::EntityId,
        xid: crate::storage::transaction::snapshot::Xid,
    ) -> RedDBError {
        RedDBError::Query(format!(
            "serialization conflict: table row {collection}/{} was modified by concurrent transaction {xid}",
            logical_id.raw()
        ))
    }

    fn check_logical_row_conflict(
        &self,
        collection: &str,
        logical_id: crate::storage::unified::entity::EntityId,
        excluded_ids: &[crate::storage::unified::entity::EntityId],
        snapshot: &crate::storage::transaction::snapshot::Snapshot,
        own_xids: &std::collections::HashSet<crate::storage::transaction::snapshot::Xid>,
    ) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(());
        };

        for candidate in manager.query_all(|_| true) {
            if excluded_ids.contains(&candidate.id) || candidate.logical_id() != logical_id {
                continue;
            }
            if self.xid_conflicts_with_snapshot(candidate.xmin, snapshot, own_xids) {
                return Err(Self::conflict_error(collection, logical_id, candidate.xmin));
            }
            if self.xid_conflicts_with_snapshot(candidate.xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(collection, logical_id, candidate.xmax));
            }
        }
        Ok(())
    }

    pub(crate) fn check_table_row_write_conflicts(
        &self,
        conn_id: u64,
        snapshot: &crate::storage::transaction::snapshot::Snapshot,
        own_xids: &std::collections::HashSet<crate::storage::transaction::snapshot::Xid>,
    ) -> RedDBResult<()> {
        let versioned_updates = self
            .inner
            .pending_versioned_updates
            .read()
            .get(&conn_id)
            .cloned()
            .unwrap_or_default();
        let tombstones = self
            .inner
            .pending_tombstones
            .read()
            .get(&conn_id)
            .cloned()
            .unwrap_or_default();

        let store = self.inner.db.store();
        for (collection, old_id, new_id, xid, previous_xmax) in versioned_updates {
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            let Some(old) = manager.get(old_id) else {
                continue;
            };
            let logical_id = old.logical_id();
            if self.xid_conflicts_with_snapshot(previous_xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(&collection, logical_id, previous_xmax));
            }
            if old.xmax != xid && self.xid_conflicts_with_snapshot(old.xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(&collection, logical_id, old.xmax));
            }
            self.check_logical_row_conflict(
                &collection,
                logical_id,
                &[old_id, new_id],
                snapshot,
                own_xids,
            )?;
        }

        for (collection, id, xid, previous_xmax) in tombstones {
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            let Some(entity) = manager.get(id) else {
                continue;
            };
            let logical_id = entity.logical_id();
            if self.xid_conflicts_with_snapshot(previous_xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(&collection, logical_id, previous_xmax));
            }
            if entity.xmax != xid
                && self.xid_conflicts_with_snapshot(entity.xmax, snapshot, own_xids)
            {
                return Err(Self::conflict_error(&collection, logical_id, entity.xmax));
            }
            self.check_logical_row_conflict(&collection, logical_id, &[id], snapshot, own_xids)?;
        }

        Ok(())
    }

    pub(crate) fn restore_pending_write_stamps(&self, conn_id: u64) {
        let versioned_updates = self
            .inner
            .pending_versioned_updates
            .read()
            .get(&conn_id)
            .cloned()
            .unwrap_or_default();
        let tombstones = self
            .inner
            .pending_tombstones
            .read()
            .get(&conn_id)
            .cloned()
            .unwrap_or_default();

        let store = self.inner.db.store();
        for (collection, old_id, _new_id, xid, _previous_xmax) in versioned_updates {
            if let Some(manager) = store.get_collection(&collection) {
                if let Some(mut entity) = manager.get(old_id) {
                    entity.set_xmax(xid);
                    let _ = manager.update(entity);
                }
            }
        }
        for (collection, id, xid, _previous_xmax) in tombstones {
            if let Some(manager) = store.get_collection(&collection) {
                if let Some(mut entity) = manager.get(id) {
                    entity.set_xmax(xid);
                    let _ = manager.update(entity);
                }
            }
        }
    }

    pub(crate) fn finalize_pending_versioned_updates(&self, conn_id: u64) {
        self.inner
            .pending_versioned_updates
            .write()
            .remove(&conn_id);
    }

    pub(crate) fn revive_pending_versioned_updates(&self, conn_id: u64) {
        let Some(pending) = self
            .inner
            .pending_versioned_updates
            .write()
            .remove(&conn_id)
        else {
            return;
        };

        let store = self.inner.db.store();
        for (collection, old_id, new_id, xid, previous_xmax) in pending {
            if let Some(manager) = store.get_collection(&collection) {
                if let Some(mut old) = manager.get(old_id) {
                    if old.xmax == xid {
                        old.set_xmax(previous_xmax);
                        let _ = manager.update(old);
                    }
                }
            }
            let _ = store.delete_batch(&collection, &[new_id]);
        }
    }

    pub(crate) fn revive_versioned_updates_since(&self, conn_id: u64, stamper_xid: u64) -> usize {
        let mut guard = self.inner.pending_versioned_updates.write();
        let Some(pending) = guard.get_mut(&conn_id) else {
            return 0;
        };

        let store = self.inner.db.store();
        let mut reverted = 0usize;
        pending.retain(|(collection, old_id, new_id, xid, previous_xmax)| {
            if *xid < stamper_xid {
                return true;
            }
            if let Some(manager) = store.get_collection(collection) {
                if let Some(mut old) = manager.get(*old_id) {
                    if old.xmax == *xid {
                        old.set_xmax(*previous_xmax);
                        let _ = manager.update(old);
                    }
                }
            }
            let _ = store.delete_batch(collection, &[*new_id]);
            reverted += 1;
            false
        });
        if pending.is_empty() {
            guard.remove(&conn_id);
        }
        reverted
    }

    /// Flush tombstones on COMMIT. The xmax stamp is already the durable
    /// delete marker; commit only drops the rollback journal and emits
    /// side effects. Physical reclamation is left for VACUUM so old
    /// snapshots can still resolve the pre-delete row version.
    pub(crate) fn finalize_pending_tombstones(&self, conn_id: u64) {
        let Some(pending) = self.inner.pending_tombstones.write().remove(&conn_id) else {
            return;
        };
        if pending.is_empty() {
            return;
        }

        let store = self.inner.db.store();
        for (collection, id, _xid, _previous_xmax) in pending {
            store.context_index().remove_entity(id);
            self.cdc_emit(
                crate::replication::cdc::ChangeOperation::Delete,
                &collection,
                id.raw(),
                "entity",
            );
        }
    }

    /// Revive tombstones on ROLLBACK — reset `xmax` to 0 so the tuples
    /// become visible again to future snapshots. Best-effort: a row
    /// already reclaimed by a concurrent VACUUM stays gone, but VACUUM
    /// never reclaims tuples whose xmax is still referenced by any
    /// active snapshot, so this case is only reachable via external
    /// storage corruption.
    pub(crate) fn revive_pending_tombstones(&self, conn_id: u64) {
        let Some(pending) = self.inner.pending_tombstones.write().remove(&conn_id) else {
            return;
        };

        let store = self.inner.db.store();
        for (collection, id, xid, previous_xmax) in pending {
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            if let Some(mut entity) = manager.get(id) {
                if entity.xmax == xid {
                    entity.set_xmax(previous_xmax);
                    let _ = manager.update(entity);
                }
            }
        }
    }

    /// Slice C of PRD #718 — accessor for the local wait registry.
    pub fn queue_wait_registry(
        &self,
    ) -> std::sync::Arc<crate::runtime::queue_wait_registry::QueueWaitRegistry> {
        self.inner.queue_wait_registry.clone()
    }

    /// Buffer a `(scope, queue)` wake on the current connection so it
    /// fires post-COMMIT, or notify immediately if no transaction is
    /// open (autocommit path). The wait registry only ever observes
    /// notifies for committed work — rollback drops the buffer.
    pub(crate) fn record_queue_wake(&self, scope: &str, queue: &str) {
        if self.current_xid().is_some() {
            let conn_id = current_connection_id();
            self.inner
                .pending_queue_wakes
                .write()
                .entry(conn_id)
                .or_default()
                .push((scope.to_string(), queue.to_string()));
            return;
        }
        self.inner.queue_wait_registry.notify(scope, queue);
    }

    pub(crate) fn finalize_pending_queue_wakes(&self, conn_id: u64) {
        let Some(pending) = self.inner.pending_queue_wakes.write().remove(&conn_id) else {
            return;
        };
        for (scope, queue) in pending {
            self.inner.queue_wait_registry.notify(&scope, &queue);
        }
    }

    pub(crate) fn discard_pending_queue_wakes(&self, conn_id: u64) {
        self.inner.pending_queue_wakes.write().remove(&conn_id);
    }

    pub(crate) fn finalize_pending_kv_watch_events(&self, conn_id: u64) {
        let Some(pending) = self.inner.pending_kv_watch_events.write().remove(&conn_id) else {
            return;
        };
        for event in pending {
            self.cdc_emit_kv(
                event.op,
                &event.collection,
                &event.key,
                0,
                event.before,
                event.after,
            );
        }
    }

    pub(crate) fn discard_pending_kv_watch_events(&self, conn_id: u64) {
        self.inner.pending_kv_watch_events.write().remove(&conn_id);
    }

    /// Materialise the entire graph store while applying MVCC visibility
    /// AND per-collection RLS to each candidate node and edge. Mirrors
    /// `materialize_graph` but routes every entity through the same
    /// gate the SELECT path uses, with the correct `PolicyTargetKind`
    /// per entity kind (`Nodes` for graph nodes, `Edges` for graph
    /// edges). Returns the filtered `GraphStore` plus the
    /// `node_id → properties` map the executor needs for `RETURN n.*`
    /// projections.
    fn materialize_graph_with_rls(
        &self,
    ) -> RedDBResult<(
        crate::storage::engine::GraphStore,
        std::collections::HashMap<
            String,
            std::collections::HashMap<String, crate::storage::schema::Value>,
        >,
        crate::storage::query::unified::EdgeProperties,
    )> {
        use crate::storage::engine::GraphStore;
        use crate::storage::query::ast::{PolicyAction, PolicyTargetKind};
        use crate::storage::unified::entity::{EntityData, EntityKind};
        use std::collections::{HashMap, HashSet};

        let store = self.inner.db.store();
        let snap_ctx = capture_current_snapshot();
        let role = current_auth_identity().map(|(_, r)| r.as_str().to_string());

        let graph = GraphStore::new();
        let mut node_properties: HashMap<String, HashMap<String, crate::storage::schema::Value>> =
            HashMap::new();
        let mut edge_properties: crate::storage::query::unified::EdgeProperties = HashMap::new();
        let mut allowed_nodes: HashSet<String> = HashSet::new();

        // Per-collection cached compiled filters — Nodes-kind for
        // first pass, Edges-kind for the second. None entries mean
        // "RLS enabled, zero matching policy → deny all of this kind".
        let mut node_rls: HashMap<String, Option<crate::storage::query::ast::Filter>> =
            HashMap::new();
        let mut edge_rls: HashMap<String, Option<crate::storage::query::ast::Filter>> =
            HashMap::new();

        let collections = store.list_collections();

        // First pass — gather nodes.
        for collection in &collections {
            let Some(manager) = store.get_collection(collection) else {
                continue;
            };
            let entities = manager.query_all(|_| true);
            for entity in entities {
                if !entity_visible_with_context(snap_ctx.as_ref(), &entity) {
                    continue;
                }
                let EntityKind::GraphNode(ref node) = entity.kind else {
                    continue;
                };
                if !node_passes_rls(self, collection, role.as_deref(), &mut node_rls, &entity) {
                    continue;
                }
                let id_str = entity.id.raw().to_string();
                graph
                    .add_node_with_label(
                        &id_str,
                        &node.label,
                        &super::graph_node_label(&node.node_type),
                    )
                    .map_err(|err| RedDBError::Query(err.to_string()))?;
                allowed_nodes.insert(id_str.clone());
                if let EntityData::Node(node_data) = &entity.data {
                    node_properties.insert(id_str, node_data.properties.clone());
                }
            }
        }

        // Second pass — gather edges. An edge appears only when both
        // endpoint nodes survived the RLS pass AND the edge itself
        // passes its own RLS gate.
        for collection in &collections {
            let Some(manager) = store.get_collection(collection) else {
                continue;
            };
            let entities = manager.query_all(|_| true);
            for entity in entities {
                if !entity_visible_with_context(snap_ctx.as_ref(), &entity) {
                    continue;
                }
                let EntityKind::GraphEdge(ref edge) = entity.kind else {
                    continue;
                };
                if !allowed_nodes.contains(&edge.from_node)
                    || !allowed_nodes.contains(&edge.to_node)
                {
                    continue;
                }
                if !edge_passes_rls(self, collection, role.as_deref(), &mut edge_rls, &entity) {
                    continue;
                }
                let weight = match &entity.data {
                    EntityData::Edge(e) => e.weight,
                    _ => edge.weight as f32 / 1000.0,
                };
                let edge_label = super::graph_edge_label(&edge.label);
                graph
                    .add_edge_with_label(&edge.from_node, &edge.to_node, &edge_label, weight)
                    .map_err(|err| RedDBError::Query(err.to_string()))?;
                if let EntityData::Edge(edge_data) = &entity.data {
                    edge_properties.insert(
                        (edge.from_node.clone(), edge_label, edge.to_node.clone()),
                        edge_data.properties.clone(),
                    );
                }
            }
        }

        // Suppress unused-PolicyAction/PolicyTargetKind warnings — both
        // are used inside the helper closures via the per-kind helpers
        // declared at the bottom of this file.
        let _ = (PolicyAction::Select, PolicyTargetKind::Nodes);

        Ok((graph, node_properties, edge_properties))
    }

    /// Phase 1.1 MVCC universal: post-save hook that stamps `xmin` on a
    /// freshly-inserted entity when the current connection holds an
    /// open transaction. Used by graph / vector / queue / timeseries
    /// write paths that go through the DevX builder API (`db.node(...)
    /// .save()` and friends) — those live in the storage crate and
    /// can't reach `current_xid()` without crossing layers, so the
    /// application layer calls this helper right after `save()` to
    /// finalise the MVCC stamp.
    ///
    /// Autocommit (outside BEGIN) is a no-op — no extra lookup or
    /// write, so the non-transactional hot path stays untouched.
    ///
    /// Best-effort: if the collection or entity disappears between
    /// the save and the stamp (concurrent DROP), we silently skip.
    pub(crate) fn stamp_xmin_if_in_txn(
        &self,
        collection: &str,
        id: crate::storage::unified::entity::EntityId,
    ) {
        let Some(xid) = self.current_xid() else {
            return;
        };
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(collection) else {
            return;
        };
        if let Some(mut entity) = manager.get(id) {
            entity.set_xmin(xid);
            let _ = manager.update(entity);
        }
    }

    /// Revive tombstones stamped by `stamper_xid` or any sub-xid
    /// allocated after it (Phase 2.3.2e savepoint rollback). Any
    /// pending entries with `xid < stamper_xid` stay queued because
    /// they belong to the enclosing scope — they'll either flush on
    /// COMMIT or revive on an outer ROLLBACK TO SAVEPOINT.
    ///
    /// Returns the number of tuples whose `xmax` was wiped back to 0.
    pub(crate) fn revive_tombstones_since(&self, conn_id: u64, stamper_xid: u64) -> usize {
        let mut guard = self.inner.pending_tombstones.write();
        let Some(pending) = guard.get_mut(&conn_id) else {
            return 0;
        };

        let store = self.inner.db.store();
        let mut revived = 0usize;
        pending.retain(|(collection, id, xid, previous_xmax)| {
            if *xid < stamper_xid {
                // Stamped before the savepoint — keep in queue.
                return true;
            }
            if let Some(manager) = store.get_collection(collection) {
                if let Some(mut entity) = manager.get(*id) {
                    if entity.xmax == *xid {
                        entity.set_xmax(*previous_xmax);
                        let _ = manager.update(entity);
                        revived += 1;
                    }
                }
            }
            false
        });
        if pending.is_empty() {
            guard.remove(&conn_id);
        }
        revived
    }

    /// Return the snapshot the current connection should use for visibility
    /// checks (Phase 2.3 PG parity).
    ///
    /// * If the connection is inside a BEGIN-wrapped transaction, reuse
    ///   the snapshot stored in its `TxnContext`.
    /// * Otherwise (autocommit), capture a fresh snapshot tied to an
    ///   implicit xid=0 — the read path treats pre-MVCC rows as always
    ///   visible so this degrades to "see everything committed".
    pub fn current_snapshot(&self) -> crate::storage::transaction::snapshot::Snapshot {
        let conn_id = current_connection_id();
        if let Some(ctx) = self.inner.tx_contexts.read().get(&conn_id).cloned() {
            return ctx.snapshot;
        }
        // Autocommit: take a fresh snapshot bounded by `peek_next_xid` so
        // every already-committed xid (which is strictly less) passes the
        // `xmin <= snap.xid` gate, while concurrently-active xids land in
        // the `in_progress` set and stay hidden until they commit. Using
        // xid=0 would incorrectly hide every MVCC-stamped tuple.
        let high_water = self.inner.snapshot_manager.peek_next_xid();
        self.inner.snapshot_manager.snapshot(high_water)
    }

    /// Xid of the current connection's active transaction, or `None` when
    /// running outside a BEGIN/COMMIT block. Write paths call this to
    /// decide whether to stamp `xmin`/`xmax` on tuples.
    /// Phase 2.3.2e: when a savepoint is open, `writer_xid` returns the
    /// sub-xid so new writes can be selectively rolled back. Otherwise
    /// the parent txn's xid is returned, matching pre-savepoint
    /// behaviour. Callers that need the enclosing *transaction* xid
    /// (e.g. VACUUM min-active calculations) should read `ctx.xid`
    /// directly.
    pub fn current_xid(&self) -> Option<crate::storage::transaction::snapshot::Xid> {
        let conn_id = current_connection_id();
        self.inner
            .tx_contexts
            .read()
            .get(&conn_id)
            .map(|ctx| ctx.writer_xid())
    }

    /// `true` when the given connection id has an open `BEGIN`. Issue
    /// #760 — `OpenStream` consults this to refuse output streams that
    /// would otherwise collide with an interactive transaction (see
    /// ADR 0029 "Transaction interaction"). HTTP requests pre-dating the
    /// connection-id plumbing run with id `0`, which never carries a
    /// transaction context, so this returns `false` on those paths.
    pub fn connection_in_transaction(&self, conn_id: u64) -> bool {
        self.inner.tx_contexts.read().contains_key(&conn_id)
    }

    /// Access the shared `SnapshotManager` — useful for VACUUM to compute
    /// the oldest-active xid when reclaiming dead tuples.
    pub fn snapshot_manager(&self) -> Arc<crate::storage::transaction::snapshot::SnapshotManager> {
        Arc::clone(&self.inner.snapshot_manager)
    }

    fn mvcc_vacuum_cutoff_xid(&self) -> crate::storage::transaction::snapshot::Xid {
        let manager = &self.inner.snapshot_manager;
        let next_xid = manager.peek_next_xid();
        let mut cutoff = next_xid;
        if let Some(oldest_active) = manager.oldest_active_xid() {
            cutoff = cutoff.min(oldest_active);
        }
        if let Some(oldest_pinned) = manager.oldest_pinned_xid() {
            cutoff = cutoff.min(oldest_pinned);
        }
        let retention_xids = self.config_u64("runtime.mvcc.vacuum_retention_xids", 0);
        if retention_xids > 0 {
            cutoff = cutoff.min(next_xid.saturating_sub(retention_xids));
        }
        cutoff
    }

    fn rebuild_runtime_indexes_for_table(&self, table: &str) -> RedDBResult<()> {
        let registered = self.inner.index_store.list_indices(table);
        if registered.is_empty() {
            return Ok(());
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(table) else {
            return Ok(());
        };
        let entity_fields = manager
            .query_all(|entity| matches!(entity.kind, crate::storage::EntityKind::TableRow { .. }))
            .into_iter()
            .map(|entity| (entity.id, table_row_index_fields(&entity)))
            .collect::<Vec<_>>();

        for index in registered {
            self.inner.index_store.drop_index(&index.name, table);
            self.inner
                .index_store
                .create_index(
                    &index.name,
                    table,
                    &index.columns,
                    index.method,
                    index.unique,
                    &entity_fields,
                )
                .map_err(RedDBError::Internal)?;
            self.inner.index_store.register(index);
        }
        self.invalidate_plan_cache();
        Ok(())
    }

    pub(crate) fn persist_runtime_index_descriptor(
        &self,
        index: super::index_store::RegisteredIndex,
    ) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let _ = store.get_or_create_collection(RUNTIME_INDEX_REGISTRY_COLLECTION);
        let entity = crate::storage::UnifiedEntity::new(
            crate::storage::EntityId::new(0),
            crate::storage::EntityKind::TableRow {
                table: std::sync::Arc::from(RUNTIME_INDEX_REGISTRY_COLLECTION),
                row_id: 0,
            },
            crate::storage::EntityData::Row(crate::storage::RowData {
                columns: Vec::new(),
                named: Some(
                    [
                        (
                            "collection".to_string(),
                            crate::storage::schema::Value::text(index.collection.clone()),
                        ),
                        (
                            "name".to_string(),
                            crate::storage::schema::Value::text(index.name.clone()),
                        ),
                        (
                            "columns".to_string(),
                            crate::storage::schema::Value::text(index.columns.join("\u{1f}")),
                        ),
                        (
                            "method".to_string(),
                            crate::storage::schema::Value::text(index_method_kind_as_str(
                                index.method,
                            )),
                        ),
                        (
                            "unique".to_string(),
                            crate::storage::schema::Value::Boolean(index.unique),
                        ),
                        (
                            "dropped".to_string(),
                            crate::storage::schema::Value::Boolean(false),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                ),
                schema: None,
            }),
        );
        store
            .insert_auto(RUNTIME_INDEX_REGISTRY_COLLECTION, entity)
            .map(|_| ())
            .map_err(|err| RedDBError::Internal(format!("{err:?}")))
    }

    pub(crate) fn persist_runtime_index_drop(
        &self,
        collection: &str,
        name: &str,
    ) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let _ = store.get_or_create_collection(RUNTIME_INDEX_REGISTRY_COLLECTION);
        let entity = crate::storage::UnifiedEntity::new(
            crate::storage::EntityId::new(0),
            crate::storage::EntityKind::TableRow {
                table: std::sync::Arc::from(RUNTIME_INDEX_REGISTRY_COLLECTION),
                row_id: 0,
            },
            crate::storage::EntityData::Row(crate::storage::RowData {
                columns: Vec::new(),
                named: Some(
                    [
                        (
                            "collection".to_string(),
                            crate::storage::schema::Value::text(collection.to_string()),
                        ),
                        (
                            "name".to_string(),
                            crate::storage::schema::Value::text(name.to_string()),
                        ),
                        (
                            "dropped".to_string(),
                            crate::storage::schema::Value::Boolean(true),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                ),
                schema: None,
            }),
        );
        store
            .insert_auto(RUNTIME_INDEX_REGISTRY_COLLECTION, entity)
            .map(|_| ())
            .map_err(|err| RedDBError::Internal(format!("{err:?}")))
    }

    fn rehydrate_runtime_index_registry(&self) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(RUNTIME_INDEX_REGISTRY_COLLECTION) else {
            return Ok(());
        };
        let mut rows = manager.query_all(|_| true);
        rows.sort_by_key(|entity| entity.id.raw());

        let mut latest = std::collections::HashMap::<
            (String, String),
            Option<super::index_store::RegisteredIndex>,
        >::new();
        for entity in rows {
            let crate::storage::EntityData::Row(row) = &entity.data else {
                continue;
            };
            let Some(named) = &row.named else {
                continue;
            };
            let Some(collection) = named_text(named, "collection") else {
                continue;
            };
            let Some(name) = named_text(named, "name") else {
                continue;
            };
            let dropped = named_bool(named, "dropped").unwrap_or(false);
            let key = (collection.clone(), name.clone());
            if dropped {
                latest.insert(key, None);
                continue;
            }
            let columns = named_text(named, "columns")
                .map(|raw| {
                    raw.split('\u{1f}')
                        .filter(|part| !part.is_empty())
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let Some(method) =
                named_text(named, "method").and_then(|raw| index_method_kind_from_str(&raw))
            else {
                continue;
            };
            latest.insert(
                key,
                Some(super::index_store::RegisteredIndex {
                    name,
                    collection,
                    columns,
                    method,
                    unique: named_bool(named, "unique").unwrap_or(false),
                }),
            );
        }

        for index in latest.into_values().flatten() {
            let Some(manager) = store.get_collection(&index.collection) else {
                continue;
            };
            let entity_fields = manager
                .query_all(|entity| {
                    matches!(entity.kind, crate::storage::EntityKind::TableRow { .. })
                })
                .into_iter()
                .map(|entity| (entity.id, table_row_index_fields(&entity)))
                .collect::<Vec<_>>();
            self.inner
                .index_store
                .create_index(
                    &index.name,
                    &index.collection,
                    &index.columns,
                    index.method,
                    index.unique,
                    &entity_fields,
                )
                .map_err(RedDBError::Internal)?;
            self.inner.index_store.register(index);
        }
        self.invalidate_plan_cache();
        Ok(())
    }

    /// Own-tx xids (parent + open/released savepoints) for the current
    /// connection. Transports + tests that build a `SnapshotContext`
    /// manually (outside the `execute_query` scope) need this set so
    /// the writer's own uncommitted tuples stay visible to self.
    pub fn current_txn_own_xids(
        &self,
    ) -> std::collections::HashSet<crate::storage::transaction::snapshot::Xid> {
        let mut set = std::collections::HashSet::new();
        if let Some(ctx) = self.inner.tx_contexts.read().get(&current_connection_id()) {
            set.insert(ctx.xid);
            for (_, sub) in &ctx.savepoints {
                set.insert(*sub);
            }
            for sub in &ctx.released_sub_xids {
                set.insert(*sub);
            }
        }
        set
    }

    /// Access the shared `ForeignTableRegistry` (Phase 3.2 PG parity).
    ///
    /// Callers use this to check whether a table name is a registered
    /// foreign table (`registry.is_foreign_table(name)`) and, if so, to
    /// scan it (`registry.scan(name)`). The read-path rewriter consults
    /// this before dispatching into native-collection lookup.
    pub fn foreign_tables(&self) -> Arc<crate::storage::fdw::ForeignTableRegistry> {
        Arc::clone(&self.inner.foreign_tables)
    }

    /// Is Row-Level Security enabled for this table? (Phase 2.5 PG parity)
    pub fn is_rls_enabled(&self, table: &str) -> bool {
        self.inner.rls_enabled_tables.read().contains(table)
    }

    /// Collect the USING predicates that apply to this `(table, role, action)`.
    ///
    /// Returned filters should be OR-combined (a row passes RLS when *any*
    /// matching policy accepts it) and then AND-ed into the query's WHERE.
    /// When the table has RLS disabled this returns an empty Vec — callers
    /// can fast-path back to the unfiltered read.
    pub fn matching_rls_policies(
        &self,
        table: &str,
        role: Option<&str>,
        action: crate::storage::query::ast::PolicyAction,
    ) -> Vec<crate::storage::query::ast::Filter> {
        // Default kind = Table preserves the pre-Phase-2.5.5 behaviour:
        // callers that don't name a kind only see Table-scoped
        // policies (which is what execute SELECT / UPDATE / DELETE
        // expect).
        self.matching_rls_policies_for_kind(
            table,
            role,
            action,
            crate::storage::query::ast::PolicyTargetKind::Table,
        )
    }

    /// Kind-aware variant used by cross-model scans (Phase 2.5.5).
    ///
    /// Graph scans request `Nodes` / `Edges`, vector ANN requests
    /// `Vectors`, queue consumers request `Messages`, and timeseries
    /// range scans request `Points`. Policies tagged with a
    /// different kind are skipped so a graph-scoped policy doesn't
    /// accidentally gate a table SELECT on the same collection.
    pub fn matching_rls_policies_for_kind(
        &self,
        table: &str,
        role: Option<&str>,
        action: crate::storage::query::ast::PolicyAction,
        kind: crate::storage::query::ast::PolicyTargetKind,
    ) -> Vec<crate::storage::query::ast::Filter> {
        if !self.is_rls_enabled(table) {
            return Vec::new();
        }
        let policies = self.inner.rls_policies.read();
        policies
            .iter()
            .filter_map(|((t, _), p)| {
                if t != table {
                    return None;
                }
                // Kind gate — Table policies also apply to every
                // other kind *iff* the policy predicate evaluates
                // against entity fields that exist uniformly; the
                // caller's kind filter is the stricter check, so
                // match literally. Auto-tenancy policies stamp
                // Table and the caller passes the concrete kind —
                // we allow Table policies to apply cross-kind for
                // backwards compat.
                if p.target_kind != kind
                    && p.target_kind != crate::storage::query::ast::PolicyTargetKind::Table
                {
                    return None;
                }
                // Action gate — `None` means "ALL" actions.
                if let Some(a) = p.action {
                    if a != action {
                        return None;
                    }
                }
                // Role gate — `None` means "any role".
                if let Some(p_role) = p.role.as_deref() {
                    match role {
                        Some(r) if r == p_role => {}
                        _ => return None,
                    }
                }
                Some((*p.using).clone())
            })
            .collect()
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

    /// Wrap the planner's `RuntimeQueryExplain` as rows on a
    /// `RuntimeQueryResult` so callers over the SQL interface see the
    /// plan tree in the same shape a SELECT produces.
    ///
    /// Columns: `op`, `source`, `est_rows`, `est_cost`, `depth`.
    /// Nodes are walked depth-first; `depth` counts from 0 at the
    /// root so a text renderer can indent without re-walking.
    fn explain_as_rows(&self, raw_query: &str, inner_sql: &str) -> RedDBResult<RuntimeQueryResult> {
        let explain = self.explain_query(inner_sql)?;

        let columns = vec![
            "op".to_string(),
            "source".to_string(),
            "est_rows".to_string(),
            "est_cost".to_string(),
            "depth".to_string(),
        ];

        let mut records: Vec<crate::storage::query::unified::UnifiedRecord> = Vec::new();

        // Prepend `CteScan` markers when the query carried a leading
        // WITH clause. The CTE bodies are already inlined into the
        // main plan tree, but operators reading EXPLAIN need to see
        // which named CTEs were resolved — without this row the plan
        // would look indistinguishable from a hand-inlined query.
        for name in &explain.cte_materializations {
            use std::sync::Arc;
            let mut rec = crate::storage::query::unified::UnifiedRecord::default();
            rec.set_arc(Arc::from("op"), Value::text("CteScan".to_string()));
            rec.set_arc(Arc::from("source"), Value::text(name.clone()));
            rec.set_arc(Arc::from("est_rows"), Value::Float(0.0));
            rec.set_arc(Arc::from("est_cost"), Value::Float(0.0));
            rec.set_arc(Arc::from("depth"), Value::Integer(0));
            records.push(rec);
        }

        walk_plan_node(&explain.logical_plan.root, 0, &mut records);

        let result = crate::storage::query::unified::UnifiedResult {
            columns,
            records,
            stats: Default::default(),
            pre_serialized_json: None,
        };

        Ok(RuntimeQueryResult {
            query: raw_query.to_string(),
            mode: explain.mode,
            statement: "explain",
            engine: "runtime-explain",
            result,
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
        })
    }

    // -----------------------------------------------------------------
    // Granular RBAC — privilege gate + GRANT/REVOKE/ALTER USER dispatch
    // -----------------------------------------------------------------

    /// Project a `QueryExpr` to the (action, resource) pair the
    /// privilege engine cares about. Returns `Ok(())` for statements
    /// that don't touch user data (transaction control, SHOW, SET, etc.).
    pub(crate) fn check_query_privilege(
        &self,
        expr: &crate::storage::query::ast::QueryExpr,
    ) -> Result<(), String> {
        use crate::auth::privileges::{Action, AuthzContext, Resource};
        use crate::auth::UserId;
        use crate::storage::query::ast::QueryExpr;

        // No auth store wired (embedded mode / fresh DB / tests) → bypass.
        // The bootstrap path itself goes through `execute_query` so this
        // is the only sensible default; once auth is wired, the gate
        // becomes active.
        let auth_store = match self.inner.auth_store.read().clone() {
            Some(s) => s,
            None => return Ok(()),
        };

        // Resolve principal + role from the thread-local identity.
        // Anonymous (no identity) is allowed to read the bootstrap path
        // only when auth_store says so; we treat missing identity as
        // platform-admin-equivalent here so embedded test harnesses
        // continue to work without setting an identity.
        let (username, role) = match current_auth_identity() {
            Some(p) => p,
            None => return Ok(()),
        };
        let tenant = current_tenant();

        let ctx = AuthzContext {
            principal: &username,
            effective_role: role,
            tenant: tenant.as_deref(),
        };
        let principal_id = UserId::from_parts(tenant.as_deref(), &username);

        // Map QueryExpr → (Action, Resource).
        let (action, resource) = match expr {
            QueryExpr::Table(t) => (Action::Select, Resource::table_from_name(&t.table)),
            QueryExpr::RankOf(_) | QueryExpr::ApproxRankOf(_) | QueryExpr::RankRange(_) => {
                (Action::Select, Resource::Database)
            }
            QueryExpr::QueueSelect(q) => {
                return self.check_queue_op_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "queue:peek",
                    &q.queue,
                );
            }
            QueryExpr::QueueCommand(cmd) => {
                use crate::storage::query::ast::QueueCommand;
                let (queue, action_verb) = match cmd {
                    QueueCommand::Push { queue, .. } => (queue.as_str(), "queue:enqueue"),
                    QueueCommand::Pop { queue, .. }
                    | QueueCommand::GroupRead { queue, .. }
                    | QueueCommand::Claim { queue, .. } => (queue.as_str(), "queue:read"),
                    QueueCommand::Peek { queue, .. }
                    | QueueCommand::Len { queue }
                    | QueueCommand::Pending { queue, .. } => (queue.as_str(), "queue:peek"),
                    QueueCommand::Ack { queue, .. } => (queue.as_str(), "queue:ack"),
                    QueueCommand::Nack {
                        queue, delay_ms, ..
                    } => {
                        // Per-failure retry overrides re-shape retry
                        // behaviour for everyone draining the queue and
                        // gate on the dedicated `queue:retry` verb so
                        // operators can grant base NACK without granting
                        // the override capability.
                        let verb = if delay_ms.is_some() {
                            "queue:retry"
                        } else {
                            "queue:nack"
                        };
                        (queue.as_str(), verb)
                    }
                    QueueCommand::Purge { queue } => (queue.as_str(), "queue:purge"),
                    // `GroupCreate` is part of the consumer-setup
                    // surface — read-side, never destructive.
                    QueueCommand::GroupCreate { queue, .. } => (queue.as_str(), "queue:read"),
                    QueueCommand::Move { source, .. } => (source.as_str(), "queue:dlq:move"),
                };
                return self.check_queue_op_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    action_verb,
                    queue,
                );
            }
            QueryExpr::Graph(g) => {
                // MATCH … RETURN is the explorer's pattern-traversal
                // surface — gate on `graph:traverse` (#757).
                self.check_graph_op_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "graph:traverse",
                )?;
                if auth_store.iam_authorization_enabled() {
                    self.check_graph_property_projection_privilege(
                        &auth_store,
                        &principal_id,
                        role,
                        tenant.as_deref(),
                        g,
                    )?;
                    return Ok(());
                }
                return Ok(());
            }
            QueryExpr::Path(_) => {
                // PATH FROM … TO … is a path-traversal query — gates
                // on `graph:traverse` like neighborhood/shortest-path
                // (#757).
                return self.check_graph_op_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "graph:traverse",
                );
            }
            QueryExpr::GraphCommand(cmd) => {
                use crate::storage::query::ast::GraphCommand;
                let action_verb = match cmd {
                    // Metadata / property reads.
                    GraphCommand::Properties { .. } => "graph:read",
                    // Traversal / pattern-walk surface.
                    GraphCommand::Neighborhood { .. }
                    | GraphCommand::Traverse { .. }
                    | GraphCommand::ShortestPath { .. } => "graph:traverse",
                    // Analytics algorithms — expensive enough that Red
                    // UI needs to gate the runner independently of
                    // ordinary traversal.
                    GraphCommand::Centrality { .. }
                    | GraphCommand::Community { .. }
                    | GraphCommand::Components { .. }
                    | GraphCommand::Cycles { .. }
                    | GraphCommand::Clustering
                    | GraphCommand::TopologicalSort => "graph:algorithm:run",
                };
                return self.check_graph_op_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    action_verb,
                );
            }
            QueryExpr::Vector(v) => {
                if auth_store.iam_authorization_enabled() {
                    self.check_vector_op_privilege(
                        &auth_store,
                        &principal_id,
                        role,
                        tenant.as_deref(),
                        "vector:search",
                        &v.collection,
                    )?;
                    self.check_table_like_column_projection_privilege(
                        &auth_store,
                        &principal_id,
                        role,
                        tenant.as_deref(),
                        &v.collection,
                        &["content".to_string()],
                    )?;
                    return Ok(());
                }
                return Ok(());
            }
            QueryExpr::SearchCommand(cmd) => {
                use crate::storage::query::ast::SearchCommand;
                if auth_store.iam_authorization_enabled() {
                    // `SEARCH SIMILAR [..] COLLECTION <c>` and `SEARCH
                    // HYBRID ... COLLECTION <c>` are the same UI
                    // affordances as `VECTOR SEARCH` / hybrid joins —
                    // Red UI must see the same `vector:search` envelope
                    // so a single toolbar grant is sufficient.
                    let collection = match cmd {
                        SearchCommand::Similar { collection, .. }
                        | SearchCommand::Hybrid { collection, .. } => Some(collection.as_str()),
                        _ => None,
                    };
                    if let Some(c) = collection {
                        self.check_vector_op_privilege(
                            &auth_store,
                            &principal_id,
                            role,
                            tenant.as_deref(),
                            "vector:search",
                            c,
                        )?;
                        return Ok(());
                    }
                }
                return Ok(());
            }
            QueryExpr::Hybrid(h) => {
                if auth_store.iam_authorization_enabled() {
                    // The vector half of a hybrid search is gated under
                    // the same `vector:search` verb as a standalone
                    // VECTOR SEARCH — Red UI's hybrid-search toolbar
                    // must surface the same UI-safe denial envelope
                    // when the principal lacks the grant. The
                    // structured half is dispatched to its own gate via
                    // the inner query during execution.
                    self.check_vector_op_privilege(
                        &auth_store,
                        &principal_id,
                        role,
                        tenant.as_deref(),
                        "vector:search",
                        &h.vector.collection,
                    )?;
                    return Ok(());
                }
                return Ok(());
            }
            QueryExpr::Insert(i) => (Action::Insert, Resource::table_from_name(&i.table)),
            QueryExpr::Update(u) => (Action::Update, Resource::table_from_name(&u.table)),
            QueryExpr::Delete(d) => (Action::Delete, Resource::table_from_name(&d.table)),
            // Joins inherit the read privilege from any constituent
            // table — for now we emit a single Select on the database
            // (admins bypass; non-admins need a Database/Schema grant).
            QueryExpr::Join(_) => (Action::Select, Resource::Database),
            // GRANT / REVOKE / USER DDL are authority statements;
            // require Admin (the helper methods enforce).
            QueryExpr::Grant(_)
            | QueryExpr::Revoke(_)
            | QueryExpr::AlterUser(_)
            | QueryExpr::CreateUser(_) => {
                return if role == crate::auth::Role::Admin {
                    Ok(())
                } else {
                    Err(format!(
                        "principal=`{}` role=`{:?}` cannot issue ACL/auth DDL",
                        username, role
                    ))
                };
            }
            QueryExpr::CreateIamPolicy { id, .. } => {
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:put",
                    "policy",
                    id,
                );
            }
            QueryExpr::DropIamPolicy { id } => {
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:drop",
                    "policy",
                    id,
                );
            }
            QueryExpr::AttachPolicy { policy_id, .. } => {
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:attach",
                    "policy",
                    policy_id,
                );
            }
            QueryExpr::DetachPolicy { policy_id, .. } => {
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:detach",
                    "policy",
                    policy_id,
                );
            }
            QueryExpr::ShowPolicies { .. } | QueryExpr::ShowEffectivePermissions { .. } => {
                return Ok(());
            }
            QueryExpr::SimulatePolicy { .. } => {
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:simulate",
                    "policy",
                    "*",
                );
            }
            QueryExpr::LintPolicy { .. } => {
                // Linting is a read-only inspection — gate it like
                // simulate (policy management role).
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    "policy:simulate",
                    "policy",
                    "*",
                );
            }
            QueryExpr::MigratePolicyMode { dry_run, .. } => {
                // DRY RUN is a pre-flight inspection (policy:simulate).
                // The actual mode flip is a privileged mutation under
                // the policy:put action (it persists a new enforcement
                // mode to the vault KV through `set_enforcement_mode`).
                let action = if *dry_run {
                    "policy:simulate"
                } else {
                    "policy:put"
                };
                return self.check_policy_management_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    action,
                    "policy",
                    "*",
                );
            }
            // DROP and TRUNCATE — Write-role gate + per-collection IAM policy
            // when IAM mode is active. Other DDL stays role-only for now.
            QueryExpr::DropTable(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::DropGraph(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::DropVector(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::DropDocument(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::DropKv(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::DropCollection(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    &q.name,
                );
            }
            QueryExpr::Truncate(q) => {
                return self.check_ddl_collection_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "truncate",
                    &q.name,
                );
            }
            // Remaining DDL (#753) — hybrid policy-aware gate. Specific
            // create/alter/drop verbs gate operations with a clear
            // per-collection target so Red UI can author fine-grained
            // policies (`create on collection:users`). Namespace-level
            // and grouped DDL fall back to broader `schema:admin` /
            // `schema:write` verbs against a `schema:<name>` resource.
            // All branches share the [`check_ddl_object_privilege`]
            // helper so allows / denies produce the same structured
            // "principal=… action=… resource=<kind>:<name> denied by
            // IAM policy" reason the Red UI security read contracts
            // (#740) already render.
            QueryExpr::CreateTable(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateCollection(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateVector(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::AlterTable(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "alter",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateIndex(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.table,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropIndex(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    "collection",
                    &q.table,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateSchema(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "schema:admin",
                    "schema",
                    &q.name,
                    crate::auth::Role::Admin,
                );
            }
            QueryExpr::DropSchema(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "schema:admin",
                    "schema",
                    &q.name,
                    crate::auth::Role::Admin,
                );
            }
            QueryExpr::CreateSequence(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropSequence(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateView(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropView(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::RefreshMaterializedView(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "alter",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreatePolicy(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.table,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropPolicy(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    "collection",
                    &q.table,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateServer(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "schema:admin",
                    "schema",
                    &q.name,
                    crate::auth::Role::Admin,
                );
            }
            QueryExpr::DropServer(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "schema:admin",
                    "schema",
                    &q.name,
                    crate::auth::Role::Admin,
                );
            }
            QueryExpr::CreateForeignTable(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "schema:write",
                    "schema",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropForeignTable(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "schema:write",
                    "schema",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateTimeSeries(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateMetric(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.path,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::AlterMetric(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "alter",
                    "collection",
                    &q.path,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateSlo(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.path,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropTimeSeries(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateQueue(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::AlterQueue(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "alter",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropQueue(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    "collection",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::CreateTree(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "create",
                    "collection",
                    &q.collection,
                    crate::auth::Role::Write,
                );
            }
            QueryExpr::DropTree(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "drop",
                    "collection",
                    &q.collection,
                    crate::auth::Role::Write,
                );
            }
            // Migration DDL — CREATE MIGRATION is grouped DDL on the
            // schema namespace; uses the `schema:write` fallback verb
            // (no obvious per-collection target).
            QueryExpr::CreateMigration(q) => {
                return self.check_ddl_object_privilege(
                    &auth_store,
                    &principal_id,
                    role,
                    tenant.as_deref(),
                    &username,
                    "schema:write",
                    "schema",
                    &q.name,
                    crate::auth::Role::Write,
                );
            }
            // APPLY / ROLLBACK change data and schema — require Admin.
            QueryExpr::ApplyMigration(_) | QueryExpr::RollbackMigration(_) => {
                return if role == crate::auth::Role::Admin {
                    Ok(())
                } else {
                    Err(format!(
                        "principal=`{}` role=`{:?}` cannot issue APPLY/ROLLBACK MIGRATION",
                        username, role
                    ))
                };
            }
            // EXPLAIN MIGRATION is read-only — any authenticated principal.
            QueryExpr::ExplainMigration(_) => return Ok(()),
            // Everything else (SET, SHOW, transaction control, graph
            // commands, queue/tree commands, MaintenanceCommand …)
            // is allowed for any authenticated principal.
            _ => return Ok(()),
        };

        if auth_store.iam_authorization_enabled() {
            let iam_action = legacy_action_to_iam(action);
            let iam_resource = legacy_resource_to_iam(&resource, tenant.as_deref());
            let iam_ctx = runtime_iam_context(role, tenant.as_deref());
            if !auth_store.check_policy_authz_with_role(
                &principal_id,
                iam_action,
                &iam_resource,
                &iam_ctx,
                role,
            ) {
                return Err(format!(
                    "principal=`{}` action=`{}` resource=`{}:{}` denied by IAM policy",
                    username, iam_action, iam_resource.kind, iam_resource.name
                ));
            }

            if let QueryExpr::Table(table) = expr {
                self.check_table_column_projection_privilege(
                    &auth_store,
                    &principal_id,
                    &iam_ctx,
                    table,
                )?;
            }

            if let QueryExpr::Update(update) = expr {
                let columns = update_set_target_columns(update);
                if !columns.is_empty() {
                    let request = column_access_request_for_table_update(&update.table, columns);
                    let outcome =
                        auth_store.check_column_projection_authz(&principal_id, &request, &iam_ctx);
                    if let Some(denied) = outcome.first_denied_column() {
                        return Err(format!(
                            "principal=`{}` action=`{}` resource=`{}:{}` denied by IAM column policy",
                            username, iam_action, denied.resource.kind, denied.resource.name
                        ));
                    }
                    if !outcome.allowed() {
                        return Err(format!(
                            "principal=`{}` action=`{}` resource=`{}:{}` denied by IAM policy",
                            username,
                            iam_action,
                            outcome.table_resource.kind,
                            outcome.table_resource.name
                        ));
                    }
                }

                if let Some(columns) = update_returning_columns_for_policy(self, update) {
                    let request = column_access_request_for_table_select(&update.table, columns);
                    let outcome =
                        auth_store.check_column_projection_authz(&principal_id, &request, &iam_ctx);
                    if let Some(denied) = outcome.first_denied_column() {
                        return Err(format!(
                            "principal=`{}` action=`select` resource=`{}:{}` denied by IAM column policy",
                            username, denied.resource.kind, denied.resource.name
                        ));
                    }
                    if !outcome.allowed() {
                        return Err(format!(
                            "principal=`{}` action=`select` resource=`{}:{}` denied by IAM policy",
                            username, outcome.table_resource.kind, outcome.table_resource.name
                        ));
                    }
                }
            }

            Ok(())
        } else {
            auth_store
                .check_grant(&ctx, action, &resource)
                .map_err(|e| e.to_string())
        }
    }

    fn check_table_column_projection_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        ctx: &crate::auth::policies::EvalContext,
        table: &crate::storage::query::ast::TableQuery,
    ) -> Result<(), String> {
        use crate::auth::{ColumnAccessRequest, ColumnDecisionEffect};

        let columns = requested_table_columns_for_policy(table);
        if columns.is_empty() {
            return Ok(());
        }

        let request = ColumnAccessRequest::select(table.table.clone(), columns);
        let outcome = auth_store.check_column_projection_authz(principal, &request, ctx);
        if outcome.allowed() {
            return Ok(());
        }

        if !matches!(
            outcome.table_decision,
            crate::auth::policies::Decision::Allow { .. }
                | crate::auth::policies::Decision::AdminBypass
        ) {
            return Err(format!(
                "principal=`{}` action=`select` resource=`{}:{}` denied by IAM policy",
                principal, outcome.table_resource.kind, outcome.table_resource.name
            ));
        }

        let denied = outcome
            .first_denied_column()
            .filter(|decision| decision.effective == ColumnDecisionEffect::Denied);
        match denied {
            Some(decision) => Err(format!(
                "principal=`{}` action=`select` resource=`{}:{}` denied by IAM policy",
                principal, decision.resource.kind, decision.resource.name
            )),
            None => Ok(()),
        }
    }

    fn check_graph_property_projection_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        query: &crate::storage::query::ast::GraphQuery,
    ) -> Result<(), String> {
        let columns = explicit_graph_projection_properties(query);
        if columns.is_empty() {
            return Ok(());
        }
        self.check_table_like_column_projection_privilege(
            auth_store, principal, role, tenant, "graph", &columns,
        )
    }

    fn check_table_like_column_projection_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        table: &str,
        columns: &[String],
    ) -> Result<(), String> {
        let iam_ctx = runtime_iam_context(role, tenant);
        let request =
            crate::auth::ColumnAccessRequest::select(table.to_string(), columns.iter().cloned());
        let outcome = auth_store.check_column_projection_authz(principal, &request, &iam_ctx);
        if outcome.allowed() {
            return Ok(());
        }
        let denied = outcome
            .first_denied_column()
            .map(|d| d.resource.name.clone())
            .unwrap_or_else(|| format!("{table}.<unknown>"));
        Err(format!(
            "principal=`{}` action=`select` resource=`column:{}` denied by IAM policy",
            principal, denied
        ))
    }

    fn check_policy_management_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        action: &str,
        resource_kind: &str,
        resource_name: &str,
    ) -> Result<(), String> {
        let ctx = runtime_iam_context(role, tenant);

        if !auth_store.iam_authorization_enabled() {
            return if role == crate::auth::Role::Admin {
                Ok(())
            } else {
                Err(format!(
                    "principal=`{}` role=`{:?}` cannot issue ACL/auth DDL",
                    principal, role
                ))
            };
        }

        if resource_kind == "policy"
            && matches!(
                action,
                "policy:put" | "policy:drop" | "policy:attach" | "policy:detach"
            )
            && self
                .inner
                .config_registry
                .get_active(resource_name)
                .map(|entry| entry.managed)
                .unwrap_or(false)
        {
            return Ok(());
        }

        let mut resource = crate::auth::policies::ResourceRef::new(
            resource_kind.to_string(),
            resource_name.to_string(),
        );
        if let Some(t) = tenant {
            resource = resource.with_tenant(t.to_string());
        }
        if auth_store.check_policy_authz_with_role(principal, action, &resource, &ctx, role) {
            Ok(())
        } else {
            Err(format!(
                "principal=`{}` action=`{}` resource=`{}:{}` denied by IAM policy",
                principal, action, resource.kind, resource.name
            ))
        }
    }

    fn check_managed_config_write_for_set_config(&self, key: &str) -> RedDBResult<()> {
        let Some(auth_store) = self.inner.auth_store.read().clone() else {
            return Ok(());
        };
        let (username, role) = current_auth_identity()
            .unwrap_or_else(|| ("anonymous".to_string(), crate::auth::Role::Read));
        let tenant = current_tenant();
        let principal = crate::auth::UserId::from_parts(tenant.as_deref(), &username);
        let ctx = runtime_iam_context(role, tenant.as_deref());
        let gate = crate::auth::managed_config::ManagedConfigGate::new(
            self.inner.config_registry.as_ref(),
        );
        match gate.check_write(&auth_store, &principal, &ctx, key) {
            crate::auth::managed_config::ManagedConfigDecision::PassThrough { .. }
            | crate::auth::managed_config::ManagedConfigDecision::Allow { .. } => Ok(()),
            crate::auth::managed_config::ManagedConfigDecision::Deny { reason, .. } => {
                Err(RedDBError::Query(format!(
                    "permission denied: managed config mutation blocked for `{key}`: {reason}"
                )))
            }
        }
    }

    /// IAM privilege check for a granular queue operation (issue #755 /
    /// PRD #735).
    ///
    /// Each queue operation maps to a stable verb in
    /// [`crate::auth::action_catalog`] (`queue:enqueue`, `queue:read`,
    /// `queue:peek`, `queue:ack`, `queue:nack`, `queue:retry`,
    /// `queue:dlq:move`, `queue:purge`, `queue:presence:read`). The
    /// resource is `queue:<name>` scoped to the current tenant. In
    /// legacy mode (no IAM authorization configured) the check is a
    /// no-op — the role gates in `execute_queue_command` still apply
    /// and the legacy `select` / `write` grant table continues to
    /// govern queue access. In IAM-enabled mode a missing granular
    /// grant yields a structured, UI-safe error of the form
    /// `principal=… action=queue:… resource=queue:… denied by IAM
    /// policy` so Red UI can surface the failing toolbar action.
    fn check_queue_op_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        action: &str,
        queue: &str,
    ) -> Result<(), String> {
        if !auth_store.iam_authorization_enabled() {
            return Ok(());
        }
        let mut resource =
            crate::auth::policies::ResourceRef::new("queue".to_string(), queue.to_string());
        if let Some(t) = tenant {
            resource = resource.with_tenant(t.to_string());
        }
        let ctx = runtime_iam_context(role, tenant);
        if auth_store.check_policy_authz_with_role(principal, action, &resource, &ctx, role) {
            Ok(())
        } else {
            Err(format!(
                "principal=`{}` action=`{}` resource=`queue:{}` denied by IAM policy",
                principal, action, queue
            ))
        }
    }

    /// IAM privilege check for a graph operation (issue #757 / PRD
    /// #735).
    ///
    /// Each graph operation maps to a stable verb in
    /// [`crate::auth::action_catalog`] — `graph:read` for
    /// metadata/property lookups, `graph:traverse` for MATCH / PATH /
    /// NEIGHBORHOOD / TRAVERSE / SHORTEST_PATH, and
    /// `graph:algorithm:run` for analytics algorithms (centrality,
    /// community, components, cycles, clustering, topological sort).
    /// The resource is `graph:*` scoped to the current tenant — the
    /// runtime today operates on a singleton graph store so the name
    /// has no concrete identifier; policies grant the explorer
    /// surface by writing `graph:*` as the resource pattern.
    ///
    /// In legacy mode (no IAM authorization configured) the check is
    /// a no-op so the existing role-based defaults continue to
    /// govern. In IAM-enabled mode a missing grant produces the
    /// UI-safe envelope `principal=… action=graph:… resource=graph:*
    /// denied by IAM policy` Red UI keys on.
    fn check_graph_op_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        action: &str,
    ) -> Result<(), String> {
        if !auth_store.iam_authorization_enabled() {
            return Ok(());
        }
        let mut resource =
            crate::auth::policies::ResourceRef::new("graph".to_string(), "*".to_string());
        if let Some(t) = tenant {
            resource = resource.with_tenant(t.to_string());
        }
        let ctx = runtime_iam_context(role, tenant);
        if auth_store.check_policy_authz_with_role(principal, action, &resource, &ctx, role) {
            Ok(())
        } else {
            Err(format!(
                "principal=`{}` action=`{}` resource=`graph:*` denied by IAM policy",
                principal, action
            ))
        }
    }

    /// IAM privilege check for a granular vector operation (issue #756
    /// / PRD #735).
    ///
    /// Each vector operation maps to a stable verb in
    /// [`crate::auth::action_catalog`] (`vector:read`, `vector:search`,
    /// `vector:artifact:read`, `vector:artifact:rebuild`,
    /// `vector:admin`). The resource is `vector:<collection>` scoped to
    /// the current tenant. In legacy mode (no IAM authorization
    /// configured) the check is a no-op — the role gates and existing
    /// `select` / column-projection grants continue to govern access.
    /// In IAM-enabled mode a missing granular grant yields a
    /// structured, UI-safe error of the form `principal=…
    /// action=vector:… resource=vector:… denied by IAM policy` so Red
    /// UI can surface the failing toolbar action.
    fn check_vector_op_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        action: &str,
        collection: &str,
    ) -> Result<(), String> {
        if !auth_store.iam_authorization_enabled() {
            return Ok(());
        }
        let mut resource =
            crate::auth::policies::ResourceRef::new("vector".to_string(), collection.to_string());
        if let Some(t) = tenant {
            resource = resource.with_tenant(t.to_string());
        }
        let ctx = runtime_iam_context(role, tenant);
        if auth_store.check_policy_authz_with_role(principal, action, &resource, &ctx, role) {
            Ok(())
        } else {
            Err(format!(
                "principal=`{}` action=`{}` resource=`vector:{}` denied by IAM policy",
                principal, action, collection
            ))
        }
    }

    /// IAM privilege check for DROP / TRUNCATE on a named collection.
    ///
    /// Delegates to [`check_ddl_object_privilege`] with `resource_kind =
    /// "collection"`. Kept as a thin wrapper so the existing DROP/TRUNCATE
    /// callsites stay readable.
    fn check_ddl_collection_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        username: &str,
        action: &str,
        collection: &str,
    ) -> Result<(), String> {
        self.check_ddl_object_privilege(
            auth_store,
            principal,
            role,
            tenant,
            username,
            action,
            "collection",
            collection,
            crate::auth::Role::Write,
        )
    }

    /// Generalised IAM privilege check for DDL on a named object.
    ///
    /// `action` is the stable verb advertised through the action catalog
    /// (`create`, `alter`, `drop`, `truncate`, `schema:write`,
    /// `schema:admin`). `resource_kind` / `resource_name` form the policy
    /// resource (`collection:<name>`, `schema:<name>`). `min_role` is the
    /// legacy gate when IAM is not yet enabled.
    ///
    /// Behaviour:
    /// * Role below `min_role` → structured "principal=… role=… cannot
    ///   issue DDL" denial, audit recorded.
    /// * IAM disabled → audit-record success and allow (legacy path).
    /// * IAM enabled → call `check_policy_authz_with_role`. Explicit Deny
    ///   and DefaultDeny in PolicyOnly mode both produce a UI-safe
    ///   "principal=… action=… resource=<kind>:<name> denied by IAM
    ///   policy" string. Explicit Allow and the LegacyRbac fallback
    ///   allow the action.
    #[allow(clippy::too_many_arguments)]
    fn check_ddl_object_privilege(
        &self,
        auth_store: &Arc<crate::auth::store::AuthStore>,
        principal: &crate::auth::UserId,
        role: crate::auth::Role,
        tenant: Option<&str>,
        username: &str,
        action: &str,
        resource_kind: &str,
        resource_name: &str,
        min_role: crate::auth::Role,
    ) -> Result<(), String> {
        if role < min_role {
            let msg = format!(
                "principal=`{}` role=`{:?}` cannot issue DDL action=`{}` resource=`{}:{}`",
                username, role, action, resource_kind, resource_name
            );
            self.inner.audit_log.record(
                action,
                username,
                resource_name,
                "denied",
                crate::json::Value::Null,
            );
            return Err(msg);
        }

        if !auth_store.iam_authorization_enabled() {
            self.inner.audit_log.record(
                action,
                username,
                resource_name,
                "ok",
                crate::json::Value::Null,
            );
            return Ok(());
        }

        let mut resource = crate::auth::policies::ResourceRef::new(
            resource_kind.to_string(),
            resource_name.to_string(),
        );
        if let Some(t) = tenant {
            resource = resource.with_tenant(t.to_string());
        }
        let ctx = runtime_iam_context(role, tenant);
        if auth_store.check_policy_authz_with_role(principal, action, &resource, &ctx, role) {
            self.inner.audit_log.record(
                action,
                username,
                resource_name,
                "ok",
                crate::json::Value::Null,
            );
            Ok(())
        } else {
            self.inner.audit_log.record(
                action,
                username,
                resource_name,
                "denied",
                crate::json::Value::Null,
            );
            Err(format!(
                "principal=`{}` action=`{}` resource=`{}:{}` denied by IAM policy",
                username, action, resource_kind, resource_name
            ))
        }
    }

    /// Translate the parsed [`GrantStmt`] into AuthStore mutations.
    fn execute_grant_statement(
        &self,
        query: &str,
        stmt: &crate::storage::query::ast::GrantStmt,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::privileges::{Action, GrantPrincipal, Resource};
        use crate::auth::UserId;
        use crate::storage::query::ast::{GrantObjectKind, GrantPrincipalRef};

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        // Granter identity + role.
        let (gname, grole) = current_auth_identity().ok_or_else(|| {
            RedDBError::Query("GRANT requires an authenticated principal".to_string())
        })?;
        let granter = UserId::from_parts(current_tenant().as_deref(), &gname);
        let granter_role = grole;

        // Build the action set.
        let mut actions: Vec<Action> = Vec::new();
        if stmt.all {
            actions.push(Action::All);
        } else {
            for kw in &stmt.actions {
                let a = Action::from_keyword(kw).ok_or_else(|| {
                    RedDBError::Query(format!("unknown privilege keyword `{}`", kw))
                })?;
                actions.push(a);
            }
        }

        // Audit emit (printed; structured emission is Agent #4's lane).
        let mut applied = 0usize;
        for obj in &stmt.objects {
            let resource = match stmt.object_kind {
                GrantObjectKind::Table => Resource::Table {
                    schema: obj.schema.clone(),
                    table: obj.name.clone(),
                },
                GrantObjectKind::Schema => Resource::Schema(obj.name.clone()),
                GrantObjectKind::Database => Resource::Database,
                GrantObjectKind::Function => Resource::Function {
                    schema: obj.schema.clone(),
                    name: obj.name.clone(),
                },
            };
            for principal in &stmt.principals {
                let p = match principal {
                    GrantPrincipalRef::Public => GrantPrincipal::Public,
                    GrantPrincipalRef::Group(g) => GrantPrincipal::Group(g.clone()),
                    GrantPrincipalRef::User { tenant, name } => {
                        GrantPrincipal::User(UserId::from_parts(tenant.as_deref(), name))
                    }
                };
                // Tenant of the grant follows the granter's tenant
                // (cross-tenant guard inside `AuthStore::grant`).
                let tenant = granter.tenant.clone();
                auth_store
                    .grant(
                        &granter,
                        granter_role,
                        p.clone(),
                        resource.clone(),
                        actions.clone(),
                        stmt.with_grant_option,
                        tenant.clone(),
                    )
                    .map_err(|e| RedDBError::Query(e.to_string()))?;

                // IAM policy translation: every GRANT also lands as a
                // synthetic `_grant_<id>` policy attached to the
                // principal so the new evaluator sees it.
                if let Some(policy) =
                    grant_to_iam_policy(&p, &resource, &actions, tenant.as_deref())
                {
                    let pid = policy.id.clone();
                    auth_store
                        .put_policy_internal(policy)
                        .map_err(|e| RedDBError::Query(e.to_string()))?;
                    let attachment = match &p {
                        GrantPrincipal::User(uid) => {
                            crate::auth::store::PrincipalRef::User(uid.clone())
                        }
                        GrantPrincipal::Group(group) => {
                            crate::auth::store::PrincipalRef::Group(group.clone())
                        }
                        GrantPrincipal::Public => crate::auth::store::PrincipalRef::Group(
                            crate::auth::store::PUBLIC_IAM_GROUP.to_string(),
                        ),
                    };
                    auth_store
                        .attach_policy(attachment, &pid)
                        .map_err(|e| RedDBError::Query(e.to_string()))?;
                }
                applied += 1;
                tracing::info!(
                    target: "audit",
                    principal = %granter,
                    action = "grant",
                    "GRANT applied"
                );
            }
        }

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("GRANT applied to {} target(s)", applied),
            "grant",
        ))
    }

    /// Translate the parsed [`RevokeStmt`] into AuthStore mutations.
    fn execute_revoke_statement(
        &self,
        query: &str,
        stmt: &crate::storage::query::ast::RevokeStmt,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::privileges::{Action, GrantPrincipal, Resource};
        use crate::auth::UserId;
        use crate::storage::query::ast::{GrantObjectKind, GrantPrincipalRef};

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        let (_gname, grole) = current_auth_identity().ok_or_else(|| {
            RedDBError::Query("REVOKE requires an authenticated principal".to_string())
        })?;
        let granter_role = grole;

        let actions: Vec<Action> = if stmt.all {
            vec![Action::All]
        } else {
            stmt.actions
                .iter()
                .map(|kw| Action::from_keyword(kw).unwrap_or(Action::Select))
                .collect()
        };

        let mut total_removed = 0usize;
        for obj in &stmt.objects {
            let resource = match stmt.object_kind {
                GrantObjectKind::Table => Resource::Table {
                    schema: obj.schema.clone(),
                    table: obj.name.clone(),
                },
                GrantObjectKind::Schema => Resource::Schema(obj.name.clone()),
                GrantObjectKind::Database => Resource::Database,
                GrantObjectKind::Function => Resource::Function {
                    schema: obj.schema.clone(),
                    name: obj.name.clone(),
                },
            };
            for principal in &stmt.principals {
                let p = match principal {
                    GrantPrincipalRef::Public => GrantPrincipal::Public,
                    GrantPrincipalRef::Group(g) => GrantPrincipal::Group(g.clone()),
                    GrantPrincipalRef::User { tenant, name } => {
                        GrantPrincipal::User(UserId::from_parts(tenant.as_deref(), name))
                    }
                };
                let removed = auth_store
                    .revoke(granter_role, &p, &resource, &actions)
                    .map_err(|e| RedDBError::Query(e.to_string()))?;
                let _removed_policies =
                    auth_store.delete_synthetic_grant_policies(&p, &resource, &actions);
                total_removed += removed;
            }
        }

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("REVOKE removed {} grant(s)", total_removed),
            "revoke",
        ))
    }

    /// Translate the parsed [`CreateUserStmt`] into an AuthStore user.
    fn execute_create_user_statement(
        &self,
        query: &str,
        stmt: &crate::storage::query::ast::CreateUserStmt,
    ) -> RedDBResult<RuntimeQueryResult> {
        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        let (_gname, grole) = current_auth_identity().ok_or_else(|| {
            RedDBError::Query("CREATE USER requires an authenticated principal".to_string())
        })?;
        if grole != crate::auth::Role::Admin {
            return Err(RedDBError::Query(
                "CREATE USER requires Admin role".to_string(),
            ));
        }

        let role = crate::auth::Role::from_str(&stmt.role)
            .ok_or_else(|| RedDBError::Query(format!("invalid role `{}`", stmt.role)))?;
        let user = auth_store
            .create_user_in_tenant(stmt.tenant.as_deref(), &stmt.username, &stmt.password, role)
            .map_err(|e| RedDBError::Query(e.to_string()))?;

        self.invalidate_result_cache();
        let target = crate::auth::UserId::from_parts(user.tenant_id.as_deref(), &user.username);
        tracing::info!(
            target: "audit",
            principal = %target,
            role = %role,
            action = "create_user",
            "CREATE USER applied"
        );

        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("CREATE USER {} applied", target),
            "create_user",
        ))
    }

    /// Translate the parsed [`AlterUserStmt`] into AuthStore mutations.
    fn execute_alter_user_statement(
        &self,
        query: &str,
        stmt: &crate::storage::query::ast::AlterUserStmt,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::privileges::UserAttributes;
        use crate::auth::UserId;
        use crate::storage::query::ast::AlterUserAttribute;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        let (_gname, grole) = current_auth_identity().ok_or_else(|| {
            RedDBError::Query("ALTER USER requires an authenticated principal".to_string())
        })?;
        if grole != crate::auth::Role::Admin {
            return Err(RedDBError::Query(
                "ALTER USER requires Admin role".to_string(),
            ));
        }

        let target = UserId::from_parts(stmt.tenant.as_deref(), &stmt.username);

        // Apply attributes incrementally — each one reads the current
        // record, mutates the relevant field, writes back.
        let mut attrs = auth_store.user_attributes(&target);
        let mut enable_change: Option<bool> = None;

        for a in &stmt.attributes {
            match a {
                AlterUserAttribute::ValidUntil(ts) => {
                    // Parse ISO-ish timestamp → ms since epoch. Fall
                    // back to integer-ms parsing for callers that pass
                    // `'1234567890123'`.
                    let ms = parse_timestamp_to_ms(ts).ok_or_else(|| {
                        RedDBError::Query(format!("invalid VALID UNTIL timestamp `{ts}`"))
                    })?;
                    attrs.valid_until = Some(ms);
                }
                AlterUserAttribute::ConnectionLimit(n) => {
                    if *n < 0 {
                        return Err(RedDBError::Query(
                            "CONNECTION LIMIT must be non-negative".to_string(),
                        ));
                    }
                    attrs.connection_limit = Some(*n as u32);
                }
                AlterUserAttribute::SetSearchPath(p) => {
                    attrs.search_path = Some(p.clone());
                }
                AlterUserAttribute::AddGroup(g) => {
                    if !attrs.groups.iter().any(|existing| existing == g) {
                        attrs.groups.push(g.clone());
                        attrs.groups.sort();
                    }
                }
                AlterUserAttribute::DropGroup(g) => {
                    attrs.groups.retain(|existing| existing != g);
                }
                AlterUserAttribute::Enable => enable_change = Some(true),
                AlterUserAttribute::Disable => enable_change = Some(false),
                AlterUserAttribute::Password(_) => {
                    // Out of scope — accept the AST but no-op so the
                    // parser stays compatible with future password
                    // rotation work.
                }
            }
        }

        auth_store
            .set_user_attributes(&target, attrs)
            .map_err(|e| RedDBError::Query(e.to_string()))?;
        if let Some(en) = enable_change {
            auth_store
                .set_user_enabled(&target, en)
                .map_err(|e| RedDBError::Query(e.to_string()))?;
        }
        self.invalidate_result_cache();
        tracing::info!(
            target: "audit",
            principal = %target,
            action = "alter_user",
            "ALTER USER applied"
        );

        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("ALTER USER {} applied", target),
            "alter_user",
        ))
    }

    // -----------------------------------------------------------------
    // IAM policy executors
    // -----------------------------------------------------------------

    fn execute_create_iam_policy(
        &self,
        query: &str,
        id: &str,
        json: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::policies::Policy;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        // Parse + validate. The kernel rejects oversize / bad shape /
        // bad action keywords. If the supplied id differs from the JSON
        // id, override it with the SQL-provided id (the JSON id is
        // optional context — the SQL DDL form is authoritative).
        let mut policy = Policy::from_json_str(json)
            .map_err(|e| RedDBError::Query(format!("policy parse: {e}")))?;
        if policy.id != id {
            policy.id = id.to_string();
        }
        let pid = policy.id.clone();
        let tenant = current_tenant();
        let (actor_name, actor_role) = current_auth_identity()
            .unwrap_or_else(|| ("anonymous".to_string(), crate::auth::Role::Read));
        let actor = crate::auth::UserId::from_parts(tenant.as_deref(), &actor_name);
        let eval_ctx = runtime_iam_context(actor_role, tenant.as_deref());
        let event_ctx = self.policy_mutation_control_ctx(&actor, tenant.as_deref());
        let ledger = self.inner.control_event_ledger.read();
        let control = crate::auth::store::PolicyMutationControl {
            ctx: &event_ctx,
            ledger: ledger.as_ref(),
            config: self.inner.control_event_config,
            registry: Some(self.inner.config_registry.as_ref()),
            actor: &actor,
            eval_ctx: &eval_ctx,
        };
        auth_store
            .put_policy_with_control_events(policy, &control)
            .map_err(|e| RedDBError::Query(e.to_string()))?;

        let principal = actor_name;
        tracing::info!(
            target: "audit",
            principal = %principal,
            action = "iam:policy.put",
            matched_policy_id = %pid,
            "CREATE POLICY applied"
        );
        self.inner.audit_log.record(
            "iam/policy.put",
            &principal,
            &pid,
            "ok",
            crate::json::Value::Null,
        );

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("policy `{pid}` stored"),
            "create_iam_policy",
        ))
    }

    fn execute_drop_iam_policy(&self, query: &str, id: &str) -> RedDBResult<RuntimeQueryResult> {
        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;
        let tenant = current_tenant();
        let (actor_name, actor_role) = current_auth_identity()
            .unwrap_or_else(|| ("anonymous".to_string(), crate::auth::Role::Read));
        let actor = crate::auth::UserId::from_parts(tenant.as_deref(), &actor_name);
        let eval_ctx = runtime_iam_context(actor_role, tenant.as_deref());
        let event_ctx = self.policy_mutation_control_ctx(&actor, tenant.as_deref());
        let ledger = self.inner.control_event_ledger.read();
        let control = crate::auth::store::PolicyMutationControl {
            ctx: &event_ctx,
            ledger: ledger.as_ref(),
            config: self.inner.control_event_config,
            registry: Some(self.inner.config_registry.as_ref()),
            actor: &actor,
            eval_ctx: &eval_ctx,
        };
        auth_store
            .delete_policy_with_control_events(id, &control)
            .map_err(|e| RedDBError::Query(e.to_string()))?;

        let principal = actor_name;
        tracing::info!(
            target: "audit",
            principal = %principal,
            action = "iam:policy.drop",
            matched_policy_id = %id,
            "DROP POLICY applied"
        );
        self.inner.audit_log.record(
            "iam/policy.drop",
            &principal,
            id,
            "ok",
            crate::json::Value::Null,
        );

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("policy `{id}` dropped"),
            "drop_iam_policy",
        ))
    }

    fn execute_attach_policy(
        &self,
        query: &str,
        policy_id: &str,
        principal: &crate::storage::query::ast::PolicyPrincipalRef,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::store::PrincipalRef;
        use crate::auth::UserId;
        use crate::storage::query::ast::PolicyPrincipalRef;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;
        let p = match principal {
            PolicyPrincipalRef::User(u) => {
                PrincipalRef::User(UserId::from_parts(u.tenant.as_deref(), &u.username))
            }
            PolicyPrincipalRef::Group(g) => PrincipalRef::Group(g.clone()),
        };
        let pretty_target = principal_label(principal);
        let tenant = current_tenant();
        let (actor_name, actor_role) = current_auth_identity()
            .unwrap_or_else(|| ("anonymous".to_string(), crate::auth::Role::Read));
        let actor = crate::auth::UserId::from_parts(tenant.as_deref(), &actor_name);
        let eval_ctx = runtime_iam_context(actor_role, tenant.as_deref());
        let event_ctx = self.policy_mutation_control_ctx(&actor, tenant.as_deref());
        let ledger = self.inner.control_event_ledger.read();
        let control = crate::auth::store::PolicyMutationControl {
            ctx: &event_ctx,
            ledger: ledger.as_ref(),
            config: self.inner.control_event_config,
            registry: Some(self.inner.config_registry.as_ref()),
            actor: &actor,
            eval_ctx: &eval_ctx,
        };
        auth_store
            .attach_policy_with_control_events(p, policy_id, &control)
            .map_err(|e| RedDBError::Query(e.to_string()))?;

        let principal_str = actor_name;
        tracing::info!(
            target: "audit",
            principal = %principal_str,
            action = "iam:policy.attach",
            matched_policy_id = %policy_id,
            target = %pretty_target,
            "ATTACH POLICY applied"
        );
        self.inner.audit_log.record(
            "iam/policy.attach",
            &principal_str,
            &pretty_target,
            "ok",
            crate::json::Value::Null,
        );

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("policy `{policy_id}` attached to {pretty_target}"),
            "attach_policy",
        ))
    }

    fn execute_detach_policy(
        &self,
        query: &str,
        policy_id: &str,
        principal: &crate::storage::query::ast::PolicyPrincipalRef,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::store::PrincipalRef;
        use crate::auth::UserId;
        use crate::storage::query::ast::PolicyPrincipalRef;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;
        let p = match principal {
            PolicyPrincipalRef::User(u) => {
                PrincipalRef::User(UserId::from_parts(u.tenant.as_deref(), &u.username))
            }
            PolicyPrincipalRef::Group(g) => PrincipalRef::Group(g.clone()),
        };
        let pretty_target = principal_label(principal);
        let tenant = current_tenant();
        let (actor_name, actor_role) = current_auth_identity()
            .unwrap_or_else(|| ("anonymous".to_string(), crate::auth::Role::Read));
        let actor = crate::auth::UserId::from_parts(tenant.as_deref(), &actor_name);
        let eval_ctx = runtime_iam_context(actor_role, tenant.as_deref());
        let event_ctx = self.policy_mutation_control_ctx(&actor, tenant.as_deref());
        let ledger = self.inner.control_event_ledger.read();
        let control = crate::auth::store::PolicyMutationControl {
            ctx: &event_ctx,
            ledger: ledger.as_ref(),
            config: self.inner.control_event_config,
            registry: Some(self.inner.config_registry.as_ref()),
            actor: &actor,
            eval_ctx: &eval_ctx,
        };
        auth_store
            .detach_policy_with_control_events(p, policy_id, &control)
            .map_err(|e| RedDBError::Query(e.to_string()))?;

        let principal_str = actor_name;
        tracing::info!(
            target: "audit",
            principal = %principal_str,
            action = "iam:policy.detach",
            matched_policy_id = %policy_id,
            target = %pretty_target,
            "DETACH POLICY applied"
        );
        self.inner.audit_log.record(
            "iam/policy.detach",
            &principal_str,
            &pretty_target,
            "ok",
            crate::json::Value::Null,
        );

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("policy `{policy_id}` detached from {pretty_target}"),
            "detach_policy",
        ))
    }

    fn execute_show_policies(
        &self,
        query: &str,
        filter: Option<&crate::storage::query::ast::PolicyPrincipalRef>,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::UserId;
        use crate::storage::query::ast::PolicyPrincipalRef;
        use crate::storage::query::unified::UnifiedRecord;
        use crate::storage::schema::Value as SchemaValue;
        use std::sync::Arc;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        let pols = match filter {
            None => auth_store.list_policies(),
            Some(PolicyPrincipalRef::User(u)) => {
                let id = UserId::from_parts(u.tenant.as_deref(), &u.username);
                auth_store.effective_policies(&id)
            }
            Some(PolicyPrincipalRef::Group(g)) => auth_store.group_policies(g),
        };

        let mut records = Vec::with_capacity(pols.len() + 1);

        // Header row (#712 / S5A): synthetic record at index 0 that
        // reports the active PolicyEnforcementMode and the hard-cutover
        // version, so an operator running SHOW POLICIES can see the
        // current posture without a separate command.
        let mode = auth_store.enforcement_mode();
        let mut header = UnifiedRecord::default();
        header.set_arc(
            Arc::from("id"),
            SchemaValue::text("<enforcement_mode>".to_string()),
        );
        header.set_arc(Arc::from("statements"), SchemaValue::Integer(0));
        header.set_arc(Arc::from("tenant"), SchemaValue::Null);
        let header_json = format!(
            r#"{{"enforcement_mode":"{}","policy_only_hard_version":"{}"}}"#,
            mode.as_str(),
            crate::auth::enforcement_mode::POLICY_ONLY_HARD_VERSION
        );
        header.set_arc(Arc::from("json"), SchemaValue::text(header_json));
        records.push(header);

        for p in pols.iter() {
            let mut rec = UnifiedRecord::default();
            rec.set_arc(Arc::from("id"), SchemaValue::text(p.id.clone()));
            rec.set_arc(
                Arc::from("statements"),
                SchemaValue::Integer(p.statements.len() as i64),
            );
            rec.set_arc(
                Arc::from("tenant"),
                p.tenant
                    .as_deref()
                    .map(|t| SchemaValue::text(t.to_string()))
                    .unwrap_or(SchemaValue::Null),
            );
            rec.set_arc(Arc::from("json"), SchemaValue::text(p.to_json_string()));
            records.push(rec);
        }
        let mut result = crate::storage::query::unified::UnifiedResult::empty();
        result.records = records;
        Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "show_policies",
            engine: "iam-policies",
            result,
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
        })
    }

    fn execute_show_effective_permissions(
        &self,
        query: &str,
        user: &crate::storage::query::ast::PolicyUserRef,
        resource: Option<&crate::storage::query::ast::PolicyResourceRef>,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::UserId;
        use crate::storage::query::unified::UnifiedRecord;
        use crate::storage::schema::Value as SchemaValue;
        use std::sync::Arc;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;
        let id = UserId::from_parts(user.tenant.as_deref(), &user.username);
        let pols = auth_store.effective_policies(&id);

        // Show one row per (policy, statement) tuple, plus any
        // resource-level filter passed by the caller.
        let mut records = Vec::new();
        for p in pols.iter() {
            for (idx, st) in p.statements.iter().enumerate() {
                if let Some(_r) = resource {
                    // Naive filter: render statement targets to strings
                    // and skip if no match. Conservative default = include
                    // (the simulator handles fine-grained matching).
                }
                let mut rec = UnifiedRecord::default();
                rec.set_arc(Arc::from("policy_id"), SchemaValue::text(p.id.clone()));
                rec.set_arc(
                    Arc::from("statement_index"),
                    SchemaValue::Integer(idx as i64),
                );
                rec.set_arc(
                    Arc::from("sid"),
                    st.sid
                        .as_deref()
                        .map(|s| SchemaValue::text(s.to_string()))
                        .unwrap_or(SchemaValue::Null),
                );
                rec.set_arc(
                    Arc::from("effect"),
                    SchemaValue::text(match st.effect {
                        crate::auth::policies::Effect::Allow => "allow",
                        crate::auth::policies::Effect::Deny => "deny",
                    }),
                );
                rec.set_arc(
                    Arc::from("actions"),
                    SchemaValue::Integer(st.actions.len() as i64),
                );
                rec.set_arc(
                    Arc::from("resources"),
                    SchemaValue::Integer(st.resources.len() as i64),
                );
                records.push(rec);
            }
        }
        let mut result = crate::storage::query::unified::UnifiedResult::empty();
        result.records = records;
        Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "show_effective_permissions",
            engine: "iam-policies",
            result,
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
        })
    }

    fn execute_lint_policy(
        &self,
        query: &str,
        source: &crate::storage::query::ast::LintPolicySource,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::policy_linter::lint;
        use crate::storage::query::ast::LintPolicySource;
        use crate::storage::query::unified::UnifiedRecord;
        use crate::storage::schema::Value as SchemaValue;
        use std::sync::Arc;

        // Resolve the policy text. `JSON` source lints the literal
        // verbatim; `Id` source fetches the stored document so
        // operators can lint a policy by name without rebuilding the
        // JSON from `SHOW POLICY`.
        let policy_text = match source {
            LintPolicySource::Json(text) => text.clone(),
            LintPolicySource::Id(id) => {
                let auth_store =
                    self.inner.auth_store.read().clone().ok_or_else(|| {
                        RedDBError::Query("auth store not configured".to_string())
                    })?;
                let policy = auth_store
                    .get_policy(id)
                    .ok_or_else(|| RedDBError::Query(format!("policy `{id}` not found")))?;
                policy.to_json_string()
            }
        };
        let diagnostics = lint(&policy_text);

        let principal_str = current_auth_identity()
            .map(|(u, _)| u)
            .unwrap_or_else(|| "anonymous".into());
        tracing::info!(
            target: "audit",
            principal = %principal_str,
            action = "iam:policy.lint",
            diagnostic_count = diagnostics.len(),
            "LINT POLICY issued"
        );
        self.inner.audit_log.record(
            "iam/policy.lint",
            &principal_str,
            match source {
                LintPolicySource::Id(id) => id.as_str(),
                LintPolicySource::Json(_) => "<json>",
            },
            "ok",
            crate::json::Value::Null,
        );

        // One row per diagnostic. Column order matches the HTTP
        // surface's JSON keys so the two contracts line up.
        const COLUMNS: [&str; 5] = ["severity", "code", "message", "suggested_fix", "location"];
        let schema = Arc::new(
            COLUMNS
                .iter()
                .map(|name| Arc::<str>::from(*name))
                .collect::<Vec<_>>(),
        );
        let records: Vec<UnifiedRecord> = diagnostics
            .iter()
            .map(|d| {
                UnifiedRecord::with_schema(
                    Arc::clone(&schema),
                    vec![
                        SchemaValue::text(d.severity.as_str()),
                        SchemaValue::text(d.code.as_str()),
                        SchemaValue::text(d.message.clone()),
                        d.suggested_fix
                            .as_deref()
                            .map(SchemaValue::text)
                            .unwrap_or(SchemaValue::Null),
                        d.location
                            .as_deref()
                            .map(SchemaValue::text)
                            .unwrap_or(SchemaValue::Null),
                    ],
                )
            })
            .collect();
        let mut result = crate::storage::query::unified::UnifiedResult::with_columns(
            COLUMNS.iter().map(|c| c.to_string()).collect(),
        );
        result.records = records;
        Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "lint_policy",
            engine: "iam-policies",
            result,
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
        })
    }

    /// `MIGRATE POLICY MODE TO '<target>' [DRY RUN]` — flip the install
    /// from `legacy_rbac` to `policy_only` after the pre-flight delta
    /// simulator confirms no non-admin principal would lose access.
    /// Issue #714.
    fn execute_migrate_policy_mode(
        &self,
        query: &str,
        target: &str,
        dry_run: bool,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::enforcement_mode::PolicyEnforcementMode;
        use crate::auth::migrate_policy_mode::{
            principal_label, simulate_migration_delta, MigratePolicyDelta,
        };
        use crate::auth::policies::ResourceRef;
        use crate::storage::query::unified::UnifiedRecord;
        use crate::storage::schema::Value as SchemaValue;
        use std::sync::Arc;

        // Only `policy_only` is a meaningful destination for this
        // command — flipping back to `legacy_rbac` is supported via
        // direct config writes (it doesn't need a pre-flight). We
        // reject everything else with the same allowlist `parse` uses.
        let parsed = PolicyEnforcementMode::parse(target).ok_or_else(|| {
            RedDBError::Query(format!(
                "MIGRATE POLICY MODE: invalid target `{target}` (expected `policy_only`)"
            ))
        })?;
        if parsed != PolicyEnforcementMode::PolicyOnly {
            return Err(RedDBError::Query(format!(
                "MIGRATE POLICY MODE: target `{target}` is not supported — only `policy_only` may be migrated to via this command"
            )));
        }

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        // Resource enumeration: every existing collection probed as
        // `table:<name>`. This is the realistic resource surface for
        // the legacy_rbac fallback (the role floors gate per-table
        // actions). Wildcard / column-scoped resources are still
        // covered by the policy evaluator because evaluate() resolves
        // resource patterns relative to the concrete resources we
        // probe here.
        let snapshot = self.inner.db.catalog_model_snapshot();
        let resources: Vec<ResourceRef> = snapshot
            .collections
            .iter()
            .map(|c| ResourceRef::new("table", c.name.clone()))
            .collect();

        let now_ms = crate::utils::now_unix_millis() as u128;
        let deltas: Vec<MigratePolicyDelta> =
            simulate_migration_delta(auth_store.as_ref(), &resources, now_ms);

        let principal_str = current_auth_identity()
            .map(|(u, _)| u)
            .unwrap_or_else(|| "anonymous".into());

        // Audit every issuance. The outcome line differentiates
        // dry-run, refused, and applied — operators can grep for these
        // strings in the audit log.
        let outcome_str = if dry_run {
            "dry_run"
        } else if deltas.is_empty() {
            "applied"
        } else {
            "refused"
        };
        tracing::info!(
            target: "audit",
            principal = %principal_str,
            action = "iam:policy.migrate_mode",
            target = %target,
            dry_run,
            delta_count = deltas.len(),
            outcome = outcome_str,
            "MIGRATE POLICY MODE issued"
        );
        self.inner.audit_log.record(
            "iam/policy.migrate_mode",
            &principal_str,
            target,
            outcome_str,
            crate::json::Value::Null,
        );

        // Refuse the non-dry-run path when any principal would lose
        // access. The error string carries a compact summary plus the
        // delta count so operators can re-run with DRY RUN to inspect.
        if !dry_run && !deltas.is_empty() {
            let summary = deltas
                .iter()
                .take(5)
                .map(|d| {
                    format!(
                        "{}:{}/{}:{}",
                        principal_label(&d.principal),
                        d.action,
                        d.resource_kind,
                        d.resource_name
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let more = if deltas.len() > 5 {
                format!(" (and {} more)", deltas.len() - 5)
            } else {
                String::new()
            };
            return Err(RedDBError::Query(format!(
                "MIGRATE POLICY MODE refused: {n} principal/action/resource pair(s) would lose access under `policy_only`. Run `MIGRATE POLICY MODE TO '{target}' DRY RUN` to inspect. Sample: {summary}{more}",
                n = deltas.len(),
            )));
        }

        // Mutate the live enforcement mode only on the non-dry-run
        // path with an empty delta. `set_enforcement_mode` also
        // persists to vault_kv so the new mode survives restart.
        if !dry_run {
            auth_store.set_enforcement_mode(parsed);
        }

        const COLUMNS: [&str; 5] = [
            "principal",
            "role",
            "action",
            "resource_kind",
            "resource_name",
        ];
        let schema = Arc::new(
            COLUMNS
                .iter()
                .map(|name| Arc::<str>::from(*name))
                .collect::<Vec<_>>(),
        );
        let records: Vec<UnifiedRecord> = deltas
            .iter()
            .map(|d| {
                UnifiedRecord::with_schema(
                    Arc::clone(&schema),
                    vec![
                        SchemaValue::text(principal_label(&d.principal)),
                        SchemaValue::text(d.role.as_str()),
                        SchemaValue::text(d.action.clone()),
                        SchemaValue::text(d.resource_kind.clone()),
                        SchemaValue::text(d.resource_name.clone()),
                    ],
                )
            })
            .collect();
        let mut result = crate::storage::query::unified::UnifiedResult::with_columns(
            COLUMNS.iter().map(|c| c.to_string()).collect(),
        );
        result.records = records;
        Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "migrate_policy_mode",
            engine: "iam-policies",
            result,
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
        })
    }

    fn execute_simulate_policy(
        &self,
        query: &str,
        user: &crate::storage::query::ast::PolicyUserRef,
        action: &str,
        resource: &crate::storage::query::ast::PolicyResourceRef,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::policies::ResourceRef;
        use crate::auth::store::SimCtx;
        use crate::auth::UserId;
        use crate::storage::query::unified::UnifiedRecord;
        use crate::storage::schema::Value as SchemaValue;
        use std::sync::Arc;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;
        let id = UserId::from_parts(user.tenant.as_deref(), &user.username);
        let r = ResourceRef::new(resource.kind.clone(), resource.name.clone());
        let outcome = auth_store.simulate(&id, action, &r, SimCtx::default());

        let principal_str = current_auth_identity()
            .map(|(u, _)| u)
            .unwrap_or_else(|| "anonymous".into());
        let (decision_str, matched_pid, matched_sid) = decision_to_strings(&outcome.decision);
        tracing::info!(
            target: "audit",
            principal = %principal_str,
            action = "iam:policy.simulate",
            decision = %decision_str,
            matched_policy_id = ?matched_pid,
            matched_sid = ?matched_sid,
            "SIMULATE issued"
        );
        self.inner.audit_log.record(
            "iam/policy.simulate",
            &principal_str,
            &id.to_string(),
            "ok",
            crate::json::Value::Null,
        );

        let mut rec = UnifiedRecord::default();
        rec.set_arc(Arc::from("decision"), SchemaValue::text(decision_str));
        rec.set_arc(
            Arc::from("matched_policy_id"),
            matched_pid
                .map(SchemaValue::text)
                .unwrap_or(SchemaValue::Null),
        );
        rec.set_arc(
            Arc::from("matched_sid"),
            matched_sid
                .map(SchemaValue::text)
                .unwrap_or(SchemaValue::Null),
        );
        rec.set_arc(Arc::from("reason"), SchemaValue::text(outcome.reason));
        rec.set_arc(
            Arc::from("trail_len"),
            SchemaValue::Integer(outcome.trail.len() as i64),
        );
        let mut result = crate::storage::query::unified::UnifiedResult::empty();
        result.records = vec![rec];
        Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "simulate_policy",
            engine: "iam-policies",
            result,
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
        })
    }
}

/// Translate a parsed GRANT into a synthetic IAM policy whose id
/// starts with `_grant_<unique>`. PUBLIC is represented as an
/// implicit IAM group; legacy GROUP grants are still rejected by the
/// grant store and are not translated here.
fn grant_to_iam_policy(
    principal: &crate::auth::privileges::GrantPrincipal,
    resource: &crate::auth::privileges::Resource,
    actions: &[crate::auth::privileges::Action],
    tenant: Option<&str>,
) -> Option<crate::auth::policies::Policy> {
    use crate::auth::policies::{
        compile_action, ActionPattern, Effect, Policy, ResourcePattern, Statement,
    };
    use crate::auth::privileges::{Action, GrantPrincipal, Resource};

    if matches!(principal, GrantPrincipal::Group(_)) {
        return None;
    }

    let now = crate::auth::now_ms();
    let id = format!("_grant_{:x}_{:x}", now, std::process::id());

    let resource_str = match resource {
        Resource::Database => "table:*".to_string(),
        Resource::Schema(s) => format!("table:{s}.*"),
        Resource::Table { schema, table } => match schema {
            Some(s) => format!("table:{s}.{table}"),
            None => format!("table:{table}"),
        },
        Resource::Function { schema, name } => match schema {
            Some(s) => format!("function:{s}.{name}"),
            None => format!("function:{name}"),
        },
    };

    // Compile actions — fall back to `*` only when the grant included
    // `Action::All`. Map every other action keyword to its lowercase
    // form so it lines up with the kernel's allowlist.
    let action_patterns: Vec<ActionPattern> = if actions.contains(&Action::All) {
        vec![ActionPattern::Wildcard]
    } else {
        actions
            .iter()
            .map(|a| compile_action(&a.as_str().to_ascii_lowercase()))
            .collect()
    };
    if action_patterns.is_empty() {
        return None;
    }

    // Inline resource compilation matching the kernel's `compile_resource`:
    //   * `*` → wildcard
    //   * contains `*` → glob
    //   * `kind:name` → exact
    let resource_patterns = if resource_str == "*" {
        vec![ResourcePattern::Wildcard]
    } else if resource_str.contains('*') {
        vec![ResourcePattern::Glob(resource_str.clone())]
    } else if let Some((kind, name)) = resource_str.split_once(':') {
        vec![ResourcePattern::Exact {
            kind: kind.to_string(),
            name: name.to_string(),
        }]
    } else {
        vec![ResourcePattern::Wildcard]
    };

    let policy = Policy {
        id,
        version: 1,
        tenant: tenant.map(|t| t.to_string()),
        created_at: now,
        updated_at: now,
        statements: vec![Statement {
            sid: None,
            effect: Effect::Allow,
            actions: action_patterns,
            resources: resource_patterns,
            condition: None,
        }],
    };
    if policy.validate().is_err() {
        return None;
    }
    Some(policy)
}

/// Coerce a `key => <number>` table-function named argument into a positive
/// iteration count for the centrality TVFs (issue #797). The parser lexes all
/// named values as `f64`, so an integral, finite, strictly-positive value is
/// required here; anything else (fractional, zero, negative, NaN/inf) is a
/// clear query error. `func` names the function for the message.
fn parse_positive_iterations(func: &str, value: &f64) -> RedDBResult<usize> {
    if !value.is_finite() || *value < 1.0 || value.fract() != 0.0 {
        return Err(RedDBError::Query(format!(
            "table function '{func}' max_iterations must be a positive integer, got {value}"
        )));
    }
    Ok(*value as usize)
}

fn legacy_action_to_iam(action: crate::auth::privileges::Action) -> &'static str {
    use crate::auth::privileges::Action;
    match action {
        Action::Select => "select",
        Action::Insert => "insert",
        Action::Update => "update",
        Action::Delete => "delete",
        Action::Truncate => "truncate",
        Action::References => "references",
        Action::Execute => "execute",
        Action::Usage => "usage",
        Action::All => "*",
    }
}

fn update_set_target_columns(query: &crate::storage::query::ast::UpdateQuery) -> Vec<String> {
    let mut columns = Vec::new();
    for (column, _) in &query.assignment_exprs {
        if !columns.iter().any(|seen| seen == column) {
            columns.push(column.clone());
        }
    }
    columns
}

fn column_access_request_for_table_update(
    table_name: &str,
    columns: Vec<String>,
) -> crate::auth::ColumnAccessRequest {
    match table_name.split_once('.') {
        Some((schema, table)) => {
            crate::auth::ColumnAccessRequest::update(table.to_string(), columns)
                .with_schema(schema.to_string())
        }
        None => crate::auth::ColumnAccessRequest::update(table_name.to_string(), columns),
    }
}

fn column_access_request_for_table_select(
    table_name: &str,
    columns: Vec<String>,
) -> crate::auth::ColumnAccessRequest {
    match table_name.split_once('.') {
        Some((schema, table)) => {
            crate::auth::ColumnAccessRequest::select(table.to_string(), columns)
                .with_schema(schema.to_string())
        }
        None => crate::auth::ColumnAccessRequest::select(table_name.to_string(), columns),
    }
}

fn update_returning_columns_for_policy(
    runtime: &RedDBRuntime,
    query: &crate::storage::query::ast::UpdateQuery,
) -> Option<Vec<String>> {
    let items = query.returning.as_ref()?;
    let mut columns = Vec::new();
    let project_all = items
        .iter()
        .any(|item| matches!(item, crate::storage::query::ast::ReturningItem::All));
    if project_all {
        collect_returning_star_columns(runtime, query, &mut columns);
    } else {
        for item in items {
            let crate::storage::query::ast::ReturningItem::Column(column) = item else {
                continue;
            };
            push_returning_policy_column(&mut columns, column);
        }
    }
    (!columns.is_empty()).then_some(columns)
}

fn collect_returning_star_columns(
    runtime: &RedDBRuntime,
    query: &crate::storage::query::ast::UpdateQuery,
    columns: &mut Vec<String>,
) {
    let store = runtime.db().store();
    let Some(manager) = store.get_collection(&query.table) else {
        return;
    };
    if let Some(schema) = manager.column_schema() {
        for column in schema.iter() {
            push_returning_policy_column(columns, column);
        }
    }
    for entity in manager.query_all(|_| true) {
        if !returning_entity_matches_update_target(&entity, query.target) {
            continue;
        }
        match &entity.data {
            crate::storage::EntityData::Row(row) => {
                for (column, _) in row.iter_fields() {
                    push_returning_policy_column(columns, column);
                }
            }
            crate::storage::EntityData::Node(node) => {
                push_returning_policy_column(columns, "label");
                push_returning_policy_column(columns, "node_type");
                for column in node.properties.keys() {
                    push_returning_policy_column(columns, column);
                }
            }
            crate::storage::EntityData::Edge(edge) => {
                push_returning_policy_column(columns, "label");
                push_returning_policy_column(columns, "from_rid");
                push_returning_policy_column(columns, "to_rid");
                push_returning_policy_column(columns, "weight");
                for column in edge.properties.keys() {
                    push_returning_policy_column(columns, column);
                }
            }
            _ => {}
        }
    }
}

fn push_returning_policy_column(columns: &mut Vec<String>, column: &str) {
    if returning_public_envelope_column(column) {
        return;
    }
    if !columns.iter().any(|seen| seen == column) {
        columns.push(column.to_string());
    }
}

fn returning_public_envelope_column(column: &str) -> bool {
    matches!(
        column.to_ascii_lowercase().as_str(),
        "rid" | "collection" | "kind" | "tenant" | "created_at" | "updated_at"
    )
}

fn returning_entity_matches_update_target(
    entity: &crate::storage::UnifiedEntity,
    target: crate::storage::query::ast::UpdateTarget,
) -> bool {
    use crate::storage::query::ast::UpdateTarget;
    match target {
        UpdateTarget::Rows => {
            matches!(returning_row_item_kind(entity), Some(ReturningRowKind::Row))
        }
        UpdateTarget::Documents => {
            matches!(
                returning_row_item_kind(entity),
                Some(ReturningRowKind::Document)
            )
        }
        UpdateTarget::Kv => matches!(returning_row_item_kind(entity), Some(ReturningRowKind::Kv)),
        UpdateTarget::Nodes => matches!(
            (&entity.kind, &entity.data),
            (
                crate::storage::EntityKind::GraphNode(_),
                crate::storage::EntityData::Node(_)
            )
        ),
        UpdateTarget::Edges => matches!(
            (&entity.kind, &entity.data),
            (
                crate::storage::EntityKind::GraphEdge(_),
                crate::storage::EntityData::Edge(_)
            )
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReturningRowKind {
    Row,
    Document,
    Kv,
}

fn returning_row_item_kind(entity: &crate::storage::UnifiedEntity) -> Option<ReturningRowKind> {
    let row = entity.data.as_row()?;
    let is_kv = row.iter_fields().all(|(column, _)| {
        column.eq_ignore_ascii_case("key") || column.eq_ignore_ascii_case("value")
    });
    if is_kv {
        return Some(ReturningRowKind::Kv);
    }
    let is_document = row
        .iter_fields()
        .any(|(_, value)| matches!(value, crate::storage::schema::Value::Json(_)));
    if is_document {
        Some(ReturningRowKind::Document)
    } else {
        Some(ReturningRowKind::Row)
    }
}

fn requested_table_columns_for_policy(
    table: &crate::storage::query::ast::TableQuery,
) -> Vec<String> {
    use crate::storage::query::sql_lowering::{
        effective_table_filter, effective_table_group_by_exprs, effective_table_having_filter,
        effective_table_projections,
    };

    let table_name = table.table.as_str();
    let table_alias = table.alias.as_deref();
    let mut columns = std::collections::BTreeSet::new();

    for projection in effective_table_projections(table) {
        collect_projection_columns(&projection, table_name, table_alias, &mut columns);
    }
    if let Some(filter) = effective_table_filter(table) {
        collect_filter_columns(&filter, table_name, table_alias, &mut columns);
    }
    for expr in effective_table_group_by_exprs(table) {
        collect_expr_columns(&expr, table_name, table_alias, &mut columns);
    }
    if let Some(filter) = effective_table_having_filter(table) {
        collect_filter_columns(&filter, table_name, table_alias, &mut columns);
    }
    for order in &table.order_by {
        if let Some(expr) = order.expr.as_ref() {
            collect_expr_columns(expr, table_name, table_alias, &mut columns);
        } else {
            collect_field_ref_column(&order.field, table_name, table_alias, &mut columns);
        }
    }

    columns.into_iter().collect()
}

fn collect_projection_columns(
    projection: &crate::storage::query::ast::Projection,
    table_name: &str,
    table_alias: Option<&str>,
    columns: &mut std::collections::BTreeSet<String>,
) {
    use crate::storage::query::ast::Projection;
    match projection {
        Projection::All => {
            columns.insert("*".to_string());
        }
        Projection::Column(column) | Projection::Alias(column, _) => {
            if column != "*" {
                columns.insert(column.clone());
            }
        }
        Projection::Function(_, args) => {
            for arg in args {
                collect_projection_columns(arg, table_name, table_alias, columns);
            }
        }
        Projection::Expression(filter, _) => {
            collect_filter_columns(filter, table_name, table_alias, columns);
        }
        Projection::Field(field, _) => {
            collect_field_ref_column(field, table_name, table_alias, columns);
        }
        // Slice 7a (#589): no runtime support yet; recurse into args so
        // any column references are still tracked in case a future
        // executor needs the column set.
        Projection::Window { args, .. } => {
            for arg in args {
                collect_projection_columns(arg, table_name, table_alias, columns);
            }
        }
    }
}

fn collect_filter_columns(
    filter: &crate::storage::query::ast::Filter,
    table_name: &str,
    table_alias: Option<&str>,
    columns: &mut std::collections::BTreeSet<String>,
) {
    use crate::storage::query::ast::Filter;
    match filter {
        Filter::Compare { field, .. }
        | Filter::IsNull(field)
        | Filter::IsNotNull(field)
        | Filter::In { field, .. }
        | Filter::Between { field, .. }
        | Filter::Like { field, .. }
        | Filter::StartsWith { field, .. }
        | Filter::EndsWith { field, .. }
        | Filter::Contains { field, .. } => {
            collect_field_ref_column(field, table_name, table_alias, columns);
        }
        Filter::CompareFields { left, right, .. } => {
            collect_field_ref_column(left, table_name, table_alias, columns);
            collect_field_ref_column(right, table_name, table_alias, columns);
        }
        Filter::CompareExpr { lhs, rhs, .. } => {
            collect_expr_columns(lhs, table_name, table_alias, columns);
            collect_expr_columns(rhs, table_name, table_alias, columns);
        }
        Filter::And(left, right) | Filter::Or(left, right) => {
            collect_filter_columns(left, table_name, table_alias, columns);
            collect_filter_columns(right, table_name, table_alias, columns);
        }
        Filter::Not(inner) => collect_filter_columns(inner, table_name, table_alias, columns),
    }
}

fn collect_expr_columns(
    expr: &crate::storage::query::ast::Expr,
    table_name: &str,
    table_alias: Option<&str>,
    columns: &mut std::collections::BTreeSet<String>,
) {
    use crate::storage::query::ast::Expr;
    match expr {
        Expr::Column { field, .. } => {
            collect_field_ref_column(field, table_name, table_alias, columns);
        }
        Expr::Literal { .. } | Expr::Parameter { .. } => {}
        Expr::UnaryOp { operand, .. } | Expr::Cast { inner: operand, .. } => {
            collect_expr_columns(operand, table_name, table_alias, columns);
        }
        Expr::BinaryOp { lhs, rhs, .. } => {
            collect_expr_columns(lhs, table_name, table_alias, columns);
            collect_expr_columns(rhs, table_name, table_alias, columns);
        }
        Expr::FunctionCall { args, .. } => {
            for arg in args {
                collect_expr_columns(arg, table_name, table_alias, columns);
            }
        }
        Expr::Case {
            branches, else_, ..
        } => {
            for (condition, value) in branches {
                collect_expr_columns(condition, table_name, table_alias, columns);
                collect_expr_columns(value, table_name, table_alias, columns);
            }
            if let Some(value) = else_ {
                collect_expr_columns(value, table_name, table_alias, columns);
            }
        }
        Expr::IsNull { operand, .. } => {
            collect_expr_columns(operand, table_name, table_alias, columns);
        }
        Expr::InList { target, values, .. } => {
            collect_expr_columns(target, table_name, table_alias, columns);
            for value in values {
                collect_expr_columns(value, table_name, table_alias, columns);
            }
        }
        Expr::Between {
            target, low, high, ..
        } => {
            collect_expr_columns(target, table_name, table_alias, columns);
            collect_expr_columns(low, table_name, table_alias, columns);
            collect_expr_columns(high, table_name, table_alias, columns);
        }
        Expr::Subquery { .. } => {}
        Expr::WindowFunctionCall { args, window, .. } => {
            for arg in args {
                collect_expr_columns(arg, table_name, table_alias, columns);
            }
            for e in &window.partition_by {
                collect_expr_columns(e, table_name, table_alias, columns);
            }
            for o in &window.order_by {
                collect_expr_columns(&o.expr, table_name, table_alias, columns);
            }
        }
    }
}

fn collect_field_ref_column(
    field: &crate::storage::query::ast::FieldRef,
    table_name: &str,
    table_alias: Option<&str>,
    columns: &mut std::collections::BTreeSet<String>,
) {
    if let Some(column) = policy_column_name_from_field_ref(field, table_name, table_alias) {
        if column != "*" {
            columns.insert(column);
        }
    }
}

fn policy_column_name_from_field_ref(
    field: &crate::storage::query::ast::FieldRef,
    table_name: &str,
    table_alias: Option<&str>,
) -> Option<String> {
    match field {
        crate::storage::query::ast::FieldRef::TableColumn { table, column } => {
            if column == "*" {
                return Some("*".to_string());
            }
            if table.is_empty() || table == table_name || Some(table.as_str()) == table_alias {
                Some(column.clone())
            } else {
                Some(format!("{table}.{column}"))
            }
        }
        _ => None,
    }
}

fn legacy_resource_to_iam(
    resource: &crate::auth::privileges::Resource,
    tenant: Option<&str>,
) -> crate::auth::policies::ResourceRef {
    use crate::auth::privileges::Resource;

    let (kind, name) = match resource {
        Resource::Database => ("database".to_string(), "*".to_string()),
        Resource::Schema(s) => ("schema".to_string(), format!("{s}.*")),
        Resource::Table { schema, table } => (
            "table".to_string(),
            match schema {
                Some(s) => format!("{s}.{table}"),
                None => table.clone(),
            },
        ),
        Resource::Function { schema, name } => (
            "function".to_string(),
            match schema {
                Some(s) => format!("{s}.{name}"),
                None => name.clone(),
            },
        ),
    };

    let mut out = crate::auth::policies::ResourceRef::new(kind, name);
    if let Some(t) = tenant {
        out = out.with_tenant(t.to_string());
    }
    out
}

#[derive(Debug)]
struct JoinTableSide {
    table: String,
    alias: String,
}

fn table_side_context(expr: &QueryExpr) -> Option<JoinTableSide> {
    match expr {
        QueryExpr::Table(table) => Some(JoinTableSide {
            table: table.table.clone(),
            alias: table.alias.clone().unwrap_or_else(|| table.table.clone()),
        }),
        _ => None,
    }
}

fn collect_projection_columns_for_table(
    projection: &Projection,
    table: &str,
    alias: Option<&str>,
    out: &mut BTreeSet<String>,
) {
    match projection {
        Projection::Column(column) | Projection::Alias(column, _) => {
            match split_qualified_column(column) {
                Some((qualifier, column))
                    if qualifier == table || alias.is_some_and(|alias| qualifier == alias) =>
                {
                    push_policy_column(column, out);
                }
                Some(_) => {}
                None => push_policy_column(column, out),
            }
        }
        Projection::Field(
            FieldRef::TableColumn {
                table: qualifier,
                column,
            },
            _,
        ) => {
            if qualifier.is_empty()
                || qualifier == table
                || alias.is_some_and(|alias| qualifier == alias)
            {
                push_policy_column(column, out);
            }
        }
        Projection::Field(
            FieldRef::NodeProperty {
                alias: qualifier,
                property,
            },
            _,
        )
        | Projection::Field(
            FieldRef::EdgeProperty {
                alias: qualifier,
                property,
            },
            _,
        ) => {
            if qualifier == table || alias.is_some_and(|alias| qualifier == alias) {
                push_policy_column(property, out);
            }
        }
        Projection::Function(_, args) => {
            for arg in args {
                collect_projection_columns_for_table(arg, table, alias, out);
            }
        }
        Projection::Expression(_, _) | Projection::All | Projection::Field(_, _) => {}
        Projection::Window { args, .. } => {
            for arg in args {
                collect_projection_columns_for_table(arg, table, alias, out);
            }
        }
    }
}

fn collect_projection_columns_for_join_side(
    projection: &Projection,
    left: Option<&JoinTableSide>,
    right: Option<&JoinTableSide>,
    out: &mut HashMap<String, BTreeSet<String>>,
) -> RedDBResult<()> {
    match projection {
        Projection::Column(column) | Projection::Alias(column, _) => {
            if let Some((qualifier, column)) = split_qualified_column(column) {
                push_qualified_join_column(qualifier, column, left, right, out);
            } else {
                push_unqualified_join_column(column, left, right, out);
            }
        }
        Projection::Field(FieldRef::TableColumn { table, column }, _) => {
            if table.is_empty() {
                push_unqualified_join_column(column, left, right, out);
            } else if let Some(side) = [left, right]
                .into_iter()
                .flatten()
                .find(|side| table == side.table.as_str() || table == side.alias.as_str())
            {
                push_join_column(&side.table, column, out);
            }
        }
        Projection::Field(FieldRef::NodeProperty { alias, property }, _)
        | Projection::Field(FieldRef::EdgeProperty { alias, property }, _) => {
            push_qualified_join_column(alias, property, left, right, out);
        }
        Projection::Function(_, args) => {
            for arg in args {
                collect_projection_columns_for_join_side(arg, left, right, out)?;
            }
        }
        Projection::Expression(_, _) | Projection::All | Projection::Field(_, _) => {}
        Projection::Window { args, .. } => {
            for arg in args {
                collect_projection_columns_for_join_side(arg, left, right, out)?;
            }
        }
    }
    Ok(())
}

fn split_qualified_column(column: &str) -> Option<(&str, &str)> {
    let (qualifier, column) = column.split_once('.')?;
    if qualifier.is_empty() || column.is_empty() || column.contains('.') {
        return None;
    }
    Some((qualifier, column))
}

fn push_qualified_join_column(
    qualifier: &str,
    column: &str,
    left: Option<&JoinTableSide>,
    right: Option<&JoinTableSide>,
    out: &mut HashMap<String, BTreeSet<String>>,
) {
    if let Some(side) = [left, right]
        .into_iter()
        .flatten()
        .find(|side| qualifier == side.table.as_str() || qualifier == side.alias.as_str())
    {
        push_join_column(&side.table, column, out);
    }
}

fn push_unqualified_join_column(
    column: &str,
    left: Option<&JoinTableSide>,
    right: Option<&JoinTableSide>,
    out: &mut HashMap<String, BTreeSet<String>>,
) {
    for side in [left, right].into_iter().flatten() {
        push_join_column(&side.table, column, out);
    }
}

fn push_join_column(table: &str, column: &str, out: &mut HashMap<String, BTreeSet<String>>) {
    if is_policy_column_name(column) {
        out.entry(table.to_string())
            .or_default()
            .insert(column.to_string());
    }
}

fn push_policy_column(column: &str, out: &mut BTreeSet<String>) {
    if is_policy_column_name(column) {
        out.insert(column.to_string());
    }
}

fn is_policy_column_name(column: &str) -> bool {
    !column.is_empty()
        && column != "*"
        && !column.starts_with("LIT:")
        && !column.starts_with("TYPE:")
}

fn runtime_iam_context(
    role: crate::auth::Role,
    tenant: Option<&str>,
) -> crate::auth::policies::EvalContext {
    crate::auth::policies::EvalContext {
        principal_tenant: tenant.map(|t| t.to_string()),
        current_tenant: tenant.map(|t| t.to_string()),
        peer_ip: None,
        mfa_present: false,
        now_ms: crate::auth::now_ms(),
        principal_is_admin_role: role == crate::auth::Role::Admin,
        principal_is_platform_scoped: tenant.is_none(),
    }
}

fn explicit_table_projection_columns(
    query: &crate::storage::query::ast::TableQuery,
) -> Vec<String> {
    use crate::storage::query::ast::{FieldRef, Projection};

    let mut columns = Vec::new();
    for projection in crate::storage::query::sql_lowering::effective_table_projections(query) {
        match projection {
            Projection::Column(column) | Projection::Alias(column, _) => {
                push_unique(&mut columns, column)
            }
            Projection::Field(FieldRef::TableColumn { column, .. }, _) => {
                push_unique(&mut columns, column)
            }
            // SELECT * and expression/function projections need the
            // executor-wide column-policy context mapped in
            // docs/security/select-relational-column-policy-audit-2026-05-08.md.
            _ => {}
        }
    }
    columns
}

fn explicit_graph_projection_properties(
    query: &crate::storage::query::ast::GraphQuery,
) -> Vec<String> {
    use crate::storage::query::ast::{FieldRef, Projection};

    let mut columns = Vec::new();
    for projection in &query.return_ {
        match projection {
            Projection::Field(FieldRef::NodeProperty { property, .. }, _)
            | Projection::Field(FieldRef::EdgeProperty { property, .. }, _) => {
                push_unique(&mut columns, property.clone())
            }
            _ => {}
        }
    }
    columns
}

fn push_unique(columns: &mut Vec<String>, column: String) {
    if !columns.iter().any(|existing| existing == &column) {
        columns.push(column);
    }
}

fn principal_label(p: &crate::storage::query::ast::PolicyPrincipalRef) -> String {
    use crate::storage::query::ast::PolicyPrincipalRef;
    match p {
        PolicyPrincipalRef::User(u) => match &u.tenant {
            Some(t) => format!("user:{t}/{}", u.username),
            None => format!("user:{}", u.username),
        },
        PolicyPrincipalRef::Group(g) => format!("group:{g}"),
    }
}

/// Render a `Decision` into the (decision, matched_policy_id, matched_sid)
/// shape used by every audit emit + the simulator response.
pub(crate) fn decision_to_strings(
    d: &crate::auth::policies::Decision,
) -> (String, Option<String>, Option<String>) {
    use crate::auth::policies::Decision;
    match d {
        Decision::Allow {
            matched_policy_id,
            matched_sid,
        } => (
            "allow".into(),
            Some(matched_policy_id.clone()),
            matched_sid.clone(),
        ),
        Decision::Deny {
            matched_policy_id,
            matched_sid,
        } => (
            "deny".into(),
            Some(matched_policy_id.clone()),
            matched_sid.clone(),
        ),
        Decision::DefaultDeny => ("default_deny".into(), None, None),
        Decision::AdminBypass => ("admin_bypass".into(), None, None),
    }
}

fn relation_scopes_for_query(query: &QueryExpr) -> Vec<String> {
    let mut scopes = Vec::new();
    collect_relation_scopes(query, &mut scopes);
    scopes.sort();
    scopes.dedup();
    scopes
}

fn collect_relation_scopes(query: &QueryExpr, scopes: &mut Vec<String>) {
    match query {
        QueryExpr::Table(table) => {
            if !table.table.is_empty() {
                scopes.push(table.table.clone());
            }
            if let Some(alias) = &table.alias {
                scopes.push(alias.clone());
            }
        }
        QueryExpr::Join(join) => {
            collect_relation_scopes(&join.left, scopes);
            collect_relation_scopes(&join.right, scopes);
        }
        _ => {}
    }
}

fn query_references_outer_scope(query: &QueryExpr, outer_scopes: &[String]) -> bool {
    let inner_scopes = relation_scopes_for_query(query);
    query_expr_references_outer_scope(query, outer_scopes, &inner_scopes)
}

fn query_expr_references_outer_scope(
    query: &QueryExpr,
    outer_scopes: &[String],
    inner_scopes: &[String],
) -> bool {
    match query {
        QueryExpr::Table(table) => {
            table.select_items.iter().any(|item| match item {
                crate::storage::query::ast::SelectItem::Wildcard => false,
                crate::storage::query::ast::SelectItem::Expr { expr, .. } => {
                    expr_references_outer_scope(expr, outer_scopes, inner_scopes)
                }
            }) || table
                .where_expr
                .as_ref()
                .is_some_and(|expr| expr_references_outer_scope(expr, outer_scopes, inner_scopes))
                || table.filter.as_ref().is_some_and(|filter| {
                    filter_references_outer_scope(filter, outer_scopes, inner_scopes)
                })
                || table.having_expr.as_ref().is_some_and(|expr| {
                    expr_references_outer_scope(expr, outer_scopes, inner_scopes)
                })
                || table.having.as_ref().is_some_and(|filter| {
                    filter_references_outer_scope(filter, outer_scopes, inner_scopes)
                })
                || table
                    .group_by_exprs
                    .iter()
                    .any(|expr| expr_references_outer_scope(expr, outer_scopes, inner_scopes))
                || table.order_by.iter().any(|clause| {
                    clause.expr.as_ref().is_some_and(|expr| {
                        expr_references_outer_scope(expr, outer_scopes, inner_scopes)
                    })
                })
        }
        QueryExpr::Join(join) => {
            query_expr_references_outer_scope(&join.left, outer_scopes, inner_scopes)
                || query_expr_references_outer_scope(&join.right, outer_scopes, inner_scopes)
                || join.filter.as_ref().is_some_and(|filter| {
                    filter_references_outer_scope(filter, outer_scopes, inner_scopes)
                })
                || join.return_items.iter().any(|item| match item {
                    crate::storage::query::ast::SelectItem::Wildcard => false,
                    crate::storage::query::ast::SelectItem::Expr { expr, .. } => {
                        expr_references_outer_scope(expr, outer_scopes, inner_scopes)
                    }
                })
        }
        _ => false,
    }
}

fn filter_references_outer_scope(
    filter: &crate::storage::query::ast::Filter,
    outer_scopes: &[String],
    inner_scopes: &[String],
) -> bool {
    use crate::storage::query::ast::Filter;
    match filter {
        Filter::Compare { field, .. }
        | Filter::IsNull(field)
        | Filter::IsNotNull(field)
        | Filter::In { field, .. }
        | Filter::Between { field, .. }
        | Filter::Like { field, .. }
        | Filter::StartsWith { field, .. }
        | Filter::EndsWith { field, .. }
        | Filter::Contains { field, .. } => {
            field_ref_references_outer_scope(field, outer_scopes, inner_scopes)
        }
        Filter::CompareFields { left, right, .. } => {
            field_ref_references_outer_scope(left, outer_scopes, inner_scopes)
                || field_ref_references_outer_scope(right, outer_scopes, inner_scopes)
        }
        Filter::CompareExpr { lhs, rhs, .. } => {
            expr_references_outer_scope(lhs, outer_scopes, inner_scopes)
                || expr_references_outer_scope(rhs, outer_scopes, inner_scopes)
        }
        Filter::And(left, right) | Filter::Or(left, right) => {
            filter_references_outer_scope(left, outer_scopes, inner_scopes)
                || filter_references_outer_scope(right, outer_scopes, inner_scopes)
        }
        Filter::Not(inner) => filter_references_outer_scope(inner, outer_scopes, inner_scopes),
    }
}

fn expr_references_outer_scope(
    expr: &crate::storage::query::ast::Expr,
    outer_scopes: &[String],
    inner_scopes: &[String],
) -> bool {
    use crate::storage::query::ast::Expr;
    match expr {
        Expr::Column { field, .. } => {
            field_ref_references_outer_scope(field, outer_scopes, inner_scopes)
        }
        Expr::BinaryOp { lhs, rhs, .. } => {
            expr_references_outer_scope(lhs, outer_scopes, inner_scopes)
                || expr_references_outer_scope(rhs, outer_scopes, inner_scopes)
        }
        Expr::UnaryOp { operand, .. }
        | Expr::Cast { inner: operand, .. }
        | Expr::IsNull { operand, .. } => {
            expr_references_outer_scope(operand, outer_scopes, inner_scopes)
        }
        Expr::FunctionCall { args, .. } => args
            .iter()
            .any(|arg| expr_references_outer_scope(arg, outer_scopes, inner_scopes)),
        Expr::Case {
            branches, else_, ..
        } => {
            branches.iter().any(|(cond, value)| {
                expr_references_outer_scope(cond, outer_scopes, inner_scopes)
                    || expr_references_outer_scope(value, outer_scopes, inner_scopes)
            }) || else_
                .as_ref()
                .is_some_and(|expr| expr_references_outer_scope(expr, outer_scopes, inner_scopes))
        }
        Expr::InList { target, values, .. } => {
            expr_references_outer_scope(target, outer_scopes, inner_scopes)
                || values
                    .iter()
                    .any(|value| expr_references_outer_scope(value, outer_scopes, inner_scopes))
        }
        Expr::Between {
            target, low, high, ..
        } => {
            expr_references_outer_scope(target, outer_scopes, inner_scopes)
                || expr_references_outer_scope(low, outer_scopes, inner_scopes)
                || expr_references_outer_scope(high, outer_scopes, inner_scopes)
        }
        Expr::Subquery { query, .. } => query_references_outer_scope(&query.query, inner_scopes),
        Expr::Literal { .. } | Expr::Parameter { .. } => false,
        Expr::WindowFunctionCall { args, window, .. } => {
            args.iter()
                .any(|arg| expr_references_outer_scope(arg, outer_scopes, inner_scopes))
                || window
                    .partition_by
                    .iter()
                    .any(|e| expr_references_outer_scope(e, outer_scopes, inner_scopes))
                || window
                    .order_by
                    .iter()
                    .any(|o| expr_references_outer_scope(&o.expr, outer_scopes, inner_scopes))
        }
    }
}

fn field_ref_references_outer_scope(
    field: &crate::storage::query::ast::FieldRef,
    outer_scopes: &[String],
    inner_scopes: &[String],
) -> bool {
    match field {
        crate::storage::query::ast::FieldRef::TableColumn { table, .. } if !table.is_empty() => {
            outer_scopes.iter().any(|scope| scope == table)
                && !inner_scopes.iter().any(|scope| scope == table)
        }
        _ => false,
    }
}

fn first_column_values(
    result: crate::storage::query::unified::UnifiedResult,
) -> RedDBResult<Vec<Value>> {
    if result.columns.len() > 1 {
        return Err(RedDBError::Query(
            "expression subquery must return exactly one column".to_string(),
        ));
    }
    let fallback_column = result
        .records
        .first()
        .and_then(|record| record.column_names().into_iter().next())
        .map(|name| name.to_string());
    let column = result.columns.first().cloned().or(fallback_column);
    let Some(column) = column else {
        return Ok(Vec::new());
    };
    Ok(result
        .records
        .iter()
        .map(|record| record.get(column.as_str()).cloned().unwrap_or(Value::Null))
        .collect())
}

fn parse_timestamp_to_ms(s: &str) -> Option<u128> {
    // Bare integer ms.
    if let Ok(n) = s.parse::<u128>() {
        return Some(n);
    }
    // Fallback: ISO-8601 like 2030-01-02 03:04:05 — accept the date
    // portion only (midnight UTC). Full RFC3339 parsing is a stretch
    // goal; the common case is `'2030-01-01'`.
    if let Some(date) = s.split_whitespace().next() {
        let parts: Vec<&str> = date.split('-').collect();
        if parts.len() == 3 {
            let (y, m, d) = (parts[0], parts[1], parts[2]);
            if let (Ok(y), Ok(m), Ok(d)) = (y.parse::<i64>(), m.parse::<u32>(), d.parse::<u32>()) {
                // Days since 1970-01-01 — simple Julian arithmetic
                // suitable for years 1970-2100. Good enough for test
                // fixtures; precise parsing lands when we wire chrono.
                let days_in = days_from_civil(y, m, d);
                return Some((days_in as u128) * 86_400_000u128);
            }
        }
    }
    None
}

/// Days from Unix epoch using H. Hinnant's civil-from-days algorithm.
/// Robust for the entire Gregorian range; used by `parse_timestamp_to_ms`.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as u64 + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i64 - 719468
}

fn walk_plan_node(
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    depth: usize,
    out: &mut Vec<crate::storage::query::unified::UnifiedRecord>,
) {
    use std::sync::Arc;
    let mut rec = crate::storage::query::unified::UnifiedRecord::default();
    rec.set_arc(Arc::from("op"), Value::text(node.operator.clone()));
    rec.set_arc(
        Arc::from("source"),
        node.source.clone().map(Value::text).unwrap_or(Value::Null),
    );
    rec.set_arc(Arc::from("est_rows"), Value::Float(node.estimated_rows));
    rec.set_arc(Arc::from("est_cost"), Value::Float(node.operator_cost));
    rec.set_arc(Arc::from("depth"), Value::Integer(depth as i64));
    out.push(rec);
    for child in &node.children {
        walk_plan_node(child, depth + 1, out);
    }
}

#[cfg(test)]
mod inline_graph_tvf_tests {
    use super::*;

    fn scopes_for(sql: &str) -> HashSet<String> {
        let expr = crate::storage::query::parser::parse(sql)
            .expect("parse")
            .query;
        query_expr_result_cache_scopes(&expr)
    }

    #[test]
    fn inline_tvf_cache_scopes_include_source_collections() {
        // The result-cache key for the inline form must derive from the
        // `nodes`/`edges` source collections so a write to either invalidates
        // the cached result (issue #799).
        let scopes = scopes_for(
            "SELECT * FROM components(nodes => (SELECT id FROM hosts), edges => (SELECT src, dst FROM links))",
        );
        assert!(scopes.contains("hosts"), "nodes source scoped: {scopes:?}");
        assert!(scopes.contains("links"), "edges source scoped: {scopes:?}");
    }

    #[test]
    fn graph_collection_tvf_cache_scope_is_graph_argument() {
        // The graph-collection form still materializes the active graph, but
        // result-cache invalidation is scoped to the named graph argument so
        // INSERT INTO g NODE/EDGE invalidates cached TVF rows.
        let scopes = scopes_for("SELECT * FROM components(g)");
        assert!(scopes.contains("g"), "collection form scoped: {scopes:?}");
    }

    #[test]
    fn abstract_degree_centrality_counts_undirected_endpoints() {
        let nodes = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let edges = vec![
            ("a".to_string(), "b".to_string(), 1.0_f32),
            ("b".to_string(), "c".to_string(), 1.0_f32),
        ];
        let degrees = abstract_degree_centrality(&nodes, &edges);
        assert_eq!(
            degrees,
            vec![
                ("a".to_string(), 1),
                ("b".to_string(), 2),
                ("c".to_string(), 1),
            ]
        );
    }
}
