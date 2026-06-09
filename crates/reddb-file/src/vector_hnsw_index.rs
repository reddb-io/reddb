//! Persisted HNSW vector-index payload codec.
//!
//! The vector engine owns graph construction, search, and layer assignment.
//! This module owns only the durable byte layout of a serialized HNSW index:
//! magic + version, the flat config block, index state, and the per-node
//! record stream. Distance-metric semantics stay in the engine, which maps its
//! `DistanceMetric` enum to/from the on-disk `metric` byte.
//!
//! Byte layout (little-endian) — DO NOT change order/width; these bytes live in
//! existing `.rdb` artifacts:
//!
//! ```text
//! "HNSW"                 4 bytes magic
//! version                u32 (== 1)
//! dimension              u32
//! m                      u32
//! m_max0                 u32
//! ef_construction        u32
//! ef_search              u32
//! ml                     f64
//! metric                 u8  (engine-defined: 0=L2, 1=Cosine, 2=InnerProduct)
//! max_layer              u32
//! entry_point            u64 (u64::MAX encodes "no entry point")
//! node_count             u64
//! repeated node_count times:
//!   id                   u64
//!   node_max_layer       u32
//!   vector               f32 * dimension   (no length prefix)
//!   repeated (node_max_layer + 1) times:
//!     conn_count         u32
//!     conn               u64 * conn_count
//! ```

/// Magic prefix for a serialized HNSW index payload.
pub const HNSW_INDEX_MAGIC: [u8; 4] = *b"HNSW";
/// Only supported on-disk version.
pub const HNSW_INDEX_VERSION_V1: u32 = 1;
/// Sentinel `entry_point` value meaning "no entry point".
pub const HNSW_INDEX_NO_ENTRY_POINT: u64 = u64::MAX;
/// Length of the fixed header preceding the node stream.
pub const HNSW_INDEX_HEADER_LEN: usize = 4  // magic
    + 4  // version
    + 4  // dimension
    + 4  // m
    + 4  // m_max0
    + 4  // ef_construction
    + 4  // ef_search
    + 8  // ml
    + 1  // metric
    + 4  // max_layer
    + 8  // entry_point
    + 8; // node_count

/// A decoded HNSW node record. The vector length is implied by the frame
/// `dimension`; `connections` carries `node_max_layer + 1` adjacency lists.
#[derive(Debug, Clone, PartialEq)]
pub struct HnswNodeFrame {
    pub id: u64,
    pub max_layer: u32,
    pub vector: Vec<f32>,
    pub connections: Vec<Vec<u64>>,
}

/// A decoded HNSW index payload. Plain data only — the engine owns algorithm
/// state (RNG, derived `next_id`) and reconstructs it from these fields.
#[derive(Debug, Clone, PartialEq)]
pub struct HnswIndexFrame {
    pub dimension: u32,
    pub m: u32,
    pub m_max0: u32,
    pub ef_construction: u32,
    pub ef_search: u32,
    pub ml: f64,
    pub metric: u8,
    pub max_layer: u32,
    /// `None` is encoded on disk as [`HNSW_INDEX_NO_ENTRY_POINT`].
    pub entry_point: Option<u64>,
    pub nodes: Vec<HnswNodeFrame>,
}

/// Errors decoding an HNSW index payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HnswIndexFrameError {
    TooShort,
    InvalidMagic,
    UnsupportedVersion(u32),
    Truncated { offset: usize, reason: &'static str },
}

impl std::fmt::Display for HnswIndexFrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort => write!(f, "Data too short"),
            Self::InvalidMagic => write!(f, "Invalid magic number"),
            Self::UnsupportedVersion(version) => write!(f, "Unsupported version: {version}"),
            Self::Truncated { offset, reason } => {
                write!(f, "truncated HNSW payload at offset {offset}: {reason}")
            }
        }
    }
}

impl std::error::Error for HnswIndexFrameError {}

