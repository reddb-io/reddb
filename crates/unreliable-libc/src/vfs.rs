//! A minimal durable-I/O abstraction (DST Fatia, #1355).
//!
//! [`Vfs`] / [`VfsFile`] are the in-process counterpart to the `LD_PRELOAD`
//! shim: where the shim gives real-syscall fidelity, this trait pair lets the
//! durable writers run against an in-memory, seed-driven, fault-injecting
//! backend that is fast and OS-portable for exhaustive fault enumeration.
//!
//! The shape mirrors the existing `RemoteBackend` two-trait precedent in
//! `reddb-server`: one trait for the namespace operations (`open` / `rename` /
//! `sync_dir`) and one for the per-file operations (`read` / `write_all` /
//! `seek` / `sync_all`). [`StdVfs`] is the production default — it is exactly
//! today's `std::fs` behavior, so routing a durable writer through `&StdVfs`
//! leaves the on-disk bytes unchanged. [`SimVfs`] is the fault-injecting
//! backend used only by tests.
//!
//! # Fault model ([`SimVfs`])
//!
//! Every fault decision is derived from the construction seed, so any discovered
//! failure reproduces exactly:
//!
//! * **Torn writes** — on a power-cut, unsynced writes survive only as the
//!   longest in-order prefix, with the boundary write possibly torn to a byte
//!   prefix. A `sync_all` is the only thing that makes a write durable, and the
//!   independent per-file tearing also models cross-file write reordering.
//! * **Dropped / reordered `fsync`** — a `sync_all` may fail with `EIO`: the
//!   flush was dropped, or an unsafe reordering left it incomplete, so
//!   durability did not happen and a correct writer must stop rather than
//!   advance its frontier. (Modeled as a loud failure, matching the
//!   `LD_PRELOAD` shim, so a follower artifact never becomes durable ahead of
//!   the WAL.)
//! * **`ENOSPC`** — a `write` may fail with a storage-full error before applying
//!   any bytes, which a correct writer propagates without advancing its
//!   frontier.
//! * **Partial rename** — a `rename` may revert (the directory entry never
//!   became durable, so the old target survives) or leave a torn target when
//!   the source was not fully durable. A `sync_dir` after the rename is what
//!   makes it durable.

use crate::prng::SplitMix64;
use std::collections::HashMap;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// How a file is opened. Mirrors the subset of `OpenOptions` the durable
/// writers actually use.
#[derive(Debug, Clone, Copy)]
pub struct OpenMode {
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub truncate: bool,
}

impl OpenMode {
    /// Create-or-truncate for writing (the WAL log's open mode).
    pub fn create_truncate() -> Self {
        Self {
            read: false,
            write: true,
            create: true,
            truncate: true,
        }
    }

    /// Create-if-absent for writing without truncating (the dual-superblock
    /// open mode: each checkpoint overwrites only its own slot).
    pub fn create_keep() -> Self {
        Self {
            read: true,
            write: true,
            create: true,
            truncate: false,
        }
    }
}

/// A handle to one open file. The durable writers only need to append, seek to a
/// fixed offset, read back, and force durability.
pub trait VfsFile {
    /// Write the entire buffer, looping over short writes like `Write::write_all`.
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()>;
    /// Read up to `buf.len()` bytes at the current position.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    /// Reposition the cursor.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64>;
    /// Force the file's contents durable (the moment a write can survive a crash).
    fn sync_all(&mut self) -> io::Result<()>;
}

/// A durable-I/O namespace: open files, rename, and force directory durability.
pub trait Vfs {
    /// The per-file handle this backend produces.
    type File: VfsFile;

    /// Open (or create) a file at `path`.
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Self::File>;
    /// Atomically rename `from` to `to` (subject to fault injection).
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Force a directory's entries durable (makes a prior `rename` survive a crash).
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;
}

// --------------------------------------------------------------------------
// StdVfs — the production default (today's `std::fs` behavior, unchanged).
// --------------------------------------------------------------------------

/// The real filesystem backend. Routing a durable writer through `&StdVfs`
/// produces byte-for-byte the same artifacts as direct `std::fs` calls.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdVfs;

