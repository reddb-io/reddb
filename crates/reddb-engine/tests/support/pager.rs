use std::cell::{Cell, RefCell};
use std::io::{self, SeekFrom};
use std::rc::Rc;

use reddb_engine::storage::{page::Page, page::PageType, page::PAGE_SIZE, pager_io};
use reddb_file::VfsFile;

#[path = "pager_restart.rs"]
mod restart;

#[path = "pager_orchestration_restart.rs"]
mod orchestration_restart;

#[path = "btree.rs"]
mod btree;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Write(u64, usize),
    Sync,
}

#[derive(Default)]
pub(crate) struct MemoryFile {
    pub(crate) bytes: RefCell<Vec<u8>>,
    pub(crate) durable: RefCell<Vec<u8>>,
    position: u64,
    pub(crate) operations: Rc<RefCell<Vec<Operation>>>,
    pub(crate) fail_at: Option<usize>,
    pub(crate) fail_prefix: Option<usize>,
    pub(crate) fail_len: Cell<bool>,
}

impl MemoryFile {
    pub(crate) fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            durable: RefCell::new(bytes.clone()),
            bytes: RefCell::new(bytes),
            ..Self::default()
        }
    }

    fn operation(&self, operation: Operation) -> io::Result<()> {
        let mut operations = self.operations.borrow_mut();
        operations.push(operation);
        if self.fail_at == Some(operations.len()) {
            return Err(io::Error::other("injected page I/O failure"));
        }
        Ok(())
    }

    fn write_prefix(&self, buffer: &[u8]) {
        let start = usize::try_from(self.position).expect("fixture offset fits usize");
        let mut bytes = self.bytes.borrow_mut();
        let length = bytes.len().max(start + buffer.len());
        bytes.resize(length, 0);
        bytes[start..start + buffer.len()].copy_from_slice(buffer);
    }
}

impl VfsFile for MemoryFile {
    fn try_clone(&self) -> io::Result<Self> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn file_len(&self) -> io::Result<u64> {
        if self.fail_len.get() {
            return Err(io::Error::other("injected metadata failure"));
        }
        Ok(self.bytes.borrow().len() as u64)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.bytes.borrow_mut().resize(
            usize::try_from(len).map_err(|_| io::ErrorKind::InvalidInput)?,
            0,
        );
        Ok(())
    }

    fn write_all(&mut self, buffer: &[u8]) -> io::Result<()> {
        if let Err(error) = self.operation(Operation::Write(self.position, buffer.len())) {
            self.write_prefix(&buffer[..self.fail_prefix.unwrap_or(buffer.len() / 2)]);
            return Err(error);
        }
        self.write_prefix(buffer);
        self.position += buffer.len() as u64;
        Ok(())
    }

    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let start = usize::try_from(self.position).map_err(|_| io::ErrorKind::InvalidInput)?;
        let bytes = self.bytes.borrow();
        let count = bytes.len().saturating_sub(start).min(buffer.len()).min(31);
        if count > 0 {
            buffer[..count].copy_from_slice(&bytes[start..start + count]);
        }
        self.position += count as u64;
        Ok(count)
    }

    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.position = match position {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            SeekFrom::End(offset) => self.file_len()?.checked_add_signed(offset),
        }
        .ok_or(io::ErrorKind::InvalidInput)?;
        Ok(self.position)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.operation(Operation::Sync)?;
        *self.durable.borrow_mut() = self.bytes.borrow().clone();
        Ok(())
    }
}

