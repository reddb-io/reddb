//! Canonical logical entity adapter for native store versions V1 through V12.
//! Physical record framing and value bytes remain owned by `reddb-file`.
use super::entity::*;
use reddb_file::native_store::{
    write_native_varu32 as write_varu32, write_native_varu64 as write_varu64,
};
use reddb_file::{
    embedded::{RdbFileError, RdbFileResult},
    native_store::*,
    native_value::{write_native_value, NativeReader},
};
use std::collections::HashMap;

/// Write entity to binary buffer
pub fn write_entity_binary(buf: &mut Vec<u8>, entity: &UnifiedEntity, format_version: u32) {
    // Entity ID
    write_varu64(buf, entity.id.raw());

    // EntityKind
    match &entity.kind {
        EntityKind::TableRow { table, row_id } => {
            buf.push(0);
            write_varu32(buf, table.len() as u32);
            buf.extend_from_slice(table.as_bytes());
            write_varu64(buf, *row_id);
        }
        EntityKind::GraphNode(ref node) => {
            buf.push(1);
            write_varu32(buf, node.label.len() as u32);
            buf.extend_from_slice(node.label.as_bytes());
            write_varu32(buf, node.node_type.len() as u32);
            buf.extend_from_slice(node.node_type.as_bytes());
        }
        EntityKind::GraphEdge(ref edge) => {
            buf.push(2);
            write_varu32(buf, edge.label.len() as u32);
            buf.extend_from_slice(edge.label.as_bytes());
            write_varu32(buf, edge.from_node.len() as u32);
            buf.extend_from_slice(edge.from_node.as_bytes());
            write_varu32(buf, edge.to_node.len() as u32);
            buf.extend_from_slice(edge.to_node.as_bytes());
            buf.extend_from_slice(&edge.weight.to_le_bytes());
        }
        EntityKind::Vector { collection } => {
            buf.push(3);
            write_varu32(buf, collection.len() as u32);
            buf.extend_from_slice(collection.as_bytes());
        }
        EntityKind::TimeSeriesPoint(ref ts) => {
            buf.push(4);
            write_varu32(buf, ts.series.len() as u32);
            buf.extend_from_slice(ts.series.as_bytes());
            write_varu32(buf, ts.metric.len() as u32);
            buf.extend_from_slice(ts.metric.as_bytes());
        }
        EntityKind::QueueMessage { queue, position } => {
            buf.push(5);
            write_varu32(buf, queue.len() as u32);
            buf.extend_from_slice(queue.as_bytes());
            write_varu64(buf, *position);
        }
    }

    // EntityData
    match &entity.data {
        EntityData::Row(row) => {
            if row.named.is_some()
                || row
                    .schema
                    .as_ref()
                    .is_some_and(|schema| schema.len() == row.columns.len())
            {
                // Both resident representations encode the same named record.
                // Compaction must not lose field names on the next checkpoint.
                buf.push(6);
                let mut entries: Vec<_> = if let Some(named) = &row.named {
                    named
                        .iter()
                        .map(|(name, value)| (name.as_str(), value))
                        .collect()
                } else {
                    row.schema
                        .as_ref()
                        .expect("validated schema")
                        .iter()
                        .map(String::as_str)
                        .zip(&row.columns)
                        .collect()
                };
                write_varu32(buf, entries.len() as u32);
                entries.sort_by(|a, b| a.0.cmp(b.0));
                for (key, value) in entries {
                    write_varu32(buf, key.len() as u32);
                    buf.extend_from_slice(key.as_bytes());
                    write_native_value(buf, value);
                }
            } else {
                buf.push(0);
                write_varu32(buf, row.columns.len() as u32);
                for col in &row.columns {
                    write_native_value(buf, col);
                }
            }
        }
        EntityData::Node(node) => {
            buf.push(1);
            write_varu32(buf, node.properties.len() as u32);
            let mut entries: Vec<_> = node.properties.iter().collect();
            entries.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
            for (key, value) in entries {
                write_varu32(buf, key.len() as u32);
                buf.extend_from_slice(key.as_bytes());
                write_native_value(buf, value);
            }
        }
        EntityData::Edge(edge) => {
            buf.push(2);
            buf.extend_from_slice(&edge.weight.to_le_bytes());
            write_varu32(buf, edge.properties.len() as u32);
            let mut entries: Vec<_> = edge.properties.iter().collect();
            entries.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
            for (key, value) in entries {
                write_varu32(buf, key.len() as u32);
                buf.extend_from_slice(key.as_bytes());
                write_native_value(buf, value);
            }
        }
        EntityData::Vector(vec) => {
            buf.push(3);
            write_varu32(buf, vec.dense.len() as u32);
            for f in &vec.dense {
                buf.extend_from_slice(&f.to_le_bytes());
            }
            if format_version >= STORE_VERSION_V6 {
                buf.push(u8::from(vec.content.is_some()));
                if let Some(content) = &vec.content {
                    write_varu32(buf, content.len() as u32);
                    buf.extend_from_slice(content.as_bytes());
                }
            }
        }
        EntityData::TimeSeries(ts) => {
            buf.push(4);
            write_varu32(buf, ts.metric.len() as u32);
            buf.extend_from_slice(ts.metric.as_bytes());
            write_varu64(buf, ts.timestamp_ns);
            buf.extend_from_slice(&ts.value.to_le_bytes());
            if format_version >= STORE_VERSION_V11 {
                // Discriminator: interned series id (1) vs inline tag map (0).
                // Interned points (dictionary path) persist just the id;
                // un-interned points (metrics remote-write, rollups) keep
                // their tags inline so the metrics read paths, which build
                // labels directly from `tags`, survive a reopen.
                match ts.series_id {
                    Some(series_id) => {
                        write_varu32(buf, 1);
                        write_varu64(buf, series_id);
                    }
                    None => {
                        write_varu32(buf, 0);
                        write_varu32(buf, ts.tags.len() as u32);
                        let mut tag_entries: Vec<_> = ts.tags.iter().collect();
                        tag_entries.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
                        for (key, value) in tag_entries {
                            write_varu32(buf, key.len() as u32);
                            buf.extend_from_slice(key.as_bytes());
                            write_varu32(buf, value.len() as u32);
                            buf.extend_from_slice(value.as_bytes());
                        }
                    }
                }
            } else if format_version >= STORE_VERSION_V5 {
                write_varu32(buf, ts.tags.len() as u32);
                let mut tag_entries: Vec<_> = ts.tags.iter().collect();
                tag_entries.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
                for (key, value) in tag_entries {
                    write_varu32(buf, key.len() as u32);
                    buf.extend_from_slice(key.as_bytes());
                    write_varu32(buf, value.len() as u32);
                    buf.extend_from_slice(value.as_bytes());
                }
            }
            if format_version >= STORE_VERSION_V12 {
                write_varu32(buf, ts.fields.len() as u32);
                let mut field_entries: Vec<_> = ts.fields.iter().collect();
                field_entries.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
                for (key, value) in field_entries {
                    write_varu32(buf, key.len() as u32);
                    buf.extend_from_slice(key.as_bytes());
                    write_native_value(buf, value);
                }
            }
        }
        EntityData::QueueMessage(msg) => {
            buf.push(5);
            write_native_value(buf, &msg.payload);
            write_varu64(buf, msg.enqueued_at_ns);
            write_varu32(buf, msg.attempts);
        }
    }

    // Timestamps
    write_varu64(buf, entity.created_at);
    write_varu64(buf, entity.updated_at);

    // Embeddings
    write_varu32(buf, entity.embeddings().len() as u32);
    for emb in entity.embeddings() {
        write_varu32(buf, emb.name.len() as u32);
        buf.extend_from_slice(emb.name.as_bytes());
        write_varu32(buf, emb.vector.len() as u32);
        for f in &emb.vector {
            buf.extend_from_slice(&f.to_le_bytes());
        }
        write_varu32(buf, emb.model.len() as u32);
        buf.extend_from_slice(emb.model.as_bytes());
    }

    // Cross-refs
    write_varu32(buf, entity.cross_refs().len() as u32);
    for cross_ref in entity.cross_refs() {
        write_varu64(buf, cross_ref.source.raw());
        write_varu64(buf, cross_ref.target.raw());
        buf.push(cross_ref.ref_type.to_byte());
        if format_version >= STORE_VERSION_V2 {
            write_varu32(buf, cross_ref.target_collection.len() as u32);
            buf.extend_from_slice(cross_ref.target_collection.as_bytes());
            buf.extend_from_slice(&cross_ref.weight.to_le_bytes());
            write_varu64(buf, cross_ref.created_at);
        }
    }

    // Sequence ID
    write_varu64(buf, entity.sequence_id);

    if format_version >= STORE_VERSION_V4 {
        if let EntityData::QueueMessage(message) = &entity.data {
            buf.push(u8::from(message.priority.is_some()));
            if let Some(priority) = message.priority {
                buf.extend_from_slice(&priority.to_le_bytes());
            }
            write_varu32(buf, message.max_attempts);
            buf.push(u8::from(message.acked));
        }
    }

    if format_version >= STORE_VERSION_V8 {
        buf.push(u8::from(entity.has_explicit_logical_id()));
        if entity.has_explicit_logical_id() {
            write_varu64(buf, entity.logical_id().raw());
        }
    }
    if format_version >= STORE_VERSION_V9 {
        write_varu64(buf, entity.xmin);
        write_varu64(buf, entity.xmax);
    }
}

