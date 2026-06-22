//! Persisted IVF (inverted-file) vector-index payload codec.
//!
//! The vector engine owns k-means training, probing, and assignment. This
//! module owns only the durable byte layout of a serialized IVF index. The
//! `"IVF1"` magic doubles as the format identifier; there is no separate
//! version word.
//!
//! Byte layout (little-endian) — DO NOT change order/width; these bytes live in
//! existing `.rdb` artifacts:
//!
//! ```text
//! "IVF1"                 4 bytes magic
//! n_lists                u32
//! n_probes               u32
//! dimension              u32
//! max_iterations         u32
//! convergence_threshold  f32
//! trained                u8  (0 / 1)
//! count                  u64
//! next_id                u64
//! list_count             u32
//! repeated list_count times:
//!   centroid_len         u32
//!   centroid             f32 * centroid_len
//!   id_count             u32
//!   id                   u64 * id_count
//!   vector_count         u32
//!   repeated vector_count times:
//!     vector_len         u32
//!     value             f32 * vector_len
//! ```

/// Magic prefix for a serialized IVF index payload.
pub const IVF_INDEX_MAGIC: [u8; 4] = *b"IVF1";
/// Minimum length of a well-formed payload (header through `list_count`).
pub const IVF_INDEX_HEADER_LEN: usize = 4  // magic
    + 4  // n_lists
    + 4  // n_probes
    + 4  // dimension
    + 4  // max_iterations
    + 4  // convergence_threshold
    + 1  // trained
    + 8  // count
    + 8  // next_id
    + 4; // list_count

/// A decoded IVF inverted list (one Voronoi cell).
#[derive(Debug, Clone, PartialEq)]
pub struct IvfListFrame {
    pub centroid: Vec<f32>,
    pub ids: Vec<u64>,
    pub vectors: Vec<Vec<f32>>,
}

/// A decoded IVF index payload. Plain data only — the engine rebuilds derived
/// state (the id→list map) from these fields.
#[derive(Debug, Clone, PartialEq)]
pub struct IvfIndexFrame {
    pub n_lists: u32,
    pub n_probes: u32,
    pub dimension: u32,
    pub max_iterations: u32,
    pub convergence_threshold: f32,
    pub trained: bool,
    pub count: u64,
    pub next_id: u64,
    pub lists: Vec<IvfListFrame>,
}

/// Errors decoding an IVF index payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IvfIndexFrameError {
    TooShort,
    InvalidMagic,
    Truncated { offset: usize, reason: &'static str },
}

impl std::fmt::Display for IvfIndexFrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort => write!(f, "data too short"),
            Self::InvalidMagic => write!(f, "invalid IVF magic"),
            Self::Truncated { offset, reason } => {
                write!(f, "truncated IVF payload at offset {offset}: {reason}")
            }
        }
    }
}

impl std::error::Error for IvfIndexFrameError {}