pub fn pager_barriers_and_restart() {
    let mut file = MemoryFile::default();
    restart::create(&mut file).expect("create paged fixture");
    restart::verify(&mut file, b"before").expect("initial page is readable");
    file.operations.borrow_mut().clear();
    restart::update(&mut file).expect("protected page write");
    let operations = file.operations.borrow();
    assert_eq!(
        &operations[..5],
        &[
            Operation::Write(
                pager_io::dwb_zone_offset(),
                reddb_file::PAGED_DWB_HEADER_SIZE + reddb_file::PAGED_DWB_ENTRY_SIZE
            ),
            Operation::Sync,
            Operation::Write(u64::from(restart::PAGE_ID) * PAGE_SIZE as u64, PAGE_SIZE),
            Operation::Sync,
            Operation::Write(pager_io::dwb_zone_offset(), PAGE_SIZE),
        ]
    );
    assert_eq!(operations.last(), Some(&Operation::Sync));
    assert_eq!(operations.len(), 5 + pager_io::DWB_ZONE_PAGES as usize);
    drop(operations);
    let mut reopened = MemoryFile::from_bytes(file.durable.into_inner());
    restart::recover(&mut reopened).expect("restart and repeated recovery preserve pages");
}

pub fn pager_failure_boundaries() {
    let mut initial = MemoryFile::default();
    restart::create(&mut initial).expect("create paged fixture");
    let baseline = initial.bytes.into_inner();
    let mut successful = MemoryFile::from_bytes(baseline.clone());
    restart::update(&mut successful).expect("count protected write boundaries");
    let count = successful.operations.borrow().len();
    for fail_at in 1..=count {
        for retain_pending in [false, true] {
            let mut interrupted = MemoryFile::from_bytes(baseline.clone());
            interrupted.fail_at = Some(fail_at);
            assert!(
                restart::update(&mut interrupted).is_err(),
                "boundary {fail_at}"
            );
            assert_eq!(
                interrupted.operations.borrow().len(),
                fail_at,
                "stop at first I/O failure"
            );
            let bytes = if retain_pending {
                interrupted.bytes.into_inner()
            } else {
                interrupted.durable.into_inner()
            };
            let mut reopened = MemoryFile::from_bytes(bytes);
            pager_io::recover_from_dwb(&mut reopened).expect("recover interrupted page write");
            let page = pager_io::read_page(&mut reopened, restart::PAGE_ID, true)
                .expect("never return a torn page");
            let value = page.read_cell(0).expect("fixture cell survives").1;
            if fail_at > 2 {
                assert_eq!(value, b"after", "synced DWB replays at boundary {fail_at}");
            } else {
                assert!(value == b"before" || value == b"after");
            }
            let before = reopened.bytes.borrow().clone();
            pager_io::recover_from_dwb(&mut reopened).expect("recovery is idempotent");
            assert_eq!(*reopened.bytes.borrow(), before);
        }
    }
}

pub fn pager_frame_bounds_and_read_errors() {
    let mut file = MemoryFile::default();
    restart::create(&mut file).expect("create paged fixture");
    let count = pager_io::DWB_MAX_PAGES_PER_FRAME + 1;
    let pages: Vec<_> = (0..count)
        .map(|index| {
            let id = restart::PAGE_ID + u32::try_from(index).expect("fixture page id fits");
            let mut page = Page::new(PageType::BTreeLeaf, id);
            page.update_checksum();
            (id, page)
        })
        .collect();
    file.operations.borrow_mut().clear();
    pager_io::write_pages(&mut file, &pages, true).expect("batch splits across bounded frames");
    assert_eq!(
        file.operations
            .borrow()
            .iter()
            .filter(|op| **op == Operation::Sync)
            .count(),
        5
    );
    for operation in file.operations.borrow().iter() {
        if let Operation::Write(offset, len) = operation {
            if *offset == pager_io::dwb_zone_offset() {
                assert!(*len <= pager_io::DWB_ZONE_BYTES);
            }
        }
    }
    for (id, page) in &pages {
        let restored = pager_io::read_page(&mut file, *id, true).expect("short reads fill a page");
        assert_eq!(restored.as_bytes(), page.as_bytes());
    }
    file.bytes.borrow_mut()[restart::PAGE_ID as usize * PAGE_SIZE + 20] ^= 1;
    assert!(matches!(
        pager_io::read_page(&mut file, restart::PAGE_ID, true),
        Err(pager_io::PagerIoError::Page(_))
    ));
    pager_io::read_page(&mut file, restart::PAGE_ID, false)
        .expect("authenticated encrypted path can skip page CRC");
    assert!(pager_io::read_page(&mut file, u32::MAX, true).is_err());
    file.fail_len.set(true);
    assert!(
        pager_io::read_superblock_zone(&mut file).is_err(),
        "metadata failure is not an empty file"
    );
    assert!(pager_io::recover_from_dwb(&mut file).is_err());
}