/// Serialize an HNSW index payload to bytes.
pub fn encode_hnsw_index_frame(frame: &HnswIndexFrame) -> Vec<u8> {
    let mut bytes = Vec::new();

    bytes.extend_from_slice(&HNSW_INDEX_MAGIC);
    bytes.extend_from_slice(&HNSW_INDEX_VERSION_V1.to_le_bytes());

    bytes.extend_from_slice(&frame.dimension.to_le_bytes());
    bytes.extend_from_slice(&frame.m.to_le_bytes());
    bytes.extend_from_slice(&frame.m_max0.to_le_bytes());
    bytes.extend_from_slice(&frame.ef_construction.to_le_bytes());
    bytes.extend_from_slice(&frame.ef_search.to_le_bytes());
    bytes.extend_from_slice(&frame.ml.to_le_bytes());
    bytes.push(frame.metric);

    bytes.extend_from_slice(&frame.max_layer.to_le_bytes());
    bytes.extend_from_slice(
        &frame
            .entry_point
            .unwrap_or(HNSW_INDEX_NO_ENTRY_POINT)
            .to_le_bytes(),
    );

    bytes.extend_from_slice(&(frame.nodes.len() as u64).to_le_bytes());

    for node in &frame.nodes {
        bytes.extend_from_slice(&node.id.to_le_bytes());
        bytes.extend_from_slice(&node.max_layer.to_le_bytes());

        for &val in &node.vector {
            bytes.extend_from_slice(&val.to_le_bytes());
        }

        for conns in &node.connections {
            bytes.extend_from_slice(&(conns.len() as u32).to_le_bytes());
            for &conn in conns {
                bytes.extend_from_slice(&conn.to_le_bytes());
            }
        }
    }

    bytes
}

/// Deserialize an HNSW index payload from bytes.
pub fn decode_hnsw_index_frame(bytes: &[u8]) -> Result<HnswIndexFrame, HnswIndexFrameError> {
    if bytes.len() < 8 {
        return Err(HnswIndexFrameError::TooShort);
    }
    if bytes[0..4] != HNSW_INDEX_MAGIC {
        return Err(HnswIndexFrameError::InvalidMagic);
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().expect("u32 length checked"));
    if version != HNSW_INDEX_VERSION_V1 {
        return Err(HnswIndexFrameError::UnsupportedVersion(version));
    }

    let mut pos = 8;
    let dimension = read_u32(bytes, &mut pos, "dimension")?;
    let m = read_u32(bytes, &mut pos, "m")?;
    let m_max0 = read_u32(bytes, &mut pos, "m_max0")?;
    let ef_construction = read_u32(bytes, &mut pos, "ef_construction")?;
    let ef_search = read_u32(bytes, &mut pos, "ef_search")?;
    let ml = read_f64(bytes, &mut pos, "ml")?;
    let metric = read_u8(bytes, &mut pos, "metric")?;

    let max_layer = read_u32(bytes, &mut pos, "max_layer")?;
    let ep_value = read_u64(bytes, &mut pos, "entry_point")?;
    let entry_point = if ep_value == HNSW_INDEX_NO_ENTRY_POINT {
        None
    } else {
        Some(ep_value)
    };

    let node_count = read_u64(bytes, &mut pos, "node_count")? as usize;
    let dim = dimension as usize;
    let mut nodes = Vec::with_capacity(node_count);
    for _ in 0..node_count {
        let id = read_u64(bytes, &mut pos, "node id")?;
        let node_max_layer = read_u32(bytes, &mut pos, "node max_layer")?;

        let mut vector = Vec::with_capacity(dim);
        for _ in 0..dim {
            vector.push(read_f32(bytes, &mut pos, "node vector")?);
        }

        let layer_count = node_max_layer as usize + 1;
        let mut connections = Vec::with_capacity(layer_count);
        for _ in 0..layer_count {
            let conn_count = read_u32(bytes, &mut pos, "connection count")? as usize;
            let mut conn_list = Vec::with_capacity(conn_count);
            for _ in 0..conn_count {
                conn_list.push(read_u64(bytes, &mut pos, "connection")?);
            }
            connections.push(conn_list);
        }

        nodes.push(HnswNodeFrame {
            id,
            max_layer: node_max_layer,
            vector,
            connections,
        });
    }

    Ok(HnswIndexFrame {
        dimension,
        m,
        m_max0,
        ef_construction,
        ef_search,
        ml,
        metric,
        max_layer,
        entry_point,
        nodes,
    })
}

fn read_u8(bytes: &[u8], pos: &mut usize, reason: &'static str) -> Result<u8, HnswIndexFrameError> {
    if *pos + 1 > bytes.len() {
        return Err(HnswIndexFrameError::Truncated {
            offset: *pos,
            reason,
        });
    }
    let value = bytes[*pos];
    *pos += 1;
    Ok(value)
}

fn read_u32(
    bytes: &[u8],
    pos: &mut usize,
    reason: &'static str,
) -> Result<u32, HnswIndexFrameError> {
    if *pos + 4 > bytes.len() {
        return Err(HnswIndexFrameError::Truncated {
            offset: *pos,
            reason,
        });
    }
    let value = u32::from_le_bytes(bytes[*pos..*pos + 4].try_into().expect("u32 length checked"));
    *pos += 4;
    Ok(value)
}

