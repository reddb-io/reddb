//! Zone map persistence — Phase 5 / PLAN.md backlog 3.3.
//!
//! `storage/index/zone_map.rs` keeps per-block (min, max,
//! null_count) tuples in memory but never serialises them.
//! Without persistence, every restart drops the planner's
//! pruning hints and queries fall back to full scans until
//! the in-memory map repopulates lazily.
//!
//! This module ships the on-disk format + read/write helpers.
//! The wiring commit hooks them into:
//! - btree write paths (update zone-map sidecar on insert/delete)
//! - startup loader (`Database::open` reads the sidecar)
//! - planner pushdown (`logical.rs::filter_pushdown` consults
//!   the in-memory map, which is now warm from disk)
//!
//! ## File format
//!
//! One sidecar file per indexed table, named
//! `{table}.zonemap` in the database directory. The format is
//! a deliberately simple repeated record:
//!
//! ```text
//! [u32 magic = 0x5A4D4150 ("ZMAP")]
//! [u32 version = 1]
//! [u32 column_count]
//! [u32 zone_count]
//! repeated zone_count times:
//!     [u32 column_index]
//!     [u32 block_id]
//!     [u32 min_value_len][min_value_bytes]
//!     [u32 max_value_len][max_value_bytes]
//!     [u64 null_count]
//!     [u64 row_count]
//! ```
//!
//! All ints are little-endian. Values are stored as their
//! `display_string()` UTF-8 form so the format is type-
//! agnostic — the planner re-parses on load via
//! `schema::coerce`.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

const MAGIC: u32 = 0x5A4D4150; // "ZMAP"
const VERSION: u32 = 1;

/// One zone-map record on disk. Mirrors the in-memory
/// `zone_map::ZoneMap` shape but with stringified bounds for
/// portability across reddb type changes.
#[derive(Debug, Clone)]
pub struct PersistedZone {
    pub column_index: u32,
    pub block_id: u32,
    pub min_value: String,
    pub max_value: String,
    pub null_count: u64,
    pub row_count: u64,
}

/// Errors raised by the persistence layer.
#[derive(Debug)]
pub enum ZoneMapPersistError {
    Io(std::io::Error),
    BadMagic { found: u32 },
    BadVersion { found: u32 },
    Truncated,
    InvalidUtf8(std::string::FromUtf8Error),
}

impl From<std::io::Error> for ZoneMapPersistError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<std::string::FromUtf8Error> for ZoneMapPersistError {
    fn from(e: std::string::FromUtf8Error) -> Self {
        Self::InvalidUtf8(e)
    }
}

impl std::fmt::Display for ZoneMapPersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "zone-map i/o: {e}"),
            Self::BadMagic { found } => {
                write!(f, "zone-map bad magic: expected {MAGIC:#x}, got {found:#x}")
            }
            Self::BadVersion { found } => {
                write!(f, "zone-map version {found} not supported (max {VERSION})")
            }
            Self::Truncated => write!(f, "zone-map file ended unexpectedly"),
            Self::InvalidUtf8(e) => write!(f, "zone-map utf8 error: {e}"),
        }
    }
}

impl std::error::Error for ZoneMapPersistError {}

/// Write a list of zones to the given sidecar path, replacing
/// any existing file. Atomic via write-to-temp + rename.
pub fn write_sidecar(
    path: &Path,
    column_count: u32,
    zones: &[PersistedZone],
) -> Result<(), ZoneMapPersistError> {
    let tmp_path = path.with_extension("zonemap.tmp");
    {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)?;
        let mut w = BufWriter::new(file);
        w.write_all(&MAGIC.to_le_bytes())?;
        w.write_all(&VERSION.to_le_bytes())?;
        w.write_all(&column_count.to_le_bytes())?;
        w.write_all(&(zones.len() as u32).to_le_bytes())?;
        for z in zones {
            w.write_all(&z.column_index.to_le_bytes())?;
            w.write_all(&z.block_id.to_le_bytes())?;
            write_str(&mut w, &z.min_value)?;
            write_str(&mut w, &z.max_value)?;
            w.write_all(&z.null_count.to_le_bytes())?;
            w.write_all(&z.row_count.to_le_bytes())?;
        }
        w.flush()?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Read a sidecar file produced by `write_sidecar`. Returns
/// `(column_count, zones)`.
pub fn read_sidecar(
    path: &Path,
) -> Result<(u32, Vec<PersistedZone>), ZoneMapPersistError> {
    let file = File::open(path)?;
    let mut r = BufReader::new(file);
    let magic = read_u32(&mut r)?;
    if magic != MAGIC {
        return Err(ZoneMapPersistError::BadMagic { found: magic });
    }
    let version = read_u32(&mut r)?;
    if version != VERSION {
        return Err(ZoneMapPersistError::BadVersion { found: version });
    }
    let column_count = read_u32(&mut r)?;
    let zone_count = read_u32(&mut r)?;
    let mut zones = Vec::with_capacity(zone_count as usize);
    for _ in 0..zone_count {
        let column_index = read_u32(&mut r)?;
        let block_id = read_u32(&mut r)?;
        let min_value = read_str(&mut r)?;
        let max_value = read_str(&mut r)?;
        let null_count = read_u64(&mut r)?;
        let row_count = read_u64(&mut r)?;
        zones.push(PersistedZone {
            column_index,
            block_id,
            min_value,
            max_value,
            null_count,
            row_count,
        });
    }
    Ok((column_count, zones))
}

fn write_str<W: Write>(w: &mut W, s: &str) -> Result<(), ZoneMapPersistError> {
    let bytes = s.as_bytes();
    w.write_all(&(bytes.len() as u32).to_le_bytes())?;
    w.write_all(bytes)?;
    Ok(())
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32, ZoneMapPersistError> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf).map_err(|_| ZoneMapPersistError::Truncated)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64<R: Read>(r: &mut R) -> Result<u64, ZoneMapPersistError> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf).map_err(|_| ZoneMapPersistError::Truncated)?;
    Ok(u64::from_le_bytes(buf))
}

fn read_str<R: Read>(r: &mut R) -> Result<String, ZoneMapPersistError> {
    let len = read_u32(r)?;
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).map_err(|_| ZoneMapPersistError::Truncated)?;
    Ok(String::from_utf8(buf)?)
}
