//! Runtime ranking / metrics / SLO / analytics-source DDL.
//!
//! Extracted verbatim from `impl_core.rs` (impl_core slice 6/10, issue #1627).
//! Houses the ranking, metrics, SLO, and analytics-source execution family:
//!
//! - **Free helpers** — `record_column_f64`, `record_rid_u64`, `RankedHeadEntry`.
//! - **Execute methods** — `execute_create_metric`, `execute_create_ranking`,
//!   `execute_show_rankings`, `execute_rank_of`, `execute_rank_range`,
//!   `compute_exact_head_rank`, `compute_ranked_head_entries`,
//!   `execute_approx_rank_of`, `compute_approx_rank`, `execute_alter_metric`,
//!   `execute_create_slo`, `execute_create_analytics_source`.
use super::*;

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

struct RankedHeadEntry {
    rank: u64,
    record: crate::storage::query::unified::UnifiedRecord,
}

impl RedDBRuntime {
    pub(crate) fn execute_create_metric(
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
    pub(crate) fn execute_create_ranking(
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
    pub(crate) fn execute_show_rankings(&self, raw_query: &str) -> RedDBResult<RuntimeQueryResult> {
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
    pub(crate) fn execute_rank_of(
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
    pub(crate) fn execute_rank_range(
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
    pub(crate) fn execute_approx_rank_of(
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

    pub(crate) fn execute_alter_metric(
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

    pub(crate) fn execute_create_slo(
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

    pub(crate) fn execute_create_analytics_source(
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
