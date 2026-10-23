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
    fn prefetch_hint(&self, _page_id: u32) {}
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
}
