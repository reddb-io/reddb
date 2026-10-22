use std::cell::RefCell;
use std::io::{self, SeekFrom};
use std::rc::Rc;

use reddb_file::{EmbeddedRdbArtifact, EmbeddedRdbFile, VfsFile};

#[test]
fn portable_and_native_share_snapshots_and_wal_in_both_directions() {
    let directory = tempfile::tempdir().expect("test directory");
    let path = directory.path().join("portable.rdb");
    let file = std::fs::File::create(&path).expect("empty file");
    drop(file);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("file");
    let mut portable = EmbeddedRdbFile::create(file, &path, b"RDSTfirst", 123).expect("create");
    portable
        .append_wal_payloads(&[
            b"first transaction".to_vec(),
            b"second transaction".to_vec(),
        ])
        .expect("append");
    drop(portable);

    let native = EmbeddedRdbArtifact::open(&path).expect("native open");
    assert_eq!(
        EmbeddedRdbArtifact::read_snapshot(&native).expect("snapshot"),
        Some(b"RDSTfirst".to_vec())
    );
    assert_eq!(
        EmbeddedRdbArtifact::read_wal_payloads(&native).expect("WAL"),
        vec![
            b"first transaction".to_vec(),
            b"second transaction".to_vec()
        ]
    );
    EmbeddedRdbArtifact::write_snapshot(&path, b"RDSTcheckpoint").expect("native checkpoint");
    EmbeddedRdbArtifact::append_wal_payloads(&path, &[b"native transaction".to_vec()])
        .expect("native append");

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("file");
    let mut portable = EmbeddedRdbFile::open(file, &path).expect("portable reopen");
    assert_eq!(
        portable.read_snapshot().expect("snapshot"),
        Some(b"RDSTcheckpoint".to_vec())
    );
    assert_eq!(
        portable.read_wal_payloads().expect("WAL"),
        vec![b"native transaction".to_vec()]
    );
    portable
        .write_snapshot(b"RDSTportable checkpoint", 128 * 1024)
        .expect("portable checkpoint");
    drop(portable);
    let native = EmbeddedRdbArtifact::open(&path).expect("native reopen");
    assert_eq!(native.manifest.wal_region_bytes, 128 * 1024);
    assert_eq!(
        EmbeddedRdbArtifact::read_snapshot(&native).expect("snapshot"),
        Some(b"RDSTportable checkpoint".to_vec())
    );
    assert!(EmbeddedRdbArtifact::read_wal_payloads(&native)
        .expect("WAL")
        .is_empty());
}

#[derive(Default)]
struct Image {
    bytes: Vec<u8>,
    durable: Vec<u8>,
    operations: usize,
    fail_at: Option<usize>,
}

impl Image {
    fn operation(&mut self) -> io::Result<()> {
        self.operations += 1;
        if self.fail_at == Some(self.operations) {
            return Err(io::Error::other("injected I/O failure"));
        }
        Ok(())
    }

    fn crash(&mut self) {
        self.bytes = self.durable.clone();
        self.fail_at = None;
    }
}

struct MemoryFile {
    image: Rc<RefCell<Image>>,
    position: u64,
}

impl MemoryFile {
    fn new(image: &Rc<RefCell<Image>>) -> Self {
        Self {
            image: Rc::clone(image),
            position: 0,
        }
    }
}

impl VfsFile for MemoryFile {
    fn try_clone(&self) -> io::Result<Self> {
        Err(io::ErrorKind::Unsupported.into())
    }
    fn file_len(&self) -> io::Result<u64> {
        Ok(self.image.borrow().bytes.len() as u64)
    }
    fn set_len(&self, len: u64) -> io::Result<()> {
        let mut image = self.image.borrow_mut();
        image.operation()?;
        image
            .bytes
            .resize(usize::try_from(len).expect("test file size"), 0);
        Ok(())
    }
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut image = self.image.borrow_mut();
        image.operation()?;
        let start = usize::try_from(self.position).expect("test offset");
        let end = start + bytes.len();
        if end > image.bytes.len() {
            image.bytes.resize(end, 0);
        }
        image.bytes[start..end].copy_from_slice(bytes);
        self.position = end as u64;
        Ok(())
    }
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let image = self.image.borrow();
        let start = usize::try_from(self.position).expect("test offset");
        // Exercise the default read_exact loop rather than full reads only.
        let count = bytes
            .len()
            .min(37)
            .min(image.bytes.len().saturating_sub(start));
        if count > 0 {
            bytes[..count].copy_from_slice(&image.bytes[start..start + count]);
        }
        self.position += count as u64;
        Ok(count)
    }
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.position = match position {
            SeekFrom::Start(offset) => offset,
            SeekFrom::Current(offset) => self
                .position
                .checked_add_signed(offset)
                .ok_or(io::ErrorKind::InvalidInput)?,
            SeekFrom::End(offset) => self
                .file_len()?
                .checked_add_signed(offset)
                .ok_or(io::ErrorKind::InvalidInput)?,
        };
        Ok(self.position)
    }
    fn sync_all(&self) -> io::Result<()> {
        let mut image = self.image.borrow_mut();
        image.operation()?;
        image.durable = image.bytes.clone();
        Ok(())
    }
}

