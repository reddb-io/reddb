use super::*;

pub(super) fn execute_runtime_expr(db: &RedDB, expr: &QueryExpr) -> RedDBResult<UnifiedResult> {
    match expr {
        QueryExpr::Graph(_) | QueryExpr::Path(_) => {
            let graph = materialize_graph(db.store().as_ref())?;
            crate::storage::query::unified::UnifiedExecutor::execute_on(&graph, expr)
                .map_err(|err| RedDBError::Query(err.to_string()))
        }
        QueryExpr::Table(table) => execute_runtime_table_query(db, table),
        QueryExpr::Join(join) => execute_runtime_join_query(db, join),
        QueryExpr::Vector(vector) => execute_runtime_vector_query(db, vector),
        QueryExpr::Hybrid(hybrid) => execute_runtime_hybrid_query(db, hybrid),
        QueryExpr::Insert(_)
        | QueryExpr::Update(_)
        | QueryExpr::Delete(_)
        | QueryExpr::CreateTable(_)
        | QueryExpr::DropTable(_)
        | QueryExpr::AlterTable(_)
        | QueryExpr::GraphCommand(_)
        | QueryExpr::SearchCommand(_) => Err(RedDBError::Query(
            "DML/DDL/Command statements are not supported through this execution path".to_string(),
        )),
    }
}

pub(super) fn scan_runtime_table_records(
    db: &RedDB,
    query: &TableQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let mut records = scan_runtime_table_source_records(db, &query.table)?;
    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);

    if let Some(filter) = query.filter.as_ref() {
        records.retain(|record| {
            evaluate_runtime_filter(record, filter, Some(table_name), Some(table_alias))
        });
    }

    if !query.order_by.is_empty() {
        records.sort_by(|left, right| {
            compare_runtime_order(
                left,
                right,
                &query.order_by,
                Some(table_name),
                Some(table_alias),
            )
        });
    } else if is_universal_entity_source(table_name) {
        records.sort_by(|left, right| {
            runtime_record_identity_key(left).cmp(&runtime_record_identity_key(right))
        });
    }

    let offset = query.offset.unwrap_or(0) as usize;
    let limit = query.limit.map(|value| value as usize);
    let iter = records.into_iter().skip(offset);
    Ok(match limit {
        Some(limit) => iter.take(limit).collect(),
        None => iter.collect(),
    })
}

pub(super) fn scan_runtime_table_source_records(
    db: &RedDB,
    table: &str,
) -> RedDBResult<Vec<UnifiedRecord>> {
    if is_universal_entity_source(table) {
        return Ok(db
            .store()
            .query_all(|_| true)
            .into_iter()
            .filter_map(|(_, entity)| runtime_any_record_from_entity(entity))
            .collect());
    }

    let manager = db
        .store()
        .get_collection(table)
        .ok_or_else(|| RedDBError::NotFound(table.to_string()))?;

    Ok(manager
        .query_all(|_| true)
        .into_iter()
        .filter_map(runtime_table_record_from_entity)
        .collect())
}

pub(super) fn is_universal_entity_source(table: &str) -> bool {
    is_universal_query_source(table)
}

pub(super) fn runtime_table_record_from_entity(entity: UnifiedEntity) -> Option<UnifiedRecord> {
    let row = match entity.data {
        EntityData::Row(row) => row,
        _ => return None,
    };

    let mut record = UnifiedRecord::new();

    if let EntityKind::TableRow { row_id, .. } = &entity.kind {
        record.set("row_id", Value::UnsignedInteger(*row_id));
    }

    record.set("_entity_id", Value::UnsignedInteger(entity.id.raw()));
    record.set(
        "_collection",
        Value::Text(entity.kind.collection().to_string()),
    );
    record.set("_kind", Value::Text(entity.kind.storage_type().to_string()));
    record.set("_created_at", Value::UnsignedInteger(entity.created_at));
    record.set("_updated_at", Value::UnsignedInteger(entity.updated_at));
    record.set("_sequence_id", Value::UnsignedInteger(entity.sequence_id));
    set_runtime_entity_metadata(&mut record, "table", runtime_row_capabilities(&row));

    if let Some(named) = row.named {
        for (key, value) in named {
            record.set(&key, value);
        }
    } else {
        for (index, value) in row.columns.into_iter().enumerate() {
            record.set(&format!("c{index}"), value);
        }
    }

    Some(record)
}

