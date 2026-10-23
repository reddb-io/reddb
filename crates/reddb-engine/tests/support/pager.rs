use std::cell::{Cell, RefCell};
use std::io::{self, SeekFrom};

use reddb_engine::storage::{page::Page, page::PageType, page::PAGE_SIZE, pager_io};
use reddb_file::VfsFile;

#[path = "pager_restart.rs"]
mod restart;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Operation {
    Write(u64, usize),
    Sync,
}

#[derive(Default)]
struct MemoryFile {
    bytes: RefCell<Vec<u8>>,
    durable: RefCell<Vec<u8>>,
    position: u64,
    operations: RefCell<Vec<Operation>>,
    fail_at: Option<usize>,
    fail_len: Cell<bool>,
}

impl MemoryFile {
    fn from_bytes(bytes: Vec<u8>) -> Self {
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
            self.write_prefix(&buffer[..buffer.len() / 2]);
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
        6
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

pub const CASES: [(&str, fn()); 5] = [
    ("pager_barriers_and_restart", pager_barriers_and_restart),
    ("pager_failure_boundaries", pager_failure_boundaries),
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