/// Decode one entity, retaining legacy optional-tail compatibility. All reads
/// are checked before indexing or allocating from persisted lengths.
pub fn read_entity_binary(
    bytes: &[u8],
    position: &mut usize,
    format_version: u32,
) -> RdbFileResult<UnifiedEntity> {
    let mut reader = NativeReader::new(bytes, position);
    let id = EntityId::new(reader.variable_u64()?);
    let kind = match reader.byte()? {
        0 => EntityKind::TableRow {
            table: reader.string()?.into(),
            row_id: reader.variable_u64()?,
        },
        1 => EntityKind::GraphNode(Box::new(GraphNodeKind {
            label: reader.string()?,
            node_type: reader.string()?,
        })),
        2 => EntityKind::GraphEdge(Box::new(GraphEdgeKind {
            label: reader.string()?,
            from_node: reader.string()?,
            to_node: reader.string()?,
            weight: u32::from_le_bytes(reader.fixed()?),
        })),
        3 => EntityKind::Vector {
            collection: reader.string()?,
        },
        4 => EntityKind::TimeSeriesPoint(Box::new(TimeSeriesPointKind {
            series: reader.string()?,
            metric: reader.string()?,
        })),
        5 => EntityKind::QueueMessage {
            queue: reader.string()?,
            position: reader.variable_u64()?,
        },
        tag => return Err(invalid(format!("Unknown EntityKind type: {tag}"))),
    };
    let mut data = match reader.byte()? {
        0 => {
            let count = reader.count(1)?;
            let mut columns = Vec::with_capacity(count);
            for _ in 0..count {
                columns.push(reader.value()?);
            }
            EntityData::Row(RowData::new(columns))
        }
        1 => EntityData::Node(NodeData::with_properties(read_properties(&mut reader)?)),
        2 => {
            let mut edge = EdgeData::new(f32::from_le_bytes(reader.fixed()?));
            edge.properties = read_properties(&mut reader)?;
            EntityData::Edge(edge)
        }
        3 => {
            let mut vector = VectorData::new(read_vector(&mut reader)?);
            if format_version >= STORE_VERSION_V6 && reader.byte()? != 0 {
                vector.content = Some(reader.string()?);
            }
            EntityData::Vector(vector)
        }
        4 => {
            let metric = reader.string()?;
            let timestamp_ns = reader.variable_u64()?;
            let value = f64::from_le_bytes(reader.fixed()?);
            let mut tags = HashMap::new();
            let series_id = if format_version >= STORE_VERSION_V11 && reader.variable_u32()? == 1 {
                Some(reader.variable_u64()?)
            } else {
                if format_version >= STORE_VERSION_V5 {
                    let count = reader.count(2)?;
                    tags = HashMap::with_capacity(count);
                    for _ in 0..count {
                        tags.insert(reader.string()?, reader.string()?);
                    }
                }
                None
            };
            let fields = if format_version >= STORE_VERSION_V12 {
                read_properties(&mut reader)?
            } else {
                HashMap::new()
            };
            EntityData::TimeSeries(TimeSeriesData {
                metric,
                timestamp_ns,
                value,
                series_id,
                tags,
                fields,
            })
        }
        5 => EntityData::QueueMessage(QueueMessageData {
            payload: reader.value()?,
            priority: None,
            enqueued_at_ns: reader.variable_u64()?,
            attempts: reader.variable_u32()?,
            max_attempts: 3,
            acked: false,
        }),
        6 => EntityData::Row(RowData {
            columns: Vec::new(),
            named: Some(read_properties(&mut reader)?),
            schema: None,
        }),
        tag => return Err(invalid(format!("Unknown EntityData type: {tag}"))),
    };
    let created_at = reader.variable_u64()?;
    let updated_at = reader.variable_u64()?;
    let count = reader.count(3)?;
    let mut embeddings = Vec::with_capacity(count);
    for _ in 0..count {
        let name = reader.string()?;
        let vector = read_vector(&mut reader)?;
        let model = reader.string()?;
        embeddings.push(EmbeddingSlot::new(name, vector, model));
    }
    let count = reader.count(3)?;
    let mut cross_refs = Vec::with_capacity(count);
    for _ in 0..count {
        let source = EntityId::new(reader.variable_u64()?);
        let target = EntityId::new(reader.variable_u64()?);
        let ref_type = RefType::from_byte(reader.byte()?);
        let (collection, weight, created_at) = if format_version >= STORE_VERSION_V2 {
            (
                reader.string()?,
                f32::from_le_bytes(reader.fixed()?),
                reader.variable_u64()?,
            )
        } else {
            (String::new(), 1.0, 0)
        };
        let mut cross_ref = CrossRef::new(source, target, collection, ref_type);
        cross_ref.weight = weight;
        cross_ref.created_at = created_at;
        cross_refs.push(cross_ref);
    }
    let sequence_id = reader.variable_u64()?;
    if format_version >= STORE_VERSION_V4 {
        if let EntityData::QueueMessage(message) = &mut data {
            if reader.remaining() != 0 {
                message.priority = if reader.byte()? != 0 {
                    Some(i32::from_le_bytes(reader.fixed()?))
                } else {
                    None
                };
                message.max_attempts = reader.variable_u32()?;
                message.acked = reader.byte()? != 0;
            }
        }
    }
    let mut entity = UnifiedEntity::new(id, kind, data);
    entity.created_at = created_at;
    entity.updated_at = updated_at;
    entity.sequence_id = sequence_id;
    if format_version >= STORE_VERSION_V8 && reader.remaining() != 0 && reader.byte()? != 0 {
        entity.set_logical_id(EntityId::new(reader.variable_u64()?));
    }
    if format_version >= STORE_VERSION_V9 && reader.remaining() != 0 {
        entity.set_xmin(reader.variable_u64()?);
        entity.set_xmax(reader.variable_u64()?);
    }
    if !embeddings.is_empty() || !cross_refs.is_empty() {
        entity.embeddings_mut().extend(embeddings);
        entity.cross_refs_mut().extend(cross_refs);
    }
    Ok(entity)
}

fn read_properties(
    reader: &mut NativeReader<'_, '_>,
) -> RdbFileResult<HashMap<String, reddb_types::Value>> {
    let count = reader.count(2)?;
    let mut properties = HashMap::with_capacity(count);
    for _ in 0..count {
        properties.insert(reader.string()?, reader.value()?);
    }
    Ok(properties)
}

fn read_vector(reader: &mut NativeReader<'_, '_>) -> RdbFileResult<Vec<f32>> {
    let count = reader.count(4)?;
    let mut vector = Vec::with_capacity(count);
    for _ in 0..count {
        vector.push(f32::from_le_bytes(reader.fixed()?));
    }
    Ok(vector)
}

fn invalid(error: impl std::fmt::Display) -> RdbFileError {
    RdbFileError::InvalidOperation(error.to_string())
}
