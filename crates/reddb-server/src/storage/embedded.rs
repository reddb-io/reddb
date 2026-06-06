//! Embedded single-file `.rdb` artifact skeleton.
//!
//! This module models the promoted path where one durable database artifact
//! carries its superblock pair, internal manifest, and WAL reservation inside
//! the `.rdb` file itself. It does not replace the current runtime pager/WAL
//! path yet; it establishes the on-disk contract that later slices can wire
//! into normal opens.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{RedDBError, RedDBResult, REDDB_FORMAT_VERSION};
use crate::storage::engine::crc32::crc32;

pub const EMBEDDED_RDB_SUPERBLOCK_SIZE: u64 = 4096;
pub const EMBEDDED_RDB_SUPERBLOCK_0_OFFSET: u64 = 0;
pub const EMBEDDED_RDB_SUPERBLOCK_1_OFFSET: u64 = EMBEDDED_RDB_SUPERBLOCK_SIZE;
pub const EMBEDDED_RDB_MANIFEST_OFFSET: u64 = EMBEDDED_RDB_SUPERBLOCK_SIZE * 2;

const SUPERBLOCK_MAGIC: &[u8; 8] = b"RDBSBLK1";
const MANIFEST_MAGIC: &[u8; 8] = b"RDBMNFS1";
const SUPERBLOCK_VERSION: u32 = 1;
const MANIFEST_VERSION: u32 = 1;
const CHECKSUM_LEN: usize = 4;
const MANIFEST_REGION_BYTES: u64 = 4096;
const WAL_REGION_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddedRdbManifest {
    pub version: u32,
    pub wal_region_offset: u64,
    pub wal_region_bytes: u64,
    pub wal_recovery_boundary: u64,
    pub created_at_unix_ms: u128,
    pub checksum: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddedRdbSuperblock {
    pub copy_index: u8,
    pub generation: u64,
    pub format_version: u32,
    pub manifest_offset: u64,
    pub manifest_len: u64,
    pub manifest_checksum: u32,
    pub wal_region_offset: u64,
    pub wal_region_bytes: u64,
    pub wal_recovery_boundary: u64,
    pub checksum: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedRdbOpen {
    pub path: PathBuf,
    pub selected_superblock: EmbeddedRdbSuperblock,
    pub manifest: EmbeddedRdbManifest,
}

pub struct EmbeddedRdbArtifact;

impl EmbeddedRdbArtifact {
    pub fn create(path: impl AsRef<Path>) -> RedDBResult<EmbeddedRdbOpen> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let created_at_unix_ms = now_unix_ms();
        let wal_region_offset = EMBEDDED_RDB_MANIFEST_OFFSET + MANIFEST_REGION_BYTES;
        let manifest = EmbeddedRdbManifest {
            version: MANIFEST_VERSION,
            wal_region_offset,
            wal_region_bytes: WAL_REGION_BYTES,
            wal_recovery_boundary: wal_region_offset,
            created_at_unix_ms,
            checksum: 0,
        };
        let manifest_bytes = encode_manifest(manifest);
        let manifest_checksum = trailer_checksum(&manifest_bytes);

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)?;
        file.set_len(wal_region_offset + WAL_REGION_BYTES)?;
        write_at(&mut file, EMBEDDED_RDB_MANIFEST_OFFSET, &manifest_bytes)?;

        let base = EmbeddedRdbSuperblock {
            copy_index: 0,
            generation: 1,
            format_version: REDDB_FORMAT_VERSION,
            manifest_offset: EMBEDDED_RDB_MANIFEST_OFFSET,
            manifest_len: manifest_bytes.len() as u64,
            manifest_checksum,
            wal_region_offset,
            wal_region_bytes: WAL_REGION_BYTES,
            wal_recovery_boundary: wal_region_offset,
            checksum: 0,
        };
        Self::write_superblock_copy(&mut file, &base)?;
        Self::write_superblock_copy(
            &mut file,
            &EmbeddedRdbSuperblock {
                copy_index: 1,
                generation: 2,
                ..base
            },
        )?;
        file.sync_all()?;

        Self::open(path)
    }

    pub fn open(path: impl AsRef<Path>) -> RedDBResult<EmbeddedRdbOpen> {
        let path = path.as_ref();
        let mut file = File::open(path)?;
        let superblocks = [
            read_superblock_copy(&mut file, 0),
            read_superblock_copy(&mut file, 1),
        ];
        let selected_superblock = superblocks
            .into_iter()
            .flatten()
            .max_by_key(|superblock| superblock.generation)
            .ok_or_else(|| RedDBError::InvalidOperation("no valid embedded superblock".into()))?;

        let manifest = read_manifest(&mut file, selected_superblock)?;
        Ok(EmbeddedRdbOpen {
            path: path.to_path_buf(),
            selected_superblock,
            manifest,
        })
    }

    pub fn write_superblock_copy(
        file: &mut File,
        superblock: &EmbeddedRdbSuperblock,
    ) -> RedDBResult<()> {
        let offset = superblock_offset(superblock.copy_index)?;
        write_at(file, offset, &encode_superblock(*superblock)?)?;
        Ok(())
    }
}

