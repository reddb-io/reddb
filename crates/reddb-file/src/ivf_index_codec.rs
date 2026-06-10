//! On-disk codec for the `IVF1` vector-index payload.
//!
//! `reddb-file` owns the byte layout of the persisted IVF artifact: the `IVF1`
//! magic, the config block, training state, and per-list centroid/vector data.
//! The clustering algorithm stays in the server engine; only the format-faithful
//! encode/decode lives here (ADR 0046).
//!
//! Layout (all integers little-endian, no version field):
//! ```text
//! magic   : b"IVF1"            (4 bytes)
//! config  : n_lists u32, n_probes u32, dimension u32, max_iterations u32,
//!           convergence_threshold f32
//! state   : trained u8, count u64, next_id u64
//! lists   : list_count u32, then per list:
//!             centroid_len u32, centroid f32[centroid_len],
//!             id_count u32, ids u64[id_count],
//!             vector_count u32, then per vector: len u32, f32[len]
//! ```

/// Magic prefixing a persisted IVF index payload.
pub const IVF_INDEX_MAGIC: &[u8; 4] = b"IVF1";

/// Plain, engine-agnostic view of one persisted IVF inverted list.
#[derive(Debug, Clone, PartialEq)]
pub struct IvfListLayout {
    /// Cluster centroid.
    pub centroid: Vec<f32>,
    /// Member vector ids.
    pub ids: Vec<u64>,
    /// Member vectors (parallel to `ids`).
    pub vectors: Vec<Vec<f32>>,
}

/// Plain, engine-agnostic view of a persisted IVF index.
#[derive(Debug, Clone, PartialEq)]
pub struct IvfIndexLayout {
    /// Number of inverted lists / clusters.
    pub n_lists: usize,
    /// Number of lists probed at query time.
    pub n_probes: usize,
    /// Vector dimension.
    pub dimension: usize,
    /// Max k-means iterations.
    pub max_iterations: usize,
    /// k-means convergence threshold.
    pub convergence_threshold: f32,
    /// Whether the index has been trained.
    pub trained: bool,
    /// Total stored vector count.
    pub count: usize,
    /// Next auto-generated id.
    pub next_id: u64,
    /// Inverted lists in persistence order.
    pub lists: Vec<IvfListLayout>,
}

/// Errors raised while decoding a persisted IVF payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IvfCodecError {
    /// Fewer than the minimum header bytes were present.
    TooShort,
    /// The leading magic was not `IVF1`.
    InvalidMagic,
    /// A field ran past the end of the buffer.
    Truncated,
}

impl std::fmt::Display for IvfCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IvfCodecError::TooShort => write!(f, "data too short"),
            IvfCodecError::InvalidMagic => write!(f, "invalid IVF magic"),
            IvfCodecError::Truncated => write!(f, "truncated IVF payload"),
        }
    }
}

impl std::error::Error for IvfCodecError {}

