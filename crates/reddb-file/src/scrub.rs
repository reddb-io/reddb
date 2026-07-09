use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Instant;

use crate::{
    RdbFileError, RdbFileResult, EMBEDDED_RDB_SUPERBLOCK_0_OFFSET,
    EMBEDDED_RDB_SUPERBLOCK_1_OFFSET, EMBEDDED_RDB_SUPERBLOCK_SIZE,
};

const CHECKSUM_LEN: usize = 4;
const SUPERBLOCK_MAGIC: &[u8; 8] = b"RDBSBLK1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageScrubFinding {
    pub zone_kind: String,
    pub physical_identity: String,
    pub collection: Option<String>,
    pub expected_checksum: Option<String>,
    pub actual_checksum: Option<String>,
    pub fault_class: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageScrubVerifiedCounters {
    pub superblock: u64,
    pub manifest: u64,
    pub wal: u64,
    pub page: u64,
    pub segment_chunk: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageScrubReport {
    pub findings: Vec<StorageScrubFinding>,
    pub verified: StorageScrubVerifiedCounters,
    pub objects_verified: u64,
    pub total_objects: u64,
    pub bytes_read: u64,
    pub duration_ms: u64,
    pub next_cursor: usize,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy)]
struct ParsedSuperblock {
    generation: u64,
    manifest_offset: u64,
    manifest_len: u64,
    manifest_checksum: u32,
    wal_recovery_boundary: u64,
    snapshot_offset: u64,
    snapshot_bytes: u64,
    snapshot_checksum: u32,
}

pub fn scrub_embedded_store(
    path: &Path,
    start_cursor: usize,
    max_objects: usize,
) -> RdbFileResult<StorageScrubReport> {
    let started = Instant::now();
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut report = StorageScrubReport {
        total_objects: 5,
        next_cursor: start_cursor,
        ..StorageScrubReport::default()
    };

    let mut superblocks = Vec::new();
    let end_cursor = start_cursor.saturating_add(max_objects.max(1)).min(5);
    for object_index in start_cursor..end_cursor {
        match object_index {
            0 => verify_superblock(&mut file, 0, &mut report, &mut superblocks)?,
            1 => verify_superblock(&mut file, 1, &mut report, &mut superblocks)?,
            2 => verify_manifest(&mut file, &mut report, &superblocks, file_len)?,
            3 => verify_snapshot(&mut file, &mut report, &superblocks, file_len)?,
            4 => verify_wal_boundary(&mut file, &mut report, &superblocks, file_len)?,
            _ => {}
        }
        report.next_cursor = object_index + 1;
    }

    report.complete = report.next_cursor >= 5;
    report.duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    Ok(report)
}

fn verify_superblock(
    file: &mut File,
    copy_index: u8,
    report: &mut StorageScrubReport,
    superblocks: &mut Vec<ParsedSuperblock>,
) -> RdbFileResult<()> {
    let offset = match copy_index {
        0 => EMBEDDED_RDB_SUPERBLOCK_0_OFFSET,
        1 => EMBEDDED_RDB_SUPERBLOCK_1_OFFSET,
        _ => unreachable!(),
    };
    let mut bytes = vec![0u8; EMBEDDED_RDB_SUPERBLOCK_SIZE as usize];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut bytes)?;
    report.bytes_read += bytes.len() as u64;
    report.objects_verified += 1;

    let stored = checksum_trailer(&bytes)?;
    let actual = crc32(&bytes[..bytes.len() - CHECKSUM_LEN]);
    let identity = format!("superblock:{copy_index}");
    if stored != actual {
        report.findings.push(StorageScrubFinding {
            zone_kind: "superblock".to_string(),
            physical_identity: identity,
            collection: None,
            expected_checksum: Some(hex32(stored)),
            actual_checksum: Some(hex32(actual)),
            fault_class: Some("bit-rot-evidence".to_string()),
        });
        return Ok(());
    }

    if let Some(parsed) = parse_superblock(copy_index, &bytes) {
        superblocks.push(parsed);
    }
    report.verified.superblock += 1;
    Ok(())
}

fn verify_manifest(
    file: &mut File,
    report: &mut StorageScrubReport,
    superblocks: &[ParsedSuperblock],
    file_len: u64,
) -> RdbFileResult<()> {
    report.objects_verified += 1;
    let Some(superblock) = newest_superblock(superblocks) else {
        report
            .findings
            .push(missing_authority("manifest", "manifest"));
        return Ok(());
    };
    if superblock.manifest_len < CHECKSUM_LEN as u64
        || superblock
            .manifest_offset
            .saturating_add(superblock.manifest_len)
            > file_len
    {
        report.findings.push(StorageScrubFinding {
            zone_kind: "manifest".to_string(),
            physical_identity: "manifest".to_string(),
            collection: None,
            expected_checksum: Some(hex32(superblock.manifest_checksum)),
            actual_checksum: None,
            fault_class: Some("torn-write-evidence".to_string()),
        });
        return Ok(());
    }
    let mut bytes = vec![0u8; superblock.manifest_len as usize];
    file.seek(SeekFrom::Start(superblock.manifest_offset))?;
    file.read_exact(&mut bytes)?;
    report.bytes_read += bytes.len() as u64;
    let actual = crc32(&bytes[..bytes.len() - CHECKSUM_LEN]);
    if actual != superblock.manifest_checksum {
        report.findings.push(StorageScrubFinding {
            zone_kind: "manifest".to_string(),
            physical_identity: "manifest".to_string(),
            collection: None,
            expected_checksum: Some(hex32(superblock.manifest_checksum)),
            actual_checksum: Some(hex32(actual)),
            fault_class: Some("bit-rot-evidence".to_string()),
        });
        return Ok(());
    }
    report.verified.manifest += 1;
    Ok(())
}

