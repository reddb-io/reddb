//! Persisted native secondary-index artifact codecs (RDGA / RDFT / RDDP).
//!
//! These three server-defined payloads are written alongside native vector
//! artifacts to describe graph adjacency, full-text postings, and document
//! path/value projections. The engine owns tokenization, JSON path extraction,
//! and the derived summary statistics; this module owns only the durable byte
//! layout of each artifact.
//!
//! All three share a length-prefixed string encoding: a `u32` little-endian
//! byte length followed by the UTF-8 bytes. All integers are little-endian.
//! DO NOT change magic/order/width — these bytes live in existing artifacts.
//!
//! `RDDP` document `entity_id` is a fixed `u64` (the raw entity id), NOT a
//! string — pinned by [`tests::rddp_entity_id_is_fixed_u64`].

// ============================================================================
// Shared helpers
// ============================================================================

/// Errors decoding a native index artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeArtifactFrameError {
    TooShort {
        artifact: &'static str,
    },
    InvalidMagic {
        artifact: &'static str,
    },
    Truncated {
        artifact: &'static str,
        offset: usize,
        reason: &'static str,
    },
    InvalidUtf8 {
        offset: usize,
    },
    EntryCountMismatch {
        expected: u64,
        seen: u64,
    },
}

impl std::fmt::Display for NativeArtifactFrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort { artifact } => write!(f, "invalid {artifact} artifact"),
            Self::InvalidMagic { artifact } => write!(f, "invalid {artifact} artifact"),
            Self::Truncated {
                artifact,
                offset,
                reason,
            } => write!(
                f,
                "truncated {artifact} artifact at offset {offset}: {reason}"
            ),
            Self::InvalidUtf8 { offset } => {
                write!(f, "invalid utf-8 in native artifact at offset {offset}")
            }
            Self::EntryCountMismatch { expected, seen } => write!(
                f,
                "document path/value artifact entry count mismatch: expected {expected}, saw {seen}"
            ),
        }
    }
}

impl std::error::Error for NativeArtifactFrameError {}

fn push_string(buf: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

fn read_string(
    bytes: &[u8],
    pos: &mut usize,
    artifact: &'static str,
) -> Result<String, NativeArtifactFrameError> {
    let len = read_u32(bytes, pos, artifact, "string length")? as usize;
    if *pos + len > bytes.len() {
        return Err(NativeArtifactFrameError::Truncated {
            artifact,
            offset: *pos,
            reason: "string content",
        });
    }
    let value = std::str::from_utf8(&bytes[*pos..*pos + len])
        .map_err(|_| NativeArtifactFrameError::InvalidUtf8 { offset: *pos })?
        .to_string();
    *pos += len;
    Ok(value)
}

fn read_u32(
    bytes: &[u8],
    pos: &mut usize,
    artifact: &'static str,
    reason: &'static str,
) -> Result<u32, NativeArtifactFrameError> {
    if *pos + 4 > bytes.len() {
        return Err(NativeArtifactFrameError::Truncated {
            artifact,
            offset: *pos,
            reason,
        });
    }
    let value = u32::from_le_bytes(
        bytes[*pos..*pos + 4]
            .try_into()
            .expect("u32 length checked"),
    );
    *pos += 4;
    Ok(value)
}

fn read_u64(
    bytes: &[u8],
    pos: &mut usize,
    artifact: &'static str,
    reason: &'static str,
) -> Result<u64, NativeArtifactFrameError> {
    if *pos + 8 > bytes.len() {
        return Err(NativeArtifactFrameError::Truncated {
            artifact,
            offset: *pos,
            reason,
        });
    }
    let value = u64::from_le_bytes(
        bytes[*pos..*pos + 8]
            .try_into()
            .expect("u64 length checked"),
    );
    *pos += 8;
    Ok(value)
}

fn read_f32(
    bytes: &[u8],
    pos: &mut usize,
    artifact: &'static str,
    reason: &'static str,
) -> Result<f32, NativeArtifactFrameError> {
    if *pos + 4 > bytes.len() {
        return Err(NativeArtifactFrameError::Truncated {
            artifact,
            offset: *pos,
            reason,
        });
    }
    let value = f32::from_le_bytes(
        bytes[*pos..*pos + 4]
            .try_into()
            .expect("f32 length checked"),
    );
    *pos += 4;
    Ok(value)
}

// ============================================================================
// RDGA — graph adjacency
// ============================================================================

/// Magic prefix for a graph-adjacency artifact.
pub const NATIVE_GRAPH_ADJACENCY_MAGIC: [u8; 4] = *b"RDGA";

/// One decoded graph edge.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeGraphEdge {
    /// Raw entity id of the edge (`EntityId::raw()` in the engine).
    pub edge_id: u64,
    pub from_node: String,
    pub to_node: String,
    pub label: String,
    pub weight: f32,
}

