//! Durable filesystem abstraction.
//!
//! [`Vfs`] and [`VfsFile`] define the filesystem operations required by
//! RedDB's durable write paths. [`StdVfs`] is the production implementation and
//! preserves the behavior of direct `std::fs` calls. Test crates can provide
//! alternate implementations without introducing a product-to-test dependency.

use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// How a file is opened. Mirrors the subset of `OpenOptions` used by durable
/// writers.
#[derive(Debug, Clone, Copy)]
pub struct OpenMode {
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub truncate: bool,
    pub create_new: bool,
}

impl OpenMode {
    /// Create or truncate a file for writing.
    pub fn create_truncate() -> Self {
        Self {
            read: false,
            write: true,
            create: true,
            truncate: true,
            create_new: false,
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
            create_new: false,
        }
    }

    /// Open an existing file for reading.
    pub fn read_only() -> Self {
        Self {
            read: true,
            write: false,
            create: false,
            truncate: false,
            create_new: false,
        }
    }

    /// Create a new file for writing, failing when it already exists.
    pub fn create_new() -> Self {
        Self {
            read: false,
            write: true,
            create: false,
            truncate: false,
            create_new: true,
        }
    }

    /// Create a file if absent and open it for writing without truncation.
    pub fn create_keep_write() -> Self {
        Self {
            read: false,
            write: true,
            create: true,
            truncate: false,
            create_new: false,
        }
    }
}

/// One directory entry returned by [`Vfs::read_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfsDirEntry {
    pub path: PathBuf,
    pub file_name: String,
    pub is_file: bool,
    pub is_dir: bool,
}

/// A handle to one open file.
pub trait VfsFile: Sized {
    /// Duplicate this handle while retaining access to the same file.
    fn try_clone(&self) -> io::Result<Self>;
    /// Return the file's logical length.
    fn file_len(&self) -> io::Result<u64>;
    /// Change the file's logical length.
    fn set_len(&self, len: u64) -> io::Result<()>;
    /// Write the entire buffer, looping over short writes like `Write::write_all`.
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()>;
    /// Read up to `buf.len()` bytes at the current position.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    /// Reposition the cursor.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64>;
    /// Force the file's contents durable.
    fn sync_all(&self) -> io::Result<()>;
    /// Force file contents durable without requiring metadata when supported.
    fn sync_data(&self) -> io::Result<()> {
        self.sync_all()
    }
    /// Best-effort block reservation that does not grow logical length.
    fn reserve(&self, _offset: u64, _len: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "file preallocation is not supported by this VFS",
        ))
    }
}

/// A durable-I/O namespace for files and directory entries.
pub trait Vfs: Clone {
    /// The per-file handle this backend produces.
    type File: VfsFile;

    /// Open or create a file at `path`.
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Self::File>;
    /// Atomically rename `from` to `to`.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Force a directory's entries durable.
    fn sync_dir(&self, dir: &Path) -> io::Result<()>;
    /// Create a directory and all missing parents.
    fn create_dir_all(&self, dir: &Path) -> io::Result<()>;
    /// List the immediate children of a directory.
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<VfsDirEntry>>;
    /// Remove one file.
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    /// Remove a directory tree.
    fn remove_dir_all(&self, path: &Path) -> io::Result<()>;
    /// Return whether a path exists.
    fn exists(&self, path: &Path) -> bool;
    /// Return whether a path names a regular file.
    fn is_file(&self, path: &Path) -> bool;
}

/// The production filesystem backend.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdVfs;

/// A real `std::fs::File` implementing [`VfsFile`].
#[derive(Debug)]
pub struct StdFile(std::fs::File);

impl VfsFile for StdFile {
    fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }

    fn file_len(&self) -> io::Result<u64> {
        self.0.metadata().map(|metadata| metadata.len())
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.0.set_len(len)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.0.write_all(buf)
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        io::Read::read(&mut self.0, buf)
    }

    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.0.seek(pos)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.0.sync_all()
    }

    fn sync_data(&self) -> io::Result<()> {
        self.0.sync_data()
    }

    #[cfg(target_os = "linux")]
    fn reserve(&self, offset: u64, len: u64) -> io::Result<()> {
        use std::os::unix::io::AsRawFd;

        if len == 0 {
            return Ok(());
        }
        // SAFETY: `self.0` owns a valid fd for this call; fallocate only
        // changes block reservations for that fd, never process memory.
        let result = unsafe {
            libc::fallocate(
                self.0.as_raw_fd(),
                libc::FALLOC_FL_KEEP_SIZE,
                offset as libc::off_t,
                len as libc::off_t,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl Vfs for StdVfs {
    type File = StdFile;

    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<StdFile> {
        std::fs::OpenOptions::new()
            .read(mode.read)
            .write(mode.write)
            .create(mode.create)
            .create_new(mode.create_new)
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

    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)
    }

    fn read_dir(&self, dir: &Path) -> io::Result<Vec<VfsDirEntry>> {
        std::fs::read_dir(dir)?
            .map(|entry| {
                let entry = entry?;
                let file_type = entry.file_type()?;
                Ok(VfsDirEntry {
                    path: entry.path(),
                    file_name: entry.file_name().to_string_lossy().into_owned(),
                    is_file: file_type.is_file(),
                    is_dir: file_type.is_dir(),
                })
            })
            .collect()
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_dir_all(path)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}
