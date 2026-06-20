//! Time-series DDL execution

use std::collections::HashMap;
use std::sync::Arc;

use super::*;

const TIMESERIES_META_COLLECTION: &str = "red_timeseries_meta";

impl RedDBRuntime {
    pub fn execute_create_timeseries(
        &self,
        raw_query: &str,
        query: &CreateTimeSeriesQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        for spec in &query.downsample_policies {
            crate::storage::timeseries::retention::DownsamplePolicy::parse(spec).ok_or_else(
                || RedDBError::Query(format!("invalid downsample policy '{}'", spec)),
            )?;
        }

        let store = self.inner.db.store();
        let exists = store.get_collection(&query.name).is_some();
        if exists {
            if query.if_not_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("timeseries '{}' already exists", query.name),
                    "create",
                ));
            }
            return Err(RedDBError::Query(format!(
                "timeseries '{}' already exists",
                query.name
            )));
        }
        store
            .create_collection(&query.name)
            .map_err(|e| RedDBError::Internal(e.to_string()))?;
        if let Some(ttl_ms) = query.retention_ms {
            self.inner
                .db
                .set_collection_default_ttl_ms(&query.name, ttl_ms);
        }
        // CREATE HYPERTABLE declares the collection as a Table so
        // INSERT goes through the row path (which now includes
        // automatic chunk routing). Plain CREATE TIMESERIES keeps
        // the native TimeSeries contract with its metric/value/tags
        // column convention.
        let contract = if query.hypertable.is_some() {
            hypertable_collection_contract(query)
        } else {
            timeseries_collection_contract(query)
        };
        self.inner
            .db
            .save_collection_contract(contract)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        // Issue #747 — record per-collection tenant ownership when an
        // active tenant context exists, so typed surfaces like
        // `red.timeseries` can scope rows to the creating tenant just
        // like `red.tables` does for `CREATE TABLE`.
        if let Some(tenant_id) = crate::runtime::impl_core::current_tenant() {
            store.set_config_tree(
                &format!("red.collection_tenants.{}", query.name),
                &crate::serde_json::Value::String(tenant_id),
            );
        }
        save_timeseries_metadata(store.as_ref(), query)?;

        // `CREATE HYPERTABLE` additionally registers a HypertableSpec
        // so chunk routing + retention sweeps can address this table.
        // Plain `CREATE TIMESERIES` leaves `hypertable` = None and the
        // runtime behaves as before.
        if let Some(ht) = &query.hypertable {
            let mut spec = crate::storage::timeseries::HypertableSpec::new(
                query.name.clone(),
                ht.time_column.clone(),
                ht.chunk_interval_ns,
            );
            if let Some(ttl) = ht.default_ttl_ns {
                spec = spec.with_ttl_ns(ttl);
            }
            self.inner.db.hypertables().register(spec);
        }

        self.invalidate_result_cache();
        self.inner
            .db
            .persist_metadata()
            .map_err(|e| RedDBError::Internal(e.to_string()))?;
        // Issue #120 — surface timeseries / hypertable in the
        // schema-vocabulary. The hypertable variant carries the
        // declared time column.
        let columns: Vec<String> = query
            .hypertable
            .as_ref()
            .map(|ht| vec![ht.time_column.clone()])
            .unwrap_or_else(|| vec!["metric".to_string(), "value".to_string()]);
        self.schema_vocabulary_apply(
            crate::runtime::schema_vocabulary::DdlEvent::CreateCollection {
                collection: query.name.clone(),
                columns,
                type_tags: Vec::new(),
                description: None,
            },
        );

        let noun = if query.hypertable.is_some() {
            "hypertable"
        } else {
            "timeseries"
        };
        let mut msg = format!("{noun} '{}' created", query.name);
        if let Some(ret) = query.retention_ms {
            msg.push_str(&format!(" (retention={}ms)", ret));
        }
        if let Some(cs) = query.chunk_size {
            msg.push_str(&format!(" (chunk_size={})", cs));
        }
        if !query.downsample_policies.is_empty() {
            msg.push_str(&format!(
                " (downsample_policies={})",
                query.downsample_policies.len()
            ));
        }
        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &msg,
            "create",
        ))
    }

    pub fn execute_drop_timeseries(
        &self,
        raw_query: &str,
        query: &DropTimeSeriesQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        let store = self.inner.db.store();
        if super::impl_ddl::is_system_schema_name(&query.name) {
            return Err(RedDBError::Query("system schema is read-only".to_string()));
        }
        if store.get_collection(&query.name).is_none() {
            if query.if_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("timeseries '{}' does not exist", query.name),
                    "drop",
                ));
            }
            return Err(RedDBError::NotFound(format!(
                "timeseries '{}' not found",
                query.name
            )));
        }
        let actual = crate::runtime::ddl::polymorphic_resolver::resolve(
            &query.name,
            &self.inner.db.catalog_model_snapshot(),
        )?;
        if actual != crate::catalog::CollectionModel::TimeSeries
            && actual != crate::catalog::CollectionModel::Table
        {
            crate::runtime::ddl::polymorphic_resolver::ensure_model_match(
                crate::catalog::CollectionModel::TimeSeries,
                actual,
            )?;
        }
        // Remove from the hypertable registry before dropping the
        // underlying collection — the registry lookup is cheap and
        // staying consistent is the point of having a separate call.
        let _ = self.inner.db.hypertables().unregister(&query.name);
        store
            .drop_collection(&query.name)
            .map_err(|e| RedDBError::Internal(e.to_string()))?;
        self.inner.db.clear_collection_default_ttl_ms(&query.name);
        self.inner
            .db
            .remove_collection_contract(&query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        remove_timeseries_metadata(store.as_ref(), &query.name);
        self.invalidate_result_cache();
        self.inner
            .db
            .persist_metadata()
            .map_err(|e| RedDBError::Internal(e.to_string()))?;
        // Issue #120 — invalidate the schema-vocabulary entry for the
        // dropped timeseries / hypertable.
        self.schema_vocabulary_apply(
            crate::runtime::schema_vocabulary::DdlEvent::DropCollection {
                collection: query.name.clone(),
            },
        );
        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("timeseries '{}' dropped", query.name),
            "drop",
        ))
    }

    /// Seal every still-open chunk of a hypertable, routing each seal
    /// through [`seal_chunk_with_config`](crate::storage::timeseries::chunk::seal_chunk_with_config)
    /// — the production caller PRD #850 lacked (#911). For a collection
    /// whose contract carries `analytical_storage.columnar = true`, the
    /// chunk's rows are materialised from the entity store into a
    /// `TimeSeriesChunk`, sealed columnar, and the resulting RDCC
    /// `ColumnBlock` recorded in `ChunkMeta.columnar_page` (bytes stashed
    /// for read-back). Without the flag the chunk falls to the row seal
    /// and `columnar_page` stays `None` — no behaviour change. Returns the
    /// number of chunks sealed columnar.
    pub fn seal_hypertable_chunks(&self, collection: &str) -> RedDBResult<usize> {
        let analytical = self
            .inner
            .db
            .collection_contract(collection)
            .and_then(|c| c.analytical_storage.clone());
        let registry = self.inner.db.hypertables();
        let Some(spec) = registry.get(collection) else {
            return Ok(0);
        };
        let time_col = spec.time_column.clone();
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(0);
        };

        let mut sealed_columnar = 0usize;
        for meta in registry.show_chunks(collection) {
            if meta.sealed {
                continue;
            }
            let start = meta.id.start_ns;
            let end = meta.end_ns_exclusive;

            // Materialise (ts, value) for rows whose time-column value
            // lands in this chunk's `[start, end)` window — the same
            // entity/row reader the read-bridge serves row chunks from.
            let points = materialize_row_points(&manager, &time_col, start, end);

            let mut chunk = crate::storage::timeseries::TimeSeriesChunk::with_max_points(
                collection.to_string(),
                HashMap::new(),
                points.len().max(1),
            );
            for (ts, value) in &points {
                chunk.append(*ts, *value);
            }

            let routed = crate::storage::timeseries::chunk::seal_chunk_with_config(
                &mut chunk,
                analytical.as_ref(),
                start,
                0,
            )
            .map_err(|err| RedDBError::Internal(format!("columnar seal failed: {err:?}")))?;

            match routed {
                crate::storage::timeseries::chunk::SealedChunkStorage::Columnar(bytes) => {
                    let page = crate::storage::engine::PageLocation::new(0, 0, bytes.len() as u32);
                    registry.seal_chunk_columnar(&meta.id, page, bytes);
                    sealed_columnar += 1;
                }
                crate::storage::timeseries::chunk::SealedChunkStorage::Row => {
                    registry.seal_chunk(&meta.id);
                }
            }
        }
        Ok(sealed_columnar)
    }

    /// Count this hypertable's chunks that were sealed columnar — i.e.
    /// whose `ChunkMeta.columnar_page` is set (#911). Lets a caller assert
    /// the columnar arm fired without exposing the registry's chunk type.
    pub fn columnar_chunk_count(&self, collection: &str) -> usize {
        self.inner
            .db
            .hypertables()
            .show_chunks(collection)
            .iter()
            .filter(|meta| meta.columnar_page.is_some())
            .count()
    }

    /// Read back a columnar-sealed chunk's points over `[start_ns, end_ns]`
    /// (inclusive) via the #856 column-block range scan, decoding the RDCC
    /// `ColumnBlock` recorded by [`seal_hypertable_chunks`](Self::seal_hypertable_chunks).
    /// `None` when the chunk was not sealed columnar (or its bytes are not
    /// RAM-resident). Points come back as `(timestamp_ns, value)`.
    pub fn columnar_chunk_points(
        &self,
        collection: &str,
        chunk_start_ns: u64,
        start_ns: u64,
        end_ns: u64,
    ) -> Option<Vec<(u64, f64)>> {
        let id = crate::storage::timeseries::ChunkId {
            hypertable: collection.to_string(),
            start_ns: chunk_start_ns,
        };
        let bytes = self.inner.db.hypertables().columnar_block(&id)?;
        let scan =
            crate::storage::timeseries::chunk::query_column_block_range(&bytes, start_ns, end_ns)
                .ok()?;
        Some(
            scan.points
                .iter()
                .map(|p| (p.timestamp_ns, p.value))
                .collect(),
        )
    }

    /// Read-bridge (#861): read every point of `collection` in the
    /// inclusive range `[start_ns, end_ns]`, dispatching **per chunk** on
    /// its storage format so row-stored and columnar (`RDCC`) chunks
    /// coexist after `COLUMNAR` is enabled — with no mass rewrite of the
    /// pre-existing row data.
    ///
    /// Each chunk's [`ChunkMeta::format`](crate::storage::timeseries::ChunkMeta::format)
    /// is the format-version gate:
    /// - [`ChunkFormat::ColumnarV1`] → decode the chunk's RDCC `ColumnBlock`
    ///   through the granule-pruned column-block range scan, after
    ///   confirming the block's embedded `format_version` is one this build
    ///   understands ([`peek_column_block_version`]).
    /// - [`ChunkFormat::Row`] → materialise the chunk's rows from the
    ///   entity/row store, the same reader the seal sources from.
    ///
    /// Points come back merged and timestamp-ordered, so a caller sees one
    /// logical series regardless of how each chunk is physically stored.
    /// Chunk windows are disjoint, so a columnar chunk is read only through
    /// its RDCC block and never double-counted via the row path.
    pub fn read_bridge_points(
        &self,
        collection: &str,
        start_ns: u64,
        end_ns: u64,
    ) -> RedDBResult<Vec<(u64, f64)>> {
        use crate::storage::timeseries::ChunkFormat;
        use crate::storage::unified::column_block::{
            peek_column_block_version, COLUMN_BLOCK_VERSION_V1,
        };

        let registry = self.inner.db.hypertables();
        let Some(spec) = registry.get(collection) else {
            return Ok(Vec::new());
        };
        let time_col = spec.time_column.clone();
        let store = self.inner.db.store();

        let mut out: Vec<(u64, f64)> = Vec::new();
        for meta in registry.show_chunks(collection) {
            // Skip chunks whose observed window cannot intersect the query.
            // An empty chunk has min_ts_ns == u64::MAX, so it is skipped.
            if meta.max_ts_ns < start_ns || meta.min_ts_ns > end_ns {
                continue;
            }
            match meta.format() {
                ChunkFormat::ColumnarV1 => {
                    // RDCC reader. Bytes may be absent post-restart (pending
                    // the durable page-write bridge); nothing to read then.
                    let Some(bytes) = registry.columnar_block(&meta.id) else {
                        continue;
                    };
                    // Format-version gate: reject a block this build cannot
                    // read rather than mis-decode it.
                    match peek_column_block_version(&bytes) {
                        Some(COLUMN_BLOCK_VERSION_V1) => {}
                        Some(v) => {
                            return Err(RedDBError::Internal(format!(
                                "chunk {} @ {} carries unsupported columnar format version {v}",
                                meta.id.hypertable, meta.id.start_ns
                            )));
                        }
                        None => {
                            return Err(RedDBError::Internal(format!(
                                "chunk {} @ {} is flagged columnar but its block is not RDCC",
                                meta.id.hypertable, meta.id.start_ns
                            )));
                        }
                    }
                    let scan = crate::storage::timeseries::chunk::query_column_block_range(
                        &bytes, start_ns, end_ns,
                    )
                    .map_err(|err| {
                        RedDBError::Internal(format!("columnar read-bridge decode failed: {err:?}"))
                    })?;
                    out.extend(scan.points.iter().map(|p| (p.timestamp_ns, p.value)));
                }
                ChunkFormat::Row => {
                    // Row reader: materialise the chunk window, then filter
                    // to the inclusive query range (mirrors the columnar
                    // scan's `[start_ns, end_ns]` contract).
                    let Some(manager) = store.get_collection(collection) else {
                        continue;
                    };
                    let chunk_start = meta.id.start_ns;
                    let chunk_end = meta.end_ns_exclusive;
                    out.extend(
                        materialize_row_points(&manager, &time_col, chunk_start, chunk_end)
                            .into_iter()
                            .filter(|(ts, _)| *ts >= start_ns && *ts <= end_ns),
                    );
                }
            }
        }
        out.sort_by_key(|(ts, _)| *ts);
        Ok(out)
    }
}