/// Serialize an IVF index payload to bytes.
pub fn encode_ivf_index_frame(frame: &IvfIndexFrame) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&IVF_INDEX_MAGIC);
    bytes.extend_from_slice(&frame.n_lists.to_le_bytes());
    bytes.extend_from_slice(&frame.n_probes.to_le_bytes());
    bytes.extend_from_slice(&frame.dimension.to_le_bytes());
    bytes.extend_from_slice(&frame.max_iterations.to_le_bytes());
    bytes.extend_from_slice(&frame.convergence_threshold.to_le_bytes());
    bytes.push(if frame.trained { 1 } else { 0 });
    bytes.extend_from_slice(&frame.count.to_le_bytes());
    bytes.extend_from_slice(&frame.next_id.to_le_bytes());
    bytes.extend_from_slice(&(frame.lists.len() as u32).to_le_bytes());

    for list in &frame.lists {
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

/// Deserialize an IVF index payload from bytes.
pub fn decode_ivf_index_frame(bytes: &[u8]) -> Result<IvfIndexFrame, IvfIndexFrameError> {
    if bytes.len() < 41 {
        return Err(IvfIndexFrameError::TooShort);
    }
    if bytes[0..4] != IVF_INDEX_MAGIC {
        return Err(IvfIndexFrameError::InvalidMagic);
    }

    let mut pos = 4usize;
    let n_lists = read_u32(bytes, &mut pos, "n_lists")?;
    let n_probes = read_u32(bytes, &mut pos, "n_probes")?;
    let dimension = read_u32(bytes, &mut pos, "dimension")?;
    let max_iterations = read_u32(bytes, &mut pos, "max_iterations")?;
    let convergence_threshold = read_f32(bytes, &mut pos, "convergence_threshold")?;
    let trained = read_u8(bytes, &mut pos, "trained")? == 1;
    let count = read_u64(bytes, &mut pos, "count")?;
    let next_id = read_u64(bytes, &mut pos, "next_id")?;
    let list_count = read_u32(bytes, &mut pos, "list_count")? as usize;

    let mut lists = Vec::with_capacity(list_count);
    for _ in 0..list_count {
        let centroid_len = read_u32(bytes, &mut pos, "centroid_len")? as usize;
        let mut centroid = Vec::with_capacity(centroid_len);
        for _ in 0..centroid_len {
            centroid.push(read_f32(bytes, &mut pos, "centroid")?);
        }

        let id_count = read_u32(bytes, &mut pos, "id_count")? as usize;
        let mut ids = Vec::with_capacity(id_count);
        for _ in 0..id_count {
            ids.push(read_u64(bytes, &mut pos, "id")?);
        }

        let vector_count = read_u32(bytes, &mut pos, "vector_count")? as usize;
        let mut vectors = Vec::with_capacity(vector_count);
        for _ in 0..vector_count {
            let vector_len = read_u32(bytes, &mut pos, "vector_len")? as usize;
            let mut vector = Vec::with_capacity(vector_len);
            for _ in 0..vector_len {
                vector.push(read_f32(bytes, &mut pos, "vector value")?);
            }
            vectors.push(vector);
        }

        lists.push(IvfListFrame {
            centroid,
            ids,
            vectors,
        });
    }

    Ok(IvfIndexFrame {
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

fn read_u8(bytes: &[u8], pos: &mut usize, reason: &'static str) -> Result<u8, IvfIndexFrameError> {
    if *pos + 1 > bytes.len() {
        return Err(IvfIndexFrameError::Truncated {
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
) -> Result<u32, IvfIndexFrameError> {
    if *pos + 4 > bytes.len() {
        return Err(IvfIndexFrameError::Truncated {
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
    reason: &'static str,
) -> Result<u64, IvfIndexFrameError> {
    if *pos + 8 > bytes.len() {
        return Err(IvfIndexFrameError::Truncated {
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
    reason: &'static str,
) -> Result<f32, IvfIndexFrameError> {
    if *pos + 4 > bytes.len() {
        return Err(IvfIndexFrameError::Truncated {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_frame() -> IvfIndexFrame {
        IvfIndexFrame {
            n_lists: 4,
            n_probes: 2,
            dimension: 3,
            max_iterations: 50,
            convergence_threshold: 1e-4,
            trained: true,
            count: 3,
            next_id: 9,
            lists: vec![
                IvfListFrame {
                    centroid: vec![0.5, 0.5, 0.5],
                    ids: vec![1, 2],
                    vectors: vec![vec![0.4, 0.4, 0.4], vec![0.6, 0.6, 0.6]],
                },
                IvfListFrame {
                    centroid: vec![9.0, 9.0, 9.0],
                    ids: vec![8],
                    vectors: vec![vec![9.1, 9.0, 8.9]],
                },
            ],
        }
    }

    #[test]
    fn ivf_index_frame_round_trips() {
        let frame = sample_frame();
        let encoded = encode_ivf_index_frame(&frame);
        let decoded = decode_ivf_index_frame(&encoded).unwrap();
        assert_eq!(decoded, frame);
        assert_eq!(encode_ivf_index_frame(&decoded), encoded);
    }

    #[test]
    fn ivf_index_frame_pins_byte_layout() {
        let frame = sample_frame();
        let encoded = encode_ivf_index_frame(&frame);
        assert_eq!(&encoded[0..4], b"IVF1");
        assert_eq!(&encoded[4..8], &4u32.to_le_bytes()); // n_lists
                                                         // `trained` byte: after magic + 4 u32 + 1 f32.
        let trained_off = 4 + 4 * 4 + 4;
        assert_eq!(encoded[trained_off], 1);
    }

    #[test]
    fn ivf_index_frame_rejects_bad_input() {
        assert_eq!(
            decode_ivf_index_frame(&[0u8; 8]),
            Err(IvfIndexFrameError::TooShort)
        );
        let mut bad_magic = encode_ivf_index_frame(&sample_frame());
        bad_magic[0] = b'X';
        assert_eq!(
            decode_ivf_index_frame(&bad_magic),
            Err(IvfIndexFrameError::InvalidMagic)
        );
        let encoded = encode_ivf_index_frame(&sample_frame());
        assert!(matches!(
            decode_ivf_index_frame(&encoded[..encoded.len() - 1]),
            Err(IvfIndexFrameError::Truncated { .. })
        ));
    }
}
