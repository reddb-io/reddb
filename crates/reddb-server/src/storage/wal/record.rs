use crate::storage::engine::crc32::{crc32, crc32_update};
use std::io::{self, Read};

/// WAL file magic bytes (RDBW)
pub const WAL_MAGIC: &[u8; 4] = b"RDBW";

/// WAL file format version
pub const WAL_VERSION: u8 = 3;
pub const WAL_VERSION_V2: u8 = 2;
pub const WAL_DEFAULT_TERM: u64 = crate::replication::DEFAULT_REPLICATION_TERM;

/// Minimum payload size (bytes) to attempt zstd compression.
/// Smaller records pay more overhead than benefit from compression.
const COMPRESS_THRESHOLD: usize = 256;

/// Compression algorithm tag embedded in `PageWriteCompressed` records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Compression {
    None = 0,
    Zstd = 1,
}

impl Compression {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Compression::None),
            1 => Some(Compression::Zstd),
            _ => None,
        }
    }
}

/// Type of WAL record
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RecordType {
    Begin = 1,
    Commit = 2,
    Rollback = 3,
    /// Legacy uncompressed page write (v1 format — still written for
    /// small payloads to avoid compression overhead).
    PageWrite = 4,
    Checkpoint = 5,
    /// Compressed page write (v2 format).
    ///
    /// Layout (after the type byte):
    /// ```text
    /// [TxID: 8][PageID: 4][Compression: 1][OrigLen: 4][DataLen: 4][Data: N][CRC: 4]
    /// ```
    /// `OrigLen` is the original (uncompressed) size; needed to pre-allocate
    /// the decompression buffer.
    PageWriteCompressed = 6,
    /// Logical autocommit transaction commit batch (v2 format).
    ///
    /// Layout (after the type byte):
    /// ```text
    /// [TxID: 8][ActionCount: 4][[DataLen: 4][Data: N]...][CRC: 4]
    /// ```
    TxCommitBatch = 7,
    /// Full-page image (FPI). Captures a complete page before its first
    /// modification within a checkpoint cycle so torn-page recovery can
    /// replay the pristine image before redo replays subsequent
    /// `PageWrite`s. Enables `fold_dwb_into_wal` to retire the `-dwb`
    /// sidecar (gh-478).
    ///
    /// Layout (after the type byte):
    /// ```text
    /// [TxID: 8][PageID: 4][CkptEpoch: 8][DataLen: 4][Data: N][CRC: 4]
    /// ```
    FullPageImage = 8,
    /// Logical vector insert for vector-turbo WAL replay.
    VectorInsert = 9,
}

impl RecordType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(RecordType::Begin),
            2 => Some(RecordType::Commit),
            3 => Some(RecordType::Rollback),
            4 => Some(RecordType::PageWrite),
            5 => Some(RecordType::Checkpoint),
            6 => Some(RecordType::PageWriteCompressed),
            7 => Some(RecordType::TxCommitBatch),
            8 => Some(RecordType::FullPageImage),
            9 => Some(RecordType::VectorInsert),
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
    /// Write of a page — always carries uncompressed data (transparent to
    /// callers: `read()` decompresses on-the-fly).
    PageWrite {
        tx_id: u64,
        page_id: u32,
        data: Vec<u8>,
    },
    /// Atomic logical commit batch. Recovery applies all actions in
    /// order iff this complete record and checksum are present.
    TxCommitBatch { tx_id: u64, actions: Vec<Vec<u8>> },
    /// Full-page image — pristine page bytes captured before the first
    /// modification per checkpoint cycle. Recovery applies these before
    /// redo so torn writes are healed without the `-dwb` sidecar.
    FullPageImage {
        tx_id: u64,
        page_id: u32,
        ckpt_epoch: u64,
        data: Vec<u8>,
    },
    /// Logical vector insert payload. Recovery can replay FP32 into the
    /// in-memory vector-turbo index without requiring snapshot files.
    VectorInsert {
        collection: String,
        entity_id: u64,
        vector: Vec<f32>,
    },
    /// Checkpoint marker (indicates up to which LSN pages are flushed)
    Checkpoint { lsn: u64 },
}