fn verify_snapshot(
    file: &mut File,
    report: &mut StorageScrubReport,
    superblocks: &[ParsedSuperblock],
    file_len: u64,
) -> RdbFileResult<()> {
    report.objects_verified += 1;
    let Some(superblock) = newest_superblock(superblocks) else {
        report.findings.push(missing_authority("page", "snapshot"));
        return Ok(());
    };
    if superblock.snapshot_bytes == 0 {
        report.verified.page += 1;
        return Ok(());
    }
    if superblock
        .snapshot_offset
        .saturating_add(superblock.snapshot_bytes)
        > file_len
    {
        report.findings.push(StorageScrubFinding {
            zone_kind: "page".to_string(),
            physical_identity: "snapshot".to_string(),
            collection: None,
            expected_checksum: Some(hex32(superblock.snapshot_checksum)),
            actual_checksum: None,
            fault_class: Some("torn-write-evidence".to_string()),
        });
        return Ok(());
    }
    let mut bytes = vec![0u8; superblock.snapshot_bytes as usize];
    file.seek(SeekFrom::Start(superblock.snapshot_offset))?;
    file.read_exact(&mut bytes)?;
    report.bytes_read += bytes.len() as u64;
    let actual = crc32(&bytes);
    if actual != superblock.snapshot_checksum {
        report.findings.push(StorageScrubFinding {
            zone_kind: "page".to_string(),
            physical_identity: "snapshot".to_string(),
            collection: None,
            expected_checksum: Some(hex32(superblock.snapshot_checksum)),
            actual_checksum: Some(hex32(actual)),
            fault_class: Some("bit-rot-evidence".to_string()),
        });
        return Ok(());
    }
    report.verified.page += 1;
    Ok(())
}

fn verify_wal_boundary(
    _file: &mut File,
    report: &mut StorageScrubReport,
    superblocks: &[ParsedSuperblock],
    file_len: u64,
) -> RdbFileResult<()> {
    report.objects_verified += 1;
    let Some(superblock) = newest_superblock(superblocks) else {
        report
            .findings
            .push(missing_authority("wal", "embedded-wal"));
        return Ok(());
    };
    if superblock.wal_recovery_boundary > file_len {
        report.findings.push(StorageScrubFinding {
            zone_kind: "wal".to_string(),
            physical_identity: "embedded-wal".to_string(),
            collection: None,
            expected_checksum: None,
            actual_checksum: None,
            fault_class: Some("torn-write-evidence".to_string()),
        });
        return Ok(());
    }
    report.verified.wal += 1;
    Ok(())
}

fn parse_superblock(copy_index: u8, bytes: &[u8]) -> Option<ParsedSuperblock> {
    if bytes.len() != EMBEDDED_RDB_SUPERBLOCK_SIZE as usize || &bytes[..8] != SUPERBLOCK_MAGIC {
        return None;
    }
    let stored_copy_index = bytes[12];
    if stored_copy_index != copy_index {
        return None;
    }
    Some(ParsedSuperblock {
        generation: read_u64(bytes, 13)?,
        manifest_offset: read_u64(bytes, 25)?,
        manifest_len: read_u64(bytes, 33)?,
        manifest_checksum: read_u32(bytes, 41)?,
        wal_recovery_boundary: read_u64(bytes, 61)?,
        snapshot_offset: read_u64(bytes, 69)?,
        snapshot_bytes: read_u64(bytes, 77)?,
        snapshot_checksum: read_u32(bytes, 85)?,
    })
}

fn newest_superblock(superblocks: &[ParsedSuperblock]) -> Option<ParsedSuperblock> {
    superblocks
        .iter()
        .max_by_key(|superblock| superblock.generation)
        .copied()
}

fn missing_authority(zone_kind: &str, physical_identity: &str) -> StorageScrubFinding {
    StorageScrubFinding {
        zone_kind: zone_kind.to_string(),
        physical_identity: physical_identity.to_string(),
        collection: None,
        expected_checksum: None,
        actual_checksum: None,
        fault_class: Some("missing-checksum-authority".to_string()),
    }
}

fn checksum_trailer(bytes: &[u8]) -> RdbFileResult<u32> {
    if bytes.len() < CHECKSUM_LEN {
        return Err(RdbFileError::InvalidOperation(
            "checksum trailer requires at least four bytes".to_string(),
        ));
    }
    Ok(u32::from_le_bytes(
        bytes[bytes.len() - CHECKSUM_LEN..].try_into().unwrap(),
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

fn hex32(value: u32) -> String {
    format!("crc32:{value:08x}")
}
