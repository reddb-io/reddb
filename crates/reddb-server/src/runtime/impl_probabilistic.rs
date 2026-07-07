//! Execution of probabilistic data structure commands (HLL, SKETCH, FILTER)

use super::*;
use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};

const PROB_HLL_STATE_PREFIX: &str = "red.probabilistic.hll.";
const PROB_SKETCH_STATE_PREFIX: &str = "red.probabilistic.sketch.";
const PROB_FILTER_STATE_PREFIX: &str = "red.probabilistic.filter.";
const PROB_ENCODING_MARKER_KEY: &str = "red.probabilistic.state_encoding";
const PROB_ENCODING_RAW_V1: &str = "raw-v1";
const PROB_WAL_KIND_HLL: u8 = 1;
const PROB_WAL_KIND_SKETCH: u8 = 2;
const PROB_WAL_KIND_FILTER: u8 = 3;
const PROB_WAL_OP_ADD: u8 = 1;
const PROB_WAL_OP_DELETE: u8 = 2;
const PROB_WAL_OP_SNAPSHOT: u8 = 3;
const PROB_WAL_OP_DROP: u8 = 4;

fn probabilistic_read<'a, T>(lock: &'a RwLock<T>, _name: &str) -> RwLockReadGuard<'a, T> {
    lock.read()
}

fn probabilistic_write<'a, T>(lock: &'a RwLock<T>, _name: &str) -> RwLockWriteGuard<'a, T> {
    lock.write()
}

fn probabilistic_collection_contract(
    name: &str,
    model: crate::catalog::CollectionModel,
) -> crate::physical::CollectionContract {
    let now = crate::utils::now_unix_millis() as u128;
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
        analytics_config: Vec::new(),
        session_key: None,
        session_gap_ms: None,
        retention_duration_ms: None,
        analytical_storage: None,

        ai_policy: None,
    }
}

fn hll_precision_mismatch_error(
    operation: &str,
    expected_name: &str,
    expected_precision: u8,
    actual_name: &str,
    actual_precision: u8,
) -> RedDBError {
    RedDBError::Query(format!(
        "HLL {operation} requires matching precision; '{expected_name}' uses precision {expected_precision}, but '{actual_name}' uses precision {actual_precision}"
    ))
}

enum ProbabilisticReadProjection {
    Cardinality { label: String },
    Freq { element: String, label: String },
    Contains { element: String, label: String },
}

struct ProbabilisticStateEntry {
    name: String,
    bytes: Vec<u8>,
    migrated_from_legacy_hex: bool,
}

impl RedDBRuntime {
    pub(crate) fn load_probabilistic_state(&self) -> RedDBResult<()> {
        let reject_legacy_hex = self.probabilistic_raw_encoding_marker();
        let mut migrated_legacy_hex = false;
        {
            let entries = self.latest_probabilistic_state_entries(
                PROB_HLL_STATE_PREFIX,
                "HLL",
                reject_legacy_hex,
            )?;
            let mut hlls =
                probabilistic_write(&self.inner.probabilistic.hlls, "probabilistic HLL store");
            for entry in entries {
                let Some(hll) = crate::storage::primitives::hyperloglog::HyperLogLog::from_bytes(
                    entry.bytes.clone(),
                ) else {
                    return Err(RedDBError::Internal(format!(
                        "invalid persisted HLL state for '{}'",
                        entry.name
                    )));
                };
                if entry.migrated_from_legacy_hex {
                    self.persist_probabilistic_blob(
                        PROB_HLL_STATE_PREFIX,
                        &entry.name,
                        &entry.bytes,
                    )?;
                    migrated_legacy_hex = true;
                }
                hlls.insert(entry.name, hll);
            }
        }

        {
            let entries = self.latest_probabilistic_state_entries(
                PROB_SKETCH_STATE_PREFIX,
                "SKETCH",
                reject_legacy_hex,
            )?;
            let mut sketches = probabilistic_write(
                &self.inner.probabilistic.sketches,
                "probabilistic sketch store",
            );
            for entry in entries {
                let sketch =
                    crate::storage::primitives::count_min_sketch::CountMinSketch::from_bytes(
                        &entry.bytes,
                    )
                    .ok_or_else(|| {
                        RedDBError::Internal(format!(
                            "invalid persisted SKETCH state for '{}'",
                            entry.name
                        ))
                    })?;
                if entry.migrated_from_legacy_hex {
                    self.persist_probabilistic_blob(
                        PROB_SKETCH_STATE_PREFIX,
                        &entry.name,
                        &entry.bytes,
                    )?;
                    migrated_legacy_hex = true;
                }
                sketches.insert(entry.name, sketch);
            }
        }

        {
            let entries = self.latest_probabilistic_state_entries(
                PROB_FILTER_STATE_PREFIX,
                "FILTER",
                reject_legacy_hex,
            )?;
            let mut filters = probabilistic_write(
                &self.inner.probabilistic.filters,
                "probabilistic filter store",
            );
            for entry in entries {
                let filter = crate::storage::primitives::cuckoo_filter::CuckooFilter::from_bytes(
                    &entry.bytes,
                )
                .ok_or_else(|| {
                    RedDBError::Internal(format!(
                        "invalid persisted FILTER state for '{}'",
                        entry.name
                    ))
                })?;
                if entry.migrated_from_legacy_hex {
                    self.persist_probabilistic_blob(
                        PROB_FILTER_STATE_PREFIX,
                        &entry.name,
                        &entry.bytes,
                    )?;
                    migrated_legacy_hex = true;
                }
                filters.insert(entry.name, filter);
            }
        }

        if migrated_legacy_hex {
            self.persist_probabilistic_encoding_marker();
        }
        self.apply_replayed_probabilistic_deltas()?;
        Ok(())
    }

