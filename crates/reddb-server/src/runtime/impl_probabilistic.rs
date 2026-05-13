//! Execution of probabilistic data structure commands (HLL, SKETCH, FILTER)

use super::*;
use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};

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
        append_only: false,
        subscriptions: Vec::new(),
    }
}

enum ProbabilisticReadProjection {
    Cardinality { label: String },
    Freq { element: String, label: String },
    Contains { element: String, label: String },
}

impl RedDBRuntime {
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
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("{} element(s) added to HLL '{}'", elements.len(), name),
                    "insert",
                ))
            }
            ProbabilisticCommand::HllCount { names } => {
                let hlls =
                    probabilistic_read(&self.inner.probabilistic.hlls, "probabilistic HLL store");
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
                    })
                } else {
                    // Multi-HLL count = union count
                    let mut merged = crate::storage::primitives::hyperloglog::HyperLogLog::new();
                    for name in names {
                        let hll = hlls.get(name).ok_or_else(|| {
                            RedDBError::NotFound(format!("HLL '{}' not found", name))
                        })?;
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
                    })
                }
            }
            ProbabilisticCommand::HllMerge { dest, sources } => {
                let mut hlls =
                    probabilistic_write(&self.inner.probabilistic.hlls, "probabilistic HLL store");
                let mut merged = crate::storage::primitives::hyperloglog::HyperLogLog::new();
                for src in sources {
                    let hll = hlls
                        .get(src)
                        .ok_or_else(|| RedDBError::NotFound(format!("HLL '{}' not found", src)))?;
                    merged.merge(hll);
                }
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
                sketches.insert(
                    name.clone(),
                    crate::storage::primitives::count_min_sketch::CountMinSketch::new(
                        *width, *depth,
                    ),
                );
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
                filters.insert(
                    name.clone(),
                    crate::storage::primitives::cuckoo_filter::CuckooFilter::new(*capacity),
                );
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
        Projection::Field(_, Some(alias)) => alias.clone(),
        Projection::Alias(_, alias) => alias.clone(),
        Projection::Function(name, _) => name
            .split_once(':')
            .map(|(_, alias)| alias.to_string())
            .unwrap_or_else(|| numbered_probabilistic_label(base, index)),
        _ => numbered_probabilistic_label(base, index),
    }
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