/// A decoded graph-adjacency (`RDGA`) artifact.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeGraphAdjacencyFrame {
    pub edges: Vec<NativeGraphEdge>,
}

/// Serialize a graph-adjacency artifact.
pub fn encode_native_graph_adjacency_frame(frame: &NativeGraphAdjacencyFrame) -> Vec<u8> {
    let mut data = Vec::with_capacity(32 + frame.edges.len() * 48);
    data.extend_from_slice(&NATIVE_GRAPH_ADJACENCY_MAGIC);
    data.extend_from_slice(&(frame.edges.len() as u32).to_le_bytes());
    for edge in &frame.edges {
        data.extend_from_slice(&edge.edge_id.to_le_bytes());
        push_string(&mut data, &edge.from_node);
        push_string(&mut data, &edge.to_node);
        push_string(&mut data, &edge.label);
        data.extend_from_slice(&edge.weight.to_le_bytes());
    }
    data
}

/// Deserialize a graph-adjacency artifact.
pub fn decode_native_graph_adjacency_frame(
    bytes: &[u8],
) -> Result<NativeGraphAdjacencyFrame, NativeArtifactFrameError> {
    const ARTIFACT: &str = "graph adjacency";
    if bytes.len() < 8 || bytes[0..4] != NATIVE_GRAPH_ADJACENCY_MAGIC {
        return Err(NativeArtifactFrameError::InvalidMagic { artifact: ARTIFACT });
    }
    let mut pos = 4usize;
    let edge_count = read_u32(bytes, &mut pos, ARTIFACT, "edge count")? as usize;
    let mut edges = Vec::with_capacity(edge_count);
    for _ in 0..edge_count {
        let edge_id = read_u64(bytes, &mut pos, ARTIFACT, "edge id")?;
        let from_node = read_string(bytes, &mut pos, ARTIFACT)?;
        let to_node = read_string(bytes, &mut pos, ARTIFACT)?;
        let label = read_string(bytes, &mut pos, ARTIFACT)?;
        let weight = read_f32(bytes, &mut pos, ARTIFACT, "edge weight")?;
        edges.push(NativeGraphEdge {
            edge_id,
            from_node,
            to_node,
            label,
            weight,
        });
    }
    Ok(NativeGraphAdjacencyFrame { edges })
}

// ============================================================================
// RDFT — full-text postings
// ============================================================================

/// Magic prefix for a full-text artifact.
pub const NATIVE_FULLTEXT_MAGIC: [u8; 4] = *b"RDFT";

/// One posting entry: an entity and its term frequency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFulltextPosting {
    /// Raw entity id (`EntityId::raw()` in the engine).
    pub entity_id: u64,
    pub term_count: u32,
}

/// A term and its posting list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFulltextTerm {
    pub term: String,
    pub postings: Vec<NativeFulltextPosting>,
}

/// A decoded full-text (`RDFT`) artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFulltextFrame {
    pub collection: String,
    /// Number of source documents (independent of the posting lists).
    pub doc_count: u32,
    pub terms: Vec<NativeFulltextTerm>,
}