pub fn pager_multiframe_checkpoint_io_and_crash_recovery() {
    let count = pager_io::DWB_MAX_PAGES_PER_FRAME + 2;
    let pages = |value: &[u8]| -> Vec<_> {
        (0..count)
            .map(|index| {
                let id = restart::PAGE_ID + u32::try_from(index).expect("bounded fixture ID");
                let mut page = Page::new(PageType::BTreeLeaf, id);
                page.insert_cell(b"key", value).expect("fixture cell fits");
                page.update_checksum();
                (id, page)
            })
            .collect()
    };
    let mut initial = MemoryFile::default();
    restart::create(&mut initial).expect("create paged fixture");
    pager_io::write_pages(&mut initial, &pages(b"before"), true).expect("seed both frames");
    let baseline = initial.durable.into_inner();
    let updated = pages(b"after");
    let mut successful = MemoryFile::from_bytes(baseline.clone());
    pager_io::write_pages(&mut successful, &updated, true).expect("update both frames");
    let operations = successful.operations.borrow();
    let bytes: usize = operations
        .iter()
        .map(|operation| match operation {
            Operation::Write(_, len) => *len,
            Operation::Sync => 0,
        })
        .sum();
    let frames = count.div_ceil(pager_io::DWB_MAX_PAGES_PER_FRAME);
    let expected_bytes = count * (2 * PAGE_SIZE + 4)
        + frames * reddb_file::PAGED_DWB_HEADER_SIZE
        + pager_io::DWB_ZONE_BYTES;
    assert_eq!(
        bytes, expected_bytes,
        "clear the recovery zone once per batch"
    );
    assert_eq!(
        operations
            .iter()
            .filter(|op| **op == Operation::Sync)
            .count(),
        2 * frames + 1
    );
    let operations = operations.clone();
    for (index, operation) in operations.iter().enumerate() {
        let fail_at = index + 1;
        let prefixes = match operation {
            Operation::Write(offset, len)
                if *offset == pager_io::dwb_zone_offset() && *len > PAGE_SIZE =>
            {
                vec![None, Some(0), Some(4), Some(12), Some(len - 1)]
            }
            _ => vec![None],
        };
        for prefix in prefixes {
            for retain_pending in [false, true] {
                let mut interrupted = MemoryFile::from_bytes(baseline.clone());
                interrupted.fail_at = Some(fail_at);
                interrupted.fail_prefix = prefix;
                assert!(pager_io::write_pages(&mut interrupted, &updated, true).is_err());
                assert_eq!(interrupted.operations.borrow().len(), fail_at);
                let bytes = if retain_pending {
                    interrupted.bytes.into_inner()
                } else {
                    interrupted.durable.into_inner()
                };
                let mut reopened = MemoryFile::from_bytes(bytes);
                pager_io::recover_from_dwb(&mut reopened).expect("recover interrupted frames");
                for (index, (id, _)) in updated.iter().enumerate() {
                    let page =
                        pager_io::read_page(&mut reopened, *id, true).expect("no torn home page");
                    let value = page.read_cell(0).expect("fixture row survives").1;
                    assert!(value == b"before" || value == b"after");
                    let staged = if index < pager_io::DWB_MAX_PAGES_PER_FRAME {
                        fail_at > 2
                    } else {
                        fail_at > pager_io::DWB_MAX_PAGES_PER_FRAME + 5
                    };
                    if staged {
                        assert_eq!(value, b"after", "both staged frames crossed their barriers");
                    }
                }
                let recovered = reopened.bytes.borrow().clone();
                pager_io::recover_from_dwb(&mut reopened).expect("repeated recovery");
                assert_eq!(*reopened.bytes.borrow(), recovered);
            }
        }
    }
}