impl WalRecord {
    /// Serialize record to bytes (including checksum).
    ///
    /// `PageWrite` records whose payload is ≥ `COMPRESS_THRESHOLD` bytes are
    /// compressed with zstd level 3 and emitted as `PageWriteCompressed`.
    /// Smaller payloads use the plain `PageWrite` encoding (no overhead).
    pub fn encode(&self) -> Vec<u8> {
        self.encode_with_term(WAL_DEFAULT_TERM)
    }

    /// Serialize record to bytes with the replication term stamped into
    /// the physical record envelope.
    pub fn encode_with_term(&self, term: u64) -> Vec<u8> {
        let mut buf = Vec::new();

        // Layout (non-PageWrite):
        // [Type: 1]
        // [Term: 8]
        // [TxID/LSN: 8]
        // [Checksum: 4]
        //
        // PageWrite (uncompressed):
        // [Type: 1][TxID: 8][PageID: 4][DataLen: 4][Data: N][CRC: 4]
        //
        // PageWriteCompressed:
        // [Type: 1][TxID: 8][PageID: 4][Compression: 1][OrigLen: 4][DataLen: 4][Data: N][CRC: 4]
        //
        // TxCommitBatch:
        // [Type: 1][TxID: 8][ActionCount: 4][[DataLen: 4][Data: N]...][CRC: 4]

        match self {
            WalRecord::Begin { tx_id } => {
                buf.push(RecordType::Begin as u8);
                buf.extend_from_slice(&term.to_le_bytes());
                buf.extend_from_slice(&tx_id.to_le_bytes());
            }
            WalRecord::Commit { tx_id } => {
                buf.push(RecordType::Commit as u8);
                buf.extend_from_slice(&term.to_le_bytes());
                buf.extend_from_slice(&tx_id.to_le_bytes());
            }
            WalRecord::Rollback { tx_id } => {
                buf.push(RecordType::Rollback as u8);
                buf.extend_from_slice(&term.to_le_bytes());
                buf.extend_from_slice(&tx_id.to_le_bytes());
            }
            WalRecord::PageWrite {
                tx_id,
                page_id,
                data,
            } => {
                if data.len() >= COMPRESS_THRESHOLD {
                    // Try zstd compression; fall back to uncompressed if it expands.
                    if let Ok(compressed) =
                        zstd::bulk::compress(data.as_slice(), /* level */ 3)
                    {
                        if compressed.len() < data.len() {
                            // Compressed is smaller — use compressed format.
                            buf.push(RecordType::PageWriteCompressed as u8);
                            buf.extend_from_slice(&term.to_le_bytes());
                            buf.extend_from_slice(&tx_id.to_le_bytes());
                            buf.extend_from_slice(&page_id.to_le_bytes());
                            buf.push(Compression::Zstd as u8);
                            buf.extend_from_slice(&(data.len() as u32).to_le_bytes()); // orig_len
                            buf.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
                            buf.extend_from_slice(&compressed);
                            let checksum = crc32(&buf);
                            buf.extend_from_slice(&checksum.to_le_bytes());
                            return buf;
                        }
                    }
                }
                // Uncompressed path (small payload or compression expanded).
                buf.push(RecordType::PageWrite as u8);
                buf.extend_from_slice(&term.to_le_bytes());
                buf.extend_from_slice(&tx_id.to_le_bytes());
                buf.extend_from_slice(&page_id.to_le_bytes());
                buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
                buf.extend_from_slice(data);
            }
            WalRecord::TxCommitBatch { tx_id, actions } => {
                buf.push(RecordType::TxCommitBatch as u8);
                buf.extend_from_slice(&term.to_le_bytes());
                buf.extend_from_slice(&tx_id.to_le_bytes());
                buf.extend_from_slice(&(actions.len() as u32).to_le_bytes());
                for action in actions {
                    buf.extend_from_slice(&(action.len() as u32).to_le_bytes());
                    buf.extend_from_slice(action);
                }
            }
            WalRecord::FullPageImage {
                tx_id,
                page_id,
                ckpt_epoch,
                data,
            } => {
                buf.push(RecordType::FullPageImage as u8);
                buf.extend_from_slice(&term.to_le_bytes());
                buf.extend_from_slice(&tx_id.to_le_bytes());
                buf.extend_from_slice(&page_id.to_le_bytes());
                buf.extend_from_slice(&ckpt_epoch.to_le_bytes());
                buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
                buf.extend_from_slice(data);
            }
            WalRecord::VectorInsert {
                collection,
                entity_id,
                vector,
            } => {
                buf.push(RecordType::VectorInsert as u8);
                buf.extend_from_slice(&term.to_le_bytes());
                buf.extend_from_slice(&(collection.len() as u32).to_le_bytes());
                buf.extend_from_slice(collection.as_bytes());
                buf.extend_from_slice(&entity_id.to_le_bytes());
                buf.extend_from_slice(&(vector.len() as u32).to_le_bytes());
                for value in vector {
                    buf.extend_from_slice(&value.to_le_bytes());
                }
            }
            WalRecord::Checkpoint { lsn } => {
                buf.push(RecordType::Checkpoint as u8);
                buf.extend_from_slice(&term.to_le_bytes());
                buf.extend_from_slice(&lsn.to_le_bytes());
            }
        }

        // Calculate and append checksum
        let checksum = crc32(&buf);
        buf.extend_from_slice(&checksum.to_le_bytes());

        buf
    }

