//! `.tv` snapshot file format for vector.turbo collections (issue #674).
//!
//! Pure cache for fast boot: snapshots capture the in-memory
//! TurboQuantIndex state at WAL checkpoint time and let recovery skip
//! the entity-manager scan. Deleting all `.tv` files is always safe —
//! recovery falls back to the slice E extent/WAL rebuild path. Snapshots
//! never become the source of truth.
//!
//! On-disk layout (little-endian, no padding):
//!
//! ```text
//! +--------------------------------+
//! | magic     [8]  = b".TVSNAP\x01"|
//! | version   [2]  u16             |
//! | flags     [2]  u16 (reserved)  |
//! | dim       [4]  u32             |
//! | seed      [8]  u64             |
//! | lsn       [8]  u64             |
//! | n_vectors [8]  u64             |
//! | body_crc  [4]  u32             |
//! | head_crc  [4]  u32             |
//! +--------------------------------+
//! | n_vectors records, each:       |
//! |   entity_id [8] u64            |
//! |   dim       [4] u32  (== hdr)  |
//! |   floats    [4*dim] f32 LE     |
//! +--------------------------------+
//! ```
//!
//! `head_crc` is computed over bytes `0..(HEADER_BYTES-4)`. `body_crc`
//! is computed over the entire body. A mismatch on either CRC, magic,
//! or version drops the snapshot and triggers the rebuild fallback.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b".TVSNAP\x01";
const VERSION: u16 = 1;
/// Size of the fixed snapshot header in bytes.
pub const HEADER_BYTES: usize = 8 + 2 + 2 + 4 + 8 + 8 + 8 + 4 + 4;

/// What the loader recovered from a snapshot file: header + replayed
/// `(entity_id, vector)` pairs in stored order. Callers feed these
/// into a fresh `TurboQuantIndex` with the same codec seed to get
/// byte-identical block/lane placement.
#[derive(Debug)]
pub struct SnapshotPayload {
    pub dim: u32,
    pub seed: u64,
    pub lsn: u64,
    pub vectors: Vec<(u64, Vec<f32>)>,
}

/// Why a snapshot read failed. Every variant is non-fatal at the
/// runtime layer — caller logs at WARN and falls back to rebuild.
#[derive(Debug)]
pub enum SnapshotError {
    Io(io::Error),
    BadMagic,
    UnsupportedVersion(u16),
    HeaderCrcMismatch,
    BodyCrcMismatch,
    DimensionMismatch { expected: u32, actual: u32 },
    SeedMismatch { expected: u64, actual: u64 },
    Truncated,
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "snapshot io: {e}"),
            Self::BadMagic => write!(f, "snapshot magic mismatch"),
            Self::UnsupportedVersion(v) => write!(f, "snapshot version {v} unsupported"),
            Self::HeaderCrcMismatch => write!(f, "snapshot header crc mismatch"),
            Self::BodyCrcMismatch => write!(f, "snapshot body crc mismatch"),
            Self::DimensionMismatch { expected, actual } => {
                write!(f, "snapshot dim {actual} != expected {expected}")
            }
            Self::SeedMismatch { expected, actual } => {
                write!(f, "snapshot seed {actual:#x} != expected {expected:#x}")
            }
            Self::Truncated => write!(f, "snapshot truncated"),
        }
    }
}

impl From<io::Error> for SnapshotError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl std::error::Error for SnapshotError {}

