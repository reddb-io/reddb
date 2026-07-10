//! Offline, reversible migration between the sidecar-backed pager layout and
//! the zoned `.rdb` (ADR 0038 §4 phase 1).
//!
//! A legacy store keeps its page-0 database header in a `rdb-hdr` sidecar and
//! its manifest page in a `rdb-meta` sidecar, each a whole-page shadow written
//! before the in-file copy. The zoned form retires both: page 0 becomes a
//! superblock ping-pong pair, and page 1 plus its overflow chain is the
//! internal manifest, rooted by the superblock.
//!
//! Per the house no-backcompat posture the engine never reads the old form —
//! [`crate::storage::engine::pager::Pager::open`] refuses it and names this
//! module. Conversion is an explicit, offline step, modelled on the reversible
//! document-body migration ([`crate::document_migration`]):
//!
//! 1. Copy the data file to a retained `<data>.pre-migration` rollback point.
//! 2. Rebuild page 0's two superblock slots from the authoritative header —
//!    preferring the in-file page 0 and falling back to the `rdb-hdr` shadow
//!    when page 0 is the torn one (that is what the shadow was for).
//! 3. Restore page 1 from the `rdb-meta` shadow if the in-file copy fails its
//!    checksum, then fsync.
//! 4. Only once the file is durable, unlink the sidecars.
//!
//! Every step before the unlink leaves the source readable by the *old* engine,
//! and [`revert_to_sidecars`] walks it back. Nothing here touches the live
//! filename contract: the retired names come from `reddb_file::layout::retired`.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use reddb_file::layout::retired;

const PAGE_SIZE: usize = reddb_file::PAGED_PAGE_SIZE;
const SLOT_SIZE: usize = reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE;
const ZONE_SIZE: usize = reddb_file::PAGED_SUPERBLOCK_ZONE_SIZE;
const SUPERBLOCK_TRAILER_OFFSET: usize = reddb_file::PAGED_SUPERBLOCK_TRAILER_OFFSET;

/// Suffix for the retained pre-migration data file (the rollback point).
const BACKUP_SUFFIX: &str = "pre-migration";

/// What a migration did, so the caller can log or assert it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneMigrationReport {
    /// The data file that now carries the zoned layout.
    pub data_path: PathBuf,
    /// Retained copy of the pre-migration data file.
    pub backup_path: PathBuf,
    /// Retired sidecars removed by the migration, in the order they were found.
    pub removed_sidecars: Vec<PathBuf>,
    /// `true` when page 0 was torn and the `rdb-hdr` shadow supplied the header.
    pub header_recovered_from_shadow: bool,
    /// `true` when page 1 was torn and the `rdb-meta` shadow supplied the
    /// manifest page.
    pub manifest_recovered_from_shadow: bool,
}

#[derive(Debug)]
pub enum ZoneMigrationError {
    /// The data file does not exist.
    MissingStore(PathBuf),
    /// No retired sidecar is present, so there is nothing to migrate.
    NotALegacyStore(PathBuf),
    /// The store is already zoned (a valid superblock zone is present).
    AlreadyZoned(PathBuf),
    /// No usable database header survives in page 0 or the `rdb-hdr` shadow.
    HeaderUnrecoverable(PathBuf),
    Io(std::io::Error),
}

impl std::fmt::Display for ZoneMigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingStore(path) => write!(f, "no store at {}", path.display()),
            Self::NotALegacyStore(path) => write!(
                f,
                "{} carries no retired rdb-hdr/rdb-meta sidecar, so there is nothing to \
                 migrate; a zoned store opens directly",
                path.display()
            ),
            Self::AlreadyZoned(path) => write!(
                f,
                "{} already has a valid superblock zone; migrating again would discard it",
                path.display()
            ),
            Self::HeaderUnrecoverable(path) => write!(
                f,
                "neither page 0 of {} nor its rdb-hdr shadow holds a readable database \
                 header, so no superblock can be seeded from this store. This is a damaged \
                 store, not a legacy one: reach for red salvage (ADR 0074 §4)",
                path.display()
            ),
            Self::Io(err) => write!(f, "io error: {err}"),
        }
    }
}

impl std::error::Error for ZoneMigrationError {}

impl From<std::io::Error> for ZoneMigrationError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

type Result<T> = std::result::Result<T, ZoneMigrationError>;

/// The retained rollback point for `data_path`.
pub fn backup_path_for(data_path: &Path) -> PathBuf {
    let mut path = data_path.as_os_str().to_os_string();
    path.push(".");
    path.push(BACKUP_SUFFIX);
    PathBuf::from(path)
}