pub(super) fn runtime_any_record_from_entity(entity: UnifiedEntity) -> Option<UnifiedRecord> {
    let identity_entity = entity.clone();
    let kind = entity.kind.clone();
    let collection = kind.collection().to_string();
    let storage_type = kind.storage_type().to_string();
    let entity_id = entity.id.raw();
    let created_at = entity.created_at;
    let updated_at = entity.updated_at;
    let sequence_id = entity.sequence_id;

    let (entity_type, capabilities, mut record) = match (kind, entity.data) {
        (EntityKind::TableRow { row_id, .. }, EntityData::Row(row)) => {
            let capabilities = runtime_row_capabilities(&row);
            let entity_type = runtime_row_entity_type(&row);
            let mut record = UnifiedRecord::new();
            record.set("row_id", Value::UnsignedInteger(row_id));
            if let Some(named) = row.named {
                for (key, value) in named {
                    record.set(&key, value);
                }
            } else {
                for (index, value) in row.columns.into_iter().enumerate() {
                    record.set(&format!("c{index}"), value);
                }
            }
            (entity_type, capabilities, record)
        }
        (EntityKind::GraphNode { label, node_type }, EntityData::Node(node)) => {
            let mut record = UnifiedRecord::new();
            record.set("id", Value::UnsignedInteger(entity_id));
            record.set("label", Value::Text(label));
            record.set("node_type", Value::Text(node_type));
            for (key, value) in node.properties {
                record.set(&key, value);
            }
            (
                "graph_node",
                runtime_record_capability_list(["graph", "graph_node"]),
                record,
            )
        }
        (
            EntityKind::GraphEdge {
                label,
                from_node,
                to_node,
                ..
            },
            EntityData::Edge(edge),
        ) => {
            let mut record = UnifiedRecord::new();
            record.set("label", Value::Text(label));
            record.set("from", Value::NodeRef(from_node));
            record.set("to", Value::NodeRef(to_node));
            record.set("weight", Value::Float(edge.weight as f64));
            for (key, value) in edge.properties {
                record.set(&key, value);
            }
            (
                "graph_edge",
                runtime_record_capability_list(["graph", "graph_edge"]),
                record,
            )
        }
        (EntityKind::Vector { .. }, EntityData::Vector(vector)) => {
            let mut record = UnifiedRecord::new();
            record.set(
                "dimension",
                Value::UnsignedInteger(vector.dense.len() as u64),
            );
            if let Some(content) = vector.content {
                record.set("content", Value::Text(content));
            }
            (
                "vector",
                runtime_record_capability_list(["vector", "similarity", "embedding"]),
                record,
            )
        }
        _ => return None,
    };

    record.set("_entity_id", Value::UnsignedInteger(entity_id));
    record.set("_collection", Value::Text(collection));
    record.set("_kind", Value::Text(storage_type));
    record.set("_created_at", Value::UnsignedInteger(created_at));
    record.set("_updated_at", Value::UnsignedInteger(updated_at));
    record.set("_sequence_id", Value::UnsignedInteger(sequence_id));
    set_runtime_entity_metadata(&mut record, entity_type, capabilities);
    apply_runtime_identity_hints(&mut record, &identity_entity);

    Some(record)
}

pub(super) fn set_runtime_entity_metadata(
    record: &mut UnifiedRecord,
    entity_type: &str,
    capabilities: BTreeSet<String>,
) {
    let capabilities_text = capabilities.into_iter().collect::<Vec<_>>().join(",");
    record.set("_entity_type", Value::Text(entity_type.to_string()));
    record.set("_capabilities", Value::Text(capabilities_text));
}

pub(super) fn runtime_record_capability_list<const N: usize>(
    values: [&str; N],
) -> BTreeSet<String> {
    values.into_iter().map(|value| value.to_string()).collect()
}

pub(super) fn runtime_row_capabilities(row: &crate::storage::RowData) -> BTreeSet<String> {
    let mut capabilities = runtime_record_capability_list(["table", "structured"]);
    if runtime_row_is_kv(row) {
        capabilities.insert("kv".to_string());
    }
    if runtime_row_has_document_capability(row) {
        capabilities.insert("document".to_string());
    }
    capabilities
}