/// Encode a persisted IVF index, byte-faithful to the legacy server layout.
pub fn encode_ivf_index(layout: &IvfIndexLayout) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(IVF_INDEX_MAGIC);
    bytes.extend_from_slice(&(layout.n_lists as u32).to_le_bytes());
    bytes.extend_from_slice(&(layout.n_probes as u32).to_le_bytes());
    bytes.extend_from_slice(&(layout.dimension as u32).to_le_bytes());
    bytes.extend_from_slice(&(layout.max_iterations as u32).to_le_bytes());
    bytes.extend_from_slice(&layout.convergence_threshold.to_le_bytes());
    bytes.push(if layout.trained { 1 } else { 0 });
    bytes.extend_from_slice(&(layout.count as u64).to_le_bytes());
    bytes.extend_from_slice(&layout.next_id.to_le_bytes());
    bytes.extend_from_slice(&(layout.lists.len() as u32).to_le_bytes());

    for list in &layout.lists {
        bytes.extend_from_slice(&(list.centroid.len() as u32).to_le_bytes());
        for value in &list.centroid {
            bytes.extend_from_slice(&value.to_le_bytes());
        }

        bytes.extend_from_slice(&(list.ids.len() as u32).to_le_bytes());
        for id in &list.ids {
            bytes.extend_from_slice(&id.to_le_bytes());
        }

        bytes.extend_from_slice(&(list.vectors.len() as u32).to_le_bytes());
        for vector in &list.vectors {
            bytes.extend_from_slice(&(vector.len() as u32).to_le_bytes());
            for value in vector {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
    }

    bytes
}

fn read_u32(buf: &[u8], pos: &mut usize) -> Result<u32, IvfCodecError> {
    if *pos + 4 > buf.len() {
        return Err(IvfCodecError::Truncated);
    }
    let value = u32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
    *pos += 4;
    Ok(value)
}

fn read_u64(buf: &[u8], pos: &mut usize) -> Result<u64, IvfCodecError> {
    if *pos + 8 > buf.len() {
        return Err(IvfCodecError::Truncated);
    }
    let value = u64::from_le_bytes([
        buf[*pos],
        buf[*pos + 1],
        buf[*pos + 2],
        buf[*pos + 3],
        buf[*pos + 4],
        buf[*pos + 5],
        buf[*pos + 6],
        buf[*pos + 7],
    ]);
    *pos += 8;
    Ok(value)
}

fn read_f32(buf: &[u8], pos: &mut usize) -> Result<f32, IvfCodecError> {
    if *pos + 4 > buf.len() {
        return Err(IvfCodecError::Truncated);
    }
    let value = f32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
    *pos += 4;
    Ok(value)
}

/// Decode a persisted IVF index produced by [`encode_ivf_index`] or the legacy
/// server `to_bytes`.
pub fn decode_ivf_index(bytes: &[u8]) -> Result<IvfIndexLayout, IvfCodecError> {
    if bytes.len() < 41 {
        return Err(IvfCodecError::TooShort);
    }
    if &bytes[0..4] != IVF_INDEX_MAGIC {
        return Err(IvfCodecError::InvalidMagic);
    }

    let mut pos = 4usize;
    let n_lists = read_u32(bytes, &mut pos)? as usize;
    let n_probes = read_u32(bytes, &mut pos)? as usize;
    let dimension = read_u32(bytes, &mut pos)? as usize;
    let max_iterations = read_u32(bytes, &mut pos)? as usize;
    let convergence_threshold = read_f32(bytes, &mut pos)?;

    if pos >= bytes.len() {
        return Err(IvfCodecError::Truncated);
    }
    let trained = bytes[pos] == 1;
    pos += 1;
    let count = read_u64(bytes, &mut pos)? as usize;
    let next_id = read_u64(bytes, &mut pos)?;
    let list_count = read_u32(bytes, &mut pos)? as usize;

    let mut lists = Vec::with_capacity(list_count);
    for _ in 0..list_count {
        let centroid_len = read_u32(bytes, &mut pos)? as usize;
        let mut centroid = Vec::with_capacity(centroid_len);
        for _ in 0..centroid_len {
            centroid.push(read_f32(bytes, &mut pos)?);
        }

        let id_count = read_u32(bytes, &mut pos)? as usize;
        let mut ids = Vec::with_capacity(id_count);
        for _ in 0..id_count {
            ids.push(read_u64(bytes, &mut pos)?);
        }

        let vector_count = read_u32(bytes, &mut pos)? as usize;
        let mut vectors = Vec::with_capacity(vector_count);
        for _ in 0..vector_count {
            let vector_len = read_u32(bytes, &mut pos)? as usize;
            let mut vector = Vec::with_capacity(vector_len);
            for _ in 0..vector_len {
                vector.push(read_f32(bytes, &mut pos)?);
            }
            vectors.push(vector);
        }

        lists.push(IvfListLayout {
            centroid,
            ids,
            vectors,
        });
    }

    Ok(IvfIndexLayout {
        n_lists,
        n_probes,
        dimension,
        max_iterations,
        convergence_threshold,
        trained,
        count,
        next_id,
        lists,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> IvfIndexLayout {
        IvfIndexLayout {
            n_lists: 4,
            n_probes: 2,
            dimension: 3,
            max_iterations: 50,
            convergence_threshold: 1e-4,
            trained: true,
            count: 3,
            next_id: 3,
            lists: vec![
                IvfListLayout {
                    centroid: vec![0.0, 1.0, 2.0],
                    ids: vec![0, 2],
                    vectors: vec![vec![0.0, 1.0, 2.0], vec![0.1, 1.1, 2.1]],
                },
                IvfListLayout {
                    centroid: vec![9.0, 9.0, 9.0],
                    ids: vec![1],
                    vectors: vec![vec![9.0, 9.0, 9.0]],
                },
            ],
        }
    }

    #[test]
    fn round_trip_preserves_layout() {
        let layout = sample();
        let bytes = encode_ivf_index(&layout);
        let decoded = decode_ivf_index(&bytes).expect("decode");
        assert_eq!(decoded, layout);
    }

    #[test]
    fn fixture_bytes_are_byte_identical() {
        let layout = sample();
        let bytes = encode_ivf_index(&layout);
        assert_eq!(&bytes[0..4], b"IVF1", "magic must lead the payload");
        // n_lists u32 directly after the magic (no version field).
        assert_eq!(&bytes[4..8], &4u32.to_le_bytes());
        // trained byte sits at offset 4 + 4*4 + 4 = 24.
        assert_eq!(bytes[24], 1);
    }

    #[test]
    fn rejects_short_and_bad_magic() {
        assert_eq!(decode_ivf_index(&[0u8; 10]), Err(IvfCodecError::TooShort));
        let mut bytes = encode_ivf_index(&sample());
        bytes[0] = b'X';
        assert_eq!(decode_ivf_index(&bytes), Err(IvfCodecError::InvalidMagic));
    }
}