pub fn pager_recovery_failure_boundaries() {
    let mut interrupted = MemoryFile::default();
    restart::create(&mut interrupted).expect("create paged fixture");
    interrupted.operations.borrow_mut().clear();
    interrupted.fail_at = Some(3);
    assert!(restart::update(&mut interrupted).is_err());
    let baseline = interrupted.bytes.into_inner();
    let mut successful = MemoryFile::from_bytes(baseline.clone());
    restart::recover(&mut successful).expect("count recovery boundaries");
    let count = successful.operations.borrow().len();
    for fail_at in 1..=count {
        for retain_pending in [false, true] {
            let mut file = MemoryFile::from_bytes(baseline.clone());
            file.fail_at = Some(fail_at);
            assert!(pager_io::recover_from_dwb(&mut file).is_err());
            assert_eq!(
                file.operations.borrow().len(),
                fail_at,
                "stop at first recovery I/O failure"
            );
            let bytes = if retain_pending {
                file.bytes.into_inner()
            } else {
                file.durable.into_inner()
            };
            let mut reopened = MemoryFile::from_bytes(bytes);
            restart::recover(&mut reopened).expect("retry recovery on a fresh handle");
        }
    }
}

pub fn pager_superblock_failure_boundaries() {
    let mut initial = MemoryFile::default();
    restart::create(&mut initial).expect("create paged fixture");
    let baseline = initial.bytes.into_inner();
    for fail_at in 1..=2 {
        for retain_pending in [false, true] {
            let mut file = MemoryFile::from_bytes(baseline.clone());
            let mut slot = [0; reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE];
            slot.copy_from_slice(&baseline[..reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE]);
            reddb_file::set_database_header_page_count(&mut slot, restart::PAGE_ID + 2)
                .expect("valid header image");
            file.fail_at = Some(fail_at);
            assert!(pager_io::write_superblock_slot(&mut file, &mut slot, 0, 3).is_err());
            assert_eq!(file.operations.borrow().len(), fail_at);
            let bytes = if retain_pending {
                file.bytes.into_inner()
            } else {
                file.durable.into_inner()
            };
            let mut reopened = MemoryFile::from_bytes(bytes);
            let zone = pager_io::read_superblock_zone(&mut reopened).expect("read surviving root");
            let selected =
                reddb_file::select_paged_superblock(&zone).expect("one root always survives");
            assert!(selected.generation == 2 || selected.generation == 3);
            let page = pager_io::read_page(&mut reopened, restart::PAGE_ID, true)
                .expect("root publication leaves existing pages readable");
            assert_eq!(page.read_cell(0).expect("existing cell").1, b"before");
        }
    }
}

fn open_pager(
    file: MemoryFile,
    create: bool,
) -> reddb_engine::storage::pager::Pager<MemoryFile, reddb_engine::storage::pager::NoWal> {
    use reddb_engine::storage::pager::{NoWal, Pager, PagerConfig};
    Pager::open_owned(
        file,
        "shared-pager.rdb".into(),
        PagerConfig {
            cache_size: 2,
            ..PagerConfig::default()
        },
        NoWal,
        create,
    )
    .expect("shared pager opens")
}

fn pager_state(
    pager: &reddb_engine::storage::pager::Pager<MemoryFile, reddb_engine::storage::pager::NoWal>,
) -> Vec<u8> {
    pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("read fixture durable bytes")
}

