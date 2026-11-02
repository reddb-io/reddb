//! Generation-bound resume counters for the server's immutable disk base.
//! The enclosing metadata checksum covers these bytes and their collection roots.
//! Writers must fence PAGE_DISK_RESIDENCY_FILE_VERSION in both superblocks first.
//! Secondary roots additionally require PAGE_SECONDARY_INDEX_FILE_VERSION.

use crate::native_store::*;
use crate::{RdbFileError, RdbFileResult, METADATA_HEADER_BYTES};
use std::collections::BTreeMap;

const MAGIC: &[u8; 4] = b"RDRS";
const VERSION: u32 = 4;
const SECONDARY_VERSION: u32 = 5;
const SECONDARY_COUNT_MAX: usize = 4096;
const SECONDARY_NAME_BYTES_MAX: usize = 1024;

/// Identity of an immutable derived tree. The enclosing collection root binds
/// its contents to the exact data generation; the metadata checksum protects
/// that binding together with the codec and definition fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondaryIndexResume {
    pub root_page: u32,
    pub codec_version: u32,
    pub definition: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionResume {
    pub root_page: u32,
    pub entities: u64,
    pub entity_id_next: u64,
    pub row_id_next: u64,
    pub sequence_id_next: u64,
    pub key_index_root: Option<u32>,
    pub secondary_indices: BTreeMap<String, SecondaryIndexResume>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogResume {
    pub entity_id_next: u64,
    pub xid_high_water_mark: Option<u64>,
    pub collections: BTreeMap<String, CollectionResume>,
}

fn invalid(message: &str) -> RdbFileError {
    RdbFileError::InvalidOperation(message.into())
}

fn take<'a>(data: &'a [u8], position: &mut usize, count: usize) -> RdbFileResult<&'a [u8]> {
    let end = position
        .checked_add(count)
        .ok_or_else(|| invalid("resume length overflow"))?;
    let bytes = data
        .get(*position..end)
        .ok_or_else(|| invalid("truncated catalog resume"))?;
    *position = end;
    Ok(bytes)
}

fn read_u32(data: &[u8], position: &mut usize) -> RdbFileResult<u32> {
    Ok(u32::from_le_bytes(
        take(data, position, 4)?
            .try_into()
            .expect("invariant: four-byte slice"),
    ))
}

fn read_u64(data: &[u8], position: &mut usize) -> RdbFileResult<u64> {
    Ok(u64::from_le_bytes(
        take(data, position, 8)?
            .try_into()
            .expect("invariant: eight-byte slice"),
    ))
}

pub fn encode_catalog_resume(out: &mut Vec<u8>, resume: &CatalogResume) -> RdbFileResult<()> {
    if resume.entity_id_next == 0
        || resume.collections.values().any(|state| {
            state.entity_id_next == 0
                || state.row_id_next == 0
                || (state.root_page == 0 && state.entities != 0)
        })
    {
        return Err(invalid("invalid catalog resume counters"));
    }
    validate_key_roots(&resume.collections)?;
    let has_secondary = resume
        .collections
        .values()
        .any(|state| !state.secondary_indices.is_empty());
    let version = if has_secondary {
        SECONDARY_VERSION
    } else {
        VERSION
    };
    let count = u32::try_from(resume.collections.len())
        .map_err(|_| invalid("too many resume collections"))?;
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&resume.entity_id_next.to_le_bytes());
    out.push(u8::from(resume.xid_high_water_mark.is_some()));
    out.extend_from_slice(&resume.xid_high_water_mark.unwrap_or(0).to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    for (name, state) in &resume.collections {
        encode_native_paged_collection_root(out, name, state.root_page);
        out.extend_from_slice(&state.entities.to_le_bytes());
        out.extend_from_slice(&state.entity_id_next.to_le_bytes());
        out.extend_from_slice(&state.row_id_next.to_le_bytes());
        out.extend_from_slice(&state.sequence_id_next.to_le_bytes());
        out.push(u8::from(state.key_index_root.is_some()));
        out.extend_from_slice(&state.key_index_root.unwrap_or(0).to_le_bytes());
        if has_secondary {
            out.extend_from_slice(&(state.secondary_indices.len() as u32).to_le_bytes());
            for (name, index) in &state.secondary_indices {
                encode_native_paged_collection_root(out, name, index.root_page);
                out.extend_from_slice(&index.codec_version.to_le_bytes());
                out.extend_from_slice(&index.definition);
            }
        }
    }
    Ok(())
}

