//! Large-value layout for B-tree leaf cells (slice E of PRD #662).
//!
//! Wires the standalone slice modules together at the write/read boundary:
//!
//! - [`value_codec`] (slice A, #698) decides whether the bytes are worth
//!   compressing.
//! - [`OverflowChain`] (slice B, #699) owns the chain of dedicated overflow
//!   pages.
//! - [`page_format::LeafCellFlags`] (slice C, #700) pins the two-bit
//!   `(pointer, compressed)` shape — this module uses the same bit
//!   layout for the per-cell flag byte so the page-format decoder
//!   stays the single source of truth for the bit positions.
//!
//! The function entry points are [`encode`] and [`decode`]. They share
//! one on-disk byte layout the rest of the engine can treat as opaque
//! stored bytes:
//!
//! ```text
//! Inline raw:        [0x00][bytes...]
//! Inline compressed: [0x02][lz4_len: u32 LE][lz4 bytes...]
//! Pointer raw:       [0x01][head_page_id: u32 LE][total_len: u64 LE]
//! Pointer compressed:[0x03][head_page_id: u32 LE][total_len: u64 LE]
//! ```
//!
//! All four shapes are valid leaf-cell payloads; the leaf layer never
//! decodes them — only the read path does (via [`decode`]).
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

/// At or below this length a value stores inline in the leaf, raw or
/// compressed. Above it, the value spills via [`OverflowChain`]. Set per
/// ADR 0023 — preserves leaf fanout regardless of page size.
pub const OVERFLOW_THRESHOLD: usize = 1024;

/// Hard upper bound on logical value size. Values above this are
/// rejected before any LZ4 or overflow work runs.
/// 2^28 = 256 MiB per ADR 0023.
pub const MAX_VALUE_SIZE: usize = 256 * 1024 * 1024;

/// Bit positions match [`crate::storage::engine::vector_btree::page_format::LeafCellFlags`]
/// so the on-disk decoder stays the single source of truth.
const FLAG_POINTER: u8 = 0b0000_0001;
const FLAG_COMPRESSED: u8 = 0b0000_0010;
const FLAG_RESERVED_MASK: u8 = !(FLAG_POINTER | FLAG_COMPRESSED);

/// Length of the spill pointer payload: `head_page_id: u32 LE` then
/// `total_len: u64 LE`.
const POINTER_PAYLOAD_LEN: usize = 4 + 8;

/// Total stored bytes for a pointer cell — flag byte + payload.
pub const POINTER_CELL_LEN: usize = 1 + POINTER_PAYLOAD_LEN;

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
                    POINTER_PAYLOAD_LEN, got
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
        let mut out = Vec::with_capacity(1 + value.len());
        out.push(0);
        out.extend_from_slice(value);
        return Ok(out);
    }

    // Step 2: above the threshold, try LZ4. The codec returns Raw when
    // compression would not shrink the input.
    let (codec_flag, codec_bytes) = value_codec::encode(value);

    // Step 3: if compressed bytes (including their length header) fit
    // inline, keep them in the leaf. Note this only fires when codec
    // actually produced Lz4 bytes — Raw bytes ≥ original length > threshold
    // by construction, so they cannot inline.
    if codec_flag == value_codec::ValueFlag::Lz4 && codec_bytes.len() <= OVERFLOW_THRESHOLD {
        let mut out = Vec::with_capacity(1 + codec_bytes.len());
        out.push(FLAG_COMPRESSED);
        out.extend_from_slice(&codec_bytes);
        return Ok(out);
    }

    // Step 4: spill. We always store the bytes the codec produced —
    // either Lz4 (with its 4-byte length header) so the reader can
    // round-trip via the same codec, or Raw bytes when compression
    // failed to shrink.
    let is_compressed = codec_flag == value_codec::ValueFlag::Lz4;
    let chain = OverflowChain::new(pager);
    let (head, total_len) = chain.store(&codec_bytes)?;

    let mut flag = FLAG_POINTER;
    if is_compressed {
        flag |= FLAG_COMPRESSED;
    }
    let mut out = Vec::with_capacity(POINTER_CELL_LEN);
    out.push(flag);
    out.extend_from_slice(&head.to_le_bytes());
    out.extend_from_slice(&total_len.to_le_bytes());
    Ok(out)
}