/// Serialize a full-text artifact.
pub fn encode_native_fulltext_frame(frame: &NativeFulltextFrame) -> Vec<u8> {
    let mut data = Vec::with_capacity(64 + frame.terms.len() * 32);
    data.extend_from_slice(&NATIVE_FULLTEXT_MAGIC);
    push_string(&mut data, &frame.collection);
    data.extend_from_slice(&frame.doc_count.to_le_bytes());
    data.extend_from_slice(&(frame.terms.len() as u32).to_le_bytes());
    for term in &frame.terms {
        push_string(&mut data, &term.term);
        data.extend_from_slice(&(term.postings.len() as u32).to_le_bytes());
        for posting in &term.postings {
            data.extend_from_slice(&posting.entity_id.to_le_bytes());
            data.extend_from_slice(&posting.term_count.to_le_bytes());
        }
    }
    data
}

/// Deserialize a full-text artifact.
pub fn decode_native_fulltext_frame(
    bytes: &[u8],
) -> Result<NativeFulltextFrame, NativeArtifactFrameError> {
    const ARTIFACT: &str = "fulltext";
    if bytes.len() < 12 || bytes[0..4] != NATIVE_FULLTEXT_MAGIC {
        return Err(NativeArtifactFrameError::InvalidMagic { artifact: ARTIFACT });
    }
    let mut pos = 4usize;
    let collection = read_string(bytes, &mut pos, ARTIFACT)?;
    let doc_count = read_u32(bytes, &mut pos, ARTIFACT, "doc count")?;
    let term_count = read_u32(bytes, &mut pos, ARTIFACT, "term count")? as usize;
    let mut terms = Vec::with_capacity(term_count);
    for _ in 0..term_count {
        let term = read_string(bytes, &mut pos, ARTIFACT)?;
        let entry_count = read_u32(bytes, &mut pos, ARTIFACT, "posting count")? as usize;
        let mut postings = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let entity_id = read_u64(bytes, &mut pos, ARTIFACT, "posting entity id")?;
            let term_count = read_u32(bytes, &mut pos, ARTIFACT, "posting term count")?;
            postings.push(NativeFulltextPosting {
                entity_id,
                term_count,
            });
        }
        terms.push(NativeFulltextTerm { term, postings });
    }
    Ok(NativeFulltextFrame {
        collection,
        doc_count,
        terms,
    })
}

// ============================================================================
// RDDP — document path/value projection
// ============================================================================

/// Magic prefix for a document path/value artifact.
pub const NATIVE_DOC_PATHVALUE_MAGIC: [u8; 4] = *b"RDDP";

/// One path/value pair extracted from a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeDocPathValueEntry {
    pub path: String,
    pub value: String,
}

/// One document and its extracted path/value entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeDocPathValue {
    /// Raw entity id — a fixed `u64`, NOT a string.
    pub entity_id: u64,
    pub entries: Vec<NativeDocPathValueEntry>,
}

/// A decoded document path/value (`RDDP`) artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeDocPathValueFrame {
    pub collection: String,
    pub docs: Vec<NativeDocPathValue>,
}

impl NativeDocPathValueFrame {
    /// Total path/value entries across all documents — the `total_entries`
    /// header word.
    fn total_entries(&self) -> usize {
        self.docs.iter().map(|doc| doc.entries.len()).sum()
    }
}

/// Serialize a document path/value artifact.
pub fn encode_native_doc_pathvalue_frame(frame: &NativeDocPathValueFrame) -> Vec<u8> {
    let total_entries = frame.total_entries();
    let mut data = Vec::with_capacity(64 + total_entries * 48);
    data.extend_from_slice(&NATIVE_DOC_PATHVALUE_MAGIC);
    push_string(&mut data, &frame.collection);
    data.extend_from_slice(&(frame.docs.len() as u32).to_le_bytes());
    data.extend_from_slice(&(total_entries as u32).to_le_bytes());
    for doc in &frame.docs {
        data.extend_from_slice(&doc.entity_id.to_le_bytes());
        data.extend_from_slice(&(doc.entries.len() as u32).to_le_bytes());
        for entry in &doc.entries {
            push_string(&mut data, &entry.path);
            push_string(&mut data, &entry.value);
        }
    }
    data
}

