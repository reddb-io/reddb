//! Exclusive-owner batches prepared in the original double-write zone.
use super::*;

impl<F: VfsFile, W: WalBarrier> Pager<F, W> {
    pub(super) fn batch_active(&self) -> Result<bool, PagerError> {
        Ok(self
            .staged_pages
            .lock()
            .map_err(|_| PagerError::LockPoisoned)?
            .is_some())
    }

    pub(super) fn bounded_writeback(&self) -> Result<bool, PagerError> {
        Ok(self.batch_active()? || self.file_lock()?.max_read_buffer_bytes() < 2 * PAGE_SIZE)
    }

    /// The host must exclude readers and other mutations through completion.
    /// Prepare at most one native DWB frame; neither old homes nor allocator
    /// roots are changed while images are staged. This is not SQL isolation.
    pub fn begin_write_batch(&self) -> Result<(), PagerError> {
        let _ownership = self.lock_ownership_mutations();
        self.ensure_writeback_available()?;
        if !self.dwb_enabled || self.batch_active()? {
            return Err(PagerError::InvalidDatabase(
                "write batch requires double-write protection and no active batch".into(),
            ));
        }
        self.sync()?;
        *self
            .staged_pages
            .lock()
            .map_err(|_| PagerError::LockPoisoned)? = Some(pager_io::StagedPageBatch::default());
        Ok(())
    }

    /// The complete original RDDW frame crosses its barrier before any home
    /// write. Existing recovery finishes all images after an interrupted commit.
    /// An error is ambiguous and fences this owner until reopen.
    pub fn commit_write_batch(&self) -> Result<(), PagerError> {
        let _ownership = self.lock_ownership_mutations();
        self.ensure_writeback_available()?;
        if !self.batch_active()? {
            return Err(PagerError::InvalidDatabase(
                "no prepared write batch".into(),
            ));
        }
        let result = (|| {
            self.flush()?;
            let mut file = self.file_lock()?;
            let batch = self
                .staged_pages
                .lock()
                .map_err(|_| PagerError::LockPoisoned)?
                .take()
                .expect("active batch under exclusive mutation fence");
            batch.publish(&mut *file)?;
            drop(file);
            if *self.header_dirty_lock()? {
                self.write_header_and_sync()?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.writeback_failed
                .store(true, std::sync::atomic::Ordering::Release);
        }
        result
    }

    /// Discard only the live owner. Recovery discards incomplete staging or
    /// finishes a sealed frame; provisional cache contents are never exposed.
    pub fn fence_write_batch(&self) {
        self.writeback_failed
            .store(true, std::sync::atomic::Ordering::Release);
    }
}