/// A real `std::fs::File`.
#[derive(Debug)]
pub struct StdFile(std::fs::File);

impl VfsFile for StdFile {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.0.write_all(buf)
    }
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        io::Read::read(&mut self.0, buf)
    }
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.0.seek(pos)
    }
    fn sync_all(&mut self) -> io::Result<()> {
        self.0.sync_all()
    }
}

impl Vfs for StdVfs {
    type File = StdFile;

    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<StdFile> {
        std::fs::OpenOptions::new()
            .read(mode.read)
            .write(mode.write)
            .create(mode.create)
            .truncate(mode.truncate)
            .open(path)
            .map(StdFile)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        std::fs::File::open(dir).and_then(|d| d.sync_all())
    }
}

// --------------------------------------------------------------------------
// SimVfs — in-memory, seed-driven, fault-injecting backend (test only).
// --------------------------------------------------------------------------

/// Probabilities (in parts-per-million) and toggles for the fault families.
/// A `ppm` of `0` disables that fault; `1_000_000` always fires it.
#[derive(Debug, Clone, Copy)]
pub struct SimFaultConfig {
    /// A `write` fails with `ENOSPC` before applying any bytes.
    pub enospc_ppm: u64,
    /// A `sync_all` fails (`EIO`): the fsync was dropped, so durability did not
    /// complete and the writer must stop instead of advancing its frontier.
    pub drop_fsync_ppm: u64,
    /// A `sync_all` fails (`EIO`): an unsafe reordering left the flush
    /// incomplete at fsync time, so durability did not complete.
    pub reorder_fsync_ppm: u64,
    /// A `rename` does not become durable (reverts to the old target on crash).
    pub revert_rename_ppm: u64,
    /// A `rename` leaves a torn (byte-prefix) target on crash.
    pub torn_rename_ppm: u64,
    /// Force a power-cut after this many durability syscalls (`None` = run to
    /// completion). A power-cut torns the unsynced tail of every file.
    pub power_cut_after: Option<u64>,
}

impl SimFaultConfig {
    /// No faults: a faithful in-memory mirror of `StdVfs`.
    pub fn none() -> Self {
        Self {
            enospc_ppm: 0,
            drop_fsync_ppm: 0,
            reorder_fsync_ppm: 0,
            revert_rename_ppm: 0,
            torn_rename_ppm: 0,
            power_cut_after: None,
        }
    }
}

/// The error returned by [`SimVfs`] once a power-cut has been triggered: every
/// subsequent durable call fails, just as a killed process can do no more I/O.
pub const POWER_CUT_MESSAGE: &str = "simulated power-cut: device gone";

#[derive(Debug, Clone, Default)]
struct SimFile {
    /// Bytes guaranteed to survive a crash (promoted by an honored `sync_all`).
    durable: Vec<u8>,
    /// Bytes as the writer currently sees them.
    live: Vec<u8>,
    /// Writes applied since the last honored `sync_all`, in order: `(offset, bytes)`.
    dirty: Vec<(usize, Vec<u8>)>,
}

#[derive(Debug)]
struct SimState {
    files: HashMap<PathBuf, SimFile>,
    rng: SplitMix64,
    cfg: SimFaultConfig,
    /// Durability syscalls (`write` / `sync_all` / `rename` / `sync_dir`) so far.
    syscalls: u64,
    /// Set once a power-cut fires; all further durable calls fail.
    crashed: bool,
}

impl SimState {
    fn fires(&mut self, ppm: u64) -> bool {
        ppm != 0 && self.rng.below(1_000_000) < ppm
    }

    /// Charge one durability syscall and trip the power-cut once the budget is
    /// reached. Returns an error if the device is (now) gone.
    fn charge(&mut self) -> io::Result<()> {
        if self.crashed {
            return Err(power_cut_error());
        }
        self.syscalls += 1;
        if let Some(budget) = self.cfg.power_cut_after {
            if self.syscalls >= budget {
                self.trip_power_cut();
                return Err(power_cut_error());
            }
        }
        Ok(())
    }

