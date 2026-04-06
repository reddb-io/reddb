use crate::storage::engine::crc32::{crc32, crc32_update};
use std::io::{self, Read};

/// WAL file magic bytes (RDBW)
pub const WAL_MAGIC: &[u8; 4] = b"RDBW";

/// WAL file format version
pub const WAL_VERSION: u8 = 1;

/// Type of WAL record
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RecordType {
    Begin = 1,
    Commit = 2,
    Rollback = 3,
    PageWrite = 4,
    Checkpoint = 5,
}

impl RecordType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(RecordType::Begin),
            2 => Some(RecordType::Commit),
            3 => Some(RecordType::Rollback),
            4 => Some(RecordType::PageWrite),
            5 => Some(RecordType::Checkpoint),
            _ => None,
        }
    }
}

/// A single entry in the write-ahead log
#[derive(Debug, Clone, PartialEq)]
pub enum WalRecord {
    /// Start of a transaction
    Begin { tx_id: u64 },
    /// Commit of a transaction
    Commit { tx_id: u64 },
    /// Rollback of a transaction
    Rollback { tx_id: u64 },
    /// Write of a page
    PageWrite {
        tx_id: u64,
        page_id: u32,
        data: Vec<u8>,
    },
    /// Checkpoint marker (indicates up to which LSN pages are flushed)
    Checkpoint { lsn: u64 },
}

impl WalRecord {
    /// Serialize record to bytes (including checksum)
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Layout:
        // [Type: 1]
        // [TxID: 8] (or LSN for Checkpoint)
        // [PageID: 4] (only for PageWrite)
        // [DataLen: 4] (only for PageWrite)
        // [Data: N] (only for PageWrite)
        // [Checksum: 4]

        match self {
            WalRecord::Begin { tx_id } => {
                buf.push(RecordType::Begin as u8);
                buf.extend_from_slice(&tx_id.to_le_bytes());
            }
            WalRecord::Commit { tx_id } => {
                buf.push(RecordType::Commit as u8);
                buf.extend_from_slice(&tx_id.to_le_bytes());
            }
            WalRecord::Rollback { tx_id } => {
                buf.push(RecordType::Rollback as u8);
                buf.extend_from_slice(&tx_id.to_le_bytes());
            }
            WalRecord::PageWrite {
                tx_id,
                page_id,
                data,
            } => {
                buf.push(RecordType::PageWrite as u8);
                buf.extend_from_slice(&tx_id.to_le_bytes());
                buf.extend_from_slice(&page_id.to_le_bytes());
                buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
                buf.extend_from_slice(data);
            }
            WalRecord::Checkpoint { lsn } => {
                buf.push(RecordType::Checkpoint as u8);
                buf.extend_from_slice(&lsn.to_le_bytes());
            }
        }

        // Calculate and append checksum
        let checksum = crc32(&buf);
        buf.extend_from_slice(&checksum.to_le_bytes());

