use super::record::{WalRecord, WAL_MAGIC, WAL_VERSION};
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

/// Reader for the Write-Ahead Log
pub struct WalReader {
    reader: BufReader<File>,
    position: u64,
}

impl WalReader {
    /// Open a WAL file for reading
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);

        // Check header
        let mut header = [0u8; 8];
        reader.read_exact(&mut header)?;

        if &header[0..4] != WAL_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid WAL magic bytes",
            ));
        }

        if header[4] != WAL_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unsupported WAL version: {}", header[4]),
            ));
        }

        Ok(Self {
            reader,
            position: 8,
        })
    }

    /// Iterate over records
    /// Returns iterator that yields (LSn, WalRecord)
    pub fn iter(self) -> WalIterator {
        WalIterator {
            reader: self.reader,
            position: self.position,
        }
    }
}

pub struct WalIterator {
    reader: BufReader<File>,
    position: u64,
}

impl Iterator for WalIterator {
    type Item = io::Result<(u64, WalRecord)>;

    fn next(&mut self) -> Option<Self::Item> {
        // Need to record start position for LSN
        // Since BufReader buffers, we can't trust file.seek(Current) directly without accounting for buffer.
        // But `WalRecord::read` reads sequentially.
        // The simple way: track position manually based on bytes read.
        // WalRecord::read consumes bytes.

        // Wait, WalRecord::read takes &mut R. We can wrap the reader to count bytes?
        // Or just rely on the fact that we read sequentially.
        // But we need to know the *start* LSN of the record to return it.

        let start_pos = self.position;

        // We need a way to track how many bytes were read by `WalRecord::read`.
        // Let's create a counting wrapper.
        struct CountingReader<'a, R> {
            inner: &'a mut R,
            count: u64,
        }

        impl<'a, R: Read> Read for CountingReader<'a, R> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let n = self.inner.read(buf)?;
                self.count += n as u64;
                Ok(n)
            }
        }

        let mut counter = CountingReader {
            inner: &mut self.reader,
            count: 0,
        };

        match WalRecord::read(&mut counter) {
            Ok(Some(record)) => {
                self.position += counter.count;
                Some(Ok((start_pos, record)))
            }
            Ok(None) => None, // EOF
            Err(e) => Some(Err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::writer::WalWriter;
    use super::*;
    use std::path::PathBuf;

    struct FileGuard {
        path: PathBuf,
    }

    impl Drop for FileGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn temp_wal(name: &str) -> (FileGuard, PathBuf) {
        let path =
            std::env::temp_dir().join(format!("rb_wal_reader_{}_{}.wal", name, std::process::id()));
        let guard = FileGuard { path: path.clone() };
        let _ = std::fs::remove_file(&path);
        (guard, path)
    }

    #[test]
    fn test_read_empty_wal() {
        let (_guard, path) = temp_wal("empty");

        // Create empty WAL
        {
            let _writer = WalWriter::open(&path).unwrap();
        }

        // Read it
        let reader = WalReader::open(&path).unwrap();
        let records: Vec<_> = reader.iter().collect();
        assert!(records.is_empty());
    }

    #[test]
    fn test_read_single_record() {
        let (_guard, path) = temp_wal("single");

        // Write one record
        {
            let mut writer = WalWriter::open(&path).unwrap();
            writer.append(&WalRecord::Begin { tx_id: 42 }).unwrap();
        }

        // Read it back
        let reader = WalReader::open(&path).unwrap();
        let records: Vec<_> = reader.iter().collect();

        assert_eq!(records.len(), 1);
        let (lsn, record) = records[0].as_ref().unwrap();
        assert_eq!(*lsn, 8);
        assert_eq!(*record, WalRecord::Begin { tx_id: 42 });
    }

    #[test]
    fn test_read_multiple_records() {
        let (_guard, path) = temp_wal("multi");

        // Write multiple records
        {
            let mut writer = WalWriter::open(&path).unwrap();
            writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
            writer
                .append(&WalRecord::PageWrite {
                    tx_id: 1,
                    page_id: 10,
                    data: vec![1, 2, 3],
                })
                .unwrap();
            writer.append(&WalRecord::Commit { tx_id: 1 }).unwrap();
        }

        // Read back
        let reader = WalReader::open(&path).unwrap();
        let records: Vec<_> = reader.iter().collect();

        assert_eq!(records.len(), 3);

        // Check each record
        match &records[0].as_ref().unwrap().1 {
            WalRecord::Begin { tx_id } => assert_eq!(*tx_id, 1),
            _ => panic!("Expected Begin"),
        }
        match &records[1].as_ref().unwrap().1 {
            WalRecord::PageWrite {
                tx_id,
                page_id,
                data,
            } => {
                assert_eq!(*tx_id, 1);
                assert_eq!(*page_id, 10);
                assert_eq!(data, &vec![1, 2, 3]);
            }
            _ => panic!("Expected PageWrite"),
        }
        match &records[2].as_ref().unwrap().1 {
            WalRecord::Commit { tx_id } => assert_eq!(*tx_id, 1),
            _ => panic!("Expected Commit"),
        }
    }

    #[test]
    fn test_lsn_tracking() {
        let (_guard, path) = temp_wal("lsn");

        // Write records
        let (lsn1, lsn2, lsn3);
        {
            let mut writer = WalWriter::open(&path).unwrap();
            lsn1 = writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
            lsn2 = writer.append(&WalRecord::Checkpoint { lsn: 100 }).unwrap();
            lsn3 = writer.append(&WalRecord::Rollback { tx_id: 1 }).unwrap();
        }

        // Read and verify LSNs
        let reader = WalReader::open(&path).unwrap();
        let records: Vec<_> = reader.iter().collect();

        assert_eq!(records.len(), 3);
        assert_eq!(records[0].as_ref().unwrap().0, lsn1);
        assert_eq!(records[1].as_ref().unwrap().0, lsn2);
        assert_eq!(records[2].as_ref().unwrap().0, lsn3);
    }

    #[test]
    fn test_invalid_magic() {
        let (_guard, path) = temp_wal("badmagic");

        // Write invalid file
        std::fs::write(&path, b"BAAD0000").unwrap();

        let result = WalReader::open(&path);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_version() {
        let (_guard, path) = temp_wal("badver");

        // Write header with wrong version
        let mut header = Vec::new();
        header.extend_from_slice(WAL_MAGIC);
        header.push(99); // Wrong version
        header.extend_from_slice(&[0u8; 3]);
        std::fs::write(&path, &header).unwrap();

        let result = WalReader::open(&path);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_large_page_write() {
        let (_guard, path) = temp_wal("large");

        let large_data: Vec<u8> = (0..4096).map(|i| (i % 256) as u8).collect();

        // Write large record
        {
            let mut writer = WalWriter::open(&path).unwrap();
            writer
                .append(&WalRecord::PageWrite {
                    tx_id: 1,
                    page_id: 0,
                    data: large_data.clone(),
                })
                .unwrap();
        }

        // Read back
        let reader = WalReader::open(&path).unwrap();
        let records: Vec<_> = reader.iter().collect();

        assert_eq!(records.len(), 1);
        match &records[0].as_ref().unwrap().1 {
            WalRecord::PageWrite { data, .. } => {
                assert_eq!(*data, large_data);
            }
            _ => panic!("Expected PageWrite"),
        }
    }
}
