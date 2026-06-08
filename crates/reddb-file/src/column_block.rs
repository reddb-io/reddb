//! Persisted `RDCC` column-block envelope.
//!
//! This module owns the byte frame: header, column directory, stream/index
//! offsets, footer, and checksum. Runtime codec choice and query pruning stay
//! outside this crate.

use crate::{COLUMN_BLOCK_MAGIC, COLUMN_BLOCK_VERSION_V1};

pub const COLUMN_BLOCK_HEADER_LEN: usize = 52;
pub const COLUMN_BLOCK_DIR_ENTRY_LEN: usize = 54;
pub const COLUMN_BLOCK_FOOTER_LEN: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnBlockFrameError {
    Truncated,
    BadMagic([u8; 4]),
    BadTailMagic([u8; 4]),
    UnsupportedVersion(u16),
    BadDirectory,
    ChecksumMismatch { expected: u32, actual: u32 },
}

impl std::fmt::Display for ColumnBlockFrameError {
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
        }
    }
}

impl std::error::Error for ColumnBlockFrameError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnBlockPart<'a> {
    pub column_id: u32,
    pub logical_type: u8,
    pub codec_tag: u8,
    pub stream: &'a [u8],
    pub granule_index: &'a [u8],
    pub granule_bloom: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnBlockColumn<'a> {
    pub column_id: u32,
    pub logical_type: u8,
    pub codec_tag: u8,
    pub stream: &'a [u8],
    pub granule_index: Option<&'a [u8]>,
    pub granule_bloom: Option<&'a [u8]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnBlockFrame<'a> {
    pub chunk_id: u64,
    pub schema_ref: u64,
    pub row_count: u64,
    pub min_ts_ns: u64,
    pub max_ts_ns: u64,
    pub columns: Vec<ColumnBlockColumn<'a>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnBlockGranuleStats {
    pub min: Vec<u8>,
    pub max: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnBlockGranuleIndex {
    pub granule_size: u32,
    pub value_width: u32,
    pub granules: Vec<ColumnBlockGranuleStats>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnBlockGranuleBloom<'a> {
    pub granule_size: u32,
    pub blooms: Vec<&'a [u8]>,
}

pub fn column_block_crc32(data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(data);
    hasher.finalize()
}

pub fn encode_column_block_frame(
    chunk_id: u64,
    schema_ref: u64,
    row_count: u64,
    min_ts_ns: u64,
    max_ts_ns: u64,
    columns: &[ColumnBlockPart<'_>],
) -> Vec<u8> {
    let column_count = columns.len();
    let dir_off = COLUMN_BLOCK_HEADER_LEN;
    let dir_len = column_count * COLUMN_BLOCK_DIR_ENTRY_LEN;
    let streams_off = dir_off + dir_len;
    let granule_region_off =
        streams_off as u64 + columns.iter().map(|c| c.stream.len() as u64).sum::<u64>();
    let bloom_region_off = granule_region_off
        + columns
            .iter()
            .map(|c| c.granule_index.len() as u64)
            .sum::<u64>();

    let mut out = Vec::with_capacity(
        streams_off
            + columns.iter().map(|c| c.stream.len()).sum::<usize>()
            + columns.iter().map(|c| c.granule_index.len()).sum::<usize>()
            + columns.iter().map(|c| c.granule_bloom.len()).sum::<usize>()
            + COLUMN_BLOCK_FOOTER_LEN,
    );

    out.extend_from_slice(&COLUMN_BLOCK_MAGIC);
    out.extend_from_slice(&COLUMN_BLOCK_VERSION_V1.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&chunk_id.to_le_bytes());
    out.extend_from_slice(&schema_ref.to_le_bytes());
    out.extend_from_slice(&row_count.to_le_bytes());
    out.extend_from_slice(&(column_count as u32).to_le_bytes());
    out.extend_from_slice(&min_ts_ns.to_le_bytes());
    out.extend_from_slice(&max_ts_ns.to_le_bytes());
    debug_assert_eq!(out.len(), COLUMN_BLOCK_HEADER_LEN);

    let mut cursor = streams_off as u64;
    let mut granule_cursor = granule_region_off;
    let mut bloom_cursor = bloom_region_off;
    for col in columns {
        out.extend_from_slice(&col.column_id.to_le_bytes());
        out.push(col.logical_type);
        out.push(col.codec_tag);
        out.extend_from_slice(&cursor.to_le_bytes());
        out.extend_from_slice(&(col.stream.len() as u64).to_le_bytes());
        if col.granule_index.is_empty() {
            out.extend_from_slice(&0u64.to_le_bytes());
            out.extend_from_slice(&0u64.to_le_bytes());
        } else {
            out.extend_from_slice(&granule_cursor.to_le_bytes());
            out.extend_from_slice(&(col.granule_index.len() as u64).to_le_bytes());
            granule_cursor += col.granule_index.len() as u64;
        }
        if col.granule_bloom.is_empty() {
            out.extend_from_slice(&0u64.to_le_bytes());
            out.extend_from_slice(&0u64.to_le_bytes());
        } else {
            out.extend_from_slice(&bloom_cursor.to_le_bytes());
            out.extend_from_slice(&(col.granule_bloom.len() as u64).to_le_bytes());
            bloom_cursor += col.granule_bloom.len() as u64;
        }
        cursor += col.stream.len() as u64;
    }
    debug_assert_eq!(out.len(), streams_off);

    for col in columns {
        out.extend_from_slice(col.stream);
    }
    debug_assert_eq!(out.len() as u64, granule_region_off);

    for col in columns {
        out.extend_from_slice(col.granule_index);
    }
    debug_assert_eq!(out.len() as u64, bloom_region_off);

    for col in columns {
        out.extend_from_slice(col.granule_bloom);
    }

    let crc = column_block_crc32(&out);
    out.extend_from_slice(&(dir_off as u64).to_le_bytes());
    out.extend_from_slice(&(dir_len as u64).to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&COLUMN_BLOCK_MAGIC);
    out
}

pub fn peek_column_block_version(bytes: &[u8]) -> Option<u16> {
    if bytes.len() < COLUMN_BLOCK_HEADER_LEN + COLUMN_BLOCK_FOOTER_LEN {
        return None;
    }
    let magic: [u8; 4] = bytes[0..4].try_into().ok()?;
    if magic != COLUMN_BLOCK_MAGIC {
        return None;
    }
    Some(u16::from_le_bytes(bytes[4..6].try_into().ok()?))
}

pub fn decode_column_block_frame(
    bytes: &[u8],
) -> Result<ColumnBlockFrame<'_>, ColumnBlockFrameError> {
    if bytes.len() < COLUMN_BLOCK_HEADER_LEN + COLUMN_BLOCK_FOOTER_LEN {
        return Err(ColumnBlockFrameError::Truncated);
    }
    let magic: [u8; 4] = bytes[0..4].try_into().expect("header length checked");
    if magic != COLUMN_BLOCK_MAGIC {
        return Err(ColumnBlockFrameError::BadMagic(magic));
    }
    let version = u16::from_le_bytes(bytes[4..6].try_into().expect("version length checked"));
    if version != COLUMN_BLOCK_VERSION_V1 {
        return Err(ColumnBlockFrameError::UnsupportedVersion(version));
    }

    let footer_start = bytes.len() - COLUMN_BLOCK_FOOTER_LEN;
    let tail_magic: [u8; 4] = bytes[bytes.len() - 4..]
        .try_into()
        .expect("footer length checked");
    if tail_magic != COLUMN_BLOCK_MAGIC {
        return Err(ColumnBlockFrameError::BadTailMagic(tail_magic));
    }
    let dir_off = u64::from_le_bytes(
        bytes[footer_start..footer_start + 8]
            .try_into()
            .expect("footer length checked"),
    ) as usize;
    let dir_len = u64::from_le_bytes(
        bytes[footer_start + 8..footer_start + 16]
            .try_into()
            .expect("footer length checked"),
    ) as usize;
    let stored_crc = u32::from_le_bytes(
        bytes[footer_start + 16..footer_start + 20]
            .try_into()
            .expect("footer length checked"),
    );
    let actual_crc = column_block_crc32(&bytes[..footer_start]);
    if actual_crc != stored_crc {
        return Err(ColumnBlockFrameError::ChecksumMismatch {
            expected: stored_crc,
            actual: actual_crc,
        });
    }

    let chunk_id = u64::from_le_bytes(bytes[8..16].try_into().expect("header length checked"));
    let schema_ref = u64::from_le_bytes(bytes[16..24].try_into().expect("header length checked"));
    let row_count = u64::from_le_bytes(bytes[24..32].try_into().expect("header length checked"));
    let column_count =
        u32::from_le_bytes(bytes[32..36].try_into().expect("header length checked")) as usize;
    let min_ts_ns = u64::from_le_bytes(bytes[36..44].try_into().expect("header length checked"));
    let max_ts_ns = u64::from_le_bytes(bytes[44..52].try_into().expect("header length checked"));

    if dir_off != COLUMN_BLOCK_HEADER_LEN
        || dir_len != column_count * COLUMN_BLOCK_DIR_ENTRY_LEN
        || dir_off + dir_len > footer_start
    {
        return Err(ColumnBlockFrameError::BadDirectory);
    }

    let mut columns = Vec::with_capacity(column_count);
    for i in 0..column_count {
        let base = dir_off + i * COLUMN_BLOCK_DIR_ENTRY_LEN;
        let column_id = u32::from_le_bytes(
            bytes[base..base + 4]
                .try_into()
                .expect("directory length checked"),
        );
        let logical_type = bytes[base + 4];
        let codec_tag = bytes[base + 5];
        let stream_offset = u64::from_le_bytes(
            bytes[base + 6..base + 14]
                .try_into()
                .expect("directory length checked"),
        ) as usize;
        let stream_len = u64::from_le_bytes(
            bytes[base + 14..base + 22]
                .try_into()
                .expect("directory length checked"),
        ) as usize;
        let granule_off = u64::from_le_bytes(
            bytes[base + 22..base + 30]
                .try_into()
                .expect("directory length checked"),
        ) as usize;
        let granule_len = u64::from_le_bytes(
            bytes[base + 30..base + 38]
                .try_into()
                .expect("directory length checked"),
        ) as usize;
        let bloom_off = u64::from_le_bytes(
            bytes[base + 38..base + 46]
                .try_into()
                .expect("directory length checked"),
        ) as usize;
        let bloom_len = u64::from_le_bytes(
            bytes[base + 46..base + 54]
                .try_into()
                .expect("directory length checked"),
        ) as usize;

        let stream_end =
            checked_region_end(stream_offset, stream_len, dir_off, dir_len, footer_start)?;
        let granule_index = if granule_len == 0 {
            None
        } else {
            let end = checked_region_end(granule_off, granule_len, dir_off, dir_len, footer_start)?;
            Some(&bytes[granule_off..end])
        };
        let granule_bloom = if bloom_len == 0 {
            None
        } else {
            let end = checked_region_end(bloom_off, bloom_len, dir_off, dir_len, footer_start)?;
            Some(&bytes[bloom_off..end])
        };

        columns.push(ColumnBlockColumn {
            column_id,
            logical_type,
            codec_tag,
            stream: &bytes[stream_offset..stream_end],
            granule_index,
            granule_bloom,
        });
    }

    Ok(ColumnBlockFrame {
        chunk_id,
        schema_ref,
        row_count,
        min_ts_ns,
        max_ts_ns,
        columns,
    })
}

fn checked_region_end(
    offset: usize,
    len: usize,
    dir_off: usize,
    dir_len: usize,
    footer_start: usize,
) -> Result<usize, ColumnBlockFrameError> {
    let end = offset
        .checked_add(len)
        .ok_or(ColumnBlockFrameError::BadDirectory)?;
    if offset < dir_off + dir_len || end > footer_start {
        return Err(ColumnBlockFrameError::BadDirectory);
    }
    Ok(end)
}

pub fn encode_column_block_granule_index_blob(index: &ColumnBlockGranuleIndex) -> Vec<u8> {
    let w = index.value_width as usize;
    let mut out = Vec::with_capacity(12 + index.granules.len() * w * 2);
    out.extend_from_slice(&index.granule_size.to_le_bytes());
    out.extend_from_slice(&index.value_width.to_le_bytes());
    out.extend_from_slice(&(index.granules.len() as u32).to_le_bytes());
    for g in &index.granules {
        out.extend_from_slice(&g.min);
        out.extend_from_slice(&g.max);
    }
    out
}

pub fn decode_column_block_granule_index_blob(
    bytes: &[u8],
) -> Result<ColumnBlockGranuleIndex, ColumnBlockFrameError> {
    if bytes.len() < 12 {
        return Err(ColumnBlockFrameError::BadDirectory);
    }
    let granule_size =
        u32::from_le_bytes(bytes[0..4].try_into().expect("index header length checked"));
    let value_width =
        u32::from_le_bytes(bytes[4..8].try_into().expect("index header length checked"));
    let count = u32::from_le_bytes(
        bytes[8..12]
            .try_into()
            .expect("index header length checked"),
    ) as usize;
    let w = value_width as usize;
    if w == 0 {
        return Err(ColumnBlockFrameError::BadDirectory);
    }
    let need = 12usize
        .checked_add(
            count
                .checked_mul(w * 2)
                .ok_or(ColumnBlockFrameError::BadDirectory)?,
        )
        .ok_or(ColumnBlockFrameError::BadDirectory)?;
    if bytes.len() < need {
        return Err(ColumnBlockFrameError::BadDirectory);
    }
    let mut granules = Vec::with_capacity(count);
    let mut cur = 12;
    for _ in 0..count {
        let min = bytes[cur..cur + w].to_vec();
        cur += w;
        let max = bytes[cur..cur + w].to_vec();
        cur += w;
        granules.push(ColumnBlockGranuleStats { min, max });
    }
    Ok(ColumnBlockGranuleIndex {
        granule_size,
        value_width,
        granules,
    })
}

pub fn encode_column_block_granule_bloom_blob(granule_size: u32, blooms: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + blooms.iter().map(|b| b.len()).sum::<usize>());
    out.extend_from_slice(&granule_size.to_le_bytes());
    out.extend_from_slice(&(blooms.len() as u32).to_le_bytes());
    for bloom in blooms {
        out.extend_from_slice(bloom);
    }
    out
}

