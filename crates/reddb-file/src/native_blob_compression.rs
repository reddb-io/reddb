//! Lossless native blob storage. Only the first page uses RDBZ; the payload
//! is one Zstd frame, with an explicit decoded length and CRC32. Readers must
//! supply a decoded-byte budget before allocating or opening the decoder.

use crate::embedded::{RdbFileError, RdbFileResult};
use std::io::Read;

pub const NATIVE_COMPRESSED_BLOB_MAGIC: &[u8; 4] = b"RDBZ";
pub const NATIVE_COMPRESSED_BLOB_HEADER_BYTES: usize = 24;
const COMPRESSION_MIN_BYTES: usize = 64 * 1024;
const WINDOW_LOG_MAX: u32 = 23;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeBlobCompressionHeader {
    pub decoded_bytes: u64,
    pub checksum: u32,
}

fn invalid(message: impl std::fmt::Display) -> RdbFileError {
    RdbFileError::InvalidOperation(format!("native blob compression: {message}"))
}

pub fn native_blob_compression_header(
    page_content: &[u8],
) -> RdbFileResult<Option<NativeBlobCompressionHeader>> {
    if page_content.get(..4) != Some(NATIVE_COMPRESSED_BLOB_MAGIC.as_slice()) {
        return Ok(None);
    }
    let length = page_content
        .get(12..20)
        .ok_or_else(|| invalid("truncated length"))?;
    let checksum = page_content
        .get(20..24)
        .ok_or_else(|| invalid("truncated checksum"))?;
    Ok(Some(NativeBlobCompressionHeader {
        decoded_bytes: u64::from_le_bytes(length.try_into().expect("length checked")),
        checksum: u32::from_le_bytes(checksum.try_into().expect("checksum checked")),
    }))
}

pub fn encode_native_compressed_blob_page(
    next_page: u32,
    header: NativeBlobCompressionHeader,
    chunk: &[u8],
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(NATIVE_COMPRESSED_BLOB_HEADER_BYTES + chunk.len());
    bytes.extend_from_slice(NATIVE_COMPRESSED_BLOB_MAGIC);
    bytes.extend_from_slice(&next_page.to_le_bytes());
    bytes.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&header.decoded_bytes.to_le_bytes());
    bytes.extend_from_slice(&header.checksum.to_le_bytes());
    bytes.extend_from_slice(chunk);
    bytes
}

/// Keep legacy raw storage for small, oversized or poorly compressible input.
pub fn compress_native_blob(
    payload: &[u8],
    decoded_limit_bytes: usize,
) -> RdbFileResult<Option<Vec<u8>>> {
    if payload.len() < COMPRESSION_MIN_BYTES || payload.len() > decoded_limit_bytes {
        return Ok(None);
    }
    let encoded = zstd::bulk::compress(payload, 1).map_err(invalid)?;
    // Require a material saving before adding decoder work and a new format.
    Ok((encoded
        .len()
        .saturating_add(NATIVE_COMPRESSED_BLOB_HEADER_BYTES)
        < payload.len() - payload.len() / 8)
        .then_some(encoded))
}

