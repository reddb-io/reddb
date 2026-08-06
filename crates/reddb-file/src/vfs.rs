//! Durable filesystem abstraction.
//!
//! [`Vfs`] and [`VfsFile`] define the filesystem operations required by
//! RedDB's durable write paths. [`StdVfs`] is the production implementation and
//! preserves the behavior of direct `std::fs` calls. Test crates can provide
//! alternate implementations without introducing a product-to-test dependency.

use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

/// How a file is opened. Mirrors the subset of `OpenOptions` used by durable
/// writers.
#[derive(Debug, Clone, Copy)]
pub struct OpenMode {
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub truncate: bool,
}

impl OpenMode {
    /// Create or truncate a file for writing.
    pub fn create_truncate() -> Self {
        Self {
            read: false,
            write: true,
            create: true,
            truncate: true,
        }
    }

    /// Create a file if absent and open it for reading and writing without
    /// truncating its contents.
    pub fn create_keep() -> Self {
        Self {
            read: true,
            write: true,
            create: true,
            truncate: false,
        }
    }
}

/// A handle to one open file.
pub trait VfsFile {
    /// Write the entire buffer, looping over short writes like `Write::write_all`.
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()>;
    /// Read up to `buf.len()` bytes at the current position.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    /// Reposition the cursor.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64>;
    /// Force the file's contents durable.
    fn sync_all(&mut self) -> io::Result<()>;
}

/// A durable-I/O namespace for files and directory entries.
pub trait Vfs {
    /// The per-file handle this backend produces.
    type File: VfsFile;

    /// Open or create a file at `path`.
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Self::File>;
    /// Atomically rename `from` to `to`.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Force a directory's entries durable.
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;
}

/// The production filesystem backend.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdVfs;

/// A real `std::fs::File` implementing [`VfsFile`].
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
        std::fs::File::open(dir).and_then(|directory| directory.sync_all())
    }
}