    fn latest_probabilistic_state_entries(
        &self,
        prefix: &str,
        kind: &str,
        reject_legacy_hex: bool,
    ) -> RedDBResult<Vec<ProbabilisticStateEntry>> {
        let Some(manager) = self.inner.db.store().get_collection("red_config") else {
            return Ok(Vec::new());
        };
        let mut latest: std::collections::HashMap<String, (u64, Option<Value>)> =
            std::collections::HashMap::new();
        for entity in manager.query_all(|_| true) {
            let EntityData::Row(row) = &entity.data else {
                continue;
            };
            let Some(named) = &row.named else {
                continue;
            };
            let Some(Value::Text(key)) = named.get("key") else {
                continue;
            };
            let Some(encoded_name) = key.strip_prefix(prefix) else {
                continue;
            };
            let value = match named.get("value") {
                Some(Value::Blob(value)) => Some(Value::Blob(value.clone())),
                Some(Value::Text(value)) => Some(Value::Text(value.clone())),
                Some(Value::Null) => None,
                _ => continue,
            };
            let entity_id = entity.id.raw();
            match latest.get(encoded_name) {
                Some((existing_id, _)) if *existing_id > entity_id => {}
                _ => {
                    latest.insert(encoded_name.to_string(), (entity_id, value));
                }
            }
        }

        let mut entries = Vec::new();
        for (encoded_name, (_, value)) in latest {
            let Some(value) = value else {
                continue;
            };
            let Some(name) = hex::decode(&encoded_name)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
            else {
                continue;
            };
            match value {
                Value::Blob(bytes) => entries.push(ProbabilisticStateEntry {
                    name,
                    bytes,
                    migrated_from_legacy_hex: false,
                }),
                Value::Text(data_hex) if reject_legacy_hex => {
                    return Err(RedDBError::Internal(format!(
                        "legacy hex-encoded {kind} state for '{name}' is rejected after probabilistic state migrated to raw bytes"
                    )));
                }
                Value::Text(data_hex) => {
                    let bytes = hex::decode(data_hex.as_ref()).map_err(|err| {
                        RedDBError::Internal(format!(
                            "invalid legacy hex-encoded {kind} state for '{name}': {err}"
                        ))
                    })?;
                    entries.push(ProbabilisticStateEntry {
                        name,
                        bytes,
                        migrated_from_legacy_hex: true,
                    });
                }
                _ => {}
            }
        }
        Ok(entries)
    }

    fn probabilistic_raw_encoding_marker(&self) -> bool {
        let Some(manager) = self.inner.db.store().get_collection("red_config") else {
            return false;
        };
        let mut latest: Option<(u64, bool)> = None;
        for entity in manager.query_all(|_| true) {
            let EntityData::Row(row) = &entity.data else {
                continue;
            };
            let Some(named) = &row.named else {
                continue;
            };
            let Some(Value::Text(key)) = named.get("key") else {
                continue;
            };
            if key.as_ref() != PROB_ENCODING_MARKER_KEY {
                continue;
            }
            let is_raw = matches!(
                named.get("value"),
                Some(Value::Text(value)) if value.as_ref() == PROB_ENCODING_RAW_V1
            );
            let entity_id = entity.id.raw();
            match latest {
                Some((existing_id, _)) if existing_id > entity_id => {}
                _ => latest = Some((entity_id, is_raw)),
            }
        }
        latest.map(|(_, is_raw)| is_raw).unwrap_or(false)
    }

    fn persist_probabilistic_blob(
        &self,
        prefix: &str,
        name: &str,
        bytes: &[u8],
    ) -> RedDBResult<()> {
        let key = format!("{prefix}{}", hex::encode(name.as_bytes()));
        self.compact_config_key(&key);
        self.insert_config_value(&key, Value::Blob(bytes.to_vec()))?;
        self.persist_probabilistic_encoding_marker();
        Ok(())
    }

    fn delete_probabilistic_blob(&self, prefix: &str, name: &str) -> RedDBResult<()> {
        let key = format!("{prefix}{}", hex::encode(name.as_bytes()));
        self.compact_config_key(&key);
        self.insert_config_value(&key, Value::Null)?;
        Ok(())
    }

    pub(crate) fn persist_probabilistic_snapshots(&self) -> RedDBResult<()> {
        let hll_snapshots: Vec<(String, Vec<u8>)> = {
            let hlls =
                probabilistic_read(&self.inner.probabilistic.hlls, "probabilistic HLL store");
            hlls.iter()
                .map(|(name, hll)| (name.clone(), hll.as_bytes().to_vec()))
                .collect()
        };
        for (name, bytes) in hll_snapshots {
            self.persist_probabilistic_blob(PROB_HLL_STATE_PREFIX, &name, &bytes)?;
            self.append_probabilistic_snapshot_delta(PROB_WAL_KIND_HLL, &name, bytes)?;
        }

        let sketch_snapshots: Vec<(String, Vec<u8>)> = {
            let sketches = probabilistic_read(
                &self.inner.probabilistic.sketches,
                "probabilistic sketch store",
            );
            sketches
                .iter()
                .map(|(name, sketch)| (name.clone(), sketch.as_bytes()))
                .collect()
        };
        for (name, bytes) in sketch_snapshots {
            self.persist_probabilistic_blob(PROB_SKETCH_STATE_PREFIX, &name, &bytes)?;
            self.append_probabilistic_snapshot_delta(PROB_WAL_KIND_SKETCH, &name, bytes)?;
        }

        let filter_snapshots: Vec<(String, Vec<u8>)> = {
            let filters = probabilistic_read(
                &self.inner.probabilistic.filters,
                "probabilistic filter store",
            );
            filters
                .iter()
                .map(|(name, filter)| (name.clone(), filter.as_bytes()))
                .collect()
        };
        for (name, bytes) in filter_snapshots {
            self.persist_probabilistic_blob(PROB_FILTER_STATE_PREFIX, &name, &bytes)?;
            self.append_probabilistic_snapshot_delta(PROB_WAL_KIND_FILTER, &name, bytes)?;
        }

        Ok(())
    }

    fn append_probabilistic_add_delta(
        &self,
        kind: u8,
        name: &str,
        operands: Vec<Vec<u8>>,
    ) -> RedDBResult<()> {
        self.append_probabilistic_delta(kind, PROB_WAL_OP_ADD, name, operands)
    }

    fn append_probabilistic_snapshot_delta(
        &self,
        kind: u8,
        name: &str,
        bytes: Vec<u8>,
    ) -> RedDBResult<()> {
        self.append_probabilistic_delta(kind, PROB_WAL_OP_SNAPSHOT, name, vec![bytes])
    }

    fn append_probabilistic_drop_delta(&self, kind: u8, name: &str) -> RedDBResult<()> {
        self.append_probabilistic_delta(kind, PROB_WAL_OP_DROP, name, Vec::new())
    }

    fn append_probabilistic_delete_delta(
        &self,
        kind: u8,
        name: &str,
        operands: Vec<Vec<u8>>,
    ) -> RedDBResult<()> {
        self.append_probabilistic_delta(kind, PROB_WAL_OP_DELETE, name, operands)
    }