/// Convert a legacy sidecar-backed store at `data_path` into the zoned form.
///
/// The store must be closed. On success the data file carries a superblock
/// ping-pong pair, the retired sidecars are gone, and the pre-migration file is
/// retained at [`ZoneMigrationReport::backup_path`]. On any failure before the
/// sidecars are unlinked the source store is left exactly as it was.
pub fn migrate_to_zoned(data_path: &Path) -> Result<ZoneMigrationReport> {
    if !data_path.exists() {
        return Err(ZoneMigrationError::MissingStore(data_path.to_path_buf()));
    }
    let sidecars = present_sidecars(data_path);
    if sidecars.is_empty() {
        return Err(ZoneMigrationError::NotALegacyStore(data_path.to_path_buf()));
    }
    if has_valid_superblock_zone(data_path)? {
        return Err(ZoneMigrationError::AlreadyZoned(data_path.to_path_buf()));
    }

    let backup_path = backup_path_for(data_path);
    fs::copy(data_path, &backup_path)?;

    let page_zero = read_page(data_path, 0)?;
    // Page 0 is authoritative unless it is the torn one — which is exactly the
    // case the `rdb-hdr` shadow existed to cover.
    let (header_image, header_recovered_from_shadow) =
        if reddb_file::database_header_magic_matches(&page_zero) {
            (page_zero, false)
        } else {
            let shadow = read_sidecar_page(&retired::pager_header_shadow_path_v0(data_path))?
                .filter(|page| reddb_file::database_header_magic_matches(page))
                .ok_or_else(|| ZoneMigrationError::HeaderUnrecoverable(data_path.to_path_buf()))?;
            (shadow, true)
        };

    let manifest_recovered_from_shadow = restore_manifest_page_if_torn(data_path)?;

    // Seed both superblock copies so the ping-pong invariant holds from the
    // migrated store's very first update.
    let mut slot = [0u8; SLOT_SIZE];
    slot.copy_from_slice(&header_image[..SLOT_SIZE]);
    let mut file = OpenOptions::new().read(true).write(true).open(data_path)?;
    for (copy_index, generation) in [(0usize, 1u64), (1usize, 2u64)] {
        reddb_file::seal_paged_superblock_slot(&mut slot, copy_index, generation)
            .map_err(|err| std::io::Error::other(err.to_string()))?;
        write_at(&mut file, superblock_offset(copy_index), &slot)?;
    }
    file.sync_all()?;
    drop(file);

    // The file is durable in its zoned form; only now is it safe to drop the
    // sidecars. A crash before this point leaves a store the old engine reads.
    for sidecar in &sidecars {
        fs::remove_file(sidecar)?;
    }

    Ok(ZoneMigrationReport {
        data_path: data_path.to_path_buf(),
        backup_path,
        removed_sidecars: sidecars,
        header_recovered_from_shadow,
        manifest_recovered_from_shadow,
    })
}

/// Walk a migration back: rebuild page 0 as a plain checksummed header page and
/// re-create the retired sidecars beside it.
///
/// This is a true inverse of [`migrate_to_zoned`], not a backup restore, so a
/// store that was migrated and then written to still reverts to a coherent
/// legacy store. The retained `.pre-migration` file, if one is still around, is
/// dropped last: once the legacy shape is durable it has nothing left to roll
/// back to.
pub fn revert_to_sidecars(data_path: &Path) -> Result<ZoneMigrationReport> {
    if !data_path.exists() {
        return Err(ZoneMigrationError::MissingStore(data_path.to_path_buf()));
    }

    let image = newest_superblock_image(data_path)?
        .ok_or_else(|| ZoneMigrationError::HeaderUnrecoverable(data_path.to_path_buf()))?;

    // Strip the slot trailer and restore the whole-page checksum: this is
    // byte-for-byte the page 0 the sidecar-era pager wrote.
    let mut header_page = [0u8; PAGE_SIZE];
    header_page[..SLOT_SIZE].copy_from_slice(&image);
    header_page[SUPERBLOCK_TRAILER_OFFSET..SLOT_SIZE].fill(0);
    reddb_file::clear_paged_page_checksum(&mut header_page);
    let checksum = crate::storage::engine::crc32::crc32(&header_page);
    reddb_file::set_paged_page_checksum(&mut header_page, checksum);

    let manifest_page = read_page(data_path, 1)?;

    let mut file = OpenOptions::new().read(true).write(true).open(data_path)?;
    write_at(&mut file, 0, &header_page)?;
    file.sync_all()?;
    drop(file);

    write_sidecar_page(
        &retired::pager_header_shadow_path_v0(data_path),
        &header_page,
    )?;
    write_sidecar_page(
        &retired::pager_meta_shadow_path_v0(data_path),
        &manifest_page,
    )?;

    let backup_path = backup_path_for(data_path);
    if backup_path.exists() {
        fs::remove_file(&backup_path)?;
    }

    Ok(ZoneMigrationReport {
        data_path: data_path.to_path_buf(),
        backup_path,
        removed_sidecars: Vec::new(),
        header_recovered_from_shadow: false,
        manifest_recovered_from_shadow: false,
    })
}