    /// Collapse every file to its post-crash durable image (torn unsynced tail)
    /// and mark the device gone.
    fn trip_power_cut(&mut self) {
        for file in self.files.values_mut() {
            let image = crash_image(file, &mut self.rng);
            file.durable = image.clone();
            file.live = image;
            file.dirty.clear();
        }
        self.crashed = true;
    }
}

/// Reconstruct one file's post-crash durable image: start from the last durable
/// snapshot and replay the dirty writes in order, keeping the longest prefix and
/// possibly tearing the boundary write to a byte prefix.
fn crash_image(file: &SimFile, rng: &mut SplitMix64) -> Vec<u8> {
    let mut image = file.durable.clone();
    let total = file.dirty.len() as u64;
    // How many dirty writes reached the platter fully (0..=total).
    let survived = as_usize(rng.below(total + 1));
    for (offset, bytes) in file.dirty.iter().take(survived) {
        apply_write(&mut image, *offset, bytes);
    }
    // The next write (if any) may be torn to a byte prefix.
    if let Some((offset, bytes)) = file.dirty.get(survived) {
        let keep = as_usize(rng.below(bytes.len() as u64 + 1));
        apply_write(&mut image, *offset, &bytes[..keep]);
    }
    image
}

/// Saturating `u64` → `usize` (test tooling never addresses past `usize::MAX`).
fn as_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// Overwrite `image[offset..offset + bytes.len()]`, zero-extending as needed.
fn apply_write(image: &mut Vec<u8>, offset: usize, bytes: &[u8]) {
    let end = offset + bytes.len();
    if image.len() < end {
        image.resize(end, 0);
    }
    image[offset..end].copy_from_slice(bytes);
}

fn power_cut_error() -> io::Error {
    io::Error::other(POWER_CUT_MESSAGE)
}

fn enospc_error() -> io::Error {
    // `ErrorKind::StorageFull` is unstable; use the raw ENOSPC errno so callers
    // see a real out-of-space error.
    io::Error::from_raw_os_error(28)
}

fn fsync_failed_error() -> io::Error {
    // A dropped / unsafely-reordered fsync did not durably complete. Surface it
    // as EIO, the same loud failure the `LD_PRELOAD` shim raises on fsync.
    io::Error::from_raw_os_error(5)
}

/// An in-memory, seed-driven, fault-injecting [`Vfs`]. Cheap to clone — clones
/// share the same simulated device.
#[derive(Debug, Clone)]
pub struct SimVfs {
    state: Arc<Mutex<SimState>>,
}

impl SimVfs {
    /// Build a backend seeded by `seed` with the given fault config.
    pub fn new(seed: u64, cfg: SimFaultConfig) -> Self {
        Self {
            state: Arc::new(Mutex::new(SimState {
                files: HashMap::new(),
                // Distinct sub-stream from the workload's own PRNG.
                rng: SplitMix64::new(seed ^ 0x5346_5F46_4C54_5354), // "SF_FLTST"
                cfg,
                syscalls: 0,
                crashed: false,
            })),
        }
    }

    /// Trigger a power-cut now (torns every unsynced tail). Idempotent.
    pub fn power_cut(&self) {
        let mut state = self.lock();
        if !state.crashed {
            state.trip_power_cut();
        }
    }

    /// Whether a power-cut has fired.
    pub fn is_crashed(&self) -> bool {
        self.lock().crashed
    }

    /// Number of durability syscalls charged so far.
    pub fn syscalls(&self) -> u64 {
        self.lock().syscalls
    }

    /// Snapshot every file's post-crash durable image. If no power-cut has
    /// fired, this is the current durable state; otherwise it is the torn image
    /// left by the crash. Use this to materialize the device for the oracle.
    pub fn crash_image(&self) -> HashMap<PathBuf, Vec<u8>> {
        let mut state = self.lock();
        if !state.crashed {
            // Compute a torn image without mutating state, so callers can both
            // inspect and keep simulating.
            let mut out = HashMap::new();
            let SimState { files, rng, .. } = &mut *state;
            for (path, file) in files.iter() {
                out.insert(path.clone(), crash_image(file, rng));
            }
            out
        } else {
            state
                .files
                .iter()
                .map(|(p, f)| (p.clone(), f.durable.clone()))
                .collect()
        }
    }