    fn append_probabilistic_delta(
        &self,
        kind: u8,
        operation: u8,
        name: &str,
        operands: Vec<Vec<u8>>,
    ) -> RedDBResult<()> {
        self.inner
            .db
            .store()
            .append_probabilistic_delta_record(kind, operation, name, operands)
            .map_err(|err| RedDBError::Internal(format!("append probabilistic WAL delta: {err}")))
    }

    fn apply_replayed_probabilistic_deltas(&self) -> RedDBResult<()> {
        let deltas = self.inner.db.store().take_replayed_probabilistic_deltas();
        for (kind, operation, name, operands) in deltas {
            self.apply_replayed_probabilistic_delta(kind, operation, &name, operands)?;
        }
        Ok(())
    }

    fn apply_replayed_probabilistic_delta(
        &self,
        kind: u8,
        operation: u8,
        name: &str,
        operands: Vec<Vec<u8>>,
    ) -> RedDBResult<()> {
        match (kind, operation) {
            (PROB_WAL_KIND_HLL, PROB_WAL_OP_SNAPSHOT) => {
                let bytes = single_probabilistic_operand(operands, "HLL snapshot")?;
                let hll = crate::storage::primitives::hyperloglog::HyperLogLog::from_bytes(bytes)
                    .ok_or_else(|| {
                    RedDBError::Internal(format!("invalid WAL HLL snapshot for '{name}'"))
                })?;
                let mut hlls =
                    probabilistic_write(&self.inner.probabilistic.hlls, "probabilistic HLL store");
                hlls.insert(name.to_string(), hll);
            }
            (PROB_WAL_KIND_HLL, PROB_WAL_OP_ADD) => {
                let mut hlls =
                    probabilistic_write(&self.inner.probabilistic.hlls, "probabilistic HLL store");
                if let Some(hll) = hlls.get_mut(name) {
                    for element in operands {
                        hll.add(&element);
                    }
                }
            }
            (PROB_WAL_KIND_HLL, PROB_WAL_OP_DROP) => {
                let mut hlls =
                    probabilistic_write(&self.inner.probabilistic.hlls, "probabilistic HLL store");
                hlls.remove(name);
            }
            (PROB_WAL_KIND_SKETCH, PROB_WAL_OP_SNAPSHOT) => {
                let bytes = single_probabilistic_operand(operands, "SKETCH snapshot")?;
                let sketch =
                    crate::storage::primitives::count_min_sketch::CountMinSketch::from_bytes(
                        &bytes,
                    )
                    .ok_or_else(|| {
                        RedDBError::Internal(format!("invalid WAL SKETCH snapshot for '{name}'"))
                    })?;
                let mut sketches = probabilistic_write(
                    &self.inner.probabilistic.sketches,
                    "probabilistic sketch store",
                );
                sketches.insert(name.to_string(), sketch);
            }
            (PROB_WAL_KIND_SKETCH, PROB_WAL_OP_ADD) => {
                let (element, count) = sketch_add_operands(operands)?;
                let mut sketches = probabilistic_write(
                    &self.inner.probabilistic.sketches,
                    "probabilistic sketch store",
                );
                if let Some(sketch) = sketches.get_mut(name) {
                    sketch.add(&element, count);
                }
            }
            (PROB_WAL_KIND_SKETCH, PROB_WAL_OP_DROP) => {
                let mut sketches = probabilistic_write(
                    &self.inner.probabilistic.sketches,
                    "probabilistic sketch store",
                );
                sketches.remove(name);
            }
            (PROB_WAL_KIND_FILTER, PROB_WAL_OP_SNAPSHOT) => {
                let bytes = single_probabilistic_operand(operands, "FILTER snapshot")?;
                let filter =
                    crate::storage::primitives::cuckoo_filter::CuckooFilter::from_bytes(&bytes)
                        .ok_or_else(|| {
                            RedDBError::Internal(format!(
                                "invalid WAL FILTER snapshot for '{name}'"
                            ))
                        })?;
                let mut filters = probabilistic_write(
                    &self.inner.probabilistic.filters,
                    "probabilistic filter store",
                );
                filters.insert(name.to_string(), filter);
            }
            (PROB_WAL_KIND_FILTER, PROB_WAL_OP_ADD) => {
                let element = single_probabilistic_operand(operands, "FILTER ADD")?;
                let mut filters = probabilistic_write(
                    &self.inner.probabilistic.filters,
                    "probabilistic filter store",
                );
                if let Some(filter) = filters.get_mut(name) {
                    let _ = filter.insert(&element);
                }
            }
            (PROB_WAL_KIND_FILTER, PROB_WAL_OP_DELETE) => {
                let element = single_probabilistic_operand(operands, "FILTER DELETE")?;
                let mut filters = probabilistic_write(
                    &self.inner.probabilistic.filters,
                    "probabilistic filter store",
                );
                if let Some(filter) = filters.get_mut(name) {
                    let _ = filter.delete(&element);
                }
            }
            (PROB_WAL_KIND_FILTER, PROB_WAL_OP_DROP) => {
                let mut filters = probabilistic_write(
                    &self.inner.probabilistic.filters,
                    "probabilistic filter store",
                );
                filters.remove(name);
            }
            _ => {
                return Err(RedDBError::Internal(format!(
                    "unknown probabilistic WAL delta kind={kind} operation={operation}"
                )));
            }
        }
        Ok(())
    }

    fn persist_probabilistic_encoding_marker(&self) {
        self.compact_config_key(PROB_ENCODING_MARKER_KEY);
        let _ = self.insert_config_value(
            PROB_ENCODING_MARKER_KEY,
            Value::text(PROB_ENCODING_RAW_V1.to_string()),
        );
    }