fn read_superblock_copy(file: &mut File, copy_index: u8) -> Option<EmbeddedRdbSuperblock> {
    let offset = superblock_offset(copy_index).ok()?;
    let mut bytes = vec![0u8; EMBEDDED_RDB_SUPERBLOCK_SIZE as usize];
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(&mut bytes).ok()?;
    decode_superblock(copy_index, &bytes).ok()
}

fn read_manifest(
    file: &mut File,
    superblock: EmbeddedRdbSuperblock,
) -> RedDBResult<EmbeddedRdbManifest> {
    if superblock.manifest_len < CHECKSUM_LEN as u64
        || superblock.manifest_len > MANIFEST_REGION_BYTES
    {
        return Err(RedDBError::InvalidOperation(format!(
            "invalid embedded manifest length {}",
            superblock.manifest_len
        )));
    }

    let mut bytes = vec![0u8; superblock.manifest_len as usize];
    file.seek(SeekFrom::Start(superblock.manifest_offset))?;
    file.read_exact(&mut bytes)?;
    let checksum = trailer_checksum(&bytes);
    if checksum != superblock.manifest_checksum {
        return Err(RedDBError::InvalidOperation(format!(
            "embedded manifest checksum mismatch: stored {:#010x}, computed {:#010x}",
            superblock.manifest_checksum, checksum
        )));
    }
    decode_manifest(&bytes)
}

fn encode_superblock(superblock: EmbeddedRdbSuperblock) -> RedDBResult<Vec<u8>> {
    let mut bytes = vec![0u8; EMBEDDED_RDB_SUPERBLOCK_SIZE as usize];
    let mut cursor = 0usize;
    put_bytes(&mut bytes, &mut cursor, SUPERBLOCK_MAGIC);
    put_u32(&mut bytes, &mut cursor, SUPERBLOCK_VERSION);
    put_u8(&mut bytes, &mut cursor, superblock.copy_index);
    put_u64(&mut bytes, &mut cursor, superblock.generation);
    put_u32(&mut bytes, &mut cursor, superblock.format_version);
    put_u64(&mut bytes, &mut cursor, superblock.manifest_offset);
    put_u64(&mut bytes, &mut cursor, superblock.manifest_len);
    put_u32(&mut bytes, &mut cursor, superblock.manifest_checksum);
    put_u64(&mut bytes, &mut cursor, superblock.wal_region_offset);
    put_u64(&mut bytes, &mut cursor, superblock.wal_region_bytes);
    put_u64(&mut bytes, &mut cursor, superblock.wal_recovery_boundary);

    let checksum_offset = bytes.len() - CHECKSUM_LEN;
    let checksum = crc32(&bytes[..checksum_offset]);
    bytes[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());
    Ok(bytes)
}