/// Locate the optional resume trailer without reading collection pages. Missing
/// trailers require legacy preparation; malformed or mismatched trailers fail.
pub fn decode_catalog_resume(data: &[u8]) -> RdbFileResult<Option<CatalogResume>> {
    let Some(header) = decode_native_paged_metadata_header(data)? else {
        return Ok(None);
    };
    let mut position = METADATA_HEADER_BYTES;
    if header.collection_count as usize > data.len().saturating_sub(position) / 8 {
        return Err(invalid("catalog collection count exceeds metadata"));
    }
    let mut roots = BTreeMap::new();
    for _ in 0..header.collection_count {
        let root = decode_native_paged_collection_root(data, &mut position)?;
        if roots.insert(root.collection, root.root_page).is_some() {
            return Err(invalid("duplicate catalog collection"));
        }
    }
    if position == data.len() {
        return Ok(None);
    }
    let references = read_u32(data, &mut position)?;
    if references as usize > data.len().saturating_sub(position) / 21 {
        return Err(invalid("cross reference count exceeds metadata"));
    }
    for _ in 0..references {
        decode_native_paged_cross_ref(data, &mut position)?;
    }
    decode_native_paged_retired_trees(data, &mut position)?;
    decode_native_paged_deferred_pages(data, &mut position)?;
    decode_native_paged_ownership_epoch(data, &mut position)?;
    if data.get(position..position.saturating_add(4)) != Some(MAGIC.as_slice()) {
        return Ok(None);
    }
    position += 4;
    let version = read_u32(data, &mut position)?;
    if !matches!(version, 3 | VERSION | SECONDARY_VERSION) {
        return Err(invalid("unsupported catalog resume version"));
    }
    let entity_id_next = read_u64(data, &mut position)?;
    let xid_high_water_mark = if version >= 4 {
        let present = take(data, &mut position, 1)?[0];
        let value = read_u64(data, &mut position)?;
        match (present, value) {
            (0, 0) => None,
            (1, value) => Some(value),
            _ => return Err(invalid("invalid transaction resume marker")),
        }
    } else {
        None
    };
    let count = read_u32(data, &mut position)?;
    if count != header.collection_count || entity_id_next == 0 {
        return Err(invalid("invalid catalog resume header"));
    }
    let mut collections = BTreeMap::new();
    for _ in 0..count {
        let root = decode_native_paged_collection_root(data, &mut position)?;
        let state = CollectionResume {
            root_page: root.root_page,
            entities: read_u64(data, &mut position)?,
            entity_id_next: read_u64(data, &mut position)?,
            row_id_next: read_u64(data, &mut position)?,
            sequence_id_next: read_u64(data, &mut position)?,
            key_index_root: {
                let present = take(data, &mut position, 1)?[0];
                let root = read_u32(data, &mut position)?;
                match (present, root) {
                    (0, 0) => None,
                    (1, root) => Some(root),
                    _ => return Err(invalid("invalid key index resume root")),
                }
            },
            secondary_indices: if version >= SECONDARY_VERSION {
                decode_secondary_indices(data, &mut position)?
            } else {
                BTreeMap::new()
            },
        };
        if roots.get(&root.collection) != Some(&state.root_page)
            || state.entity_id_next == 0
            || state.row_id_next == 0
            || (state.root_page == 0 && state.entities != 0)
            || collections.insert(root.collection, state).is_some()
        {
            return Err(invalid(
                "catalog resume differs from its published roots or has invalid counters",
            ));
        }
    }
    validate_key_roots(&collections)?;
    Ok(Some(CatalogResume {
        entity_id_next,
        xid_high_water_mark,
        collections,
    }))
}

fn validate_key_roots(collections: &BTreeMap<String, CollectionResume>) -> RdbFileResult<()> {
    let data_roots: std::collections::BTreeSet<_> =
        collections.values().map(|state| state.root_page).collect();
    let mut key_roots = std::collections::BTreeSet::new();
    for root in collections
        .values()
        .filter_map(|state| state.key_index_root)
        .filter(|root| *root != 0)
    {
        if data_roots.contains(&root) || !key_roots.insert(root) {
            return Err(invalid("key index root aliases another published tree"));
        }
    }
    for state in collections.values() {
        if state.secondary_indices.len() > SECONDARY_COUNT_MAX {
            return Err(invalid("too many secondary indexes in catalog resume"));
        }
        for (name, index) in &state.secondary_indices {
            if name.is_empty() || name.len() > SECONDARY_NAME_BYTES_MAX || index.codec_version == 0
            {
                return Err(invalid("invalid secondary index identity"));
            }
            if index.root_page != 0
                && (data_roots.contains(&index.root_page) || !key_roots.insert(index.root_page))
            {
                return Err(invalid(
                    "secondary index root aliases another published tree",
                ));
            }
            if state.root_page == 0 && index.root_page != 0 {
                return Err(invalid("secondary index has no source data root"));
            }
        }
    }
    Ok(())
}

