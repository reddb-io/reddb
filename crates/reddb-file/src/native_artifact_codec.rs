//! On-disk codecs for the server-defined native artifact payloads:
//! `RDGA` (graph adjacency), `RDFT` (full-text index), and `RDDP`
//! (document path/value). These are derived index artifacts persisted into the
//! native store; `reddb-file` owns their byte layout (ADR 0046) while the
//! tokenisation / index-construction algorithms stay in the server engine.
//!
//! All three share a length-prefixed string framing: a `u32` little-endian
//! byte length followed by the UTF-8 bytes.
//!
//! `RDDP` pins `entity_id` as a fixed little-endian `u64`, **not** a string.

use std::collections::BTreeMap;

/// Magic prefixing a persisted graph-adjacency artifact.
pub const GRAPH_ADJACENCY_MAGIC: &[u8; 4] = b"RDGA";
/// Magic prefixing a persisted full-text index artifact.
pub const FULLTEXT_INDEX_MAGIC: &[u8; 4] = b"RDFT";
/// Magic prefixing a persisted document path/value artifact.
pub const DOC_PATHVALUE_MAGIC: &[u8; 4] = b"RDDP";

/// Errors raised while decoding a native artifact payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeArtifactError {
    /// The payload was shorter than its fixed header or had the wrong magic.
    InvalidHeader(&'static str),
    /// A field ran past the end of the buffer.
    Truncated(&'static str),
    /// A length-prefixed string contained invalid UTF-8.
    InvalidUtf8,
    /// The `RDDP` header entry count disagreed with the decoded records.
    EntryCountMismatch,
}

impl std::fmt::Display for NativeArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NativeArtifactError::InvalidHeader(m) => write!(f, "{}", m),
            NativeArtifactError::Truncated(m) => write!(f, "{}", m),
            NativeArtifactError::InvalidUtf8 => write!(f, "invalid utf-8 in native artifact"),
            NativeArtifactError::EntryCountMismatch => {
                write!(f, "document path/value artifact entry count mismatch")
            }
        }
    }
}

impl std::error::Error for NativeArtifactError {}