/// Materialise `(timestamp_ns, value)` rows from the entity/row store for
/// the half-open chunk window `[start, end)`, timestamp-ordered. This is
/// the shared row reader: the columnar seal sources its chunk from it, and
/// the read-bridge serves row-stored chunks through it (#861). `time_col`
/// names the time axis; the value column follows the `value` convention.
fn materialize_row_points(
    manager: &crate::storage::unified::SegmentManager,
    time_col: &str,
    start: u64,
    end: u64,
) -> Vec<(u64, f64)> {
    let mut points: Vec<(u64, f64)> = manager
        .query_all(|entity| {
            entity
                .data
                .as_row()
                .and_then(|row| row.get_field(time_col))
                .and_then(field_as_u64)
                .is_some_and(|ts| ts >= start && ts < end)
        })
        .iter()
        .filter_map(|entity| {
            let row = entity.data.as_row()?;
            let ts = row.get_field(time_col).and_then(field_as_u64)?;
            let value = row.get_field("value").and_then(field_as_f64).unwrap_or(0.0);
            Some((ts, value))
        })
        .collect();
    points.sort_by_key(|(ts, _)| *ts);
    points
}

/// Read a row field as a non-negative `u64` timestamp, accepting the
/// integer shapes the INSERT path stores for a time column (#911).
fn field_as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Integer(n) | Value::BigInt(n) | Value::Timestamp(n) if *n >= 0 => Some(*n as u64),
        Value::UnsignedInteger(n) => Some(*n),
        _ => None,
    }
}

