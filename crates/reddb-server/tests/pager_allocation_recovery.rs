//! A page may reach disk through cache eviction before the allocation header.
//! Reopen must reserve that physical address, including after a torn tail.
use reddb_server::storage::engine::{Page, PageType, Pager, PagerConfig, PAGE_SIZE};
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};

#[test]
fn reopen_never_allocates_over_a_page_written_beyond_the_persisted_counter() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("data.rdb");
    let stale_count;
    {
        let pager = Pager::open_default(&path).expect("new pager");
        stale_count = pager.page_count().expect("page count");
        pager.sync().expect("persist initial allocation header");
    }
    // Crash image: writeback has persisted a referenced page, while the last
    // valid superblock still contains the preceding allocation counter.
    let mut retained = Page::new(PageType::Overflow, stale_count + 3);
    retained.content_mut()[..8].copy_from_slice(b"retained");
    retained.update_checksum();
    {
        let mut file = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open fixture");
        file.seek(SeekFrom::Start(
            u64::from(retained.page_id()) * PAGE_SIZE as u64,
        ))
        .expect("seek");
        file.write_all(retained.as_bytes())
            .expect("write crash image");
        file.sync_all().expect("sync crash image");
    }
    let original = std::fs::read(&path).expect("read fixture");
    {
        let read_only = Pager::open(
            &path,
            PagerConfig {
                read_only: true,
                create: false,
                ..Default::default()
            },
        )
        .expect("read-only reopen");
        assert!(read_only.page_count().expect("recovered counter") > retained.page_id());
    }
    assert_eq!(
        std::fs::read(&path).expect("read again"),
        original,
        "read-only recovery must not write"
    );
    {
        let reopened = Pager::open_default(&path).expect("writable reopen");
        for _ in 0..5 {
            let allocated = reopened
                .allocate_page(PageType::NativeMeta)
                .expect("allocate");
            assert!(
                allocated.page_id() > retained.page_id(),
                "allocation must not reuse crash-persisted addresses"
            );
        }
        reopened.sync().expect("sync allocations");
        assert_eq!(
            reopened
                .read_page(retained.page_id())
                .expect("retained page")
                .as_bytes(),
            retained.as_bytes()
        );
    }
    let reopened = Pager::open_default(&path).expect("second reopen");
    assert!(reopened.page_count().expect("durable counter") > retained.page_id());
    assert_eq!(
        reopened
            .read_page(retained.page_id())
            .expect("retained page")
            .as_bytes(),
        retained.as_bytes()
    );
}

#[test]
fn reopen_quarantines_a_partial_tail_instead_of_allocating_over_it() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("data.rdb");
    let stale_count;
    {
        let pager = Pager::open_default(&path).expect("new pager");
        stale_count = pager.page_count().expect("page count");
        pager.sync().expect("sync");
    }
    let mut file = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open tail");
    file.write_all(b"torn").expect("partial tail");
    file.sync_all().expect("sync tail");
    drop(file);
    let reopened = Pager::open_default(&path).expect("reopen with partial tail");
    let allocated = reopened
        .allocate_page(PageType::BTreeLeaf)
        .expect("allocate");
    assert!(
        allocated.page_id() > stale_count,
        "incomplete tail must remain quarantined"
    );
}