pub fn pager_orchestration_restart() {
    let pager = open_pager(MemoryFile::default(), true);
    let mut ids = Vec::new();
    for value in 0..96u8 {
        let mut page = pager.allocate_page(PageType::BTreeLeaf).expect("allocate");
        let id = page.page_id();
        page.insert_cell(b"key", &[value]).expect("cell fits");
        pager.write_page(id, page).expect("write");
        ids.push(id);
    }
    assert!(
        pager.cache_len() <= pager.cache_capacity() && pager.cache_len() < 96,
        "small cache forces dirty eviction"
    );
    pager.sync().expect("sync allocation and data");
    let pager = open_pager(MemoryFile::from_bytes(pager_state(&pager)), false);
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(
            pager
                .read_page(*id)
                .expect("read evicted page")
                .read_cell(0)
                .expect("cell")
                .1,
            vec![u8::try_from(index).expect("fixture value fits")]
        );
    }
    pager.enable_deferred_reclamation().expect("defer frees");
    for id in &ids[..4] {
        pager.free_page(*id).expect("defer");
    }
    assert_eq!(pager.free_page_count().expect("free count"), 0);
    let (pending, published) = pager.deferred_reclamation_pages().expect("deferred pages");
    assert_eq!(pending, ids[..4]);
    assert!(published.is_empty());
    pager
        .complete_deferred_publication(&[], &pending)
        .expect("publish removed roots");
    pager.sync().expect("persist freelist");
    let pager = open_pager(MemoryFile::from_bytes(pager_state(&pager)), false);
    let reused = pager
        .allocate_page(PageType::BTreeLeaf)
        .expect("reuse free page")
        .page_id();
    assert!(
        ids[..4].contains(&reused) || reused == pager_io::FIRST_ALLOCATABLE_PAGE + 96,
        "freelist may also reuse its consumed trunk"
    );
    pager
        .sync()
        .expect("mirror allocation ownership to both roots");
    let bytes = pager_state(&pager);
    for copy in 0..reddb_file::PAGED_SUPERBLOCK_SLOT_COUNT {
        let mut damaged = bytes.clone();
        let start = copy * reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE;
        damaged[start..start + reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE].fill(0);
        let recovered = open_pager(MemoryFile::from_bytes(damaged), false);
        assert_ne!(
            recovered
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate through sibling")
                .page_id(),
            reused,
            "sibling root cannot reuse live ownership"
        );
    }
}

pub fn pager_orchestration_wal_barrier() {
    use reddb_engine::storage::pager::{Pager, PagerConfig, WalBarrier};
    struct Barrier {
        operations: Rc<RefCell<Vec<Operation>>>,
        failed: Cell<bool>,
        seen: Cell<u64>,
    }
    impl WalBarrier for Barrier {
        fn flush_until(&self, lsn: u64) -> io::Result<()> {
            assert!(
                self.operations.borrow().is_empty(),
                "WAL barrier precedes every data-file write"
            );
            self.seen.set(lsn);
            if self.failed.get() {
                Err(io::Error::other("injected WAL failure"))
            } else {
                Ok(())
            }
        }
    }
    for fail in [false, true] {
        let file = MemoryFile::default();
        let operations = Rc::clone(&file.operations);
        let barrier = Barrier {
            operations: Rc::clone(&operations),
            failed: Cell::new(fail),
            seen: Cell::new(0),
        };
        let pager = Pager::open_owned(
            file,
            "wal.rdb".into(),
            PagerConfig::default(),
            barrier,
            true,
        )
        .expect("pager");
        operations.borrow_mut().clear();
        let mut page = pager.allocate_page(PageType::BTreeLeaf).expect("allocate");
        let id = page.page_id();
        page.set_lsn(42);
        pager.write_page(id, page).expect("write");
        assert_eq!(pager.flush().is_err(), fail);
        assert_eq!(pager.wal_barrier().seen.get(), 42);
        assert_eq!(
            operations.borrow().is_empty(),
            fail,
            "failed WAL must prevent home or DWB writes"
        );
    }
    let pager = open_pager(MemoryFile::default(), true);
    let mut page = pager.allocate_page(PageType::BTreeLeaf).expect("allocate");
    let id = page.page_id();
    page.set_lsn(1);
    pager.write_page(id, page).expect("write");
    assert!(pager.flush().is_err(), "NoWal rejects nonzero LSN");
}

