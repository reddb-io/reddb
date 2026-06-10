//! On-disk codec for the `HNSW` vector-index payload.
//!
//! `reddb-file` owns the byte layout of the persisted HNSW artifact: the
//! `HNSW` magic, version `1`, the config block, and the per-node graph state.
//! The graph-construction algorithm itself stays in the server engine; only the
//! format-faithful encode/decode lives here so the bytes have a single
//! authority (ADR 0046).
//!
//! Layout (all integers little-endian):
//! ```text
//! magic   : b"HNSW"            (4 bytes)
//! version : u32 == 1           (4 bytes)
//! config  : dimension u32, m u32, m_max0 u32, ef_construction u32,
//!           ef_search u32, ml f64, metric u8
//! state   : max_layer u32, entry_point u64 (u64::MAX == None)
//! nodes   : count u64, then `count` node records:
//!             id u32-pair? -> id u64, max_layer u32,
//!             vector: `dimension` f32 values,
//!             per layer 0..=max_layer: conn_count u32, then conn_count u64 ids
//! ```

/// Magic prefixing a persisted HNSW index payload.
pub const HNSW_INDEX_MAGIC: &[u8; 4] = b"HNSW";
/// On-disk format version owned by `reddb-file`.
pub const HNSW_INDEX_VERSION: u32 = 1;

/// Plain, engine-agnostic view of a persisted HNSW node.
#[derive(Debug, Clone, PartialEq)]
pub struct HnswNodeLayout {
    /// Node identifier.
    pub id: u64,
    /// Highest layer this node participates in.
    pub max_layer: usize,
    /// The node's vector (length equals the index `dimension`).
    pub vector: Vec<f32>,
    /// Neighbour ids per layer (`max_layer + 1` layers).
    pub connections: Vec<Vec<u64>>,
}

/// Plain, engine-agnostic view of a persisted HNSW index.
#[derive(Debug, Clone, PartialEq)]
pub struct HnswIndexLayout {
    /// Vector dimension.
    pub dimension: usize,
    /// Max connections per node above layer 0.
    pub m: usize,
    /// Max connections at layer 0.
    pub m_max0: usize,
    /// Construction candidate-list size.
    pub ef_construction: usize,
    /// Search candidate-list size.
    pub ef_search: usize,
    /// Layer-assignment normalization factor.
    pub ml: f64,
    /// Distance-metric discriminant byte (engine owns the enum mapping).
    pub metric: u8,
    /// Highest populated layer in the graph.
    pub max_layer: usize,
    /// Entry-point node id, or `None` when the index is empty.
    pub entry_point: Option<u64>,
    /// Graph nodes in persistence order.
    pub nodes: Vec<HnswNodeLayout>,
}

/// Errors raised while decoding a persisted HNSW payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HnswCodecError {
    /// Fewer than the minimum header bytes were present.
    TooShort,
    /// The leading magic was not `HNSW`.
    InvalidMagic,
    /// The version field was not `1`.
    UnsupportedVersion(u32),
    /// A field ran past the end of the buffer.
    Truncated,
}

impl std::fmt::Display for HnswCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HnswCodecError::TooShort => write!(f, "Data too short"),
            HnswCodecError::InvalidMagic => write!(f, "Invalid magic number"),
            HnswCodecError::UnsupportedVersion(v) => write!(f, "Unsupported version: {}", v),
            HnswCodecError::Truncated => write!(f, "truncated HNSW payload"),
        }
    }
}

impl std::error::Error for HnswCodecError {}

/// Encode a persisted HNSW index, byte-faithful to the legacy server layout.
pub fn encode_hnsw_index(layout: &HnswIndexLayout) -> Vec<u8> {
    let mut bytes = Vec::new();

    bytes.extend_from_slice(HNSW_INDEX_MAGIC);
    bytes.extend_from_slice(&HNSW_INDEX_VERSION.to_le_bytes());

    bytes.extend_from_slice(&(layout.dimension as u32).to_le_bytes());
    bytes.extend_from_slice(&(layout.m as u32).to_le_bytes());
    bytes.extend_from_slice(&(layout.m_max0 as u32).to_le_bytes());
    bytes.extend_from_slice(&(layout.ef_construction as u32).to_le_bytes());
    bytes.extend_from_slice(&(layout.ef_search as u32).to_le_bytes());
    bytes.extend_from_slice(&layout.ml.to_le_bytes());
    bytes.push(layout.metric);

    bytes.extend_from_slice(&(layout.max_layer as u32).to_le_bytes());
    bytes.extend_from_slice(&layout.entry_point.unwrap_or(u64::MAX).to_le_bytes());

    bytes.extend_from_slice(&(layout.nodes.len() as u64).to_le_bytes());

    for node in &layout.nodes {
        bytes.extend_from_slice(&node.id.to_le_bytes());
        bytes.extend_from_slice(&(node.max_layer as u32).to_le_bytes());

        for &val in &node.vector {
            bytes.extend_from_slice(&val.to_le_bytes());
        }

        for layer in 0..=node.max_layer {
            let conns = &node.connections[layer];
            bytes.extend_from_slice(&(conns.len() as u32).to_le_bytes());
            for &conn in conns {
                bytes.extend_from_slice(&conn.to_le_bytes());
            }
        }
    }

    bytes
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], HnswCodecError> {
        if self.pos + n > self.bytes.len() {
            return Err(HnswCodecError::Truncated);
        }
        let slice = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, HnswCodecError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, HnswCodecError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn f32(&mut self) -> Result<f32, HnswCodecError> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn f64(&mut self) -> Result<f64, HnswCodecError> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn byte(&mut self) -> Result<u8, HnswCodecError> {
        Ok(self.take(1)?[0])
    }
}