    fn compact_config_key(&self, key: &str) {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return;
        };
        let ids: Vec<EntityId> = manager
            .query_all(|_| true)
            .into_iter()
            .filter_map(|entity| {
                let EntityData::Row(row) = &entity.data else {
                    return None;
                };
                let named = row.named.as_ref()?;
                let Some(Value::Text(candidate)) = named.get("key") else {
                    return None;
                };
                (candidate.as_ref() == key).then_some(entity.id)
            })
            .collect();
        for id in ids {
            let _ = store.delete("red_config", id);
        }
    }

    fn insert_config_value(&self, key: &str, value: Value) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let _ = store.get_or_create_collection("red_config");
        let entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: std::sync::Arc::from("red_config"),
                row_id: 0,
            },
            EntityData::Row(crate::storage::RowData {
                columns: Vec::new(),
                named: Some(
                    [
                        ("key".to_string(), Value::text(key.to_string())),
                        ("value".to_string(), value),
                    ]
                    .into_iter()
                    .collect(),
                ),
                schema: None,
            }),
        );
        store
            .insert_auto("red_config", entity)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        Ok(())
    }

    fn create_probabilistic_catalog_entry(
        &self,
        name: &str,
        model: crate::catalog::CollectionModel,
    ) -> RedDBResult<()> {
        let store = self.inner.db.store();
        store
            .create_collection(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.inner
            .db
            .save_collection_contract(probabilistic_collection_contract(name, model))
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        if let Some(tenant_id) = crate::runtime::impl_core::current_tenant() {
            store.set_config_tree(
                &format!("red.collection_tenants.{name}"),
                &crate::serde_json::Value::String(tenant_id),
            );
        }
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.invalidate_result_cache();
        Ok(())
    }

    fn drop_probabilistic_catalog_entry(&self, name: &str) -> RedDBResult<()> {
        let store = self.inner.db.store();
        if store.get_collection(name).is_some() {
            store
                .drop_collection(name)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        self.inner
            .db
            .remove_collection_contract(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.invalidate_result_cache();
        Ok(())
    }

    pub(crate) fn execute_probabilistic_select(
        &self,
        query: &TableQuery,
    ) -> RedDBResult<Option<UnifiedResult>> {
        let projections = crate::storage::query::sql_lowering::effective_table_projections(query);
        let mut read_projections = Vec::new();
        for projection in &projections {
            if let Some(read_projection) =
                parse_probabilistic_read_projection(projection, read_projections.len())?
            {
                read_projections.push(read_projection);
            }
        }

        let Some(actual_model) = self
            .inner
            .db
            .collection_contract(&query.table)
            .map(|contract| contract.declared_model)
        else {
            return if read_projections.is_empty() {
                Ok(None)
            } else {
                Err(RedDBError::NotFound(format!(
                    "probabilistic collection '{}' not found",
                    query.table
                )))
            };
        };

        let is_probabilistic_model = matches!(
            actual_model,
            crate::catalog::CollectionModel::Hll
                | crate::catalog::CollectionModel::Sketch
                | crate::catalog::CollectionModel::Filter
        );
        if read_projections.is_empty() {
            return if is_probabilistic_model {
                Err(RedDBError::Query(format!(
                    "probabilistic collection '{}' supports SELECT CARDINALITY, FREQ(...), or CONTAINS(...) read forms",
                    query.table
                )))
            } else {
                Ok(None)
            };
        }

        validate_probabilistic_read_model(&query.table, actual_model, &read_projections)?;
        let (columns, record) =
            self.materialize_probabilistic_select_row(&query.table, &read_projections)?;
        let mut result = UnifiedResult::with_columns(columns);
        if probabilistic_select_row_visible(self, query, &record) {
            result.push(record);
        }
        Ok(Some(result))
    }

    pub fn execute_probabilistic_command(
        &self,
        raw_query: &str,
        cmd: &ProbabilisticCommand,
    ) -> RedDBResult<RuntimeQueryResult> {
        // Mixed read/write surface: count/info/check are read-side and
        // must remain available on read-only replicas; create/add/
        // merge/delete/drop are mutations and must go through the gate.
        let is_mutation = matches!(
            cmd,
            ProbabilisticCommand::CreateHll { .. }
                | ProbabilisticCommand::HllAdd { .. }
                | ProbabilisticCommand::HllMerge { .. }
                | ProbabilisticCommand::DropHll { .. }
                | ProbabilisticCommand::CreateSketch { .. }
                | ProbabilisticCommand::SketchAdd { .. }
                | ProbabilisticCommand::SketchMerge { .. }
                | ProbabilisticCommand::DropSketch { .. }
                | ProbabilisticCommand::CreateFilter { .. }
                | ProbabilisticCommand::FilterAdd { .. }
                | ProbabilisticCommand::FilterDelete { .. }
                | ProbabilisticCommand::DropFilter { .. }
        );
        if is_mutation {
            self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        }
        match cmd {
            // ── HyperLogLog ──────────────────────────────────────────
            ProbabilisticCommand::CreateHll {
                name,
                precision,
                if_not_exists,
            } => {
                let mut hlls =
                    probabilistic_write(&self.inner.probabilistic.hlls, "probabilistic HLL store");
                if hlls.contains_key(name) {
                    if *if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            raw_query.to_string(),
                            &format!("HLL '{}' already exists", name),
                            "create",
                        ));
                    }
                    return Err(RedDBError::Query(format!("HLL '{}' already exists", name)));
                }
                let hll = crate::storage::primitives::hyperloglog::HyperLogLog::with_precision(
                    *precision,
                )
                .ok_or_else(|| {
                    RedDBError::Query(format!(
                        "HLL precision must be between 4 and 18, got {precision}"
                    ))
                })?;
                self.create_probabilistic_catalog_entry(
                    name,
                    crate::catalog::CollectionModel::Hll,
                )?;
                self.persist_probabilistic_blob(PROB_HLL_STATE_PREFIX, name, hll.as_bytes())?;
                self.append_probabilistic_snapshot_delta(
                    PROB_WAL_KIND_HLL,
                    name,
                    hll.as_bytes().to_vec(),
                )?;
                hlls.insert(name.clone(), hll);
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("HLL '{}' created", name),
                    "create",
                ))
            }
            ProbabilisticCommand::HllAdd { name, elements } => {
                let mut hlls =
                    probabilistic_write(&self.inner.probabilistic.hlls, "probabilistic HLL store");
                let hll = hlls
                    .get_mut(name)
                    .ok_or_else(|| RedDBError::NotFound(format!("HLL '{}' not found", name)))?;
                for elem in elements {
                    hll.add(elem.as_bytes());
                }
                self.append_probabilistic_add_delta(
                    PROB_WAL_KIND_HLL,
                    name,
                    elements
                        .iter()
                        .map(|element| element.as_bytes().to_vec())
                        .collect(),
                )?;
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("{} element(s) added to HLL '{}'", elements.len(), name),
                    "insert",
                ))
            }
            ProbabilisticCommand::HllCount { names } => {
                let hlls =
                    probabilistic_read(&self.inner.probabilistic.hlls, "probabilistic HLL store");
                if names.is_empty() {
                    return Err(RedDBError::Query(
                        "HLL COUNT requires at least one HLL name".to_string(),
                    ));
                }
                if names.len() == 1 {
                    let hll = hlls.get(&names[0]).ok_or_else(|| {
                        RedDBError::NotFound(format!("HLL '{}' not found", names[0]))
                    })?;
                    let count = hll.count();
                    let mut result = UnifiedResult::with_columns(vec!["count".into()]);
                    let mut record = UnifiedRecord::new();
                    record.set("count", Value::UnsignedInteger(count));
                    result.push(record);
                    Ok(RuntimeQueryResult {
                        query: raw_query.to_string(),
                        mode: QueryMode::Sql,
                        statement: "hll_count",
                        engine: "runtime-probabilistic",
                        result,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    })
                } else {
                    // Multi-HLL count = union count
                    let first_name = &names[0];
                    let first = hlls.get(first_name).ok_or_else(|| {
                        RedDBError::NotFound(format!("HLL '{}' not found", first_name))
                    })?;
                    let expected_precision = first.precision();
                    let mut merged =
                        crate::storage::primitives::hyperloglog::HyperLogLog::with_precision(
                            expected_precision,
                        )
                        .expect("loaded HLL precision is valid");
                    for name in names {
                        let hll = hlls.get(name).ok_or_else(|| {
                            RedDBError::NotFound(format!("HLL '{}' not found", name))
                        })?;
                        if hll.precision() != expected_precision {
                            return Err(hll_precision_mismatch_error(
                                "COUNT",
                                first_name,
                                expected_precision,
                                name,
                                hll.precision(),
                            ));
                        }
                        merged.merge(hll);
                    }
                    let count = merged.count();
                    let mut result = UnifiedResult::with_columns(vec!["count".into()]);
                    let mut record = UnifiedRecord::new();
                    record.set("count", Value::UnsignedInteger(count));
                    result.push(record);
                    Ok(RuntimeQueryResult {
                        query: raw_query.to_string(),
                        mode: QueryMode::Sql,
                        statement: "hll_count",
                        engine: "runtime-probabilistic",
                        result,
                        affected_rows: 0,
                        statement_type: "select",
                        bookmark: None,
                    })
                }
            }
            ProbabilisticCommand::HllMerge { dest, sources } => {
                let mut hlls =
                    probabilistic_write(&self.inner.probabilistic.hlls, "probabilistic HLL store");
                let first_src = sources.first().ok_or_else(|| {
                    RedDBError::Query("HLL MERGE requires at least one source HLL".to_string())
                })?;
                let first = hlls.get(first_src).ok_or_else(|| {
                    RedDBError::NotFound(format!("HLL '{}' not found", first_src))
                })?;
                let expected_precision = first.precision();
                let mut merged =
                    crate::storage::primitives::hyperloglog::HyperLogLog::with_precision(
                        expected_precision,
                    )
                    .expect("loaded HLL precision is valid");
                for src in sources {
                    let hll = hlls
                        .get(src)
                        .ok_or_else(|| RedDBError::NotFound(format!("HLL '{}' not found", src)))?;
                    if hll.precision() != expected_precision {
                        return Err(hll_precision_mismatch_error(
                            "MERGE",
                            first_src,
                            expected_precision,
                            src,
                            hll.precision(),
                        ));
                    }
                    merged.merge(hll);
                }
                self.persist_probabilistic_blob(PROB_HLL_STATE_PREFIX, dest, merged.as_bytes())?;
                self.append_probabilistic_snapshot_delta(
                    PROB_WAL_KIND_HLL,
                    dest,
                    merged.as_bytes().to_vec(),
                )?;
                hlls.insert(dest.clone(), merged);
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!(
                        "HLL '{}' created from merge of {}",
                        dest,
                        sources.join(", ")
                    ),
                    "create",
                ))
            }
            ProbabilisticCommand::HllInfo { name } => {
                let hlls =
                    probabilistic_read(&self.inner.probabilistic.hlls, "probabilistic HLL store");
                let hll = hlls
                    .get(name)
                    .ok_or_else(|| RedDBError::NotFound(format!("HLL '{}' not found", name)))?;
                let mut result = UnifiedResult::with_columns(vec![
                    "name".into(),
                    "precision".into(),
                    "count".into(),
                    "memory_bytes".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("name", Value::text(name.clone()));
                record.set("precision", Value::UnsignedInteger(hll.precision() as u64));
                record.set("count", Value::UnsignedInteger(hll.count()));
                record.set(
                    "memory_bytes",
                    Value::UnsignedInteger(hll.memory_bytes() as u64),
                );
                result.push(record);
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "hll_info",
                    engine: "runtime-probabilistic",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                })
            }
            ProbabilisticCommand::DropHll { name, if_exists } => {
                let mut hlls =
                    probabilistic_write(&self.inner.probabilistic.hlls, "probabilistic HLL store");
                if hlls.remove(name).is_none() {
                    if *if_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            raw_query.to_string(),
                            &format!("HLL '{}' does not exist", name),
                            "drop",
                        ));
                    }
                    return Err(RedDBError::NotFound(format!("HLL '{}' not found", name)));
                }
                self.drop_probabilistic_catalog_entry(name)?;
                self.delete_probabilistic_blob(PROB_HLL_STATE_PREFIX, name)?;
                self.append_probabilistic_drop_delta(PROB_WAL_KIND_HLL, name)?;
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("HLL '{}' dropped", name),
                    "drop",
                ))
            }

            // ── Count-Min Sketch ───────────────────────────────────────
            ProbabilisticCommand::CreateSketch {
                name,
                width,
                depth,
                if_not_exists,
            } => {
                let mut sketches = probabilistic_write(
                    &self.inner.probabilistic.sketches,
                    "probabilistic sketch store",
                );
                if sketches.contains_key(name) {
                    if *if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            raw_query.to_string(),
                            &format!("SKETCH '{}' already exists", name),
                            "create",
                        ));
                    }
                    return Err(RedDBError::Query(format!(
                        "SKETCH '{}' already exists",
                        name
                    )));
                }
                self.create_probabilistic_catalog_entry(
                    name,
                    crate::catalog::CollectionModel::Sketch,
                )?;
                let sketch = crate::storage::primitives::count_min_sketch::CountMinSketch::new(
                    *width, *depth,
                );
                self.persist_probabilistic_blob(
                    PROB_SKETCH_STATE_PREFIX,
                    name,
                    &sketch.as_bytes(),
                )?;
                self.append_probabilistic_snapshot_delta(
                    PROB_WAL_KIND_SKETCH,
                    name,
                    sketch.as_bytes(),
                )?;
                sketches.insert(name.clone(), sketch);
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!(
                        "SKETCH '{}' created (width={}, depth={})",
                        name, width, depth
                    ),
                    "create",
                ))
            }
            ProbabilisticCommand::SketchAdd {
                name,
                element,
                count,
            } => {
                let mut sketches = probabilistic_write(
                    &self.inner.probabilistic.sketches,
                    "probabilistic sketch store",
                );
                let sketch = sketches
                    .get_mut(name)
                    .ok_or_else(|| RedDBError::NotFound(format!("SKETCH '{}' not found", name)))?;
                sketch.add(element.as_bytes(), *count);
                self.append_probabilistic_add_delta(
                    PROB_WAL_KIND_SKETCH,
                    name,
                    vec![element.as_bytes().to_vec(), count.to_le_bytes().to_vec()],
                )?;
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("added {} to SKETCH '{}'", count, name),
                    "insert",
                ))
            }
            ProbabilisticCommand::SketchCount { name, element } => {
                let sketches = probabilistic_read(
                    &self.inner.probabilistic.sketches,
                    "probabilistic sketch store",
                );
                let sketch = sketches
                    .get(name)
                    .ok_or_else(|| RedDBError::NotFound(format!("SKETCH '{}' not found", name)))?;
                let estimate = sketch.estimate(element.as_bytes());
                let mut result = UnifiedResult::with_columns(vec!["estimate".into()]);
                let mut record = UnifiedRecord::new();
                record.set("estimate", Value::UnsignedInteger(estimate));
                result.push(record);
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "sketch_count",
                    engine: "runtime-probabilistic",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                })
            }
            ProbabilisticCommand::SketchMerge { dest, sources } => {
                let mut sketches = probabilistic_write(
                    &self.inner.probabilistic.sketches,
                    "probabilistic sketch store",
                );
                let first_src = sketches.get(&sources[0]).ok_or_else(|| {
                    RedDBError::NotFound(format!("SKETCH '{}' not found", sources[0]))
                })?;
                let mut merged = crate::storage::primitives::count_min_sketch::CountMinSketch::new(
                    first_src.width(),
                    first_src.depth(),
                );
                for src in sources {
                    let sketch = sketches.get(src).ok_or_else(|| {
                        RedDBError::NotFound(format!("SKETCH '{}' not found", src))
                    })?;
                    if !merged.merge(sketch) {
                        return Err(RedDBError::Query(format!(
                            "SKETCH '{}' has incompatible dimensions",
                            src
                        )));
                    }
                }
                self.persist_probabilistic_blob(
                    PROB_SKETCH_STATE_PREFIX,
                    dest,
                    &merged.as_bytes(),
                )?;
                self.append_probabilistic_snapshot_delta(
                    PROB_WAL_KIND_SKETCH,
                    dest,
                    merged.as_bytes(),
                )?;
                sketches.insert(dest.clone(), merged);
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!(
                        "SKETCH '{}' created from merge of {}",
                        dest,
                        sources.join(", ")
                    ),
                    "create",
                ))
            }
            ProbabilisticCommand::SketchInfo { name } => {
                let sketches = probabilistic_read(
                    &self.inner.probabilistic.sketches,
                    "probabilistic sketch store",
                );
                let sketch = sketches
                    .get(name)
                    .ok_or_else(|| RedDBError::NotFound(format!("SKETCH '{}' not found", name)))?;
                let mut result = UnifiedResult::with_columns(vec![
                    "name".into(),
                    "width".into(),
                    "depth".into(),
                    "total".into(),
                    "memory_bytes".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("name", Value::text(name.clone()));
                record.set("width", Value::UnsignedInteger(sketch.width() as u64));
                record.set("depth", Value::UnsignedInteger(sketch.depth() as u64));
                record.set("total", Value::UnsignedInteger(sketch.total()));
                record.set(
                    "memory_bytes",
                    Value::UnsignedInteger(sketch.memory_bytes() as u64),
                );
                result.push(record);
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "sketch_info",
                    engine: "runtime-probabilistic",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                })
            }
            ProbabilisticCommand::DropSketch { name, if_exists } => {
                let mut sketches = probabilistic_write(
                    &self.inner.probabilistic.sketches,
                    "probabilistic sketch store",
                );
                if sketches.remove(name).is_none() {
                    if *if_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            raw_query.to_string(),
                            &format!("SKETCH '{}' does not exist", name),
                            "drop",
                        ));
                    }
                    return Err(RedDBError::NotFound(format!("SKETCH '{}' not found", name)));
                }
                self.drop_probabilistic_catalog_entry(name)?;
                self.delete_probabilistic_blob(PROB_SKETCH_STATE_PREFIX, name)?;
                self.append_probabilistic_drop_delta(PROB_WAL_KIND_SKETCH, name)?;
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("SKETCH '{}' dropped", name),
                    "drop",
                ))
            }

            // ── Cuckoo Filter ─────────────────────────────────────────
            ProbabilisticCommand::CreateFilter {
                name,
                capacity,
                if_not_exists,
            } => {
                let mut filters = probabilistic_write(
                    &self.inner.probabilistic.filters,
                    "probabilistic filter store",
                );
                if filters.contains_key(name) {
                    if *if_not_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            raw_query.to_string(),
                            &format!("FILTER '{}' already exists", name),
                            "create",
                        ));
                    }
                    return Err(RedDBError::Query(format!(
                        "FILTER '{}' already exists",
                        name
                    )));
                }
                self.create_probabilistic_catalog_entry(
                    name,
                    crate::catalog::CollectionModel::Filter,
                )?;
                let filter =
                    crate::storage::primitives::cuckoo_filter::CuckooFilter::new(*capacity);
                self.persist_probabilistic_blob(
                    PROB_FILTER_STATE_PREFIX,
                    name,
                    &filter.as_bytes(),
                )?;
                self.append_probabilistic_snapshot_delta(
                    PROB_WAL_KIND_FILTER,
                    name,
                    filter.as_bytes(),
                )?;
                filters.insert(name.clone(), filter);
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("FILTER '{}' created (capacity={})", name, capacity),
                    "create",
                ))
            }
            ProbabilisticCommand::FilterAdd { name, element } => {
                let mut filters = probabilistic_write(
                    &self.inner.probabilistic.filters,
                    "probabilistic filter store",
                );
                let filter = filters
                    .get_mut(name)
                    .ok_or_else(|| RedDBError::NotFound(format!("FILTER '{}' not found", name)))?;
                if !filter.insert(element.as_bytes()) {
                    return Err(RedDBError::Query(format!("FILTER '{}' is full", name)));
                }
                self.append_probabilistic_add_delta(
                    PROB_WAL_KIND_FILTER,
                    name,
                    vec![element.as_bytes().to_vec()],
                )?;
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("element added to FILTER '{}'", name),
                    "insert",
                ))
            }
            ProbabilisticCommand::FilterCheck { name, element } => {
                let filters = probabilistic_read(
                    &self.inner.probabilistic.filters,
                    "probabilistic filter store",
                );
                let filter = filters
                    .get(name)
                    .ok_or_else(|| RedDBError::NotFound(format!("FILTER '{}' not found", name)))?;
                let exists = filter.contains(element.as_bytes());
                let mut result = UnifiedResult::with_columns(vec!["exists".into()]);
                let mut record = UnifiedRecord::new();
                record.set("exists", Value::Boolean(exists));
                result.push(record);
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "filter_check",
                    engine: "runtime-probabilistic",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                })
            }
            ProbabilisticCommand::FilterDelete { name, element } => {
                let mut filters = probabilistic_write(
                    &self.inner.probabilistic.filters,
                    "probabilistic filter store",
                );
                let filter = filters
                    .get_mut(name)
                    .ok_or_else(|| RedDBError::NotFound(format!("FILTER '{}' not found", name)))?;
                let removed = filter.delete(element.as_bytes());
                self.append_probabilistic_delete_delta(
                    PROB_WAL_KIND_FILTER,
                    name,
                    vec![element.as_bytes().to_vec()],
                )?;
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!(
                        "element {} from FILTER '{}'",
                        if removed { "deleted" } else { "not found in" },
                        name
                    ),
                    "delete",
                ))
            }
            ProbabilisticCommand::FilterCount { name } => {
                let filters = probabilistic_read(
                    &self.inner.probabilistic.filters,
                    "probabilistic filter store",
                );
                let filter = filters
                    .get(name)
                    .ok_or_else(|| RedDBError::NotFound(format!("FILTER '{}' not found", name)))?;
                let mut result = UnifiedResult::with_columns(vec!["count".into()]);
                let mut record = UnifiedRecord::new();
                record.set("count", Value::UnsignedInteger(filter.count() as u64));
                result.push(record);
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "filter_count",
                    engine: "runtime-probabilistic",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                })
            }
            ProbabilisticCommand::FilterInfo { name } => {
                let filters = probabilistic_read(
                    &self.inner.probabilistic.filters,
                    "probabilistic filter store",
                );
                let filter = filters
                    .get(name)
                    .ok_or_else(|| RedDBError::NotFound(format!("FILTER '{}' not found", name)))?;
                let mut result = UnifiedResult::with_columns(vec![
                    "name".into(),
                    "count".into(),
                    "load_factor".into(),
                    "memory_bytes".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("name", Value::text(name.clone()));
                record.set("count", Value::UnsignedInteger(filter.count() as u64));
                record.set("load_factor", Value::Float(filter.load_factor()));
                record.set(
                    "memory_bytes",
                    Value::UnsignedInteger(filter.memory_bytes() as u64),
                );
                result.push(record);
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "filter_info",
                    engine: "runtime-probabilistic",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                    bookmark: None,
                })
            }
            ProbabilisticCommand::DropFilter { name, if_exists } => {
                let mut filters = probabilistic_write(
                    &self.inner.probabilistic.filters,
                    "probabilistic filter store",
                );
                if filters.remove(name).is_none() {
                    if *if_exists {
                        return Ok(RuntimeQueryResult::ok_message(
                            raw_query.to_string(),
                            &format!("FILTER '{}' does not exist", name),
                            "drop",
                        ));
                    }
                    return Err(RedDBError::NotFound(format!("FILTER '{}' not found", name)));
                }
                self.drop_probabilistic_catalog_entry(name)?;
                self.delete_probabilistic_blob(PROB_FILTER_STATE_PREFIX, name)?;
                self.append_probabilistic_drop_delta(PROB_WAL_KIND_FILTER, name)?;
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("FILTER '{}' dropped", name),
                    "drop",
                ))
            }
        }
    }
}

