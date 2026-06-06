//! Strict append-only segment V0.
//!
//! This is the first closed-segment contract for `APPEND ONLY`
//! collections. It deliberately models the immutable segment boundary
//! without taking over the general unified write path yet: appends land in
//! fixed-size chunks, close freezes bytes plus metadata, and reads verify
//! chunk checksums stored outside the chunk payload.

use std::collections::{BTreeMap, HashMap};

use crate::storage::engine::crc32::crc32;
use crate::storage::schema::{value_to_canonical_key, CanonicalKey, Value};

pub const APPEND_ONLY_SEGMENT_CHUNK_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendOnlySegmentError {
    Closed,
    DuplicatePrimaryKey(String),
    RecordTooLarge {
        len: usize,
        max: usize,
    },
    MissingChunkChecksum {
        chunk_index: usize,
    },
    ChecksumMismatch {
        chunk_index: usize,
        expected: u32,
        actual: u32,
    },
    MissingPrimaryKey(String),
    UpdateRejected,
    DeleteRejected,
}

impl std::fmt::Display for AppendOnlySegmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "append-only segment is closed and immutable"),
            Self::DuplicatePrimaryKey(pk) => {
                write!(f, "duplicate primary key in append-only segment: {pk}")
            }
            Self::RecordTooLarge { len, max } => {
                write!(
                    f,
                    "append-only record is too large: {len} bytes exceeds {max}"
                )
            }
            Self::MissingChunkChecksum { chunk_index } => {
                write!(
                    f,
                    "append-only segment metadata missing checksum for chunk {chunk_index}"
                )
            }
            Self::ChecksumMismatch {
                chunk_index,
                expected,
                actual,
            } => write!(
                f,
                "append-only segment checksum mismatch for chunk {chunk_index}: \
                 expected {expected:#010x}, got {actual:#010x}"
            ),
            Self::MissingPrimaryKey(pk) => write!(f, "primary key not found in segment: {pk}"),
            Self::UpdateRejected => {
                write!(f, "APPEND ONLY segment rejects logical UPDATE in V0")
            }
            Self::DeleteRejected => {
                write!(f, "APPEND ONLY segment rejects logical DELETE in V0")
            }
        }
    }
}