pub fn decode_column_block_granule_bloom_blob(
    bytes: &[u8],
) -> Result<ColumnBlockGranuleBloom<'_>, ColumnBlockFrameError> {
    if bytes.len() < 8 {
        return Err(ColumnBlockFrameError::BadDirectory);
    }
    let granule_size =
        u32::from_le_bytes(bytes[0..4].try_into().expect("bloom header length checked"));
    let count =
        u32::from_le_bytes(bytes[4..8].try_into().expect("bloom header length checked")) as usize;
    let mut blooms = Vec::with_capacity(count);
    let mut cur = 8;
    for _ in 0..count {
        if cur + 4 > bytes.len() {
            return Err(ColumnBlockFrameError::BadDirectory);
        }
        let num_blocks = u32::from_le_bytes(
            bytes[cur..cur + 4]
                .try_into()
                .expect("bloom block-count length checked"),
        ) as usize;
        let bloom_len = 4usize
            .checked_add(
                num_blocks
                    .checked_mul(32)
                    .ok_or(ColumnBlockFrameError::BadDirectory)?,
            )
            .ok_or(ColumnBlockFrameError::BadDirectory)?;
        let end = cur
            .checked_add(bloom_len)
            .ok_or(ColumnBlockFrameError::BadDirectory)?;
        if end > bytes.len() {
            return Err(ColumnBlockFrameError::BadDirectory);
        }
        blooms.push(&bytes[cur..end]);
        cur = end;
    }
    Ok(ColumnBlockGranuleBloom {
        granule_size,
        blooms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trips_header_directory_and_slices() {
        let stream = b"stream";
        let granule = b"granule";
        let bloom = b"\x01\0\0\0................................";
        let encoded = encode_column_block_frame(
            42,
            7,
            10,
            1,
            99,
            &[ColumnBlockPart {
                column_id: 3,
                logical_type: 2,
                codec_tag: 4,
                stream,
                granule_index: granule,
                granule_bloom: bloom,
            }],
        );

        assert_eq!(
            peek_column_block_version(&encoded),
            Some(COLUMN_BLOCK_VERSION_V1)
        );
        let decoded = decode_column_block_frame(&encoded).unwrap();
        assert_eq!(decoded.chunk_id, 42);
        assert_eq!(decoded.columns[0].column_id, 3);
        assert_eq!(decoded.columns[0].stream, stream);
        assert_eq!(decoded.columns[0].granule_index, Some(&granule[..]));
        assert_eq!(decoded.columns[0].granule_bloom, Some(&bloom[..]));
    }

    #[test]
    fn frame_rejects_checksum_mismatch() {
        let mut encoded = encode_column_block_frame(1, 0, 0, 0, 0, &[]);
        encoded[COLUMN_BLOCK_HEADER_LEN - 1] ^= 0xFF;
        assert!(matches!(
            decode_column_block_frame(&encoded),
            Err(ColumnBlockFrameError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn granule_index_blob_round_trips() {
        let index = ColumnBlockGranuleIndex {
            granule_size: 128,
            value_width: 2,
            granules: vec![ColumnBlockGranuleStats {
                min: vec![1, 2],
                max: vec![3, 4],
            }],
        };
        let encoded = encode_column_block_granule_index_blob(&index);
        assert_eq!(
            decode_column_block_granule_index_blob(&encoded).unwrap(),
            index
        );
    }

    #[test]
    fn granule_bloom_blob_slices_each_bloom_payload() {
        let mut one = vec![0u8; 36];
        one[0] = 1;
        let encoded = encode_column_block_granule_bloom_blob(64, &[&one[..]]);
        let decoded = decode_column_block_granule_bloom_blob(&encoded).unwrap();
        assert_eq!(decoded.granule_size, 64);
        assert_eq!(decoded.blooms, vec![&one[..]]);
    }
}
