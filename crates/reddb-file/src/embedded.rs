//! Embedded single-file `.rdb` artifact.
//!
//! This module models the promoted path where one durable database artifact
//! carries its superblock pair, internal manifest, WAL reservation, and current
//! store snapshot inside the `.rdb` file itself.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;

pub type RdbFileResult<T> = Result<T, RdbFileError>;

pub const DEFAULT_FORMAT_VERSION: u32 = 1;

#[derive(Debug)]
pub enum RdbFileError {
    InvalidOperation(String),
    Io(std::io::Error),
    /// A named zone of the `.rdb` cannot be trusted, so the store does not
    /// open. ADR 0074 §2 requires the zone be named and §4 requires the
    /// operator be pointed at `red salvage` rather than left with a panic.
    ZoneUnrecoverable {
        zone: &'static str,
        path: PathBuf,
    },
}

impl std::fmt::Display for RdbFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOperation(msg) => write!(f, "INVALID_OPERATION: {msg}"),
            Self::Io(err) => write!(f, "io error: {err}"),
            Self::ZoneUnrecoverable { zone, path } => write!(
                f,
                "{zone} zone of {} failed validation, so the store will not be opened \
                 (opening it could only return data the zone can no longer vouch for). \
                 Run scrub to classify the fault and red salvage to extract every entity the \
                 damage did not touch; red salvage never writes into the damaged file \
                 (ADR 0074 §2/§4).",
                path.display()
            ),
        }
    }
}

impl std::error::Error for RdbFileError {}

impl From<std::io::Error> for RdbFileError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(data);
    hasher.finalize()
}

pub const EMBEDDED_RDB_SUPERBLOCK_SIZE: u64 = 4096;
pub const EMBEDDED_RDB_SUPERBLOCK_0_OFFSET: u64 = 0;
pub const EMBEDDED_RDB_SUPERBLOCK_1_OFFSET: u64 = EMBEDDED_RDB_SUPERBLOCK_SIZE;

/// The manifest zone is a ping-pong pair, exactly like the superblock zone.
///
/// A single in-place manifest could not satisfy "never torn": overwriting it
/// leaves a window in which the durable superblock still names the old
/// checksum while the bytes are already the new ones. Publishing into the
/// *inactive* slot and only then pointing a fresh superblock at it means every
/// valid superblock always references an intact manifest — pre-update or
/// post-update, never a mixture.
pub const EMBEDDED_RDB_MANIFEST_SLOT_SIZE: u64 = 4096;
pub const EMBEDDED_RDB_MANIFEST_0_OFFSET: u64 = EMBEDDED_RDB_SUPERBLOCK_SIZE * 2;
pub const EMBEDDED_RDB_MANIFEST_1_OFFSET: u64 =
    EMBEDDED_RDB_MANIFEST_0_OFFSET + EMBEDDED_RDB_MANIFEST_SLOT_SIZE;
pub const EMBEDDED_RDB_MANIFEST_ZONE_END: u64 =
    EMBEDDED_RDB_MANIFEST_1_OFFSET + EMBEDDED_RDB_MANIFEST_SLOT_SIZE;

