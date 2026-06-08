//! Native unified-store file contracts.
//!
//! The server owns the storage-engine semantics, but these magic bytes, format
//! versions, and persisted summary shapes are file contracts. They live here so
//! `.rdb` compatibility is governed by `reddb-file`.

use std::collections::BTreeMap;

use crate::embedded::{RdbFileError, RdbFileResult};
use crate::physical_metadata::{ManifestEvent, ManifestEventKind};

pub const STORE_MAGIC: &[u8; 4] = b"RDST";
pub const STORE_VERSION_V1: u32 = 1;
pub const STORE_VERSION_V2: u32 = 2;
pub const STORE_VERSION_V3: u32 = 3;
pub const STORE_VERSION_V4: u32 = 4;
pub const STORE_VERSION_V5: u32 = 5;
pub const STORE_VERSION_V6: u32 = 6;
/// Entity records include metadata (`serialize_entity_record` format).
pub const STORE_VERSION_V7: u32 = 7;
/// Table rows may carry explicit logical identity.
pub const STORE_VERSION_V8: u32 = 8;
/// Entity records persist MVCC xmin/xmax.
pub const STORE_VERSION_V9: u32 = 9;
pub const STORE_VERSION_CURRENT: u32 = STORE_VERSION_V9;

pub const METADATA_MAGIC: &[u8; 4] = b"RDM2";
pub const METADATA_HEADER_BYTES: usize = 12;
pub const NATIVE_COLLECTION_ROOTS_MAGIC: &[u8; 4] = b"RDRT";
pub const NATIVE_MANIFEST_MAGIC: &[u8; 4] = b"RDMF";
pub const NATIVE_REGISTRY_MAGIC: &[u8; 4] = b"RDRG";
pub const NATIVE_RECOVERY_MAGIC: &[u8; 4] = b"RDRV";
pub const NATIVE_CATALOG_MAGIC: &[u8; 4] = b"RDCL";
pub const NATIVE_METADATA_STATE_MAGIC: &[u8; 4] = b"RDMS";
pub const NATIVE_VECTOR_ARTIFACT_MAGIC: &[u8; 4] = b"RDVA";
pub const NATIVE_BLOB_MAGIC: &[u8; 4] = b"RDBL";
pub const NATIVE_MANIFEST_SAMPLE_LIMIT: usize = 16;
pub const ENTITY_RECORD_MAGIC: &[u8; 4] = b"RER1";
pub const METADATA_OVERFLOW_MAGIC: &[u8; 4] = b"RDM3";
pub const METADATA_OVERFLOW_HEADER_BYTES: usize = 16;
pub const METADATA_OVERFLOW_CONTINUATION_HEADER_BYTES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeEntityRecordFrame<'a> {
    pub entity: &'a [u8],
    pub metadata: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeMetadataOverflowHeader {
    pub format_version: u32,
    pub total_payload_bytes: u32,
    pub next_overflow_page_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeMetadataOverflowContinuationHeader {
    pub next_overflow_page_id: u32,
    pub chunk_bytes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativePagedMetadataHeader {
    pub format_version: u32,
    pub collection_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePagedCollectionRoot {
    pub collection: String,
    pub root_page: u32,
}

pub fn native_store_magic_matches(bytes: &[u8]) -> bool {
    bytes.len() >= STORE_MAGIC.len() && &bytes[..STORE_MAGIC.len()] == STORE_MAGIC
}

pub fn encode_native_entity_record_frame(entity: &[u8], metadata: Option<&[u8]>) -> Vec<u8> {
    let metadata = metadata.unwrap_or(&[]);
    let mut out = Vec::with_capacity(12 + entity.len() + metadata.len());
    out.extend_from_slice(ENTITY_RECORD_MAGIC);
    out.extend_from_slice(&(entity.len() as u32).to_le_bytes());
    out.extend_from_slice(entity);
    out.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    out.extend_from_slice(metadata);
    out
}

pub fn decode_native_entity_record_frame(
    data: &[u8],
) -> RdbFileResult<Option<NativeEntityRecordFrame<'_>>> {
    if data.len() < 8 || &data[..4] != ENTITY_RECORD_MAGIC {
        return Ok(None);
    }

    let mut pos = 4usize;
    let entity_len =
        read_native_u32(data, &mut pos, "truncated entity record entity length")? as usize;
    let entity = read_native_bytes(
        data,
        &mut pos,
        entity_len,
        "truncated entity record payload",
    )?;
    let metadata_len =
        read_native_u32(data, &mut pos, "truncated entity record metadata length")? as usize;
    let metadata = read_native_bytes(
        data,
        &mut pos,
        metadata_len,
        "truncated entity record metadata",
    )?;

    Ok(Some(NativeEntityRecordFrame { entity, metadata }))
}

pub fn encode_native_metadata_overflow_header(
    out: &mut [u8],
    header: NativeMetadataOverflowHeader,
) -> RdbFileResult<()> {
    if out.len() < METADATA_OVERFLOW_HEADER_BYTES {
        return Err(RdbFileError::InvalidOperation(
            "metadata overflow header buffer too small".to_string(),
        ));
    }
    out[0..4].copy_from_slice(METADATA_OVERFLOW_MAGIC);
    out[4..8].copy_from_slice(&header.format_version.to_le_bytes());
    out[8..12].copy_from_slice(&header.total_payload_bytes.to_le_bytes());
    out[12..16].copy_from_slice(&header.next_overflow_page_id.to_le_bytes());
    Ok(())
}

pub fn decode_native_metadata_overflow_header(
    data: &[u8],
) -> RdbFileResult<Option<NativeMetadataOverflowHeader>> {
    if data.len() < 4 || &data[..4] != METADATA_OVERFLOW_MAGIC {
        return Ok(None);
    }
    if data.len() < METADATA_OVERFLOW_HEADER_BYTES {
        return Err(RdbFileError::InvalidOperation(
            "truncated metadata overflow header".to_string(),
        ));
    }
    Ok(Some(NativeMetadataOverflowHeader {
        format_version: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
        total_payload_bytes: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
        next_overflow_page_id: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
    }))
}

pub fn encode_native_metadata_overflow_continuation_header(
    out: &mut [u8],
    header: NativeMetadataOverflowContinuationHeader,
) -> RdbFileResult<()> {
    if out.len() < METADATA_OVERFLOW_CONTINUATION_HEADER_BYTES {
        return Err(RdbFileError::InvalidOperation(
            "metadata overflow continuation header buffer too small".to_string(),
        ));
    }
    out[0..4].copy_from_slice(&header.next_overflow_page_id.to_le_bytes());
    out[4..8].copy_from_slice(&header.chunk_bytes.to_le_bytes());
    Ok(())
}

pub fn decode_native_metadata_overflow_continuation_header(
    data: &[u8],
) -> RdbFileResult<NativeMetadataOverflowContinuationHeader> {
    if data.len() < METADATA_OVERFLOW_CONTINUATION_HEADER_BYTES {
        return Err(RdbFileError::InvalidOperation(
            "truncated metadata overflow continuation header".to_string(),
        ));
    }
    Ok(NativeMetadataOverflowContinuationHeader {
        next_overflow_page_id: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        chunk_bytes: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
    })
}

pub fn encode_native_paged_metadata_header(out: &mut Vec<u8>, header: NativePagedMetadataHeader) {
    out.extend_from_slice(METADATA_MAGIC);
    out.extend_from_slice(&header.format_version.to_le_bytes());
    out.extend_from_slice(&header.collection_count.to_le_bytes());
}

pub fn decode_native_paged_metadata_header(
    data: &[u8],
) -> RdbFileResult<Option<NativePagedMetadataHeader>> {
    if data.len() < 4 || &data[..4] != METADATA_MAGIC {
        return Ok(None);
    }
    if data.len() < METADATA_HEADER_BYTES {
        return Err(RdbFileError::InvalidOperation(
            "truncated native paged metadata header".to_string(),
        ));
    }
    Ok(Some(NativePagedMetadataHeader {
        format_version: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
        collection_count: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
    }))
}

pub fn encode_native_len_prefixed_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

pub fn encode_native_len_prefixed_str(out: &mut Vec<u8>, value: &str) {
    encode_native_len_prefixed_bytes(out, value.as_bytes());
}

pub fn decode_native_len_prefixed_bytes<'a>(
    data: &'a [u8],
    pos: &mut usize,
) -> RdbFileResult<&'a [u8]> {
    let len = read_native_u32(data, pos, "truncated native length-prefixed length")? as usize;
    read_native_bytes(data, pos, len, "truncated native length-prefixed payload")
}

