use super::{MemoryFile, Operation};
use reddb_engine::storage::{
    page::{Page, PageType, PAGE_SIZE},
    pager::{NoWal, Pager, PagerConfig, PagerError, WalBarrier},
    pager_io,
};
use std::{
    cell::{Cell, RefCell},
    io,
    rc::Rc,
};

#[derive(Clone, Copy, Debug)]
enum Admission {
    Read,
    ReadNoChecksum,
    Write,
    WriteNoChecksum,
    Allocate,
    Reserve,
}

const ADMISSIONS: [Admission; 6] = [
    Admission::Read,
    Admission::ReadNoChecksum,
    Admission::Write,
    Admission::WriteNoChecksum,
    Admission::Allocate,
    Admission::Reserve,
];

fn config() -> PagerConfig {
    PagerConfig {
        cache_size: 1,
        ..PagerConfig::default()
    }
}

fn page(id: u32, value: &[u8], lsn: u64) -> Page {
    let mut page = Page::new(PageType::BTreeLeaf, id);
    page.insert_cell(b"key", value).expect("fixture cell fits");
    page.set_lsn(lsn);
    page.update_checksum();
    page
}

fn seed<W: WalBarrier>(barrier: W, lsn: u64) -> (Pager<MemoryFile, W>, Vec<u32>) {
    let pager = Pager::open_owned(
        MemoryFile::default(),
        "eviction.rdb".into(),
        config(),
        barrier,
        true,
    )
    .expect("create pager");
    let mut ids = Vec::new();
    // More than one cache generation leaves published pages available for
    // read/write admissions as well as new allocations. The last generation
    // fills every shard, so no test depends on the SIEVE victim choice.
    for _ in 0..pager.cache_capacity() * 2 {
        let id = pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate source")
            .page_id();
        pager
            .write_page(id, page(id, b"before", 0))
            .expect("seed source");
        ids.push(id);
    }
    pager.sync().expect("publish baseline");
    let capacity = pager.cache_capacity();
    for &id in &ids[ids.len() - capacity..] {
        pager
            .write_page(id, page(id, b"after", lsn))
            .expect("dirty resident source");
    }
    assert_eq!(pager.dirty_page_count(), capacity);
    (pager, ids)
}

fn admit<W: WalBarrier>(
    pager: &Pager<MemoryFile, W>,
    id: u32,
    admission: Admission,
) -> Result<(), PagerError> {
    match admission {
        Admission::Read => pager.read_page(id).map(|_| ()),
        Admission::ReadNoChecksum => pager.read_page_no_checksum(id).map(|_| ()),
        Admission::Write => pager.write_page(id, page(id, b"new", 0)),
        Admission::WriteNoChecksum => pager.write_page_no_checksum(id, page(id, b"new", 0)),
        Admission::Allocate => pager.allocate_page(PageType::BTreeLeaf).map(|_| ()),
        Admission::Reserve => pager.reserve_contig_extent(1).map(|_| ()),
    }
}