pub(super) fn runtime_row_entity_type(row: &crate::storage::RowData) -> &'static str {
    if runtime_row_is_kv(row) {
        return "kv";
    }

    if runtime_row_has_document_capability(row) {
        "document"
    } else {
        "table"
    }
}

fn runtime_row_is_kv(row: &crate::storage::RowData) -> bool {
    let Some(named) = row.named.as_ref() else {
        return false;
    };

    if named.len() == 2 {
        named.contains_key("key") && named.contains_key("value")
    } else if named.len() == 1 {
        named.contains_key("key") || named.contains_key("value")
    } else {
        false
    }
}

pub(super) fn runtime_row_has_document_capability(row: &crate::storage::RowData) -> bool {
    row.named
        .as_ref()
        .map(|named| named.values().any(runtime_documentish_value))
        .unwrap_or(false)
        || row.columns.iter().any(runtime_documentish_value)
}

pub(super) fn runtime_documentish_value(value: &Value) -> bool {
    matches!(value, Value::Json(_) | Value::Blob(_))
}

pub(super) fn runtime_search_collections(
    db: &RedDB,
    collections: Option<Vec<String>>,
) -> Option<Vec<String>> {
    match collections {
        Some(collections) if !collections.is_empty() => Some(collections),
        _ => Some(db.store().list_collections()),
    }
}

pub(super) fn runtime_filter_dsl_result(
    result: &mut DslQueryResult,
    entity_types: Option<Vec<String>>,
    capabilities: Option<Vec<String>>,
) {
    let entity_types = entity_types
        .map(|items| {
            items
                .into_iter()
                .map(|item| item.trim().to_ascii_lowercase())
                .filter(|item| !item.is_empty())
                .collect::<BTreeSet<_>>()
        })
        .filter(|items| !items.is_empty());
    let capabilities = capabilities
        .map(|items| {
            items
                .into_iter()
                .map(|item| item.trim().to_ascii_lowercase())
                .filter(|item| !item.is_empty())
                .collect::<BTreeSet<_>>()
        })
        .filter(|items| !items.is_empty());

    if entity_types.is_none() && capabilities.is_none() {
        return;
    }

    result.matches.retain(|item| {
        let (entity_type, item_capabilities) = runtime_entity_type_and_capabilities(&item.entity);
        let type_ok = entity_types
            .as_ref()
            .is_none_or(|accepted| accepted.contains(entity_type));
        let capability_ok = capabilities.as_ref().is_none_or(|accepted| {
            item_capabilities
                .iter()
                .any(|capability| accepted.contains(capability))
        });
        type_ok && capability_ok
    });

    normalize_runtime_dsl_result_scores(result);
}

pub(super) fn normalize_runtime_dsl_result_scores(result: &mut DslQueryResult) {
    for item in &mut result.matches {
        if let Some(final_score) = item
            .components
            .final_score
            .filter(|score| score.is_finite())
        {
            item.score = final_score;
        } else {
            item.components.final_score = Some(item.score);
        }
    }

    result.matches.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.entity.id.raw().cmp(&right.entity.id.raw()))
    });
}

pub(super) fn runtime_entity_type_and_capabilities(
    entity: &UnifiedEntity,
) -> (&'static str, BTreeSet<String>) {
    match (&entity.kind, &entity.data) {
        (EntityKind::TableRow { .. }, EntityData::Row(row)) => {
            (runtime_row_entity_type(row), runtime_row_capabilities(row))
        }
        (EntityKind::GraphNode { .. }, EntityData::Node(_)) => (
            "graph_node",
            runtime_record_capability_list(["graph", "graph_node"]),
        ),
        (EntityKind::GraphEdge { .. }, EntityData::Edge(_)) => (
            "graph_edge",
            runtime_record_capability_list(["graph", "graph_edge"]),
        ),
        (EntityKind::Vector { .. }, EntityData::Vector(_)) => (
            "vector",
            runtime_record_capability_list(["vector", "similarity", "embedding"]),
        ),
        _ => ("unknown", BTreeSet::new()),
    }
}

