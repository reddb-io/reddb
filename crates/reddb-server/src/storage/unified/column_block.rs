//! Sealed hypertable chunk — columnar on-disk layout v1 (`RDCC`).
//!
//! This is the **physical columnar form of a sealed hypertable chunk**,
//! not a parallel object: PRD #850 rejects a standalone "columnar
//! segment" as a bloat vector. The sealed chunk *is* the columnar
//! segment, and this module defines its durable byte contract — emitted
//! as a [`PageType::ColumnBlock`](crate::storage::engine::PageType) page.
//!
//! It is the first real caller of [`segment_codec`](super::segment_codec):
//! every column stream is one `segment_codec` pipeline output. The codec
//! chain is chosen **per column** from its [`ColumnSemantics`] via
//! [`select_codecs`](super::segment_codec::select_codecs) (#853) — the
//! directory's `codec` byte records the leading (semantic) codec so the
//! sealed chunk is self-describing; granule skip-indexes are #854 and
//! per-granule blooms are #855. The directory reserves zeroed fields for
//! both so those slices extend within this envelope without forcing a
//! format v2.
//!
//! # Layout (`RDCC`)
//!
//! ```text
//! Header (52 bytes)
//!   magic            b"RDCC"   (4)
//!   format_version   u16  = 1
//!   flags            u16  = 0           reserved
//!   chunk_id         u64
//!   schema_ref       u64                catalog schema id column_ids resolve against
//!   row_count        u64
//!   column_count     u32
//!   min_ts_ns        u64                mirror ChunkMeta → self-describing read
//!   max_ts_ns        u64
//!
//! Column directory   (column_count entries, 54 bytes each, at offset 52)
//!   column_id            u32
//!   logical_type         u8             Value type tag (DataType::to_byte)
//!   codec                u8             ColumnCodec tag (segment_codec)
//!   stream_offset        u64            byte offset of this column's stream within the block
//!   stream_len           u64
//!   granule_index_off    u64  = 0       reserved → #854
//!   granule_index_len    u64  = 0
//!   bloom_off            u64  = 0       reserved → #855
//!   bloom_len            u64  = 0
//!
//! Column streams        column_count segment_codec runs, back-to-back
//!
//! Footer (24 bytes)
//!   col_directory_off    u64
//!   col_directory_len    u64
//!   crc32                u32            over header+directory+streams
//!   magic_tail           b"RDCC"  (4)
//! ```

use super::segment_codec::{
    decode_bytes, encode_bytes, select_codecs, CodecError, ColumnCodec, ColumnSemantics,
};
use crate::storage::engine::crc32::crc32;

/// `b"RDCC"` — RedDB Columnar Chunk. Opens and closes every block.
pub const COLUMN_BLOCK_MAGIC: [u8; 4] = *b"RDCC";
/// On-disk format version. Bumped only on a breaking layout change;
/// #853–#856 extend the reserved directory fields without a bump.
pub const COLUMN_BLOCK_VERSION_V1: u16 = 1;

const HEADER_LEN: usize = 52;
const DIR_ENTRY_LEN: usize = 54;
const FOOTER_LEN: usize = 24;

/// A logical column handed to [`write_column_block`]. `data` is the
/// column's raw, uncompressed, little-endian value bytes; the writer runs
/// it through the `segment_codec` pipeline to produce the stored stream.
#[derive(Debug, Clone)]
pub struct ColumnInput<'a> {
    /// Stable column id resolved against `schema_ref`.
    pub column_id: u32,
    /// Logical type tag (`DataType::to_byte()`).
    pub logical_type: u8,
    /// The column's role — drives per-column codec selection (#853).
    /// Callers that have no semantic hint pass [`ColumnSemantics::Generic`].
    pub semantics: ColumnSemantics,
    /// Raw little-endian column bytes (pre-compression).
    pub data: &'a [u8],
}

/// One decoded column produced by [`read_column_block`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedColumn {
    pub column_id: u32,
    pub logical_type: u8,
    /// Codec tag recorded in the directory for this column.
    pub codec_tag: u8,
    /// Decompressed raw little-endian column bytes — identical to the
    /// `ColumnInput::data` the writer was given.
    pub data: Vec<u8>,
}

/// A fully decoded column block: the self-describing header plus every
/// column's raw bytes, ready to transpose back into rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedColumnBlock {
    pub chunk_id: u64,
    pub schema_ref: u64,
    pub row_count: u64,
    pub min_ts_ns: u64,
    pub max_ts_ns: u64,
    pub columns: Vec<DecodedColumn>,
}

