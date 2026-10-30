#[path = "support/codec_corpus.rs"]
mod codec_corpus;

#[path = "support/portable.rs"]
mod portable;

#[path = "support/catalog.rs"]
mod catalog;

#[path = "support/enforcement.rs"]
mod enforcement;

#[path = "support/enforcement_restart.rs"]
mod enforcement_restart;

#[path = "support/record_filters.rs"]
mod record_filters;

#[path = "support/record_filters_restart.rs"]
mod record_filters_restart;

#[path = "support/record_ordering.rs"]
mod record_ordering;

#[path = "support/record_projection.rs"]
mod record_projection;

#[path = "support/numeric_scalars.rs"]
mod numeric_scalars;

#[test]
fn portable_numeric_scalar_conformance() {
    for (name, run) in numeric_scalars::CASES {
        eprintln!("{name}");
        run();
    }
}

#[test]
fn portable_record_projection_conformance() {
    for (name, run) in record_projection::CASES {
        eprintln!("{name}");
        run();
    }
}

#[test]
fn portable_record_ordering_conformance() {
    for (name, run) in record_ordering::CASES {
        eprintln!("{name}");
        run();
    }
}

#[test]
fn portable_record_filter_conformance() {
    for (name, run) in record_filters::CASES {
        eprintln!("{name}");
        run();
    }
}

#[test]
fn portable_contract_enforcement_conformance() {
    for (name, run) in enforcement::CASES {
        eprintln!("{name}");
        run();
    }
}

