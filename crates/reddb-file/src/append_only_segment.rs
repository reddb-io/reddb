//! Append-only immutable segment v1 contract.
//!
//! `reddb-file` owns the disk-visible frame, codec marker, chunk sizing, and
//! checksum validation. Runtime crates hand this module opaque row bytes.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const APPEND_ONLY_SEGMENT_FORMAT_VERSION: u32 = 1;
pub const APPEND_ONLY_SEGMENT_CHUNK_SIZE: u32 = 512 * 1024;

const MAGIC: &[u8; 8] = b"RDAOSEG1";
const HEADER_LEN_BYTES: usize = 4;
const DEFAULT_ZSTD_LEVEL: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppendOnlySegmentCodec {
    Zstd,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendOnlyChunkRef {
    pub offset: u64,
    pub len: u32,
    pub checksum: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendOnlySegmentMetadata {
    pub format_version: u32,
    pub chunk_size: u32,
    pub codec: AppendOnlySegmentCodec,
    pub rows: u64,
    pub raw_len: u64,
    pub encoded_len: u64,
    pub chunks: Vec<AppendOnlyChunkRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendOnlySegmentRead {
    pub metadata: AppendOnlySegmentMetadata,
    pub codec: AppendOnlySegmentCodec,
    pub rows: u64,
    pub payload: Vec<u8>,
}

pub fn write_append_only_segment(
    path: &Path,
    payload: &[u8],
    rows: u64,
    codec: AppendOnlySegmentCodec,
) -> io::Result<AppendOnlySegmentMetadata> {
    let encoded = encode_payload(payload, codec)?;
    let metadata = AppendOnlySegmentMetadata {
        format_version: APPEND_ONLY_SEGMENT_FORMAT_VERSION,
        chunk_size: APPEND_ONLY_SEGMENT_CHUNK_SIZE,
        codec,
        rows,
        raw_len: payload.len() as u64,
        encoded_len: encoded.len() as u64,
        chunks: chunk_refs(&encoded),
    };
    let header = serde_json::to_vec(&metadata).map_err(invalid_data)?;
    let header_len = u32::try_from(header.len())
        .map_err(|_| invalid_data("append-only segment header is too large"))?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(path)?;
    file.write_all(MAGIC)?;
    file.write_all(&header_len.to_le_bytes())?;
    file.write_all(&header)?;
    file.write_all(&encoded)?;
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(metadata)
}

pub fn read_append_only_segment(path: &Path) -> io::Result<AppendOnlySegmentRead> {
    let bytes = fs::read(path)?;
    if bytes.len() < MAGIC.len() + HEADER_LEN_BYTES {
        return Err(invalid_data("append-only segment is truncated"));
    }
    if &bytes[..MAGIC.len()] != MAGIC {
        return Err(invalid_data("append-only segment magic mismatch"));
    }
    let mut len_bytes = [0u8; HEADER_LEN_BYTES];
    len_bytes.copy_from_slice(&bytes[MAGIC.len()..MAGIC.len() + HEADER_LEN_BYTES]);
    let header_len = u32::from_le_bytes(len_bytes) as usize;
    let payload_start = MAGIC.len() + HEADER_LEN_BYTES + header_len;
    if bytes.len() < payload_start {
        return Err(invalid_data("append-only segment header is truncated"));
    }

    let metadata: AppendOnlySegmentMetadata =
        serde_json::from_slice(&bytes[MAGIC.len() + HEADER_LEN_BYTES..payload_start])
            .map_err(invalid_data)?;
    if metadata.format_version != APPEND_ONLY_SEGMENT_FORMAT_VERSION {
        return Err(invalid_data(format!(
            "unsupported append-only segment format version: {}",
            metadata.format_version
        )));
    }
    if metadata.chunk_size != APPEND_ONLY_SEGMENT_CHUNK_SIZE {
        return Err(invalid_data(format!(
            "unsupported append-only segment chunk size: {}",
            metadata.chunk_size
        )));
    }

    let encoded = &bytes[payload_start..];
    if encoded.len() as u64 != metadata.encoded_len {
        return Err(invalid_data("append-only segment payload length mismatch"));
    }
    let actual_chunks = chunk_refs(encoded);
    if actual_chunks != metadata.chunks {
        return Err(invalid_data("append-only segment chunk checksum mismatch"));
    }
    let payload = decode_payload(encoded, metadata.codec, metadata.raw_len)?;
    Ok(AppendOnlySegmentRead {
        codec: metadata.codec,
        rows: metadata.rows,
        metadata,
        payload,
    })
}

fn encode_payload(payload: &[u8], codec: AppendOnlySegmentCodec) -> io::Result<Vec<u8>> {
    match codec {
        AppendOnlySegmentCodec::Zstd => zstd::bulk::compress(payload, DEFAULT_ZSTD_LEVEL)
            .map_err(|err| invalid_data(format!("zstd compress append-only segment: {err}"))),
        AppendOnlySegmentCodec::None => Ok(payload.to_vec()),
    }
}

fn decode_payload(
    encoded: &[u8],
    codec: AppendOnlySegmentCodec,
    raw_len: u64,
) -> io::Result<Vec<u8>> {
    match codec {
        AppendOnlySegmentCodec::Zstd => {
            let max_len = usize::try_from(raw_len)
                .map_err(|_| invalid_data("append-only segment raw_len exceeds usize"))?;
            zstd::bulk::decompress(encoded, max_len)
                .map_err(|err| invalid_data(format!("zstd decompress append-only segment: {err}")))
        }
        AppendOnlySegmentCodec::None => Ok(encoded.to_vec()),
    }
}

fn chunk_refs(bytes: &[u8]) -> Vec<AppendOnlyChunkRef> {
    bytes
        .chunks(APPEND_ONLY_SEGMENT_CHUNK_SIZE as usize)
        .scan(0u64, |offset, chunk| {
            let current = *offset;
            *offset += chunk.len() as u64;
            Some(AppendOnlyChunkRef {
                offset: current,
                len: chunk.len() as u32,
                checksum: crc32fast::hash(chunk),
            })
        })
        .collect()
}

fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn invalid_data(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_round_trip_validates_chunk_checksums() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("segment.rseg");
        let payload = b"row one\nrow two\n";

        let metadata =
            write_append_only_segment(&path, payload, 2, AppendOnlySegmentCodec::Zstd).unwrap();
        let read = read_append_only_segment(&path).unwrap();

        assert_eq!(read.payload, payload);
        assert_eq!(read.rows, 2);
        assert_eq!(read.codec, AppendOnlySegmentCodec::Zstd);
        assert_eq!(read.metadata.chunks, metadata.chunks);
        assert!(!read.metadata.chunks.is_empty());
    }

    #[test]
    fn segment_none_codec_keeps_payload_plain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("segment.rseg");
        let payload = b"already compressed bytes";

        let metadata =
            write_append_only_segment(&path, payload, 1, AppendOnlySegmentCodec::None).unwrap();
        let read = read_append_only_segment(&path).unwrap();

        assert_eq!(metadata.codec, AppendOnlySegmentCodec::None);
        assert_eq!(read.payload, payload);
    }
}