fn decode_superblock(copy_index: u8, bytes: &[u8]) -> RedDBResult<EmbeddedRdbSuperblock> {
    if bytes.len() != EMBEDDED_RDB_SUPERBLOCK_SIZE as usize {
        return Err(RedDBError::InvalidOperation(
            "invalid embedded superblock size".into(),
        ));
    }
    let checksum_offset = bytes.len() - CHECKSUM_LEN;
    let stored_checksum = u32::from_le_bytes(bytes[checksum_offset..].try_into().unwrap());
    let computed_checksum = crc32(&bytes[..checksum_offset]);
    if stored_checksum != computed_checksum {
        return Err(RedDBError::InvalidOperation(
            "embedded superblock checksum mismatch".into(),
        ));
    }

    let mut cursor = 0usize;
    if take_bytes(bytes, &mut cursor, SUPERBLOCK_MAGIC.len())? != SUPERBLOCK_MAGIC {
        return Err(RedDBError::InvalidOperation(
            "invalid embedded superblock magic".into(),
        ));
    }
    let version = take_u32(bytes, &mut cursor)?;
    if version != SUPERBLOCK_VERSION {
        return Err(RedDBError::InvalidOperation(format!(
            "unsupported embedded superblock version {version}"
        )));
    }
    let stored_copy_index = take_u8(bytes, &mut cursor)?;
    if stored_copy_index != copy_index {
        return Err(RedDBError::InvalidOperation(
            "embedded superblock copy index mismatch".into(),
        ));
    }

    Ok(EmbeddedRdbSuperblock {
        copy_index: stored_copy_index,
        generation: take_u64(bytes, &mut cursor)?,
        format_version: take_u32(bytes, &mut cursor)?,
        manifest_offset: take_u64(bytes, &mut cursor)?,
        manifest_len: take_u64(bytes, &mut cursor)?,
        manifest_checksum: take_u32(bytes, &mut cursor)?,
        wal_region_offset: take_u64(bytes, &mut cursor)?,
        wal_region_bytes: take_u64(bytes, &mut cursor)?,
        wal_recovery_boundary: take_u64(bytes, &mut cursor)?,
        checksum: stored_checksum,
    })
}

fn encode_manifest(manifest: EmbeddedRdbManifest) -> Vec<u8> {
    let mut bytes = vec![0u8; 8 + 4 + 8 + 8 + 8 + 16 + CHECKSUM_LEN];
    let mut cursor = 0usize;
    put_bytes(&mut bytes, &mut cursor, MANIFEST_MAGIC);
    put_u32(&mut bytes, &mut cursor, manifest.version);
    put_u64(&mut bytes, &mut cursor, manifest.wal_region_offset);
    put_u64(&mut bytes, &mut cursor, manifest.wal_region_bytes);
    put_u64(&mut bytes, &mut cursor, manifest.wal_recovery_boundary);
    put_u128(&mut bytes, &mut cursor, manifest.created_at_unix_ms);

    let checksum_offset = bytes.len() - CHECKSUM_LEN;
    let checksum = crc32(&bytes[..checksum_offset]);
    bytes[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());
    bytes
}