#[test]
fn portable_catalog_conformance() {
    for (name, run) in catalog::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/storage.rs"]
mod storage;

#[path = "support/pager.rs"]
mod pager;

#[test]
fn paged_layout_version_is_independent_of_current_writer_version() {
    use reddb_engine::storage::pager::{NoWal, Pager, PagerConfig, PagerError};
    use reddb_file::{PAGED_SUPERBLOCK_SLOT_COUNT, PAGED_SUPERBLOCK_SLOT_SIZE};

    let open = |file, create| {
        Pager::open_owned(
            file,
            "version-test.rdb".into(),
            PagerConfig::default(),
            NoWal,
            create,
        )
    };
    let initial = open(pager::MemoryFile::default(), true).expect("create fixture");
    let bytes = initial
        .with_file(|file| file.durable.borrow().clone())
        .expect("copy durable fixture");
    drop(initial);

    let with_version = |version| {
        let mut image = bytes.clone();
        for copy in 0..PAGED_SUPERBLOCK_SLOT_COUNT {
            let offset = copy * PAGED_SUPERBLOCK_SLOT_SIZE;
            let slot = &mut image[offset..offset + PAGED_SUPERBLOCK_SLOT_SIZE];
            let generation = reddb_file::paged_superblock_slot_generation(slot, copy)
                .expect("fixture slot is valid");
            reddb_file::set_database_header_version(slot, version).expect("set fixture version");
            reddb_file::seal_paged_superblock_slot(slot, copy, generation)
                .expect("seal fixture slot");
        }
        pager::MemoryFile::from_bytes(image)
    };
    let supported = open(with_version(0x0001_0001), false)
        .expect("the original phase-3 DWB layout must stay readable");
    assert_eq!(
        supported.header().expect("read header").version,
        0x0001_0001
    );
    assert!(matches!(
        open(with_version(0x0001_0000), false),
        Err(PagerError::LegacyPagedStore { .. })
    ));
    assert!(matches!(
        open(with_version(reddb_file::PAGE_FILE_VERSION + 1), false),
        Err(PagerError::InvalidDatabase(_))
    ));
}

#[test]
fn unjournaled_trunk_allocation_preserves_unconsumed_free_pages_after_crash() {
    use reddb_engine::storage::{
        page::PageType,
        pager::{NoWal, Pager, PagerConfig},
    };
    let open = |file, create| {
        Pager::open_owned(
            file,
            "free-reuse.rdb".into(),
            PagerConfig::default(),
            NoWal,
            create,
        )
        .expect("open allocator fixture")
    };
    let initial = open(pager::MemoryFile::default(), true);
    let mut ids = std::collections::HashSet::new();
    for _ in 0..32 {
        ids.insert(
            initial
                .allocate_page(PageType::BTreeLeaf)
                .expect("seed page")
                .page_id(),
        );
    }
    initial.sync().expect("publish allocations");
    initial
        .free_retired_pages(&ids.iter().copied().collect::<Vec<_>>())
        .expect("free seed pages");
    initial.sync().expect("publish free trunk");
    let baseline_count = initial.page_count().expect("page count");
    let image = initial
        .with_file(|file| file.durable.borrow().clone())
        .expect("durable seed");
    let allocation = open(pager::MemoryFile::from_bytes(image), false);
    let reserved = allocation
        .allocate_page(PageType::BTreeLeaf)
        .expect("consume one free page")
        .page_id();
    assert!(ids.remove(&reserved));
    // Capture only the durable bytes, before Drop or a checkpoint can flush the
    // remaining in-memory free IDs. This models the native non-epoch WAL barrier.
    let crash = allocation
        .with_file(|file| file.durable.borrow().clone())
        .expect("crash image");
    let recovered = open(pager::MemoryFile::from_bytes(crash), false);
    for _ in 0..ids.len() {
        let id = recovered
            .allocate_page(PageType::BTreeLeaf)
            .expect("reuse untouched page")
            .page_id();
        assert_ne!(id, reserved, "an already reserved page became free again");
        assert!(
            ids.remove(&id),
            "allocator lost the detached trunk remainder: {id}"
        );
    }
    assert!(
        ids.is_empty(),
        "all untouched free pages must be reused exactly once"
    );
    assert_eq!(recovered.page_count().expect("page count"), baseline_count);
}

#[test]
fn unjournaled_trunk_rewrite_io_failures_preserve_remaining_ownership() {
    use reddb_engine::storage::{
        page::PageType,
        pager::{NoWal, Pager, PagerConfig},
    };
    let open = |file, create| {
        Pager::open_owned(
            file,
            "trunk-fault.rdb".into(),
            PagerConfig::default(),
            NoWal,
            create,
        )
        .expect("open allocator fixture")
    };
    let seed = open(pager::MemoryFile::default(), true);
    let ids: Vec<_> = (0..8)
        .map(|_| {
            seed.allocate_page(PageType::BTreeLeaf)
                .expect("seed page")
                .page_id()
        })
        .collect();
    seed.sync().expect("seed allocations");
    seed.free_retired_pages(&ids).expect("retire pages");
    seed.sync().expect("seed trunk");
    let baseline = seed.page_count().expect("seed count");
    let image = seed
        .with_file(|file| file.durable.borrow().clone())
        .expect("seed image");
    for fail_at in 1..=12 {
        let attempt = open(pager::MemoryFile::from_bytes(image.clone()), false);
        attempt
            .with_file(|file| {
                file.operations.borrow_mut().clear();
                file.fail_at = Some(fail_at);
            })
            .expect("inject I/O failure");
        let allocated = attempt
            .allocate_page(PageType::BTreeLeaf)
            .ok()
            .map(|page| page.page_id());
        let crash = attempt
            .with_file(|file| file.durable.borrow().clone())
            .expect("crash image");
        let recovered = open(pager::MemoryFile::from_bytes(crash), false);
        let mut reused = std::collections::HashSet::new();
        loop {
            let id = recovered
                .allocate_page(PageType::BTreeLeaf)
                .expect("recovery allocation")
                .page_id();
            if id >= baseline {
                break;
            }
            assert!(
                ids.contains(&id),
                "unexpected free page at boundary {fail_at}"
            );
            assert_ne!(
                Some(id),
                allocated,
                "published reservation aliased at boundary {fail_at}"
            );
            assert!(reused.insert(id), "duplicate reuse at boundary {fail_at}");
        }
        // An uncertain allocation may have reserved its one ID. Every other
        // free ID must survive either side of the protected trunk rewrite.
        assert!(
            reused.len() >= ids.len() - 1,
            "lost remainder at boundary {fail_at}"
        );
    }
}

#[test]
fn incremental_test_file_matches_full_image_durability() {
    use reddb_file::VfsFile;
    use std::io::SeekFrom;
    let mut full = pager::MemoryFile::from_bytes(vec![19; 8192]);
    let mut incremental = pager::MemoryFile::from_bytes(vec![19; 8192]);
    incremental.incremental_sync = true;
    for cycle in 0..32_u64 {
        for file in [&mut full, &mut incremental] {
            file.seek(SeekFrom::Start(cycle * 173)).expect("seek");
            file.write_all(&[u8::try_from(cycle).expect("test cycle fits in a byte"); 97])
                .expect("overlapping write");
            if cycle % 4 == 0 {
                file.set_len(1024).expect("truncate");
                file.set_len(8192).expect("extend with zeros");
            }
            let before = file.durable.borrow().clone();
            file.fail_at = Some(file.operations.borrow().len() + 1);
            assert!(file.sync_all().is_err());
            assert_eq!(
                *file.durable.borrow(),
                before,
                "failed sync must not publish"
            );
            file.fail_at = None;
            file.sync_all().expect("successful sync");
        }
        assert_eq!(*incremental.bytes.borrow(), *full.bytes.borrow());
        assert_eq!(*incremental.durable.borrow(), *full.durable.borrow());
    }
}

#[test]
fn format_upgrade_fences_both_superblocks_and_stops_after_io_failure() {
    use reddb_engine::storage::{
        page::PageType,
        pager::{NoWal, Pager, PagerConfig},
    };
    use reddb_file::{PAGED_SUPERBLOCK_SLOT_COUNT, PAGED_SUPERBLOCK_SLOT_SIZE, PAGE_FILE_VERSION};

    let create = || {
        let pager = Pager::open_owned(
            pager::MemoryFile::default(),
            "format-fence.rdb".into(),
            PagerConfig::default(),
            NoWal,
            true,
        )
        .expect("create pager");
        pager
            .commit_superblock(|slot| {
                reddb_file::set_database_header_version(slot, PAGE_FILE_VERSION - 1).map_err(
                    |error| {
                        reddb_engine::storage::pager::PagerError::InvalidDatabase(error.to_string())
                    },
                )
            })
            .expect("create one older-version slot");
        pager
    };
    let pager = create();
    let persisted_page_count = pager.header().expect("header").page_count;
    pager
        .allocate_page(PageType::BTreeLeaf)
        .expect("dirty allocation");
    pager
        .fence_file_format_version(PAGE_FILE_VERSION)
        .expect("fence format");
    let bytes = pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("durable bytes");
    for copy in 0..PAGED_SUPERBLOCK_SLOT_COUNT {
        let start = copy * PAGED_SUPERBLOCK_SLOT_SIZE;
        let slot = &bytes[start..start + PAGED_SUPERBLOCK_SLOT_SIZE];
        assert!(reddb_file::paged_superblock_slot_generation(slot, copy).is_some());
        let header = reddb_file::decode_database_header(slot).expect("header");
        assert_eq!(header.version, PAGE_FILE_VERSION);
        assert_eq!(
            header.page_count, persisted_page_count,
            "format fence must not publish dirty allocator state"
        );
        let mut damaged = bytes.clone();
        damaged[start + 64] ^= 1;
        let reopened = Pager::open_owned(
            pager::MemoryFile::from_bytes(damaged),
            "format-fallback.rdb".into(),
            PagerConfig::default(),
            NoWal,
            false,
        )
        .expect("either fenced copy can recover");
        assert_eq!(
            reopened.header().expect("fallback header").version,
            PAGE_FILE_VERSION
        );
    }

    for fail_at in 1..=4 {
        let pager = create();
        pager
            .with_file(|file| {
                file.operations.borrow_mut().clear();
                file.fail_at = Some(fail_at);
            })
            .expect("inject failure");
        assert!(
            pager.fence_file_format_version(PAGE_FILE_VERSION).is_err(),
            "boundary {fail_at}"
        );
        pager
            .with_file(|file| file.fail_at = None)
            .expect("restore I/O");
        assert!(
            pager.allocate_page(PageType::BTreeLeaf).is_err(),
            "failure must fence writes"
        );
    }
}

#[test]
fn pager_records_ownership_before_allocator_changes_and_fences_errors() {
    use reddb_engine::storage::{
        page::PageType,
        pager::{Pager, PagerConfig, WalBarrier},
    };
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Event {
        Reserve(u32, u32),
        Detach(Vec<u32>),
        Retire(Vec<u32>),
    }
    struct Barrier {
        events: Rc<RefCell<Vec<Event>>>,
        fail: Rc<Cell<bool>>,
    }
    impl WalBarrier for Barrier {
        fn has_page_ownership_journal(&self) -> std::io::Result<bool> {
            Ok(true)
        }

        fn flush_until(&self, _: u64) -> std::io::Result<()> {
            Ok(())
        }
        fn reserve_pages(&self, start: u32, count: u32) -> std::io::Result<()> {
            if self.fail.get() {
                return Err(std::io::Error::other("injected ownership failure"));
            }
            self.events.borrow_mut().push(Event::Reserve(start, count));
            Ok(())
        }
        fn detach_free_pages(&self, pages: &[u32]) -> std::io::Result<()> {
            if self.fail.get() {
                return Err(std::io::Error::other("injected ownership failure"));
            }
            self.events.borrow_mut().push(Event::Detach(pages.to_vec()));
            Ok(())
        }
        fn retire_pages(&self, pages: &[u32]) -> std::io::Result<()> {
            if self.fail.get() {
                return Err(std::io::Error::other("injected ownership failure"));
            }
            self.events.borrow_mut().push(Event::Retire(pages.to_vec()));
            Ok(())
        }
    }
    let events = Rc::new(RefCell::new(Vec::new()));
    let fail = Rc::new(Cell::new(false));
    let barrier = || Barrier {
        events: Rc::clone(&events),
        fail: Rc::clone(&fail),
    };
    let open = |file, create| {
        Pager::open_owned(
            file,
            "ownership-hooks.rdb".into(),
            PagerConfig::default(),
            barrier(),
            create,
        )
        .expect("open pager")
    };

    let pager = open(pager::MemoryFile::default(), true);
    let a = pager
        .allocate_page(PageType::BTreeLeaf)
        .expect("allocate")
        .page_id();
    let b = pager
        .allocate_page(PageType::BTreeLeaf)
        .expect("allocate")
        .page_id();
    assert_eq!(
        &*events.borrow(),
        &[Event::Reserve(a, 1), Event::Reserve(b, 1)]
    );
    pager
        .record_retirement_intent(&[a, b])
        .expect("record retirement");
    pager.free_retired_pages(&[a, b]).expect("retire pages");
    assert_eq!(
        &events.borrow()[2..],
        &[Event::Retire(vec![a, b]), Event::Retire(vec![a, b])]
    );
    pager.sync().expect("persist freelist");
    let bytes = pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("durable file");
    drop(pager);

    events.borrow_mut().clear();
    let detached_failure = open(pager::MemoryFile::from_bytes(bytes.clone()), false);
    fail.set(true);
    assert!(detached_failure.allocate_page(PageType::BTreeLeaf).is_err());
    fail.set(false);
    assert!(detached_failure.allocate_page(PageType::BTreeLeaf).is_err());
    assert_eq!(
        detached_failure
            .with_file(|file| file.durable.borrow().clone())
            .expect("durable file"),
        bytes
    );
    drop(detached_failure);
    let reopened = open(pager::MemoryFile::from_bytes(bytes), false);
    let reused = reopened
        .allocate_page(PageType::BTreeLeaf)
        .expect("reuse")
        .page_id();
    let observed = events.borrow();
    assert!(
        matches!(&observed[..], [Event::Detach(pages), Event::Reserve(id, 1)] if pages.len() == 2 && pages.contains(&a) && pages.contains(&b) && *id == reused)
    );
    drop(observed);

    let other_events = Rc::new(RefCell::new(Vec::new()));
    let other_fail = Rc::new(Cell::new(true));
    let failing = Pager::open_owned(
        pager::MemoryFile::default(),
        "ownership-fail.rdb".into(),
        PagerConfig::default(),
        Barrier {
            events: other_events,
            fail: Rc::clone(&other_fail),
        },
        true,
    )
    .expect("open pager");
    let count = failing.page_count().expect("page count");
    assert!(failing.allocate_page(PageType::BTreeLeaf).is_err());
    assert_eq!(failing.page_count().expect("page count"), count);
    other_fail.set(false);
    assert!(
        failing.allocate_page(PageType::BTreeLeaf).is_err(),
        "failed journal must fence writes"
    );
}

#[test]
fn checkpoint_before_image_failure_prevents_cache_and_disk_overwrite() {
    use reddb_engine::storage::{
        page::{Page, PageType},
        pager::{Pager, PagerConfig, WalBarrier},
    };
    use std::{cell::Cell, rc::Rc};

    struct Barrier(Rc<Cell<bool>>);
    impl WalBarrier for Barrier {
        fn flush_until(&self, _: u64) -> std::io::Result<()> {
            Ok(())
        }
        fn needs_page_image(&self, _: u32) -> std::io::Result<bool> {
            Ok(self.0.get())
        }
        fn capture_page_image(&self, page: &Page) -> std::io::Result<()> {
            assert_eq!(page.content()[0], 17, "capture the pre-overwrite image");
            page.verify_checksum().expect("valid original page");
            Err(std::io::Error::other(
                "injected before-image append failure",
            ))
        }
    }
    for unchecked in [false, true] {
        let capture = Rc::new(Cell::new(false));
        let pager = Pager::open_owned(
            pager::MemoryFile::default(),
            "before-image-failure.rdb".into(),
            PagerConfig::default(),
            Barrier(Rc::clone(&capture)),
            true,
        )
        .expect("create pager");
        let mut original = pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate page");
        let id = original.page_id();
        original.content_mut()[0] = 17;
        pager.write_page(id, original).expect("write baseline");
        pager.sync().expect("durable baseline");
        let durable = pager
            .with_file(|file| file.durable.borrow().clone())
            .expect("baseline bytes");
        let mut replacement = pager.read_page(id).expect("original");
        replacement.content_mut()[0] = 29;
        capture.set(true);
        let result = if unchecked {
            pager.write_page_no_checksum(id, replacement)
        } else {
            pager.write_page(id, replacement)
        };
        assert!(result.is_err());
        assert_eq!(
            pager.read_page(id).expect("cached original").content()[0],
            17
        );
        capture.set(false);
        assert!(
            pager.sync().is_err(),
            "uncertain WAL append fences writeback"
        );
        assert!(pager.allocate_page(PageType::BTreeLeaf).is_err());
        assert_eq!(
            pager
                .with_file(|file| file.durable.borrow().clone())
                .expect("disk bytes"),
            durable
        );
    }
}

#[test]
fn recovery_reclaims_pages_idempotently_across_superblock_fallbacks() {
    use reddb_engine::storage::{
        page::PageType,
        pager::{NoWal, Pager, PagerConfig},
    };
    use reddb_file::{PAGED_SUPERBLOCK_SLOT_COUNT, PAGED_SUPERBLOCK_SLOT_SIZE};

    let open = |file, create| {
        Pager::open_owned(
            file,
            "recovery-free.rdb".into(),
            PagerConfig::default(),
            NoWal,
            create,
        )
        .expect("open pager")
    };
    let pager = open(pager::MemoryFile::default(), true);
    let a = pager
        .allocate_page(PageType::BTreeLeaf)
        .expect("allocate")
        .page_id();
    let b = pager
        .allocate_page(PageType::BTreeLeaf)
        .expect("allocate")
        .page_id();
    pager.sync().expect("persist allocations");
    pager
        .validate_owned_pages(&[a, b])
        .expect("allocated pages are not free");
    pager.recover_unowned_pages(&[a, b]).expect("recover pages");
    assert!(
        pager.validate_owned_pages(&[a]).is_err(),
        "a free page cannot be preserved as owned"
    );
    pager
        .recover_unowned_pages(&[a, b])
        .expect("repeat recovery");
    pager.sync_recovered_allocator().expect("fence allocator");
    let bytes = pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("durable file");
    drop(pager);

    for copy in 0..PAGED_SUPERBLOCK_SLOT_COUNT {
        let mut damaged = bytes.clone();
        damaged[copy * PAGED_SUPERBLOCK_SLOT_SIZE + 64] ^= 1;
        let reopened = open(pager::MemoryFile::from_bytes(damaged), false);
        reopened
            .recover_unowned_pages(&[a, b])
            .expect("repeat after restart");
        reopened
            .sync_recovered_allocator()
            .expect("fence recovered allocator");
        let first = reopened
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate")
            .page_id();
        let second = reopened
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate")
            .page_id();
        assert_ne!(first, second, "a page cannot be handed out twice");
        assert!([a, b].contains(&first) && [a, b].contains(&second));
    }
}

#[test]
fn recovery_rejects_a_requested_page_duplicated_in_a_freelist_trunk() {
    use reddb_engine::storage::{
        page::{Page, PageType, HEADER_SIZE, PAGE_SIZE},
        pager::{NoWal, Pager, PagerConfig},
    };
    let open = |file, create| {
        Pager::open_owned(
            file,
            "recovery-duplicate.rdb".into(),
            PagerConfig::default(),
            NoWal,
            create,
        )
        .expect("open pager")
    };
    let pager = open(pager::MemoryFile::default(), true);
    let a = pager
        .allocate_page(PageType::BTreeLeaf)
        .expect("allocate")
        .page_id();
    let b = pager
        .allocate_page(PageType::BTreeLeaf)
        .expect("allocate")
        .page_id();
    pager.free_retired_pages(&[a, b]).expect("free");
    pager.sync().expect("sync");
    let head = pager.header().expect("header").freelist_head;
    let mut bytes = pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("file");
    drop(pager);
    let offset = head as usize * PAGE_SIZE;
    let mut trunk = Page::from_bytes(bytes[offset..offset + PAGE_SIZE].try_into().expect("page"));
    trunk.as_bytes_mut()[HEADER_SIZE + 4..HEADER_SIZE + 8].copy_from_slice(&2u32.to_le_bytes());
    trunk.as_bytes_mut()[HEADER_SIZE + 8..HEADER_SIZE + 12].copy_from_slice(&a.to_le_bytes());
    trunk.as_bytes_mut()[HEADER_SIZE + 12..HEADER_SIZE + 16].copy_from_slice(&a.to_le_bytes());
    trunk.update_checksum();
    bytes[offset..offset + PAGE_SIZE].copy_from_slice(trunk.as_bytes());
    let reopened = open(pager::MemoryFile::from_bytes(bytes), false);
    assert!(reopened.recover_unowned_pages(&[a]).is_err());
}

#[test]
fn interrupted_allocator_recovery_can_be_retried_after_another_interruption() {
    use reddb_engine::storage::{
        page::PageType,
        pager::{NoWal, Pager, PagerConfig},
    };

    let open = |bytes: Option<Vec<u8>>| {
        let create = bytes.is_none();
        let file = bytes.map_or_else(pager::MemoryFile::default, pager::MemoryFile::from_bytes);
        Pager::open_owned(
            file,
            "recovery-interruption.rdb".into(),
            PagerConfig::default(),
            NoWal,
            create,
        )
        .expect("open pager")
    };
    let initial = open(None);
    let ids = [
        initial
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate")
            .page_id(),
        initial
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate")
            .page_id(),
    ];
    initial.sync().expect("persist allocations");
    let baseline = initial
        .with_file(|file| file.durable.borrow().clone())
        .expect("baseline");
    drop(initial);

    let probe = open(Some(baseline.clone()));
    probe.recover_unowned_pages(&ids).expect("recover pages");
    probe
        .with_file(|file| file.operations.borrow_mut().clear())
        .expect("reset operations");
    probe
        .sync_recovered_allocator()
        .expect("count recovery boundaries");
    let boundaries = probe
        .with_file(|file| file.operations.borrow().len())
        .expect("operations");
    drop(probe);
    assert!(boundaries > 4);

    for fail_at in 1..=boundaries {
        for retain_pending in [false, true] {
            let first = open(Some(baseline.clone()));
            first.recover_unowned_pages(&ids).expect("recover pages");
            first
                .with_file(|file| {
                    file.operations.borrow_mut().clear();
                    file.fail_at = Some(fail_at);
                })
                .expect("inject first interruption");
            assert!(
                first.sync_recovered_allocator().is_err(),
                "boundary {fail_at}"
            );
            let image = first
                .with_file(|file| {
                    if retain_pending {
                        file.bytes.borrow().clone()
                    } else {
                        file.durable.borrow().clone()
                    }
                })
                .expect("interrupted image");
            drop(first);

            let second = open(Some(image));
            second.recover_unowned_pages(&ids).expect("retry recovery");
            second
                .with_file(|file| {
                    file.operations.borrow_mut().clear();
                    file.fail_at = Some(1);
                })
                .expect("inject recovery interruption");
            assert!(
                second.sync_recovered_allocator().is_err(),
                "retry boundary {fail_at}"
            );
            let image = second
                .with_file(|file| file.durable.borrow().clone())
                .expect("second image");
            drop(second);

            let recovered = open(Some(image));
            recovered
                .recover_unowned_pages(&ids)
                .expect("recover after repeated interruption");
            recovered
                .sync_recovered_allocator()
                .expect("persist recovered allocator");
            let first = recovered
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate")
                .page_id();
            let second = recovered
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate")
                .page_id();
            assert_ne!(first, second, "boundary {fail_at} allocated a page twice");
            assert!(
                ids.contains(&first) && ids.contains(&second),
                "boundary {fail_at} lost free pages"
            );
        }
    }
}

#[test]
fn retired_batch_rejects_duplicate_pages_without_modifying_allocator() {
    use reddb_engine::storage::{
        page::PageType,
        pager::{NoWal, Pager, PagerConfig},
    };

    let pager = Pager::open_owned(
        pager::MemoryFile::default(),
        "duplicate-retirement.rdb".into(),
        PagerConfig::default(),
        NoWal,
        true,
    )
    .expect("create pager");
    let retired = pager
        .allocate_page(PageType::BTreeLeaf)
        .expect("allocate retired page")
        .page_id();
    let before = pager.free_page_count().expect("read free count");
    assert!(pager.free_retired_pages(&[retired, retired]).is_err());
    assert_eq!(pager.free_page_count().expect("read free count"), before);
    assert_ne!(
        pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate live page")
            .page_id(),
        retired
    );
    pager
        .free_retired_pages(&[retired])
        .expect("release one page");
    assert_eq!(
        pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("reuse retired page")
            .page_id(),
        retired
    );
}

#[test]
fn retired_batch_rejects_pages_already_free_before_and_after_reopen() {
    use reddb_engine::storage::{
        page::PageType,
        pager::{NoWal, Pager, PagerConfig},
    };

    let open = |file, create| {
        Pager::open_owned(
            file,
            "repeat-retirement.rdb".into(),
            PagerConfig::default(),
            NoWal,
            create,
        )
        .expect("open pager")
    };
    let pager = open(pager::MemoryFile::default(), true);
    let ids: Vec<_> = (0..3)
        .map(|_| {
            pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("pager operation")
                .page_id()
        })
        .collect();
    pager
        .free_retired_pages(&ids[..2])
        .expect("first retirement");
    let before = pager.free_page_count().expect("pager operation");
    assert!(pager.free_retired_pages(&[ids[2], ids[0]]).is_err());
    assert_eq!(pager.free_page_count().expect("pager operation"), before);
    pager.sync().expect("persist free pages");
    let bytes = pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("pager operation");
    drop(pager);

    let reopened = open(pager::MemoryFile::from_bytes(bytes), false);
    for &already_free in &ids[..2] {
        let before = reopened.free_page_count().expect("pager operation");
        assert!(reopened
            .free_retired_pages(&[ids[2], already_free])
            .is_err());
        assert_eq!(reopened.free_page_count().expect("pager operation"), before);
    }
    let a = reopened
        .allocate_page(PageType::BTreeLeaf)
        .expect("pager operation")
        .page_id();
    let b = reopened
        .allocate_page(PageType::BTreeLeaf)
        .expect("pager operation")
        .page_id();
    assert_ne!(a, b, "a page may not be handed to two owners");
    assert!(ids[..2].contains(&a) && ids[..2].contains(&b));
    assert_ne!(
        reopened
            .allocate_page(PageType::BTreeLeaf)
            .expect("pager operation")
            .page_id(),
        ids[2]
    );
}

#[test]
fn retired_batch_rejects_corrupt_freelist_without_modifying_allocator() {
    use reddb_engine::storage::{
        page::{Page, PageType, HEADER_SIZE, PAGE_SIZE},
        pager::{NoWal, Pager, PagerConfig},
    };

    let open = |file, create| {
        Pager::open_owned(
            file,
            "cyclic-freelist.rdb".into(),
            PagerConfig::default(),
            NoWal,
            create,
        )
        .expect("open pager")
    };
    let pager = open(pager::MemoryFile::default(), true);
    let free = pager
        .allocate_page(PageType::BTreeLeaf)
        .expect("allocate free page")
        .page_id();
    let live = pager
        .allocate_page(PageType::BTreeLeaf)
        .expect("allocate live page")
        .page_id();
    pager.free_retired_pages(&[free]).expect("retire page");
    pager.sync().expect("persist freelist");
    let head = pager.header().expect("header").freelist_head;
    let bytes = pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("file");
    drop(pager);

    let offset = head as usize * PAGE_SIZE;
    for cyclic in [true, false] {
        let mut bytes = bytes.clone();
        let mut trunk =
            Page::from_bytes(bytes[offset..offset + PAGE_SIZE].try_into().expect("page"));
        let retired = if cyclic {
            trunk.as_bytes_mut()[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(&head.to_le_bytes());
            live
        } else {
            let mut header = trunk.header().expect("trunk header");
            header.page_id = live;
            trunk.set_header(&header);
            head
        };
        trunk.update_checksum();
        bytes[offset..offset + PAGE_SIZE].copy_from_slice(trunk.as_bytes());
        let reopened = open(pager::MemoryFile::from_bytes(bytes), false);
        let before = reopened.free_page_count().expect("free count");
        assert!(reopened.free_retired_pages(&[retired]).is_err());
        assert_eq!(reopened.free_page_count().expect("free count"), before);
        assert!(
            reopened.allocate_page(PageType::BTreeLeaf).is_err(),
            "allocation must reject a self-linked or misdirected trunk"
        );
    }
}

#[test]
fn portable_execution_conformance() {
    for (name, run) in portable::CASES {
        eprintln!("{name}");
        run();
    }
}

#[test]
fn portable_storage_conformance() {
    for (name, run) in storage::CASES {
        eprintln!("{name}");
        run();
    }
}

#[test]
fn portable_pager_io_conformance() {
    for (name, run) in pager::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/transaction.rs"]
mod transaction;

#[test]
fn portable_transaction_conformance() {
    for (name, run) in transaction::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/entity.rs"]
mod entity;

#[test]
fn portable_entity_read_conformance() {
    for (name, run) in entity::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/entity_codec.rs"]
mod entity_codec;

#[test]
fn portable_entity_codec_conformance() {
    for (name, run) in entity_codec::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/record_corpus.rs"]
mod record_corpus;
#[path = "support/records.rs"]
mod records;

#[test]
fn portable_record_materialization_conformance() {
    for (name, run) in records::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/segments.rs"]
mod segments;

#[test]
fn portable_resident_segment_conformance() {
    for (name, run) in segments::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/metadata_codec.rs"]
mod metadata_codec;
#[path = "support/metadata_corpus.rs"]
mod metadata_corpus;

#[test]
fn portable_metadata_codec_conformance() {
    for (name, run) in metadata_codec::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/snapshot.rs"]
mod snapshot;

#[test]
fn portable_snapshot_conformance() {
    for (name, run) in snapshot::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/manager.rs"]
mod manager;

#[test]
fn portable_collection_lifecycle_conformance() {
    for (name, run) in manager::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/read_indexes.rs"]
mod read_indexes;

#[test]
fn portable_resident_read_index_conformance() {
    for (name, run) in read_indexes::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/resident_store.rs"]
mod resident_store;

#[test]
fn portable_resident_store_conformance() {
    for (name, run) in resident_store::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/store_wal.rs"]
mod store_wal;

#[test]
fn portable_store_wal_replay_conformance() {
    for (name, run) in store_wal::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/resident_file.rs"]
mod resident_file;
#[path = "support/resident_file_restart.rs"]
mod resident_file_restart;

#[test]
fn portable_resident_file_conformance() {
    for (name, run) in resident_file::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/text_scalars.rs"]
mod text_scalars;

#[test]
fn portable_text_scalar_conformance() {
    for (name, run) in text_scalars::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/temporal_scalars.rs"]
mod temporal_scalars;

#[test]
fn portable_temporal_scalar_conformance() {
    for (name, run) in temporal_scalars::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/geographic.rs"]
mod geographic;

#[test]
fn portable_geographic_conformance() {
    for (name, run) in geographic::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/table_plan.rs"]
mod table_plan;

#[test]
fn portable_table_plan_conformance() {
    for (name, run) in table_plan::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/table_planning.rs"]
mod table_planning;

#[test]
fn portable_table_planning_conformance() {
    for (name, run) in table_planning::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/runtime_expressions.rs"]
mod runtime_expressions;

#[test]
fn portable_runtime_expression_conformance() {
    for (name, run) in runtime_expressions::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/value_functions.rs"]
mod value_functions;

#[test]
fn portable_value_function_conformance() {
    for (name, run) in value_functions::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/window_phase.rs"]
mod window_phase;

#[test]
fn portable_window_phase_conformance() {
    for (name, run) in window_phase::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/aggregation.rs"]
mod aggregation;
#[test]
fn portable_aggregation_conformance() {
    for (name, run) in aggregation::CASES {
        eprintln!("{name}");
        run();
    }
}

/// Recovery must not lose free-list trunks or hand a live page out as free.
/// Exercise more than two trunks, two superblock fallback choices and repeated reuse.
#[test]
fn multi_trunk_recovery_reuses_every_page_without_growing() {
    use reddb_engine::storage::{
        freelist::FREE_IDS_PER_TRUNK,
        page::PageType,
        pager::{NoWal, Pager, PagerConfig},
    };
    use reddb_file::{PAGED_SUPERBLOCK_SLOT_COUNT, PAGED_SUPERBLOCK_SLOT_SIZE};
    use std::collections::BTreeSet;

    let open = |bytes: Option<Vec<u8>>| {
        let create = bytes.is_none();
        let mut file = bytes.map_or_else(pager::MemoryFile::default, pager::MemoryFile::from_bytes);
        file.incremental_sync = true;
        Pager::open_owned(
            file,
            "multi-trunk-reuse.rdb".into(),
            PagerConfig {
                // This fixture targets allocator trunks. Tiny-cache eviction is
                // covered by the native crash matrix and portable pager tests.
                cache_size: 2 * FREE_IDS_PER_TRUNK + 16,
                ..Default::default()
            },
            NoWal,
            create,
        )
        .expect("valid multi-trunk recovery fixture")
    };
    let initial = open(None);
    let mut sentinel = initial
        .allocate_page(PageType::BTreeLeaf)
        .expect("valid multi-trunk recovery fixture");
    let live = sentinel.page_id();
    sentinel
        .insert_cell(b"sentinel", b"must survive all reuse")
        .expect("valid multi-trunk recovery fixture");
    initial
        .write_page(live, sentinel)
        .expect("valid multi-trunk recovery fixture");
    let types = [
        PageType::BTreeLeaf,
        PageType::BTreeInterior,
        PageType::Overflow,
    ];
    let ids: BTreeSet<_> = (0..2 * FREE_IDS_PER_TRUNK + 7)
        .map(|index| {
            initial
                .allocate_page(types[index % types.len()])
                .expect("valid multi-trunk recovery fixture")
                .page_id()
        })
        .collect();
    initial.sync().expect("valid multi-trunk recovery fixture");
    let page_count = initial
        .page_count()
        .expect("valid multi-trunk recovery fixture");
    let baseline = initial
        .with_file(|file| file.durable.borrow().clone())
        .expect("valid multi-trunk recovery fixture");
    drop(initial);
    for damaged_copy in 0..PAGED_SUPERBLOCK_SLOT_COUNT {
        let mut image = baseline.clone();
        for cycle in 0..3 {
            let store = open(Some(image));
            let recovered: Vec<_> = ids.iter().copied().collect();
            store
                .recover_unowned_pages(&recovered)
                .expect("valid multi-trunk recovery fixture");
            store
                .recover_unowned_pages(&recovered)
                .expect("valid multi-trunk recovery fixture");
            store
                .sync_recovered_allocator()
                .expect("valid multi-trunk recovery fixture");
            // The free-page counter excludes the trunk pages themselves.
            // The allocation-ID oracle below must recover those pages too.
            image = store
                .with_file(|file| file.durable.borrow().clone())
                .expect("valid multi-trunk recovery fixture");
            drop(store);
            image[damaged_copy * PAGED_SUPERBLOCK_SLOT_SIZE + 64] ^= 1;
            let store = open(Some(image));
            // Journal replay after losing either superblock must be idempotent.
            store
                .recover_unowned_pages(&recovered)
                .expect("valid multi-trunk recovery fixture");
            store
                .sync_recovered_allocator()
                .expect("valid multi-trunk recovery fixture");
            let mut allocated = BTreeSet::new();
            let mut written = BTreeSet::new();
            for index in 0..ids.len() {
                let mut page = store
                    .allocate_page(types[index % types.len()])
                    .expect("valid multi-trunk recovery fixture");
                let id = page.page_id();
                assert_ne!(id, live, "live sentinel was recycled");
                assert!(ids.contains(&id), "lost reusable page, appended {id}");
                assert!(allocated.insert(id), "page allocated twice: {id}");
                // Touch both sides of trunk boundaries without fsyncing thousands
                // of full in-memory file images in this portable fixture.
                if index % FREE_IDS_PER_TRUNK <= 1 {
                    page.insert_cell(b"generation", &(cycle as u64).to_le_bytes())
                        .expect("valid multi-trunk recovery fixture");
                    store
                        .write_page(id, page)
                        .expect("valid multi-trunk recovery fixture");
                    written.insert(id);
                }
            }
            assert_eq!(allocated, ids);
            assert_eq!(
                store
                    .page_count()
                    .expect("valid multi-trunk recovery fixture"),
                page_count
            );
            assert_eq!(
                store
                    .free_page_count()
                    .expect("valid multi-trunk recovery fixture"),
                0
            );
            store.sync().expect("valid multi-trunk recovery fixture");
            image = store
                .with_file(|file| file.durable.borrow().clone())
                .expect("valid multi-trunk recovery fixture");
            assert_eq!(
                image.len(),
                baseline.len(),
                "file grew in reuse cycle {cycle}"
            );
            drop(store);
            let store = open(Some(image));
            assert_eq!(
                store
                    .read_page(live)
                    .expect("valid multi-trunk recovery fixture")
                    .read_cell(0)
                    .expect("valid multi-trunk recovery fixture"),
                (b"sentinel".to_vec(), b"must survive all reuse".to_vec())
            );
            for id in &written {
                assert_eq!(
                    store
                        .read_page(*id)
                        .expect("valid multi-trunk recovery fixture")
                        .read_cell(0)
                        .expect("valid multi-trunk recovery fixture"),
                    (
                        b"generation".to_vec(),
                        (cycle as u64).to_le_bytes().to_vec()
                    )
                );
            }
            image = store
                .with_file(|file| file.durable.borrow().clone())
                .expect("valid multi-trunk recovery fixture");
        }
    }
}
