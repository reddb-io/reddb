use reddb_engine::storage::{page::Page, page::PageType, pager_io};
use reddb_file::{VfsFile, PAGED_SUPERBLOCK_SLOT_SIZE};

pub const PAGE_ID: u32 = pager_io::FIRST_ALLOCATABLE_PAGE;

pub fn fixture_page(value: &[u8]) -> Page {
    let mut page = Page::new(PageType::BTreeLeaf, PAGE_ID);
    page.insert_cell(b"key", value).expect("fixture cell fits");
    page.update_checksum();
    page
}

pub fn create<F: VfsFile>(file: &mut F) -> Result<(), pager_io::PagerIoError> {
    assert_eq!(file.file_len()?, 0, "fixture only creates an empty file");
    let header = Page::new_header_page(PAGE_ID + 1);
    pager_io::write_page(file, 0, &header)?;
    for (id, kind) in [(1, PageType::Header), (2, PageType::Vault)] {
        let mut page = Page::new(kind, id);
        page.update_checksum();
        pager_io::write_page(file, id, &page)?;
    }
    pager_io::clear_dwb_zone(file)?;
    pager_io::write_pages(file, &[(PAGE_ID, fixture_page(b"before"))], true)?;
    let mut slot = [0; PAGED_SUPERBLOCK_SLOT_SIZE];
    slot.copy_from_slice(&header.as_bytes()[..PAGED_SUPERBLOCK_SLOT_SIZE]);
    pager_io::write_superblock_slot(file, &mut slot, 0, 1)?;
    pager_io::write_superblock_slot(file, &mut slot, 1, 2)?;
    Ok(())
}

pub fn verify<F: VfsFile>(file: &mut F, expected: &[u8]) -> Result<(), pager_io::PagerIoError> {
    let zone = pager_io::read_superblock_zone(file)?;
    let selected = reddb_file::select_paged_superblock(&zone).expect("valid superblock");
    assert_eq!(selected.generation, 2, "page I/O never publishes roots");
    let page = pager_io::read_page(file, PAGE_ID, true)?;
    assert_eq!(page.read_cell(0).expect("fixture cell exists").1, expected);
    Ok(())
}

pub fn update<F: VfsFile>(file: &mut F) -> Result<(), pager_io::PagerIoError> {
    pager_io::write_pages(file, &[(PAGE_ID, fixture_page(b"after"))], true)
}

pub fn recover<F: VfsFile>(file: &mut F) -> Result<(), pager_io::PagerIoError> {
    pager_io::recover_from_dwb(file)?;
    verify(file, b"after")?;
    pager_io::recover_from_dwb(file)?;
    verify(file, b"after")
}
