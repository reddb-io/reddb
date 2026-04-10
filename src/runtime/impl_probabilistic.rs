//! Execution of probabilistic data structure commands (HLL, SKETCH, FILTER)

use super::*;

impl RedDBRuntime {
    pub fn execute_probabilistic_command(
        &self,
        raw_query: &str,
        cmd: &ProbabilisticCommand,
    ) -> RedDBResult<RuntimeQueryResult> {
        match cmd {
            // ── HyperLogLog ──────────────────────────────────────────
            ProbabilisticCommand::CreateHll {
                name,
                if_not_exists,
            } => {
                let mut hlls = self.inner.probabilistic.hlls.write().unwrap();
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
                hlls.insert(
                    name.clone(),
                    crate::storage::primitives::hyperloglog::HyperLogLog::new(),
                );
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("HLL '{}' created", name),
                    "create",
                ))
            }
            ProbabilisticCommand::HllAdd { name, elements } => {
                let mut hlls = self.inner.probabilistic.hlls.write().unwrap();
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
                let hlls = self.inner.probabilistic.hlls.read().unwrap();
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
                let mut hlls = self.inner.probabilistic.hlls.write().unwrap();
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
                let hlls = self.inner.probabilistic.hlls.read().unwrap();
                let hll = hlls
                    .get(name)
                    .ok_or_else(|| RedDBError::NotFound(format!("HLL '{}' not found", name)))?;
                let mut result = UnifiedResult::with_columns(vec![
                    "name".into(),
                    "count".into(),
                    "memory_bytes".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("name", Value::Text(name.clone()));
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
                let mut hlls = self.inner.probabilistic.hlls.write().unwrap();
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
                let mut sketches = self.inner.probabilistic.sketches.write().unwrap();
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
                let mut sketches = self.inner.probabilistic.sketches.write().unwrap();
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
                let sketches = self.inner.probabilistic.sketches.read().unwrap();
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
                let mut sketches = self.inner.probabilistic.sketches.write().unwrap();
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
                let sketches = self.inner.probabilistic.sketches.read().unwrap();
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
                record.set("name", Value::Text(name.clone()));
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
                let mut sketches = self.inner.probabilistic.sketches.write().unwrap();
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
                let mut filters = self.inner.probabilistic.filters.write().unwrap();
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
                let mut filters = self.inner.probabilistic.filters.write().unwrap();
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
                let filters = self.inner.probabilistic.filters.read().unwrap();
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
                let mut filters = self.inner.probabilistic.filters.write().unwrap();
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
                let filters = self.inner.probabilistic.filters.read().unwrap();
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
                let filters = self.inner.probabilistic.filters.read().unwrap();
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
                record.set("name", Value::Text(name.clone()));
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
                let mut filters = self.inner.probabilistic.filters.write().unwrap();
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
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("FILTER '{}' dropped", name),
                    "drop",
                ))
            }
        }
    }
}