fn parse_probabilistic_read_projection(
    projection: &Projection,
    index: usize,
) -> RedDBResult<Option<ProbabilisticReadProjection>> {
    if let Some(column) = projection_unqualified_column(projection) {
        if column.eq_ignore_ascii_case("CARDINALITY") {
            return Ok(Some(ProbabilisticReadProjection::Cardinality {
                label: probabilistic_projection_label(projection, "cardinality", index),
            }));
        }
    }

    let Some((function, args)) = projection_function(projection) else {
        return Ok(None);
    };
    if function.eq_ignore_ascii_case("FREQ") {
        let element = projection_single_text_arg(function, args)?;
        return Ok(Some(ProbabilisticReadProjection::Freq {
            element,
            label: probabilistic_projection_label(projection, "freq", index),
        }));
    }
    if function.eq_ignore_ascii_case("CONTAINS") {
        let element = projection_single_text_arg(function, args)?;
        return Ok(Some(ProbabilisticReadProjection::Contains {
            element,
            label: probabilistic_projection_label(projection, "contains", index),
        }));
    }

    Ok(None)
}

fn single_probabilistic_operand(mut operands: Vec<Vec<u8>>, context: &str) -> RedDBResult<Vec<u8>> {
    if operands.len() != 1 {
        return Err(RedDBError::Internal(format!(
            "{context} WAL delta expected one operand, got {}",
            operands.len()
        )));
    }
    Ok(operands.remove(0))
}

