//! Persisted graph-store envelope.
//!
//! The graph engine owns pages, registry semantics, and index rebuilds. This
//! module owns the durable envelope around graph pages.

pub const GRAPH_STORE_MAGIC: [u8; 4] = *b"RBGR";
pub const GRAPH_STORE_VERSION_V1: u32 = 1;
pub const GRAPH_STORE_VERSION_V2: u32 = 2;
pub const GRAPH_STORE_HEADER_LEN: usize = 24;
pub const GRAPH_STORE_REGISTRY_LEN_BYTES: usize = 4;
pub const GRAPH_STORE_PAGE_COUNT_BYTES: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphStoreFrame {
    pub version: u32,
    pub node_count: u64,
    pub edge_count: u64,
    pub registry_bytes: Option<Vec<u8>>,
    pub node_pages: Vec<Vec<u8>>,
    pub edge_pages: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphStoreFrameError {
    UnsupportedVersion(u32),
    RegistryRequired,
    RegistryForbidden,
    TooManyPages { len: usize, max: usize },
    Malformed { offset: usize, reason: &'static str },
}

impl std::fmt::Display for GraphStoreFrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported graph store frame version {version}")
            }
            Self::RegistryRequired => write!(f, "graph store v2 frame requires registry bytes"),
            Self::RegistryForbidden => {
                write!(f, "graph store v1 frame cannot carry registry bytes")
            }
            Self::TooManyPages { len, max } => {
                write!(f, "too many graph store pages: {len} (max {max})")
            }
            Self::Malformed { offset, reason } => {
                write!(
                    f,
                    "malformed graph store frame at offset {offset}: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for GraphStoreFrameError {}

pub fn encode_graph_store_frame(frame: &GraphStoreFrame) -> Result<Vec<u8>, GraphStoreFrameError> {
    match frame.version {
        GRAPH_STORE_VERSION_V1 if frame.registry_bytes.is_some() => {
            return Err(GraphStoreFrameError::RegistryForbidden);
        }
        GRAPH_STORE_VERSION_V1 => {}
        GRAPH_STORE_VERSION_V2 if frame.registry_bytes.is_none() => {
            return Err(GraphStoreFrameError::RegistryRequired);
        }
        GRAPH_STORE_VERSION_V2 => {}
        version => return Err(GraphStoreFrameError::UnsupportedVersion(version)),
    }

    let node_page_count =
        u32::try_from(frame.node_pages.len()).map_err(|_| GraphStoreFrameError::TooManyPages {
            len: frame.node_pages.len(),
            max: u32::MAX as usize,
        })?;
    let edge_page_count =
        u32::try_from(frame.edge_pages.len()).map_err(|_| GraphStoreFrameError::TooManyPages {
            len: frame.edge_pages.len(),
            max: u32::MAX as usize,
        })?;

    let registry_len = frame.registry_bytes.as_ref().map(Vec::len).unwrap_or(0);
    let page_bytes: usize = frame
        .node_pages
        .iter()
        .chain(frame.edge_pages.iter())
        .map(Vec::len)
        .sum();
    let mut buf = Vec::with_capacity(
        GRAPH_STORE_HEADER_LEN
            + if frame.version == GRAPH_STORE_VERSION_V2 {
                GRAPH_STORE_REGISTRY_LEN_BYTES + registry_len
            } else {
                0
            }
            + GRAPH_STORE_PAGE_COUNT_BYTES * 2
            + page_bytes,
    );

    buf.extend_from_slice(&GRAPH_STORE_MAGIC);
    buf.extend_from_slice(&frame.version.to_le_bytes());
    buf.extend_from_slice(&frame.node_count.to_le_bytes());
    buf.extend_from_slice(&frame.edge_count.to_le_bytes());

    if frame.version == GRAPH_STORE_VERSION_V2 {
        let registry = frame
            .registry_bytes
            .as_ref()
            .expect("v2 registry checked above");
        let registry_len =
            u32::try_from(registry.len()).map_err(|_| GraphStoreFrameError::Malformed {
                offset: GRAPH_STORE_HEADER_LEN,
                reason: "registry too large",
            })?;
        buf.extend_from_slice(&registry_len.to_le_bytes());
        buf.extend_from_slice(registry);
    }

    buf.extend_from_slice(&node_page_count.to_le_bytes());
    for page in &frame.node_pages {
        buf.extend_from_slice(page);
    }
    buf.extend_from_slice(&edge_page_count.to_le_bytes());
    for page in &frame.edge_pages {
        buf.extend_from_slice(page);
    }

    Ok(buf)
}

pub fn decode_graph_store_frame(
    data: &[u8],
    page_size: usize,
) -> Result<GraphStoreFrame, GraphStoreFrameError> {
    if data.len() < GRAPH_STORE_HEADER_LEN {
        return Err(GraphStoreFrameError::Malformed {
            offset: 0,
            reason: "header truncated",
        });
    }
    if data[0..4] != GRAPH_STORE_MAGIC {
        return Err(GraphStoreFrameError::Malformed {
            offset: 0,
            reason: "invalid magic",
        });
    }

    let version = read_u32(data, 4, "version")?;
    let node_count = read_u64(data, 8, "node count")?;
    let edge_count = read_u64(data, 16, "edge count")?;
    let mut offset = GRAPH_STORE_HEADER_LEN;

    let registry_bytes = match version {
        GRAPH_STORE_VERSION_V1 => None,
        GRAPH_STORE_VERSION_V2 => {
            let len = read_u32(data, offset, "registry length")? as usize;
            offset += GRAPH_STORE_REGISTRY_LEN_BYTES;
            if data.len() < offset + len {
                return Err(GraphStoreFrameError::Malformed {
                    offset,
                    reason: "registry bytes truncated",
                });
            }
            let bytes = data[offset..offset + len].to_vec();
            offset += len;
            Some(bytes)
        }
        version => return Err(GraphStoreFrameError::UnsupportedVersion(version)),
    };

    let node_page_count = read_u32(data, offset, "node page count")? as usize;
    offset += GRAPH_STORE_PAGE_COUNT_BYTES;
    let (node_pages, next_offset) = read_pages(data, offset, node_page_count, page_size, "node")?;
    offset = next_offset;

    let edge_page_count = read_u32(data, offset, "edge page count")? as usize;
    offset += GRAPH_STORE_PAGE_COUNT_BYTES;
    let (edge_pages, _next_offset) = read_pages(data, offset, edge_page_count, page_size, "edge")?;

    Ok(GraphStoreFrame {
        version,
        node_count,
        edge_count,
        registry_bytes,
        node_pages,
        edge_pages,
    })
}

fn read_pages(
    data: &[u8],
    mut offset: usize,
    count: usize,
    page_size: usize,
    label: &'static str,
) -> Result<(Vec<Vec<u8>>, usize), GraphStoreFrameError> {
    let mut pages = Vec::with_capacity(count);
    for _ in 0..count {
        if data.len() < offset + page_size {
            return Err(GraphStoreFrameError::Malformed {
                offset,
                reason: if label == "node" {
                    "node pages truncated"
                } else {
                    "edge pages truncated"
                },
            });
        }
        pages.push(data[offset..offset + page_size].to_vec());
        offset += page_size;
    }
    Ok((pages, offset))
}

fn read_u32(data: &[u8], offset: usize, reason: &'static str) -> Result<u32, GraphStoreFrameError> {
    if data.len() < offset + 4 {
        return Err(GraphStoreFrameError::Malformed { offset, reason });
    }
    Ok(u32::from_le_bytes(
        data[offset..offset + 4]
            .try_into()
            .expect("u32 length checked"),
    ))
}

fn read_u64(data: &[u8], offset: usize, reason: &'static str) -> Result<u64, GraphStoreFrameError> {
    if data.len() < offset + 8 {
        return Err(GraphStoreFrameError::Malformed { offset, reason });
    }
    Ok(u64::from_le_bytes(
        data[offset..offset + 8]
            .try_into()
            .expect("u64 length checked"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_store_v2_frame_round_trips() {
        let frame = GraphStoreFrame {
            version: GRAPH_STORE_VERSION_V2,
            node_count: 2,
            edge_count: 1,
            registry_bytes: Some(vec![1, 2, 3]),
            node_pages: vec![vec![10; 8], vec![11; 8]],
            edge_pages: vec![vec![20; 8]],
        };

        let encoded = encode_graph_store_frame(&frame).unwrap();
        let decoded = decode_graph_store_frame(&encoded, 8).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn graph_store_v1_frame_has_no_registry() {
        let frame = GraphStoreFrame {
            version: GRAPH_STORE_VERSION_V1,
            node_count: 1,
            edge_count: 1,
            registry_bytes: None,
            node_pages: vec![vec![10; 8]],
            edge_pages: vec![vec![20; 8]],
        };

        let encoded = encode_graph_store_frame(&frame).unwrap();
        let decoded = decode_graph_store_frame(&encoded, 8).unwrap();
        assert_eq!(decoded.registry_bytes, None);
        assert_eq!(decoded.node_pages.len(), 1);
        assert_eq!(decoded.edge_pages.len(), 1);
    }

    #[test]
    fn graph_store_frame_rejects_truncated_input() {
        assert!(matches!(
            decode_graph_store_frame(&[1, 2, 3], 8),
            Err(GraphStoreFrameError::Malformed { .. })
        ));

        let frame = GraphStoreFrame {
            version: GRAPH_STORE_VERSION_V2,
            node_count: 1,
            edge_count: 0,
            registry_bytes: Some(vec![]),
            node_pages: vec![vec![10; 8]],
            edge_pages: vec![],
        };
        let encoded = encode_graph_store_frame(&frame).unwrap();
        assert!(matches!(
            decode_graph_store_frame(&encoded[..encoded.len() - 1], 8),
            Err(GraphStoreFrameError::Malformed { .. })
        ));
    }
}