        buf
    }

    /// Read a record from a reader
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Option<WalRecord>> {
        // Read type byte
        let mut type_buf = [0u8; 1];
        match reader.read_exact(&mut type_buf) {
            Ok(_) => (),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        };

        let record_type = RecordType::from_u8(type_buf[0])
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid record type"))?;

        // Start checksum calculation
        let mut running_crc = crc32_update(0, &type_buf);

        let record = match record_type {
            RecordType::Begin | RecordType::Commit | RecordType::Rollback => {
                let mut buf = [0u8; 8];
                reader.read_exact(&mut buf)?;
                running_crc = crc32_update(running_crc, &buf);
                let tx_id = u64::from_le_bytes(buf);

                match record_type {
                    RecordType::Begin => WalRecord::Begin { tx_id },
                    RecordType::Commit => WalRecord::Commit { tx_id },
                    RecordType::Rollback => WalRecord::Rollback { tx_id },
                    _ => unreachable!(),
                }
            }
            RecordType::PageWrite => {
                // Read TxID
                let mut tx_buf = [0u8; 8];
                reader.read_exact(&mut tx_buf)?;
                running_crc = crc32_update(running_crc, &tx_buf);
                let tx_id = u64::from_le_bytes(tx_buf);

                // Read PageID
                let mut page_buf = [0u8; 4];
                reader.read_exact(&mut page_buf)?;
                running_crc = crc32_update(running_crc, &page_buf);
                let page_id = u32::from_le_bytes(page_buf);

                // Read Length
                let mut len_buf = [0u8; 4];
                reader.read_exact(&mut len_buf)?;
                running_crc = crc32_update(running_crc, &len_buf);
                let len = u32::from_le_bytes(len_buf) as usize;

                // Read Data
                let mut data = vec![0u8; len];
                reader.read_exact(&mut data)?;
                running_crc = crc32_update(running_crc, &data);

                WalRecord::PageWrite {
                    tx_id,
                    page_id,
                    data,
                }
            }
            RecordType::Checkpoint => {
                let mut buf = [0u8; 8];
                reader.read_exact(&mut buf)?;
                running_crc = crc32_update(running_crc, &buf);
                let lsn = u64::from_le_bytes(buf);
                WalRecord::Checkpoint { lsn }
            }
        };

        // Verify checksum
        let mut crc_buf = [0u8; 4];
        reader.read_exact(&mut crc_buf)?;
        let stored_crc = u32::from_le_bytes(crc_buf);

        // Note: Our crc32_update takes the previous CRC value (not raw accumulator)
        // But our `crc32_update` implementation expects the *previous computed CRC* as input.
        // Wait, `crc32_update` in `crc32.rs` is:
        // let mut crc = crc ^ 0xFFFFFFFF; ... crc ^ 0xFFFFFFFF
        // So passing 0 starts a new one. Passing the result of a previous call continues it.
        // HOWEVER, `crc32(&buf)` is equivalent to `crc32_update(0, &buf)`.
        // So `running_crc` here should be correct.

        if running_crc != stored_crc {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "WAL record checksum mismatch",
            ));
        }

        Ok(Some(record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ==================== RecordType Tests ====================

    #[test]
    fn test_record_type_from_u8() {
        assert_eq!(RecordType::from_u8(1), Some(RecordType::Begin));
        assert_eq!(RecordType::from_u8(2), Some(RecordType::Commit));
        assert_eq!(RecordType::from_u8(3), Some(RecordType::Rollback));
        assert_eq!(RecordType::from_u8(4), Some(RecordType::PageWrite));
        assert_eq!(RecordType::from_u8(5), Some(RecordType::Checkpoint));
    }

    #[test]
    fn test_record_type_invalid() {
        assert_eq!(RecordType::from_u8(0), None);
        assert_eq!(RecordType::from_u8(6), None);
        assert_eq!(RecordType::from_u8(255), None);
    }

    // ==================== WalRecord::encode Tests ====================

    #[test]
    fn test_encode_begin() {
        let record = WalRecord::Begin { tx_id: 12345 };
        let encoded = record.encode();

        // Type (1) + TxID (8) + Checksum (4) = 13 bytes
        assert_eq!(encoded.len(), 13);
        assert_eq!(encoded[0], RecordType::Begin as u8);
    }

    #[test]
    fn test_encode_commit() {
        let record = WalRecord::Commit { tx_id: 99999 };
        let encoded = record.encode();

        assert_eq!(encoded.len(), 13);
        assert_eq!(encoded[0], RecordType::Commit as u8);
    }

    #[test]
    fn test_encode_rollback() {
        let record = WalRecord::Rollback { tx_id: 54321 };
        let encoded = record.encode();

        assert_eq!(encoded.len(), 13);
        assert_eq!(encoded[0], RecordType::Rollback as u8);
    }

    #[test]
    fn test_encode_checkpoint() {
        let record = WalRecord::Checkpoint { lsn: 1000000 };
        let encoded = record.encode();

        assert_eq!(encoded.len(), 13);
        assert_eq!(encoded[0], RecordType::Checkpoint as u8);
    }

    #[test]
    fn test_encode_page_write() {
        let data = vec![1, 2, 3, 4, 5];
        let record = WalRecord::PageWrite {
            tx_id: 100,
            page_id: 42,
            data: data.clone(),
        };
        let encoded = record.encode();

        // Type (1) + TxID (8) + PageID (4) + Len (4) + Data (5) + Checksum (4) = 26 bytes
        assert_eq!(encoded.len(), 26);
        assert_eq!(encoded[0], RecordType::PageWrite as u8);
    }

    #[test]
    fn test_encode_page_write_empty_data() {
        let record = WalRecord::PageWrite {
            tx_id: 1,
            page_id: 0,
            data: vec![],
        };
        let encoded = record.encode();

        // Type (1) + TxID (8) + PageID (4) + Len (4) + Checksum (4) = 21 bytes
        assert_eq!(encoded.len(), 21);
    }

    // ==================== WalRecord::read Tests ====================

    #[test]
    fn test_read_begin_roundtrip() {
        let original = WalRecord::Begin { tx_id: 42 };
        let encoded = original.encode();

        let mut cursor = Cursor::new(encoded);
        let decoded = WalRecord::read(&mut cursor).unwrap().unwrap();

        assert_eq!(decoded, original);
    }

    #[test]
    fn test_read_commit_roundtrip() {
        let original = WalRecord::Commit { tx_id: 999 };
        let encoded = original.encode();

        let mut cursor = Cursor::new(encoded);
        let decoded = WalRecord::read(&mut cursor).unwrap().unwrap();

        assert_eq!(decoded, original);
    }

    #[test]
    fn test_read_rollback_roundtrip() {
        let original = WalRecord::Rollback { tx_id: 777 };
        let encoded = original.encode();

        let mut cursor = Cursor::new(encoded);
        let decoded = WalRecord::read(&mut cursor).unwrap().unwrap();

        assert_eq!(decoded, original);
    }

    #[test]
    fn test_read_checkpoint_roundtrip() {
        let original = WalRecord::Checkpoint { lsn: 123456789 };
        let encoded = original.encode();

        let mut cursor = Cursor::new(encoded);
        let decoded = WalRecord::read(&mut cursor).unwrap().unwrap();

        assert_eq!(decoded, original);
    }

    #[test]
    fn test_read_page_write_roundtrip() {
        let original = WalRecord::PageWrite {
            tx_id: 50,
            page_id: 100,
            data: vec![10, 20, 30, 40, 50, 60, 70, 80],
        };
        let encoded = original.encode();

        let mut cursor = Cursor::new(encoded);
        let decoded = WalRecord::read(&mut cursor).unwrap().unwrap();

        assert_eq!(decoded, original);
    }

    #[test]
    fn test_read_page_write_large_data() {
        let data: Vec<u8> = (0..4096).map(|i| (i % 256) as u8).collect();
        let original = WalRecord::PageWrite {
            tx_id: 1,
            page_id: 0,
            data,
        };
        let encoded = original.encode();

        let mut cursor = Cursor::new(encoded);
        let decoded = WalRecord::read(&mut cursor).unwrap().unwrap();

        assert_eq!(decoded, original);
    }

    #[test]
    fn test_read_eof() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let result = WalRecord::read(&mut cursor).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_read_invalid_record_type() {
        let buf = vec![99, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]; // Invalid type 99
        let mut cursor = Cursor::new(buf);
        let result = WalRecord::read(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_checksum_mismatch() {
        let record = WalRecord::Begin { tx_id: 42 };
        let mut encoded = record.encode();

        // Corrupt the last byte (checksum)
        let len = encoded.len();
        encoded[len - 1] ^= 0xFF;

        let mut cursor = Cursor::new(encoded);
        let result = WalRecord::read(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_data_corruption() {
        let record = WalRecord::PageWrite {
            tx_id: 1,
            page_id: 2,
            data: vec![1, 2, 3, 4],
        };
        let mut encoded = record.encode();

        // Corrupt a data byte
        encoded[15] ^= 0xFF;

        let mut cursor = Cursor::new(encoded);
        let result = WalRecord::read(&mut cursor);
        assert!(result.is_err()); // Checksum will fail
    }

    // ==================== Multiple Records Tests ====================

    #[test]
    fn test_multiple_records_sequential() {
        let records = vec![
            WalRecord::Begin { tx_id: 1 },
            WalRecord::PageWrite {
                tx_id: 1,
                page_id: 10,
                data: vec![1, 2, 3],
            },
            WalRecord::PageWrite {
                tx_id: 1,
                page_id: 20,
                data: vec![4, 5, 6],
            },
            WalRecord::Commit { tx_id: 1 },
            WalRecord::Checkpoint { lsn: 100 },
        ];

        // Encode all
        let mut buf = Vec::new();
        for r in &records {
            buf.extend_from_slice(&r.encode());
        }

        // Read them back
        let mut cursor = Cursor::new(buf);
        for expected in &records {
            let decoded = WalRecord::read(&mut cursor).unwrap().unwrap();
            assert_eq!(&decoded, expected);
        }

        // Next read should return None (EOF)
        assert!(WalRecord::read(&mut cursor).unwrap().is_none());
    }

    // ==================== Constants Tests ====================

    #[test]
    fn test_wal_magic() {
        assert_eq!(WAL_MAGIC, b"RDBW");
    }

    #[test]
    fn test_wal_version() {
        assert_eq!(WAL_VERSION, 1);
    }
}
