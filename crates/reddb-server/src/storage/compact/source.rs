use super::*;
use reddb_engine::storage::{page_store::PageStore, pager::PagerError};
use std::io::{Read, Seek, SeekFrom};

/// The source handle owns a shared read lock, which excludes the writer's
/// exclusive lock while preserving an OS-enforced read-only descriptor.
pub(super) struct Source {
    file: parking_lot::Mutex<File>,
    pub header: f::DatabaseHeader,
    pub bytes: u64,
    pub pending_double_write: bool,
    // One byte per addressable page, capped at 64 MiB. No page payload cache.
    ownership: parking_lot::Mutex<Vec<u8>>,
}
impl Source {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_inner(path, false)
    }

    /// Do not follow allocator or catalog pages until the copied image has
    /// passed the pager's double-write and checkpoint recovery.
    pub fn open_for_wal_recovery(path: &Path) -> Result<Self> {
        Self::open_inner(path, true)
    }

    fn open_inner(path: &Path, allow_unified_wal: bool) -> Result<Self> {
        let canonical_path = path.canonicalize()?;
        let path = canonical_path.as_path();
        for unsupported in [
            f::primary_replica_root(path),
            f::layout::serverless_root(path),
        ] {
            ensure(
                !unsupported.exists(),
                "tiered/replicated/serverless storage requires a separate compaction qualification",
            )?;
        }
        ensure(
            f::list_physical_metadata_journal_paths(path)?.is_empty(),
            "historical metadata journals require separate compaction qualification",
        )?;
        let support = f::support_dir_for(path);
        if support.exists() {
            if allow_unified_wal {
                validate_recovery_support(&support, 0, false)?;
            } else {
                validate_empty_directory(&support, 0)?;
            }
        }
        let mut file = File::open(path)?;
        // Some network filesystems (including OCI FSS/NFS) reject an exclusive
        // lock on an O_RDONLY descriptor with EBADF. A shared lock is the exact
        // contract needed here: any number of immutable readers are harmless,
        // while the server's lifetime exclusive lock remains excluded.
        fs2::FileExt::try_lock_shared(&file)?;
        let bytes = file.metadata()?.len();
        let mut zero = [0; PAGE_SIZE];
        file.read_exact(&mut zero)?;
        ensure(
            !f::paged_encryption_marker_present(&zero),
            "encrypted pager is not supported by offline compaction",
        )?;
        let selected = f::select_paged_superblock(&zero).ok_or("no valid native superblock")?;
        let start = selected.copy_index * f::PAGED_SUPERBLOCK_SLOT_SIZE;
        let header =
            f::decode_database_header(&zero[start..start + f::PAGED_SUPERBLOCK_SLOT_SIZE])?;
        ensure(
            allow_unified_wal || !header.checkpoint_in_progress,
            "source needs checkpoint recovery",
        )?;
        ensure(
            header.page_count >= FIRST_ALLOCATABLE_PAGE && header.page_count <= 64 * 1024 * 1024,
            "source page inventory exceeds compaction budget",
        )?;
        ensure(
            if allow_unified_wal {
                bytes.div_ceil(PAGE_SIZE as u64) >= u64::from(header.page_count)
                    && bytes.div_ceil(PAGE_SIZE as u64) <= 64 * 1024 * 1024
            } else {
                bytes == u64::from(header.page_count) * PAGE_SIZE as u64
            },
            "source allocation boundary differs from file length",
        )?;
        // Ownership intents and undo images live in this WAL; none may be pending.
        for (wal, pending_supported) in [
            (f::unified_wal_path(path), allow_unified_wal),
            (f::engine_wal_path(path), allow_unified_wal),
            (f::pager_legacy_wal_path(path), false),
        ] {
            match std::fs::symlink_metadata(&wal) {
                Ok(stat) => {
                    ensure(stat.is_file(), "WAL must be a regular file")?;
                    ensure(
                        stat.len() == f::WAL_FILE_HEADER_BYTES as u64 || pending_supported,
                        "source WAL is not checkpointed; close/checkpoint a preserved copy first",
                    )?;
                    let mut data = [0; f::WAL_FILE_HEADER_BYTES];
                    File::open(wal)?.read_exact(&mut data)?;
                    f::decode_wal_file_header(&data)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        ensure(
            f::layout::retired::first_present_phase1_sidecar(path).is_none()
                && f::layout::retired::first_present_phase3_dwb_sidecar(path).is_none(),
            "legacy sidecar layout requires migration",
        )?;
        file.seek(SeekFrom::Start(
            u64::from(DWB_ZONE_START_PAGE) * PAGE_SIZE as u64,
        ))?;
        let mut double_write = vec![0; DWB_ZONE_PAGES as usize * PAGE_SIZE];
        file.read_exact(&mut double_write)?;
        let pending_double_write =
            !reddb_engine::storage::pager_io::dwb_zone_is_clear(&double_write);
        if pending_double_write {
            ensure(
                allow_unified_wal,
                "source has pending double-write recovery",
            )?;
            let entries = f::decode_paged_dwb_frame(&double_write)?;
            ensure(!entries.is_empty(), "empty double-write frame")?;
            let mut seen = std::collections::HashSet::new();
            for entry in entries {
                ensure(
                    entry.page_id > 0
                        && (entry.page_id < DWB_ZONE_START_PAGE
                            || entry.page_id >= FIRST_ALLOCATABLE_PAGE)
                        && u64::from(entry.page_id) < bytes.div_ceil(PAGE_SIZE as u64)
                        && seen.insert(entry.page_id),
                    "invalid double-write page ownership",
                )?;
                let page = Page::from_bytes(entry.page);
                ensure(
                    page.page_id() == entry.page_id,
                    "double-write page identity mismatch",
                )?;
                page.page_type()?;
                if entry.page_id != 2 {
                    page.verify_checksum()?;
                }
            }
        }
        let mut ownership = vec![0; header.page_count as usize];
        ownership[..FIRST_ALLOCATABLE_PAGE as usize].fill(1);
        let source = Self {
            file: parking_lot::Mutex::new(file),
            bytes,
            pending_double_write,
            header,
            ownership: parking_lot::Mutex::new(ownership),
        };
        if !allow_unified_wal {
            source.validate_freelist()?;
        }
        Ok(source)
    }
    pub fn raw_page(&self, id: u32, checksum: bool) -> Result<Page> {
        ensure(
            id < self.header.page_count,
            "page outside source allocation boundary",
        )?;
        let mut bytes = [0; PAGE_SIZE];
        let mut file = self.file.lock();
        file.seek(SeekFrom::Start(u64::from(id) * PAGE_SIZE as u64))?;
        file.read_exact(&mut bytes)?;
        let page = Page::from_bytes(bytes);
        if checksum {
            page.verify_checksum()?;
        }
        ensure(
            page.page_id() == id || (id == 2 && bytes.iter().all(|b| *b == 0)),
            "page identity mismatch",
        )?;
        Ok(page)
    }
    pub fn copy_snapshot(&self, destination: &Path) -> Result<()> {
        let mut source = self.file.lock();
        source.seek(SeekFrom::Start(0))?;
        let mut output = create_new(destination)?;
        ensure(
            std::io::copy(&mut *source, &mut output)? == self.bytes,
            "source size changed while copying",
        )?;
        output.sync_all()?;
        drop(output);
        source.seek(SeekFrom::Start(0))?;
        let original_hash = hash_reader(&mut *source)?;
        let copied_hash = hash_reader(&mut File::open(destination)?)?;
        ensure(
            original_hash == copied_hash,
            "database copy failed complete readback parity",
        )
    }
    pub fn verify_superblocks(&self) -> Result<()> {
        let mut zone = [0; f::PAGED_SUPERBLOCK_ZONE_SIZE];
        let mut file = self.file.lock();
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut zone)?;
        for (copy, slot) in zone.chunks_exact(f::PAGED_SUPERBLOCK_SLOT_SIZE).enumerate() {
            ensure(
                f::paged_superblock_slot_generation(slot, copy).is_some(),
                "superblock damage requires separate recovery",
            )?;
            f::decode_database_header(slot)?;
        }
        Ok(())
    }
    pub fn unclaimed_pages(&self) -> u64 {
        self.ownership.lock()[FIRST_ALLOCATABLE_PAGE as usize..]
            .iter()
            .filter(|owner| **owner == 0)
            .count() as u64
    }
    pub fn verify_unclaimed_vector_pages(&self) -> Result<()> {
        for id in FIRST_ALLOCATABLE_PAGE..self.header.page_count {
            if self.ownership.lock()[id as usize] == 0 {
                ensure(
                    self.raw_page(id, true)?.page_type()? == PageType::Vector,
                    "unclassified recovery page is not a vector cache page",
                )?;
            }
        }
        Ok(())
    }
    pub fn claim(&self, id: u32) -> Result<()> {
        ensure(
            id >= FIRST_ALLOCATABLE_PAGE && id < self.header.page_count,
            "invalid owned page",
        )?;
        let mut ownership = self.ownership.lock();
        ensure(
            ownership[id as usize] == 0,
            "live page overlaps allocator or another owner",
        )?;
        ownership[id as usize] = 4;
        Ok(())
    }
    fn validate_freelist(&self) -> Result<()> {
        let mut trunk = self.header.freelist_head;
        while trunk != 0 {
            self.mark_free(trunk)?;
            let page = self.raw_page(trunk, true)?;
            ensure(
                page.page_type()? == PageType::FreelistTrunk,
                "invalid free-list trunk type",
            )?;
            let data = page.content();
            let count = number(data, 4)? as usize;
            ensure(
                count <= (data.len() - 8) / 4,
                "invalid free-list entry count",
            )?;
            for index in 0..count {
                self.mark_free(number(data, 8 + index * 4)?)?;
            }
            trunk = number(data, 0)?;
        }
        Ok(())
    }
    fn mark_free(&self, id: u32) -> Result<()> {
        ensure(
            id >= FIRST_ALLOCATABLE_PAGE && id < self.header.page_count,
            "invalid free page",
        )?;
        let mut ownership = self.ownership.lock();
        ensure(
            ownership[id as usize] == 0,
            "duplicate free page or free-list cycle",
        )?;
        ownership[id as usize] = 2;
        Ok(())
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(self.file.get_mut());
    }
}
impl PageStore for Source {
    fn read_page(&self, id: u32) -> std::result::Result<Page, PagerError> {
        self.raw_page(id, true).map_err(|error| {
            match error.downcast::<reddb_engine::storage::page::PageError>() {
                Ok(error) => PagerError::Page(*error),
                Err(error) => PagerError::InvalidDatabase(error.to_string()),
            }
        })
    }
    fn page_count(&self) -> std::result::Result<u32, PagerError> {
        Ok(self.header.page_count)
    }
    fn write_page(&self, _: u32, _: Page) -> std::result::Result<(), PagerError> {
        Err(PagerError::ReadOnly)
    }
    fn allocate_page(&self, _: PageType) -> std::result::Result<Page, PagerError> {
        Err(PagerError::ReadOnly)
    }
    fn free_page(&self, _: u32) -> std::result::Result<(), PagerError> {
        Err(PagerError::ReadOnly)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_locked_source_excludes_writer_until_drop() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("source.rdb");
        {
            let pager = Pager::open_default(&path).expect("create source");
            pager.sync().expect("checkpoint source");
        }

        let source = Source::open(&path).expect("read-lock source");
        let writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("writer descriptor");
        assert!(
            fs2::FileExt::try_lock_exclusive(&writer).is_err(),
            "shared compaction lock must exclude a writer"
        );

        drop(source);
        fs2::FileExt::try_lock_exclusive(&writer).expect("writer after compactor drop");
        fs2::FileExt::unlock(&writer).expect("unlock writer");
    }
}