fn push_string(buf: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

fn read_string(bytes: &[u8], pos: &mut usize) -> Result<String, NativeArtifactError> {
    if *pos + 4 > bytes.len() {
        return Err(NativeArtifactError::Truncated(
            "truncated native artifact string length",
        ));
    }
    let len = u32::from_le_bytes([
        bytes[*pos],
        bytes[*pos + 1],
        bytes[*pos + 2],
        bytes[*pos + 3],
    ]) as usize;
    *pos += 4;
    if *pos + len > bytes.len() {
        return Err(NativeArtifactError::Truncated(
            "truncated native artifact string content",
        ));
    }
    let value = std::str::from_utf8(&bytes[*pos..*pos + len])
        .map_err(|_| NativeArtifactError::InvalidUtf8)?
        .to_string();
    *pos += len;
    Ok(value)
}

fn read_u32(bytes: &[u8], pos: &mut usize, ctx: &'static str) -> Result<u32, NativeArtifactError> {
    if *pos + 4 > bytes.len() {
        return Err(NativeArtifactError::Truncated(ctx));
    }
    let value = u32::from_le_bytes([
        bytes[*pos],
        bytes[*pos + 1],
        bytes[*pos + 2],
        bytes[*pos + 3],
    ]);
    *pos += 4;
    Ok(value)
}

fn read_u64(bytes: &[u8], pos: &mut usize, ctx: &'static str) -> Result<u64, NativeArtifactError> {
    if *pos + 8 > bytes.len() {
        return Err(NativeArtifactError::Truncated(ctx));
    }
    let value = u64::from_le_bytes([
        bytes[*pos],
        bytes[*pos + 1],
        bytes[*pos + 2],
        bytes[*pos + 3],
        bytes[*pos + 4],
        bytes[*pos + 5],
        bytes[*pos + 6],
        bytes[*pos + 7],
    ]);
    *pos += 8;
    Ok(value)
}

// ============================================================================
// RDGA — graph adjacency
// ============================================================================

/// One persisted graph edge (`RDGA`).
#[derive(Debug, Clone, PartialEq)]
pub struct GraphAdjacencyEdge {
    /// Edge entity id (`entity_id.raw()` on the server side).
    pub edge_id: u64,
    /// Source node key.
    pub from_node: String,
    /// Target node key.
    pub to_node: String,
    /// Edge label.
    pub label: String,
    /// Edge weight.
    pub weight: f32,
}

/// Encode a graph-adjacency (`RDGA`) artifact, byte-faithful to the server.
pub fn encode_graph_adjacency(edges: &[GraphAdjacencyEdge]) -> Vec<u8> {
    let mut data = Vec::with_capacity(32 + edges.len() * 48);
    data.extend_from_slice(GRAPH_ADJACENCY_MAGIC);
    data.extend_from_slice(&(edges.len() as u32).to_le_bytes());
    for edge in edges {
        data.extend_from_slice(&edge.edge_id.to_le_bytes());
        push_string(&mut data, &edge.from_node);
        push_string(&mut data, &edge.to_node);
        push_string(&mut data, &edge.label);
        data.extend_from_slice(&edge.weight.to_le_bytes());
    }
    data
}

/// Decode a graph-adjacency (`RDGA`) artifact.
pub fn decode_graph_adjacency(
    bytes: &[u8],
) -> Result<Vec<GraphAdjacencyEdge>, NativeArtifactError> {
    if bytes.len() < 8 || &bytes[0..4] != GRAPH_ADJACENCY_MAGIC {
        return Err(NativeArtifactError::InvalidHeader(
            "invalid graph adjacency artifact",
        ));
    }
    let mut pos = 4usize;
    let edge_count = read_u32(bytes, &mut pos, "truncated graph adjacency artifact")? as usize;
    let mut edges = Vec::with_capacity(edge_count);
    for _ in 0..edge_count {
        let edge_id = read_u64(bytes, &mut pos, "truncated graph adjacency artifact")?;
        let from_node = read_string(bytes, &mut pos)?;
        let to_node = read_string(bytes, &mut pos)?;
        let label = read_string(bytes, &mut pos)?;
        if pos + 4 > bytes.len() {
            return Err(NativeArtifactError::Truncated(
                "truncated graph adjacency artifact weight",
            ));
        }
        let weight =
            f32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        pos += 4;
        edges.push(GraphAdjacencyEdge {
            edge_id,
            from_node,
            to_node,
            label,
            weight,
        });
    }
    Ok(edges)
}

// ============================================================================
// RDFT — full-text index
// ============================================================================

/// A decoded full-text index artifact (`RDFT`).
#[derive(Debug, Clone, PartialEq)]
pub struct FulltextIndex {
    /// Collection the index was built over.
    pub collection: String,
    /// Number of source documents.
    pub doc_count: u32,
    /// term -> sorted postings of `(entity_id, term_frequency)`.
    pub postings: BTreeMap<String, Vec<(u64, u32)>>,
}

/// Encode a full-text index (`RDFT`) artifact, byte-faithful to the server.
///
/// `postings` is written in `BTreeMap` (lexicographic term) order. The legacy
/// server built the same `BTreeMap` before serialising, so the byte stream is
/// identical.
pub fn encode_fulltext_index(
    collection: &str,
    doc_count: usize,
    postings: &BTreeMap<String, Vec<(u64, u32)>>,
) -> Vec<u8> {
    let mut data = Vec::with_capacity(64 + postings.len() * 32);
    data.extend_from_slice(FULLTEXT_INDEX_MAGIC);
    push_string(&mut data, collection);
    data.extend_from_slice(&(doc_count as u32).to_le_bytes());
    data.extend_from_slice(&(postings.len() as u32).to_le_bytes());
    for (term, entries) in postings {
        push_string(&mut data, term);
        data.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (entity_id, term_count) in entries {
            data.extend_from_slice(&entity_id.to_le_bytes());
            data.extend_from_slice(&term_count.to_le_bytes());
        }
    }
    data
}

/// Decode a full-text index (`RDFT`) artifact.
pub fn decode_fulltext_index(bytes: &[u8]) -> Result<FulltextIndex, NativeArtifactError> {
    if bytes.len() < 12 || &bytes[0..4] != FULLTEXT_INDEX_MAGIC {
        return Err(NativeArtifactError::InvalidHeader(
            "invalid fulltext artifact",
        ));
    }
    let mut pos = 4usize;
    let collection = read_string(bytes, &mut pos)?;
    let doc_count = read_u32(bytes, &mut pos, "truncated fulltext artifact")?;
    let term_count = read_u32(bytes, &mut pos, "truncated fulltext artifact")? as usize;
    let mut postings = BTreeMap::new();
    for _ in 0..term_count {
        let term = read_string(bytes, &mut pos)?;
        let entry_count = read_u32(bytes, &mut pos, "truncated fulltext posting count")? as usize;
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let entity_id = read_u64(bytes, &mut pos, "truncated fulltext postings")?;
            let term_freq = read_u32(bytes, &mut pos, "truncated fulltext postings")?;
            entries.push((entity_id, term_freq));
        }
        postings.insert(term, entries);
    }
    Ok(FulltextIndex {
        collection,
        doc_count,
        postings,
    })
}