/// Serialize the supplied state to `path` atomically (write to
/// `<path>.tmp`, fsync, rename). Returns `Ok(())` once the rename has
/// completed; the directory fsync is best-effort.
///
/// Caller is responsible for ensuring `path.parent()` exists.
pub fn write_snapshot(
    path: &Path,
    dim: u32,
    seed: u64,
    lsn: u64,
    vectors: &[(u64, Vec<f32>)],
) -> Result<(), SnapshotError> {
    let tmp = path.with_extension("tv.tmp");
    let mut body = Vec::with_capacity(vectors.len() * (8 + 4 + dim as usize * 4));
    for (id, vec) in vectors {
        body.extend_from_slice(&id.to_le_bytes());
        body.extend_from_slice(&(vec.len() as u32).to_le_bytes());
        for v in vec {
            body.extend_from_slice(&v.to_le_bytes());
        }
    }
    let body_crc = crc32(&body);

    let mut head = Vec::with_capacity(HEADER_BYTES);
    head.extend_from_slice(MAGIC);
    head.extend_from_slice(&VERSION.to_le_bytes());
    head.extend_from_slice(&0u16.to_le_bytes()); // flags
    head.extend_from_slice(&dim.to_le_bytes());
    head.extend_from_slice(&seed.to_le_bytes());
    head.extend_from_slice(&lsn.to_le_bytes());
    head.extend_from_slice(&(vectors.len() as u64).to_le_bytes());
    head.extend_from_slice(&body_crc.to_le_bytes());
    let head_crc = crc32(&head);
    head.extend_from_slice(&head_crc.to_le_bytes());
    debug_assert_eq!(head.len(), HEADER_BYTES);

    {
        let file = File::create(&tmp)?;
        let mut w = BufWriter::new(&file);
        w.write_all(&head)?;
        w.write_all(&body)?;
        w.flush()?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn read_snapshot(
    path: &Path,
    expected_dim: u32,
    expected_seed: u64,
) -> Result<SnapshotPayload, SnapshotError> {
    let file = File::open(path)?;
    let mut r = BufReader::new(file);

    let mut head = [0u8; HEADER_BYTES];
    r.read_exact(&mut head).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => SnapshotError::Truncated,
        _ => SnapshotError::Io(e),
    })?;

    if &head[0..8] != MAGIC {
        return Err(SnapshotError::BadMagic);
    }
    let head_crc_actual =
        u32::from_le_bytes(head[HEADER_BYTES - 4..HEADER_BYTES].try_into().unwrap());
    let head_crc_expected = crc32(&head[..HEADER_BYTES - 4]);
    if head_crc_actual != head_crc_expected {
        return Err(SnapshotError::HeaderCrcMismatch);
    }

    let version = u16::from_le_bytes(head[8..10].try_into().unwrap());
    if version != VERSION {
        return Err(SnapshotError::UnsupportedVersion(version));
    }
    let dim = u32::from_le_bytes(head[12..16].try_into().unwrap());
    let seed = u64::from_le_bytes(head[16..24].try_into().unwrap());
    let lsn = u64::from_le_bytes(head[24..32].try_into().unwrap());
    let n_vectors = u64::from_le_bytes(head[32..40].try_into().unwrap()) as usize;
    let body_crc_expected = u32::from_le_bytes(head[40..44].try_into().unwrap());

    if dim != expected_dim {
        return Err(SnapshotError::DimensionMismatch {
            expected: expected_dim,
            actual: dim,
        });
    }
    if seed != expected_seed {
        return Err(SnapshotError::SeedMismatch {
            expected: expected_seed,
            actual: seed,
        });
    }

    let record_bytes = 8 + 4 + dim as usize * 4;
    let mut body = vec![0u8; n_vectors * record_bytes];
    r.read_exact(&mut body).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => SnapshotError::Truncated,
        _ => SnapshotError::Io(e),
    })?;
    let body_crc_actual = crc32(&body);
    if body_crc_actual != body_crc_expected {
        return Err(SnapshotError::BodyCrcMismatch);
    }

    let mut vectors = Vec::with_capacity(n_vectors);
    let mut off = 0usize;
    for _ in 0..n_vectors {
        let id = u64::from_le_bytes(body[off..off + 8].try_into().unwrap());
        off += 8;
        let rec_dim = u32::from_le_bytes(body[off..off + 4].try_into().unwrap());
        off += 4;
        if rec_dim != dim {
            return Err(SnapshotError::DimensionMismatch {
                expected: dim,
                actual: rec_dim,
            });
        }
        let mut v = Vec::with_capacity(dim as usize);
        for _ in 0..dim {
            let f = f32::from_le_bytes(body[off..off + 4].try_into().unwrap());
            off += 4;
            v.push(f);
        }
        vectors.push((id, v));
    }

    Ok(SnapshotPayload {
        dim,
        seed,
        lsn,
        vectors,
    })
}

/// IEEE 802.3 CRC-32 (poly 0xEDB88320). Self-contained to avoid pulling
/// a new dependency into the engine for one short helper.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        let mut x = (crc ^ byte as u32) & 0xFF;
        for _ in 0..8 {
            x = if x & 1 != 0 {
                (x >> 1) ^ 0xEDB8_8320
            } else {
                x >> 1
            };
        }
        crc = (crc >> 8) ^ x;
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("reddb-tv-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("c.tv")
    }

    #[test]
    fn round_trip_preserves_vectors_in_order() {
        let path = tmp("round-trip");
        let vectors = vec![
            (1u64, vec![1.0, 2.0, 3.0]),
            (5u64, vec![4.0, 5.0, 6.0]),
            (9u64, vec![7.0, 8.0, 9.0]),
        ];
        write_snapshot(&path, 3, 0xdead_beef, 42, &vectors).unwrap();
        let payload = read_snapshot(&path, 3, 0xdead_beef).unwrap();
        assert_eq!(payload.dim, 3);
        assert_eq!(payload.lsn, 42);
        assert_eq!(payload.vectors, vectors);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn read_rejects_bad_magic() {
        let path = tmp("bad-magic");
        std::fs::write(&path, b"NOTAREDDBTVSNAP").unwrap();
        let err = read_snapshot(&path, 3, 0).unwrap_err();
        assert!(matches!(
            err,
            SnapshotError::BadMagic | SnapshotError::Truncated
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn read_detects_body_crc_corruption() {
        let path = tmp("body-crc");
        let vectors = vec![(7u64, vec![1.0, 2.0])];
        write_snapshot(&path, 2, 1, 0, &vectors).unwrap();
        // Flip one byte in the body region.
        let mut bytes = std::fs::read(&path).unwrap();
        let idx = HEADER_BYTES + 2;
        bytes[idx] ^= 0xff;
        std::fs::write(&path, &bytes).unwrap();
        let err = read_snapshot(&path, 2, 1).unwrap_err();
        assert!(matches!(err, SnapshotError::BodyCrcMismatch));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn read_rejects_dimension_mismatch() {
        let path = tmp("dim");
        write_snapshot(&path, 3, 1, 0, &[(1, vec![0.0, 0.0, 0.0])]).unwrap();
        let err = read_snapshot(&path, 4, 1).unwrap_err();
        assert!(matches!(err, SnapshotError::DimensionMismatch { .. }));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
