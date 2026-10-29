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
