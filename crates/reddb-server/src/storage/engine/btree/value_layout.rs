//! Large-value layout for B-tree leaf cells (slice E of PRD #662).
//!
//! Wires the standalone slice modules together at the write/read boundary:
//!
//! - [`value_codec`] (slice A, #698) decides whether the bytes are worth
//!   compressing.
//! - [`OverflowChain`] (slice B, #699) owns the chain of dedicated overflow
//!   pages.
//! - `reddb-file` pins the persisted two-bit `(pointer, compressed)` cell
//!   shape and pointer payload encoding.
//!
//! The function entry points are [`encode`] and [`decode`]. They persist
//! opaque cell bytes whose on-disk shape is owned by `reddb-file`.
//!
//! Decision ladder applied by [`encode`]:
//!
//! 1. `value.len() > MAX_VALUE_SIZE` → [`ValueLayoutError::ValueTooLarge`]
//!    (no allocation, no LZ4 work).
//! 2. `value.len() <= OVERFLOW_THRESHOLD` → inline raw.
//! 3. Else try LZ4. If the compressed bytes fit inline → inline compressed.
//! 4. Else spill via [`OverflowChain`], storing only the pointer in the
//!    leaf cell.
//!
//! Per ADR 0025 (overflow chain MVCC) the chain identity is anchored at
//! the leaf cell, so this slice does not add any WAL records of its own
//! — the overflow page writes go through the existing pager path.

use crate::storage::engine::overflow::{OverflowChain, OverflowError};
use crate::storage::engine::pager::Pager;
use crate::storage::engine::vector_btree::value_codec;
use reddb_file::BTreeValueCell;

/// At or below this length a value stores inline in the leaf, raw or
/// compressed. Above it, the value spills via [`OverflowChain`]. Set per
/// ADR 0023 — preserves leaf fanout regardless of page size.
pub const OVERFLOW_THRESHOLD: usize = reddb_file::BTREE_VALUE_OVERFLOW_THRESHOLD;

/// Hard upper bound on logical value size. Values above this are
/// rejected before any LZ4 or overflow work runs.
/// 2^28 = 256 MiB per ADR 0023.
pub const MAX_VALUE_SIZE: usize = reddb_file::BTREE_VALUE_MAX_SIZE;

/// Total stored bytes for a pointer cell — flag byte + payload.
pub const POINTER_CELL_LEN: usize = reddb_file::BTREE_VALUE_POINTER_CELL_LEN;

/// Errors returned by [`encode`] and [`decode`].
#[derive(Debug)]
pub enum ValueLayoutError {
    /// Logical value length exceeds [`MAX_VALUE_SIZE`].
    ValueTooLarge(usize),
    /// Stored cell flag byte sets a reserved bit.
    UnknownFlag(u8),
    /// Stored bytes ended before the expected pointer payload.
    TruncatedPointer { got: usize },
    /// LZ4 decode failed.
    Codec(value_codec::ValueCodecError),
    /// Overflow chain operation failed.
    Overflow(OverflowError),
}

impl std::fmt::Display for ValueLayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ValueTooLarge(n) => {
                write!(f, "value too large: {} bytes (max {})", n, MAX_VALUE_SIZE)
            }
            Self::UnknownFlag(b) => write!(f, "unknown leaf-cell flag byte: 0b{:08b}", b),
            Self::TruncatedPointer { got } => {
                write!(
                    f,
                    "pointer cell truncated: need {} bytes after flag, got {}",
                    POINTER_CELL_LEN - 1,
                    got
                )
            }
            Self::Codec(e) => write!(f, "value codec: {}", e),
            Self::Overflow(e) => write!(f, "overflow chain: {}", e),
        }
    }
}

impl std::error::Error for ValueLayoutError {}

impl From<value_codec::ValueCodecError> for ValueLayoutError {
    fn from(e: value_codec::ValueCodecError) -> Self {
        Self::Codec(e)
    }
}

impl From<reddb_file::BTreeValueCellError> for ValueLayoutError {
    fn from(e: reddb_file::BTreeValueCellError) -> Self {
        match e {
            reddb_file::BTreeValueCellError::UnknownFlag(flag) => Self::UnknownFlag(flag),
            reddb_file::BTreeValueCellError::TruncatedPointer { got } => {
                Self::TruncatedPointer { got }
            }
        }
    }
}

impl From<OverflowError> for ValueLayoutError {
    fn from(e: OverflowError) -> Self {
        Self::Overflow(e)
    }
}

