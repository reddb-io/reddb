//! Generation-bound resume counters for the server's immutable disk base.
//! The enclosing metadata checksum covers these bytes and their collection roots.
//! Writers must fence PAGE_DISK_RESIDENCY_FILE_VERSION in both superblocks first.

use crate::native_store::*;
use crate::{RdbFileError, RdbFileResult, METADATA_HEADER_BYTES};
use std::collections::BTreeMap;

const MAGIC: &[u8; 4] = b"RDRS";
const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionResume {
    pub root_page: u32,
    pub entities: u64,
    pub entity_id_next: u64,
    pub row_id_next: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogResume {
    pub entity_id_next: u64,
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
    let count = u32::try_from(resume.collections.len())
        .map_err(|_| invalid("too many resume collections"))?;
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&resume.entity_id_next.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    for (name, state) in &resume.collections {
        encode_native_paged_collection_root(out, name, state.root_page);
        out.extend_from_slice(&state.entities.to_le_bytes());
        out.extend_from_slice(&state.entity_id_next.to_le_bytes());
        out.extend_from_slice(&state.row_id_next.to_le_bytes());
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
    if read_u32(data, &mut position)? != VERSION {
        return Err(invalid("unsupported catalog resume version"));
    }
    let entity_id_next = read_u64(data, &mut position)?;
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
    Ok(Some(CatalogResume {
        entity_id_next,
        collections,
    }))
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
            collections: BTreeMap::from([(
                "rows".into(),
                CollectionResume {
                    root_page: 70,
                    entities: 12,
                    entity_id_next: 999,
                    row_id_next: 3000,
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
}