pub fn decode_native_len_prefixed_string(data: &[u8], pos: &mut usize) -> RdbFileResult<String> {
    let bytes = decode_native_len_prefixed_bytes(data, pos)?;
    String::from_utf8(bytes.to_vec()).map_err(|err| RdbFileError::InvalidOperation(err.to_string()))
}

pub fn encode_native_paged_collection_root(out: &mut Vec<u8>, collection: &str, root_page: u32) {
    encode_native_len_prefixed_str(out, collection);
    out.extend_from_slice(&root_page.to_le_bytes());
}

pub fn decode_native_paged_collection_root(
    data: &[u8],
    pos: &mut usize,
) -> RdbFileResult<NativePagedCollectionRoot> {
    let collection = decode_native_len_prefixed_string(data, pos)?;
    let root_page = read_native_u32(data, pos, "truncated native paged collection root")?;
    Ok(NativePagedCollectionRoot {
        collection,
        root_page,
    })
}

pub fn is_supported_store_version(version: u32) -> bool {
    matches!(
        version,
        STORE_VERSION_V1
            | STORE_VERSION_V2
            | STORE_VERSION_V3
            | STORE_VERSION_V4
            | STORE_VERSION_V5
            | STORE_VERSION_V6
            | STORE_VERSION_V7
            | STORE_VERSION_V8
            | STORE_VERSION_V9
    )
}

pub fn encode_native_store_header(version: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    out.extend_from_slice(STORE_MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    out
}

pub fn decode_native_store_header(bytes: &[u8]) -> RdbFileResult<u32> {
    if bytes.len() < 8 {
        return Err(RdbFileError::InvalidOperation("File too small".to_string()));
    }
    if &bytes[0..4] != STORE_MAGIC {
        return Err(RdbFileError::InvalidOperation(
            "Invalid magic bytes - expected RDST".to_string(),
        ));
    }
    let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if !is_supported_store_version(version) {
        return Err(RdbFileError::InvalidOperation(format!(
            "Unsupported version: {version}"
        )));
    }
    Ok(version)
}

pub fn append_native_store_crc32_footer(bytes: &mut Vec<u8>) {
    let checksum = native_store_dump_crc32(bytes);
    bytes.extend_from_slice(&checksum.to_le_bytes());
}

pub fn verify_native_store_crc32_footer(bytes: &mut Vec<u8>, version: u32) -> RdbFileResult<()> {
    if version < STORE_VERSION_V3 {
        return Ok(());
    }
    if bytes.len() < 12 {
        return Err(RdbFileError::InvalidOperation(
            "File too small for CRC32 verification".to_string(),
        ));
    }
    let footer_at = bytes.len() - 4;
    let stored_crc = u32::from_le_bytes([
        bytes[footer_at],
        bytes[footer_at + 1],
        bytes[footer_at + 2],
        bytes[footer_at + 3],
    ]);
    let computed_crc = native_store_dump_crc32(&bytes[..footer_at]);
    if stored_crc != computed_crc {
        return Err(RdbFileError::InvalidOperation(
            "Binary store CRC32 mismatch — file corrupted".to_string(),
        ));
    }
    bytes.truncate(footer_at);
    Ok(())
}

fn native_store_dump_crc32(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

fn read_native_bytes<'a>(
    data: &'a [u8],
    pos: &mut usize,
    len: usize,
    err: &'static str,
) -> RdbFileResult<&'a [u8]> {
    let end = pos
        .checked_add(len)
        .ok_or_else(|| RdbFileError::InvalidOperation(err.to_string()))?;
    if end > data.len() {
        return Err(RdbFileError::InvalidOperation(err.to_string()));
    }
    let bytes = &data[*pos..end];
    *pos = end;
    Ok(bytes)
}

fn read_native_u32(data: &[u8], pos: &mut usize, err: &'static str) -> RdbFileResult<u32> {
    let bytes = read_native_bytes(data, pos, 4, err)?;
    let mut raw = [0u8; 4];
    raw.copy_from_slice(bytes);
    Ok(u32::from_le_bytes(raw))
}