pub(super) fn resolve_runtime_vector_source(
    db: &RedDB,
    source: &VectorSource,
) -> RedDBResult<Vec<f32>> {
    match source {
        VectorSource::Literal(vector) => Ok(vector.clone()),
        VectorSource::Reference {
            collection: _,
            vector_id,
        } => {
            let entity = db
                .get(EntityId::new(*vector_id))
                .ok_or_else(|| RedDBError::NotFound(format!("vector:{vector_id}")))?;
            match entity.data {
                EntityData::Vector(data) => Ok(data.dense),
                _ => Err(RedDBError::Query(format!(
                    "entity {vector_id} is not a vector source"
                ))),
            }
        }
        VectorSource::Text(_) => Err(RedDBError::Query(
            "text-to-embedding vector queries are parsed but not yet wired into /query".to_string(),
        )),
        VectorSource::Subquery(_) => Err(RedDBError::Query(
            "subquery vector sources are parsed but not yet wired into /query".to_string(),
        )),
    }
}

pub(super) fn runtime_vector_record_from_match(item: SimilarResult) -> UnifiedRecord {
    let mut record = UnifiedRecord::new();
    let (entity_type, capabilities) = runtime_entity_type_and_capabilities(&item.entity);
    record.set("entity_id", Value::UnsignedInteger(item.entity_id.raw()));
    record.set("_entity_id", Value::UnsignedInteger(item.entity_id.raw()));
    record.set("score", Value::Float(item.score as f64));
    record.set("_score", Value::Float(item.score as f64));
    record.set("final_score", Value::Float(item.score as f64));
    record.set("distance", Value::Float(item.distance as f64));
    record.set("_distance", Value::Float(item.distance as f64));
    record.set("vector_distance", Value::Float(item.distance as f64));
    record.set("vector_score", Value::Float(item.score as f64));
    record.set("vector_similarity", Value::Float(item.score as f64));
    record.set(
        "collection",
        Value::Text(item.entity.kind.collection().to_string()),
    );
    record.set(
        "_collection",
        Value::Text(item.entity.kind.collection().to_string()),
    );
    record.set(
        "_kind",
        Value::Text(item.entity.kind.storage_type().to_string()),
    );
    record.set(
        "_created_at",
        Value::UnsignedInteger(item.entity.created_at),
    );
    record.set(
        "_updated_at",
        Value::UnsignedInteger(item.entity.updated_at),
    );
    record.set(
        "_sequence_id",
        Value::UnsignedInteger(item.entity.sequence_id),
    );
    set_runtime_entity_metadata(&mut record, entity_type, capabilities);
    apply_runtime_identity_hints(&mut record, &item.entity);

    match item.entity.data {
        EntityData::Vector(data) => {
            record.set("dimension", Value::UnsignedInteger(data.dense.len() as u64));
            if let Some(content) = data.content {
                record.set("content", Value::Text(content));
            } else {
                record.set("content", Value::Null);
            }
        }
        EntityData::Row(row) => {
            record.set("dimension", Value::Null);
            if let Some(named) = row.named {
                for (key, value) in named {
                    record.set(&key, value);
                }
            }
        }
        EntityData::Node(node) => {
            record.set("dimension", Value::Null);
            for (key, value) in node.properties {
                record.set(&key, value);
            }
        }
        EntityData::Edge(edge) => {
            record.set("dimension", Value::Null);
            record.set("weight", Value::Float(edge.weight as f64));
            for (key, value) in edge.properties {
                record.set(&key, value);
            }
        }
    }

    record
}

pub(super) fn hybrid_candidate_keys(
    structured: &HashMap<String, UnifiedRecord>,
    vector: &HashMap<String, UnifiedRecord>,
    fusion: &FusionStrategy,
) -> Vec<String> {
    let structured_keys: BTreeSet<String> = structured.keys().cloned().collect();
    let vector_keys: BTreeSet<String> = vector.keys().cloned().collect();

    match fusion {
        FusionStrategy::Rerank { .. } => structured_keys.into_iter().collect(),
        FusionStrategy::FilterThenSearch
        | FusionStrategy::SearchThenFilter
        | FusionStrategy::Intersection => structured_keys
            .intersection(&vector_keys)
            .cloned()
            .collect(),
        FusionStrategy::Union { .. } | FusionStrategy::RRF { .. } => {
            structured_keys.union(&vector_keys).cloned().collect()
        }
    }
}