// ============================================================================
// RDDP — document path/value
// ============================================================================

/// One persisted document record (`RDDP`).
#[derive(Debug, Clone, PartialEq)]
pub struct DocPathValueRecord {
    /// Document entity id, persisted as a fixed little-endian `u64`.
    pub entity_id: u64,
    /// `(path, value)` entries in persistence order.
    pub entries: Vec<(String, String)>,
}

/// A decoded document path/value artifact (`RDDP`).
#[derive(Debug, Clone, PartialEq)]
pub struct DocPathValueIndex {
    /// Collection the artifact was built over.
    pub collection: String,
    /// Document records in persistence order.
    pub documents: Vec<DocPathValueRecord>,
}

/// Encode a document path/value (`RDDP`) artifact, byte-faithful to the server.
///
/// `entity_id` is written as a fixed little-endian `u64` (the server passes
/// `entity_id.raw()`), never a string.
pub fn encode_document_pathvalue(collection: &str, documents: &[DocPathValueRecord]) -> Vec<u8> {
    let total_entries: usize = documents.iter().map(|doc| doc.entries.len()).sum();
    let mut data = Vec::with_capacity(64 + total_entries * 48);
    data.extend_from_slice(DOC_PATHVALUE_MAGIC);
    push_string(&mut data, collection);
    data.extend_from_slice(&(documents.len() as u32).to_le_bytes());
    data.extend_from_slice(&(total_entries as u32).to_le_bytes());
    for doc in documents {
        data.extend_from_slice(&doc.entity_id.to_le_bytes());
        data.extend_from_slice(&(doc.entries.len() as u32).to_le_bytes());
        for (path, value) in &doc.entries {
            push_string(&mut data, path);
            push_string(&mut data, value);
        }
    }
    data
}