fn seeded_file() -> (Rc<RefCell<Image>>, EmbeddedRdbFile<MemoryFile>) {
    let image = Rc::new(RefCell::new(Image::default()));
    let mut file = EmbeddedRdbFile::create(MemoryFile::new(&image), "memory.rdb", b"RDSTbase", 1)
        .expect("create");
    file.append_wal_payloads(&[b"acknowledged".to_vec()])
        .expect("seed append");
    image.borrow_mut().operations = 0;
    (image, file)
}

#[test]
fn append_failure_at_each_write_or_barrier_preserves_acknowledged_data() {
    let (successful, mut file) = seeded_file();
    file.append_wal_payloads(&[b"pending A".to_vec(), b"pending B".to_vec()])
        .expect("append");
    let operations = successful.borrow().operations;
    assert!(operations >= 4);

    for failure in 1..=operations {
        let (image, mut file) = seeded_file();
        image.borrow_mut().fail_at = Some(failure);
        assert!(file
            .append_wal_payloads(&[b"pending A".to_vec(), b"pending B".to_vec()])
            .is_err());
        assert!(file
            .append_wal_payloads(&[b"unsafe retry".to_vec()])
            .is_err());
        drop(file);
        image.borrow_mut().crash();
        let mut recovered =
            EmbeddedRdbFile::open(MemoryFile::new(&image), "memory.rdb").expect("recover");
        assert_eq!(
            recovered.read_snapshot().expect("snapshot"),
            Some(b"RDSTbase".to_vec())
        );
        assert_eq!(
            recovered.read_wal_payloads().expect("WAL"),
            vec![b"acknowledged".to_vec()],
            "failure {failure}"
        );
    }
}

#[test]
fn checkpoint_failure_at_each_write_or_barrier_preserves_previous_root_and_wal() {
    let (successful, mut file) = seeded_file();
    file.write_snapshot(b"RDSTnew image", 0)
        .expect("checkpoint");
    let operations = successful.borrow().operations;
    assert!(operations >= 6);

    for failure in 1..=operations {
        let (image, mut file) = seeded_file();
        image.borrow_mut().fail_at = Some(failure);
        assert!(file.write_snapshot(b"RDSTnew image", 0).is_err());
        assert!(file.read_snapshot().is_err());
        drop(file);
        image.borrow_mut().crash();
        let mut recovered =
            EmbeddedRdbFile::open(MemoryFile::new(&image), "memory.rdb").expect("recover");
        assert_eq!(
            recovered.read_snapshot().expect("snapshot"),
            Some(b"RDSTbase".to_vec()),
            "failure {failure}"
        );
        assert_eq!(
            recovered.read_wal_payloads().expect("WAL"),
            vec![b"acknowledged".to_vec()]
        );
    }
}

#[test]
fn creating_over_an_existing_file_does_not_destroy_it() {
    let (image, file) = seeded_file();
    drop(file);
    let before = image.borrow().bytes.clone();
    assert!(EmbeddedRdbFile::create(MemoryFile::new(&image), "memory.rdb", &[], 0).is_err());
    assert_eq!(image.borrow().bytes, before);
}

#[test]
fn corrupt_published_wal_is_an_error_instead_of_a_successful_partial_recovery() {
    let (image, mut file) = seeded_file();
    let state = file.state().expect("state");
    drop(file);
    image.borrow_mut().bytes[state.manifest.wal_region_offset as usize + 40] ^= 1;
    assert!(EmbeddedRdbFile::open(MemoryFile::new(&image), "memory.rdb").is_err());
}