/// Deserialize a document path/value artifact. Validates the on-disk
/// `total_entries` header word against the documents actually parsed.
pub fn decode_native_doc_pathvalue_frame(
    bytes: &[u8],
) -> Result<NativeDocPathValueFrame, NativeArtifactFrameError> {
    const ARTIFACT: &str = "document path/value";
    if bytes.len() < 12 || bytes[0..4] != NATIVE_DOC_PATHVALUE_MAGIC {
        return Err(NativeArtifactFrameError::InvalidMagic { artifact: ARTIFACT });
    }
    let mut pos = 4usize;
    let collection = read_string(bytes, &mut pos, ARTIFACT)?;
    let doc_count = read_u32(bytes, &mut pos, ARTIFACT, "doc count")? as usize;
    let total_entries = read_u32(bytes, &mut pos, ARTIFACT, "total entries")? as u64;
    let mut docs = Vec::with_capacity(doc_count);
    let mut seen_entries = 0u64;
    for _ in 0..doc_count {
        let entity_id = read_u64(bytes, &mut pos, ARTIFACT, "document entity id")?;
        let entry_count = read_u32(bytes, &mut pos, ARTIFACT, "document entry count")? as usize;
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let path = read_string(bytes, &mut pos, ARTIFACT)?;
            let value = read_string(bytes, &mut pos, ARTIFACT)?;
            entries.push(NativeDocPathValueEntry { path, value });
            seen_entries += 1;
        }
        docs.push(NativeDocPathValue { entity_id, entries });
    }
    if seen_entries != total_entries {
        return Err(NativeArtifactFrameError::EntryCountMismatch {
            expected: total_entries,
            seen: seen_entries,
        });
    }
    Ok(NativeDocPathValueFrame { collection, docs })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rdga_round_trips_and_pins_layout() {
        let frame = NativeGraphAdjacencyFrame {
            edges: vec![
                NativeGraphEdge {
                    edge_id: 42,
                    from_node: "a".into(),
                    to_node: "b".into(),
                    label: "knows".into(),
                    weight: 1.5,
                },
                NativeGraphEdge {
                    edge_id: 7,
                    from_node: "b".into(),
                    to_node: "c".into(),
                    label: "likes".into(),
                    weight: -0.25,
                },
            ],
        };
        let encoded = encode_native_graph_adjacency_frame(&frame);
        assert_eq!(&encoded[0..4], b"RDGA");
        assert_eq!(&encoded[4..8], &2u32.to_le_bytes());
        // First edge id is a fixed u64 immediately after the count.
        assert_eq!(&encoded[8..16], &42u64.to_le_bytes());
        let decoded = decode_native_graph_adjacency_frame(&encoded).unwrap();
        assert_eq!(decoded, frame);
        assert_eq!(encode_native_graph_adjacency_frame(&decoded), encoded);
    }

    #[test]
    fn rdft_round_trips_and_pins_layout() {
        let frame = NativeFulltextFrame {
            collection: "docs".into(),
            doc_count: 3,
            terms: vec![
                NativeFulltextTerm {
                    term: "alpha".into(),
                    postings: vec![
                        NativeFulltextPosting {
                            entity_id: 100,
                            term_count: 2,
                        },
                        NativeFulltextPosting {
                            entity_id: 101,
                            term_count: 1,
                        },
                    ],
                },
                NativeFulltextTerm {
                    term: "beta".into(),
                    postings: vec![NativeFulltextPosting {
                        entity_id: 100,
                        term_count: 5,
                    }],
                },
            ],
        };
        let encoded = encode_native_fulltext_frame(&frame);
        assert_eq!(&encoded[0..4], b"RDFT");
        let decoded = decode_native_fulltext_frame(&encoded).unwrap();
        assert_eq!(decoded, frame);
        assert_eq!(encode_native_fulltext_frame(&decoded), encoded);
    }

    #[test]
    fn rddp_round_trips_and_validates_total() {
        let frame = NativeDocPathValueFrame {
            collection: "docs".into(),
            docs: vec![
                NativeDocPathValue {
                    entity_id: 9,
                    entries: vec![
                        NativeDocPathValueEntry {
                            path: "a.b".into(),
                            value: "1".into(),
                        },
                        NativeDocPathValueEntry {
                            path: "a.c".into(),
                            value: "2".into(),
                        },
                    ],
                },
                NativeDocPathValue {
                    entity_id: 10,
                    entries: vec![NativeDocPathValueEntry {
                        path: "x".into(),
                        value: "y".into(),
                    }],
                },
            ],
        };
        let encoded = encode_native_doc_pathvalue_frame(&frame);
        assert_eq!(&encoded[0..4], b"RDDP");
        let decoded = decode_native_doc_pathvalue_frame(&encoded).unwrap();
        assert_eq!(decoded, frame);
        assert_eq!(encode_native_doc_pathvalue_frame(&decoded), encoded);
    }

    #[test]
    fn rddp_entity_id_is_fixed_u64() {
        // A single document with no entries: after "RDDP", a 4-byte length-0
        // collection string, doc_count=1, total_entries=0, the next 8 bytes are
        // the entity id as a little-endian u64 — NOT a length-prefixed string.
        let frame = NativeDocPathValueFrame {
            collection: String::new(),
            docs: vec![NativeDocPathValue {
                entity_id: 0xDEAD_BEEF_CAFE_F00D,
                entries: vec![],
            }],
        };
        let encoded = encode_native_doc_pathvalue_frame(&frame);
        // 4 magic + 4 collection-len(0) + 4 doc_count + 4 total_entries = 16
        let id_off = 16;
        assert_eq!(
            &encoded[id_off..id_off + 8],
            &0xDEAD_BEEF_CAFE_F00Du64.to_le_bytes()
        );
        let decoded = decode_native_doc_pathvalue_frame(&encoded).unwrap();
        assert_eq!(decoded.docs[0].entity_id, 0xDEAD_BEEF_CAFE_F00D);
    }

    #[test]
    fn native_artifacts_reject_bad_magic() {
        assert!(matches!(
            decode_native_graph_adjacency_frame(&[0u8; 8]),
            Err(NativeArtifactFrameError::InvalidMagic { .. })
        ));
        assert!(matches!(
            decode_native_fulltext_frame(&[0u8; 12]),
            Err(NativeArtifactFrameError::InvalidMagic { .. })
        ));
        assert!(matches!(
            decode_native_doc_pathvalue_frame(&[0u8; 12]),
            Err(NativeArtifactFrameError::InvalidMagic { .. })
        ));
    }

    #[test]
    fn rddp_rejects_total_entries_mismatch() {
        let frame = NativeDocPathValueFrame {
            collection: "c".into(),
            docs: vec![NativeDocPathValue {
                entity_id: 1,
                entries: vec![NativeDocPathValueEntry {
                    path: "p".into(),
                    value: "v".into(),
                }],
            }],
        };
        let mut encoded = encode_native_doc_pathvalue_frame(&frame);
        // Corrupt the total_entries word (offset 4 magic + 4 collection-len + 1
        // collection byte + 4 doc_count = 13).
        let total_off = 4 + 4 + frame.collection.len() + 4;
        encoded[total_off..total_off + 4].copy_from_slice(&9u32.to_le_bytes());
        assert!(matches!(
            decode_native_doc_pathvalue_frame(&encoded),
            Err(NativeArtifactFrameError::EntryCountMismatch { .. })
        ));
    }
}