pub(super) fn runtime_record_identity_key(record: &UnifiedRecord) -> String {
    for key in [
        "_source_row",
        "_source_node",
        "_source_edge",
        "_source_entity",
        "_linked_identity",
    ] {
        if let Some(value) = record.values.get(key) {
            if let Some(fragment) = runtime_identity_fragment(value) {
                return format!("link:{fragment}");
            }
        }
    }

    if let Some(value) = record
        .values
        .get("entity_id")
        .or_else(|| record.values.get("_entity_id"))
    {
        if let Some(fragment) = runtime_identity_fragment(value) {
            return format!("entity:{fragment}");
        }
    }

    if let (Some(collection), Some(row_id)) = (
        record
            .values
            .get("_collection")
            .and_then(runtime_value_text),
        record
            .values
            .get("row_id")
            .or_else(|| record.values.get("id")),
    ) {
        if let Some(fragment) = runtime_identity_fragment(row_id) {
            return format!("row:{collection}:{fragment}");
        }
    }

    if let Some((alias, node)) = record.nodes.iter().next() {
        return format!("node:{alias}:{}", node.id);
    }

    if let Some(value) = record
        .values
        .iter()
        .find_map(|(key, value)| key.ends_with(".id").then_some(value))
    {
        if let Some(fragment) = runtime_identity_fragment(value) {
            return format!("ref:{fragment}");
        }
    }

    if let Some(value) = record.values.get("id") {
        if let Some(fragment) = runtime_identity_fragment(value) {
            return format!("id:{fragment}");
        }
    }

    if let Some(node) = record.paths.first().and_then(|path| path.nodes.first()) {
        return format!("path:{node}");
    }

    format!(
        "fingerprint:{:016x}",
        runtime_record_identity_fingerprint(record)
    )
}

fn runtime_record_identity_fingerprint(record: &UnifiedRecord) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mix = |hash: &mut u64, bytes: &[u8]| {
        for &byte in bytes {
            *hash ^= u64::from(byte);
            *hash = hash.wrapping_mul(0x100000001b3);
        }
    };

    let mut value_keys: Vec<_> = record.values.iter().collect();
    value_keys.sort_by(|left, right| left.0.cmp(right.0));
    for (key, value) in value_keys {
        mix(&mut hash, key.as_bytes());
        mix(&mut hash, b"\x00");
        let bytes = value.to_bytes();
        mix(&mut hash, &bytes);
        mix(&mut hash, b"|");
    }

    let mut nodes: Vec<_> = record.nodes.iter().collect();
    nodes.sort_by(|left, right| left.0.cmp(right.0));
    for (alias, node) in nodes {
        mix(&mut hash, alias.as_bytes());
        mix(&mut hash, b"\x1f");
        mix(&mut hash, node.id.as_bytes());
        mix(&mut hash, node.label.as_bytes());
        mix(&mut hash, format!("{:?}", node.node_type).as_bytes());
        mix(&mut hash, b"|");
    }

    let mut edges: Vec<_> = record.edges.iter().collect();
    edges.sort_by(|left, right| left.0.cmp(right.0));
    for (alias, edge) in edges {
        mix(&mut hash, alias.as_bytes());
        mix(&mut hash, b"\x1f");
        mix(&mut hash, edge.from.as_bytes());
        mix(&mut hash, b"->");
        mix(&mut hash, edge.to.as_bytes());
        mix(&mut hash, format!("{:?}", edge.edge_type).as_bytes());
        mix(&mut hash, b"::");
        mix(&mut hash, format!("{:.8}", edge.weight).as_bytes());
        mix(&mut hash, b"|");
    }

    let mut paths: Vec<_> = record.paths.iter().collect();
    paths.sort_by(|left, right| {
        let left_node = left.nodes.first().map(|node| node.as_str()).unwrap_or("");
        let right_node = right.nodes.first().map(|node| node.as_str()).unwrap_or("");
        left_node.cmp(right_node)
    });
    for path in paths {
        for node in &path.nodes {
            mix(&mut hash, node.as_bytes());
            mix(&mut hash, b",");
        }
        mix(&mut hash, b"|");
        for edge in &path.edges {
            mix(&mut hash, edge.from.as_bytes());
            mix(&mut hash, b"->");
            mix(&mut hash, edge.to.as_bytes());
            mix(&mut hash, b"::");
            mix(&mut hash, format!("{:?}", edge.edge_type).as_bytes());
            mix(&mut hash, b":");
            mix(&mut hash, format!("{:.8}", edge.weight).as_bytes());
            mix(&mut hash, b",");
        }
        mix(&mut hash, b"|");
    }

    let mut vector_results: Vec<_> = record.vector_results.iter().collect();
    vector_results.sort_by(|left, right| {
        (left.collection.as_str(), left.id).cmp(&(right.collection.as_str(), right.id))
    });
    for result in vector_results {
        mix(&mut hash, result.collection.as_bytes());
        mix(&mut hash, b"#");
        mix(&mut hash, result.id.to_string().as_bytes());
        mix(&mut hash, b"::");
        mix(&mut hash, format!("{:.8}", result.distance).as_bytes());
    }

    hash
}