pub fn pager_dirty_eviction_recovers_a_torn_home_write() {
    for admission in ADMISSIONS {
        let (pager, ids) = seed(NoWal, 0);
        let first = u64::from(ids[0]) * PAGE_SIZE as u64;
        let end = u64::from(ids[ids.len() - 1] + 1) * PAGE_SIZE as u64;
        pager
            .with_file(|file| file.fail_write_range = Some(first..end))
            .expect("arm home failure");
        let failure = admit(&pager, ids[0], admission).expect_err("interrupt eviction");
        let interrupted = pager
            .with_file(|file| file.failed_write_offset)
            .expect("inspect failure");
        assert!(
            interrupted.is_some(),
            "{admission:?}: expected home failure, got {failure:?}"
        );
        let (victim, pending, durable, operation_count) = pager
            .with_file(|file| {
                (
                    file.failed_write_offset.expect("home write interrupted") / PAGE_SIZE as u64,
                    file.bytes.borrow().clone(),
                    file.durable.borrow().clone(),
                    file.operations.borrow().len(),
                )
            })
            .expect("capture crash state");
        // A new write must not erase the only intact DWB image or publish a
        // new root after the evicted dirty image has left the cache.
        assert!(pager.flush().is_err());
        assert!(pager.write_page(ids[0], page(ids[0], b"later", 0)).is_err());
        assert!(pager.allocate_page(PageType::BTreeLeaf).is_err());
        assert!(pager.write_header_and_sync().is_err());
        pager
            .with_file(|file| {
                assert_eq!(file.operations.borrow().len(), operation_count);
                assert_eq!(*file.bytes.borrow(), pending);
            })
            .expect("no subsequent publication");
        drop(pager);
        // Model both survival and loss of the pending partial home write.
        for crashed in [pending, durable] {
            let reopened = Pager::open_owned(
                MemoryFile::from_bytes(crashed),
                "eviction.rdb".into(),
                config(),
                NoWal,
                false,
            )
            .expect("recover interrupted dirty eviction");
            let victim = u32::try_from(victim).expect("fixture page ID fits");
            let actual = reopened
                .read_page(victim)
                .expect("recover checksum-valid published page");
            assert_eq!(
                actual.read_cell(0).expect("victim cell").1,
                b"after",
                "{admission:?}"
            );
            reopened.sync().expect("reopened pager accepts publication");
        }
    }
}

pub fn pager_dirty_eviction_obeys_wal_barrier() {
    struct Barrier {
        operations: Rc<RefCell<Vec<Operation>>>,
        fail: Cell<bool>,
        seen: Cell<u64>,
    }
    impl WalBarrier for Barrier {
        fn flush_until(&self, lsn: u64) -> io::Result<()> {
            assert!(
                self.operations.borrow().is_empty(),
                "WAL before DWB and home writes"
            );
            self.seen.set(lsn);
            if self.fail.get() {
                Err(io::Error::other("injected WAL failure"))
            } else {
                Ok(())
            }
        }
    }
    for admission in ADMISSIONS {
        for fail in [false, true] {
            let operations = Rc::new(RefCell::new(Vec::new()));
            let barrier = Barrier {
                operations: Rc::clone(&operations),
                fail: Cell::new(fail),
                seen: Cell::new(0),
            };
            let (pager, ids) = seed(barrier, 42);
            pager
                .with_file(|file| file.operations = Rc::clone(&operations))
                .expect("observe admission I/O");
            assert_eq!(
                admit(&pager, ids[0], admission).is_err(),
                fail,
                "{admission:?}"
            );
            assert_eq!(pager.wal_barrier().seen.get(), 42, "{admission:?}");
            if fail {
                assert!(
                    operations.borrow().is_empty(),
                    "failed WAL prevents data-file writes"
                );
                assert!(pager.sync().is_err(), "failed eviction requires recovery");
                assert!(operations.borrow().is_empty());
            } else {
                let operations = operations.borrow();
                assert!(
                    matches!(operations[0], Operation::Write(offset, _) if offset == pager_io::dwb_zone_offset())
                );
                assert_eq!(operations[1], Operation::Sync);
                assert!(
                    matches!(operations[2], Operation::Write(offset, PAGE_SIZE) if offset >= u64::from(ids[0]) * PAGE_SIZE as u64)
                );
                let barriers = operations
                    .iter()
                    .filter(|operation| **operation == Operation::Sync)
                    .count();
                assert_eq!(
                    barriers, 3,
                    "one bounded eviction frame retains every barrier"
                );
                let homes = operations.iter().filter(|operation| matches!(operation,
                    Operation::Write(offset, PAGE_SIZE) if *offset >= u64::from(ids[0]) * PAGE_SIZE as u64)).count();
                assert!(
                    homes > 1 && homes <= pager_io::DWB_MAX_PAGES_PER_FRAME,
                    "amortize barriers over a bounded batch, got {homes} images"
                );
                assert_eq!(operations.last(), Some(&Operation::Sync));
            }
        }
    }
}

