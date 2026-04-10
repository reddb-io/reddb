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

            // ── Count-Min Sketch (stubs for Phase 7) ─────────────────
            ProbabilisticCommand::CreateSketch { name, .. } => Ok(RuntimeQueryResult::ok_message(
                raw_query.to_string(),
                &format!("SKETCH '{}' created", name),
                "create",
            )),
            ProbabilisticCommand::SketchAdd { name, .. } => Ok(RuntimeQueryResult::ok_message(
                raw_query.to_string(),
                &format!("element added to SKETCH '{}'", name),
                "insert",
            )),
            ProbabilisticCommand::SketchCount { name, element } => {
                let mut result = UnifiedResult::with_columns(vec!["estimate".into()]);
                let mut record = UnifiedRecord::new();
                record.set("estimate", Value::UnsignedInteger(0));
                result.push(record);
                let _ = (name, element);
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
            ProbabilisticCommand::SketchMerge { dest, .. } => Ok(RuntimeQueryResult::ok_message(
                raw_query.to_string(),
                &format!("SKETCH '{}' merged", dest),
                "create",
            )),
            ProbabilisticCommand::SketchInfo { name } => {
                let mut result = UnifiedResult::with_columns(vec!["name".into()]);
                let mut record = UnifiedRecord::new();
                record.set("name", Value::Text(name.clone()));
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
            ProbabilisticCommand::DropSketch { name, .. } => Ok(RuntimeQueryResult::ok_message(
                raw_query.to_string(),
                &format!("SKETCH '{}' dropped", name),
                "drop",
            )),

            // ── Cuckoo Filter (stubs for Phase 8) ────────────────────
            ProbabilisticCommand::CreateFilter { name, .. } => Ok(RuntimeQueryResult::ok_message(
                raw_query.to_string(),
                &format!("FILTER '{}' created", name),
                "create",
            )),
            ProbabilisticCommand::FilterAdd { name, .. } => Ok(RuntimeQueryResult::ok_message(
                raw_query.to_string(),
                &format!("element added to FILTER '{}'", name),
                "insert",
            )),
            ProbabilisticCommand::FilterCheck { name, element } => {
                let mut result = UnifiedResult::with_columns(vec!["exists".into()]);
                let mut record = UnifiedRecord::new();
                record.set("exists", Value::Boolean(false));
                result.push(record);
                let _ = (name, element);
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
            ProbabilisticCommand::FilterDelete { name, .. } => Ok(RuntimeQueryResult::ok_message(
                raw_query.to_string(),
                &format!("element deleted from FILTER '{}'", name),
                "delete",
            )),
            ProbabilisticCommand::FilterCount { name } => {
                let mut result = UnifiedResult::with_columns(vec!["count".into()]);
                let mut record = UnifiedRecord::new();
                record.set("count", Value::UnsignedInteger(0));
                result.push(record);
                let _ = name;
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
                let mut result = UnifiedResult::with_columns(vec!["name".into()]);
                let mut record = UnifiedRecord::new();
                record.set("name", Value::Text(name.clone()));
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
            ProbabilisticCommand::DropFilter { name, .. } => Ok(RuntimeQueryResult::ok_message(
                raw_query.to_string(),
                &format!("FILTER '{}' dropped", name),
                "drop",
            )),
        }
    }
}