    /// Materialize the post-crash device into a real directory so the
    /// `Path`-based recovery oracle can run against it.
    pub fn materialize(&self, dir: &Path) -> io::Result<()> {
        for (path, bytes) in self.crash_image() {
            let name = path
                .file_name()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "file has no name"))?;
            std::fs::write(dir.join(name), bytes)?;
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SimState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A handle to one [`SimVfs`] file. Writes mutate the shared device.
#[derive(Debug)]
pub struct SimFileHandle {
    state: Arc<Mutex<SimState>>,
    path: PathBuf,
    pos: u64,
}

impl SimFileHandle {
    fn lock(&self) -> std::sync::MutexGuard<'_, SimState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl VfsFile for SimFileHandle {
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        let mut state = self.lock();
        state.charge()?;
        let enospc_ppm = state.cfg.enospc_ppm;
        if state.fires(enospc_ppm) {
            return Err(enospc_error());
        }
        let offset = usize::try_from(self.pos).unwrap_or(usize::MAX);
        if let Some(file) = state.files.get_mut(&self.path) {
            apply_write(&mut file.live, offset, buf);
            file.dirty.push((offset, buf.to_vec()));
        } else {
            return Err(io::Error::new(io::ErrorKind::NotFound, "file not open"));
        }
        drop(state);
        self.pos += buf.len() as u64;
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let state = self.lock();
        let file = state
            .files
            .get(&self.path)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "file not open"))?;
        let start = usize::try_from(self.pos)
            .unwrap_or(usize::MAX)
            .min(file.live.len());
        let end = (start + buf.len()).min(file.live.len());
        let n = end - start;
        buf[..n].copy_from_slice(&file.live[start..end]);
        drop(state);
        self.pos += n as u64;
        Ok(n)
    }

    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let len = {
            let state = self.lock();
            state
                .files
                .get(&self.path)
                .map_or(0, |f| f.live.len() as u64)
        };
        let next = match pos {
            SeekFrom::Start(n) => n,
            SeekFrom::End(delta) => len.saturating_add_signed(delta),
            SeekFrom::Current(delta) => self.pos.saturating_add_signed(delta),
        };
        self.pos = next;
        Ok(next)
    }

    fn sync_all(&mut self) -> io::Result<()> {
        let mut state = self.lock();
        state.charge()?;
        // A dropped or unsafely-reordered fsync surfaces as a *loud* durability
        // failure (`EIO`), exactly like the `LD_PRELOAD` shim's fsync fault: the
        // request did not durably complete, so a correct writer must stop rather
        // than advance its committed frontier. Modeling it as a silent success
        // would let a follower artifact (superblock / manifest) become durable
        // ahead of the WAL — a cross-file inversion the recovery oracle is not
        // meant to tolerate and that a correct writer cannot defend against.
        // Both coins are drawn so the seed stream stays stable.
        let (drop_ppm, reorder_ppm) = (state.cfg.drop_fsync_ppm, state.cfg.reorder_fsync_ppm);
        let dropped = state.fires(drop_ppm);
        let reordered = state.fires(reorder_ppm);
        if dropped || reordered {
            return Err(fsync_failed_error());
        }
        if let Some(file) = state.files.get_mut(&self.path) {
            // An honored fsync makes the whole live image durable. Durable
            // length is therefore monotonic, so previously-committed data is
            // never lost — only the unsynced tail is ever at risk on a crash.
            file.durable = file.live.clone();
            file.dirty.clear();
        }
        Ok(())
    }
}