pub fn pager_sparse_dirty_pages_share_a_protected_batch() {
    let pager = Pager::open_owned(
        MemoryFile::default(),
        "sparse-dirty.rdb".into(),
        PagerConfig {
            cache_size: 1024,
            ..PagerConfig::default()
        },
        NoWal,
        true,
    )
    .expect("create sparse dirty fixture");
    let mut ids = Vec::new();
    for _ in 0..pager.cache_capacity() {
        ids.push(
            pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate baseline")
                .page_id(),
        );
    }
    pager.sync().expect("publish clean cache");
    let dirty: Vec<_> = ids.iter().copied().step_by(67).collect();
    for &id in &dirty {
        pager
            .write_page(id, page(id, b"updated", 0))
            .expect("update sparse page");
    }
    assert_eq!(pager.dirty_page_count(), dirty.len());
    pager
        .with_file(|file| file.operations.borrow_mut().clear())
        .expect("observe sparse flush");
    pager.flush().expect("publish sparse dirty pages");
    pager.with_file(|file| {
        let operations = file.operations.borrow();
        let barriers = operations.iter().filter(|operation| **operation == Operation::Sync).count();
        assert_eq!(barriers, 3, "one frame needs stage, home and clear barriers; clean cache pages must not split it");
        let homes: Vec<_> = operations.iter().filter_map(|operation| match operation {
            Operation::Write(offset, PAGE_SIZE) if *offset >= u64::from(ids[0]) * PAGE_SIZE as u64 => Some(*offset),
            _ => None,
        }).collect();
        assert_eq!(homes.len(), dirty.len(), "flush only dirty images");
        assert!(homes.windows(2).all(|pair| pair[0] < pair[1]), "publish homes in ascending physical order");
    }).expect("verify bounded ordered I/O");
    assert_eq!(pager.dirty_page_count(), 0);
    for id in dirty {
        let actual = pager
            .with_file(|file| pager_io::read_page(file, id, true))
            .expect("inspect disk")
            .expect("valid home page");
        assert_eq!(actual.read_cell(0).expect("updated cell").1, b"updated");
    }
}

pub fn pager_eviction_batch_stays_within_one_frame() {
    let pager = Pager::open_owned(
        MemoryFile::default(),
        "eviction-frame.rdb".into(),
        PagerConfig {
            cache_size: 128,
            ..PagerConfig::default()
        },
        NoWal,
        true,
    )
    .expect("create bounded eviction fixture");
    let mut ids = Vec::new();
    for _ in 0..pager.cache_capacity() {
        ids.push(
            pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate baseline")
                .page_id(),
        );
    }
    pager.sync().expect("publish baseline");
    for &id in &ids {
        pager
            .write_page(id, page(id, b"updated", 0))
            .expect("dirty resident page");
    }
    pager
        .with_file(|file| file.operations.borrow_mut().clear())
        .expect("observe eviction batch");
    pager
        .allocate_page(PageType::BTreeLeaf)
        .expect("protected bounded eviction");
    pager
        .with_file(|file| {
            let operations = file.operations.borrow();
            assert_eq!(
                operations
                    .iter()
                    .filter(|operation| **operation == Operation::Sync)
                    .count(),
                3
            );
            let homes = operations
                .iter()
                .filter(|operation| {
                    matches!(operation,
            Operation::Write(offset, PAGE_SIZE) if *offset >= u64::from(ids[0]) * PAGE_SIZE as u64)
                })
                .count();
            assert_eq!(
                homes,
                pager_io::DWB_MAX_PAGES_PER_FRAME,
                "one victim plus a bounded resident batch"
            );
        })
        .expect("verify bounded frame");
    assert_eq!(
        pager.dirty_page_count(),
        ids.len() - pager_io::DWB_MAX_PAGES_PER_FRAME + 1
    );
}
