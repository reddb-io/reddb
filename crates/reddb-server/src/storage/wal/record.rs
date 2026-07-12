use reddb_file::{
    decode_main_wal_record_frame_with_authority, encode_main_wal_record_frame_into,
    encode_main_wal_record_frame_with_authority_into, MainWalRecordFrame, MainWalRecordFrameRef,
    WAL_FILE_VERSION,
};
use std::io::{self, Read};

pub const WAL_DEFAULT_TERM: u64 = crate::replication::DEFAULT_REPLICATION_TERM;

pub use reddb_file::MainWalRecordAuthority as WalRecordAuthority;
pub use reddb_file::MainWalRecordType as RecordType;

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
    /// Logical probabilistic-structure mutation delta. Checkpoints carry
    /// full snapshots through the regular store state; crash recovery
    /// replays these deltas after loading the latest snapshot.
    ProbabilisticDelta {
        kind: u8,
        operation: u8,
        name: String,
        operands: Vec<Vec<u8>>,
    },
    /// Checkpoint marker (indicates up to which LSN pages are flushed)
    Checkpoint { lsn: u64 },
}

impl WalRecord {
    /// Serialize record to bytes (including checksum).
    ///
    /// `PageWrite` compression and physical framing are owned by `reddb-file`.
    pub fn encode(&self) -> Vec<u8> {
        self.encode_with_term(WAL_DEFAULT_TERM)
    }

    /// Serialize record to bytes with the replication term stamped into
    /// the physical record envelope.
    pub fn encode_with_term(&self, term: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        self.encode_with_term_into(&mut buf, term);
        buf
    }

    /// Serialize record into a caller-owned scratch buffer, appending the
    /// encoded bytes (including checksum) to `out`.
    ///
    /// This is the allocation-light entry point for the lock-free append path:
    /// concurrent appenders each encode into their own per-call `out` buffer
    /// *before* taking the WAL lock, so the scratch is never shared across
    /// threads and needs no `thread_local!`. Reusing one `out` across many
    /// records (the commit blob) avoids the fresh `Vec` + copy that
    /// [`encode`](Self::encode) allocates per record. The bytes appended are
    /// byte-identical to `encode()`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        self.encode_with_term_into(out, WAL_DEFAULT_TERM)
    }

    /// Serialize record into a caller-owned scratch buffer with the replication
    /// term stamped into the physical envelope. See [`encode_into`](Self::encode_into).
    ///
    /// The checksum is computed over only the bytes this call appends (the slice
    /// starting at the buffer's prior length), so appending after existing
    /// records leaves them untouched and keeps each record's CRC self-contained.
    ///
    /// The payload is written straight from this record's own buffers — the
    /// append path never clones a page image or a commit batch into an owned
    /// file frame first.
    pub fn encode_with_term_into(&self, out: &mut Vec<u8>, term: u64) {
        encode_main_wal_record_frame_into(self.to_file_frame_ref(), term, out)
            .expect("main WAL record cannot be encoded");
    }

    pub fn encode_with_authority_into(&self, out: &mut Vec<u8>, authority: WalRecordAuthority) {
        encode_main_wal_record_frame_with_authority_into(self.to_file_frame_ref(), authority, out)
            .expect("main WAL record cannot be encoded");
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
        Self::read_with_format_version(reader, WAL_FILE_VERSION)
    }

    /// Read a record and return the term + ownership epoch stamped into
    /// its physical envelope.
    pub fn read_with_authority<R: Read>(
        reader: &mut R,
    ) -> io::Result<Option<(WalRecordAuthority, WalRecord)>> {
        Self::read_with_authority_format_version(reader, WAL_FILE_VERSION)
    }

    pub(crate) fn read_with_format_version<R: Read>(
        reader: &mut R,
        format_version: u8,
    ) -> io::Result<Option<(u64, WalRecord)>> {
        Ok(
            Self::read_with_authority_format_version(reader, format_version)?
                .map(|(authority, record)| (authority.term, record)),
        )
    }

    pub(crate) fn read_with_authority_format_version<R: Read>(
        reader: &mut R,
        format_version: u8,
    ) -> io::Result<Option<(WalRecordAuthority, WalRecord)>> {
        Ok(decode_main_wal_record_frame_with_authority(
            reader,
            format_version,
            WalRecordAuthority {
                term: WAL_DEFAULT_TERM,
                ownership_epoch: None,
            },
        )?
        .map(|(authority, frame)| (authority, WalRecord::from_file_frame(frame))))
    }
}

