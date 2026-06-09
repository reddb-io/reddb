//! Shared-memory file substrate (gh-475).
//!
//! When provisioning is enabled (tier-wired for `Standard` and above in a
//! later slice), opening a database creates the sibling file selected by
//! `reddb_file::layout::shm_path` with a deterministic binary header owned by
//! `reddb-file`. This module owns the runtime lock policy: current owner pid,
//! generation counter, reader registry, and crash takeover decisions.
//!
//! ## Lock protocol
//!
//! 1. On open, the writer attempts to claim ownership. If the magic is
//!    absent or invalid, it initialises the header with its pid and a
//!    fresh generation. If the magic is present, it inspects
//!    `owner_pid`: if the pid is no longer alive, this is a crash — the
//!    new owner bumps `generation`, rewrites the header, and the load
//!    path treats `reader_count` as authoritative for cleanup decisions
//!    in a later slice.
//! 2. Embedded readers attach by incrementing `reader_count` and
//!    detach by decrementing it. The count survives the writer crash
//!    so the next opener sees how many stale handles must be cleaned.
//! 3. mmap-ing the file is a follow-up slice; the on-disk substrate is
//!    valid without it. The file size is fixed at one OS page so mmap
//!    integration is mechanical when wired.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub use reddb_file::{ShmHeader, SHM_FILE_SIZE, SHM_HEADER_SIZE, SHM_MAGIC, SHM_VERSION};

static SHM_POLICY: AtomicU8 = AtomicU8::new(0);

/// Process-wide opt-in for SHM provisioning. Default off so
/// existing single-writer flows (`minimal`) keep their current shape.
/// Tier wiring should call this with `true` when `tier >= Standard`.
/// Escape hatch: `REDDB_SHM_PROVISION=1`.
pub fn set_shm_provisioning_enabled(enabled: bool) {
    SHM_POLICY.store(if enabled { 1 } else { 2 }, Ordering::Relaxed);
}

/// Whether the open path should provision a SHM file.
pub fn shm_provisioning_enabled() -> bool {
    match SHM_POLICY.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => std::env::var("REDDB_SHM_PROVISION")
            .ok()
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
            .unwrap_or(false),
    }
}

/// Sibling path of the `-shm` substrate file for a given data file.
pub fn shm_path_for(data_path: &Path) -> PathBuf {
    reddb_file::layout::shm_path(data_path)
}

/// Outcome of a provisioning attempt — distinguishes a clean takeover
/// from a crash recovery for diagnostics and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShmProvisionState {
    /// File did not exist; created fresh.
    Created,
    /// Existing owner pid is still alive; attached as an additional handle.
    AttachedToLiveOwner,
    /// Existing owner pid is dead; took ownership and bumped generation.
    RecoveredFromCrash,
    /// File existed but the header was unreadable; reinitialised.
    HealedCorruptHeader,
}

/// Handle to the provisioned `-shm` file. Drop semantics intentionally
/// minimal in this slice — callers must invoke [`Self::detach_reader`]
/// explicitly to mirror the eventual mmap-backed lifecycle.
pub struct ShmHandle {
    pub path: PathBuf,
    pub header: ShmHeader,
    pub state: ShmProvisionState,
    file: File,
}

impl ShmHandle {
    /// Current generation counter. Bumps on every crash recovery so
    /// observers can detect that ownership changed between snapshots.
    pub fn generation(&self) -> u64 {
        self.header.generation
    }

    /// Increment the on-disk reader counter. Returns the new count.
    pub fn attach_reader(&mut self) -> io::Result<u64> {
        self.header.reader_count = self.header.reader_count.saturating_add(1);
        self.rewrite_header()?;
        Ok(self.header.reader_count)
    }

    /// Decrement the on-disk reader counter (saturating). Returns new count.
    pub fn detach_reader(&mut self) -> io::Result<u64> {
        self.header.reader_count = self.header.reader_count.saturating_sub(1);
        self.rewrite_header()?;
        Ok(self.header.reader_count)
    }

    /// Refresh `last_heartbeat_ms` to the current unix-ms.
    pub fn heartbeat(&mut self) -> io::Result<()> {
        self.header.last_heartbeat_ms = unix_ms_now();
        self.rewrite_header()
    }

    fn rewrite_header(&mut self) -> io::Result<()> {
        reddb_file::write_shm_header_to_file(&mut self.file, &self.header)
    }
}

/// Provision the `-shm` substrate for a data file. Idempotent; safe to
/// call from every open. See module docs for the lock protocol.
pub fn provision_shm(data_path: &Path) -> io::Result<ShmHandle> {
    let path = shm_path_for(data_path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;

    let metadata = file.metadata()?;
    let fresh = metadata.len() == 0;

    if fresh {
        let header = ShmHeader::new(current_pid(), 1, 0, unix_ms_now());
        reddb_file::initialize_shm_file(&mut file, &header)?;
        return Ok(ShmHandle {
            path,
            header,
            state: ShmProvisionState::Created,
            file,
        });
    }

    let existing = match reddb_file::read_shm_header_from_file(&mut file) {
        Ok(header) => Some(header),
        Err(_) => None,
    };

    let (header, state) = match existing {
        Some(prev) if pid_alive(prev.owner_pid) && prev.owner_pid != current_pid() => {
            // Attach to live owner — increment reader_count, keep generation.
            let next = ShmHeader::new(
                prev.owner_pid,
                prev.generation,
                prev.reader_count.saturating_add(1),
                prev.last_heartbeat_ms,
            );
            (next, ShmProvisionState::AttachedToLiveOwner)
        }
        Some(prev) if prev.owner_pid == current_pid() => {
            // Same-process reopen; refresh heartbeat, keep counters.
            let next = ShmHeader::new(
                prev.owner_pid,
                prev.generation,
                prev.reader_count,
                unix_ms_now(),
            );
            (next, ShmProvisionState::AttachedToLiveOwner)
        }
        Some(prev) => {
            // Owner is dead — take over, bump generation, clear reader count.
            let next = ShmHeader::new(
                current_pid(),
                prev.generation.saturating_add(1),
                0,
                unix_ms_now(),
            );
            (next, ShmProvisionState::RecoveredFromCrash)
        }
        None => {
            // File exists but header is unreadable — heal in place.
            let next = ShmHeader::new(current_pid(), 1, 0, unix_ms_now());
            reddb_file::initialize_shm_file(&mut file, &next)?;
            (next, ShmProvisionState::HealedCorruptHeader)
        }
    };

    reddb_file::write_shm_header_to_file(&mut file, &header)?;

    Ok(ShmHandle {
        path,
        header,
        state,
        file,
    })
}

/// Read the current header without taking ownership. Returns `Ok(None)`
/// when the file does not exist; surfaces a real I/O error if the file
/// is present but unreadable.
pub fn read_shm_header(data_path: &Path) -> io::Result<Option<ShmHeader>> {
    let path = shm_path_for(data_path);
    if !path.exists() {
        return Ok(None);
    }
    let mut file = OpenOptions::new().read(true).open(&path)?;
    reddb_file::read_shm_header_from_file(&mut file).map(Some)
}

fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn current_pid() -> u32 {
    std::process::id()
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // `kill(pid, 0)` returns 0 if the process exists, -1 otherwise.
    // EPERM still implies the process exists (we just can't signal it).
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    io::Error::last_os_error()
        .raw_os_error()
        .map(|e| e == libc::EPERM)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    // Conservative: assume alive on non-unix until a platform-specific
    // probe is wired. Crash recovery on those platforms is a follow-up.
    true
}