    /// Read a record from a reader.
    ///
    /// Handles both v1 (`PageWrite`) and v2 (`PageWriteCompressed`) record
    /// formats transparently — callers always receive uncompressed data.
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Option<WalRecord>> {
        Ok(Self::read_with_term(reader)?.map(|(_, record)| record))
    }

    /// Read a record and return the term stamped into its physical envelope.
    pub fn read_with_term<R: Read>(reader: &mut R) -> io::Result<Option<(u64, WalRecord)>> {
        Self::read_with_format_version(reader, WAL_VERSION)
    }

    pub(crate) fn read_with_format_version<R: Read>(
        reader: &mut R,
        format_version: u8,
    ) -> io::Result<Option<(u64, WalRecord)>> {
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
        let term = match format_version {
            WAL_VERSION => {
                let mut term_buf = [0u8; 8];
                reader.read_exact(&mut term_buf)?;
                running_crc = crc32_update(running_crc, &term_buf);
                u64::from_le_bytes(term_buf)
            }
            WAL_VERSION_V2 => WAL_DEFAULT_TERM,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Unsupported WAL version: {format_version}"),
                ));
            }
        };

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
            RecordType::PageWriteCompressed => {
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

                // Read Compression algorithm byte
                let mut comp_buf = [0u8; 1];
                reader.read_exact(&mut comp_buf)?;
                running_crc = crc32_update(running_crc, &comp_buf);
                let compression = Compression::from_u8(comp_buf[0]).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Unknown WAL compression algorithm: {}", comp_buf[0]),
                    )
                })?;

                // Read original (uncompressed) length — used to pre-allocate decompression buffer
                let mut orig_len_buf = [0u8; 4];
                reader.read_exact(&mut orig_len_buf)?;
                running_crc = crc32_update(running_crc, &orig_len_buf);
                let orig_len = u32::from_le_bytes(orig_len_buf) as usize;

                // Read compressed data length
                let mut len_buf = [0u8; 4];
                reader.read_exact(&mut len_buf)?;
                running_crc = crc32_update(running_crc, &len_buf);
                let len = u32::from_le_bytes(len_buf) as usize;

                // Read compressed data
                let mut compressed = vec![0u8; len];
                reader.read_exact(&mut compressed)?;
                running_crc = crc32_update(running_crc, &compressed);

                // Decompress
                let data = match compression {
                    Compression::Zstd => {
                        let mut out = vec![0u8; orig_len];
                        zstd::bulk::decompress_to_buffer(&compressed, &mut out).map_err(|e| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("WAL zstd decompress failed: {e}"),
                            )
                        })?;
                        out
                    }
                    Compression::None => compressed,
                };

                WalRecord::PageWrite {
                    tx_id,
                    page_id,
                    data,
                }
            }
            RecordType::TxCommitBatch => {
                let mut tx_buf = [0u8; 8];
                reader.read_exact(&mut tx_buf)?;
                running_crc = crc32_update(running_crc, &tx_buf);
                let tx_id = u64::from_le_bytes(tx_buf);

                let mut count_buf = [0u8; 4];
                reader.read_exact(&mut count_buf)?;
                running_crc = crc32_update(running_crc, &count_buf);
                let count = u32::from_le_bytes(count_buf) as usize;

                let mut actions = Vec::with_capacity(count);
                for _ in 0..count {
                    let mut len_buf = [0u8; 4];
                    reader.read_exact(&mut len_buf)?;
                    running_crc = crc32_update(running_crc, &len_buf);
                    let len = u32::from_le_bytes(len_buf) as usize;

                    let mut action = vec![0u8; len];
                    reader.read_exact(&mut action)?;
                    running_crc = crc32_update(running_crc, &action);
                    actions.push(action);
                }

                WalRecord::TxCommitBatch { tx_id, actions }
            }
            RecordType::VectorInsert => {
                let mut len_buf = [0u8; 4];
                reader.read_exact(&mut len_buf)?;
                running_crc = crc32_update(running_crc, &len_buf);
                let collection_len = u32::from_le_bytes(len_buf) as usize;

                let mut collection_buf = vec![0u8; collection_len];
                reader.read_exact(&mut collection_buf)?;
                running_crc = crc32_update(running_crc, &collection_buf);
                let collection = String::from_utf8(collection_buf).map_err(|err| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid collection utf8: {err}"),
                    )
                })?;

                let mut entity_buf = [0u8; 8];
                reader.read_exact(&mut entity_buf)?;
                running_crc = crc32_update(running_crc, &entity_buf);
                let entity_id = u64::from_le_bytes(entity_buf);

                let mut count_buf = [0u8; 4];
                reader.read_exact(&mut count_buf)?;
                running_crc = crc32_update(running_crc, &count_buf);
                let count = u32::from_le_bytes(count_buf) as usize;

                let mut vector = Vec::with_capacity(count);
                for _ in 0..count {
                    let mut value_buf = [0u8; 4];
                    reader.read_exact(&mut value_buf)?;
                    running_crc = crc32_update(running_crc, &value_buf);
                    vector.push(f32::from_le_bytes(value_buf));
                }

                WalRecord::VectorInsert {
                    collection,
                    entity_id,
                    vector,
                }
            }
            RecordType::FullPageImage => {
                let mut tx_buf = [0u8; 8];
                reader.read_exact(&mut tx_buf)?;
                running_crc = crc32_update(running_crc, &tx_buf);
                let tx_id = u64::from_le_bytes(tx_buf);

                let mut page_buf = [0u8; 4];
                reader.read_exact(&mut page_buf)?;
                running_crc = crc32_update(running_crc, &page_buf);
                let page_id = u32::from_le_bytes(page_buf);

                let mut epoch_buf = [0u8; 8];
                reader.read_exact(&mut epoch_buf)?;
                running_crc = crc32_update(running_crc, &epoch_buf);
                let ckpt_epoch = u64::from_le_bytes(epoch_buf);

                let mut len_buf = [0u8; 4];
                reader.read_exact(&mut len_buf)?;
                running_crc = crc32_update(running_crc, &len_buf);
                let len = u32::from_le_bytes(len_buf) as usize;

                let mut data = vec![0u8; len];
                reader.read_exact(&mut data)?;
                running_crc = crc32_update(running_crc, &data);

                WalRecord::FullPageImage {
                    tx_id,
                    page_id,
                    ckpt_epoch,
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

        if running_crc != stored_crc {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "WAL record checksum mismatch",
            ));
        }

        Ok(Some((term, record)))
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
        assert_eq!(
            RecordType::from_u8(6),
            Some(RecordType::PageWriteCompressed)
        );
        assert_eq!(RecordType::from_u8(7), Some(RecordType::TxCommitBatch));
        assert_eq!(RecordType::from_u8(8), Some(RecordType::FullPageImage));
        assert_eq!(RecordType::from_u8(9), Some(RecordType::VectorInsert));
    }

    #[test]
    fn test_record_type_invalid() {
        assert_eq!(RecordType::from_u8(0), None);
        assert_eq!(RecordType::from_u8(10), None);
        assert_eq!(RecordType::from_u8(255), None);
    }

    // ==================== WalRecord::encode Tests ====================

    #[test]
    fn test_encode_begin() {
        let record = WalRecord::Begin { tx_id: 12345 };
        let encoded = record.encode();

        // Type (1) + Term (8) + TxID (8) + Checksum (4) = 21 bytes
        assert_eq!(encoded.len(), 21);
        assert_eq!(encoded[0], RecordType::Begin as u8);
    }

    #[test]
    fn test_encode_commit() {
        let record = WalRecord::Commit { tx_id: 99999 };
        let encoded = record.encode();

        assert_eq!(encoded.len(), 21);
        assert_eq!(encoded[0], RecordType::Commit as u8);
    }

    #[test]
    fn test_encode_rollback() {
        let record = WalRecord::Rollback { tx_id: 54321 };
        let encoded = record.encode();

        assert_eq!(encoded.len(), 21);
        assert_eq!(encoded[0], RecordType::Rollback as u8);
    }

    #[test]
    fn test_encode_checkpoint() {
        let record = WalRecord::Checkpoint { lsn: 1000000 };
        let encoded = record.encode();

        assert_eq!(encoded.len(), 21);
        assert_eq!(encoded[0], RecordType::Checkpoint as u8);
    }

    #[test]
    fn test_encode_page_write_small() {
        // Small data (< COMPRESS_THRESHOLD) stays uncompressed.
        let data = vec![1, 2, 3, 4, 5];
        let record = WalRecord::PageWrite {
            tx_id: 100,
            page_id: 42,
            data: data.clone(),
        };
        let encoded = record.encode();

        // Type (1) + Term (8) + TxID (8) + PageID (4) + Len (4) + Data (5) + Checksum (4) = 34 bytes
        assert_eq!(encoded.len(), 34);
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

        // Type (1) + Term (8) + TxID (8) + PageID (4) + Len (4) + Checksum (4) = 29 bytes
        assert_eq!(encoded.len(), 29);
    }

    #[test]
    fn test_encode_tx_commit_batch() {
        let record = WalRecord::TxCommitBatch {
            tx_id: 7,
            actions: vec![b"insert".to_vec(), b"update".to_vec()],
        };
        let encoded = record.encode();

        assert_eq!(encoded[0], RecordType::TxCommitBatch as u8);
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
    fn test_read_begin_roundtrip_preserves_term() {
        let original = WalRecord::Begin { tx_id: 42 };
        let encoded = original.encode_with_term(9);

        let mut cursor = Cursor::new(encoded);
        let (term, decoded) = WalRecord::read_with_term(&mut cursor).unwrap().unwrap();

        assert_eq!(term, 9);
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_read_v2_begin_defaults_term() {
        let tx_id = 42u64;
        let mut encoded = Vec::new();
        encoded.push(RecordType::Begin as u8);
        encoded.extend_from_slice(&tx_id.to_le_bytes());
        let checksum = crc32(&encoded);
        encoded.extend_from_slice(&checksum.to_le_bytes());

        let mut cursor = Cursor::new(encoded);
        let (term, decoded) = WalRecord::read_with_format_version(&mut cursor, WAL_VERSION_V2)
            .unwrap()
            .unwrap();

        assert_eq!(term, WAL_DEFAULT_TERM);
        assert_eq!(decoded, WalRecord::Begin { tx_id });
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
    fn test_read_tx_commit_batch_roundtrip() {
        let original = WalRecord::TxCommitBatch {
            tx_id: 42,
            actions: vec![b"old-version".to_vec(), b"new-version".to_vec()],
        };
        let encoded = original.encode();

        let mut cursor = Cursor::new(encoded);
        let decoded = WalRecord::read(&mut cursor).unwrap().unwrap();

        assert_eq!(decoded, original);
    }

    #[test]
    fn test_vector_insert_roundtrip() {
        let original = WalRecord::VectorInsert {
            collection: "turbo".to_string(),
            entity_id: 42,
            vector: vec![1.0, -0.5, 0.25],
        };
        let encoded = original.encode();

        let mut cursor = Cursor::new(encoded);
        let decoded = WalRecord::read(&mut cursor).unwrap().unwrap();

        assert_eq!(decoded, original);
    }

    #[test]
    fn test_read_page_write_large_data() {
        // Large enough to trigger compression.
        let data: Vec<u8> = (0..4096).map(|i| (i % 256) as u8).collect();
        let original = WalRecord::PageWrite {
            tx_id: 1,
            page_id: 0,
            data,
        };
        let encoded = original.encode();

        let mut cursor = Cursor::new(encoded);
        let decoded = WalRecord::read(&mut cursor).unwrap().unwrap();

        // Round-trip: decoded data matches original (even if encoded differently).
        assert_eq!(decoded, original);
    }

    #[test]
    fn page_write_compressed_roundtrip() {
        // Highly compressible payload: 1 KiB of repeated bytes.
        let data = vec![0xABu8; 1024];
        let record = WalRecord::PageWrite {
            tx_id: 7,
            page_id: 3,
            data: data.clone(),
        };
        let encoded = record.encode();

        // Should be stored as PageWriteCompressed (compressible > threshold).
        assert_eq!(encoded[0], RecordType::PageWriteCompressed as u8);

        // And round-trip decoding recovers original.
        let mut cursor = Cursor::new(encoded);
        let decoded = WalRecord::read(&mut cursor).unwrap().unwrap();
        assert_eq!(
            decoded,
            WalRecord::PageWrite {
                tx_id: 7,
                page_id: 3,
                data
            }
        );
    }

    #[test]
    fn full_page_image_roundtrip() {
        let data: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
        let original = WalRecord::FullPageImage {
            tx_id: 11,
            page_id: 9,
            ckpt_epoch: 42,
            data: data.clone(),
        };
        let encoded = original.encode();
        assert_eq!(encoded[0], RecordType::FullPageImage as u8);

        let mut cursor = Cursor::new(encoded);
        let decoded = WalRecord::read(&mut cursor).unwrap().unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn full_page_image_checksum_mismatch_detected() {
        let original = WalRecord::FullPageImage {
            tx_id: 1,
            page_id: 2,
            ckpt_epoch: 3,
            data: vec![0xAA; 32],
        };
        let mut encoded = original.encode();
        let mid = encoded.len() / 2;
        encoded[mid] ^= 0xFF;
        let mut cursor = Cursor::new(encoded);
        assert!(WalRecord::read(&mut cursor).is_err());
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
        assert_eq!(WAL_VERSION, 3);
    }
}