pub fn pager_orchestration_checkpoint_recovery() {
    let file = MemoryFile::default();
    // Borrowed fixture handle lets us inspect durable bytes after pager teardown.
    let mut file = file;
    orchestration_restart::create(BorrowedMemoryFile(&mut file))
        .expect("create shared pager fixture");
    let initial = file.durable.borrow().clone();
    let mut complete = MemoryFile::from_bytes(initial.clone());
    complete.operations.borrow_mut().clear();
    orchestration_restart::update(BorrowedMemoryFile(&mut complete)).expect("successful update");
    let operations = complete.operations.borrow().len();
    for fail_at in 1..=operations {
        for retain_pending in [false, true] {
            let mut interrupted = MemoryFile::from_bytes(initial.clone());
            interrupted.fail_at = Some(fail_at);
            assert!(orchestration_restart::update(BorrowedMemoryFile(&mut interrupted)).is_err());
            let bytes = if retain_pending {
                interrupted.bytes.into_inner()
            } else {
                interrupted.durable.into_inner()
            };
            let reopened = open_pager(MemoryFile::from_bytes(bytes), false);
            let value = reopened
                .read_page(orchestration_restart::PAGE_ID)
                .expect("recover whole page")
                .read_cell(0)
                .expect("cell")
                .1;
            assert!(
                value == b"before" || value == b"after",
                "recover a complete page"
            );
            assert!(reopened.header().expect("header").checkpoint_in_progress);
        }
    }
    orchestration_restart::recover(BorrowedMemoryFile(&mut complete))
        .expect("complete interrupted checkpoint");
    orchestration_restart::verify(BorrowedMemoryFile(&mut complete))
        .expect("checkpoint survives fresh pager");
}

pub(crate) struct BorrowedMemoryFile<'a>(pub(crate) &'a mut MemoryFile);
impl VfsFile for BorrowedMemoryFile<'_> {
    fn try_clone(&self) -> io::Result<Self> {
        Err(io::ErrorKind::Unsupported.into())
    }
    fn file_len(&self) -> io::Result<u64> {
        self.0.file_len()
    }
    fn set_len(&self, size: u64) -> io::Result<()> {
        self.0.set_len(size)
    }
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.write_all(bytes)
    }
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.read(bytes)
    }
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.0.seek(pos)
    }
    fn sync_all(&self) -> io::Result<()> {
        self.0.sync_all()
    }
}

pub fn pager_orchestration_open_policy() {
    use reddb_engine::storage::pager::{NoWal, Pager, PagerConfig};
    let file = MemoryFile::default();
    assert!(
        Pager::open_owned(
            file,
            "empty.rdb".into(),
            PagerConfig::default(),
            NoWal,
            false
        )
        .is_err(),
        "empty existing file is not recreated"
    );
    let mut corrupt = MemoryFile::from_bytes(vec![42; PAGE_SIZE]);
    assert!(Pager::open_owned(
        BorrowedMemoryFile(&mut corrupt),
        "corrupt.rdb".into(),
        PagerConfig::default(),
        NoWal,
        true
    )
    .is_err());
    assert!(Pager::open_owned(
        BorrowedMemoryFile(&mut corrupt),
        "corrupt.rdb".into(),
        PagerConfig::default(),
        NoWal,
        false
    )
    .is_err());
    assert_eq!(
        *corrupt.bytes.borrow(),
        vec![42; PAGE_SIZE],
        "corruption is preserved"
    );
    let pager = open_pager(MemoryFile::default(), true);
    let bytes = pager_state(&pager);
    let mut partial = bytes.clone();
    partial.extend_from_slice(&[1; 17]);
    let reopened = open_pager(MemoryFile::from_bytes(partial), false);
    assert_eq!(
        reopened
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate past partial tail")
            .page_id(),
        pager_io::FIRST_ALLOCATABLE_PAGE + 1
    );
    let read_only = Pager::open_owned(
        MemoryFile::from_bytes(bytes),
        "readonly.rdb".into(),
        PagerConfig {
            read_only: true,
            ..PagerConfig::default()
        },
        NoWal,
        false,
    )
    .expect("read only opens");
    assert!(read_only.allocate_page(PageType::BTreeLeaf).is_err());
    assert!(read_only
        .free_page(pager_io::FIRST_ALLOCATABLE_PAGE)
        .is_err());
    assert!(read_only.complete_checkpoint(42).is_err());
}