fn sketch_add_operands(mut operands: Vec<Vec<u8>>) -> RedDBResult<(Vec<u8>, u64)> {
    if operands.len() != 2 {
        return Err(RedDBError::Internal(format!(
            "SKETCH ADD WAL delta expected two operands, got {}",
            operands.len()
        )));
    }
    let count_bytes = operands.remove(1);
    let count_array: [u8; 8] = count_bytes.as_slice().try_into().map_err(|_| {
        RedDBError::Internal(format!(
            "SKETCH ADD WAL delta count expected 8 bytes, got {}",
            count_bytes.len()
        ))
    })?;
    Ok((operands.remove(0), u64::from_le_bytes(count_array)))
}

fn validate_probabilistic_read_model(
    collection: &str,
    actual_model: crate::catalog::CollectionModel,
    projections: &[ProbabilisticReadProjection],
) -> RedDBResult<()> {
    for projection in projections {
        let expected_model = match projection {
            ProbabilisticReadProjection::Cardinality { .. } => crate::catalog::CollectionModel::Hll,
            ProbabilisticReadProjection::Freq { .. } => crate::catalog::CollectionModel::Sketch,
            ProbabilisticReadProjection::Contains { .. } => crate::catalog::CollectionModel::Filter,
        };
        if actual_model != expected_model {
            return Err(RedDBError::Query(format!(
                "{} is only supported for {} collections; '{}' is {}",
                probabilistic_projection_form(projection),
                crate::runtime::ddl::polymorphic_resolver::model_name(expected_model),
                collection,
                crate::runtime::ddl::polymorphic_resolver::model_name(actual_model)
            )));
        }
    }
    Ok(())
}

