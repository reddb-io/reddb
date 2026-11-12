//! Fixed-capacity file adapter for files whose physical space was allocated up front.
//!
//! A board provisioner owns filesystem mounting and preallocation. This adapter
//! keeps that policy below the canonical pager and makes every later
//! [`VfsFile::reserve`] a bounded check rather than another allocation.

use crate::VfsFile;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    sync::{Arc, Mutex, MutexGuard},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixedFileState {
    /// A newly preallocated file that has not received a RedDB header yet.
    Empty,
    /// A preallocated file containing an existing RedDB database.
    Existing,
}

struct Shared {
    file: Mutex<File>,
    capacity: u64,
    logical_len: Mutex<u64>,
}

/// A logical RedDB file backed by a physically preallocated fixed-size file.
///
/// The provisioner must guarantee that all `capacity` bytes have backing
/// storage. A merely sparse host file does not satisfy this durability
/// contract. The ESP-IDF firmware uses FatFS `f_expand(..., alloc_now=true)`.
/// Existing files recover their logical length from the newest canonical
/// RedDB superblock; physical preallocation must never be reported as used
/// logical extent to the pager.
pub struct FixedCapacityFile {
    shared: Arc<Shared>,
    position: u64,
}

impl FixedCapacityFile {
    pub fn open(mut file: File, capacity: u64, state: FixedFileState) -> io::Result<Self> {
        if capacity == 0 || file.metadata()?.len() != capacity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fixed-capacity file must already have its exact physical length",
            ));
        }
        let logical_len = match state {
            FixedFileState::Empty => 0,
            FixedFileState::Existing => canonical_logical_len(&mut file, capacity)?,
        };
        Ok(Self {
            shared: Arc::new(Shared {
                file: Mutex::new(file),
                capacity,
                logical_len: Mutex::new(logical_len),
            }),
            position: 0,
        })
    }

    pub fn capacity(&self) -> u64 {
        self.shared.capacity
    }

    fn lock(&self) -> io::Result<MutexGuard<'_, File>> {
        self.shared
            .file
            .lock()
            .map_err(|_| io::Error::other("fixed-capacity file lock poisoned"))
    }

    fn checked_end(&self, bytes: u64) -> io::Result<u64> {
        self.position
            .checked_add(bytes)
            .filter(|end| *end <= self.shared.capacity)
            .ok_or_else(|| capacity_error("write", self.position, bytes, self.shared.capacity))
    }
}

impl VfsFile for FixedCapacityFile {
    fn max_read_buffer_bytes(&self) -> usize {
        crate::engine::storage::page::PAGE_SIZE
    }

    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            shared: Arc::clone(&self.shared),
            position: self.position,
        })
    }

    fn same_file(&self, other: &Self) -> io::Result<bool> {
        Ok(Arc::ptr_eq(&self.shared, &other.shared))
    }

    fn file_len(&self) -> io::Result<u64> {
        self.shared
            .logical_len
            .lock()
            .map(|len| *len)
            .map_err(|_| io::Error::other("fixed-capacity length lock poisoned"))
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        if len > self.shared.capacity {
            return Err(capacity_error("set length", 0, len, self.shared.capacity));
        }
        let mut logical_len = self
            .shared
            .logical_len
            .lock()
            .map_err(|_| io::Error::other("fixed-capacity length lock poisoned"))?;
        if len == 0 && *logical_len != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "fixed-capacity database cannot be truncated",
            ));
        }
        *logical_len = len;
        Ok(())
    }

    fn reserve(&self, offset: u64, len: u64) -> io::Result<()> {
        offset
            .checked_add(len)
            .filter(|end| *end <= self.shared.capacity)
            .map(|_| ())
            .ok_or_else(|| capacity_error("reserve", offset, len, self.shared.capacity))
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let end = self.checked_end(bytes.len() as u64)?;
        let mut file = self.lock()?;
        file.seek(SeekFrom::Start(self.position))?;
        file.write_all(bytes)?;
        drop(file);
        let mut logical_len = self
            .shared
            .logical_len
            .lock()
            .map_err(|_| io::Error::other("fixed-capacity length lock poisoned"))?;
        *logical_len = (*logical_len).max(end);
        self.position = end;
        Ok(())
    }

    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let remaining = self.file_len()?.saturating_sub(self.position);
        let admitted = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let mut file = self.lock()?;
        file.seek(SeekFrom::Start(self.position))?;
        let count = file.read(&mut bytes[..admitted])?;
        drop(file);
        self.position += count as u64;
        Ok(count)
    }

    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let next = match position {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            SeekFrom::End(offset) => self.shared.capacity.checked_add_signed(offset),
        }
        .filter(|position| *position <= self.shared.capacity)
        .ok_or(io::ErrorKind::InvalidInput)?;
        self.position = next;
        Ok(next)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.lock()?.sync_all()
    }

    fn sync_data(&self) -> io::Result<()> {
        self.lock()?.sync_data()
    }
}

fn canonical_logical_len(file: &mut File, capacity: u64) -> io::Result<u64> {
    let mut zone = [0; crate::file::PAGED_SUPERBLOCK_ZONE_SIZE];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut zone)?;
    let selection = crate::file::select_paged_superblock(&zone).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "fixed-capacity database has no valid canonical superblock",
        )
    })?;
    let start = selection.copy_index * crate::file::PAGED_SUPERBLOCK_SLOT_SIZE;
    let end = start + crate::file::PAGED_SUPERBLOCK_SLOT_SIZE;
    let header = crate::file::decode_database_header(&zone[start..end])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let logical_len = u64::from(header.page_count)
        .checked_mul(crate::engine::storage::page::PAGE_SIZE as u64)
        .filter(|len| *len <= capacity)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "canonical database extent exceeds fixed physical capacity",
            )
        })?;
    Ok(logical_len)
}

fn capacity_error(operation: &str, offset: u64, len: u64, capacity: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::WriteZero,
        format!(
            "fixed-capacity {operation} at offset {offset} for {len} bytes exceeds {capacity} bytes"
        ),
    )
}
