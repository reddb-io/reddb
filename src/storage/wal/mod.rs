//! Write-Ahead Log (WAL) for RedDB durability.
//!
//! Handles crash recovery by logging all changes before they are applied to the database file.
//!
//! # Format
//!
//! - **Header:** `RDBW` (4 bytes) + Version (1 byte) + Reserved (3 bytes)
//! - **Records:** Sequence of variable-length records.
//! - **Checksum:** Each record ends with a CRC32 checksum.

pub mod checkpoint;
pub mod reader;
pub mod record;
pub mod transaction;
pub mod writer;

pub use checkpoint::{CheckpointError, CheckpointMode, CheckpointResult, Checkpointer};
pub use reader::WalReader;
pub use record::{RecordType, WalRecord};
pub use transaction::{Transaction, TransactionManager, TxError, TxState};
pub use writer::WalWriter;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn test_wal_write_read() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("reddb_wal_test_{}", timestamp));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test.wal");
        if path.exists() {
            fs::remove_file(&path).unwrap();
        }

        // Write
        {
            let mut writer = WalWriter::open(&path).unwrap();

            let rec1 = WalRecord::Begin { tx_id: 100 };
            writer.append(&rec1).unwrap();

            let rec2 = WalRecord::PageWrite {
                tx_id: 100,
                page_id: 5,
                data: vec![1, 2, 3, 4],
            };
            writer.append(&rec2).unwrap();

            let rec3 = WalRecord::Commit { tx_id: 100 };
            writer.append(&rec3).unwrap();
        }

        // Read
        {
            let reader = WalReader::open(&path).unwrap();
            let records: Vec<_> = reader.iter().map(|r| r.unwrap().1).collect();

            assert_eq!(records.len(), 3);

            match &records[0] {
                WalRecord::Begin { tx_id } => assert_eq!(*tx_id, 100),
                _ => panic!("Wrong type"),
            }

            match &records[1] {
                WalRecord::PageWrite {
                    tx_id,
                    page_id,
                    data,
                } => {
                    assert_eq!(*tx_id, 100);
                    assert_eq!(*page_id, 5);
                    assert_eq!(*data, vec![1, 2, 3, 4]);
                }
                _ => panic!("Wrong type"),
            }

            match &records[2] {
                WalRecord::Commit { tx_id } => assert_eq!(*tx_id, 100),
                _ => panic!("Wrong type"),
            }
        }

        // Cleanup
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
        if dir.exists() {
            fs::remove_dir(dir).unwrap();
        }
    }
}
