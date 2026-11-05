//! Full shared-pager fixture, executed natively and over real OPFS.
use reddb_engine::storage::{
    page::{Page, PageType},
    pager::{NoWal, Pager, PagerConfig, PagerError},
    pager_io,
};
use reddb_file::VfsFile;

pub const PAGE_ID: u32 = pager_io::FIRST_ALLOCATABLE_PAGE;

fn open<F: VfsFile>(file: F, create: bool) -> Result<Pager<F, NoWal>, PagerError> {
    Pager::open_owned(
        file,
        "pager-orchestration.rdb".into(),
        PagerConfig {
            cache_size: 2,
            ..PagerConfig::default()
        },
        NoWal,
        create,
    )
}

fn page(value: &[u8]) -> Page {
    let mut page = Page::new(PageType::BTreeLeaf, PAGE_ID);
    page.insert_cell(b"key", value).expect("fixture cell fits");
    page
}

pub fn create<F: VfsFile>(file: F) -> Result<(), PagerError> {
    let pager = open(file, true)?;
    assert_eq!(pager.allocate_page(PageType::BTreeLeaf)?.page_id(), PAGE_ID);
    pager.write_page(PAGE_ID, page(b"before"))?;
    pager.sync()?;
    pager.set_checkpoint_in_progress(true, 42)?;
    Ok(())
}

pub fn update<F: VfsFile>(file: F) -> Result<(), PagerError> {
    let pager = open(file, false)?;
    assert_eq!(pager.read_page(PAGE_ID)?.read_cell(0)?.1, b"before");
    assert!(pager.header()?.checkpoint_in_progress);
    pager.write_page(PAGE_ID, page(b"after"))?;
    // The test interrupts this call after the DWB barrier, in the home write.
    pager.sync()
}

pub fn recover<F: VfsFile>(file: F) -> Result<(), PagerError> {
    let pager = open(file, false)?;
    assert_eq!(pager.read_page(PAGE_ID)?.read_cell(0)?.1, b"after");
    assert_eq!(pager.page_count()?, PAGE_ID + 1);
    assert!(pager.header()?.checkpoint_in_progress);
    assert_eq!(pager.header()?.checkpoint_target_lsn, 42);
    pager.complete_checkpoint(42)?;
    pager.sync()?;
    Ok(())
}

pub fn verify<F: VfsFile>(file: F) -> Result<(), PagerError> {
    let pager = open(file, false)?;
    assert_eq!(pager.read_page(PAGE_ID)?.read_cell(0)?.1, b"after");
    assert_eq!(pager.header()?.checkpoint_lsn, 42);
    assert!(!pager.header()?.checkpoint_in_progress);
    assert_eq!(pager.header()?.checkpoint_target_lsn, 0);
    Ok(())
}