/// Decode a document path/value (`RDDP`) artifact, validating the header's
/// total-entry count against the decoded records.
pub fn decode_document_pathvalue(bytes: &[u8]) -> Result<DocPathValueIndex, NativeArtifactError> {
    if bytes.len() < 12 || &bytes[0..4] != DOC_PATHVALUE_MAGIC {
        return Err(NativeArtifactError::InvalidHeader(
            "invalid document path/value artifact",
        ));
    }
    let mut pos = 4usize;
    let collection = read_string(bytes, &mut pos)?;
    let doc_count = read_u32(bytes, &mut pos, "truncated document path/value artifact")? as usize;
    let total_entries = read_u32(bytes, &mut pos, "truncated document path/value artifact")? as u64;
    let mut documents = Vec::with_capacity(doc_count);
    let mut seen_entries = 0u64;
    for _ in 0..doc_count {
        let entity_id = read_u64(bytes, &mut pos, "truncated document path/value record")?;
        let entry_count =
            read_u32(bytes, &mut pos, "truncated document path/value record")? as usize;
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let path = read_string(bytes, &mut pos)?;
            let value = read_string(bytes, &mut pos)?;
            entries.push((path, value));
            seen_entries += 1;
        }
        documents.push(DocPathValueRecord { entity_id, entries });
    }
    if seen_entries != total_entries {
        return Err(NativeArtifactError::EntryCountMismatch);
    }
    Ok(DocPathValueIndex {
        collection,
        documents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_adjacency_round_trip() {
        let edges = vec![
            GraphAdjacencyEdge {
                edge_id: 42,
                from_node: "a".to_string(),
                to_node: "b".to_string(),
                label: "knows".to_string(),
                weight: 0.5,
            },
            GraphAdjacencyEdge {
                edge_id: 7,
                from_node: "b".to_string(),
                to_node: "c".to_string(),
                label: "likes".to_string(),
                weight: -1.25,
            },
        ];
        let bytes = encode_graph_adjacency(&edges);
        assert_eq!(&bytes[0..4], b"RDGA");
        // edge_id of the first edge is a u64 immediately after the count.
        assert_eq!(&bytes[8..16], &42u64.to_le_bytes());
        assert_eq!(decode_graph_adjacency(&bytes).unwrap(), edges);
    }

    #[test]
    fn fulltext_round_trip() {
        let mut postings: BTreeMap<String, Vec<(u64, u32)>> = BTreeMap::new();
        postings.insert("alpha".to_string(), vec![(1, 2), (3, 1)]);
        postings.insert("beta".to_string(), vec![(3, 5)]);
        let bytes = encode_fulltext_index("docs", 2, &postings);
        assert_eq!(&bytes[0..4], b"RDFT");
        let decoded = decode_fulltext_index(&bytes).unwrap();
        assert_eq!(decoded.collection, "docs");
        assert_eq!(decoded.doc_count, 2);
        assert_eq!(decoded.postings, postings);
    }

    #[test]
    fn doc_pathvalue_round_trip_pins_u64_entity_id() {
        let documents = vec![
            DocPathValueRecord {
                entity_id: 0xDEAD_BEEF_0000_0001,
                entries: vec![
                    ("name".to_string(), "alice".to_string()),
                    ("age".to_string(), "30".to_string()),
                ],
            },
            DocPathValueRecord {
                entity_id: 2,
                entries: vec![("name".to_string(), "bob".to_string())],
            },
        ];
        let bytes = encode_document_pathvalue("people", &documents);
        assert_eq!(&bytes[0..4], b"RDDP");

        // The entity_id must be a fixed little-endian u64 right after the
        // collection string + doc_count u32 + total_entries u32 header.
        let mut pos = 4usize;
        let coll_len =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
                as usize;
        pos += 4 + coll_len; // collection string
        pos += 4; // doc_count
        pos += 4; // total_entries
        let entity_id = u64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap());
        assert_eq!(entity_id, 0xDEAD_BEEF_0000_0001);

        let decoded = decode_document_pathvalue(&bytes).unwrap();
        assert_eq!(decoded.collection, "people");
        assert_eq!(decoded.documents, documents);
    }

    #[test]
    fn doc_pathvalue_detects_entry_count_mismatch() {
        let documents = vec![DocPathValueRecord {
            entity_id: 1,
            entries: vec![("p".to_string(), "v".to_string())],
        }];
        let mut bytes = encode_document_pathvalue("c", &documents);
        // Corrupt the total_entries header (collection "c" => len 1).
        // layout: magic(4) + len(4) + "c"(1) + doc_count(4) + total_entries(4)
        let total_off = 4 + 4 + 1 + 4;
        bytes[total_off] = 9;
        assert_eq!(
            decode_document_pathvalue(&bytes),
            Err(NativeArtifactError::EntryCountMismatch)
        );
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(matches!(
            decode_graph_adjacency(b"XXXX\0\0\0\0"),
            Err(NativeArtifactError::InvalidHeader(_))
        ));
        assert!(matches!(
            decode_fulltext_index(&[0u8; 12]),
            Err(NativeArtifactError::InvalidHeader(_))
        ));
        assert!(matches!(
            decode_document_pathvalue(&[0u8; 12]),
            Err(NativeArtifactError::InvalidHeader(_))
        ));
    }
}
