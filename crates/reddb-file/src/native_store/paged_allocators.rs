//! Optional allocator floors, published with the paged collection catalog.
//! Older readers ignore this tail. Older writers drop it: writer downgrade
//! requires a separately verified pre-upgrade copy, just like retirement tails.
use super::{
    decode_native_len_prefixed_string, encode_native_len_prefixed_str, read_native_u32,
    read_native_u64, RdbFileError, RdbFileResult,
};
use std::collections::HashSet;

const MAGIC: &[u8; 4] = b"RDA1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePagedRowAllocator {
    pub collection: String,
    /// Next value, not the last allocated value. It must never be zero.
    pub next_row_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePagedAllocatorState {
    pub next_entity_id: u64,
    pub collections: Vec<NativePagedRowAllocator>,
}

pub fn encode_native_paged_allocators(
    out: &mut Vec<u8>,
    state: &NativePagedAllocatorState,
) -> RdbFileResult<()> {
    let count = u32::try_from(state.collections.len())
        .map_err(|_| invalid("too many allocator collections"))?;
    if state.next_entity_id == 0 {
        return Err(invalid("zero entity allocator next value"));
    }
    let mut names = HashSet::new();
    for entry in &state.collections {
        u32::try_from(entry.collection.len())
            .map_err(|_| invalid("allocator collection name too long"))?;
        if !names.insert(entry.collection.as_str()) || entry.next_row_id == 0 {
            return Err(invalid("invalid or duplicate row allocator"));
        }
    }
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&state.next_entity_id.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    for entry in &state.collections {
        encode_native_len_prefixed_str(out, &entry.collection);
        out.extend_from_slice(&entry.next_row_id.to_le_bytes());
    }
    Ok(())
}

pub fn decode_native_paged_allocators(
    bytes: &[u8],
    pos: &mut usize,
) -> RdbFileResult<Option<NativePagedAllocatorState>> {
    if bytes.get(*pos..pos.saturating_add(4)) != Some(MAGIC.as_slice()) {
        return Ok(None);
    }
    let mut cursor = *pos + MAGIC.len();
    let next_entity_id = read_native_u64(bytes, &mut cursor, "truncated entity allocator")?;
    if next_entity_id == 0 {
        return Err(invalid("zero entity allocator next value"));
    }
    let count = read_native_u32(bytes, &mut cursor, "truncated allocator count")? as usize;
    // Each entry has a u32 name length and a u64 next value, before its name.
    if count > bytes.len().saturating_sub(cursor) / 12 {
        return Err(invalid("truncated allocator entries"));
    }
    let mut collections = Vec::with_capacity(count);
    let mut names = HashSet::with_capacity(count);
    for _ in 0..count {
        let collection = decode_native_len_prefixed_string(bytes, &mut cursor)?;
        let next_row_id = read_native_u64(bytes, &mut cursor, "truncated row allocator")?;
        if !names.insert(collection.clone()) || next_row_id == 0 {
            return Err(invalid("invalid or duplicate row allocator"));
        }
        collections.push(NativePagedRowAllocator {
            collection,
            next_row_id,
        });
    }
    *pos = cursor;
    Ok(Some(NativePagedAllocatorState {
        next_entity_id,
        collections,
    }))
}

fn invalid(message: &str) -> RdbFileError {
    RdbFileError::InvalidOperation(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> NativePagedAllocatorState {
        NativePagedAllocatorState {
            next_entity_id: 91,
            collections: vec![NativePagedRowAllocator {
                collection: "rows".into(),
                next_row_id: 31,
            }],
        }
    }

    #[test]
    fn allocator_tail_roundtrips_and_accepts_legacy_padding() {
        let mut pos = 0;
        assert_eq!(
            decode_native_paged_allocators(&[0; 32], &mut pos).expect("legacy"),
            None
        );
        assert_eq!(pos, 0);
        let mut bytes = vec![7; 9];
        encode_native_paged_allocators(&mut bytes, &state()).expect("encode");
        let end = bytes.len();
        bytes.resize(end + 16, 0);
        pos = 9;
        assert_eq!(
            decode_native_paged_allocators(&bytes, &mut pos).expect("decode"),
            Some(state())
        );
        assert_eq!(pos, end);
    }

    #[test]
    fn allocator_tail_rejects_truncation_without_advancing_the_cursor() {
        let mut bytes = Vec::new();
        encode_native_paged_allocators(&mut bytes, &state()).expect("encode");
        for length in 4..bytes.len() {
            let mut pos = 0;
            assert!(
                decode_native_paged_allocators(&bytes[..length], &mut pos).is_err(),
                "truncation length={length}"
            );
            assert_eq!(pos, 0);
        }
    }

    #[test]
    fn allocator_tail_rejects_zero_values_duplicates_and_impossible_counts() {
        let mut good = Vec::new();
        encode_native_paged_allocators(&mut good, &state()).expect("encode");
        for range in [4..12, good.len() - 8..good.len()] {
            let mut bytes = good.clone();
            bytes[range].fill(0);
            assert!(decode_native_paged_allocators(&bytes, &mut 0).is_err());
        }
        let mut bytes = good.clone();
        bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_native_paged_allocators(&bytes, &mut 0).is_err());
        let mut duplicate = state();
        duplicate.collections.push(duplicate.collections[0].clone());
        let mut out = vec![7];
        assert!(encode_native_paged_allocators(&mut out, &duplicate).is_err());
        assert_eq!(out, [7], "invalid input must not append a partial tail");
        let mut bytes = good;
        bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_within(16..);
        assert!(decode_native_paged_allocators(&bytes, &mut 0).is_err());
    }
}