impl Vfs for SimVfs {
    type File = SimFileHandle;

    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<SimFileHandle> {
        let mut state = self.lock();
        if state.crashed {
            return Err(power_cut_error());
        }
        let exists = state.files.contains_key(path);
        if !exists && !mode.create {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no such file"));
        }
        let entry = state.files.entry(path.to_path_buf()).or_default();
        if mode.truncate {
            entry.live.clear();
            entry.durable.clear();
            entry.dirty.clear();
        }
        Ok(SimFileHandle {
            state: Arc::clone(&self.state),
            path: path.to_path_buf(),
            pos: 0,
        })
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut state = self.lock();
        state.charge()?;
        let source = state
            .files
            .remove(from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "rename source missing"))?;
        let (revert_ppm, torn_ppm) = (state.cfg.revert_rename_ppm, state.cfg.torn_rename_ppm);
        let revert = state.fires(revert_ppm);
        let torn = !revert && state.fires(torn_ppm);
        // POSIX rename is atomic, so the writer's *live* view always sees the
        // full new contents at `to`. The fault only governs what survives a
        // crash before a `sync_dir`: a revert keeps the old durable target, and
        // a torn rename leaves only a byte prefix durable.
        let mut dest = source.clone();
        dest.live = source.live.clone();
        if revert {
            // Keep whatever was previously durable at `to` (old target / absent).
            let old = state
                .files
                .get(to)
                .map(|f| f.durable.clone())
                .unwrap_or_default();
            dest.durable = old;
        } else if torn {
            let keep = as_usize(state.rng.below(source.live.len() as u64 + 1));
            dest.durable = source.live[..keep].to_vec();
        } else {
            // Not yet durable until sync_dir; model the source's durable bytes
            // carrying over (a clean rename of already-synced data).
            dest.durable = source.durable.clone();
        }
        dest.dirty.clear();
        state.files.insert(to.to_path_buf(), dest);
        Ok(())
    }

    fn sync_dir(&self, _dir: &Path) -> io::Result<()> {
        let mut state = self.lock();
        state.charge()?;
        // Directory durability makes the current live view of every file its
        // durable view at the namespace level — in particular it commits a
        // prior rename so it can no longer revert.
        for file in state.files.values_mut() {
            file.durable = file.live.clone();
            file.dirty.clear();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_back<V: Vfs>(vfs: &V, path: &Path) -> Vec<u8> {
        let mut f = vfs.open(path, OpenMode::create_keep()).unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 64];
        loop {
            let n = f.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        out
    }

    #[test]
    fn std_vfs_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let vfs = StdVfs;
        let path = dir.path().join("a.bin");
        let mut f = vfs.open(&path, OpenMode::create_truncate()).unwrap();
        f.write_all(b"hello").unwrap();
        f.sync_all().unwrap();
        assert_eq!(read_back(&vfs, &path), b"hello");
    }

    #[test]
    fn sim_vfs_no_faults_mirrors_std() {
        let vfs = SimVfs::new(7, SimFaultConfig::none());
        let path = Path::new("a.bin");
        let mut f = vfs.open(path, OpenMode::create_truncate()).unwrap();
        f.write_all(b"durable").unwrap();
        f.sync_all().unwrap();
        let image = vfs.crash_image();
        assert_eq!(image.get(path).unwrap().as_slice(), b"durable");
    }

    #[test]
    fn unsynced_write_is_torn_or_lost_on_crash() {
        let vfs = SimVfs::new(3, SimFaultConfig::none());
        let path = Path::new("w.bin");
        let mut f = vfs.open(path, OpenMode::create_truncate()).unwrap();
        f.write_all(b"committed").unwrap();
        f.sync_all().unwrap();
        // This second write is never synced.
        f.write_all(b"-volatile").unwrap();
        vfs.power_cut();
        let image = vfs.crash_image();
        let bytes = image.get(path).unwrap();
        // The synced prefix always survives; the unsynced tail is at most a
        // prefix of what was written.
        assert!(bytes.starts_with(b"committed"));
        assert!(bytes.len() <= b"committed-volatile".len());
    }

    #[test]
    fn enospc_is_seed_controlled_and_reproducible() {
        let cfg = SimFaultConfig {
            enospc_ppm: 1_000_000,
            ..SimFaultConfig::none()
        };
        let vfs = SimVfs::new(11, cfg);
        let mut f = vfs
            .open(Path::new("e.bin"), OpenMode::create_truncate())
            .unwrap();
        let err = f.write_all(b"x").unwrap_err();
        assert_eq!(err.raw_os_error(), Some(28));
    }

    #[test]
    fn dropped_fsync_fails_loudly_and_keeps_nothing_durable() {
        let cfg = SimFaultConfig {
            drop_fsync_ppm: 1_000_000,
            ..SimFaultConfig::none()
        };
        let vfs = SimVfs::new(5, cfg);
        let path = Path::new("d.bin");
        let mut f = vfs.open(path, OpenMode::create_truncate()).unwrap();
        f.write_all(b"data").unwrap();
        // The fsync is dropped: it fails loudly so the writer cannot mistake the
        // tail for durable.
        let err = f.sync_all().unwrap_err();
        assert_eq!(err.raw_os_error(), Some(5));
        vfs.power_cut();
        // The write was never durably synced, so the crash may keep at most a
        // torn prefix of it — never more than was written, and never promoted
        // to a durable commit the writer was told it had.
        let bytes = vfs.crash_image().get(path).unwrap().clone();
        assert!(bytes.len() <= b"data".len());
        assert!(b"data".starts_with(bytes.as_slice()));
    }

    #[test]
    fn power_cut_after_budget_stops_io() {
        let cfg = SimFaultConfig {
            power_cut_after: Some(2),
            ..SimFaultConfig::none()
        };
        let vfs = SimVfs::new(1, cfg);
        let mut f = vfs
            .open(Path::new("p.bin"), OpenMode::create_truncate())
            .unwrap();
        f.write_all(b"one").unwrap(); // syscall 1
        let err = f.write_all(b"two").unwrap_err(); // syscall 2 -> power-cut
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(vfs.is_crashed());
        assert!(f.write_all(b"three").is_err());
    }

    #[test]
    fn revert_rename_keeps_old_target() {
        let cfg = SimFaultConfig {
            revert_rename_ppm: 1_000_000,
            ..SimFaultConfig::none()
        };
        let vfs = SimVfs::new(2, cfg);
        // Old, durable target.
        let mut old = vfs
            .open(Path::new("dst"), OpenMode::create_truncate())
            .unwrap();
        old.write_all(b"OLD").unwrap();
        old.sync_all().unwrap();
        // New source, synced, then renamed over the target.
        let mut tmp = vfs
            .open(Path::new("tmp"), OpenMode::create_truncate())
            .unwrap();
        tmp.write_all(b"NEWVALUE").unwrap();
        tmp.sync_all().unwrap();
        vfs.rename(Path::new("tmp"), Path::new("dst")).unwrap();
        // Crash before sync_dir: the rename reverts to the old durable target.
        vfs.power_cut();
        assert_eq!(
            vfs.crash_image().get(Path::new("dst")).unwrap().as_slice(),
            b"OLD"
        );
    }

    #[test]
    fn synced_rename_survives_crash() {
        let vfs = SimVfs::new(2, SimFaultConfig::none());
        let mut tmp = vfs
            .open(Path::new("tmp"), OpenMode::create_truncate())
            .unwrap();
        tmp.write_all(b"NEWVALUE").unwrap();
        tmp.sync_all().unwrap();
        vfs.rename(Path::new("tmp"), Path::new("dst")).unwrap();
        vfs.sync_dir(Path::new(".")).unwrap();
        vfs.power_cut();
        assert_eq!(
            vfs.crash_image().get(Path::new("dst")).unwrap().as_slice(),
            b"NEWVALUE"
        );
    }

    #[test]
    fn same_seed_same_fault_decisions() {
        let cfg = SimFaultConfig {
            drop_fsync_ppm: 500_000,
            enospc_ppm: 200_000,
            ..SimFaultConfig::none()
        };
        let run = |seed: u64| {
            let vfs = SimVfs::new(seed, cfg);
            let mut errs = Vec::new();
            for i in 0..20u8 {
                let mut f = vfs
                    .open(Path::new("s.bin"), OpenMode::create_keep())
                    .unwrap();
                errs.push(f.write_all(&[i]).is_err());
                errs.push(f.sync_all().is_err());
            }
            errs
        };
        assert_eq!(run(99), run(99), "same seed must replay identically");
    }
}
