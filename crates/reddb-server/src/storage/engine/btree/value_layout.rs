//! Native compatibility paths for the shared large-value layout.
use crate::storage::engine::pager::Pager;
pub use reddb_engine::storage::btree::value_layout::{
    pointer_head, projected_cell_len, ValueLayoutError, COMPRESSION_THRESHOLD, MAX_VALUE_SIZE,
    OVERFLOW_THRESHOLD, POINTER_CELL_LEN,
};
pub fn encode(pager: &Pager, value: &[u8]) -> Result<Vec<u8>, ValueLayoutError> {
    reddb_engine::storage::btree::value_layout::encode(pager, value)
}
pub fn decode(pager: &Pager, stored: &[u8]) -> Result<Vec<u8>, ValueLayoutError> {
    reddb_engine::storage::btree::value_layout::decode(pager, stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::engine::pager::Pager;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_db_path() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "reddb_value_layout_test_{}_{}.db",
            std::process::id(),
            id
        ));
        path
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        for sidecar in reddb_file::layout::pager_shadow_sidecar_paths(path) {
            let _ = std::fs::remove_file(sidecar);
        }
    }

    fn fresh_pager() -> (Pager, PathBuf) {
        let path = temp_db_path();
        cleanup(&path);
        let pager = Pager::open_default(&path).unwrap();
        (pager, path)
    }

    #[test]
    fn inline_raw_round_trip_below_compression_threshold() {
        let (pager, path) = fresh_pager();
        let value = vec![0xABu8; COMPRESSION_THRESHOLD - 1];
        let stored = encode(&pager, &value).unwrap();
        assert_eq!(
            reddb_file::decode_btree_value_cell(&stored).unwrap(),
            reddb_file::BTreeValueCell::Inline {
                is_compressed: false,
                payload: value.as_slice(),
            }
        );
        assert_eq!(stored.len(), 1 + value.len());
        let decoded = decode(&pager, &stored).unwrap();
        assert_eq!(decoded, value);
        cleanup(&path);
    }

    #[test]
    fn inline_raw_at_exact_compression_threshold() {
        let (pager, path) = fresh_pager();
        let value = vec![0x7Eu8; COMPRESSION_THRESHOLD];
        let stored = encode(&pager, &value).unwrap();
        assert!(matches!(
            reddb_file::decode_btree_value_cell(&stored).unwrap(),
            reddb_file::BTreeValueCell::Inline {
                is_compressed: false,
                ..
            }
        ));
        assert_eq!(decode(&pager, &stored).unwrap(), value);
        cleanup(&path);
    }

    #[test]
    fn compressible_above_threshold_inlines_compressed() {
        let (pager, path) = fresh_pager();
        // ~44 KB of repeating text — compresses into a few hundred bytes
        // which fit inline.
        let value = "the quick brown fox jumps over the lazy dog\n"
            .repeat(1024)
            .into_bytes();
        assert!(value.len() > OVERFLOW_THRESHOLD);
        let stored = encode(&pager, &value).unwrap();
        assert!(matches!(
            reddb_file::decode_btree_value_cell(&stored).unwrap(),
            reddb_file::BTreeValueCell::Inline {
                is_compressed: true,
                ..
            }
        ));
        assert!(
            stored.len() <= 1 + OVERFLOW_THRESHOLD,
            "compressed cell must fit inline budget"
        );
        let decoded = decode(&pager, &stored).unwrap();
        assert_eq!(decoded, value);
        cleanup(&path);
    }

    #[test]
    fn incompressible_above_threshold_spills_raw() {
        let (pager, path) = fresh_pager();
        // Pseudo-random bytes — incompressible.
        let mut state: u64 = 0x1234_5678_9ABC_DEF0;
        let value: Vec<u8> = (0..5 * 1024 * 1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let stored = encode(&pager, &value).unwrap();
        assert!(matches!(
            reddb_file::decode_btree_value_cell(&stored).unwrap(),
            reddb_file::BTreeValueCell::Pointer {
                is_compressed: false,
                ..
            }
        ));
        assert_eq!(stored.len(), POINTER_CELL_LEN);
        let decoded = decode(&pager, &stored).unwrap();
        assert_eq!(decoded.len(), value.len());
        assert_eq!(decoded, value);
        cleanup(&path);
    }

    #[test]
    fn value_above_max_rejected_without_allocation() {
        let (pager, path) = fresh_pager();
        let before = pager.page_count().unwrap();
        let value = vec![0u8; MAX_VALUE_SIZE + 1];
        let err = encode(&pager, &value).unwrap_err();
        assert!(matches!(err, ValueLayoutError::ValueTooLarge(_)));
        let after = pager.page_count().unwrap();
        assert_eq!(before, after, "rejected value must not allocate pages");
        cleanup(&path);
    }

    #[test]
    fn pointer_head_extracts_head_id_only_for_pointer_cells() {
        let inline = reddb_file::encode_btree_inline_raw(&[1, 2, 3]);
        assert_eq!(pointer_head(&inline), None);
        let inline_compressed = reddb_file::encode_btree_inline_compressed(&[0, 0, 0, 5]);
        assert_eq!(pointer_head(&inline_compressed), None);

        let cell = reddb_file::encode_btree_pointer(0x0102_0304, 0, false);
        assert_eq!(pointer_head(&cell), Some(0x0102_0304));
        let compressed_cell = reddb_file::encode_btree_pointer(0x0102_0304, 0, true);
        assert_eq!(pointer_head(&compressed_cell), Some(0x0102_0304));
    }

    #[test]
    fn empty_value_round_trips() {
        let (pager, path) = fresh_pager();
        let stored = encode(&pager, &[]).unwrap();
        assert_eq!(stored, vec![0u8]);
        assert!(decode(&pager, &stored).unwrap().is_empty());
        cleanup(&path);
    }
}