#[derive(Debug, Clone)]
pub struct NativeManifestEntrySummary {
    pub collection: String,
    pub object_key: String,
    pub kind: String,
    pub block_index: u64,
    pub block_checksum: u128,
    pub snapshot_min: u64,
    pub snapshot_max: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct NativeManifestSummary {
    pub sequence: u64,
    pub event_count: u32,
    pub events_complete: bool,
    pub omitted_event_count: u32,
    pub recent_events: Vec<NativeManifestEntrySummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRegistryIndexSummary {
    pub name: String,
    pub kind: String,
    pub collection: Option<String>,
    pub enabled: bool,
    pub entries: u64,
    pub estimated_memory_bytes: u64,
    pub last_refresh_ms: Option<u128>,
    pub backend: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRegistryProjectionSummary {
    pub name: String,
    pub source: String,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    pub node_labels: Vec<String>,
    pub node_types: Vec<String>,
    pub edge_labels: Vec<String>,
    pub last_materialized_sequence: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRegistryJobSummary {
    pub id: String,
    pub kind: String,
    pub projection: Option<String>,
    pub state: String,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    pub last_run_sequence: Option<u64>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeVectorArtifactSummary {
    pub collection: String,
    pub artifact_kind: String,
    pub vector_count: u64,
    pub dimension: u32,
    pub max_layer: u32,
    pub serialized_bytes: u64,
    pub checksum: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeVectorArtifactPageSummary {
    pub collection: String,
    pub artifact_kind: String,
    pub root_page: u32,
    pub page_count: u32,
    pub byte_len: u64,
    pub checksum: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRegistrySummary {
    pub collection_count: u32,
    pub index_count: u32,
    pub graph_projection_count: u32,
    pub analytics_job_count: u32,
    pub vector_artifact_count: u32,
    pub collections_complete: bool,
    pub indexes_complete: bool,
    pub graph_projections_complete: bool,
    pub analytics_jobs_complete: bool,
    pub vector_artifacts_complete: bool,
    pub omitted_collection_count: u32,
    pub omitted_index_count: u32,
    pub omitted_graph_projection_count: u32,
    pub omitted_analytics_job_count: u32,
    pub omitted_vector_artifact_count: u32,
    pub collection_names: Vec<String>,
    pub indexes: Vec<NativeRegistryIndexSummary>,
    pub graph_projections: Vec<NativeRegistryProjectionSummary>,
    pub analytics_jobs: Vec<NativeRegistryJobSummary>,
    pub vector_artifacts: Vec<NativeVectorArtifactSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSnapshotSummary {
    pub snapshot_id: u64,
    pub created_at_unix_ms: u128,
    pub superblock_sequence: u64,
    pub collection_count: u32,
    pub total_entities: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeExportSummary {
    pub name: String,
    pub created_at_unix_ms: u128,
    pub snapshot_id: Option<u64>,
    pub superblock_sequence: u64,
    pub collection_count: u32,
    pub total_entities: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRecoverySummary {
    pub snapshot_count: u32,
    pub export_count: u32,
    pub snapshots_complete: bool,
    pub exports_complete: bool,
    pub omitted_snapshot_count: u32,
    pub omitted_export_count: u32,
    pub snapshots: Vec<NativeSnapshotSummary>,
    pub exports: Vec<NativeExportSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCatalogCollectionSummary {
    pub name: String,
    pub entities: u64,
    pub cross_refs: u64,
    pub segments: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCatalogSummary {
    pub collection_count: u32,
    pub total_entities: u64,
    pub collections_complete: bool,
    pub omitted_collection_count: u32,
    pub collections: Vec<NativeCatalogCollectionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeMetadataStateSummary {
    pub protocol_version: String,
    pub generated_at_unix_ms: u128,
    pub last_loaded_from: Option<String>,
    pub last_healed_at_unix_ms: Option<u128>,
}

pub fn native_store_page_checksum(data: &[u8]) -> u64 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(data);
    hasher.finalize() as u64
}

pub fn encode_native_collection_roots_page(roots: &BTreeMap<String, u64>) -> Vec<u8> {
    let mut data = Vec::with_capacity(1024);
    data.extend_from_slice(NATIVE_COLLECTION_ROOTS_MAGIC);
    data.extend_from_slice(&(roots.len() as u32).to_le_bytes());
    for (collection, root) in roots {
        data.extend_from_slice(&(collection.len() as u32).to_le_bytes());
        data.extend_from_slice(collection.as_bytes());
        data.extend_from_slice(&root.to_le_bytes());
    }
    data
}

pub fn decode_native_collection_roots_page(content: &[u8]) -> RdbFileResult<BTreeMap<String, u64>> {
    if content.len() < 8 || &content[0..4] != NATIVE_COLLECTION_ROOTS_MAGIC {
        return Err(RdbFileError::InvalidOperation(
            "invalid native collection roots page".to_string(),
        ));
    }

    let count = u32::from_le_bytes([content[4], content[5], content[6], content[7]]) as usize;
    let mut pos = 8usize;
    let mut roots = BTreeMap::new();

    for _ in 0..count {
        if pos + 4 > content.len() {
            break;
        }
        let name_len = u32::from_le_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
        ]) as usize;
        pos += 4;
        if pos + name_len + 8 > content.len() {
            break;
        }
        let name = String::from_utf8(content[pos..pos + name_len].to_vec())
            .map_err(|err| RdbFileError::InvalidOperation(err.to_string()))?;
        pos += name_len;
        let root = u64::from_le_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
            content[pos + 4],
            content[pos + 5],
            content[pos + 6],
            content[pos + 7],
        ]);
        pos += 8;
        roots.insert(name, root);
    }

    Ok(roots)
}

pub fn encode_native_manifest_summary_page(sequence: u64, events: &[ManifestEvent]) -> Vec<u8> {
    let sample_start = events.len().saturating_sub(NATIVE_MANIFEST_SAMPLE_LIMIT);
    let sample = &events[sample_start..];

    let mut data = Vec::with_capacity(1024);
    data.extend_from_slice(NATIVE_MANIFEST_MAGIC);
    data.extend_from_slice(&sequence.to_le_bytes());
    data.extend_from_slice(&(events.len() as u32).to_le_bytes());
    data.push(u8::from(events.len() <= NATIVE_MANIFEST_SAMPLE_LIMIT));
    data.extend_from_slice(&(events.len().saturating_sub(sample.len()) as u32).to_le_bytes());
    data.extend_from_slice(&(sample.len() as u32).to_le_bytes());
    for event in sample {
        data.push(native_manifest_kind_to_byte(event.kind));
        data.extend_from_slice(&(event.collection.len() as u16).to_le_bytes());
        data.extend_from_slice(event.collection.as_bytes());
        data.extend_from_slice(&(event.object_key.len() as u16).to_le_bytes());
        data.extend_from_slice(event.object_key.as_bytes());
        data.extend_from_slice(&event.block.index.to_le_bytes());
        data.extend_from_slice(&event.block.checksum.to_le_bytes());
        data.extend_from_slice(&event.snapshot_min.to_le_bytes());
        match event.snapshot_max {
            Some(value) => {
                data.push(1);
                data.extend_from_slice(&value.to_le_bytes());
            }
            None => data.push(0),
        }
    }

    data
}

pub fn decode_native_manifest_summary_page(content: &[u8]) -> RdbFileResult<NativeManifestSummary> {
    if content.len() < 25 || &content[0..4] != NATIVE_MANIFEST_MAGIC {
        return Err(RdbFileError::InvalidOperation(
            "invalid native manifest summary page".to_string(),
        ));
    }

    let sequence = u64::from_le_bytes([
        content[4],
        content[5],
        content[6],
        content[7],
        content[8],
        content[9],
        content[10],
        content[11],
    ]);
    let event_count = u32::from_le_bytes([content[12], content[13], content[14], content[15]]);
    let events_complete = content[16] == 1;
    let omitted_event_count =
        u32::from_le_bytes([content[17], content[18], content[19], content[20]]);
    let sample_count =
        u32::from_le_bytes([content[21], content[22], content[23], content[24]]) as usize;

    let mut pos = 25usize;
    let mut recent_events = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        if pos + 1 + 2 > content.len() {
            break;
        }
        let kind = native_manifest_kind_from_byte(content[pos]).to_string();
        pos += 1;
        let collection_len = u16::from_le_bytes([content[pos], content[pos + 1]]) as usize;
        pos += 2;
        if pos + collection_len + 2 > content.len() {
            break;
        }
        let collection = String::from_utf8(content[pos..pos + collection_len].to_vec())
            .map_err(|err| RdbFileError::InvalidOperation(err.to_string()))?;
        pos += collection_len;
        let object_key_len = u16::from_le_bytes([content[pos], content[pos + 1]]) as usize;
        pos += 2;
        if pos + object_key_len + 8 + 16 + 8 + 1 > content.len() {
            break;
        }
        let object_key = String::from_utf8(content[pos..pos + object_key_len].to_vec())
            .map_err(|err| RdbFileError::InvalidOperation(err.to_string()))?;
        pos += object_key_len;
        let block_index = u64::from_le_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
            content[pos + 4],
            content[pos + 5],
            content[pos + 6],
            content[pos + 7],
        ]);
        pos += 8;
        let mut checksum_bytes = [0u8; 16];
        checksum_bytes.copy_from_slice(&content[pos..pos + 16]);
        pos += 16;
        let snapshot_min = u64::from_le_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
            content[pos + 4],
            content[pos + 5],
            content[pos + 6],
            content[pos + 7],
        ]);
        pos += 8;
        let snapshot_max = match content.get(pos).copied() {
            Some(1) => {
                pos += 1;
                if pos + 8 > content.len() {
                    return Err(RdbFileError::InvalidOperation(
                        "truncated native manifest snapshot_max".to_string(),
                    ));
                }
                let value = u64::from_le_bytes([
                    content[pos],
                    content[pos + 1],
                    content[pos + 2],
                    content[pos + 3],
                    content[pos + 4],
                    content[pos + 5],
                    content[pos + 6],
                    content[pos + 7],
                ]);
                pos += 8;
                Some(value)
            }
            Some(_) => {
                pos += 1;
                None
            }
            None => None,
        };

        recent_events.push(NativeManifestEntrySummary {
            collection,
            object_key,
            kind,
            block_index,
            block_checksum: u128::from_le_bytes(checksum_bytes),
            snapshot_min,
            snapshot_max,
        });
    }

    Ok(NativeManifestSummary {
        sequence,
        event_count,
        events_complete,
        omitted_event_count,
        recent_events,
    })
}

pub fn encode_native_registry_summary_page(summary: &NativeRegistrySummary) -> Vec<u8> {
    let mut data = Vec::with_capacity(2048);
    data.extend_from_slice(NATIVE_REGISTRY_MAGIC);
    data.extend_from_slice(&summary.collection_count.to_le_bytes());
    data.extend_from_slice(&summary.index_count.to_le_bytes());
    data.extend_from_slice(&summary.graph_projection_count.to_le_bytes());
    data.extend_from_slice(&summary.analytics_job_count.to_le_bytes());
    data.extend_from_slice(&summary.vector_artifact_count.to_le_bytes());
    data.push(u8::from(summary.collections_complete));
    data.push(u8::from(summary.indexes_complete));
    data.push(u8::from(summary.graph_projections_complete));
    data.push(u8::from(summary.analytics_jobs_complete));
    data.push(u8::from(summary.vector_artifacts_complete));
    data.extend_from_slice(&summary.omitted_collection_count.to_le_bytes());
    data.extend_from_slice(&summary.omitted_index_count.to_le_bytes());
    data.extend_from_slice(&summary.omitted_graph_projection_count.to_le_bytes());
    data.extend_from_slice(&summary.omitted_analytics_job_count.to_le_bytes());
    data.extend_from_slice(&summary.omitted_vector_artifact_count.to_le_bytes());
    data.extend_from_slice(&(summary.collection_names.len() as u32).to_le_bytes());
    data.extend_from_slice(&(summary.indexes.len() as u32).to_le_bytes());
    data.extend_from_slice(&(summary.graph_projections.len() as u32).to_le_bytes());
    data.extend_from_slice(&(summary.analytics_jobs.len() as u32).to_le_bytes());
    data.extend_from_slice(&(summary.vector_artifacts.len() as u32).to_le_bytes());

    for name in &summary.collection_names {
        push_native_string(&mut data, name);
    }
    for index in &summary.indexes {
        push_native_string(&mut data, &index.name);
        push_native_string(&mut data, &index.kind);
        match &index.collection {
            Some(collection) => {
                data.push(1);
                push_native_string(&mut data, collection);
            }
            None => data.push(0),
        }
        data.push(u8::from(index.enabled));
        data.extend_from_slice(&index.entries.to_le_bytes());
        data.extend_from_slice(&index.estimated_memory_bytes.to_le_bytes());
        match index.last_refresh_ms {
            Some(value) => {
                data.push(1);
                data.extend_from_slice(&value.to_le_bytes());
            }
            None => data.push(0),
        }
        push_native_string(&mut data, &index.backend);
    }
    for projection in &summary.graph_projections {
        push_native_string(&mut data, &projection.name);
        push_native_string(&mut data, &projection.source);
        data.extend_from_slice(&projection.created_at_unix_ms.to_le_bytes());
        data.extend_from_slice(&projection.updated_at_unix_ms.to_le_bytes());
        push_native_string_list(&mut data, &projection.node_labels);
        push_native_string_list(&mut data, &projection.node_types);
        push_native_string_list(&mut data, &projection.edge_labels);
        match projection.last_materialized_sequence {
            Some(value) => {
                data.push(1);
                data.extend_from_slice(&value.to_le_bytes());
            }
            None => data.push(0),
        }
    }
    for job in &summary.analytics_jobs {
        push_native_string(&mut data, &job.id);
        push_native_string(&mut data, &job.kind);
        match &job.projection {
            Some(projection) => {
                data.push(1);
                push_native_string(&mut data, projection);
            }
            None => data.push(0),
        }
        push_native_string(&mut data, &job.state);
        data.extend_from_slice(&job.created_at_unix_ms.to_le_bytes());
        data.extend_from_slice(&job.updated_at_unix_ms.to_le_bytes());
        match job.last_run_sequence {
            Some(value) => {
                data.push(1);
                data.extend_from_slice(&value.to_le_bytes());
            }
            None => data.push(0),
        }
        let metadata_count = job.metadata.len().min(u16::MAX as usize) as u16;
        data.extend_from_slice(&metadata_count.to_le_bytes());
        for (key, value) in job.metadata.iter().take(metadata_count as usize) {
            push_native_string(&mut data, key);
            push_native_string(&mut data, value);
        }
    }
    for artifact in &summary.vector_artifacts {
        push_native_string(&mut data, &artifact.collection);
        push_native_string(&mut data, &artifact.artifact_kind);
        data.extend_from_slice(&artifact.vector_count.to_le_bytes());
        data.extend_from_slice(&artifact.dimension.to_le_bytes());
        data.extend_from_slice(&artifact.max_layer.to_le_bytes());
        data.extend_from_slice(&artifact.serialized_bytes.to_le_bytes());
        data.extend_from_slice(&artifact.checksum.to_le_bytes());
    }

    data
}

pub fn decode_native_registry_summary_page(content: &[u8]) -> RdbFileResult<NativeRegistrySummary> {
    if content.len() < 77 || &content[0..4] != NATIVE_REGISTRY_MAGIC {
        return Err(RdbFileError::InvalidOperation(
            "invalid native registry summary page".to_string(),
        ));
    }

    let collection_count = u32::from_le_bytes([content[4], content[5], content[6], content[7]]);
    let index_count = u32::from_le_bytes([content[8], content[9], content[10], content[11]]);
    let graph_projection_count =
        u32::from_le_bytes([content[12], content[13], content[14], content[15]]);
    let analytics_job_count =
        u32::from_le_bytes([content[16], content[17], content[18], content[19]]);
    let vector_artifact_count =
        u32::from_le_bytes([content[20], content[21], content[22], content[23]]);
    let collections_complete = content[24] == 1;
    let indexes_complete = content[25] == 1;
    let graph_projections_complete = content[26] == 1;
    let analytics_jobs_complete = content[27] == 1;
    let vector_artifacts_complete = content[28] == 1;
    let omitted_collection_count =
        u32::from_le_bytes([content[29], content[30], content[31], content[32]]);
    let omitted_index_count =
        u32::from_le_bytes([content[33], content[34], content[35], content[36]]);
    let omitted_graph_projection_count =
        u32::from_le_bytes([content[37], content[38], content[39], content[40]]);
    let omitted_analytics_job_count =
        u32::from_le_bytes([content[41], content[42], content[43], content[44]]);
    let omitted_vector_artifact_count =
        u32::from_le_bytes([content[45], content[46], content[47], content[48]]);
    let collection_sample_count =
        u32::from_le_bytes([content[49], content[50], content[51], content[52]]) as usize;
    let index_sample_count =
        u32::from_le_bytes([content[53], content[54], content[55], content[56]]) as usize;
    let projection_sample_count =
        u32::from_le_bytes([content[57], content[58], content[59], content[60]]) as usize;
    let job_sample_count =
        u32::from_le_bytes([content[61], content[62], content[63], content[64]]) as usize;
    let vector_artifact_sample_count =
        u32::from_le_bytes([content[65], content[66], content[67], content[68]]) as usize;

    let mut pos = 69usize;
    let mut collection_names = Vec::with_capacity(collection_sample_count);
    for _ in 0..collection_sample_count {
        collection_names.push(read_native_string(content, &mut pos)?);
    }

    let mut indexes = Vec::with_capacity(index_sample_count);
    for _ in 0..index_sample_count {
        let name = read_native_string(content, &mut pos)?;
        let kind = read_native_string(content, &mut pos)?;
        let collection = read_flagged_string(content, &mut pos)?;
        let enabled = content.get(pos).copied().unwrap_or(0) == 1;
        pos = pos.saturating_add(1);
        if pos + 16 > content.len() {
            break;
        }
        let entries = read_u64(content, &mut pos);
        let estimated_memory_bytes = read_u64(content, &mut pos);
        let last_refresh_ms =
            read_flagged_u128(content, &mut pos, "native registry refresh timestamp")?;
        let backend = read_native_string(content, &mut pos)?;
        indexes.push(NativeRegistryIndexSummary {
            name,
            kind,
            collection,
            enabled,
            entries,
            estimated_memory_bytes,
            last_refresh_ms,
            backend,
        });
    }

    let mut graph_projections = Vec::with_capacity(projection_sample_count);
    for _ in 0..projection_sample_count {
        let name = read_native_string(content, &mut pos)?;
        let source = read_native_string(content, &mut pos)?;
        if pos + 32 > content.len() {
            break;
        }
        let created_at_unix_ms = read_u128(content, &mut pos);
        let updated_at_unix_ms = read_u128(content, &mut pos);
        let node_labels = read_native_string_list(content, &mut pos)?;
        let node_types = read_native_string_list(content, &mut pos)?;
        let edge_labels = read_native_string_list(content, &mut pos)?;
        let last_materialized_sequence = read_flagged_u64(
            content,
            &mut pos,
            "native projection materialization sequence",
        )?;
        graph_projections.push(NativeRegistryProjectionSummary {
            name,
            source,
            created_at_unix_ms,
            updated_at_unix_ms,
            node_labels,
            node_types,
            edge_labels,
            last_materialized_sequence,
        });
    }

    let mut analytics_jobs = Vec::with_capacity(job_sample_count);
    for _ in 0..job_sample_count {
        let id = read_native_string(content, &mut pos)?;
        let kind = read_native_string(content, &mut pos)?;
        let projection = read_flagged_string(content, &mut pos)?;
        let state = read_native_string(content, &mut pos)?;
        if pos + 32 > content.len() {
            break;
        }
        let created_at_unix_ms = read_u128(content, &mut pos);
        let updated_at_unix_ms = read_u128(content, &mut pos);
        let last_run_sequence =
            read_flagged_u64(content, &mut pos, "native analytics job run sequence")?;
        if pos + 2 > content.len() {
            return Err(RdbFileError::InvalidOperation(
                "truncated native analytics job metadata count".to_string(),
            ));
        }
        let metadata_count = u16::from_le_bytes([content[pos], content[pos + 1]]) as usize;
        pos += 2;
        let mut metadata = BTreeMap::new();
        for _ in 0..metadata_count {
            let key = read_native_string(content, &mut pos)?;
            let value = read_native_string(content, &mut pos)?;
            metadata.insert(key, value);
        }
        analytics_jobs.push(NativeRegistryJobSummary {
            id,
            kind,
            projection,
            state,
            created_at_unix_ms,
            updated_at_unix_ms,
            last_run_sequence,
            metadata,
        });
    }

    let mut vector_artifacts = Vec::with_capacity(vector_artifact_sample_count);
    for _ in 0..vector_artifact_sample_count {
        let collection = read_native_string(content, &mut pos)?;
        let artifact_kind = read_native_string(content, &mut pos)?;
        if pos + 32 > content.len() {
            break;
        }
        let vector_count = read_u64(content, &mut pos);
        let dimension = read_u32(content, &mut pos);
        let max_layer = read_u32(content, &mut pos);
        let serialized_bytes = read_u64(content, &mut pos);
        let checksum = read_u64(content, &mut pos);
        vector_artifacts.push(NativeVectorArtifactSummary {
            collection,
            artifact_kind,
            vector_count,
            dimension,
            max_layer,
            serialized_bytes,
            checksum,
        });
    }

    Ok(NativeRegistrySummary {
        collection_count,
        index_count,
        graph_projection_count,
        analytics_job_count,
        vector_artifact_count,
        collections_complete,
        indexes_complete,
        graph_projections_complete,
        analytics_jobs_complete,
        vector_artifacts_complete,
        omitted_collection_count,
        omitted_index_count,
        omitted_graph_projection_count,
        omitted_analytics_job_count,
        omitted_vector_artifact_count,
        collection_names,
        indexes,
        graph_projections,
        analytics_jobs,
        vector_artifacts,
    })
}

pub fn encode_native_recovery_summary_page(summary: &NativeRecoverySummary) -> Vec<u8> {
    let mut data = Vec::with_capacity(2048);
    data.extend_from_slice(NATIVE_RECOVERY_MAGIC);
    data.extend_from_slice(&summary.snapshot_count.to_le_bytes());
    data.extend_from_slice(&summary.export_count.to_le_bytes());
    data.push(u8::from(summary.snapshots_complete));
    data.push(u8::from(summary.exports_complete));
    data.extend_from_slice(&summary.omitted_snapshot_count.to_le_bytes());
    data.extend_from_slice(&summary.omitted_export_count.to_le_bytes());
    data.extend_from_slice(&(summary.snapshots.len() as u32).to_le_bytes());
    data.extend_from_slice(&(summary.exports.len() as u32).to_le_bytes());

    for snapshot in &summary.snapshots {
        data.extend_from_slice(&snapshot.snapshot_id.to_le_bytes());
        data.extend_from_slice(&snapshot.created_at_unix_ms.to_le_bytes());
        data.extend_from_slice(&snapshot.superblock_sequence.to_le_bytes());
        data.extend_from_slice(&snapshot.collection_count.to_le_bytes());
        data.extend_from_slice(&snapshot.total_entities.to_le_bytes());
    }

    for export in &summary.exports {
        push_native_string(&mut data, &export.name);
        data.extend_from_slice(&export.created_at_unix_ms.to_le_bytes());
        match export.snapshot_id {
            Some(snapshot_id) => {
                data.push(1);
                data.extend_from_slice(&snapshot_id.to_le_bytes());
            }
            None => data.push(0),
        }
        data.extend_from_slice(&export.superblock_sequence.to_le_bytes());
        data.extend_from_slice(&export.collection_count.to_le_bytes());
        data.extend_from_slice(&export.total_entities.to_le_bytes());
    }

    data
}

pub fn decode_native_recovery_summary_page(content: &[u8]) -> RdbFileResult<NativeRecoverySummary> {
    if content.len() < 30 || &content[0..4] != NATIVE_RECOVERY_MAGIC {
        return Err(RdbFileError::InvalidOperation(
            "invalid native recovery summary page".to_string(),
        ));
    }

    let snapshot_count = u32::from_le_bytes([content[4], content[5], content[6], content[7]]);
    let export_count = u32::from_le_bytes([content[8], content[9], content[10], content[11]]);
    let snapshots_complete = content[12] == 1;
    let exports_complete = content[13] == 1;
    let omitted_snapshot_count =
        u32::from_le_bytes([content[14], content[15], content[16], content[17]]);
    let omitted_export_count =
        u32::from_le_bytes([content[18], content[19], content[20], content[21]]);
    let snapshot_sample_count =
        u32::from_le_bytes([content[22], content[23], content[24], content[25]]) as usize;
    let export_sample_count =
        u32::from_le_bytes([content[26], content[27], content[28], content[29]]) as usize;

    let mut pos = 30usize;
    let mut snapshots = Vec::with_capacity(snapshot_sample_count);
    for _ in 0..snapshot_sample_count {
        if pos + 44 > content.len() {
            break;
        }
        let snapshot_id = read_u64(content, &mut pos);
        let created_at_unix_ms = read_u128(content, &mut pos);
        let superblock_sequence = read_u64(content, &mut pos);
        let collection_count = read_u32(content, &mut pos);
        let total_entities = read_u64(content, &mut pos);
        snapshots.push(NativeSnapshotSummary {
            snapshot_id,
            created_at_unix_ms,
            superblock_sequence,
            collection_count,
            total_entities,
        });
    }

    let mut exports = Vec::with_capacity(export_sample_count);
    for _ in 0..export_sample_count {
        let name = read_native_string(content, &mut pos)?;
        if pos + 16 > content.len() {
            break;
        }
        let created_at_unix_ms = read_u128(content, &mut pos);
        let snapshot_id = read_flagged_u64(content, &mut pos, "native export snapshot id")?;
        if pos + 20 > content.len() {
            break;
        }
        let superblock_sequence = read_u64(content, &mut pos);
        let collection_count = read_u32(content, &mut pos);
        let total_entities = read_u64(content, &mut pos);
        exports.push(NativeExportSummary {
            name,
            created_at_unix_ms,
            snapshot_id,
            superblock_sequence,
            collection_count,
            total_entities,
        });
    }

    Ok(NativeRecoverySummary {
        snapshot_count,
        export_count,
        snapshots_complete,
        exports_complete,
        omitted_snapshot_count,
        omitted_export_count,
        snapshots,
        exports,
    })
}

pub fn encode_native_catalog_summary_page(summary: &NativeCatalogSummary) -> Vec<u8> {
    let mut data = Vec::with_capacity(2048);
    data.extend_from_slice(NATIVE_CATALOG_MAGIC);
    data.extend_from_slice(&summary.collection_count.to_le_bytes());
    data.extend_from_slice(&summary.total_entities.to_le_bytes());
    data.push(u8::from(summary.collections_complete));
    data.extend_from_slice(&summary.omitted_collection_count.to_le_bytes());
    data.extend_from_slice(&(summary.collections.len() as u32).to_le_bytes());
    for collection in &summary.collections {
        push_native_string(&mut data, &collection.name);
        data.extend_from_slice(&collection.entities.to_le_bytes());
        data.extend_from_slice(&collection.cross_refs.to_le_bytes());
        data.extend_from_slice(&collection.segments.to_le_bytes());
    }
    data
}

pub fn decode_native_catalog_summary_page(content: &[u8]) -> RdbFileResult<NativeCatalogSummary> {
    if content.len() < 25 || &content[0..4] != NATIVE_CATALOG_MAGIC {
        return Err(RdbFileError::InvalidOperation(
            "invalid native catalog summary page".to_string(),
        ));
    }

    let collection_count = u32::from_le_bytes([content[4], content[5], content[6], content[7]]);
    let mut pos = 8usize;
    let total_entities = read_u64(content, &mut pos);
    let collections_complete = content[16] == 1;
    let omitted_collection_count =
        u32::from_le_bytes([content[17], content[18], content[19], content[20]]);
    let sample_count =
        u32::from_le_bytes([content[21], content[22], content[23], content[24]]) as usize;

    let mut pos = 25usize;
    let mut collections = Vec::with_capacity(sample_count);
    for _ in 0..sample_count {
        let name = read_native_string(content, &mut pos)?;
        if pos + 20 > content.len() {
            break;
        }
        let entities = read_u64(content, &mut pos);
        let cross_refs = read_u64(content, &mut pos);
        let segments = read_u32(content, &mut pos);
        collections.push(NativeCatalogCollectionSummary {
            name,
            entities,
            cross_refs,
            segments,
        });
    }

    Ok(NativeCatalogSummary {
        collection_count,
        total_entities,
        collections_complete,
        omitted_collection_count,
        collections,
    })
}

pub fn encode_native_metadata_state_summary_page(summary: &NativeMetadataStateSummary) -> Vec<u8> {
    let mut data = Vec::with_capacity(512);
    data.extend_from_slice(NATIVE_METADATA_STATE_MAGIC);
    push_native_string(&mut data, &summary.protocol_version);
    data.extend_from_slice(&summary.generated_at_unix_ms.to_le_bytes());
    match &summary.last_loaded_from {
        Some(value) => {
            data.push(1);
            push_native_string(&mut data, value);
        }
        None => data.push(0),
    }
    match summary.last_healed_at_unix_ms {
        Some(value) => {
            data.push(1);
            data.extend_from_slice(&value.to_le_bytes());
        }
        None => data.push(0),
    }
    data
}

pub fn decode_native_metadata_state_summary_page(
    content: &[u8],
) -> RdbFileResult<NativeMetadataStateSummary> {
    if content.len() < 22 || &content[0..4] != NATIVE_METADATA_STATE_MAGIC {
        return Err(RdbFileError::InvalidOperation(
            "invalid native metadata state page".to_string(),
        ));
    }

    let mut pos = 4usize;
    let protocol_version = read_native_string(content, &mut pos)?;
    if pos + 16 > content.len() {
        return Err(RdbFileError::InvalidOperation(
            "truncated native metadata state timestamp".to_string(),
        ));
    }
    let generated_at_unix_ms = read_u128(content, &mut pos);
    let last_loaded_from = read_flagged_string(content, &mut pos)?;
    let last_healed_at_unix_ms =
        read_flagged_u128(content, &mut pos, "native metadata heal timestamp")?;

    Ok(NativeMetadataStateSummary {
        protocol_version,
        generated_at_unix_ms,
        last_loaded_from,
        last_healed_at_unix_ms,
    })
}

pub const NATIVE_BLOB_PAGE_HEADER_BYTES: usize = 12;

pub fn native_blob_chunk_capacity(page_size: usize, page_header_size: usize) -> usize {
    page_size - page_header_size - NATIVE_BLOB_PAGE_HEADER_BYTES
}

pub fn encode_native_blob_page(next_page: u32, chunk: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(chunk.len() + NATIVE_BLOB_PAGE_HEADER_BYTES);
    data.extend_from_slice(NATIVE_BLOB_MAGIC);
    data.extend_from_slice(&next_page.to_le_bytes());
    data.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
    data.extend_from_slice(chunk);
    data
}

pub fn decode_native_blob_page(content: &[u8]) -> RdbFileResult<(u32, Vec<u8>)> {
    if content.len() < NATIVE_BLOB_PAGE_HEADER_BYTES || &content[0..4] != NATIVE_BLOB_MAGIC {
        return Err(RdbFileError::InvalidOperation(
            "invalid native blob page".to_string(),
        ));
    }
    let next_page = u32::from_le_bytes([content[4], content[5], content[6], content[7]]);
    let chunk_len = u32::from_le_bytes([content[8], content[9], content[10], content[11]]) as usize;
    if NATIVE_BLOB_PAGE_HEADER_BYTES + chunk_len > content.len() {
        return Err(RdbFileError::InvalidOperation(
            "truncated native blob page".to_string(),
        ));
    }
    Ok((
        next_page,
        content[NATIVE_BLOB_PAGE_HEADER_BYTES..NATIVE_BLOB_PAGE_HEADER_BYTES + chunk_len].to_vec(),
    ))
}

pub fn encode_native_vector_artifact_store_page(
    summaries: &[NativeVectorArtifactPageSummary],
) -> Vec<u8> {
    let mut data = Vec::with_capacity(1024 + summaries.len() * 64);
    data.extend_from_slice(NATIVE_VECTOR_ARTIFACT_MAGIC);
    data.extend_from_slice(&(summaries.len() as u32).to_le_bytes());
    for summary in summaries {
        push_native_string(&mut data, &summary.collection);
        push_native_string(&mut data, &summary.artifact_kind);
        data.extend_from_slice(&summary.root_page.to_le_bytes());
        data.extend_from_slice(&summary.page_count.to_le_bytes());
        data.extend_from_slice(&summary.byte_len.to_le_bytes());
        data.extend_from_slice(&summary.checksum.to_le_bytes());
    }
    data
}

pub fn decode_native_vector_artifact_store_page(
    content: &[u8],
) -> RdbFileResult<Vec<NativeVectorArtifactPageSummary>> {
    if content.len() < 8 || &content[0..4] != NATIVE_VECTOR_ARTIFACT_MAGIC {
        return Err(RdbFileError::InvalidOperation(
            "invalid native vector artifact store page".to_string(),
        ));
    }
    let count = u32::from_le_bytes([content[4], content[5], content[6], content[7]]) as usize;
    let mut pos = 8usize;
    let mut summaries = Vec::with_capacity(count);
    for _ in 0..count {
        let collection = read_native_string(content, &mut pos)?;
        let artifact_kind = read_native_string(content, &mut pos)?;
        if pos + 24 > content.len() {
            break;
        }
        let root_page = read_u32(content, &mut pos);
        let page_count = read_u32(content, &mut pos);
        let byte_len = read_u64(content, &mut pos);
        let checksum = read_u64(content, &mut pos);
        summaries.push(NativeVectorArtifactPageSummary {
            collection,
            artifact_kind,
            root_page,
            page_count,
            byte_len,
            checksum,
        });
    }
    Ok(summaries)
}

fn native_manifest_kind_to_byte(kind: ManifestEventKind) -> u8 {
    match kind {
        ManifestEventKind::Insert => 1,
        ManifestEventKind::Update => 2,
        ManifestEventKind::Remove => 3,
        ManifestEventKind::Checkpoint => 4,
    }
}

fn push_native_string(data: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    let len = bytes.len().min(u16::MAX as usize) as u16;
    data.extend_from_slice(&len.to_le_bytes());
    data.extend_from_slice(&bytes[..len as usize]);
}

fn read_native_string(content: &[u8], pos: &mut usize) -> RdbFileResult<String> {
    if *pos + 2 > content.len() {
        return Err(RdbFileError::InvalidOperation(
            "truncated native string length".to_string(),
        ));
    }
    let len = u16::from_le_bytes([content[*pos], content[*pos + 1]]) as usize;
    *pos += 2;
    if *pos + len > content.len() {
        return Err(RdbFileError::InvalidOperation(
            "truncated native string payload".to_string(),
        ));
    }
    let value = String::from_utf8(content[*pos..*pos + len].to_vec())
        .map_err(|err| RdbFileError::InvalidOperation(err.to_string()))?;
    *pos += len;
    Ok(value)
}

fn push_native_string_list(data: &mut Vec<u8>, values: &[String]) {
    let count = values.len().min(u16::MAX as usize) as u16;
    data.extend_from_slice(&count.to_le_bytes());
    for value in values.iter().take(count as usize) {
        push_native_string(data, value);
    }
}

fn read_native_string_list(content: &[u8], pos: &mut usize) -> RdbFileResult<Vec<String>> {
    if *pos + 2 > content.len() {
        return Err(RdbFileError::InvalidOperation(
            "truncated native string list count".to_string(),
        ));
    }
    let count = u16::from_le_bytes([content[*pos], content[*pos + 1]]) as usize;
    *pos += 2;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(read_native_string(content, pos)?);
    }
    Ok(values)
}

fn read_flagged_string(content: &[u8], pos: &mut usize) -> RdbFileResult<Option<String>> {
    match content.get(*pos).copied() {
        Some(1) => {
            *pos += 1;
            Ok(Some(read_native_string(content, pos)?))
        }
        Some(_) => {
            *pos += 1;
            Ok(None)
        }
        None => Ok(None),
    }
}

fn read_flagged_u64(content: &[u8], pos: &mut usize, label: &str) -> RdbFileResult<Option<u64>> {
    match content.get(*pos).copied() {
        Some(1) => {
            *pos += 1;
            if *pos + 8 > content.len() {
                return Err(RdbFileError::InvalidOperation(format!("truncated {label}")));
            }
            Ok(Some(read_u64(content, pos)))
        }
        Some(_) => {
            *pos += 1;
            Ok(None)
        }
        None => Ok(None),
    }
}

fn read_flagged_u128(content: &[u8], pos: &mut usize, label: &str) -> RdbFileResult<Option<u128>> {
    match content.get(*pos).copied() {
        Some(1) => {
            *pos += 1;
            if *pos + 16 > content.len() {
                return Err(RdbFileError::InvalidOperation(format!("truncated {label}")));
            }
            Ok(Some(read_u128(content, pos)))
        }
        Some(_) => {
            *pos += 1;
            Ok(None)
        }
        None => Ok(None),
    }
}

fn read_u32(content: &[u8], pos: &mut usize) -> u32 {
    let value = u32::from_le_bytes([
        content[*pos],
        content[*pos + 1],
        content[*pos + 2],
        content[*pos + 3],
    ]);
    *pos += 4;
    value
}

fn read_u64(content: &[u8], pos: &mut usize) -> u64 {
    let value = u64::from_le_bytes([
        content[*pos],
        content[*pos + 1],
        content[*pos + 2],
        content[*pos + 3],
        content[*pos + 4],
        content[*pos + 5],
        content[*pos + 6],
        content[*pos + 7],
    ]);
    *pos += 8;
    value
}

fn read_u128(content: &[u8], pos: &mut usize) -> u128 {
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&content[*pos..*pos + 16]);
    *pos += 16;
    u128::from_le_bytes(bytes)
}

fn native_manifest_kind_from_byte(byte: u8) -> &'static str {
    match byte {
        1 => "insert",
        2 => "update",
        3 => "remove",
        4 => "checkpoint",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physical_metadata::BlockReference;

    #[test]
    fn native_store_dump_header_and_crc_footer_are_canonical() {
        let mut bytes = encode_native_store_header(STORE_VERSION_CURRENT);
        bytes.extend_from_slice(b"payload");
        append_native_store_crc32_footer(&mut bytes);

        let version = decode_native_store_header(&bytes).unwrap();
        assert_eq!(version, STORE_VERSION_CURRENT);

        let original_len = bytes.len();
        verify_native_store_crc32_footer(&mut bytes, version).unwrap();
        assert_eq!(bytes.len(), original_len - 4);
        assert_eq!(&bytes[0..4], STORE_MAGIC);
        assert_eq!(&bytes[8..], b"payload");

        let mut corrupt = encode_native_store_header(STORE_VERSION_CURRENT);
        corrupt.extend_from_slice(b"payload");
        append_native_store_crc32_footer(&mut corrupt);
        corrupt[8] ^= 0xff;
        assert!(verify_native_store_crc32_footer(&mut corrupt, STORE_VERSION_CURRENT).is_err());
    }

    #[test]
    fn native_store_magic_matcher_is_canonical() {
        assert!(native_store_magic_matches(b"RDSTpayload"));
        assert!(!native_store_magic_matches(b"RDS"));
        assert!(!native_store_magic_matches(b"NOPEpayload"));
    }

    #[test]
    fn native_entity_record_frame_round_trips_payloads() {
        let encoded = encode_native_entity_record_frame(b"entity", Some(b"metadata"));
        let decoded = decode_native_entity_record_frame(&encoded)
            .expect("decode frame")
            .expect("entity record frame");

        assert_eq!(decoded.entity, b"entity");
        assert_eq!(decoded.metadata, b"metadata");
    }

    #[test]
    fn native_entity_record_frame_handles_empty_metadata_and_legacy_payloads() {
        let encoded = encode_native_entity_record_frame(b"entity", None);
        let decoded = decode_native_entity_record_frame(&encoded)
            .expect("decode frame")
            .expect("entity record frame");

        assert_eq!(decoded.entity, b"entity");
        assert_eq!(decoded.metadata, b"");
        assert!(decode_native_entity_record_frame(b"legacy-entity")
            .expect("decode legacy")
            .is_none());
    }

    #[test]
    fn native_entity_record_frame_rejects_truncated_payloads() {
        let mut encoded = encode_native_entity_record_frame(b"entity", Some(b"metadata"));
        encoded.truncate(encoded.len() - 1);

        assert!(decode_native_entity_record_frame(&encoded).is_err());
    }

    #[test]
    fn native_metadata_overflow_headers_round_trip() {
        let mut page1 = [0u8; METADATA_OVERFLOW_HEADER_BYTES];
        encode_native_metadata_overflow_header(
            &mut page1,
            NativeMetadataOverflowHeader {
                format_version: 9,
                total_payload_bytes: 1024,
                next_overflow_page_id: 42,
            },
        )
        .expect("encode page1 header");
        assert_eq!(
            decode_native_metadata_overflow_header(&page1)
                .expect("decode page1 header")
                .expect("overflow header"),
            NativeMetadataOverflowHeader {
                format_version: 9,
                total_payload_bytes: 1024,
                next_overflow_page_id: 42,
            }
        );
        assert!(decode_native_metadata_overflow_header(b"RDM2payload")
            .expect("decode non-overflow")
            .is_none());

        let mut continuation = [0u8; METADATA_OVERFLOW_CONTINUATION_HEADER_BYTES];
        encode_native_metadata_overflow_continuation_header(
            &mut continuation,
            NativeMetadataOverflowContinuationHeader {
                next_overflow_page_id: 77,
                chunk_bytes: 2048,
            },
        )
        .expect("encode continuation header");
        assert_eq!(
            decode_native_metadata_overflow_continuation_header(&continuation)
                .expect("decode continuation header"),
            NativeMetadataOverflowContinuationHeader {
                next_overflow_page_id: 77,
                chunk_bytes: 2048,
            }
        );
    }

    #[test]
    fn native_paged_metadata_header_round_trips_and_skips_legacy_payloads() {
        let mut bytes = Vec::new();
        encode_native_paged_metadata_header(
            &mut bytes,
            NativePagedMetadataHeader {
                format_version: 9,
                collection_count: 200,
            },
        );

        assert_eq!(
            decode_native_paged_metadata_header(&bytes)
                .expect("decode header")
                .expect("metadata header"),
            NativePagedMetadataHeader {
                format_version: 9,
                collection_count: 200,
            }
        );
        assert!(decode_native_paged_metadata_header(&123u32.to_le_bytes())
            .expect("decode legacy")
            .is_none());

        assert!(decode_native_paged_metadata_header(b"RDM2").is_err());
    }

    #[test]
    fn native_len_prefixed_string_and_bytes_round_trip() {
        let mut bytes = Vec::new();
        encode_native_len_prefixed_str(&mut bytes, "collection");
        encode_native_len_prefixed_bytes(&mut bytes, b"\0payload");

        let mut pos = 0;
        assert_eq!(
            decode_native_len_prefixed_string(&bytes, &mut pos).expect("decode string"),
            "collection"
        );
        assert_eq!(
            decode_native_len_prefixed_bytes(&bytes, &mut pos).expect("decode bytes"),
            b"\0payload"
        );
        assert_eq!(pos, bytes.len());

        let mut truncated = bytes.clone();
        truncated.pop();
        let mut pos = 0;
        decode_native_len_prefixed_string(&truncated, &mut pos).expect("decode first string");
        assert!(decode_native_len_prefixed_bytes(&truncated, &mut pos).is_err());
    }

    #[test]
    fn native_paged_collection_root_round_trips() {
        let mut bytes = Vec::new();
        encode_native_paged_collection_root(&mut bytes, "events", 42);

        let mut pos = 0;
        assert_eq!(
            decode_native_paged_collection_root(&bytes, &mut pos).expect("decode root"),
            NativePagedCollectionRoot {
                collection: "events".to_string(),
                root_page: 42,
            }
        );
        assert_eq!(pos, bytes.len());

        let mut truncated = bytes.clone();
        truncated.pop();
        let mut pos = 0;
        assert!(decode_native_paged_collection_root(&truncated, &mut pos).is_err());
    }

    #[test]
    fn native_collection_roots_page_round_trips() {
        let roots = BTreeMap::from([("events".to_string(), 10), ("users".to_string(), 42)]);
        let bytes = encode_native_collection_roots_page(&roots);
        assert_eq!(decode_native_collection_roots_page(&bytes).unwrap(), roots);
    }

    #[test]
    fn native_manifest_summary_page_round_trips_sample() {
        let events: Vec<ManifestEvent> = (0..20)
            .map(|i| ManifestEvent {
                collection: "events".to_string(),
                object_key: format!("k{i}"),
                kind: ManifestEventKind::Checkpoint,
                block: BlockReference {
                    index: i,
                    checksum: i as u128 + 1,
                },
                snapshot_min: i,
                snapshot_max: Some(i + 100),
            })
            .collect();

        let bytes = encode_native_manifest_summary_page(7, &events);
        let decoded = decode_native_manifest_summary_page(&bytes).unwrap();
        assert_eq!(decoded.sequence, 7);
        assert_eq!(decoded.event_count, 20);
        assert!(!decoded.events_complete);
        assert_eq!(decoded.omitted_event_count, 4);
        assert_eq!(decoded.recent_events.len(), NATIVE_MANIFEST_SAMPLE_LIMIT);
        assert_eq!(decoded.recent_events[0].object_key, "k4");
    }
}
