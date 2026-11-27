//! Page operations used by the shared B-tree and overflow algorithms.
use super::page::{Page, PageType};
use super::pager::{Pager, PagerError, WalBarrier};
use reddb_file::VfsFile;

pub trait PageStore {
    fn read_page(&self, page_id: u32) -> Result<Page, PagerError>;
    fn write_page(&self, page_id: u32, page: Page) -> Result<(), PagerError>;
    fn allocate_page(&self, page_type: PageType) -> Result<Page, PagerError>;
    fn free_page(&self, page_id: u32) -> Result<(), PagerError>;
    fn page_count(&self) -> Result<u32, PagerError>;
    /// A generation proof retains old paths until its new root is published.
    fn enable_deferred_reclamation(&self) -> Result<(), PagerError> {
        Err(PagerError::InvalidDatabase(
            "page generation proofs require deferred reclamation".into(),
        ))
    }
    fn prefetch_hint(&self, _page_id: u32) {}
    /// [`Self::read_page`] that never waits: `None` only while a dirty image
    /// another thread's ownership epoch evicted is on its way to the file.
    /// A store without such an epoch always answers.
    fn try_read_page(&self, page_id: u32) -> Result<Option<Page>, PagerError> {
        self.read_page(page_id).map(Some)
    }
    /// Between two [`Self::try_read_page`] calls that declined. The caller
    /// holds nothing the store's writers need.
    fn wait_to_retry_read(&self) {}
    /// Enters the store's ownership epoch, if it has one. Callers that hold
    /// their own lock across store calls take this first, so they never wait
    /// for the epoch while holding what its owner needs.
    fn ownership_guard(&self) -> Option<parking_lot::ReentrantMutexGuard<'_, ()>> {
        None
    }
}

impl<F: VfsFile, W: WalBarrier> PageStore for Pager<F, W> {
    fn read_page(&self, id: u32) -> Result<Page, PagerError> {
        self.read_page(id)
    }
    fn write_page(&self, id: u32, page: Page) -> Result<(), PagerError> {
        self.write_page(id, page)
    }
    fn allocate_page(&self, kind: PageType) -> Result<Page, PagerError> {
        self.allocate_page(kind)
    }
    fn free_page(&self, id: u32) -> Result<(), PagerError> {
        self.free_page(id)
    }
    fn page_count(&self) -> Result<u32, PagerError> {
        self.page_count()
    }
    fn try_read_page(&self, id: u32) -> Result<Option<Page>, PagerError> {
        self.try_read_page(id)
    }
    fn wait_to_retry_read(&self) {
        self.wait_to_retry_read()
    }
    fn ownership_guard(&self) -> Option<parking_lot::ReentrantMutexGuard<'_, ()>> {
        Some(self.lock_ownership_mutations())
    }

    fn enable_deferred_reclamation(&self) -> Result<(), PagerError> {
        self.enable_deferred_reclamation()
    }
}
