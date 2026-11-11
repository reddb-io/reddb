//! Fixed-capacity file adapter for files whose physical space was allocated up front.
//!
//! A board provisioner owns filesystem mounting and preallocation. This adapter
//! keeps that policy below the canonical pager and makes every later
//! [`VfsFile::reserve`] a bounded check rather than another allocation.

use crate::VfsFile;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
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
    initialized: AtomicBool,
}

/// A logical RedDB file backed by a physically preallocated fixed-size file.
///
/// The provisioner must guarantee that all `capacity` bytes have backing
/// storage. A merely sparse host file does not satisfy this durability
/// contract. The ESP-IDF firmware uses FatFS `f_expand(..., alloc_now=true)`.
pub struct FixedCapacityFile {
    shared: Arc<Shared>,
    position: u64,
}

impl FixedCapacityFile {
    pub fn open(file: File, capacity: u64, state: FixedFileState) -> io::Result<Self> {
        if capacity == 0 || file.metadata()?.len() != capacity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fixed-capacity file must already have its exact physical length",
            ));
        }
        Ok(Self {
            shared: Arc::new(Shared {
                file: Mutex::new(file),
                capacity,
                initialized: AtomicBool::new(state == FixedFileState::Existing),
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
            .ok_or_else(storage_full)
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
        Ok(if self.shared.initialized.load(Ordering::Acquire) {
            self.shared.capacity
        } else {
            0
        })
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        if len > self.shared.capacity {
            return Err(storage_full());
        }
        if len == 0 && self.shared.initialized.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "fixed-capacity database cannot be truncated",
            ));
        }
        if len != 0 {
            self.shared.initialized.store(true, Ordering::Release);
        }
        Ok(())
    }

    fn reserve(&self, offset: u64, len: u64) -> io::Result<()> {
        offset
            .checked_add(len)
            .filter(|end| *end <= self.shared.capacity)
            .map(|_| ())
            .ok_or_else(storage_full)
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let end = self.checked_end(bytes.len() as u64)?;
        self.shared.initialized.store(true, Ordering::Release);
        let mut file = self.lock()?;
        file.seek(SeekFrom::Start(self.position))?;
        file.write_all(bytes)?;
        drop(file);
        self.position = end;
        Ok(())
    }

    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let remaining = self.shared.capacity.saturating_sub(self.position);
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

fn storage_full() -> io::Error {
    io::Error::new(io::ErrorKind::WriteZero, "fixed-capacity database is full")
}