/// Inspect leaf-cell flags, follow the pointer if any, concatenate the
/// chain, decode if compressed, return the materialised value.
pub fn decode(pager: &Pager, stored: &[u8]) -> Result<Vec<u8>, ValueLayoutError> {
    if stored.is_empty() {
        // An empty cell payload encodes an empty value the same way as
        // an inline-raw cell of length zero — there is no flag byte to
        // read, but the materialised value is unambiguous.
        return Ok(Vec::new());
    }

    let flag = stored[0];
    if flag & FLAG_RESERVED_MASK != 0 {
        return Err(ValueLayoutError::UnknownFlag(flag));
    }
    let is_pointer = flag & FLAG_POINTER != 0;
    let is_compressed = flag & FLAG_COMPRESSED != 0;
    let payload = &stored[1..];

    if is_pointer {
        if payload.len() != POINTER_PAYLOAD_LEN {
            return Err(ValueLayoutError::TruncatedPointer { got: payload.len() });
        }
        let head = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        let total_len = u64::from_le_bytes([
            payload[4],
            payload[5],
            payload[6],
            payload[7],
            payload[8],
            payload[9],
            payload[10],
            payload[11],
        ]);
        let chain = OverflowChain::new(pager);
        let chain_bytes = chain.read(head, total_len)?;
        if is_compressed {
            Ok(value_codec::decode(
                value_codec::ValueFlag::Lz4,
                &chain_bytes,
            )?)
        } else {
            Ok(chain_bytes)
        }
    } else if is_compressed {
        Ok(value_codec::decode(value_codec::ValueFlag::Lz4, payload)?)
    } else {
        Ok(payload.to_vec())
    }
}

/// Return the head overflow page id when `stored` represents a pointer
/// cell, else `None`. Returns `None` for inline (raw or compressed)
/// cells and for empty/malformed cells. Callers use this from the
/// B-tree delete and shrinking-update paths (slice F of PRD #662) to
/// free the chain before the leaf cell goes away.
pub fn pointer_head(stored: &[u8]) -> Option<u32> {
    if stored.is_empty() {
        return None;
    }
    let flag = stored[0];
    if flag & FLAG_RESERVED_MASK != 0 {
        return None;
    }
    if flag & FLAG_POINTER == 0 {
        return None;
    }
    if stored.len() < 1 + POINTER_PAYLOAD_LEN {
        return None;
    }
    let payload = &stored[1..];
    Some(u32::from_le_bytes([
        payload[0], payload[1], payload[2], payload[3],
    ]))
}

/// `true` when [`encode`] would emit a spill pointer for a value of
/// this length (without actually spilling). Used by the leaf-fit check
/// in `bulk_insert_sorted` to size cells before allocation.
#[inline]
#[allow(dead_code)]
pub fn projected_cell_len(input: &[u8]) -> usize {
    if input.len() <= OVERFLOW_THRESHOLD {
        return 1 + input.len();
    }
    let codec_len = value_codec::would_encode_to(input);
    // Track the codec's own decision: when LZ4 would not shrink the
    // input, codec_len == input.len() and the encoded flag is Raw. In
    // that case the encoded payload cannot fit inline (input > threshold
    // is the entry condition) so we spill.
    if codec_len < input.len() && codec_len <= OVERFLOW_THRESHOLD {
        1 + codec_len
    } else {
        POINTER_CELL_LEN
    }
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
        assert_eq!(stored[0], 0, "inline raw flag must be zero");
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
        assert_eq!(stored[0], 0);
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
        assert_eq!(
            stored[0], FLAG_COMPRESSED,
            "highly repetitive payload must inline compressed"
        );
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
        assert_eq!(
            stored[0] & FLAG_POINTER,
            FLAG_POINTER,
            "incompressible spill must set pointer flag"
        );
        assert_eq!(
            stored[0] & FLAG_COMPRESSED,
            0,
            "incompressible payload must not claim to be compressed"
        );
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
    fn decode_rejects_reserved_flag_bits() {
        let (pager, path) = fresh_pager();
        let stored = vec![0b0000_0100u8];
        let err = decode(&pager, &stored).unwrap_err();
        assert!(matches!(err, ValueLayoutError::UnknownFlag(_)));
        cleanup(&path);
    }

    #[test]
    fn pointer_head_extracts_head_id_only_for_pointer_cells() {
        // Inline raw (flag 0x00) → None.
        assert_eq!(pointer_head(&[0x00, 1, 2, 3]), None);
        // Inline compressed (flag 0x02) → None.
        assert_eq!(pointer_head(&[0x02, 0, 0, 0, 5, 0xff, 0xfe]), None);
        // Empty stored bytes → None.
        assert_eq!(pointer_head(&[]), None);
        // Reserved bits set → None (callers must not free anything they
        // cannot interpret).
        assert_eq!(pointer_head(&[0b0000_0100]), None);
        // Pointer raw with head id 0x01020304 and total_len 0 → Some(...)
        let mut cell = vec![FLAG_POINTER];
        cell.extend_from_slice(&0x0102_0304u32.to_le_bytes());
        cell.extend_from_slice(&0u64.to_le_bytes());
        assert_eq!(pointer_head(&cell), Some(0x0102_0304));
        // Pointer compressed flag also yields the same head.
        cell[0] = FLAG_POINTER | FLAG_COMPRESSED;
        assert_eq!(pointer_head(&cell), Some(0x0102_0304));
        // Pointer flag but payload truncated → None (no UB).
        assert_eq!(pointer_head(&[FLAG_POINTER, 1, 2]), None);
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