fn read_u64(
    bytes: &[u8],
    pos: &mut usize,
    reason: &'static str,
) -> Result<u64, HnswIndexFrameError> {
    if *pos + 8 > bytes.len() {
        return Err(HnswIndexFrameError::Truncated {
            offset: *pos,
            reason,
        });
    }
    let value = u64::from_le_bytes(bytes[*pos..*pos + 8].try_into().expect("u64 length checked"));
    *pos += 8;
    Ok(value)
}

fn read_f32(
    bytes: &[u8],
    pos: &mut usize,
    reason: &'static str,
) -> Result<f32, HnswIndexFrameError> {
    if *pos + 4 > bytes.len() {
        return Err(HnswIndexFrameError::Truncated {
            offset: *pos,
            reason,
        });
    }
    let value = f32::from_le_bytes(bytes[*pos..*pos + 4].try_into().expect("f32 length checked"));
    *pos += 4;
    Ok(value)
}

fn read_f64(
    bytes: &[u8],
    pos: &mut usize,
    reason: &'static str,
) -> Result<f64, HnswIndexFrameError> {
    if *pos + 8 > bytes.len() {
        return Err(HnswIndexFrameError::Truncated {
            offset: *pos,
            reason,
        });
    }
    let value = f64::from_le_bytes(bytes[*pos..*pos + 8].try_into().expect("f64 length checked"));
    *pos += 8;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_frame() -> HnswIndexFrame {
        HnswIndexFrame {
            dimension: 3,
            m: 16,
            m_max0: 32,
            ef_construction: 100,
            ef_search: 50,
            ml: 0.360_673_760_222_104_4,
            metric: 1,
            max_layer: 2,
            entry_point: Some(7),
            nodes: vec![
                HnswNodeFrame {
                    id: 7,
                    max_layer: 2,
                    vector: vec![1.0, 2.0, 3.0],
                    connections: vec![vec![1, 2], vec![2], vec![]],
                },
                HnswNodeFrame {
                    id: 1,
                    max_layer: 0,
                    vector: vec![-1.5, 0.0, 4.25],
                    connections: vec![vec![7]],
                },
            ],
        }
    }

    #[test]
    fn hnsw_index_frame_round_trips() {
        let frame = sample_frame();
        let encoded = encode_hnsw_index_frame(&frame);
        let decoded = decode_hnsw_index_frame(&encoded).unwrap();
        assert_eq!(decoded, frame);
        // Re-encode must be byte-identical.
        assert_eq!(encode_hnsw_index_frame(&decoded), encoded);
    }

    #[test]
    fn hnsw_index_frame_pins_byte_layout() {
        let frame = sample_frame();
        let encoded = encode_hnsw_index_frame(&frame);
        // magic + version
        assert_eq!(&encoded[0..4], b"HNSW");
        assert_eq!(&encoded[4..8], &1u32.to_le_bytes());
        // dimension
        assert_eq!(&encoded[8..12], &3u32.to_le_bytes());
        // metric byte sits after the 5 u32 config words + the f64 ml.
        let metric_off = 8 + 4 * 5 + 8;
        assert_eq!(encoded[metric_off], 1);
    }

    #[test]
    fn hnsw_index_frame_encodes_missing_entry_point_as_sentinel() {
        let mut frame = sample_frame();
        frame.entry_point = None;
        frame.nodes.clear();
        let encoded = encode_hnsw_index_frame(&frame);
        let ep_off = HNSW_INDEX_HEADER_LEN - 8 - 8; // entry_point precedes node_count
        assert_eq!(
            &encoded[ep_off..ep_off + 8],
            &HNSW_INDEX_NO_ENTRY_POINT.to_le_bytes()
        );
        let decoded = decode_hnsw_index_frame(&encoded).unwrap();
        assert_eq!(decoded.entry_point, None);
    }

    #[test]
    fn hnsw_index_frame_rejects_bad_input() {
        assert_eq!(
            decode_hnsw_index_frame(&[0u8; 4]),
            Err(HnswIndexFrameError::TooShort)
        );
        let mut bad_magic = encode_hnsw_index_frame(&sample_frame());
        bad_magic[0] = b'X';
        assert_eq!(
            decode_hnsw_index_frame(&bad_magic),
            Err(HnswIndexFrameError::InvalidMagic)
        );
        let mut bad_version = encode_hnsw_index_frame(&sample_frame());
        bad_version[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            decode_hnsw_index_frame(&bad_version),
            Err(HnswIndexFrameError::UnsupportedVersion(2))
        );
        let encoded = encode_hnsw_index_frame(&sample_frame());
        assert!(matches!(
            decode_hnsw_index_frame(&encoded[..encoded.len() - 1]),
            Err(HnswIndexFrameError::Truncated { .. })
        ));
    }
}
