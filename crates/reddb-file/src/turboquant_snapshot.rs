use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b".TVSNAP\x01";
const VERSION: u16 = 1;
pub const TURBOQUANT_SNAPSHOT_HEADER_BYTES: usize = 8 + 2 + 2 + 4 + 8 + 8 + 8 + 4 + 4;

#[derive(Debug)]
pub struct TurboQuantSnapshotPayload {
    pub dim: u32,
    pub seed: u64,
    pub lsn: u64,
    pub vectors: Vec<(u64, Vec<f32>)>,
}

#[derive(Debug)]
pub enum TurboQuantSnapshotError {
    Io(io::Error),
    BadMagic,
    UnsupportedVersion(u16),
    HeaderCrcMismatch,
    BodyCrcMismatch,
    DimensionMismatch { expected: u32, actual: u32 },
    SeedMismatch { expected: u64, actual: u64 },
    Truncated,
}

impl std::fmt::Display for TurboQuantSnapshotError {
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

impl From<io::Error> for TurboQuantSnapshotError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl std::error::Error for TurboQuantSnapshotError {}

pub fn write_turboquant_snapshot(
    path: &Path,
    dim: u32,
    seed: u64,
    lsn: u64,
    vectors: &[(u64, Vec<f32>)],
) -> Result<(), TurboQuantSnapshotError> {
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

    let mut head = Vec::with_capacity(TURBOQUANT_SNAPSHOT_HEADER_BYTES);
    head.extend_from_slice(MAGIC);
    head.extend_from_slice(&VERSION.to_le_bytes());
    head.extend_from_slice(&0u16.to_le_bytes());
    head.extend_from_slice(&dim.to_le_bytes());
    head.extend_from_slice(&seed.to_le_bytes());
    head.extend_from_slice(&lsn.to_le_bytes());
    head.extend_from_slice(&(vectors.len() as u64).to_le_bytes());
    head.extend_from_slice(&body_crc.to_le_bytes());
    let head_crc = crc32(&head);
    head.extend_from_slice(&head_crc.to_le_bytes());
    debug_assert_eq!(head.len(), TURBOQUANT_SNAPSHOT_HEADER_BYTES);

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

pub fn read_turboquant_snapshot(
    path: &Path,
    expected_dim: u32,
    expected_seed: u64,
) -> Result<TurboQuantSnapshotPayload, TurboQuantSnapshotError> {
    let file = File::open(path)?;
    let mut r = BufReader::new(file);

    let mut head = [0u8; TURBOQUANT_SNAPSHOT_HEADER_BYTES];
    r.read_exact(&mut head).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => TurboQuantSnapshotError::Truncated,
        _ => TurboQuantSnapshotError::Io(e),
    })?;

    if &head[0..8] != MAGIC {
        return Err(TurboQuantSnapshotError::BadMagic);
    }
    let head_crc_actual = u32::from_le_bytes(
        head[TURBOQUANT_SNAPSHOT_HEADER_BYTES - 4..TURBOQUANT_SNAPSHOT_HEADER_BYTES]
            .try_into()
            .unwrap(),
    );
    let head_crc_expected = crc32(&head[..TURBOQUANT_SNAPSHOT_HEADER_BYTES - 4]);
    if head_crc_actual != head_crc_expected {
        return Err(TurboQuantSnapshotError::HeaderCrcMismatch);
    }

    let version = u16::from_le_bytes(head[8..10].try_into().unwrap());
    if version != VERSION {
        return Err(TurboQuantSnapshotError::UnsupportedVersion(version));
    }
    let dim = u32::from_le_bytes(head[12..16].try_into().unwrap());
    let seed = u64::from_le_bytes(head[16..24].try_into().unwrap());
    let lsn = u64::from_le_bytes(head[24..32].try_into().unwrap());
    let n_vectors = u64::from_le_bytes(head[32..40].try_into().unwrap()) as usize;
    let body_crc_expected = u32::from_le_bytes(head[40..44].try_into().unwrap());

    if dim != expected_dim {
        return Err(TurboQuantSnapshotError::DimensionMismatch {
            expected: expected_dim,
            actual: dim,
        });
    }
    if seed != expected_seed {
        return Err(TurboQuantSnapshotError::SeedMismatch {
            expected: expected_seed,
            actual: seed,
        });
    }

    let record_bytes = 8 + 4 + dim as usize * 4;
    let mut body = vec![0u8; n_vectors * record_bytes];
    r.read_exact(&mut body).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => TurboQuantSnapshotError::Truncated,
        _ => TurboQuantSnapshotError::Io(e),
    })?;
    let body_crc_actual = crc32(&body);
    if body_crc_actual != body_crc_expected {
        return Err(TurboQuantSnapshotError::BodyCrcMismatch);
    }

    let mut vectors = Vec::with_capacity(n_vectors);
    let mut off = 0usize;
    for _ in 0..n_vectors {
        let id = u64::from_le_bytes(body[off..off + 8].try_into().unwrap());
        off += 8;
        let rec_dim = u32::from_le_bytes(body[off..off + 4].try_into().unwrap());
        off += 4;
        if rec_dim != dim {
            return Err(TurboQuantSnapshotError::DimensionMismatch {
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

    Ok(TurboQuantSnapshotPayload {
        dim,
        seed,
        lsn,
        vectors,
    })
}

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
        write_turboquant_snapshot(&path, 3, 0xdead_beef, 42, &vectors).unwrap();
        let payload = read_turboquant_snapshot(&path, 3, 0xdead_beef).unwrap();
        assert_eq!(payload.dim, 3);
        assert_eq!(payload.lsn, 42);
        assert_eq!(payload.vectors, vectors);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn read_rejects_bad_magic() {
        let path = tmp("bad-magic");
        std::fs::write(&path, b"NOTAREDDBTVSNAP").unwrap();
        let err = read_turboquant_snapshot(&path, 3, 0).unwrap_err();
        assert!(matches!(
            err,
            TurboQuantSnapshotError::BadMagic | TurboQuantSnapshotError::Truncated
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn read_detects_body_crc_corruption() {
        let path = tmp("body-crc");
        let vectors = vec![(7u64, vec![1.0, 2.0])];
        write_turboquant_snapshot(&path, 2, 1, 0, &vectors).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let idx = TURBOQUANT_SNAPSHOT_HEADER_BYTES + 2;
        bytes[idx] ^= 0xff;
        std::fs::write(&path, &bytes).unwrap();
        let err = read_turboquant_snapshot(&path, 2, 1).unwrap_err();
        assert!(matches!(err, TurboQuantSnapshotError::BodyCrcMismatch));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn read_rejects_dimension_mismatch() {
        let path = tmp("dim");
        write_turboquant_snapshot(&path, 3, 1, 0, &[(1, vec![0.0, 0.0, 0.0])]).unwrap();
        let err = read_turboquant_snapshot(&path, 4, 1).unwrap_err();
        assert!(matches!(
            err,
            TurboQuantSnapshotError::DimensionMismatch { .. }
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