fn decode_secondary_indices(
    data: &[u8],
    position: &mut usize,
) -> RdbFileResult<BTreeMap<String, SecondaryIndexResume>> {
    let count = read_u32(data, position)? as usize;
    if count > SECONDARY_COUNT_MAX || count > data.len().saturating_sub(*position) / 45 {
        return Err(invalid("secondary index count exceeds catalog bounds"));
    }
    let mut indices = BTreeMap::new();
    for _ in 0..count {
        // Check before the shared decoder allocates the name.
        let mut probe = *position;
        let name_bytes = read_u32(data, &mut probe)? as usize;
        if name_bytes == 0 || name_bytes > SECONDARY_NAME_BYTES_MAX {
            return Err(invalid("invalid secondary index name length"));
        }
        let root = decode_native_paged_collection_root(data, position)?;
        let index = SecondaryIndexResume {
            root_page: root.root_page,
            codec_version: read_u32(data, position)?,
            definition: take(data, position, 32)?
                .try_into()
                .expect("invariant: fingerprint width"),
        };
        if indices.insert(root.collection, index).is_some() {
            return Err(invalid("duplicate secondary index identity"));
        }
    }
    Ok(indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Vec<u8> {
        let mut data = Vec::new();
        encode_native_paged_metadata_header(
            &mut data,
            NativePagedMetadataHeader {
                format_version: 11,
                collection_count: 1,
            },
        );
        encode_native_paged_collection_root(&mut data, "rows", 70);
        data.extend_from_slice(&0u32.to_le_bytes());
        encode_native_paged_retired_trees(&mut data, &[]).expect("retired");
        encode_native_paged_deferred_pages(&mut data, &[]).expect("deferred");
        encode_native_paged_ownership_epoch(&mut data, 7).expect("epoch");
        data
    }

    fn resume() -> CatalogResume {
        CatalogResume {
            entity_id_next: 999,
            xid_high_water_mark: Some(4000),
            collections: BTreeMap::from([(
                "rows".into(),
                CollectionResume {
                    root_page: 70,
                    entities: 12,
                    entity_id_next: 999,
                    row_id_next: 3000,
                    sequence_id_next: 5000,
                    key_index_root: Some(81),
                    secondary_indices: BTreeMap::new(),
                },
            )]),
        }
    }

    #[test]
    fn resume_roundtrip_and_legacy_absence() {
        let mut data = catalog();
        assert_eq!(decode_catalog_resume(&data).expect("legacy"), None);
        let state = resume();
        encode_catalog_resume(&mut data, &state).expect("encode");
        assert_eq!(decode_catalog_resume(&data).expect("decode"), Some(state));
    }

    fn secondary_resume() -> CatalogResume {
        let mut state = resume();
        state
            .collections
            .get_mut("rows")
            .expect("rows")
            .secondary_indices
            .insert(
                "email".into(),
                SecondaryIndexResume {
                    root_page: 91,
                    codec_version: 1,
                    definition: [17; 32],
                },
            );
        state
    }

    #[test]
    fn secondary_roots_roundtrip_and_truncation_fail_closed() {
        let state = secondary_resume();
        let mut data = catalog();
        let start = data.len();
        encode_catalog_resume(&mut data, &state).expect("secondary roots");
        assert_eq!(&data[start + 4..start + 8], &5u32.to_le_bytes());
        assert_eq!(decode_catalog_resume(&data).expect("decode"), Some(state));
        for end in start + 4..data.len() {
            assert!(
                decode_catalog_resume(&data[..end]).is_err(),
                "truncation at {end}"
            );
        }
        // No derived roots: retain v4, so legacy prepared databases do not
        // acquire a new compatibility requirement just by being reopened.
        let mut data = catalog();
        encode_catalog_resume(&mut data, &resume()).expect("legacy");
        assert_eq!(&data[start + 4..start + 8], &4u32.to_le_bytes());
    }

    #[test]
    fn secondary_roots_cannot_alias_data_keys_or_other_indices() {
        for root in [70, 81, 91] {
            let mut state = secondary_resume();
            state
                .collections
                .get_mut("rows")
                .expect("rows")
                .secondary_indices
                .insert(
                    "other".into(),
                    SecondaryIndexResume {
                        root_page: root,
                        codec_version: 1,
                        definition: [18; 32],
                    },
                );
            assert!(
                encode_catalog_resume(&mut catalog(), &state).is_err(),
                "alias {root}"
            );
        }
        let mut data = catalog();
        encode_catalog_resume(&mut data, &secondary_resume()).expect("secondary");
        let root_position = data.len() - 40;
        data[root_position..root_position + 4].copy_from_slice(&70u32.to_le_bytes());
        assert!(
            decode_catalog_resume(&data).is_err(),
            "corrupt alias on disk"
        );
    }

    #[test]
    fn secondary_catalog_bounds_and_empty_tree_are_explicit() {
        for name in [String::new(), "x".repeat(SECONDARY_NAME_BYTES_MAX + 1)] {
            let mut state = resume();
            state
                .collections
                .get_mut("rows")
                .expect("rows")
                .secondary_indices
                .insert(
                    name,
                    SecondaryIndexResume {
                        root_page: 0,
                        codec_version: 1,
                        definition: [0; 32],
                    },
                );
            assert!(encode_catalog_resume(&mut catalog(), &state).is_err());
        }
        let mut state = secondary_resume();
        state
            .collections
            .get_mut("rows")
            .expect("rows")
            .secondary_indices
            .get_mut("email")
            .expect("email")
            .root_page = 0;
        let mut data = catalog();
        encode_catalog_resume(&mut data, &state).expect("empty secondary tree");
        assert_eq!(decode_catalog_resume(&data).expect("decode"), Some(state));
        let codec_position = data.len() - 36;
        data[codec_position..codec_position + 4].fill(0);
        assert!(
            decode_catalog_resume(&data).is_err(),
            "zero codec is invalid"
        );
    }

    #[test]
    fn xid_resume_reads_v3_as_unknown_and_rejects_invalid_presence() {
        let mut data = catalog();
        let start = data.len();
        encode_catalog_resume(&mut data, &resume()).expect("encode");
        let presence = start + 16;
        let mut legacy = data.clone();
        legacy[start + 4..start + 8].copy_from_slice(&3u32.to_le_bytes());
        legacy.drain(presence..presence + 9);
        let mut expected = resume();
        expected.xid_high_water_mark = None;
        assert_eq!(
            decode_catalog_resume(&legacy).expect("v3"),
            Some(expected.clone())
        );
        data[presence] = 2;
        assert!(decode_catalog_resume(&data).is_err());
        data[presence] = 0;
        assert!(
            decode_catalog_resume(&data).is_err(),
            "unknown markers must have zero payload"
        );
        data[presence + 1..presence + 9].fill(0);
        assert_eq!(
            decode_catalog_resume(&data).expect("unknown"),
            Some(expected)
        );
    }

    #[test]
    fn resume_rejects_truncation_and_root_mismatch() {
        let mut data = catalog();
        let start = data.len();
        encode_catalog_resume(&mut data, &resume()).expect("encode");
        for end in start + 4..data.len() {
            assert!(
                decode_catalog_resume(&data[..end]).is_err(),
                "truncated at {end}"
            );
        }
        let mut mismatched = resume();
        mismatched
            .collections
            .get_mut("rows")
            .expect("rows")
            .root_page += 1;
        let mut data = catalog();
        encode_catalog_resume(&mut data, &mismatched).expect("encode mismatch");
        assert!(decode_catalog_resume(&data).is_err());
    }
    #[test]
    fn resume_preserves_missing_key_preparation_and_rejects_aliased_indexes() {
        let mut state = resume();
        state
            .collections
            .get_mut("rows")
            .expect("rows")
            .key_index_root = None;
        let mut bytes = catalog();
        encode_catalog_resume(&mut bytes, &state).expect("unprepared index marker");
        assert_eq!(
            decode_catalog_resume(&bytes).expect("missing key index"),
            Some(state)
        );
        let mut state = resume();
        state
            .collections
            .get_mut("rows")
            .expect("rows")
            .key_index_root = Some(70);
        assert!(encode_catalog_resume(&mut catalog(), &state).is_err());
        let mut bytes = catalog();
        encode_catalog_resume(&mut bytes, &resume()).expect("paired roots");
        let end = bytes.len();
        bytes[end - 4..].copy_from_slice(&70_u32.to_le_bytes());
        assert!(
            decode_catalog_resume(&bytes).is_err(),
            "a key tree cannot alias a physical data tree"
        );
    }
}