impl RedDBRuntime {
    fn materialize_probabilistic_select_row(
        &self,
        collection: &str,
        projections: &[ProbabilisticReadProjection],
    ) -> RedDBResult<(Vec<String>, UnifiedRecord)> {
        let mut columns = Vec::with_capacity(projections.len());
        let mut record = UnifiedRecord::new();
        for projection in projections {
            match projection {
                ProbabilisticReadProjection::Cardinality { label } => {
                    let hlls = probabilistic_read(
                        &self.inner.probabilistic.hlls,
                        "probabilistic HLL store",
                    );
                    let hll = hlls.get(collection).ok_or_else(|| {
                        RedDBError::NotFound(format!("HLL '{}' not found", collection))
                    })?;
                    columns.push(label.clone());
                    record.set(label, Value::UnsignedInteger(hll.count()));
                }
                ProbabilisticReadProjection::Freq { element, label } => {
                    let sketches = probabilistic_read(
                        &self.inner.probabilistic.sketches,
                        "probabilistic sketch store",
                    );
                    let sketch = sketches.get(collection).ok_or_else(|| {
                        RedDBError::NotFound(format!("SKETCH '{}' not found", collection))
                    })?;
                    columns.push(label.clone());
                    record.set(
                        label,
                        Value::UnsignedInteger(sketch.estimate(element.as_bytes())),
                    );
                }
                ProbabilisticReadProjection::Contains { element, label } => {
                    let filters = probabilistic_read(
                        &self.inner.probabilistic.filters,
                        "probabilistic filter store",
                    );
                    let filter = filters.get(collection).ok_or_else(|| {
                        RedDBError::NotFound(format!("FILTER '{}' not found", collection))
                    })?;
                    columns.push(label.clone());
                    record.set(label, Value::Boolean(filter.contains(element.as_bytes())));
                }
            }
        }
        Ok((columns, record))
    }
}