/// Read a row field as `f64` for the columnar value column (#911).
fn field_as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Float(f) => Some(*f),
        Value::Integer(n) | Value::BigInt(n) => Some(*n as f64),
        Value::UnsignedInteger(n) => Some(*n as f64),
        _ => None,
    }
}

fn save_timeseries_metadata(
    store: &crate::storage::unified::UnifiedStore,
    query: &CreateTimeSeriesQuery,
) -> RedDBResult<()> {
    remove_timeseries_metadata(store, &query.name);
    let _ = store.get_or_create_collection(TIMESERIES_META_COLLECTION);

    let mut fields = HashMap::new();
    fields.insert(
        "kind".to_string(),
        Value::text("timeseries_config".to_string()),
    );
    fields.insert("series".to_string(), Value::text(query.name.clone()));
    fields.insert(
        "retention_ms".to_string(),
        query
            .retention_ms
            .map(Value::UnsignedInteger)
            .unwrap_or(Value::Null),
    );
    fields.insert(
        "chunk_size".to_string(),
        query
            .chunk_size
            .map(|value| Value::UnsignedInteger(value as u64))
            .unwrap_or(Value::Null),
    );
    fields.insert(
        "downsample_policies".to_string(),
        Value::Array(
            query
                .downsample_policies
                .iter()
                .cloned()
                .map(Value::text)
                .collect(),
        ),
    );

    store
        .insert_auto(
            TIMESERIES_META_COLLECTION,
            UnifiedEntity::new(
                EntityId::new(0),
                EntityKind::TableRow {
                    table: Arc::from(TIMESERIES_META_COLLECTION),
                    row_id: 0,
                },
                EntityData::Row(crate::storage::RowData {
                    columns: Vec::new(),
                    named: Some(fields),
                    schema: None,
                }),
            ),
        )
        .map_err(|err| RedDBError::Internal(err.to_string()))?;

    Ok(())
}