pub(super) fn runtime_identity_fragment(value: &Value) -> Option<String> {
    match value {
        Value::Integer(value) => Some(value.to_string()),
        Value::UnsignedInteger(value) => Some(value.to_string()),
        Value::Float(value) => Some(value.to_string()),
        Value::Text(value) => Some(value.clone()),
        Value::NodeRef(value) => Some(value.clone()),
        Value::EdgeRef(value) => Some(value.clone()),
        Value::RowRef(table, row_id) => Some(format!("{table}:{row_id}")),
        Value::VectorRef(collection, vector_id) => Some(format!("{collection}:{vector_id}")),
        _ => runtime_value_text(value),
    }
}

pub(super) fn apply_runtime_identity_hints(record: &mut UnifiedRecord, entity: &UnifiedEntity) {
    for cross_ref in &entity.cross_refs {
        let value = match cross_ref.ref_type {
            RefType::VectorToRow | RefType::NodeToRow => Some(Value::RowRef(
                cross_ref.target_collection.clone(),
                cross_ref.target.raw(),
            )),
            RefType::VectorToNode | RefType::RowToNode => Some(Value::NodeRef(format!(
                "{}:{}",
                cross_ref.target_collection, cross_ref.target
            ))),
            RefType::RowToEdge | RefType::EdgeToVector => Some(Value::EdgeRef(format!(
                "{}:{}",
                cross_ref.target_collection, cross_ref.target
            ))),
            _ => Some(Value::Text(format!(
                "{}:{}",
                cross_ref.target_collection, cross_ref.target
            ))),
        };

        if let Some(value) = value {
            match cross_ref.ref_type {
                RefType::VectorToRow | RefType::NodeToRow => {
                    record
                        .values
                        .insert("_source_row".to_string(), value.clone());
                    record
                        .values
                        .entry("_linked_identity".to_string())
                        .or_insert(value);
                }
                RefType::VectorToNode | RefType::RowToNode => {
                    record
                        .values
                        .insert("_source_node".to_string(), value.clone());
                    record
                        .values
                        .entry("_linked_identity".to_string())
                        .or_insert(value);
                }
                RefType::RowToEdge | RefType::EdgeToVector => {
                    record
                        .values
                        .insert("_source_edge".to_string(), value.clone());
                    record
                        .values
                        .entry("_linked_identity".to_string())
                        .or_insert(value);
                }
                _ => {
                    record
                        .values
                        .entry("_source_entity".to_string())
                        .or_insert(value.clone());
                    record
                        .values
                        .entry("_linked_identity".to_string())
                        .or_insert(value);
                }
            }
        }
    }
}

pub(super) fn runtime_vector_entity_matches_filter(
    db: &RedDB,
    collection: &str,
    entity: &UnifiedEntity,
    filter: &VectorMetadataFilter,
) -> bool {
    let metadata = db
        .store()
        .get_metadata(collection, entity.id)
        .unwrap_or_default();
    let entry = runtime_metadata_entry(&metadata);
    filter.matches(&entry)
}

pub(super) fn runtime_metadata_entry(metadata: &Metadata) -> MetadataEntry {
    let mut entry = MetadataEntry::new();
    for (key, value) in metadata.iter() {
        if let Some(converted) = runtime_vector_metadata_value(value) {
            entry.insert(key.clone(), converted);
        }
    }
    entry
}

