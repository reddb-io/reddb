//! Native path opening, process locks, and append-tail caching.

use super::native_lock;
use super::*;
use crate::vfs::BorrowedStdFile as NativeFile;
use fs2::FileExt;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct EmbeddedPathState {
    tail: Option<CachedWalTail>,
}

struct CachedWalTail {
    // Published only after validating this generation's snapshot, or after
    // appending to an unchanged file with an already validated snapshot.
    stamp: FileMutationStamp,
    superblock: EmbeddedRdbSuperblock,
    next_sequence: u64,
    previous_frame_crc: u32,
    valid_bytes: u64,
}

// The superblock detects cooperating writers/checkpoints. File identity and
// mutation times also invalidate replacement or in-place edits. On platforms
// without this identity vocabulary, append keeps the validated full scan.
#[derive(PartialEq, Eq)]
struct FileMutationStamp {
    device: u64,
    inode: u64,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

fn file_mutation_stamp(file: &File) -> std::io::Result<Option<FileMutationStamp>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(Some(FileMutationStamp {
            device: metadata.dev(),
            inode: metadata.ino(),
            len: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }))
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(None)
    }
}

/// Process-lifetime writer fence for an embedded `.rdb`.
///
/// The per-operation lock on the data file only serializes individual appends
/// and checkpoints. A writer process keeps the whole store in memory and
/// checkpoints that image on close, so two processes writing the same file
/// would each publish their own image and the last one to close would
/// silently discard the other's commits. Holding this lock for the writer's
/// lifetime makes the second opener fail instead.
///
/// The sibling lock protects initial creation and interoperates with older
/// writers. A second lock on the data file itself fences hardlinks, symlinks,
/// and renamed aliases. It uses a range independent of publication locks.
pub struct EmbeddedRdbWriterLock {
    path: PathBuf,
    _file: File,
    _data_file: Option<File>,
}

impl EmbeddedRdbWriterLock {
    pub fn acquire(data_path: impl AsRef<Path>) -> RdbFileResult<Self> {
        let data_path = data_path.as_ref();
        let path = crate::layout::embedded_writer_lock_path(data_path);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        loop {
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&path)?;
            if let Err(err) = file.try_lock_exclusive() {
                if err.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                    return Err(RdbFileError::InvalidOperation(format!(
                        "{} is already open for writing by another process; an embedded \
                         single-file store has exactly one writer process",
                        data_path.display()
                    )));
                }
                return Err(err.into());
            }
            // The previous owner unlinks the lock file on release. If that
            // happened between our open and our lock, we hold an orphaned
            // inode no later opener can see, so start over on the live path.
            if lock_file_is_live(&file, &path)? {
                let mut guard = Self {
                    path,
                    _file: file,
                    _data_file: None,
                };
                let (mut data_file, created) = match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(data_path)
                {
                    Ok(file) => (file, true),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (
                        OpenOptions::new().read(true).write(true).open(data_path)?,
                        false,
                    ),
                    Err(error) => return Err(error.into()),
                };
                if let Err(error) = native_lock::try_lock_writer(&data_file) {
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        return Err(RdbFileError::InvalidOperation(format!(
                            "{} is already open for writing through this or another path; an embedded single-file store has exactly one writer process",
                            data_path.display()
                        )));
                    }
                    return Err(error.into());
                }
                if created {
                    create_file(
                        &mut NativeFile(&mut data_file),
                        data_path,
                        &[],
                        now_unix_ms(),
                    )?;
                }
                guard._data_file = Some(data_file);
                return Ok(guard);
            }
        }
    }
}

impl Drop for EmbeddedRdbWriterLock {
    fn drop(&mut self) {
        // Unlink while still holding the lock so a closed store is back to
        // one operator-visible artifact. Rust opens Windows files with delete
        // sharing, so the same ordering also closes the long-standing Windows
        // gap without releasing the writer fence before deletion. Unix
        // `acquire` re-checks the inode if it raced this unlink.
        let _ = fs::remove_file(&self.path);
    }
}