fn remove_timeseries_metadata(store: &crate::storage::unified::UnifiedStore, series: &str) {
    let Some(manager) = store.get_collection(TIMESERIES_META_COLLECTION) else {
        return;
    };
    let rows = manager.query_all(|entity| {
        entity.data.as_row().is_some_and(|row| {
            row.get_field("series").is_some_and(
                |value| matches!(value, Value::Text(candidate) if &**candidate == series),
            )
        })
    });
    for row in rows {
        let _ = store.delete(TIMESERIES_META_COLLECTION, row.id);
    }
}

/// Build the contract's [`AnalyticalStorageConfig`] when the DDL carried
/// `COLUMNAR` (#911). `time_key` is the column carrying the time axis —
/// the hypertable's declared time column, or the timeseries `timestamp`
/// convention. `None` when columnar was not requested, so non-columnar
/// collections keep the row engine default.
fn analytical_storage_for(
    columnar: bool,
    time_key: &str,
) -> Option<crate::catalog::AnalyticalStorageConfig> {
    columnar.then(|| crate::catalog::AnalyticalStorageConfig {
        columnar: true,
        time_key: time_key.to_string(),
        order_by_key: None,
    })
}

fn hypertable_collection_contract(
    query: &CreateTimeSeriesQuery,
) -> crate::physical::CollectionContract {
    let now = current_unix_ms();
    let time_key = query
        .hypertable
        .as_ref()
        .map(|ht| ht.time_column.as_str())
        .unwrap_or("timestamp");
    crate::physical::CollectionContract {
        name: query.name.clone(),
        // Table model — rows go through the normal INSERT path,
        // which now calls HypertableRegistry::route after each row
        // lands. Hypertable-specific behaviour (chunk bounds, TTL
        // sweeps) lives on the registry, not the contract.
        declared_model: crate::catalog::CollectionModel::Table,
        schema_mode: crate::catalog::SchemaMode::SemiStructured,
        origin: crate::physical::ContractOrigin::Explicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: query.retention_ms,
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
        // Hypertable data is conceptually immutable once the chunk
        // seals. Reject UPDATE / DELETE at parse time and give the
        // operator a clear message instead of silent coalescing.
        append_only: true,
        subscriptions: Vec::new(),
        analytics_config: Vec::new(),
        session_key: None,
        session_gap_ms: None,
        retention_duration_ms: None,
        analytical_storage: analytical_storage_for(query.columnar, time_key),

        ai_policy: None,
    }
}