/// Failures decoding (or, rarely, encoding) a column block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnBlockError {
    /// Buffer shorter than the fixed header/footer it must contain.
    Truncated,
    /// Leading magic was not `RDCC`.
    BadMagic([u8; 4]),
    /// Trailing magic was not `RDCC` (block was clipped or corrupt).
    BadTailMagic([u8; 4]),
    /// `format_version` is newer than this build understands.
    UnsupportedVersion(u16),
    /// A directory entry points outside the block's byte range.
    BadDirectory,
    /// CRC32 over header+directory+streams did not match the footer.
    ChecksumMismatch { expected: u32, actual: u32 },
    /// A column stream failed to decode through `segment_codec`.
    Codec(CodecError),
}

impl std::fmt::Display for ColumnBlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "column block truncated"),
            Self::BadMagic(m) => write!(f, "bad column block magic: {m:?}"),
            Self::BadTailMagic(m) => write!(f, "bad column block tail magic: {m:?}"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported column block version: {v}"),
            Self::BadDirectory => write!(f, "column directory entry out of range"),
            Self::ChecksumMismatch { expected, actual } => write!(
                f,
                "column block checksum mismatch: expected 0x{expected:08X}, got 0x{actual:08X}"
            ),
            Self::Codec(e) => write!(f, "column stream codec error: {e}"),
        }
    }
}

impl std::error::Error for ColumnBlockError {}

impl From<CodecError> for ColumnBlockError {
    fn from(e: CodecError) -> Self {
        Self::Codec(e)
    }
}

