//! Persisted graph-table index frame.
//!
//! The server owns the bidirectional in-memory indexes and locking. This
//! module owns the durable byte contract for node-to-row mappings.

use crate::{decode_graph_table_ref, encode_graph_table_ref, GraphTableRef, GRAPH_TABLE_REF_SIZE};

pub const GRAPH_TABLE_INDEX_ENTRY_HEADER_LEN: usize = 2;
pub const GRAPH_TABLE_INDEX_HEADER_LEN: usize = 4;
pub const GRAPH_TABLE_INDEX_MAX_NODE_ID_LEN: usize = u16::MAX as usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphTableIndexEntry {
    pub node_id: String,
    pub table_ref: GraphTableRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphTableIndexFrameError {
    TooManyEntries { len: usize, max: usize },
    NodeIdTooLong { len: usize, max: usize },
    Malformed { offset: usize, reason: &'static str },
}

impl std::fmt::Display for GraphTableIndexFrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyEntries { len, max } => {
                write!(f, "too many graph-table index entries: {len} (max {max})")
            }
            Self::NodeIdTooLong { len, max } => {
                write!(f, "graph-table node id too long: {len} bytes (max {max})")
            }
            Self::Malformed { offset, reason } => {
                write!(
                    f,
                    "malformed graph-table index at offset {offset}: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for GraphTableIndexFrameError {}

pub fn encode_graph_table_index_frame(
    entries: &[GraphTableIndexEntry],
) -> Result<Vec<u8>, GraphTableIndexFrameError> {
    let entry_count =
        u32::try_from(entries.len()).map_err(|_| GraphTableIndexFrameError::TooManyEntries {
            len: entries.len(),
            max: u32::MAX as usize,
        })?;

    let mut buf = Vec::with_capacity(
        GRAPH_TABLE_INDEX_HEADER_LEN
            + entries.len() * (GRAPH_TABLE_INDEX_ENTRY_HEADER_LEN + GRAPH_TABLE_REF_SIZE),
    );
    buf.extend_from_slice(&entry_count.to_le_bytes());

    for entry in entries {
        let id_bytes = entry.node_id.as_bytes();
        let id_len = u16::try_from(id_bytes.len()).map_err(|_| {
            GraphTableIndexFrameError::NodeIdTooLong {
                len: id_bytes.len(),
                max: GRAPH_TABLE_INDEX_MAX_NODE_ID_LEN,
            }
        })?;

        buf.extend_from_slice(&id_len.to_le_bytes());
        buf.extend_from_slice(id_bytes);
        buf.extend_from_slice(&encode_graph_table_ref(entry.table_ref));
    }

    Ok(buf)
}

pub fn decode_graph_table_index_frame(
    data: &[u8],
) -> Result<Vec<GraphTableIndexEntry>, GraphTableIndexFrameError> {
    if data.len() < GRAPH_TABLE_INDEX_HEADER_LEN {
        return Err(GraphTableIndexFrameError::Malformed {
            offset: 0,
            reason: "header truncated",
        });
    }

    let count = u32::from_le_bytes(
        data[0..GRAPH_TABLE_INDEX_HEADER_LEN]
            .try_into()
            .expect("header length checked"),
    ) as usize;
    let mut offset = GRAPH_TABLE_INDEX_HEADER_LEN;
    let mut entries = Vec::with_capacity(count);

    for _ in 0..count {
        if data.len() < offset + GRAPH_TABLE_INDEX_ENTRY_HEADER_LEN {
            return Err(GraphTableIndexFrameError::Malformed {
                offset,
                reason: "node id length truncated",
            });
        }
        let id_len = u16::from_le_bytes(
            data[offset..offset + GRAPH_TABLE_INDEX_ENTRY_HEADER_LEN]
                .try_into()
                .expect("entry header length checked"),
        ) as usize;
        offset += GRAPH_TABLE_INDEX_ENTRY_HEADER_LEN;

        if data.len() < offset + id_len {
            return Err(GraphTableIndexFrameError::Malformed {
                offset,
                reason: "node id bytes truncated",
            });
        }
        let node_id = std::str::from_utf8(&data[offset..offset + id_len])
            .map_err(|_| GraphTableIndexFrameError::Malformed {
                offset,
                reason: "node id not utf8",
            })?
            .to_string();
        offset += id_len;

        if data.len() < offset + GRAPH_TABLE_REF_SIZE {
            return Err(GraphTableIndexFrameError::Malformed {
                offset,
                reason: "table ref truncated",
            });
        }
        let table_ref = decode_graph_table_ref(&data[offset..offset + GRAPH_TABLE_REF_SIZE])
            .ok_or(GraphTableIndexFrameError::Malformed {
                offset,
                reason: "invalid table ref",
            })?;
        offset += GRAPH_TABLE_REF_SIZE;

        entries.push(GraphTableIndexEntry { node_id, table_ref });
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_table_index_frame_round_trips() {
        let entries = vec![
            GraphTableIndexEntry {
                node_id: "node:a".to_string(),
                table_ref: GraphTableRef {
                    table_id: 1,
                    row_id: 100,
                },
            },
            GraphTableIndexEntry {
                node_id: "node:b".to_string(),
                table_ref: GraphTableRef {
                    table_id: 2,
                    row_id: 200,
                },
            },
        ];

        let encoded = encode_graph_table_index_frame(&entries).unwrap();
        let decoded = decode_graph_table_index_frame(&encoded).unwrap();
        assert_eq!(decoded, entries);
    }

    #[test]
    fn graph_table_index_frame_rejects_truncated_input() {
        assert!(matches!(
            decode_graph_table_index_frame(&[1, 2, 3]),
            Err(GraphTableIndexFrameError::Malformed { .. })
        ));

        let encoded = encode_graph_table_index_frame(&[GraphTableIndexEntry {
            node_id: "node:a".to_string(),
            table_ref: GraphTableRef {
                table_id: 1,
                row_id: 100,
            },
        }])
        .unwrap();
        assert!(matches!(
            decode_graph_table_index_frame(&encoded[..encoded.len() - 1]),
            Err(GraphTableIndexFrameError::Malformed { .. })
        ));
    }

    #[test]
    fn graph_table_index_frame_rejects_invalid_utf8_node_id() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.push(0xff);
        bytes.extend_from_slice(&encode_graph_table_ref(GraphTableRef {
            table_id: 1,
            row_id: 100,
        }));

        assert!(matches!(
            decode_graph_table_index_frame(&bytes),
            Err(GraphTableIndexFrameError::Malformed { .. })
        ));
    }
}