fn lock_file_is_live(file: &File, path: &Path) -> RdbFileResult<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let held = file.metadata()?;
        match fs::metadata(path) {
            Ok(current) => Ok(held.dev() == current.dev() && held.ino() == current.ino()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err.into()),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (file, path);
        Ok(true)
    }
}

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

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)?;
        create_file(&mut NativeFile(&mut file), path, snapshot, now_unix_ms())
    }

    pub fn open(path: impl AsRef<Path>) -> RdbFileResult<EmbeddedRdbOpen> {
        Self::open_inner(path, true)
    }

    /// Probe a format without waiting on a native pager's lifetime writer lock.
    pub fn try_open(path: impl AsRef<Path>) -> RdbFileResult<EmbeddedRdbOpen> {
        let path = path.as_ref();
        let mut file = File::open(path)?;
        native_lock::try_lock_shared(&file)?;
        Self::open_inner_with_file(&mut NativeFile(&mut file), path, true)
    }

    /// Open the selected snapshot and return the validated image.
    pub fn open_with_snapshot(
        path: impl AsRef<Path>,
    ) -> RdbFileResult<(EmbeddedRdbOpen, Option<Vec<u8>>)> {
        let path = path.as_ref();
        let mut file = File::open(path)?;
        native_lock::lock_shared(&file)?;
        Self::open_inner_with_snapshot(&mut NativeFile(&mut file), path)
    }

    /// Nonblocking format probe that also reads the validated snapshot.
    pub fn try_open_with_snapshot(
        path: impl AsRef<Path>,
    ) -> RdbFileResult<(EmbeddedRdbOpen, Option<Vec<u8>>)> {
        let path = path.as_ref();
        let mut file = File::open(path)?;
        native_lock::try_lock_shared(&file)?;
        Self::open_inner_with_snapshot(&mut NativeFile(&mut file), path)
    }

    fn open_inner(
        path: impl AsRef<Path>,
        validate_snapshot_refs: bool,
    ) -> RdbFileResult<EmbeddedRdbOpen> {
        let path = path.as_ref();
        let mut file = File::open(path)?;
        // Readers wait for an in-flight publication. On Windows an unlocked
        // ReadFile races the writer's mandatory byte-range lock and fails 33.
        native_lock::lock_shared(&file)?;
        Self::open_inner_with_file(&mut NativeFile(&mut file), path, validate_snapshot_refs)
    }

    pub fn write_snapshot_with_wal_capacity(
        path: impl AsRef<Path>,
        snapshot: &[u8],
        min_wal_bytes: u64,
    ) -> RdbFileResult<EmbeddedRdbOpen> {
        let path = path.as_ref();
        group_commit::wait_for_pending_appends(path)?;
        let path_lock = embedded_path_lock(path);
        let mut path_guard = path_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        path_guard.tail = None;
        // One handle for the whole operation: it carries the exclusive lock,
        // and on Windows that lock would deny any *other* handle this module
        // opened to the same path (see `open_inner_with_file`).
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        native_lock::lock_exclusive(&file)?;

        let result = checkpoint_file(&mut NativeFile(&mut file), path, snapshot, min_wal_bytes)?;
        native_lock::unlock(&file)?;
        Ok(result)
    }

    pub fn open_strict_manifest(path: impl AsRef<Path>) -> RdbFileResult<EmbeddedRdbOpen> {
        let path = path.as_ref();
        let file = File::open(path)?;
        native_lock::lock_shared(&file)?;
        Self::open_strict_manifest_file(file, path)
    }

    /// Nonblocking format probe without reading the snapshot payload twice.
    pub fn try_open_strict_manifest(path: impl AsRef<Path>) -> RdbFileResult<EmbeddedRdbOpen> {
        let path = path.as_ref();
        let file = File::open(path)?;
        native_lock::try_lock_shared(&file)?;
        Self::open_strict_manifest_file(file, path)
    }

    fn open_strict_manifest_file(mut file: File, path: &Path) -> RdbFileResult<EmbeddedRdbOpen> {
        let selected_superblock = [
            read_superblock_copy(&mut NativeFile(&mut file), 0)?,
            read_superblock_copy(&mut NativeFile(&mut file), 1)?,
        ]
        .into_iter()
        .flatten()
        .max_by_key(|superblock| superblock.generation)
        .ok_or_else(|| RdbFileError::InvalidOperation("no valid embedded superblock".into()))?;

        let mut manifest = read_manifest(&mut NativeFile(&mut file), selected_superblock)?;
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
        native_lock::lock_shared(&file)?;
        read_snapshot_file(&mut NativeFile(&mut file), open)
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
        let mut path_guard = path_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Take before any fallible I/O: a failed append must leave no cached
        // state that could describe a partially published mutation.
        let cached_tail = path_guard.tail.take();
        // Single locked handle for the whole append — see
        // `write_snapshot_with_wal_capacity` and `open_inner_with_file`.
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        native_lock::lock_exclusive(&file)?;

        let open = Self::open_inner_with_file(&mut NativeFile(&mut file), path, false)?;
        let stamp = file_mutation_stamp(&file)?;
        let cached_tail = cached_tail.filter(|tail| {
            stamp.as_ref() == Some(&tail.stamp) && tail.superblock == open.selected_superblock
        });
        let snapshot_validated = cached_tail.is_some();
        let wal_scan = match cached_tail {
            Some(tail) => WalScan {
                next_sequence: tail.next_sequence,
                previous_frame_crc: tail.previous_frame_crc,
                valid_bytes: tail.valid_bytes,
                payloads: Vec::new(),
            },
            None => match wal_region_start_checked(&open)? {
                Some(wal_start) => {
                    scan_wal_with_file(&mut NativeFile(&mut file), &open, wal_start)?
                }
                None => WalScan {
                    next_sequence: 1,
                    ..WalScan::default()
                },
            },
        };
        let published = append_file(
            &mut NativeFile(&mut file),
            path,
            payloads,
            open,
            wal_scan,
            snapshot_validated,
        )?;
        let next_stamp = file_mutation_stamp(&file)?;
        native_lock::unlock(&file)?;
        if let Some(stamp) = next_stamp
            .filter(|_| published.open.selected_superblock.generation == published.generation)
        {
            path_guard.tail = Some(CachedWalTail {
                stamp,
                superblock: published.open.selected_superblock,
                next_sequence: published.next_sequence,
                previous_frame_crc: published.previous_frame_crc,
                valid_bytes: published.valid_bytes,
            });
        }
        Ok(published.open)
    }
    pub fn write_superblock_copy(
        file: &mut File,
        superblock: &EmbeddedRdbSuperblock,
    ) -> RdbFileResult<()> {
        super::write_superblock_copy(&mut NativeFile(file), superblock)
    }
}

fn embedded_path_lock(path: &Path) -> Arc<Mutex<EmbeddedPathState>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<EmbeddedPathState>>>>> =
        OnceLock::new();
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks
        .entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(EmbeddedPathState::default())))
        .clone()
}

/// Scan the WAL, opening the file for reading. Callers already holding a
/// handle (the writer path) use [`scan_wal_with_file`] instead.
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
    native_lock::lock_shared(&file)?;
    scan_wal_with_file(&mut NativeFile(&mut file), open, wal_start)
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
