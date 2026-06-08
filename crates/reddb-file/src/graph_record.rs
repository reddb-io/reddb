//! Persisted graph node/edge record frames.
//!
//! The graph engine owns indexes, label resolution, and page locations. This
//! module owns the durable byte layout for graph records stored inside pages.

pub const GRAPH_MAX_ID_SIZE: usize = 256;
pub const GRAPH_MAX_LABEL_SIZE: usize = 512;

pub const GRAPH_NODE_HEADER_SIZE_V1: usize = 10;
pub const GRAPH_NODE_HEADER_SIZE: usize = 13;
pub const GRAPH_TABLE_REF_SIZE: usize = 10;
pub const GRAPH_NODE_FLAG_HAS_TABLE_REF: u8 = 0x01;
pub const GRAPH_NODE_FLAG_HAS_VECTOR_REF: u8 = 0x02;
pub const GRAPH_VECTOR_REF_HEADER_SIZE: usize = 10;

pub const GRAPH_EDGE_HEADER_SIZE_V1: usize = 9;
pub const GRAPH_EDGE_HEADER_SIZE: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GraphTableRef {
    pub table_id: u16,
    pub row_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphVectorRef {
    pub collection: String,
    pub vector_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNodeRecord {
    pub id: String,
    pub label: String,
    pub label_id: u32,
    pub flags: u8,
    pub out_edge_count: u32,
    pub in_edge_count: u32,
    pub table_ref: Option<GraphTableRef>,
    pub vector_ref: Option<GraphVectorRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyGraphNodeRecord {
    pub id: String,
    pub label: String,
    pub legacy_type: u8,
    pub flags: u8,
    pub out_edge_count: u32,
    pub in_edge_count: u32,
    pub table_ref: Option<GraphTableRef>,
    pub vector_ref: Option<GraphVectorRef>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphEdgeRecord {
    pub source_id: String,
    pub target_id: String,
    pub label_id: u32,
    pub weight: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LegacyGraphEdgeRecord {
    pub source_id: String,
    pub target_id: String,
    pub legacy_type: u8,
    pub weight: f32,
}

pub fn encode_graph_table_ref(table_ref: GraphTableRef) -> [u8; GRAPH_TABLE_REF_SIZE] {
    let mut buf = [0u8; GRAPH_TABLE_REF_SIZE];
    buf[0..2].copy_from_slice(&table_ref.table_id.to_le_bytes());
    buf[2..10].copy_from_slice(&table_ref.row_id.to_le_bytes());
    buf
}

pub fn decode_graph_table_ref(data: &[u8]) -> Option<GraphTableRef> {
    if data.len() < GRAPH_TABLE_REF_SIZE {
        return None;
    }
    Some(GraphTableRef {
        table_id: u16::from_le_bytes(data[0..2].try_into().ok()?),
        row_id: u64::from_le_bytes(data[2..10].try_into().ok()?),
    })
}

pub fn encode_graph_node_record_v2(record: &GraphNodeRecord) -> Vec<u8> {
    let id_bytes = record.id.as_bytes();
    let label_bytes = record.label.as_bytes();
    let has_table_ref = record.table_ref.is_some();
    let has_vector_ref = record.vector_ref.is_some();

    let mut flags =
        record.flags & !(GRAPH_NODE_FLAG_HAS_TABLE_REF | GRAPH_NODE_FLAG_HAS_VECTOR_REF);
    if has_table_ref {
        flags |= GRAPH_NODE_FLAG_HAS_TABLE_REF;
    }
    if has_vector_ref {
        flags |= GRAPH_NODE_FLAG_HAS_VECTOR_REF;
    }

    let table_ref_size = if has_table_ref {
        GRAPH_TABLE_REF_SIZE
    } else {
        0
    };
    let vector_ref_size = record
        .vector_ref
        .as_ref()
        .map(|v| 2 + v.collection.len() + 8)
        .unwrap_or(0);

    let mut buf = Vec::with_capacity(
        GRAPH_NODE_HEADER_SIZE
            + id_bytes.len()
            + label_bytes.len()
            + table_ref_size
            + vector_ref_size,
    );
    buf.extend_from_slice(&(id_bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(label_bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(&record.label_id.to_le_bytes());
    buf.push(flags);
    buf.extend_from_slice(&(record.out_edge_count as u16).to_le_bytes());
    buf.extend_from_slice(&(record.in_edge_count as u16).to_le_bytes());
    buf.extend_from_slice(id_bytes);
    buf.extend_from_slice(label_bytes);

    if let Some(table_ref) = record.table_ref {
        buf.extend_from_slice(&encode_graph_table_ref(table_ref));
    }
    if let Some(vector_ref) = &record.vector_ref {
        let collection = vector_ref.collection.as_bytes();
        buf.extend_from_slice(&(collection.len() as u16).to_le_bytes());
        buf.extend_from_slice(collection);
        buf.extend_from_slice(&vector_ref.vector_id.to_le_bytes());
    }
    buf
}

pub fn decode_graph_node_record_v2(data: &[u8]) -> Option<GraphNodeRecord> {
    if data.len() < GRAPH_NODE_HEADER_SIZE {
        return None;
    }
    let id_len = u16::from_le_bytes(data[0..2].try_into().ok()?) as usize;
    let label_len = u16::from_le_bytes(data[2..4].try_into().ok()?) as usize;
    let label_id = u32::from_le_bytes(data[4..8].try_into().ok()?);
    let flags = data[8];
    let out_edge_count = u16::from_le_bytes(data[9..11].try_into().ok()?) as u32;
    let in_edge_count = u16::from_le_bytes(data[11..13].try_into().ok()?) as u32;
    let (id, label, table_ref, vector_ref) =
        decode_graph_node_payload(data, GRAPH_NODE_HEADER_SIZE, id_len, label_len, flags)?;
    Some(GraphNodeRecord {
        id,
        label,
        label_id,
        flags,
        out_edge_count,
        in_edge_count,
        table_ref,
        vector_ref,
    })
}

pub fn decode_graph_node_record_v1(data: &[u8]) -> Option<LegacyGraphNodeRecord> {
    if data.len() < GRAPH_NODE_HEADER_SIZE_V1 {
        return None;
    }
    let id_len = u16::from_le_bytes(data[0..2].try_into().ok()?) as usize;
    let label_len = u16::from_le_bytes(data[2..4].try_into().ok()?) as usize;
    let legacy_type = data[4];
    let flags = data[5];
    let out_edge_count = u16::from_le_bytes(data[6..8].try_into().ok()?) as u32;
    let in_edge_count = u16::from_le_bytes(data[8..10].try_into().ok()?) as u32;
    let (id, label, table_ref, vector_ref) =
        decode_graph_node_payload(data, GRAPH_NODE_HEADER_SIZE_V1, id_len, label_len, flags)?;
    Some(LegacyGraphNodeRecord {
        id,
        label,
        legacy_type,
        flags,
        out_edge_count,
        in_edge_count,
        table_ref,
        vector_ref,
    })
}

fn decode_graph_node_payload(
    data: &[u8],
    header_size: usize,
    id_len: usize,
    label_len: usize,
    flags: u8,
) -> Option<(
    String,
    String,
    Option<GraphTableRef>,
    Option<GraphVectorRef>,
)> {
    let has_table_ref = (flags & GRAPH_NODE_FLAG_HAS_TABLE_REF) != 0;
    let has_vector_ref = (flags & GRAPH_NODE_FLAG_HAS_VECTOR_REF) != 0;
    let table_ref_size = if has_table_ref {
        GRAPH_TABLE_REF_SIZE
    } else {
        0
    };
    let mut offset = header_size + id_len + label_len + table_ref_size;
    if data.len() < offset {
        return None;
    }

    let id = String::from_utf8_lossy(&data[header_size..header_size + id_len]).to_string();
    let label =
        String::from_utf8_lossy(&data[header_size + id_len..header_size + id_len + label_len])
            .to_string();
    let table_ref = if has_table_ref {
        let start = header_size + id_len + label_len;
        decode_graph_table_ref(&data[start..])
    } else {
        None
    };
    let vector_ref = if has_vector_ref {
        if data.len() < offset + 2 {
            return None;
        }
        let collection_len = u16::from_le_bytes(data[offset..offset + 2].try_into().ok()?) as usize;
        offset += 2;
        if data.len() < offset + collection_len + 8 {
            return None;
        }
        let collection =
            String::from_utf8_lossy(&data[offset..offset + collection_len]).to_string();
        offset += collection_len;
        let vector_id = u64::from_le_bytes(data[offset..offset + 8].try_into().ok()?);
        Some(GraphVectorRef {
            collection,
            vector_id,
        })
    } else {
        None
    };
    Some((id, label, table_ref, vector_ref))
}

pub fn graph_node_record_v2_encoded_size(record: &GraphNodeRecord) -> usize {
    let table_ref_size = if record.table_ref.is_some() {
        GRAPH_TABLE_REF_SIZE
    } else {
        0
    };
    let vector_ref_size = record
        .vector_ref
        .as_ref()
        .map(|v| 2 + v.collection.len() + 8)
        .unwrap_or(0);
    GRAPH_NODE_HEADER_SIZE + record.id.len() + record.label.len() + table_ref_size + vector_ref_size
}

pub fn encode_graph_edge_record_v2(record: &GraphEdgeRecord) -> Vec<u8> {
    let source_bytes = record.source_id.as_bytes();
    let target_bytes = record.target_id.as_bytes();
    let mut buf =
        Vec::with_capacity(GRAPH_EDGE_HEADER_SIZE + source_bytes.len() + target_bytes.len());
    buf.extend_from_slice(&(source_bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(target_bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(&record.label_id.to_le_bytes());
    buf.extend_from_slice(&record.weight.to_le_bytes());
    buf.extend_from_slice(source_bytes);
    buf.extend_from_slice(target_bytes);
    buf
}

pub fn decode_graph_edge_record_v2(data: &[u8]) -> Option<GraphEdgeRecord> {
    if data.len() < GRAPH_EDGE_HEADER_SIZE {
        return None;
    }
    let source_len = u16::from_le_bytes(data[0..2].try_into().ok()?) as usize;
    let target_len = u16::from_le_bytes(data[2..4].try_into().ok()?) as usize;
    let label_id = u32::from_le_bytes(data[4..8].try_into().ok()?);
    let weight = f32::from_le_bytes(data[8..12].try_into().ok()?);
    if data.len() < GRAPH_EDGE_HEADER_SIZE + source_len + target_len {
        return None;
    }
    let source_id =
        String::from_utf8_lossy(&data[GRAPH_EDGE_HEADER_SIZE..GRAPH_EDGE_HEADER_SIZE + source_len])
            .to_string();
    let target_id = String::from_utf8_lossy(
        &data
            [GRAPH_EDGE_HEADER_SIZE + source_len..GRAPH_EDGE_HEADER_SIZE + source_len + target_len],
    )
    .to_string();
    Some(GraphEdgeRecord {
        source_id,
        target_id,
        label_id,
        weight,
    })
}

pub fn decode_graph_edge_record_v1(data: &[u8]) -> Option<LegacyGraphEdgeRecord> {
    if data.len() < GRAPH_EDGE_HEADER_SIZE_V1 {
        return None;
    }
    let source_len = u16::from_le_bytes(data[0..2].try_into().ok()?) as usize;
    let target_len = u16::from_le_bytes(data[2..4].try_into().ok()?) as usize;
    let legacy_type = data[4];
    let weight = f32::from_le_bytes(data[5..9].try_into().ok()?);
    if data.len() < GRAPH_EDGE_HEADER_SIZE_V1 + source_len + target_len {
        return None;
    }
    let source_id = String::from_utf8_lossy(
        &data[GRAPH_EDGE_HEADER_SIZE_V1..GRAPH_EDGE_HEADER_SIZE_V1 + source_len],
    )
    .to_string();
    let target_id = String::from_utf8_lossy(
        &data[GRAPH_EDGE_HEADER_SIZE_V1 + source_len
            ..GRAPH_EDGE_HEADER_SIZE_V1 + source_len + target_len],
    )
    .to_string();
    Some(LegacyGraphEdgeRecord {
        source_id,
        target_id,
        legacy_type,
        weight,
    })
}

pub fn graph_edge_record_v2_encoded_size(record: &GraphEdgeRecord) -> usize {
    GRAPH_EDGE_HEADER_SIZE + record.source_id.len() + record.target_id.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_table_ref_round_trips() {
        let table_ref = GraphTableRef {
            table_id: 7,
            row_id: 42,
        };
        let bytes = encode_graph_table_ref(table_ref);
        assert_eq!(decode_graph_table_ref(&bytes), Some(table_ref));
    }

    #[test]
    fn graph_node_v2_round_trips_with_refs() {
        let record = GraphNodeRecord {
            id: "node-1".to_string(),
            label: "Node One".to_string(),
            label_id: 64,
            flags: 0x80,
            out_edge_count: 2,
            in_edge_count: 3,
            table_ref: Some(GraphTableRef {
                table_id: 9,
                row_id: 10,
            }),
            vector_ref: Some(GraphVectorRef {
                collection: "embeddings".to_string(),
                vector_id: 11,
            }),
        };
        let encoded = encode_graph_node_record_v2(&record);
        let decoded = decode_graph_node_record_v2(&encoded).unwrap();
        assert_eq!(decoded.id, record.id);
        assert_eq!(decoded.label, record.label);
        assert_eq!(decoded.label_id, record.label_id);
        assert_eq!(decoded.flags & 0x80, 0x80);
        assert_eq!(decoded.table_ref, record.table_ref);
        assert_eq!(decoded.vector_ref, record.vector_ref);
    }

    #[test]
    fn graph_edge_v2_round_trips() {
        let record = GraphEdgeRecord {
            source_id: "a".to_string(),
            target_id: "b".to_string(),
            label_id: 10,
            weight: 1.5,
        };
        let encoded = encode_graph_edge_record_v2(&record);
        assert_eq!(decode_graph_edge_record_v2(&encoded).unwrap(), record);
    }

    #[test]
    fn legacy_records_decode_shape() {
        let mut node = Vec::new();
        node.extend_from_slice(&1u16.to_le_bytes());
        node.extend_from_slice(&1u16.to_le_bytes());
        node.push(0);
        node.push(0);
        node.extend_from_slice(&0u16.to_le_bytes());
        node.extend_from_slice(&0u16.to_le_bytes());
        node.extend_from_slice(b"a");
        node.extend_from_slice(b"b");
        assert_eq!(decode_graph_node_record_v1(&node).unwrap().legacy_type, 0);

        let mut edge = Vec::new();
        edge.extend_from_slice(&1u16.to_le_bytes());
        edge.extend_from_slice(&1u16.to_le_bytes());
        edge.push(0);
        edge.extend_from_slice(&1.0f32.to_le_bytes());
        edge.extend_from_slice(b"a");
        edge.extend_from_slice(b"b");
        assert_eq!(decode_graph_edge_record_v1(&edge).unwrap().legacy_type, 0);
    }
}