fn timeseries_collection_contract(
    query: &CreateTimeSeriesQuery,
) -> crate::physical::CollectionContract {
    let now = current_unix_ms();
    crate::physical::CollectionContract {
        name: query.name.clone(),
        declared_model: crate::catalog::CollectionModel::TimeSeries,
        schema_mode: crate::catalog::SchemaMode::SemiStructured,
        origin: crate::physical::ContractOrigin::Explicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: query.retention_ms,
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
        // Time-series collections are append-only by nature — the
        // storage model forbids in-place UPDATE already, so the flag
        // makes the catalog honest rather than changing semantics.
        append_only: true,
        subscriptions: Vec::new(),
        analytics_config: Vec::new(),
        // `WITH SESSION_KEY <col> SESSION_GAP <duration>` from the
        // CREATE TIMESERIES DDL becomes the default partition/gap
        // pairing for the SESSIONIZE operator (slice 2+). Stored on
        // the contract so a restart preserves the values without an
        // extra metadata side-table.
        session_key: query.session_key.clone(),
        session_gap_ms: query.session_gap_ms,
        retention_duration_ms: None,
        // Plain timeseries store points under the `timestamp` axis
        // convention (the `value` column carries the measurement).
        analytical_storage: analytical_storage_for(query.columnar, "timestamp"),

        ai_policy: None,
    }
}

fn current_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
