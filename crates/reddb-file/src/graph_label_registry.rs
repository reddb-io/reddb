//! Persisted graph label-registry frame.
//!
//! The storage engine owns label allocation, legacy seeds, and namespace
//! semantics. This module owns only the durable byte contract.

pub const GRAPH_LABEL_REGISTRY_MAX_LABEL_LEN: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphLabelRegistryEntry {
    pub id: u32,
    pub namespace: u8,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphLabelRegistryFrameError {
    LabelTooLong { len: usize, max: usize },
    Malformed { offset: usize, reason: &'static str },
}

impl std::fmt::Display for GraphLabelRegistryFrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LabelTooLong { len, max } => {
                write!(f, "graph label too long: {len} bytes (max {max})")
            }
            Self::Malformed { offset, reason } => {
                write!(
                    f,
                    "malformed graph label registry at offset {offset}: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for GraphLabelRegistryFrameError {}

pub fn encode_graph_label_registry_frame(
    entries: &[GraphLabelRegistryEntry],
) -> Result<Vec<u8>, GraphLabelRegistryFrameError> {
    let mut buf = Vec::with_capacity(4 + entries.len() * 16);
    buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
        let bytes = entry.label.as_bytes();
        if bytes.len() > GRAPH_LABEL_REGISTRY_MAX_LABEL_LEN {
            return Err(GraphLabelRegistryFrameError::LabelTooLong {
                len: bytes.len(),
                max: GRAPH_LABEL_REGISTRY_MAX_LABEL_LEN,
            });
        }
        let len =
            u16::try_from(bytes.len()).map_err(|_| GraphLabelRegistryFrameError::LabelTooLong {
                len: bytes.len(),
                max: GRAPH_LABEL_REGISTRY_MAX_LABEL_LEN,
            })?;

        buf.extend_from_slice(&entry.id.to_le_bytes());
        buf.push(entry.namespace);
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(bytes);
    }
    Ok(buf)
}

pub fn decode_graph_label_registry_frame(
    data: &[u8],
) -> Result<Vec<GraphLabelRegistryEntry>, GraphLabelRegistryFrameError> {
    if data.len() < 4 {
        return Err(GraphLabelRegistryFrameError::Malformed {
            offset: 0,
            reason: "header truncated",
        });
    }

    let count = u32::from_le_bytes(data[0..4].try_into().expect("header length checked")) as usize;
    let mut off = 4;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if data.len() < off + 7 {
            return Err(GraphLabelRegistryFrameError::Malformed {
                offset: off,
                reason: "entry header truncated",
            });
        }
        let id = u32::from_le_bytes(data[off..off + 4].try_into().expect("entry length checked"));
        let namespace = data[off + 4];
        let len = u16::from_le_bytes(
            data[off + 5..off + 7]
                .try_into()
                .expect("entry length checked"),
        ) as usize;
        off += 7;
        if data.len() < off + len {
            return Err(GraphLabelRegistryFrameError::Malformed {
                offset: off,
                reason: "label bytes truncated",
            });
        }
        let label = std::str::from_utf8(&data[off..off + len])
            .map_err(|_| GraphLabelRegistryFrameError::Malformed {
                offset: off,
                reason: "label not utf8",
            })?
            .to_string();
        entries.push(GraphLabelRegistryEntry {
            id,
            namespace,
            label,
        });
        off += len;
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_frame_round_trips() {
        let entries = vec![
            GraphLabelRegistryEntry {
                id: 1,
                namespace: 0,
                label: "host".to_string(),
            },
            GraphLabelRegistryEntry {
                id: 64,
                namespace: 1,
                label: "purchased".to_string(),
            },
        ];
        let encoded = encode_graph_label_registry_frame(&entries).unwrap();
        let decoded = decode_graph_label_registry_frame(&encoded).unwrap();
        assert_eq!(decoded, entries);
    }

    #[test]
    fn registry_frame_rejects_truncated_input() {
        assert!(matches!(
            decode_graph_label_registry_frame(&[1, 2, 3]),
            Err(GraphLabelRegistryFrameError::Malformed { .. })
        ));
        let encoded = encode_graph_label_registry_frame(&[GraphLabelRegistryEntry {
            id: 64,
            namespace: 0,
            label: "abcd".to_string(),
        }])
        .unwrap();
        assert!(matches!(
            decode_graph_label_registry_frame(&encoded[..encoded.len() - 1]),
            Err(GraphLabelRegistryFrameError::Malformed { .. })
        ));
    }
}