pub fn decompress_native_blob(
    payload: &[u8],
    header: NativeBlobCompressionHeader,
    decoded_limit_bytes: usize,
) -> RdbFileResult<Vec<u8>> {
    let decoded_bytes = usize::try_from(header.decoded_bytes).map_err(invalid)?;
    if decoded_bytes == 0 || decoded_bytes > decoded_limit_bytes {
        return Err(invalid(format!(
            "decoded length {decoded_bytes} exceeds budget {decoded_limit_bytes} or is empty"
        )));
    }
    if payload.len() > decoded_bytes {
        return Err(invalid(
            "compressed input exceeds its declared decoded length",
        ));
    }
    let content_size = zstd::zstd_safe::get_frame_content_size(payload).map_err(invalid)?;
    if content_size != Some(header.decoded_bytes)
        || zstd::zstd_safe::find_frame_compressed_size(payload).map_err(invalid)? != payload.len()
    {
        return Err(invalid("frame length mismatch or trailing frame data"));
    }
    let mut decoder = zstd::stream::read::Decoder::with_buffer(payload).map_err(invalid)?;
    decoder.window_log_max(WINDOW_LOG_MAX).map_err(invalid)?;
    let mut decoder = decoder.single_frame();
    let mut decoded = Vec::new();
    decoded.try_reserve_exact(decoded_bytes).map_err(invalid)?;
    decoded.resize(decoded_bytes, 0);
    decoder.read_exact(&mut decoded).map_err(invalid)?;
    let mut extra = [0; 1];
    if decoder.read(&mut extra).map_err(invalid)? != 0 {
        return Err(invalid("decoded data exceeds declared length"));
    }
    if crc32fast::hash(&decoded) != header.checksum {
        return Err(invalid("decoded checksum mismatch"));
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_native_blob_consumes_shared_fixture() {
        use sha2::{Digest, Sha256};

        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../testdata/conformance/native-blob-compression/manifest.json"
        ))
        .expect("shared fixture");
        let page = hex::decode(fixture["encodedPageHex"].as_str().expect("page hex"))
            .expect("encoded page");
        let header = native_blob_compression_header(&page)
            .expect("header")
            .expect("compressed page");
        assert_eq!(
            header.decoded_bytes,
            fixture["decodedBytes"].as_u64().unwrap()
        );
        assert_eq!(
            u64::from(header.checksum),
            fixture["decodedCrc32"].as_u64().unwrap()
        );
        let (next, compressed) = crate::decode_native_blob_page(&page).expect("page");
        assert_eq!(u64::from(next), fixture["nextPage"].as_u64().unwrap());
        let limit = usize::try_from(fixture["decodedLimitBytes"].as_u64().unwrap()).unwrap();
        let decoded = decompress_native_blob(&compressed, header, limit).expect("decode fixture");
        assert_eq!(
            hex::encode(Sha256::digest(&decoded)),
            fixture["decodedSha256"]
        );
        let logical = &fixture["logicalFrame"];
        let expected = crate::encode_native_fulltext_frame(&crate::NativeFulltextFrame {
            collection: logical["collection"].as_str().unwrap().into(),
            doc_count: logical["documentCount"].as_u64().unwrap() as u32,
            terms: vec![crate::NativeFulltextTerm {
                term: "a".repeat(logical["termBytes"].as_u64().unwrap() as usize),
                postings: vec![crate::NativeFulltextPosting {
                    entity_id: logical["entityId"].as_u64().unwrap(),
                    term_count: logical["termFrequency"].as_u64().unwrap() as u32,
                }],
            }],
        });
        assert_eq!(decoded, expected);
        assert!(decompress_native_blob(&compressed, header, decoded.len() - 1).is_err());
    }

    #[test]
    fn compressed_native_blob_roundtrips_and_enforces_its_budget() {
        let source = b"native index payload ".repeat(16 * 1024);
        let encoded = compress_native_blob(&source, source.len())
            .expect("compress")
            .expect("saving");
        let header = NativeBlobCompressionHeader {
            decoded_bytes: source.len() as u64,
            checksum: crc32fast::hash(&source),
        };
        assert_eq!(
            decompress_native_blob(&encoded, header, source.len()).expect("decode"),
            source
        );
        assert!(decompress_native_blob(&encoded, header, source.len() - 1).is_err());
        assert!(decompress_native_blob(
            &[],
            NativeBlobCompressionHeader {
                decoded_bytes: u64::MAX,
                checksum: 0,
            },
            1024
        )
        .expect_err("budget before decoder")
        .to_string()
        .contains("budget"));
        assert!(compress_native_blob(&source, source.len() - 1)
            .expect("conservative raw fallback")
            .is_none());
        assert!(compress_native_blob(b"small", 1024)
            .expect("small raw")
            .is_none());
        let page = encode_native_compressed_blob_page(17, header, &encoded);
        assert_eq!(
            native_blob_compression_header(&page).expect("header"),
            Some(header)
        );
        assert_eq!(
            crate::decode_native_blob_page(&page).expect("stored chunk"),
            (17, encoded)
        );
        assert_eq!(
            native_blob_compression_header(b"RDBLlegacy").expect("legacy"),
            None
        );
    }

    #[test]
    fn compressed_native_blob_rejects_corruption_lengths_and_extra_frames() {
        let source = b"postings with stable IDs ".repeat(4096);
        let encoded = compress_native_blob(&source, source.len())
            .expect("compress")
            .expect("saving");
        let header = NativeBlobCompressionHeader {
            decoded_bytes: source.len() as u64,
            checksum: crc32fast::hash(&source),
        };
        for length in [0, 1, encoded.len() - 1] {
            assert!(decompress_native_blob(&encoded[..length], header, source.len()).is_err());
        }
        let mut bad = encoded.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(decompress_native_blob(&bad, header, source.len()).is_err());
        let mut concatenated = encoded.clone();
        concatenated.extend_from_slice(&encoded);
        assert!(decompress_native_blob(&concatenated, header, source.len()).is_err());
        assert!(decompress_native_blob(
            &encoded,
            NativeBlobCompressionHeader {
                decoded_bytes: header.decoded_bytes - 1,
                ..header
            },
            source.len()
        )
        .is_err());
        assert!(decompress_native_blob(
            &encoded,
            NativeBlobCompressionHeader {
                checksum: header.checksum ^ 1,
                ..header
            },
            source.len()
        )
        .is_err());
        let page = encode_native_compressed_blob_page(0, header, &encoded);
        for length in 4..NATIVE_COMPRESSED_BLOB_HEADER_BYTES {
            assert!(native_blob_compression_header(&page[..length]).is_err());
        }
    }
}