impl std::error::Error for AppendOnlySegmentError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentOffset {
    pub chunk_index: usize,
    pub offset: usize,
    pub len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMinMax {
    pub min: Value,
    pub max: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendOnlySegmentMetadata {
    pub collection: String,
    pub segment_id: u64,
    pub chunk_size: usize,
    pub row_count: usize,
    pub chunk_checksums: Vec<u32>,
    pub primary_index: BTreeMap<String, SegmentOffset>,
    pub column_min_max: BTreeMap<String, ColumnMinMax>,
}

#[derive(Debug, Clone)]
struct PendingRecord {
    primary_key: String,
    offset: SegmentOffset,
    stats: Vec<(String, Value)>,
}

#[derive(Debug, Clone)]
pub struct AppendOnlySegment {
    collection: String,
    segment_id: u64,
    chunks: Vec<Vec<u8>>,
    pending_records: Vec<PendingRecord>,
    closed: Option<AppendOnlySegmentMetadata>,
}

impl AppendOnlySegment {
    pub fn new(segment_id: u64, collection: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
            segment_id,
            chunks: vec![Vec::with_capacity(APPEND_ONLY_SEGMENT_CHUNK_BYTES)],
            pending_records: Vec::new(),
            closed: None,
        }
    }

    pub fn append(
        &mut self,
        primary_key: impl Into<String>,
        record: &[u8],
        stats: impl IntoIterator<Item = (String, Value)>,
    ) -> Result<SegmentOffset, AppendOnlySegmentError> {
        if self.closed.is_some() {
            return Err(AppendOnlySegmentError::Closed);
        }
        if record.len() > APPEND_ONLY_SEGMENT_CHUNK_BYTES {
            return Err(AppendOnlySegmentError::RecordTooLarge {
                len: record.len(),
                max: APPEND_ONLY_SEGMENT_CHUNK_BYTES,
            });
        }

        let primary_key = primary_key.into();
        if self
            .pending_records
            .iter()
            .any(|record| record.primary_key == primary_key)
        {
            return Err(AppendOnlySegmentError::DuplicatePrimaryKey(primary_key));
        }

        let needs_new_chunk = self
            .chunks
            .last()
            .is_some_and(|chunk| chunk.len() + record.len() > APPEND_ONLY_SEGMENT_CHUNK_BYTES);
        if needs_new_chunk {
            self.chunks
                .push(Vec::with_capacity(APPEND_ONLY_SEGMENT_CHUNK_BYTES));
        }

        let chunk_index = self.chunks.len() - 1;
        let chunk = self.chunks.last_mut().expect("segment always has a chunk");
        let offset = SegmentOffset {
            chunk_index,
            offset: chunk.len(),
            len: record.len(),
        };
        chunk.extend_from_slice(record);
        self.pending_records.push(PendingRecord {
            primary_key,
            offset: offset.clone(),
            stats: stats.into_iter().collect(),
        });
        Ok(offset)
    }

    pub fn close(&mut self) -> Result<&AppendOnlySegmentMetadata, AppendOnlySegmentError> {
        if self.closed.is_none() {
            let chunk_checksums = self.chunks.iter().map(|chunk| crc32(chunk)).collect();
            let mut primary_index = BTreeMap::new();
            let mut column_min_max: HashMap<String, (CanonicalKey, Value, CanonicalKey, Value)> =
                HashMap::new();

            for record in &self.pending_records {
                if primary_index
                    .insert(record.primary_key.clone(), record.offset.clone())
                    .is_some()
                {
                    return Err(AppendOnlySegmentError::DuplicatePrimaryKey(
                        record.primary_key.clone(),
                    ));
                }

                for (column, value) in &record.stats {
                    let Some(key) = value_to_canonical_key(value) else {
                        continue;
                    };
                    column_min_max
                        .entry(column.clone())
                        .and_modify(|(min_key, min_value, max_key, max_value)| {
                            if key < *min_key {
                                *min_key = key.clone();
                                *min_value = value.clone();
                            }
                            if key > *max_key {
                                *max_key = key.clone();
                                *max_value = value.clone();
                            }
                        })
                        .or_insert_with(|| (key.clone(), value.clone(), key, value.clone()));
                }
            }

            let column_min_max = column_min_max
                .into_iter()
                .map(|(column, (_min_key, min, _max_key, max))| (column, ColumnMinMax { min, max }))
                .collect();

            self.closed = Some(AppendOnlySegmentMetadata {
                collection: self.collection.clone(),
                segment_id: self.segment_id,
                chunk_size: APPEND_ONLY_SEGMENT_CHUNK_BYTES,
                row_count: self.pending_records.len(),
                chunk_checksums,
                primary_index,
                column_min_max,
            });
        }
        Ok(self.closed.as_ref().expect("metadata just initialized"))
    }

    pub fn metadata(&self) -> Option<&AppendOnlySegmentMetadata> {
        self.closed.as_ref()
    }

    pub fn chunks(&self) -> &[Vec<u8>] {
        &self.chunks
    }

    pub fn read_by_primary_key(&self, primary_key: &str) -> Result<&[u8], AppendOnlySegmentError> {
        let metadata = self.closed.as_ref().ok_or(AppendOnlySegmentError::Closed)?;
        self.validate_checksums(metadata)?;
        let offset = metadata
            .primary_index
            .get(primary_key)
            .ok_or_else(|| AppendOnlySegmentError::MissingPrimaryKey(primary_key.to_string()))?;
        Ok(&self.chunks[offset.chunk_index][offset.offset..offset.offset + offset.len])
    }

    pub fn validate_checksums(
        &self,
        metadata: &AppendOnlySegmentMetadata,
    ) -> Result<(), AppendOnlySegmentError> {
        for (chunk_index, chunk) in self.chunks.iter().enumerate() {
            let expected = metadata
                .chunk_checksums
                .get(chunk_index)
                .copied()
                .ok_or(AppendOnlySegmentError::MissingChunkChecksum { chunk_index })?;
            let actual = crc32(chunk);
            if actual != expected {
                return Err(AppendOnlySegmentError::ChecksumMismatch {
                    chunk_index,
                    expected,
                    actual,
                });
            }
        }
        Ok(())
    }

    pub fn corrupt_chunk_for_test(&mut self, chunk_index: usize, offset: usize, byte: u8) {
        if let Some(chunk) = self.chunks.get_mut(chunk_index) {
            if let Some(slot) = chunk.get_mut(offset) {
                *slot = byte;
            }
        }
    }

    pub fn update_logical(&mut self, _primary_key: &str) -> Result<(), AppendOnlySegmentError> {
        Err(AppendOnlySegmentError::UpdateRejected)
    }

    pub fn delete_logical(&mut self, _primary_key: &str) -> Result<(), AppendOnlySegmentError> {
        Err(AppendOnlySegmentError::DeleteRejected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_close_and_read_uses_closed_metadata() {
        let mut segment = AppendOnlySegment::new(7, "audit_log");

        segment
            .append(
                "pk-1",
                b"{\"id\":1,\"ts\":10}",
                [
                    ("id".to_string(), Value::Integer(1)),
                    ("ts".to_string(), Value::Integer(10)),
                ],
            )
            .expect("append first record");
        segment
            .append(
                "pk-2",
                b"{\"id\":2,\"ts\":20}",
                [
                    ("id".to_string(), Value::Integer(2)),
                    ("ts".to_string(), Value::Integer(20)),
                ],
            )
            .expect("append second record");

        let metadata = segment.close().expect("close segment").clone();

        assert_eq!(metadata.collection, "audit_log");
        assert_eq!(metadata.segment_id, 7);
        assert_eq!(metadata.chunk_size, APPEND_ONLY_SEGMENT_CHUNK_BYTES);
        assert_eq!(metadata.row_count, 2);
        assert_eq!(metadata.primary_index.len(), 2);
        assert_eq!(metadata.chunk_checksums.len(), segment.chunks().len());
        assert_eq!(
            segment.read_by_primary_key("pk-2").expect("read pk-2"),
            b"{\"id\":2,\"ts\":20}"
        );
    }

    #[test]
    fn chunks_are_fixed_at_512_kib_and_checksums_live_in_metadata() {
        let mut segment = AppendOnlySegment::new(8, "events");
        let almost_full = vec![b'a'; APPEND_ONLY_SEGMENT_CHUNK_BYTES - 8];
        let second = vec![b'b'; 16];

        let first_offset = segment
            .append("a", &almost_full, [("id".to_string(), Value::Integer(1))])
            .expect("append first");
        let second_offset = segment
            .append("b", &second, [("id".to_string(), Value::Integer(2))])
            .expect("append second");
        let metadata = segment.close().expect("close").clone();

        assert_eq!(first_offset.chunk_index, 0);
        assert_eq!(second_offset.chunk_index, 1);
        assert_eq!(metadata.chunk_size, 512 * 1024);
        assert_eq!(
            segment.chunks()[0].len(),
            APPEND_ONLY_SEGMENT_CHUNK_BYTES - 8
        );
        assert_eq!(segment.chunks()[1].len(), 16);
        assert_eq!(metadata.chunk_checksums.len(), 2);
        segment
            .validate_checksums(&metadata)
            .expect("metadata checksums validate chunks");
    }

    #[test]
    fn closed_segment_is_immutable() {
        let mut segment = AppendOnlySegment::new(9, "ledger");
        segment
            .append("pk-1", b"one", [("id".to_string(), Value::Integer(1))])
            .expect("append");
        segment.close().expect("close");

        let err = segment
            .append("pk-2", b"two", [("id".to_string(), Value::Integer(2))])
            .expect_err("closed segment rejects append");
        assert_eq!(err, AppendOnlySegmentError::Closed);
    }

    #[test]
    fn read_detects_chunk_checksum_failure() {
        let mut segment = AppendOnlySegment::new(10, "audit_log");
        segment
            .append("pk-1", b"stable", [("id".to_string(), Value::Integer(1))])
            .expect("append");
        segment.close().expect("close");
        segment.corrupt_chunk_for_test(0, 0, b'X');

        let err = segment
            .read_by_primary_key("pk-1")
            .expect_err("read must verify chunk checksum");
        assert!(
            matches!(
                err,
                AppendOnlySegmentError::ChecksumMismatch { chunk_index: 0, .. }
            ),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn closed_metadata_contains_pruning_min_max_statistics() {
        let mut segment = AppendOnlySegment::new(11, "metrics");
        segment
            .append(
                "pk-1",
                b"one",
                [
                    ("id".to_string(), Value::Integer(10)),
                    ("tenant".to_string(), Value::text("b")),
                ],
            )
            .expect("append one");
        segment
            .append(
                "pk-2",
                b"two",
                [
                    ("id".to_string(), Value::Integer(2)),
                    ("tenant".to_string(), Value::text("a")),
                ],
            )
            .expect("append two");
        segment
            .append(
                "pk-3",
                b"three",
                [
                    ("id".to_string(), Value::Integer(30)),
                    ("tenant".to_string(), Value::text("c")),
                ],
            )
            .expect("append three");

        let metadata = segment.close().expect("close").clone();
        let id = metadata.column_min_max.get("id").expect("id stats");
        let tenant = metadata.column_min_max.get("tenant").expect("tenant stats");

        assert_eq!(id.min, Value::Integer(2));
        assert_eq!(id.max, Value::Integer(30));
        assert_eq!(tenant.min, Value::text("a"));
        assert_eq!(tenant.max, Value::text("c"));
    }

    #[test]
    fn strict_append_only_rejects_logical_update_and_delete() {
        let mut segment = AppendOnlySegment::new(12, "audit_log");
        segment
            .append("pk-1", b"one", [("id".to_string(), Value::Integer(1))])
            .expect("append");

        let update = segment.update_logical("pk-1").expect_err("update rejected");
        let delete = segment.delete_logical("pk-1").expect_err("delete rejected");

        assert_eq!(update, AppendOnlySegmentError::UpdateRejected);
        assert_eq!(delete, AppendOnlySegmentError::DeleteRejected);
    }
}