/// Serialize `columns` into the v1 `RDCC` layout. Each column's raw bytes
/// run through the codec chain [`select_codecs`] picks from its
/// [`ColumnSemantics`]; the directory records the *leading* (semantic)
/// codec tag. The reader decodes from each stream's own self-describing
/// header, so #853 changed only write-time selection, never the read path.
pub fn write_column_block(
    chunk_id: u64,
    schema_ref: u64,
    row_count: u64,
    min_ts_ns: u64,
    max_ts_ns: u64,
    columns: &[ColumnInput<'_>],
) -> Result<Vec<u8>, ColumnBlockError> {
    let column_count = columns.len();
    let dir_off = HEADER_LEN;
    let dir_len = column_count * DIR_ENTRY_LEN;
    let streams_off = dir_off + dir_len;

    // Encode every column stream first so we know each length/offset.
    // The codec chain is chosen per column from its semantics; the tag we
    // record in the directory is the *leading* (semantic) codec — the one
    // that characterises the column. An empty chain (never produced by
    // `select_codecs`) records `None`.
    let mut streams: Vec<Vec<u8>> = Vec::with_capacity(column_count);
    let mut codec_tags: Vec<u8> = Vec::with_capacity(column_count);
    for col in columns {
        let codecs = select_codecs(col.logical_type, col.semantics);
        codec_tags.push(codecs.first().unwrap_or(&ColumnCodec::None).tag());
        streams.push(encode_bytes(&codecs, col.data)?);
    }

    let mut out = Vec::with_capacity(streams_off + streams.iter().map(Vec::len).sum::<usize>());

    // --- Header ---
    out.extend_from_slice(&COLUMN_BLOCK_MAGIC);
    out.extend_from_slice(&COLUMN_BLOCK_VERSION_V1.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // flags (reserved)
    out.extend_from_slice(&chunk_id.to_le_bytes());
    out.extend_from_slice(&schema_ref.to_le_bytes());
    out.extend_from_slice(&row_count.to_le_bytes());
    out.extend_from_slice(&(column_count as u32).to_le_bytes());
    out.extend_from_slice(&min_ts_ns.to_le_bytes());
    out.extend_from_slice(&max_ts_ns.to_le_bytes());
    debug_assert_eq!(out.len(), HEADER_LEN);

    // --- Column directory ---
    let mut cursor = streams_off as u64;
    for ((col, stream), codec_tag) in columns.iter().zip(streams.iter()).zip(codec_tags.iter()) {
        out.extend_from_slice(&col.column_id.to_le_bytes());
        out.push(col.logical_type);
        out.push(*codec_tag);
        out.extend_from_slice(&cursor.to_le_bytes()); // stream_offset
        out.extend_from_slice(&(stream.len() as u64).to_le_bytes()); // stream_len
        out.extend_from_slice(&0u64.to_le_bytes()); // granule_index_off (reserved #854)
        out.extend_from_slice(&0u64.to_le_bytes()); // granule_index_len
        out.extend_from_slice(&0u64.to_le_bytes()); // bloom_off (reserved #855)
        out.extend_from_slice(&0u64.to_le_bytes()); // bloom_len
        cursor += stream.len() as u64;
    }
    debug_assert_eq!(out.len(), streams_off);

    // --- Column streams ---
    for stream in &streams {
        out.extend_from_slice(stream);
    }

    // --- Footer ---
    let crc = crc32(&out); // over header+directory+streams
    out.extend_from_slice(&(dir_off as u64).to_le_bytes());
    out.extend_from_slice(&(dir_len as u64).to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&COLUMN_BLOCK_MAGIC);

    Ok(out)
}

/// Decode a v1 `RDCC` block: verify both magics, the version, and the
/// CRC, then decode each column stream back to its raw bytes.
pub fn read_column_block(bytes: &[u8]) -> Result<DecodedColumnBlock, ColumnBlockError> {
    if bytes.len() < HEADER_LEN + FOOTER_LEN {
        return Err(ColumnBlockError::Truncated);
    }
    let magic: [u8; 4] = bytes[0..4].try_into().unwrap();
    if magic != COLUMN_BLOCK_MAGIC {
        return Err(ColumnBlockError::BadMagic(magic));
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != COLUMN_BLOCK_VERSION_V1 {
        return Err(ColumnBlockError::UnsupportedVersion(version));
    }

    // --- Footer (fixed, at the tail) ---
    let footer_start = bytes.len() - FOOTER_LEN;
    let tail_magic: [u8; 4] = bytes[bytes.len() - 4..].try_into().unwrap();
    if tail_magic != COLUMN_BLOCK_MAGIC {
        return Err(ColumnBlockError::BadTailMagic(tail_magic));
    }
    let dir_off = u64::from_le_bytes(bytes[footer_start..footer_start + 8].try_into().unwrap());
    let dir_len = u64::from_le_bytes(
        bytes[footer_start + 8..footer_start + 16]
            .try_into()
            .unwrap(),
    );
    let stored_crc = u32::from_le_bytes(
        bytes[footer_start + 16..footer_start + 20]
            .try_into()
            .unwrap(),
    );
    let actual_crc = crc32(&bytes[..footer_start]);
    if actual_crc != stored_crc {
        return Err(ColumnBlockError::ChecksumMismatch {
            expected: stored_crc,
            actual: actual_crc,
        });
    }

    // --- Header fields needed for reconstruction ---
    let chunk_id = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    let schema_ref = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
    let row_count = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
    let column_count = u32::from_le_bytes(bytes[32..36].try_into().unwrap()) as usize;
    let min_ts_ns = u64::from_le_bytes(bytes[36..44].try_into().unwrap());
    let max_ts_ns = u64::from_le_bytes(bytes[44..52].try_into().unwrap());

    let dir_off = dir_off as usize;
    let dir_len = dir_len as usize;
    if dir_off != HEADER_LEN
        || dir_len != column_count * DIR_ENTRY_LEN
        || dir_off + dir_len > footer_start
    {
        return Err(ColumnBlockError::BadDirectory);
    }

    let mut columns = Vec::with_capacity(column_count);
    for i in 0..column_count {
        let base = dir_off + i * DIR_ENTRY_LEN;
        let column_id = u32::from_le_bytes(bytes[base..base + 4].try_into().unwrap());
        let logical_type = bytes[base + 4];
        let codec_tag = bytes[base + 5];
        let stream_offset =
            u64::from_le_bytes(bytes[base + 6..base + 14].try_into().unwrap()) as usize;
        let stream_len =
            u64::from_le_bytes(bytes[base + 14..base + 22].try_into().unwrap()) as usize;
        let end = stream_offset
            .checked_add(stream_len)
            .ok_or(ColumnBlockError::BadDirectory)?;
        if stream_offset < dir_off + dir_len || end > footer_start {
            return Err(ColumnBlockError::BadDirectory);
        }
        // Decode by the recorded stream (its own segment_codec header
        // carries the codec); the directory tag is bookkeeping for #853.
        let data = decode_bytes(&bytes[stream_offset..end])?;
        columns.push(DecodedColumn {
            column_id,
            logical_type,
            codec_tag,
            data,
        });
    }

    Ok(DecodedColumnBlock {
        chunk_id,
        schema_ref,
        row_count,
        min_ts_ns,
        max_ts_ns,
        columns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u64_stream(values: &[u64]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn f64_stream(values: &[f64]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn round_trips_two_columns_value_for_value() {
        let ts: Vec<u64> = (0..500)
            .map(|i| 1_700_000_000_000 + i * 1_000_000)
            .collect();
        let vals: Vec<f64> = (0..500).map(|i| 95.0 + (i % 7) as f64 * 0.25).collect();
        let ts_raw = u64_stream(&ts);
        let val_raw = f64_stream(&vals);

        let block = write_column_block(
            42,
            7,
            ts.len() as u64,
            *ts.first().unwrap(),
            *ts.last().unwrap(),
            &[
                ColumnInput {
                    column_id: 0,
                    logical_type: 2,
                    semantics: ColumnSemantics::Timestamp,
                    data: &ts_raw,
                },
                ColumnInput {
                    column_id: 1,
                    logical_type: 3,
                    semantics: ColumnSemantics::Gauge,
                    data: &val_raw,
                },
            ],
        )
        .unwrap();

        let decoded = read_column_block(&block).unwrap();
        assert_eq!(decoded.chunk_id, 42);
        assert_eq!(decoded.schema_ref, 7);
        assert_eq!(decoded.row_count, 500);
        assert_eq!(decoded.min_ts_ns, *ts.first().unwrap());
        assert_eq!(decoded.max_ts_ns, *ts.last().unwrap());
        assert_eq!(decoded.columns.len(), 2);
        assert_eq!(decoded.columns[0].column_id, 0);
        assert_eq!(decoded.columns[0].logical_type, 2);
        // Per-column selection: the directory records the *leading*
        // semantic codec — DoubleDelta for the timestamp column, XOR for
        // the float gauge — not a single uniform ZSTD tag.
        assert_eq!(decoded.columns[0].codec_tag, ColumnCodec::DoubleDelta.tag());
        assert_eq!(decoded.columns[1].codec_tag, ColumnCodec::Xor.tag());
        // …yet both still decode byte-for-byte (criterion 1 + 2).
        assert_eq!(decoded.columns[0].data, ts_raw);
        assert_eq!(decoded.columns[1].data, val_raw);
    }

    fn str_stream(items: &[&str]) -> Vec<u8> {
        let mut out = (items.len() as u32).to_le_bytes().to_vec();
        for s in items {
            out.extend_from_slice(&(s.len() as u16).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        out
    }

    #[test]
    fn records_counter_and_low_cardinality_codecs_and_round_trips() {
        let counter = u64_stream(&(0..300).map(|i| (i * 5) as u64).collect::<Vec<_>>());
        let labels: Vec<&str> = (0..300).map(|i| ["a", "b", "c"][i % 3]).collect();
        let labels_raw = str_stream(&labels);

        let block = write_column_block(
            9,
            1,
            300,
            0,
            0,
            &[
                ColumnInput {
                    column_id: 10,
                    logical_type: 2,
                    semantics: ColumnSemantics::Counter,
                    data: &counter,
                },
                ColumnInput {
                    column_id: 11,
                    logical_type: 4,
                    semantics: ColumnSemantics::LowCardinality,
                    data: &labels_raw,
                },
            ],
        )
        .unwrap();

        let decoded = read_column_block(&block).unwrap();
        assert_eq!(decoded.columns[0].codec_tag, ColumnCodec::Delta.tag());
        assert_eq!(decoded.columns[1].codec_tag, ColumnCodec::Dict.tag());
        assert_eq!(decoded.columns[0].data, counter);
        assert_eq!(decoded.columns[1].data, labels_raw);
    }

    #[test]
    fn header_carries_magic_and_version() {
        let block = write_column_block(1, 0, 0, 0, 0, &[]).unwrap();
        assert_eq!(&block[0..4], &COLUMN_BLOCK_MAGIC);
        assert_eq!(
            u16::from_le_bytes([block[4], block[5]]),
            COLUMN_BLOCK_VERSION_V1
        );
        assert_eq!(&block[block.len() - 4..], &COLUMN_BLOCK_MAGIC);
        // Empty (zero-column) block still decodes.
        let decoded = read_column_block(&block).unwrap();
        assert!(decoded.columns.is_empty());
    }

    #[test]
    fn rejects_bad_leading_magic() {
        let mut block = write_column_block(1, 0, 0, 0, 0, &[]).unwrap();
        block[0] = b'X';
        assert!(matches!(
            read_column_block(&block),
            Err(ColumnBlockError::BadMagic(_))
        ));
    }

    #[test]
    fn rejects_future_version() {
        let raw = u64_stream(&[1, 2, 3]);
        let mut block = write_column_block(
            1,
            0,
            3,
            1,
            3,
            &[ColumnInput {
                column_id: 0,
                logical_type: 2,
                semantics: ColumnSemantics::Generic,
                data: &raw,
            }],
        )
        .unwrap();
        block[4..6].copy_from_slice(&(COLUMN_BLOCK_VERSION_V1 + 1).to_le_bytes());
        assert!(matches!(
            read_column_block(&block),
            Err(ColumnBlockError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn detects_payload_corruption_via_crc() {
        let raw = f64_stream(&[1.5, 2.5, 3.5, 4.5]);
        let mut block = write_column_block(
            1,
            0,
            4,
            0,
            0,
            &[ColumnInput {
                column_id: 0,
                logical_type: 3,
                semantics: ColumnSemantics::Gauge,
                data: &raw,
            }],
        )
        .unwrap();
        // Flip a byte inside the stream region (after the header).
        block[HEADER_LEN + DIR_ENTRY_LEN] ^= 0xFF;
        assert!(matches!(
            read_column_block(&block),
            Err(ColumnBlockError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn rejects_truncated_buffer() {
        assert!(matches!(
            read_column_block(&[0u8; 8]),
            Err(ColumnBlockError::Truncated)
        ));
    }
}