/// Decode a persisted HNSW index produced by [`encode_hnsw_index`] or the
/// legacy server `to_bytes`.
pub fn decode_hnsw_index(bytes: &[u8]) -> Result<HnswIndexLayout, HnswCodecError> {
    if bytes.len() < 8 {
        return Err(HnswCodecError::TooShort);
    }
    if &bytes[0..4] != HNSW_INDEX_MAGIC {
        return Err(HnswCodecError::InvalidMagic);
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    if version != HNSW_INDEX_VERSION {
        return Err(HnswCodecError::UnsupportedVersion(version));
    }

    let mut cur = Cursor::new(bytes);
    cur.pos = 8;

    let dimension = cur.u32()? as usize;
    let m = cur.u32()? as usize;
    let m_max0 = cur.u32()? as usize;
    let ef_construction = cur.u32()? as usize;
    let ef_search = cur.u32()? as usize;
    let ml = cur.f64()?;
    let metric = cur.byte()?;

    let max_layer = cur.u32()? as usize;
    let ep_value = cur.u64()?;
    let entry_point = if ep_value == u64::MAX {
        None
    } else {
        Some(ep_value)
    };

    let node_count = cur.u64()? as usize;
    let mut nodes = Vec::with_capacity(node_count);

    for _ in 0..node_count {
        let id = cur.u64()?;
        let level = cur.u32()? as usize;

        let mut vector = Vec::with_capacity(dimension);
        for _ in 0..dimension {
            vector.push(cur.f32()?);
        }

        let mut connections = vec![Vec::new(); level + 1];
        for conn_list in connections.iter_mut().take(level + 1) {
            let conn_count = cur.u32()? as usize;
            for _ in 0..conn_count {
                conn_list.push(cur.u64()?);
            }
        }

        nodes.push(HnswNodeLayout {
            id,
            max_layer: level,
            vector,
            connections,
        });
    }

    Ok(HnswIndexLayout {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> HnswIndexLayout {
        HnswIndexLayout {
            dimension: 3,
            m: 16,
            m_max0: 32,
            ef_construction: 100,
            ef_search: 50,
            ml: 0.36067376022224085,
            metric: 1,
            max_layer: 1,
            entry_point: Some(7),
            nodes: vec![
                HnswNodeLayout {
                    id: 7,
                    max_layer: 1,
                    vector: vec![0.1, 0.2, 0.3],
                    connections: vec![vec![3, 9], vec![3]],
                },
                HnswNodeLayout {
                    id: 3,
                    max_layer: 0,
                    vector: vec![-1.0, 0.0, 2.5],
                    connections: vec![vec![7]],
                },
            ],
        }
    }

    #[test]
    fn round_trip_preserves_layout() {
        let layout = sample();
        let bytes = encode_hnsw_index(&layout);
        let decoded = decode_hnsw_index(&bytes).expect("decode");
        assert_eq!(decoded, layout);
    }

    #[test]
    fn fixture_bytes_are_byte_identical() {
        // Pinned pre-move fixture: magic + version 1 + config + node state.
        let layout = sample();
        let bytes = encode_hnsw_index(&layout);

        assert_eq!(&bytes[0..4], b"HNSW", "magic must lead the payload");
        assert_eq!(&bytes[4..8], &1u32.to_le_bytes(), "version 1 little-endian");
        // dimension u32 follows the version.
        assert_eq!(&bytes[8..12], &3u32.to_le_bytes());
        // entry_point present (Some(7)) must round-trip, not the None sentinel.
        let decoded = decode_hnsw_index(&bytes).unwrap();
        assert_eq!(decoded.entry_point, Some(7));
    }

    #[test]
    fn empty_entry_point_uses_sentinel() {
        let mut layout = sample();
        layout.entry_point = None;
        layout.nodes.clear();
        let bytes = encode_hnsw_index(&layout);
        // entry_point sits at offset 8 + 4*5 + 8 + 1 + 4 = 41.
        assert_eq!(&bytes[41..49], &u64::MAX.to_le_bytes());
        let decoded = decode_hnsw_index(&bytes).unwrap();
        assert_eq!(decoded.entry_point, None);
    }

    #[test]
    fn rejects_bad_magic_and_version() {
        assert_eq!(decode_hnsw_index(&[0u8; 4]), Err(HnswCodecError::TooShort));
        let mut bytes = encode_hnsw_index(&sample());
        bytes[0] = b'X';
        assert_eq!(decode_hnsw_index(&bytes), Err(HnswCodecError::InvalidMagic));
        let mut bytes = encode_hnsw_index(&sample());
        bytes[4] = 2;
        assert_eq!(
            decode_hnsw_index(&bytes),
            Err(HnswCodecError::UnsupportedVersion(2))
        );
    }
}