/// The newest valid superblock slot image, or `None` when the zone is absent
/// or unrecoverable.
fn newest_superblock_image(data_path: &Path) -> Result<Option<[u8; SLOT_SIZE]>> {
    let zone = read_superblock_zone(data_path)?;
    let Some(selection) = reddb_file::select_paged_superblock(&zone) else {
        return Ok(None);
    };
    let start = selection.copy_index * SLOT_SIZE;
    let mut image = [0u8; SLOT_SIZE];
    image.copy_from_slice(&zone[start..start + SLOT_SIZE]);
    Ok(Some(image))
}

fn read_superblock_zone(data_path: &Path) -> Result<[u8; ZONE_SIZE]> {
    let mut zone = [0u8; ZONE_SIZE];
    let mut file = File::open(data_path)?;
    let len = file.metadata()?.len().min(ZONE_SIZE as u64) as usize;
    if len > 0 {
        file.read_exact(&mut zone[..len])?;
    }
    Ok(zone)
}

fn present_sidecars(data_path: &Path) -> Vec<PathBuf> {
    let mut seen: Vec<PathBuf> = Vec::new();
    for candidate in retired::phase1_sidecar_paths(data_path) {
        if candidate.exists() && !seen.contains(&candidate) {
            seen.push(candidate);
        }
    }
    seen
}

fn has_valid_superblock_zone(data_path: &Path) -> Result<bool> {
    let zone = read_superblock_zone(data_path)?;
    Ok(reddb_file::select_paged_superblock(&zone).is_some())
}

/// Overwrite page 1 from the `rdb-meta` shadow when the in-file page fails its
/// checksum. Returns whether the shadow was used.
fn restore_manifest_page_if_torn(data_path: &Path) -> Result<bool> {
    let page_one = read_page(data_path, 1)?;
    if page_checksum_valid(&page_one) {
        return Ok(false);
    }
    let Some(shadow) = read_sidecar_page(&retired::pager_meta_shadow_path_v0(data_path))? else {
        return Ok(false);
    };
    if !page_checksum_valid(&shadow) {
        return Ok(false);
    }
    let mut file = OpenOptions::new().read(true).write(true).open(data_path)?;
    write_at(&mut file, PAGE_SIZE as u64, &shadow)?;
    file.sync_all()?;
    Ok(true)
}

/// Verify a page's own CRC the way the pager's `Page::verify_checksum` does:
/// CRC over the page with the checksum field zeroed.
fn page_checksum_valid(page: &[u8; PAGE_SIZE]) -> bool {
    let stored = reddb_file::paged_page_checksum(page);
    let mut scratch = *page;
    reddb_file::clear_paged_page_checksum(&mut scratch);
    stored == crate::storage::engine::crc32::crc32(&scratch)
}

fn superblock_offset(copy_index: usize) -> u64 {
    reddb_file::paged_superblock_slot_offset(copy_index)
}

fn read_page(path: &Path, page_id: u64) -> Result<[u8; PAGE_SIZE]> {
    let mut file = File::open(path)?;
    let mut page = [0u8; PAGE_SIZE];
    file.seek(SeekFrom::Start(page_id * PAGE_SIZE as u64))?;
    file.read_exact(&mut page)?;
    Ok(page)
}

fn read_sidecar_page(path: &Path) -> Result<Option<[u8; PAGE_SIZE]>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut file = File::open(path)?;
    let mut page = [0u8; PAGE_SIZE];
    match file.read_exact(&mut page) {
        Ok(()) => Ok(Some(page)),
        // A shadow shorter than a page is itself torn: it recovers nothing.
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn write_sidecar_page(path: &Path, page: &[u8; PAGE_SIZE]) -> Result<()> {
    let mut file = File::create(path)?;
    file.write_all(page)?;
    file.sync_all()?;
    Ok(())
}

fn write_at(file: &mut File, offset: u64, bytes: &[u8]) -> Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(bytes)?;
    Ok(())
}
