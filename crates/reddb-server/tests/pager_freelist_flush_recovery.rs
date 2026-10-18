use reddb_server::storage::engine::{PageType, Pager, PagerConfig};
#[test]
fn freelist_publication_preserves_an_evicted_dirty_page() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.rdb");
    let mut keep = Vec::new();
    {
        let pager = Pager::open(
            &path,
            PagerConfig {
                cache_size: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let retired = pager.allocate_page(PageType::Overflow).unwrap().page_id();
        // cache_size is rounded up to a minimum per shard. Fill the actual
        // capacity with consecutive IDs so every shard is full.
        for _ in 0..pager.cache_capacity() {
            let mut original = pager.allocate_page(PageType::Overflow).unwrap();
            let id = original.page_id();
            original.content_mut()[..8].copy_from_slice(b"original");
            pager.write_page(id, original).unwrap();
            keep.push(id);
        }
        pager.sync().unwrap();
        pager.free_page(retired).unwrap();
        for &id in &keep {
            let mut page = pager.read_page(id).unwrap();
            page.content_mut()[..8].copy_from_slice(b"retained");
            pager.write_page(id, page).unwrap();
        }
        assert_eq!(pager.dirty_page_count(), pager.cache_capacity());
        // Publishing a freelist trunk must evict a dirty page from a full shard.
        pager.sync().unwrap();
    }
    let pager = Pager::open_default(&path).unwrap();
    for id in keep {
        assert_eq!(
            &pager.read_page(id).unwrap().content()[..8],
            b"retained",
            "page {id}"
        );
    }
}
