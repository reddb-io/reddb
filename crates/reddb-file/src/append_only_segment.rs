use serde::{Deserialize, Serialize};

use crate::{RdbFileError, RdbFileResult};

pub const APPEND_ONLY_SEGMENT_VERSION: u32 = 1;
pub const APPEND_ONLY_SEGMENT_CHUNK_BYTES: u32 = 512 * 1024;

const MAGIC: &[u8; 8] = b"RDBAOS01";
const FIXED_HEADER_LEN: usize = 8 + 4 + 8 + 4 + 4;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppendOnlySegmentCodec {
    #[default]
    Zstd,
    None,
}

impl AppendOnlySegmentCodec {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zstd => "zstd",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendOnlySegmentRow {
    pub primary_key: Vec<u8>,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendOnlySegmentChunkChecksum {
    pub offset: u64,
    pub len: u32,
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendOnlySegmentBloom {
    pub num_hashes: u8,
    pub bit_size: u32,
    pub inserted: u32,
    pub bits_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendOnlySegment {
    pub version: u32,
    pub codec: AppendOnlySegmentCodec,
    pub chunk_size: u32,
    pub rows: Vec<AppendOnlySegmentRow>,
    pub primary_min: Option<Vec<u8>>,
    pub primary_max: Option<Vec<u8>>,
    pub primary_bloom: Option<AppendOnlySegmentBloom>,
    pub chunk_checksums: Vec<AppendOnlySegmentChunkChecksum>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SegmentHeader {
    version: u32,
    codec: AppendOnlySegmentCodec,
    chunk_size: u32,
    row_count: u64,
    uncompressed_len: u64,
    primary_min_hex: Option<String>,
    primary_max_hex: Option<String>,
    primary_bloom: Option<AppendOnlySegmentBloom>,
    chunk_checksums: Vec<AppendOnlySegmentChunkChecksum>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RowJson {
    primary_key_hex: String,
    payload_hex: String,
}

pub fn encode_append_only_segment(
    codec: AppendOnlySegmentCodec,
    rows: &[AppendOnlySegmentRow],
) -> RdbFileResult<Vec<u8>> {
    let row_json = rows
        .iter()
        .map(|row| RowJson {
            primary_key_hex: hex::encode(&row.primary_key),
            payload_hex: hex::encode(&row.payload),
        })
        .collect::<Vec<_>>();
    let plain = serde_json::to_vec(&row_json).map_err(invalid_operation)?;
    let body = match codec {
        AppendOnlySegmentCodec::Zstd => {
            zstd::bulk::compress(&plain, 3).map_err(invalid_operation)?
        }
        AppendOnlySegmentCodec::None => plain.clone(),
    };
    let chunk_checksums = chunk_checksums(&body);
    let header = SegmentHeader {
        version: APPEND_ONLY_SEGMENT_VERSION,
        codec,
        chunk_size: APPEND_ONLY_SEGMENT_CHUNK_BYTES,
        row_count: rows.len() as u64,
        uncompressed_len: plain.len() as u64,
        primary_min_hex: rows
            .iter()
            .map(|row| row.primary_key.as_slice())
            .min()
            .map(hex::encode),
        primary_max_hex: rows
            .iter()
            .map(|row| row.primary_key.as_slice())
            .max()
            .map(hex::encode),
        primary_bloom: build_primary_bloom(rows),
        chunk_checksums,
    };
    let header_bytes = serde_json::to_vec(&header).map_err(invalid_operation)?;
    let header_len = u32::try_from(header_bytes.len()).map_err(|_| {
        RdbFileError::InvalidOperation("append-only segment header too large".into())
    })?;
    let body_len = u64::try_from(body.len())
        .map_err(|_| RdbFileError::InvalidOperation("append-only segment body too large".into()))?;

    let mut out = Vec::with_capacity(FIXED_HEADER_LEN + header_bytes.len() + body.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&header_len.to_le_bytes());
    out.extend_from_slice(&body_len.to_le_bytes());
    out.extend_from_slice(&crc32(&header_bytes).to_le_bytes());
    out.extend_from_slice(&crc32(&body).to_le_bytes());
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn decode_append_only_segment(bytes: &[u8]) -> RdbFileResult<AppendOnlySegment> {
    if bytes.len() < FIXED_HEADER_LEN {
        return Err(RdbFileError::InvalidOperation(
            "append-only segment frame too short".into(),
        ));
    }
    if &bytes[..8] != MAGIC {
        return Err(RdbFileError::InvalidOperation(
            "invalid append-only segment magic".into(),
        ));
    }
    let header_len = read_u32(bytes, 8)? as usize;
    let body_len = read_u64(bytes, 12)? as usize;
    let header_crc = read_u32(bytes, 20)?;
    let body_crc = read_u32(bytes, 24)?;
    let header_start = FIXED_HEADER_LEN;
    let body_start = header_start.checked_add(header_len).ok_or_else(|| {
        RdbFileError::InvalidOperation("append-only segment size overflow".into())
    })?;
    let end = body_start.checked_add(body_len).ok_or_else(|| {
        RdbFileError::InvalidOperation("append-only segment size overflow".into())
    })?;
    if bytes.len() != end {
        return Err(RdbFileError::InvalidOperation(
            "append-only segment frame length mismatch".into(),
        ));
    }
    let header_bytes = &bytes[header_start..body_start];
    let body = &bytes[body_start..end];
    if crc32(header_bytes) != header_crc {
        return Err(RdbFileError::InvalidOperation(
            "append-only segment header checksum mismatch".into(),
        ));
    }
    if crc32(body) != body_crc {
        return Err(RdbFileError::InvalidOperation(
            "append-only segment body checksum mismatch".into(),
        ));
    }
    let header: SegmentHeader = serde_json::from_slice(header_bytes).map_err(invalid_operation)?;
    if header.version != APPEND_ONLY_SEGMENT_VERSION {
        return Err(RdbFileError::InvalidOperation(format!(
            "unsupported append-only segment version: {}",
            header.version
        )));
    }
    if header.chunk_size != APPEND_ONLY_SEGMENT_CHUNK_BYTES {
        return Err(RdbFileError::InvalidOperation(
            "append-only segment chunk size mismatch".into(),
        ));
    }
    if header.chunk_checksums != chunk_checksums(body) {
        return Err(RdbFileError::InvalidOperation(
            "append-only segment chunk checksum mismatch".into(),
        ));
    }

    let plain = match header.codec {
        AppendOnlySegmentCodec::Zstd => {
            let max_len = usize::try_from(header.uncompressed_len).map_err(|_| {
                RdbFileError::InvalidOperation("append-only segment plain length too large".into())
            })?;
            zstd::bulk::decompress(body, max_len).map_err(invalid_operation)?
        }
        AppendOnlySegmentCodec::None => body.to_vec(),
    };
    if plain.len() as u64 != header.uncompressed_len {
        return Err(RdbFileError::InvalidOperation(
            "append-only segment plain length mismatch".into(),
        ));
    }
    let row_json: Vec<RowJson> = serde_json::from_slice(&plain).map_err(invalid_operation)?;
    if row_json.len() as u64 != header.row_count {
        return Err(RdbFileError::InvalidOperation(
            "append-only segment row count mismatch".into(),
        ));
    }
    let rows = row_json
        .into_iter()
        .map(|row| {
            Ok(AppendOnlySegmentRow {
                primary_key: hex::decode(row.primary_key_hex).map_err(invalid_operation)?,
                payload: hex::decode(row.payload_hex).map_err(invalid_operation)?,
            })
        })
        .collect::<RdbFileResult<Vec<_>>>()?;
    let primary_min = decode_optional_hex(header.primary_min_hex)?;
    let primary_max = decode_optional_hex(header.primary_max_hex)?;
    Ok(AppendOnlySegment {
        version: header.version,
        codec: header.codec,
        chunk_size: header.chunk_size,
        rows,
        primary_min,
        primary_max,
        primary_bloom: header.primary_bloom,
        chunk_checksums: header.chunk_checksums,
    })
}

pub fn append_only_segment_chunk_checksums(bytes: &[u8]) -> Vec<AppendOnlySegmentChunkChecksum> {
    chunk_checksums(bytes)
}

pub fn append_only_segment_primary_bloom_might_contain(
    bloom: &AppendOnlySegmentBloom,
    key: &[u8],
) -> bool {
    if bloom.bit_size == 0 || bloom.num_hashes == 0 {
        return true;
    }
    let Ok(bits) = hex::decode(&bloom.bits_hex) else {
        return true;
    };
    for idx in primary_bloom_indexes(key, bloom.bit_size, bloom.num_hashes) {
        let byte = (idx / 8) as usize;
        let mask = 1u8 << (idx % 8);
        if bits
            .get(byte)
            .map(|value| value & mask == 0)
            .unwrap_or(true)
        {
            return false;
        }
    }
    true
}

fn build_primary_bloom(rows: &[AppendOnlySegmentRow]) -> Option<AppendOnlySegmentBloom> {
    if rows.is_empty() {
        return None;
    }
    let inserted = u32::try_from(rows.len()).ok()?;
    let bit_size = inserted.saturating_mul(10).max(64);
    let num_hashes = 3;
    let mut bits = vec![0u8; (bit_size as usize).div_ceil(8)];
    for row in rows {
        for idx in primary_bloom_indexes(&row.primary_key, bit_size, num_hashes) {
            bits[(idx / 8) as usize] |= 1u8 << (idx % 8);
        }
    }
    Some(AppendOnlySegmentBloom {
        num_hashes,
        bit_size,
        inserted,
        bits_hex: hex::encode(bits),
    })
}

fn primary_bloom_indexes(key: &[u8], bit_size: u32, num_hashes: u8) -> impl Iterator<Item = u32> {
    let h1 = fnv1a64(key);
    let h2 = djb2_64(key).max(1);
    (0..num_hashes)
        .map(move |idx| h1.wrapping_add(u64::from(idx).wrapping_mul(h2)) as u32 % bit_size)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn djb2_64(bytes: &[u8]) -> u64 {
    let mut hash = 5381u64;
    for byte in bytes {
        hash = hash.wrapping_mul(33).wrapping_add(u64::from(*byte));
    }
    hash
}

fn chunk_checksums(bytes: &[u8]) -> Vec<AppendOnlySegmentChunkChecksum> {
    bytes
        .chunks(APPEND_ONLY_SEGMENT_CHUNK_BYTES as usize)
        .enumerate()
        .map(|(idx, chunk)| AppendOnlySegmentChunkChecksum {
            offset: (idx as u64) * u64::from(APPEND_ONLY_SEGMENT_CHUNK_BYTES),
            len: chunk.len() as u32,
            checksum: format!("crc32:{:08x}", crc32(chunk)),
        })
        .collect()
}

fn read_u32(bytes: &[u8], offset: usize) -> RdbFileResult<u32> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| RdbFileError::InvalidOperation("append-only segment u32 missing".into()))?;
    Ok(u32::from_le_bytes(slice.try_into().expect("slice length")))
}

fn read_u64(bytes: &[u8], offset: usize) -> RdbFileResult<u64> {
    let slice = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| RdbFileError::InvalidOperation("append-only segment u64 missing".into()))?;
    Ok(u64::from_le_bytes(slice.try_into().expect("slice length")))
}

fn decode_optional_hex(value: Option<String>) -> RdbFileResult<Option<Vec<u8>>> {
    value
        .map(hex::decode)
        .transpose()
        .map_err(invalid_operation)
}

fn crc32(data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(data);
    hasher.finalize()
}

fn invalid_operation(err: impl ToString) -> RdbFileError {
    RdbFileError::InvalidOperation(err.to_string())
}