fn probabilistic_select_row_visible(
    runtime: &RedDBRuntime,
    query: &TableQuery,
    record: &UnifiedRecord,
) -> bool {
    if query.limit == Some(0) || query.offset.is_some_and(|offset| offset > 0) {
        return false;
    }
    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);
    crate::storage::query::sql_lowering::effective_table_filter(query).is_none_or(|filter| {
        super::join_filter::evaluate_runtime_filter_with_db(
            Some(&runtime.inner.db),
            record,
            &filter,
            Some(table_name),
            Some(table_alias),
        )
    })
}

fn projection_unqualified_column(projection: &Projection) -> Option<&str> {
    match projection {
        Projection::Field(FieldRef::TableColumn { table, column }, _) if table.is_empty() => {
            Some(column.as_str())
        }
        Projection::Column(column) => Some(column.as_str()),
        Projection::Alias(column, _) => Some(column.as_str()),
        _ => None,
    }
}

fn projection_function(projection: &Projection) -> Option<(&str, &[Projection])> {
    match projection {
        Projection::Function(name, args) => {
            let function = name.split_once(':').map(|(name, _)| name).unwrap_or(name);
            Some((function, args.as_slice()))
        }
        _ => None,
    }
}

fn projection_single_text_arg(function: &str, args: &[Projection]) -> RedDBResult<String> {
    if args.len() != 1 {
        return Err(RedDBError::Query(format!(
            "{function}(...) expects exactly one string literal"
        )));
    }
    match &args[0] {
        Projection::Column(column) => column
            .strip_prefix("LIT:")
            .map(ToString::to_string)
            .ok_or_else(|| {
                RedDBError::Query(format!("{function}(...) expects a string literal argument"))
            }),
        _ => Err(RedDBError::Query(format!(
            "{function}(...) expects a string literal argument"
        ))),
    }
}

fn probabilistic_projection_label(projection: &Projection, base: &str, index: usize) -> String {
    match projection {
        Projection::Field(FieldRef::TableColumn { column, .. }, Some(alias))
            if alias.eq_ignore_ascii_case(column) =>
        {
            numbered_probabilistic_label(base, index)
        }
        Projection::Field(_, Some(alias)) => alias.clone(),
        Projection::Alias(column, alias) if column.eq_ignore_ascii_case(alias) => {
            numbered_probabilistic_label(base, index)
        }
        Projection::Alias(_, alias) => alias.clone(),
        Projection::Function(name, _) => name
            .split_once(':')
            .map(|(_, alias)| {
                if is_generated_probabilistic_function_label(alias, base) {
                    numbered_probabilistic_label(base, index)
                } else {
                    alias.to_string()
                }
            })
            .unwrap_or_else(|| numbered_probabilistic_label(base, index)),
        _ => numbered_probabilistic_label(base, index),
    }
}

fn is_generated_probabilistic_function_label(alias: &str, base: &str) -> bool {
    alias
        .get(..base.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(base))
        && alias[base.len()..].starts_with('(')
}

fn numbered_probabilistic_label(base: &str, index: usize) -> String {
    if index == 0 {
        base.to_string()
    } else {
        format!("{base}_{}", index + 1)
    }
}

fn probabilistic_projection_form(projection: &ProbabilisticReadProjection) -> &'static str {
    match projection {
        ProbabilisticReadProjection::Cardinality { .. } => "SELECT CARDINALITY",
        ProbabilisticReadProjection::Freq { .. } => "FREQ(...)",
        ProbabilisticReadProjection::Contains { .. } => "CONTAINS(...)",
    }
}