pub(super) fn runtime_vector_metadata_value(
    value: &UnifiedMetadataValue,
) -> Option<VectorMetadataValue> {
    match value {
        UnifiedMetadataValue::Null => Some(VectorMetadataValue::Null),
        UnifiedMetadataValue::Bool(value) => Some(VectorMetadataValue::Bool(*value)),
        UnifiedMetadataValue::Int(value) => Some(VectorMetadataValue::Integer(*value)),
        UnifiedMetadataValue::Float(value) => Some(VectorMetadataValue::Float(*value)),
        UnifiedMetadataValue::String(value) => Some(VectorMetadataValue::String(value.clone())),
        UnifiedMetadataValue::Timestamp(value) => Some(VectorMetadataValue::Integer(*value as i64)),
        UnifiedMetadataValue::Reference(target) => Some(VectorMetadataValue::String(
            runtime_ref_target_string(target),
        )),
        UnifiedMetadataValue::References(targets) => Some(VectorMetadataValue::String(
            targets
                .iter()
                .map(runtime_ref_target_string)
                .collect::<Vec<_>>()
                .join(","),
        )),
        UnifiedMetadataValue::Array(values) => Some(VectorMetadataValue::String(
            values
                .iter()
                .filter_map(runtime_vector_metadata_value)
                .map(|value| match value {
                    VectorMetadataValue::String(value) => value,
                    VectorMetadataValue::Integer(value) => value.to_string(),
                    VectorMetadataValue::Float(value) => value.to_string(),
                    VectorMetadataValue::Bool(value) => value.to_string(),
                    VectorMetadataValue::Null => "null".to_string(),
                })
                .collect::<Vec<_>>()
                .join(","),
        )),
        UnifiedMetadataValue::Object(_)
        | UnifiedMetadataValue::Bytes(_)
        | UnifiedMetadataValue::Geo { .. } => None,
    }
}

pub(super) fn runtime_ref_target_string(target: &RefTarget) -> String {
    match target {
        RefTarget::TableRow { table, row_id } => format!("{table}:{row_id}"),
        RefTarget::Node {
            collection,
            node_id,
        } => format!("{collection}:{node_id}"),
        RefTarget::Edge {
            collection,
            edge_id,
        } => format!("{collection}:{edge_id}"),
        RefTarget::Vector {
            collection,
            vector_id,
        } => format!("{collection}:{vector_id}"),
        RefTarget::Entity {
            collection,
            entity_id,
        } => format!("{collection}:{entity_id}"),
    }
}

pub(super) fn runtime_entity_vector_similarity(entity: &UnifiedEntity, query: &[f32]) -> f32 {
    let mut best_similarity = 0.0f32;

    for emb in &entity.embeddings {
        best_similarity = best_similarity.max(cosine_similarity(query, &emb.vector));
    }

    if let EntityData::Vector(vec_data) = &entity.data {
        best_similarity = best_similarity.max(cosine_similarity(query, &vec_data.dense));
    }

    best_similarity
}

pub(super) fn runtime_structured_score(record: &UnifiedRecord, rank: Option<usize>) -> f64 {
    if let Some(value) = record
        .values
        .get("_score")
        .or_else(|| record.values.get("final_score"))
        .or_else(|| record.values.get("score"))
        .or_else(|| record.values.get("hybrid_score"))
        .or_else(|| record.values.get("graph_score"))
        .or_else(|| record.values.get("table_score"))
        .or_else(|| record.values.get("graph_match"))
        .or_else(|| record.values.get("vector_similarity"))
        .or_else(|| record.values.get("structured_match"))
        .or_else(|| record.values.get("text_relevance"))
    {
        if let Some(number) = runtime_value_number(value) {
            return number;
        }
    }

    rank.map(|value| 1.0 / (value as f64 + 1.0)).unwrap_or(0.0)
}

pub(super) fn runtime_vector_score(record: &UnifiedRecord) -> f64 {
    record
        .values
        .get("_score")
        .or_else(|| record.values.get("final_score"))
        .or_else(|| record.values.get("score"))
        .or_else(|| record.values.get("vector_similarity"))
        .or_else(|| record.values.get("graph_score"))
        .or_else(|| record.values.get("table_score"))
        .and_then(runtime_value_number)
        .unwrap_or(0.0)
}