impl WalRecord {
    /// Borrow this record as a file frame. Every payload is passed through by
    /// reference, so encoding a `PageWrite`, `FullPageImage` or `TxCommitBatch`
    /// costs no copy of the payload.
    fn to_file_frame_ref(&self) -> MainWalRecordFrameRef<'_> {
        match self {
            WalRecord::Begin { tx_id } => MainWalRecordFrameRef::Begin { tx_id: *tx_id },
            WalRecord::Commit { tx_id } => MainWalRecordFrameRef::Commit { tx_id: *tx_id },
            WalRecord::Rollback { tx_id } => MainWalRecordFrameRef::Rollback { tx_id: *tx_id },
            WalRecord::PageWrite {
                tx_id,
                page_id,
                data,
            } => MainWalRecordFrameRef::PageWrite {
                tx_id: *tx_id,
                page_id: *page_id,
                data,
            },
            WalRecord::TxCommitBatch { tx_id, actions } => MainWalRecordFrameRef::TxCommitBatch {
                tx_id: *tx_id,
                actions,
            },
            WalRecord::FullPageImage {
                tx_id,
                page_id,
                ckpt_epoch,
                data,
            } => MainWalRecordFrameRef::FullPageImage {
                tx_id: *tx_id,
                page_id: *page_id,
                ckpt_epoch: *ckpt_epoch,
                data,
            },
            WalRecord::VectorInsert {
                collection,
                entity_id,
                vector,
            } => MainWalRecordFrameRef::VectorInsert {
                collection,
                entity_id: *entity_id,
                vector,
            },
            WalRecord::ProbabilisticDelta {
                kind,
                operation,
                name,
                operands,
            } => MainWalRecordFrameRef::ProbabilisticDelta {
                kind: *kind,
                operation: *operation,
                name,
                operands,
            },
            WalRecord::Checkpoint { lsn } => MainWalRecordFrameRef::Checkpoint { lsn: *lsn },
        }
    }

    /// The pre-#2013 owning conversion, kept as the baseline the borrowed
    /// encode path is proven byte-identical against.
    #[cfg(test)]
    fn to_file_frame(&self) -> MainWalRecordFrame {
        match self {
            WalRecord::Begin { tx_id } => MainWalRecordFrame::Begin { tx_id: *tx_id },
            WalRecord::Commit { tx_id } => MainWalRecordFrame::Commit { tx_id: *tx_id },
            WalRecord::Rollback { tx_id } => MainWalRecordFrame::Rollback { tx_id: *tx_id },
            WalRecord::PageWrite {
                tx_id,
                page_id,
                data,
            } => MainWalRecordFrame::PageWrite {
                tx_id: *tx_id,
                page_id: *page_id,
                data: data.clone(),
            },
            WalRecord::TxCommitBatch { tx_id, actions } => MainWalRecordFrame::TxCommitBatch {
                tx_id: *tx_id,
                actions: actions.clone(),
            },
            WalRecord::FullPageImage {
                tx_id,
                page_id,
                ckpt_epoch,
                data,
            } => MainWalRecordFrame::FullPageImage {
                tx_id: *tx_id,
                page_id: *page_id,
                ckpt_epoch: *ckpt_epoch,
                data: data.clone(),
            },
            WalRecord::VectorInsert {
                collection,
                entity_id,
                vector,
            } => MainWalRecordFrame::VectorInsert {
                collection: collection.clone(),
                entity_id: *entity_id,
                vector: vector.clone(),
            },
            WalRecord::ProbabilisticDelta {
                kind,
                operation,
                name,
                operands,
            } => MainWalRecordFrame::ProbabilisticDelta {
                kind: *kind,
                operation: *operation,
                name: name.clone(),
                operands: operands.clone(),
            },
            WalRecord::Checkpoint { lsn } => MainWalRecordFrame::Checkpoint { lsn: *lsn },
        }
    }

    fn from_file_frame(frame: MainWalRecordFrame) -> Self {
        match frame {
            MainWalRecordFrame::Begin { tx_id } => WalRecord::Begin { tx_id },
            MainWalRecordFrame::Commit { tx_id } => WalRecord::Commit { tx_id },
            MainWalRecordFrame::Rollback { tx_id } => WalRecord::Rollback { tx_id },
            MainWalRecordFrame::PageWrite {
                tx_id,
                page_id,
                data,
            } => WalRecord::PageWrite {
                tx_id,
                page_id,
                data,
            },
            MainWalRecordFrame::TxCommitBatch { tx_id, actions } => {
                WalRecord::TxCommitBatch { tx_id, actions }
            }
            MainWalRecordFrame::FullPageImage {
                tx_id,
                page_id,
                ckpt_epoch,
                data,
            } => WalRecord::FullPageImage {
                tx_id,
                page_id,
                ckpt_epoch,
                data,
            },
            MainWalRecordFrame::VectorInsert {
                collection,
                entity_id,
                vector,
            } => WalRecord::VectorInsert {
                collection,
                entity_id,
                vector,
            },
            MainWalRecordFrame::ProbabilisticDelta {
                kind,
                operation,
                name,
                operands,
            } => WalRecord::ProbabilisticDelta {
                kind,
                operation,
                name,
                operands,
            },
            MainWalRecordFrame::Checkpoint { lsn } => WalRecord::Checkpoint { lsn },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ==================== WalRecord::encode Tests ====================

    #[test]
    fn test_encode_begin() {
        let record = WalRecord::Begin { tx_id: 12345 };
        let encoded = record.encode();
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_encode_commit() {
        let record = WalRecord::Commit { tx_id: 99999 };
        let encoded = record.encode();
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_encode_rollback() {
        let record = WalRecord::Rollback { tx_id: 54321 };
        let encoded = record.encode();
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_encode_checkpoint() {
        let record = WalRecord::Checkpoint { lsn: 1000000 };
        let encoded = record.encode();
        assert!(!encoded.is_empty());
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
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_encode_page_write_empty_data() {
        let record = WalRecord::PageWrite {
            tx_id: 1,
            page_id: 0,
            data: vec![],
        };
        let encoded = record.encode();
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_encode_tx_commit_batch() {
        let record = WalRecord::TxCommitBatch {
            tx_id: 7,
            actions: vec![b"insert".to_vec(), b"update".to_vec()],
        };
        let encoded = record.encode();
        assert!(!encoded.is_empty());
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
    fn test_read_begin_roundtrip_preserves_authority_epoch() {
        let original = WalRecord::Begin { tx_id: 42 };
        let authority = WalRecordAuthority {
            term: 9,
            ownership_epoch: Some(12),
        };
        let mut encoded = Vec::new();
        original.encode_with_authority_into(&mut encoded, authority);

        let mut cursor = Cursor::new(encoded);
        let (decoded_authority, decoded) = WalRecord::read_with_authority(&mut cursor)
            .unwrap()
            .unwrap();

        assert_eq!(decoded_authority, authority);
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

    // ==================== encode_into scratch-buffer Tests ====================

    /// `encode_into` appended to a fresh scratch must be byte-identical to the
    /// per-allocation `encode()` baseline, for every record variant.
    #[test]
    fn test_encode_into_matches_encode_for_all_variants() {
        let records = vec![
            WalRecord::Begin { tx_id: 12345 },
            WalRecord::Commit { tx_id: 99999 },
            WalRecord::Rollback { tx_id: 54321 },
            WalRecord::Checkpoint { lsn: 1_000_000 },
            WalRecord::PageWrite {
                tx_id: 100,
                page_id: 42,
                data: vec![1, 2, 3, 4, 5],
            },
            // Large, highly compressible payload → exercises the
            // PageWriteCompressed early-return branch.
            WalRecord::PageWrite {
                tx_id: 7,
                page_id: 3,
                data: vec![0xABu8; 1024],
            },
            WalRecord::TxCommitBatch {
                tx_id: 7,
                actions: vec![b"insert".to_vec(), b"update".to_vec()],
            },
            WalRecord::FullPageImage {
                tx_id: 11,
                page_id: 9,
                ckpt_epoch: 42,
                data: (0..4096).map(|i| (i % 251) as u8).collect(),
            },
            WalRecord::VectorInsert {
                collection: "turbo".to_string(),
                entity_id: 42,
                vector: vec![1.0, -0.5, 0.25],
            },
        ];

        for record in &records {
            let baseline = record.encode();
            let mut scratch = Vec::new();
            record.encode_into(&mut scratch);
            assert_eq!(scratch, baseline, "encode_into mismatch for {record:?}");
        }
    }

    /// Reusing one scratch buffer across several records yields exactly the
    /// concatenation of the per-record `encode()` baselines — proving the
    /// checksum is computed over each record's own slice, not the whole buffer.
    #[test]
    fn test_encode_into_reuses_scratch_across_records() {
        let records = vec![
            WalRecord::Begin { tx_id: 1 },
            WalRecord::PageWrite {
                tx_id: 1,
                page_id: 10,
                data: vec![1, 2, 3],
            },
            WalRecord::Commit { tx_id: 1 },
        ];

        let mut expected = Vec::new();
        for r in &records {
            expected.extend_from_slice(&r.encode());
        }

        // One scratch, reused for every record — no per-record allocation.
        let mut scratch = Vec::new();
        for r in &records {
            r.encode_into(&mut scratch);
        }

        assert_eq!(scratch, expected);

        // And the concatenation round-trips back to the original records.
        let mut cursor = Cursor::new(scratch);
        for expected in &records {
            let decoded = WalRecord::read(&mut cursor).unwrap().unwrap();
            assert_eq!(&decoded, expected);
        }
        assert!(WalRecord::read(&mut cursor).unwrap().is_none());
    }

    /// Every record variant, including empty/large/compressible payloads and
    /// the edge cases each variant can carry.
    fn every_record_variant() -> Vec<WalRecord> {
        vec![
            WalRecord::Begin { tx_id: 12345 },
            WalRecord::Commit { tx_id: 99999 },
            WalRecord::Rollback { tx_id: 54321 },
            WalRecord::Checkpoint { lsn: 1_000_000 },
            // Below the compression threshold → literal PageWrite frame.
            WalRecord::PageWrite {
                tx_id: 100,
                page_id: 42,
                data: vec![1, 2, 3, 4, 5],
            },
            WalRecord::PageWrite {
                tx_id: 1,
                page_id: 0,
                data: Vec::new(),
            },
            // Compressible and above the threshold → PageWriteCompressed frame.
            WalRecord::PageWrite {
                tx_id: 7,
                page_id: 3,
                data: vec![0xABu8; 1024],
            },
            // Above the threshold but incompressible → stays a literal PageWrite.
            WalRecord::PageWrite {
                tx_id: 8,
                page_id: 4,
                data: (0..300u32)
                    .map(|i| i.wrapping_mul(2_654_435_761) as u8)
                    .collect(),
            },
            WalRecord::TxCommitBatch {
                tx_id: 7,
                actions: vec![b"insert".to_vec(), Vec::new(), b"update".to_vec()],
            },
            WalRecord::TxCommitBatch {
                tx_id: 8,
                actions: Vec::new(),
            },
            WalRecord::FullPageImage {
                tx_id: 11,
                page_id: 9,
                ckpt_epoch: 42,
                data: (0..4096).map(|i| (i % 251) as u8).collect(),
            },
            WalRecord::VectorInsert {
                collection: "turbo".to_string(),
                entity_id: 42,
                vector: vec![1.0, -0.5, 0.25],
            },
            WalRecord::ProbabilisticDelta {
                kind: 1,
                operation: 2,
                name: "visitors".to_string(),
                operands: vec![b"alice".to_vec(), b"bob".to_vec()],
            },
        ]
    }

    /// The borrowed encode path (#2013) must persist *exactly* the bytes the
    /// pre-#2013 owning path persisted, for every record variant and for both
    /// the term-only and the full-authority envelope. The on-disk format is
    /// frozen; only the in-memory representation changed.
    #[test]
    fn borrowed_encode_is_byte_identical_to_the_owning_frame_path() {
        for record in every_record_variant() {
            // Old path: clone the payload into an owned frame, then encode.
            let mut owned_bytes = Vec::new();
            encode_main_wal_record_frame_into(&record.to_file_frame(), 17, &mut owned_bytes)
                .expect("owning encode");

            // New path: encode straight from the borrowed record.
            let mut borrowed_bytes = Vec::new();
            record.encode_with_term_into(&mut borrowed_bytes, 17);

            assert_eq!(
                borrowed_bytes, owned_bytes,
                "borrowed encode diverged from the owning baseline for {record:?}"
            );

            // Same again through the authority envelope (with an ownership epoch).
            let authority = WalRecordAuthority {
                term: 17,
                ownership_epoch: Some(5),
            };
            let mut owned_auth = Vec::new();
            encode_main_wal_record_frame_with_authority_into(
                &record.to_file_frame(),
                authority,
                &mut owned_auth,
            )
            .expect("owning authority encode");

            let mut borrowed_auth = Vec::new();
            record.encode_with_authority_into(&mut borrowed_auth, authority);

            assert_eq!(
                borrowed_auth, owned_auth,
                "borrowed authority encode diverged from the owning baseline for {record:?}"
            );

            // And the persisted bytes still decode back to the original record.
            let mut cursor = Cursor::new(borrowed_bytes);
            let (term, decoded) = WalRecord::read_with_term(&mut cursor).unwrap().unwrap();
            assert_eq!(term, 17);
            assert_eq!(decoded, record);
        }
    }

    /// `encode_with_term_into` honours the term and matches the allocating
    /// `encode_with_term` baseline even when appended after existing bytes.
    #[test]
    fn test_encode_with_term_into_matches_and_preserves_prefix() {
        let prefix = b"PREFIX-BYTES".to_vec();
        let record = WalRecord::Begin { tx_id: 42 };

        let mut scratch = prefix.clone();
        record.encode_with_term_into(&mut scratch, 9);

        // Prefix untouched; suffix equals the allocating baseline.
        assert_eq!(&scratch[..prefix.len()], &prefix[..]);
        assert_eq!(&scratch[prefix.len()..], &record.encode_with_term(9)[..]);
    }
}