fn decode_manifest(bytes: &[u8]) -> RedDBResult<EmbeddedRdbManifest> {
    let checksum_offset = bytes
        .len()
        .checked_sub(CHECKSUM_LEN)
        .ok_or_else(|| RedDBError::InvalidOperation("embedded manifest too short".into()))?;
    let stored_checksum = u32::from_le_bytes(bytes[checksum_offset..].try_into().unwrap());
    let computed_checksum = crc32(&bytes[..checksum_offset]);
    if stored_checksum != computed_checksum {
        return Err(RedDBError::InvalidOperation(
            "embedded manifest checksum mismatch".into(),
        ));
    }

    let mut cursor = 0usize;
    if take_bytes(bytes, &mut cursor, MANIFEST_MAGIC.len())? != MANIFEST_MAGIC {
        return Err(RedDBError::InvalidOperation(
            "invalid embedded manifest magic".into(),
        ));
    }
    let version = take_u32(bytes, &mut cursor)?;
    if version != MANIFEST_VERSION {
        return Err(RedDBError::InvalidOperation(format!(
            "unsupported embedded manifest version {version}"
        )));
    }
    Ok(EmbeddedRdbManifest {
        version,
        wal_region_offset: take_u64(bytes, &mut cursor)?,
        wal_region_bytes: take_u64(bytes, &mut cursor)?,
        wal_recovery_boundary: take_u64(bytes, &mut cursor)?,
        created_at_unix_ms: take_u128(bytes, &mut cursor)?,
        checksum: stored_checksum,
    })
}

fn trailer_checksum(bytes: &[u8]) -> u32 {
    let checksum_offset = bytes.len() - CHECKSUM_LEN;
    u32::from_le_bytes(bytes[checksum_offset..].try_into().unwrap())
}

fn superblock_offset(copy_index: u8) -> RedDBResult<u64> {
    match copy_index {
        0 => Ok(EMBEDDED_RDB_SUPERBLOCK_0_OFFSET),
        1 => Ok(EMBEDDED_RDB_SUPERBLOCK_1_OFFSET),
        _ => Err(RedDBError::InvalidOperation(format!(
            "invalid embedded superblock copy index {copy_index}"
        ))),
    }
}

fn write_at(file: &mut File, offset: u64, bytes: &[u8]) -> RedDBResult<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(bytes)?;
    Ok(())
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn put_bytes(target: &mut [u8], cursor: &mut usize, value: &[u8]) {
    target[*cursor..*cursor + value.len()].copy_from_slice(value);
    *cursor += value.len();
}

fn put_u8(target: &mut [u8], cursor: &mut usize, value: u8) {
    target[*cursor] = value;
    *cursor += 1;
}

fn put_u32(target: &mut [u8], cursor: &mut usize, value: u32) {
    put_bytes(target, cursor, &value.to_le_bytes());
}

fn put_u64(target: &mut [u8], cursor: &mut usize, value: u64) {
    put_bytes(target, cursor, &value.to_le_bytes());
}

fn put_u128(target: &mut [u8], cursor: &mut usize, value: u128) {
    put_bytes(target, cursor, &value.to_le_bytes());
}

fn take_bytes<'a>(bytes: &'a [u8], cursor: &mut usize, len: usize) -> RedDBResult<&'a [u8]> {
    let end = cursor
        .checked_add(len)
        .ok_or_else(|| RedDBError::InvalidOperation("embedded artifact cursor overflow".into()))?;
    if end > bytes.len() {
        return Err(RedDBError::InvalidOperation(
            "embedded artifact truncated".into(),
        ));
    }
    let value = &bytes[*cursor..end];
    *cursor = end;
    Ok(value)
}

fn take_u8(bytes: &[u8], cursor: &mut usize) -> RedDBResult<u8> {
    Ok(take_bytes(bytes, cursor, 1)?[0])
}

fn take_u32(bytes: &[u8], cursor: &mut usize) -> RedDBResult<u32> {
    Ok(u32::from_le_bytes(
        take_bytes(bytes, cursor, 4)?.try_into().unwrap(),
    ))
}

fn take_u64(bytes: &[u8], cursor: &mut usize) -> RedDBResult<u64> {
    Ok(u64::from_le_bytes(
        take_bytes(bytes, cursor, 8)?.try_into().unwrap(),
    ))
}

fn take_u128(bytes: &[u8], cursor: &mut usize) -> RedDBResult<u128> {
    Ok(u128::from_le_bytes(
        take_bytes(bytes, cursor, 16)?.try_into().unwrap(),
    ))
}
