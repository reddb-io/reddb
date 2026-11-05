use reddb_engine::storage::freelist::{FreeList, FREE_IDS_PER_TRUNK};
use reddb_engine::storage::page::{Page, PageError, PageType, HEADER_SIZE, PAGE_SIZE};
use reddb_engine::storage::page_cache::{PageCache, PageCacheShard};

pub fn page_layout_and_corruption() {
    assert_eq!(PAGE_SIZE, 16 * 1024);
    let mut page = Page::new(PageType::BTreeLeaf, 73);
    page.insert_cell(b"key", b"value").expect("cell fits");
    page.set_lsn(0x001f_ffff_ffff_ffff);
    page.update_checksum();

    // The file authority must interpret the engine's bytes identically on
    // native and WASM targets; no browser-specific page representation.
    let header_bytes = page.as_bytes()[..HEADER_SIZE]
        .try_into()
        .expect("page has a complete header");
    let header = reddb_file::decode_paged_page_header(header_bytes);
    assert_eq!(header.page_type, PageType::BTreeLeaf as u8);
    assert_eq!(header.page_id, 73);
    assert_eq!(header.cell_count, 1);
    assert_eq!(header.lsn, 0x001f_ffff_ffff_ffff);

    let restored = Page::from_slice(page.as_bytes()).expect("full page restores");
    restored.verify_checksum().expect("checksum survives copy");
    assert_eq!(
        restored.read_cell(0).expect("cell exists"),
        (b"key".to_vec(), b"value".to_vec())
    );
    assert!(matches!(
        Page::from_slice(&page.as_bytes()[1..]),
        Err(PageError::InvalidSize(_))
    ));

    // Header and payload corruption must both fail checksum verification.
    for offset in [8, PAGE_SIZE - 1] {
        let mut corrupt = restored.clone();
        corrupt.as_bytes_mut()[offset] ^= 1;
        assert!(matches!(
            corrupt.verify_checksum(),
            Err(PageError::ChecksumMismatch { .. })
        ));
    }
}

pub fn freelist_trunk_recovery() {
    let count = u32::try_from(FREE_IDS_PER_TRUNK + 5).expect("fixture fits u32");
    let mut original = FreeList::new();
    original.free_batch(&(100..100 + count).collect::<Vec<_>>());
    let trunks = original.flush_to_trunks(0, || panic!("must reuse free pages"));
    assert_eq!(trunks.len(), 2);
    let mut recovered = FreeList::from_header(original.trunk_head(), 0);
    recovered
        .merge_all_trunks(|id| {
            trunks
                .iter()
                .find(|trunk| trunk.page_id() == id)
                .map(|trunk| {
                    trunk.verify_checksum().expect("trunk checksum is valid");
                    Page::from_slice(trunk.as_bytes()).expect("trunk page restores")
                })
        })
        .expect("trunk chain restores");
    let mut ids = Vec::new();
    while let Some(id) = recovered.allocate() {
        ids.push(id);
    }
    ids.sort_unstable();
    assert_eq!(ids, (100..100 + count).collect::<Vec<_>>());
    assert!(recovered.is_empty());
    assert_eq!(recovered.total_free(), 0);

    let wrong_type = Page::new(PageType::BTreeLeaf, 99);
    assert!(recovered.load_from_trunk(&wrong_type).is_err());
}

pub fn cache_eviction_and_writeback() {
    let shard = PageCacheShard::new(4);
    for id in 1..=4 {
        shard.insert(id, Page::new(PageType::BTreeLeaf, id));
    }
    assert!(shard.pin(1));
    shard.mark_dirty(2);
    let mut written = Vec::new();
    for id in 5..=12 {
        if let Some(victim) = shard.insert(id, Page::new(PageType::BTreeLeaf, id)) {
            written.push(victim.page_id());
        }
        assert!(shard.contains(1), "pinned page survives eviction");
    }
    written.extend(shard.flush_dirty().iter().map(|(id, _)| *id));
    assert_eq!(written, vec![2], "dirty page is handed back exactly once");
    assert!(shard.unpin(1));

    let cache = PageCache::new(16);
    for id in 1..=8 {
        cache.insert(id, Page::new(PageType::BTreeLeaf, id));
        cache.mark_dirty(id);
    }
    assert!(cache.flush_some_dirty(0).is_empty());
    let first = cache.flush_some_dirty(3);
    assert_eq!(first.len(), 3);
    assert_eq!(cache.dirty_count(), 5);
    let mut ids: Vec<_> = first.into_iter().map(|(id, _)| id).collect();
    ids.extend(cache.flush_dirty().into_iter().map(|(id, _)| id));
    ids.sort_unstable();
    assert_eq!(ids, (1..=8).collect::<Vec<_>>());
    assert_eq!(cache.dirty_count(), 0);
}

pub const CASES: [(&str, fn()); 3] = [
    ("page_layout_and_corruption", page_layout_and_corruption),
    ("freelist_trunk_recovery", freelist_trunk_recovery),
    ("cache_eviction_and_writeback", cache_eviction_and_writeback),
];