pub fn pager_orchestration_allocation_fence() {
    let initial = open_pager(MemoryFile::default(), true);
    let mut ids = Vec::new();
    for _ in 0..8 {
        ids.push(
            initial
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate")
                .page_id(),
        );
    }
    initial.sync().expect("persist allocated pages");
    for id in &ids {
        initial.free_page(*id).expect("free");
    }
    initial.sync().expect("persist freelist trunk");
    let baseline = pager_state(&initial);
    // Each ownership fence writes and flushes a superblock, then flushes the
    // file. Neither copy may still own the trunk when its pages are reused.
    for fail_at in 1..=6 {
        let pager = open_pager(MemoryFile::from_bytes(baseline.clone()), false);
        let trunk = pager.header().expect("header").freelist_head;
        assert_ne!(trunk, 0);
        pager
            .with_file(|file| {
                file.operations.borrow_mut().clear();
                file.fail_at = Some(fail_at);
            })
            .expect("inject ownership fence failure");
        assert!(pager.allocate_page(PageType::BTreeLeaf).is_err());
        pager
            .with_file(|file| file.fail_at = None)
            .expect("restore file I/O");
        pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("retry completes both fences before reuse");
        let bytes = pager_state(&pager);
        for copy in 0..reddb_file::PAGED_SUPERBLOCK_SLOT_COUNT {
            let start = copy * reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE;
            let header = reddb_file::decode_database_header(
                &bytes[start..start + reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE],
            )
            .expect("valid fenced root");
            assert_ne!(
                header.freelist_head, trunk,
                "both roots relinquish consumed trunk before reuse"
            );
        }
    }
}

pub const CASES: [(&str, fn()); 14] = [
    (
        "btree_persisted_round_trip",
        btree::btree_persisted_round_trip,
    ),
    (
        "btree_reclamation_and_cursor_lifetime",
        btree::btree_reclamation_and_cursor_lifetime,
    ),
    (
        "btree_bounds_and_corruption",
        btree::btree_bounds_and_corruption,
    ),
    ("pager_orchestration_restart", pager_orchestration_restart),
    (
        "pager_orchestration_allocation_fence",
        pager_orchestration_allocation_fence,
    ),
    (
        "pager_orchestration_wal_barrier",
        pager_orchestration_wal_barrier,
    ),
    (
        "pager_orchestration_checkpoint_recovery",
        pager_orchestration_checkpoint_recovery,
    ),
    (
        "pager_orchestration_open_policy",
        pager_orchestration_open_policy,
    ),
    ("pager_barriers_and_restart", pager_barriers_and_restart),
    ("pager_failure_boundaries", pager_failure_boundaries),
    (
        "pager_multiframe_checkpoint_io_and_crash_recovery",
        pager_multiframe_checkpoint_io_and_crash_recovery,
    ),
    (
        "pager_frame_bounds_and_read_errors",
        pager_frame_bounds_and_read_errors,
    ),
    (
        "pager_superblock_failure_boundaries",
        pager_superblock_failure_boundaries,
    ),
    (
        "pager_recovery_failure_boundaries",
        pager_recovery_failure_boundaries,
    ),
];