/// Apply the decision ladder and return the bytes the B-tree should
/// persist in the leaf cell value slot. May allocate overflow pages
/// through `pager`.
pub fn encode(pager: &Pager, value: &[u8]) -> Result<Vec<u8>, ValueLayoutError> {
    if value.len() > MAX_VALUE_SIZE {
        return Err(ValueLayoutError::ValueTooLarge(value.len()));
    }

    // Step 1: small values short-circuit to inline raw. No LZ4 work, no
    // overflow allocation — preserves the legacy hot path for tiny
    // values like entity IDs or short JSON.
    if value.len() <= OVERFLOW_THRESHOLD {
        return Ok(reddb_file::encode_btree_inline_raw(value));
    }

    // Step 2: above the threshold, try LZ4. The codec returns Raw when
    // compression would not shrink the input.
    let (codec_flag, codec_bytes) = value_codec::encode(value);

    // Step 3: if compressed bytes (including their length header) fit
    // inline, keep them in the leaf. Note this only fires when codec
    // actually produced Lz4 bytes — Raw bytes ≥ original length > threshold
    // by construction, so they cannot inline.
    if codec_flag == value_codec::ValueFlag::Lz4 && codec_bytes.len() <= OVERFLOW_THRESHOLD {
        return Ok(reddb_file::encode_btree_inline_compressed(&codec_bytes));
    }

    // Step 4: spill. We always store the bytes the codec produced —
    // either Lz4 (with its 4-byte length header) so the reader can
    // round-trip via the same codec, or Raw bytes when compression
    // failed to shrink.
    let is_compressed = codec_flag == value_codec::ValueFlag::Lz4;
    let chain = OverflowChain::new(pager);
    let (head, total_len) = chain.store(&codec_bytes)?;

    Ok(reddb_file::encode_btree_pointer(
        head,
        total_len,
        is_compressed,
    ))
}

/// Inspect leaf-cell flags, follow the pointer if any, concatenate the
/// chain, decode if compressed, return the materialised value.
pub fn decode(pager: &Pager, stored: &[u8]) -> Result<Vec<u8>, ValueLayoutError> {
    match reddb_file::decode_btree_value_cell(stored)? {
        BTreeValueCell::Pointer {
            is_compressed,
            head_page_id,
            total_len,
        } => {
            let chain = OverflowChain::new(pager);
            let chain_bytes = chain.read(head_page_id, total_len)?;
            if is_compressed {
                Ok(value_codec::decode(
                    value_codec::ValueFlag::Lz4,
                    &chain_bytes,
                )?)
            } else {
                Ok(chain_bytes)
            }
        }
        BTreeValueCell::Inline {
            is_compressed,
            payload,
        } => Ok(reddb_file::decode_btree_inline_payload(
            is_compressed,
            payload,
        )?),
    }
}

/// Return the head overflow page id when `stored` represents a pointer
/// cell, else `None`. Returns `None` for inline (raw or compressed)
/// cells and for empty/malformed cells. Callers use this from the
/// B-tree delete and shrinking-update paths (slice F of PRD #662) to
/// free the chain before the leaf cell goes away.
pub fn pointer_head(stored: &[u8]) -> Option<u32> {
    reddb_file::btree_value_pointer_head(stored)
}

/// `true` when [`encode`] would emit a spill pointer for a value of
/// this length (without actually spilling). Used by the leaf-fit check
/// in `bulk_insert_sorted` to size cells before allocation.
#[inline]
#[allow(dead_code)]
pub fn projected_cell_len(input: &[u8]) -> usize {
    let codec_len = value_codec::would_encode_to(input);
    reddb_file::btree_projected_cell_len(input.len(), codec_len)
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
        for suffix in ["-hdr", "-meta", "-dwb"] {
            let mut p = path.to_path_buf().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(&p);
        }
    }

    fn fresh_pager() -> (Pager, PathBuf) {
        let path = temp_db_path();
        cleanup(&path);
        let pager = Pager::open_default(&path).unwrap();
        (pager, path)
    }

    #[test]
    fn inline_raw_round_trip_below_threshold() {
        let (pager, path) = fresh_pager();
        let value = vec![0xABu8; OVERFLOW_THRESHOLD - 1];
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
    fn inline_raw_at_exact_threshold() {
        let (pager, path) = fresh_pager();
        let value = vec![0x7Eu8; OVERFLOW_THRESHOLD];
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