const SUPERBLOCK_MAGIC: &[u8; 8] = b"RDBSBLK1";
const MANIFEST_MAGIC: &[u8; 8] = b"RDBMNFS1";
const SUPERBLOCK_VERSION: u32 = 2;
const MANIFEST_VERSION: u32 = 2;
const LEGACY_SUPERBLOCK_VERSION: u32 = 1;
const LEGACY_MANIFEST_VERSION: u32 = 1;
const CHECKSUM_LEN: usize = 4;
const MANIFEST_REGION_BYTES: u64 = EMBEDDED_RDB_MANIFEST_SLOT_SIZE;
const WAL_REGION_BYTES: u64 = 64 * 1024;
const SNAPSHOT_ALIGNMENT: u64 = 4096;
const SNAPSHOT_MAGIC: &[u8; 4] = b"RDST";
const WAL_FRAME_MAGIC: &[u8; 8] = b"RDBEWAL1";
const WAL_FRAME_VERSION: u16 = 2;
const WAL_FRAME_HEADER_BYTES: usize = 8 + 2 + 2 + 8 + 4 + 4 + 4 + 4;
const LEGACY_WAL_FRAME_HEADER_BYTES: usize = 8 + 4 + 4;
const CRASH_INJECT_ENV: &str = "REDDB_EMBEDDED_RDB_CRASH_AT";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddedRdbManifest {
    pub version: u32,
    pub wal_region_offset: u64,
    pub wal_region_bytes: u64,
    pub wal_recovery_boundary: u64,
    pub wal_live_bytes: u64,
    pub snapshot_offset: u64,
    pub snapshot_bytes: u64,
    pub snapshot_checksum: u32,
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
    pub wal_live_bytes: u64,
    pub snapshot_offset: u64,
    pub snapshot_bytes: u64,
    pub snapshot_checksum: u32,
    pub checksum: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedRdbOpen {
    pub path: PathBuf,
    pub selected_superblock: EmbeddedRdbSuperblock,
    pub manifest: EmbeddedRdbManifest,
}

#[derive(Debug, Default)]
struct WalScan {
    payloads: Vec<Vec<u8>>,
    next_sequence: u64,
    previous_frame_crc: u32,
    valid_bytes: u64,
}

pub struct EmbeddedRdbArtifact;

impl EmbeddedRdbArtifact {
    pub fn create(path: impl AsRef<Path>) -> RdbFileResult<EmbeddedRdbOpen> {
        Self::create_with_snapshot(path, &[])
    }

    pub fn create_with_snapshot(
        path: impl AsRef<Path>,
        snapshot: &[u8],
    ) -> RdbFileResult<EmbeddedRdbOpen> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let created_at_unix_ms = now_unix_ms();
        let wal_region_offset = EMBEDDED_RDB_MANIFEST_ZONE_END;
        let snapshot_offset = wal_region_offset + WAL_REGION_BYTES;
        let manifest = EmbeddedRdbManifest {
            version: MANIFEST_VERSION,
            wal_region_offset,
            wal_region_bytes: WAL_REGION_BYTES,
            wal_recovery_boundary: wal_region_offset,
            wal_live_bytes: 0,
            snapshot_offset,
            snapshot_bytes: snapshot.len() as u64,
            snapshot_checksum: crc32(snapshot),
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
        file.set_len(snapshot_offset + snapshot.len() as u64)?;
        write_at(&mut file, EMBEDDED_RDB_MANIFEST_0_OFFSET, &manifest_bytes)?;
        if !snapshot.is_empty() {
            write_at(&mut file, snapshot_offset, snapshot)?;
        }

        let base = EmbeddedRdbSuperblock {
            copy_index: 0,
            generation: 1,
            format_version: DEFAULT_FORMAT_VERSION,
            manifest_offset: EMBEDDED_RDB_MANIFEST_0_OFFSET,
            manifest_len: manifest_bytes.len() as u64,
            manifest_checksum,
            wal_region_offset,
            wal_region_bytes: WAL_REGION_BYTES,
            wal_recovery_boundary: wal_region_offset,
            wal_live_bytes: 0,
            snapshot_offset,
            snapshot_bytes: snapshot.len() as u64,
            snapshot_checksum: crc32(snapshot),
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

    pub fn open(path: impl AsRef<Path>) -> RdbFileResult<EmbeddedRdbOpen> {
        Self::open_inner(path, true)
    }

    fn open_for_wal_append(path: impl AsRef<Path>) -> RdbFileResult<EmbeddedRdbOpen> {
        Self::open_inner(path, false)
    }

    fn open_inner(
        path: impl AsRef<Path>,
        validate_snapshot_refs: bool,
    ) -> RdbFileResult<EmbeddedRdbOpen> {
        let path = path.as_ref();
        let mut file = File::open(path)?;
        let mut superblocks: Vec<EmbeddedRdbSuperblock> = [
            read_superblock_copy(&mut file, 0),
            read_superblock_copy(&mut file, 1),
        ]
        .into_iter()
        .flatten()
        .collect();
        superblocks.sort_by_key(|superblock| std::cmp::Reverse(superblock.generation));

        if superblocks.is_empty() {
            return Err(RdbFileError::ZoneUnrecoverable {
                zone: "superblock",
                path: path.to_path_buf(),
            });
        }

        for selected_superblock in superblocks {
            // The manifest a valid superblock names is always intact: it was
            // fsynced into an inactive slot before this generation existed. A
            // checksum failure here is therefore bit rot, not a torn update,
            // and ADR 0074 §2 says it fails the open by name — never falls
            // back to a stale root that would resurrect superseded state.
            let mut manifest = read_manifest(&mut file, selected_superblock).map_err(|_| {
                RdbFileError::ZoneUnrecoverable {
                    zone: "manifest",
                    path: path.to_path_buf(),
                }
            })?;
            manifest.wal_recovery_boundary = selected_superblock.wal_recovery_boundary;
            manifest.wal_live_bytes = selected_superblock.wal_live_bytes;
            if validate_snapshot_refs && !snapshot_reference_valid(&mut file, &manifest)? {
                continue;
            }
            return Ok(EmbeddedRdbOpen {
                path: path.to_path_buf(),
                selected_superblock,
                manifest,
            });
        }

        Err(RdbFileError::ZoneUnrecoverable {
            zone: "snapshot",
            path: path.to_path_buf(),
        })
    }

    pub fn wal_payloads_encoded_len(payloads: &[Vec<u8>]) -> RdbFileResult<u64> {
        let mut len = 0u64;
        for payload in payloads {
            let payload_len = u32::try_from(payload.len()).map_err(|_| {
                RdbFileError::InvalidOperation("embedded wal payload too large".into())
            })?;
            let frame_len = WAL_FRAME_HEADER_BYTES as u64 + payload_len as u64;
            len = len.checked_add(frame_len).ok_or_else(|| {
                RdbFileError::InvalidOperation("embedded wal encoded length overflow".into())
            })?;
        }
        Ok(len)
    }

    pub fn write_snapshot_with_wal_capacity(
        path: impl AsRef<Path>,
        snapshot: &[u8],
        min_wal_bytes: u64,
    ) -> RdbFileResult<EmbeddedRdbOpen> {
        let path = path.as_ref();
        let path_lock = embedded_path_lock(path);
        let _path_guard = path_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let lock_file = OpenOptions::new().read(true).write(true).open(path)?;
        lock_file.lock_exclusive()?;

        let open = Self::open(path)?;
        let wal_scan = scan_wal(&open)?;
        let checkpoint_boundary = wal_boundary_after_live_bytes(&open, wal_scan.valid_bytes)?;
        let wal_region_bytes =
            grow_wal_region_bytes(open.manifest.wal_region_bytes, min_wal_bytes)?;
        let snapshot_offset = next_snapshot_offset(path, &open, wal_region_bytes, snapshot)?;
        let snapshot_checksum = crc32(snapshot);
        let manifest = EmbeddedRdbManifest {
            wal_region_bytes,
            wal_recovery_boundary: checkpoint_boundary,
            wal_live_bytes: 0,
            snapshot_offset,
            snapshot_bytes: snapshot.len() as u64,
            snapshot_checksum,
            checksum: 0,
            ..open.manifest
        };
        let manifest_bytes = encode_manifest(manifest);
        let manifest_checksum = trailer_checksum(&manifest_bytes);

        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        file.set_len(snapshot_offset + snapshot.len() as u64)?;
        if !snapshot.is_empty() {
            write_at(&mut file, snapshot_offset, snapshot)?;
        }
        crash_inject("snapshot_after_image_write");
        file.sync_data()?;
        crash_inject("snapshot_after_image_sync");

        // Publish the manifest into the slot the live superblock does NOT
        // reference, and make it durable before any superblock names it. Until
        // the superblock write below lands, the durable state still roots
        // through the old manifest slot, which these bytes never touched.
        let next_manifest_offset = inactive_manifest_offset(open.selected_superblock)?;
        write_at(&mut file, next_manifest_offset, &manifest_bytes)?;
        crash_inject("snapshot_after_manifest_write");
        file.sync_data()?;
        crash_inject("snapshot_after_manifest_sync");

        let next_copy_index = if open.selected_superblock.copy_index == 0 {
            1
        } else {
            0
        };
        let next_superblock = EmbeddedRdbSuperblock {
            copy_index: next_copy_index,
            generation: open.selected_superblock.generation.saturating_add(1),
            manifest_offset: next_manifest_offset,
            manifest_len: manifest_bytes.len() as u64,
            manifest_checksum,
            wal_region_bytes,
            wal_recovery_boundary: checkpoint_boundary,
            wal_live_bytes: 0,
            snapshot_offset,
            snapshot_bytes: snapshot.len() as u64,
            snapshot_checksum,
            checksum: 0,
            ..open.selected_superblock
        };
        Self::write_superblock_copy(&mut file, &next_superblock)?;
        crash_inject("snapshot_after_superblock_write");
        file.sync_all()?;
        lock_file.unlock()?;
        Self::open(path)
    }

    pub fn open_strict_manifest(path: impl AsRef<Path>) -> RdbFileResult<EmbeddedRdbOpen> {
        let path = path.as_ref();
        let mut file = File::open(path)?;
        let selected_superblock = [
            read_superblock_copy(&mut file, 0),
            read_superblock_copy(&mut file, 1),
        ]
        .into_iter()
        .flatten()
        .max_by_key(|superblock| superblock.generation)
        .ok_or_else(|| RdbFileError::InvalidOperation("no valid embedded superblock".into()))?;

        let mut manifest = read_manifest(&mut file, selected_superblock)?;
        manifest.wal_recovery_boundary = selected_superblock.wal_recovery_boundary;
        // The superblock is the WAL authority: appends update it without
        // rewriting the manifest zone, so a stale manifest wal_live_bytes
        // would make the scan see an empty region and drop live records.
        manifest.wal_live_bytes = selected_superblock.wal_live_bytes;
        Ok(EmbeddedRdbOpen {
            path: path.to_path_buf(),
            selected_superblock,
            manifest,
        })
    }

    pub fn read_snapshot(open: &EmbeddedRdbOpen) -> RdbFileResult<Option<Vec<u8>>> {
        if open.manifest.snapshot_bytes == 0 {
            return Ok(None);
        }
        let mut file = File::open(&open.path)?;
        let mut bytes = vec![0u8; open.manifest.snapshot_bytes as usize];
        file.seek(SeekFrom::Start(open.manifest.snapshot_offset))?;
        file.read_exact(&mut bytes)?;
        let checksum = crc32(&bytes);
        if checksum != open.manifest.snapshot_checksum {
            return Err(RdbFileError::InvalidOperation(format!(
                "embedded snapshot checksum mismatch: stored {:#010x}, computed {:#010x}",
                open.manifest.snapshot_checksum, checksum
            )));
        }
        if bytes.len() >= SNAPSHOT_MAGIC.len() && &bytes[..SNAPSHOT_MAGIC.len()] != SNAPSHOT_MAGIC {
            return Err(RdbFileError::InvalidOperation(
                "invalid embedded snapshot magic".into(),
            ));
        }
        Ok(Some(bytes))
    }

    pub fn write_snapshot(
        path: impl AsRef<Path>,
        snapshot: &[u8],
    ) -> RdbFileResult<EmbeddedRdbOpen> {
        Self::write_snapshot_with_wal_capacity(path, snapshot, 0)
    }

    pub fn read_wal_payloads(open: &EmbeddedRdbOpen) -> RdbFileResult<Vec<Vec<u8>>> {
        Ok(scan_wal(open)?.payloads)
    }

    pub fn append_wal_payloads(
        path: impl AsRef<Path>,
        payloads: &[Vec<u8>],
    ) -> RdbFileResult<EmbeddedRdbOpen> {
        let path = path.as_ref();
        if payloads.is_empty() {
            return Self::open(path);
        }

        let path_lock = embedded_path_lock(path);
        let _path_guard = path_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let lock_file = OpenOptions::new().read(true).write(true).open(path)?;
        lock_file.lock_exclusive()?;

        let open = Self::open_for_wal_append(path)?;
        let wal_scan = scan_wal(&open)?;
        let mut sequence = wal_scan.next_sequence;
        let mut previous_frame_crc = wal_scan.previous_frame_crc;
        let mut encoded = Vec::new();
        for payload in payloads {
            let (frame, frame_crc) = encode_wal_frame(sequence, previous_frame_crc, payload)?;
            encoded.extend_from_slice(&frame);
            previous_frame_crc = frame_crc;
            sequence = sequence.saturating_add(1);
        }

        let encoded_len = encoded.len() as u64;
        let free_bytes = open
            .manifest
            .wal_region_bytes
            .checked_sub(wal_scan.valid_bytes)
            .ok_or_else(|| {
                RdbFileError::InvalidOperation("embedded wal live bytes overflow".into())
            })?;
        if encoded_len > free_bytes {
            return Err(RdbFileError::InvalidOperation(
                "embedded wal region full".into(),
            ));
        }
        let next_boundary =
            wal_boundary_after_live_bytes(&open, wal_scan.valid_bytes + encoded_len)?;

        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        write_circular_wal_bytes(&mut file, &open, &encoded)?;
        crash_inject("wal_after_frame_write");
        file.sync_data()?;
        crash_inject("wal_after_frame_sync");

        let next_copy_index = if open.selected_superblock.copy_index == 0 {
            1
        } else {
            0
        };
        let next_superblock = EmbeddedRdbSuperblock {
            copy_index: next_copy_index,
            generation: open.selected_superblock.generation.saturating_add(1),
            wal_recovery_boundary: next_boundary,
            wal_live_bytes: wal_scan.valid_bytes + encoded_len,
            checksum: 0,
            ..open.selected_superblock
        };
        Self::write_superblock_copy(&mut file, &next_superblock)?;
        crash_inject("wal_after_superblock_write");
        file.sync_all()?;
        lock_file.unlock()?;
        Self::open(path)
    }

    pub fn write_superblock_copy(
        file: &mut File,
        superblock: &EmbeddedRdbSuperblock,
    ) -> RdbFileResult<()> {
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

/// The manifest slot the given superblock does not reference.
///
/// With two superblock copies and two manifest slots this is always the slot
/// belonging to the *stale* superblock copy — the same copy the update is about
/// to overwrite. That coincidence is what makes the pair safe: a crash can only
/// ever clobber the manifest of a superblock that was already being replaced.
fn inactive_manifest_offset(superblock: EmbeddedRdbSuperblock) -> RdbFileResult<u64> {
    match superblock.manifest_offset {
        EMBEDDED_RDB_MANIFEST_0_OFFSET => Ok(EMBEDDED_RDB_MANIFEST_1_OFFSET),
        EMBEDDED_RDB_MANIFEST_1_OFFSET => Ok(EMBEDDED_RDB_MANIFEST_0_OFFSET),
        other => Err(RdbFileError::InvalidOperation(format!(
            "superblock names a manifest offset outside the zone: {other}"
        ))),
    }
}

fn read_manifest(
    file: &mut File,
    superblock: EmbeddedRdbSuperblock,
) -> RdbFileResult<EmbeddedRdbManifest> {
    if superblock.manifest_len < CHECKSUM_LEN as u64
        || superblock.manifest_len > MANIFEST_REGION_BYTES
    {
        return Err(RdbFileError::InvalidOperation(format!(
            "invalid embedded manifest length {}",
            superblock.manifest_len
        )));
    }
    // A superblock that points outside the manifest zone is a misdirected or
    // forged write; never chase the pointer.
    if !matches!(
        superblock.manifest_offset,
        EMBEDDED_RDB_MANIFEST_0_OFFSET | EMBEDDED_RDB_MANIFEST_1_OFFSET
    ) {
        return Err(RdbFileError::InvalidOperation(format!(
            "superblock names a manifest offset outside the zone: {}",
            superblock.manifest_offset
        )));
    }

    let mut bytes = vec![0u8; superblock.manifest_len as usize];
    file.seek(SeekFrom::Start(superblock.manifest_offset))?;
    file.read_exact(&mut bytes)?;
    let checksum = trailer_checksum(&bytes);
    if checksum != superblock.manifest_checksum {
        return Err(RdbFileError::InvalidOperation(format!(
            "embedded manifest checksum mismatch: stored {:#010x}, computed {:#010x}",
            superblock.manifest_checksum, checksum
        )));
    }
    decode_manifest(&bytes)
}

fn snapshot_reference_valid(
    file: &mut File,
    manifest: &EmbeddedRdbManifest,
) -> RdbFileResult<bool> {
    if manifest.snapshot_bytes == 0 {
        return Ok(true);
    }
    let snapshot_end = manifest
        .snapshot_offset
        .checked_add(manifest.snapshot_bytes)
        .ok_or_else(|| RdbFileError::InvalidOperation("embedded snapshot end overflow".into()))?;
    if snapshot_end > file.metadata()?.len() {
        return Ok(false);
    }

    let mut bytes = vec![0u8; manifest.snapshot_bytes as usize];
    file.seek(SeekFrom::Start(manifest.snapshot_offset))?;
    if file.read_exact(&mut bytes).is_err() {
        return Ok(false);
    }
    if crc32(&bytes) != manifest.snapshot_checksum {
        return Ok(false);
    }
    if bytes.len() >= SNAPSHOT_MAGIC.len() && &bytes[..SNAPSHOT_MAGIC.len()] != SNAPSHOT_MAGIC {
        return Ok(false);
    }
    Ok(true)
}

fn grow_wal_region_bytes(current: u64, min_required: u64) -> RdbFileResult<u64> {
    let mut next = current.max(WAL_REGION_BYTES);
    while next < min_required {
        next = next.checked_mul(2).ok_or_else(|| {
            RdbFileError::InvalidOperation("embedded wal region size overflow".into())
        })?;
    }
    Ok(next)
}

fn embedded_path_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks
        .entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn next_snapshot_offset(
    path: &Path,
    open: &EmbeddedRdbOpen,
    wal_region_bytes: u64,
    snapshot: &[u8],
) -> RdbFileResult<u64> {
    let base = open
        .manifest
        .wal_region_offset
        .checked_add(wal_region_bytes)
        .ok_or_else(|| {
            RdbFileError::InvalidOperation("embedded snapshot offset overflow".into())
        })?;
    if open.manifest.snapshot_bytes == 0 && snapshot.is_empty() {
        return Ok(base);
    }

    let file_len = std::fs::metadata(path).map(|metadata| metadata.len())?;
    let active_snapshot_end = open
        .manifest
        .snapshot_offset
        .checked_add(open.manifest.snapshot_bytes)
        .ok_or_else(|| RdbFileError::InvalidOperation("embedded snapshot end overflow".into()))?;
    align_up(
        file_len.max(active_snapshot_end).max(base),
        SNAPSHOT_ALIGNMENT,
    )
}

fn align_up(value: u64, alignment: u64) -> RdbFileResult<u64> {
    if alignment == 0 {
        return Ok(value);
    }
    let remainder = value % alignment;
    if remainder == 0 {
        return Ok(value);
    }
    value
        .checked_add(alignment - remainder)
        .ok_or_else(|| RdbFileError::InvalidOperation("embedded alignment overflow".into()))
}

fn scan_wal(open: &EmbeddedRdbOpen) -> RdbFileResult<WalScan> {
    let wal_start = open.manifest.wal_region_offset;
    let wal_end = wal_start
        .checked_add(open.manifest.wal_region_bytes)
        .ok_or_else(|| RdbFileError::InvalidOperation("embedded wal end overflow".into()))?;
    let append_boundary = open.manifest.wal_recovery_boundary;
    if append_boundary < wal_start || append_boundary > wal_end {
        return Err(RdbFileError::InvalidOperation(format!(
            "invalid embedded wal boundary {append_boundary}"
        )));
    }
    if open.manifest.wal_live_bytes > open.manifest.wal_region_bytes {
        return Err(RdbFileError::InvalidOperation(format!(
            "invalid embedded wal live bytes {}",
            open.manifest.wal_live_bytes
        )));
    }
    if open.manifest.wal_live_bytes == 0 {
        return Ok(WalScan {
            next_sequence: 1,
            ..WalScan::default()
        });
    }

    let mut file = File::open(&open.path)?;
    let file_len = file.metadata()?.len();
    if file_len <= wal_start {
        return Ok(WalScan {
            next_sequence: 1,
            ..WalScan::default()
        });
    }
    let bytes = read_circular_wal_bytes(&mut file, open, file_len)?;
    Ok(scan_wal_bytes(&bytes))
}

fn read_circular_wal_bytes(
    file: &mut File,
    open: &EmbeddedRdbOpen,
    file_len: u64,
) -> RdbFileResult<Vec<u8>> {
    let live_start = wal_live_start_relative(open)?;
    let region_bytes = open.manifest.wal_region_bytes;
    let live_bytes = open.manifest.wal_live_bytes;
    let mut remaining = live_bytes.min(file_len.saturating_sub(open.manifest.wal_region_offset));
    let mut relative = live_start;
    let mut bytes = Vec::with_capacity(remaining as usize);
    while remaining > 0 {
        let contiguous = (region_bytes - relative).min(remaining);
        let offset = open.manifest.wal_region_offset + relative;
        file.seek(SeekFrom::Start(offset))?;
        let mut chunk = vec![0u8; contiguous as usize];
        file.read_exact(&mut chunk)?;
        bytes.extend_from_slice(&chunk);
        remaining -= contiguous;
        relative = 0;
    }
    Ok(bytes)
}

fn write_circular_wal_bytes(
    file: &mut File,
    open: &EmbeddedRdbOpen,
    bytes: &[u8],
) -> RdbFileResult<()> {
    let mut written = 0usize;
    let mut relative = wal_append_relative(open)?;
    while written < bytes.len() {
        if relative == open.manifest.wal_region_bytes {
            relative = 0;
        }
        let contiguous = (open.manifest.wal_region_bytes - relative) as usize;
        let chunk_len = contiguous.min(bytes.len() - written);
        write_at(
            file,
            open.manifest.wal_region_offset + relative,
            &bytes[written..written + chunk_len],
        )?;
        written += chunk_len;
        relative += chunk_len as u64;
    }
    Ok(())
}

fn wal_append_relative(open: &EmbeddedRdbOpen) -> RdbFileResult<u64> {
    open.manifest
        .wal_recovery_boundary
        .checked_sub(open.manifest.wal_region_offset)
        .ok_or_else(|| RdbFileError::InvalidOperation("embedded wal boundary underflow".into()))
}

fn wal_live_start_relative(open: &EmbeddedRdbOpen) -> RdbFileResult<u64> {
    let append = wal_append_relative(open)?;
    let region = open.manifest.wal_region_bytes;
    if region == 0 {
        return Err(RdbFileError::InvalidOperation(
            "embedded wal region is empty".into(),
        ));
    }
    Ok((append + region - (open.manifest.wal_live_bytes % region)) % region)
}

fn wal_boundary_after_live_bytes(open: &EmbeddedRdbOpen, live_bytes: u64) -> RdbFileResult<u64> {
    if live_bytes > open.manifest.wal_region_bytes {
        return Err(RdbFileError::InvalidOperation(format!(
            "embedded wal live bytes {live_bytes} exceed region size {}",
            open.manifest.wal_region_bytes
        )));
    }
    let start = if open.manifest.wal_live_bytes == 0 {
        wal_append_relative(open)?
    } else {
        wal_live_start_relative(open)?
    };
    let relative = (start + live_bytes) % open.manifest.wal_region_bytes;
    open.manifest
        .wal_region_offset
        .checked_add(relative)
        .ok_or_else(|| RdbFileError::InvalidOperation("embedded wal boundary overflow".into()))
}

fn scan_wal_bytes(bytes: &[u8]) -> WalScan {
    let mut scan = WalScan {
        next_sequence: 1,
        ..WalScan::default()
    };
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let Some(frame) = decode_next_wal_frame(bytes, cursor, &scan) else {
            break;
        };
        scan.payloads.push(frame.payload);
        scan.next_sequence = scan.next_sequence.saturating_add(1);
        scan.previous_frame_crc = frame.frame_crc;
        cursor = frame.end;
        scan.valid_bytes = cursor as u64;
    }
    scan
}

struct DecodedWalFrame {
    payload: Vec<u8>,
    frame_crc: u32,
    end: usize,
}

fn decode_next_wal_frame(bytes: &[u8], start: usize, scan: &WalScan) -> Option<DecodedWalFrame> {
    let remaining = bytes.len().checked_sub(start)?;
    if remaining < WAL_FRAME_MAGIC.len() {
        return None;
    }
    if &bytes[start..start + WAL_FRAME_MAGIC.len()] != WAL_FRAME_MAGIC {
        return None;
    }
    if remaining < WAL_FRAME_MAGIC.len() + 2 {
        return None;
    }
    let version_offset = start + WAL_FRAME_MAGIC.len();
    let version = u16::from_le_bytes(bytes[version_offset..version_offset + 2].try_into().ok()?);
    if version == WAL_FRAME_VERSION {
        decode_v2_wal_frame(bytes, start, scan)
    } else {
        decode_legacy_wal_frame(bytes, start)
    }
}

fn decode_v2_wal_frame(bytes: &[u8], start: usize, scan: &WalScan) -> Option<DecodedWalFrame> {
    if bytes.len().checked_sub(start)? < WAL_FRAME_HEADER_BYTES {
        return None;
    }
    let header_len_offset = start + 10;
    let header_len = u16::from_le_bytes(
        bytes[header_len_offset..header_len_offset + 2]
            .try_into()
            .ok()?,
    ) as usize;
    if header_len != WAL_FRAME_HEADER_BYTES {
        return None;
    }
    let sequence_offset = start + 12;
    let sequence = u64::from_le_bytes(
        bytes[sequence_offset..sequence_offset + 8]
            .try_into()
            .ok()?,
    );
    if sequence != scan.next_sequence {
        return None;
    }
    let payload_len_offset = start + 20;
    let payload_len = u32::from_le_bytes(
        bytes[payload_len_offset..payload_len_offset + 4]
            .try_into()
            .ok()?,
    ) as usize;
    let payload_crc_offset = start + 24;
    let payload_crc = u32::from_le_bytes(
        bytes[payload_crc_offset..payload_crc_offset + 4]
            .try_into()
            .ok()?,
    );
    let previous_frame_crc_offset = start + 28;
    let previous_frame_crc = u32::from_le_bytes(
        bytes[previous_frame_crc_offset..previous_frame_crc_offset + 4]
            .try_into()
            .ok()?,
    );
    if previous_frame_crc != scan.previous_frame_crc {
        return None;
    }
    let header_crc_offset = start + 32;
    let header_crc = u32::from_le_bytes(
        bytes[header_crc_offset..header_crc_offset + 4]
            .try_into()
            .ok()?,
    );
    if header_crc != crc32(&bytes[start..header_crc_offset]) {
        return None;
    }
    let payload_start = start.checked_add(header_len)?;
    let end = payload_start.checked_add(payload_len)?;
    if end > bytes.len() {
        return None;
    }
    let payload = bytes[payload_start..end].to_vec();
    if crc32(&payload) != payload_crc {
        return None;
    }
    Some(DecodedWalFrame {
        payload,
        frame_crc: crc32(&bytes[start..end]),
        end,
    })
}

fn decode_legacy_wal_frame(bytes: &[u8], start: usize) -> Option<DecodedWalFrame> {
    if bytes.len().checked_sub(start)? < LEGACY_WAL_FRAME_HEADER_BYTES {
        return None;
    }
    let payload_len_offset = start + WAL_FRAME_MAGIC.len();
    let payload_len = u32::from_le_bytes(
        bytes[payload_len_offset..payload_len_offset + 4]
            .try_into()
            .ok()?,
    ) as usize;
    let payload_crc_offset = payload_len_offset + 4;
    let payload_crc = u32::from_le_bytes(
        bytes[payload_crc_offset..payload_crc_offset + 4]
            .try_into()
            .ok()?,
    );
    let payload_start = start.checked_add(LEGACY_WAL_FRAME_HEADER_BYTES)?;
    let end = payload_start.checked_add(payload_len)?;
    if end > bytes.len() {
        return None;
    }
    let payload = bytes[payload_start..end].to_vec();
    if crc32(&payload) != payload_crc {
        return None;
    }
    Some(DecodedWalFrame {
        payload,
        frame_crc: crc32(&bytes[start..end]),
        end,
    })
}

fn encode_wal_frame(
    sequence: u64,
    previous_frame_crc: u32,
    payload: &[u8],
) -> RdbFileResult<(Vec<u8>, u32)> {
    let payload_len = u32::try_from(payload.len())
        .map_err(|_| RdbFileError::InvalidOperation("embedded wal payload too large".into()))?;
    let mut frame = Vec::with_capacity(WAL_FRAME_HEADER_BYTES + payload.len());
    frame.extend_from_slice(WAL_FRAME_MAGIC);
    frame.extend_from_slice(&WAL_FRAME_VERSION.to_le_bytes());
    frame.extend_from_slice(&(WAL_FRAME_HEADER_BYTES as u16).to_le_bytes());
    frame.extend_from_slice(&sequence.to_le_bytes());
    frame.extend_from_slice(&payload_len.to_le_bytes());
    frame.extend_from_slice(&crc32(payload).to_le_bytes());
    frame.extend_from_slice(&previous_frame_crc.to_le_bytes());
    let header_crc = crc32(&frame);
    frame.extend_from_slice(&header_crc.to_le_bytes());
    frame.extend_from_slice(payload);
    let frame_crc = crc32(&frame);
    Ok((frame, frame_crc))
}

fn encode_superblock(superblock: EmbeddedRdbSuperblock) -> RdbFileResult<Vec<u8>> {
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
    put_u64(&mut bytes, &mut cursor, superblock.wal_live_bytes);
    put_u64(&mut bytes, &mut cursor, superblock.snapshot_offset);
    put_u64(&mut bytes, &mut cursor, superblock.snapshot_bytes);
    put_u32(&mut bytes, &mut cursor, superblock.snapshot_checksum);

    let checksum_offset = bytes.len() - CHECKSUM_LEN;
    let checksum = crc32(&bytes[..checksum_offset]);
    bytes[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());
    Ok(bytes)
}

fn decode_superblock(copy_index: u8, bytes: &[u8]) -> RdbFileResult<EmbeddedRdbSuperblock> {
    if bytes.len() != EMBEDDED_RDB_SUPERBLOCK_SIZE as usize {
        return Err(RdbFileError::InvalidOperation(
            "invalid embedded superblock size".into(),
        ));
    }
    let checksum_offset = bytes.len() - CHECKSUM_LEN;
    let stored_checksum = u32::from_le_bytes(bytes[checksum_offset..].try_into().unwrap());
    let computed_checksum = crc32(&bytes[..checksum_offset]);
    if stored_checksum != computed_checksum {
        return Err(RdbFileError::InvalidOperation(
            "embedded superblock checksum mismatch".into(),
        ));
    }

    let mut cursor = 0usize;
    if take_bytes(bytes, &mut cursor, SUPERBLOCK_MAGIC.len())? != SUPERBLOCK_MAGIC {
        return Err(RdbFileError::InvalidOperation(
            "invalid embedded superblock magic".into(),
        ));
    }
    let version = take_u32(bytes, &mut cursor)?;
    if version != SUPERBLOCK_VERSION && version != LEGACY_SUPERBLOCK_VERSION {
        return Err(RdbFileError::InvalidOperation(format!(
            "unsupported embedded superblock version {version}"
        )));
    }
    let stored_copy_index = take_u8(bytes, &mut cursor)?;
    if stored_copy_index != copy_index {
        return Err(RdbFileError::InvalidOperation(
            "embedded superblock copy index mismatch".into(),
        ));
    }

    let generation = take_u64(bytes, &mut cursor)?;
    let format_version = take_u32(bytes, &mut cursor)?;
    let manifest_offset = take_u64(bytes, &mut cursor)?;
    let manifest_len = take_u64(bytes, &mut cursor)?;
    let manifest_checksum = take_u32(bytes, &mut cursor)?;
    let wal_region_offset = take_u64(bytes, &mut cursor)?;
    let wal_region_bytes = take_u64(bytes, &mut cursor)?;
    let wal_recovery_boundary = take_u64(bytes, &mut cursor)?;
    let wal_live_bytes = if version == SUPERBLOCK_VERSION {
        take_u64(bytes, &mut cursor)?
    } else {
        wal_recovery_boundary.saturating_sub(wal_region_offset)
    };

    Ok(EmbeddedRdbSuperblock {
        copy_index: stored_copy_index,
        generation,
        format_version,
        manifest_offset,
        manifest_len,
        manifest_checksum,
        wal_region_offset,
        wal_region_bytes,
        wal_recovery_boundary,
        wal_live_bytes,
        snapshot_offset: take_u64(bytes, &mut cursor)?,
        snapshot_bytes: take_u64(bytes, &mut cursor)?,
        snapshot_checksum: take_u32(bytes, &mut cursor)?,
        checksum: stored_checksum,
    })
}

fn encode_manifest(manifest: EmbeddedRdbManifest) -> Vec<u8> {
    let mut bytes = vec![0u8; 8 + 4 + 8 + 8 + 8 + 8 + 8 + 8 + 4 + 16 + CHECKSUM_LEN];
    let mut cursor = 0usize;
    put_bytes(&mut bytes, &mut cursor, MANIFEST_MAGIC);
    put_u32(&mut bytes, &mut cursor, manifest.version);
    put_u64(&mut bytes, &mut cursor, manifest.wal_region_offset);
    put_u64(&mut bytes, &mut cursor, manifest.wal_region_bytes);
    put_u64(&mut bytes, &mut cursor, manifest.wal_recovery_boundary);
    put_u64(&mut bytes, &mut cursor, manifest.wal_live_bytes);
    put_u64(&mut bytes, &mut cursor, manifest.snapshot_offset);
    put_u64(&mut bytes, &mut cursor, manifest.snapshot_bytes);
    put_u32(&mut bytes, &mut cursor, manifest.snapshot_checksum);
    put_u128(&mut bytes, &mut cursor, manifest.created_at_unix_ms);

    let checksum_offset = bytes.len() - CHECKSUM_LEN;
    let checksum = crc32(&bytes[..checksum_offset]);
    bytes[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());
    bytes
}

fn decode_manifest(bytes: &[u8]) -> RdbFileResult<EmbeddedRdbManifest> {
    let checksum_offset = bytes
        .len()
        .checked_sub(CHECKSUM_LEN)
        .ok_or_else(|| RdbFileError::InvalidOperation("embedded manifest too short".into()))?;
    let stored_checksum = u32::from_le_bytes(bytes[checksum_offset..].try_into().unwrap());
    let computed_checksum = crc32(&bytes[..checksum_offset]);
    if stored_checksum != computed_checksum {
        return Err(RdbFileError::InvalidOperation(
            "embedded manifest checksum mismatch".into(),
        ));
    }

    let mut cursor = 0usize;
    if take_bytes(bytes, &mut cursor, MANIFEST_MAGIC.len())? != MANIFEST_MAGIC {
        return Err(RdbFileError::InvalidOperation(
            "invalid embedded manifest magic".into(),
        ));
    }
    let version = take_u32(bytes, &mut cursor)?;
    if version != MANIFEST_VERSION && version != LEGACY_MANIFEST_VERSION {
        return Err(RdbFileError::InvalidOperation(format!(
            "unsupported embedded manifest version {version}"
        )));
    }
    let wal_region_offset = take_u64(bytes, &mut cursor)?;
    let wal_region_bytes = take_u64(bytes, &mut cursor)?;
    let wal_recovery_boundary = take_u64(bytes, &mut cursor)?;
    let wal_live_bytes = if version == MANIFEST_VERSION {
        take_u64(bytes, &mut cursor)?
    } else {
        wal_recovery_boundary.saturating_sub(wal_region_offset)
    };
    Ok(EmbeddedRdbManifest {
        version,
        wal_region_offset,
        wal_region_bytes,
        wal_recovery_boundary,
        wal_live_bytes,
        snapshot_offset: take_u64(bytes, &mut cursor)?,
        snapshot_bytes: take_u64(bytes, &mut cursor)?,
        snapshot_checksum: take_u32(bytes, &mut cursor)?,
        created_at_unix_ms: take_u128(bytes, &mut cursor)?,
        checksum: stored_checksum,
    })
}

fn trailer_checksum(bytes: &[u8]) -> u32 {
    let checksum_offset = bytes.len() - CHECKSUM_LEN;
    u32::from_le_bytes(bytes[checksum_offset..].try_into().unwrap())
}

fn superblock_offset(copy_index: u8) -> RdbFileResult<u64> {
    match copy_index {
        0 => Ok(EMBEDDED_RDB_SUPERBLOCK_0_OFFSET),
        1 => Ok(EMBEDDED_RDB_SUPERBLOCK_1_OFFSET),
        _ => Err(RdbFileError::InvalidOperation(format!(
            "invalid embedded superblock copy index {copy_index}"
        ))),
    }
}

fn write_at(file: &mut File, offset: u64, bytes: &[u8]) -> RdbFileResult<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(bytes)?;
    Ok(())
}

fn crash_inject(point: &str) {
    if std::env::var(CRASH_INJECT_ENV).ok().as_deref() == Some(point) {
        std::process::exit(173);
    }
    if crate::buggify!(CRASH_INJECT_ENV, point) {
        std::process::exit(173);
    }
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

fn take_bytes<'a>(bytes: &'a [u8], cursor: &mut usize, len: usize) -> RdbFileResult<&'a [u8]> {
    let end = cursor.checked_add(len).ok_or_else(|| {
        RdbFileError::InvalidOperation("embedded artifact cursor overflow".into())
    })?;
    if end > bytes.len() {
        return Err(RdbFileError::InvalidOperation(
            "embedded artifact truncated".into(),
        ));
    }
    let value = &bytes[*cursor..end];
    *cursor = end;
    Ok(value)
}

fn take_u8(bytes: &[u8], cursor: &mut usize) -> RdbFileResult<u8> {
    Ok(take_bytes(bytes, cursor, 1)?[0])
}

fn take_u32(bytes: &[u8], cursor: &mut usize) -> RdbFileResult<u32> {
    Ok(u32::from_le_bytes(
        take_bytes(bytes, cursor, 4)?.try_into().unwrap(),
    ))
}

fn take_u64(bytes: &[u8], cursor: &mut usize) -> RdbFileResult<u64> {
    Ok(u64::from_le_bytes(
        take_bytes(bytes, cursor, 8)?.try_into().unwrap(),
    ))
}

fn take_u128(bytes: &[u8], cursor: &mut usize) -> RdbFileResult<u128> {
    Ok(u128::from_le_bytes(
        take_bytes(bytes, cursor, 16)?.try_into().unwrap(),
    ))
}