pub(super) fn merge_hybrid_records(
    structured: Option<&UnifiedRecord>,
    vector: Option<&UnifiedRecord>,
) -> UnifiedRecord {
    let mut merged = structured.cloned().unwrap_or_default();

    if let Some(vector_record) = vector {
        for (key, value) in &vector_record.values {
            if let Some(existing) = merged.values.get(key) {
                if existing != value {
                    merged.values.insert(format!("vector.{key}"), value.clone());
                }
            } else {
                merged.values.insert(key.clone(), value.clone());
            }
        }

        for (alias, node) in &vector_record.nodes {
            merged
                .nodes
                .entry(alias.clone())
                .or_insert_with(|| node.clone());
        }
        for (alias, edge) in &vector_record.edges {
            merged
                .edges
                .entry(alias.clone())
                .or_insert_with(|| edge.clone());
        }
        merged.paths.extend(vector_record.paths.clone());
        merged
            .vector_results
            .extend(vector_record.vector_results.clone());
    }

    merged
}

pub(super) fn merge_join_records(
    left: Option<&UnifiedRecord>,
    right: Option<&UnifiedRecord>,
    left_query: &TableQuery,
    right_prefix: Option<&str>,
) -> UnifiedRecord {
    let left_table_name = left_query.table.as_str();
    let left_table_alias = left_query.alias.as_deref().unwrap_or(left_table_name);
    let mut merged = UnifiedRecord::new();

    if let Some(left_record) = left {
        merged = project_runtime_record(
            left_record,
            &left_query.columns,
            Some(left_table_name),
            Some(left_table_alias),
            false,
            false,
        );
    }

    if let Some(right_record) = right {
        for (key, value) in &right_record.values {
            if merged.values.contains_key(key) {
                if let Some(prefix) = right_prefix {
                    merged
                        .values
                        .insert(format!("{prefix}.{key}"), value.clone());
                }
            } else {
                merged.values.insert(key.clone(), value.clone());
            }
        }

        for (alias, node) in &right_record.nodes {
            merged.nodes.insert(alias.clone(), node.clone());
        }
        for (alias, edge) in &right_record.edges {
            merged.edges.insert(alias.clone(), edge.clone());
        }
        merged.paths.extend(right_record.paths.clone());
        merged
            .vector_results
            .extend(right_record.vector_results.clone());
    }

    merged
}

pub(super) fn join_condition_matches(
    left_record: &UnifiedRecord,
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    left_field: &FieldRef,
    right_record: &UnifiedRecord,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
    right_field: &FieldRef,
) -> bool {
    let left_value =
        resolve_runtime_field(left_record, left_field, left_table_name, left_table_alias);
    let right_value = resolve_runtime_field(
        right_record,
        right_field,
        right_table_name,
        right_table_alias,
    );

    match (left_value.as_ref(), right_value.as_ref()) {
        (Some(left), Some(right)) => compare_runtime_values(left, right, CompareOp::Eq),
        _ => false,
    }
}

pub(super) fn canonical_join_type(
    node: &crate::storage::query::planner::CanonicalLogicalNode,
) -> RedDBResult<JoinType> {
    match node.details.get("join_type").map(String::as_str) {
        Some("inner") => Ok(JoinType::Inner),
        Some("left_outer") => Ok(JoinType::LeftOuter),
        Some("right_outer") => Ok(JoinType::RightOuter),
        Some(other) => Err(RedDBError::Query(format!(
            "unsupported canonical join type {other}"
        ))),
        None => Err(RedDBError::Query(
            "canonical join operator is missing join_type".to_string(),
        )),
    }
}

pub(super) fn canonical_join_field(
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    key: &str,
) -> RedDBResult<FieldRef> {
    let value = node
        .details
        .get(key)
        .ok_or_else(|| RedDBError::Query(format!("canonical join operator is missing {key}")))?;
    parse_canonical_field_ref(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CanonicalJoinStrategy {
    IndexedNestedLoop,
    GraphLookupJoin,
    NestedLoop,
}

pub(super) fn canonical_join_strategy(
    node: &crate::storage::query::planner::CanonicalLogicalNode,
) -> RedDBResult<CanonicalJoinStrategy> {
    match node.details.get("join_strategy").map(String::as_str) {
        Some("indexed_nested_loop") => Ok(CanonicalJoinStrategy::IndexedNestedLoop),
        Some("graph_lookup_join") => Ok(CanonicalJoinStrategy::GraphLookupJoin),
        Some("nested_loop") => Ok(CanonicalJoinStrategy::NestedLoop),
        Some(other) => Err(RedDBError::Query(format!(
            "unsupported canonical join strategy {other}"
        ))),
        None => Err(RedDBError::Query(
            "canonical join operator is missing join_strategy".to_string(),
        )),
    }
}
