use super::*;
use std::io::{self, Read, Seek, Write};

struct CountingFile {
    file: std::fs::File,
    bytes_read: u64,
}

impl VfsFile for CountingFile {
    fn try_clone(&self) -> io::Result<Self> {
        Err(io::ErrorKind::Unsupported.into())
    }
    fn file_len(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }
    fn set_len(&self, len: u64) -> io::Result<()> {
        self.file.set_len(len)
    }
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)
    }
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.file.read(bytes)?;
        self.bytes_read += count as u64;
        Ok(count)
    }
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file.seek(pos)
    }
    fn sync_all(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}

#[test]
fn opening_embedded_snapshot_reads_its_bytes_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("snapshot.rdb");
    let mut snapshot = vec![42u8; 8 * 1024 * 1024];
    snapshot[..4].copy_from_slice(b"RDST");
    EmbeddedRdbArtifact::create_with_snapshot(&path, &snapshot).unwrap();
    let mut file = CountingFile {
        file: std::fs::File::open(&path).unwrap(),
        bytes_read: 0,
    };
    let (_, loaded) = EmbeddedRdbArtifact::open_inner_with_snapshot(&mut file, &path).unwrap();
    assert_eq!(loaded.as_deref(), Some(snapshot.as_slice()));
    eprintln!(
        "embedded startup: snapshot_bytes={} read_bytes={}",
        snapshot.len(),
        file.bytes_read
    );
    assert!(
        file.bytes_read <= snapshot.len() as u64 + 128 * 1024,
        "embedded open read {} bytes for a {}-byte snapshot",
        file.bytes_read,
        snapshot.len()
    );
}
