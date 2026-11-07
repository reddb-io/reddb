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
//! 2. `value.len() <= COMPRESSION_THRESHOLD` → inline raw.
//! 3. Else try LZ4. If the selected raw/compressed bytes fit the native
//!    leaf's `OVERFLOW_THRESHOLD` → inline with the matching flag.
//! 4. Else spill via [`OverflowChain`], storing only the pointer in the
//!    leaf cell.
//!
//! Per ADR 0025 (overflow chain MVCC) the chain identity is anchored at
//! the leaf cell, so this slice does not add any WAL records of its own
//! — the overflow page writes go through the existing pager path.

use crate::storage::overflow::{OverflowChain, OverflowError};
use crate::storage::page_store::PageStore;
use reddb_file::vector_value_codec as value_codec;
use reddb_file::BTreeValueCell;

/// Encoded values up to this size share a native leaf, raw or compressed.
/// Larger values spill via [`OverflowChain`] (ADR 0023).
pub const OVERFLOW_THRESHOLD: usize = reddb_file::BTREE_VALUE_OVERFLOW_THRESHOLD;

/// Preserve compression above the established 1-KiB raw fast path.
pub const COMPRESSION_THRESHOLD: usize = reddb_file::BTREE_VALUE_COMPRESSION_THRESHOLD;

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
    /// Decode scratch plus output would exceed the read's memory allowance.
    ReadLimitExceeded {
        required_bytes: usize,
        limit_bytes: usize,
    },
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
            Self::ReadLimitExceeded {
                required_bytes,
                limit_bytes,
            } => write!(
                f,
                "read needs {required_bytes} bytes over read memory limit {limit_bytes}"
            ),
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

/// Encode only when the result fits in a leaf, without allocating pages.
pub(crate) fn encode_inline_without_pages(
    value: &[u8],
) -> Result<Option<Vec<u8>>, ValueLayoutError> {
    if value.len() > MAX_VALUE_SIZE {
        return Err(ValueLayoutError::ValueTooLarge(value.len()));
    }
    if value.len() <= COMPRESSION_THRESHOLD {
        return Ok(Some(reddb_file::encode_btree_inline_raw(value)));
    }
    let (flag, bytes) = value_codec::encode(value);
    if bytes.len() <= OVERFLOW_THRESHOLD {
        return Ok(Some(match flag {
            value_codec::ValueFlag::Raw => reddb_file::encode_btree_inline_raw(&bytes),
            value_codec::ValueFlag::Lz4 => reddb_file::encode_btree_inline_compressed(&bytes),
        }));
    }
    Ok(None)
}

/// Apply the decision ladder and return the bytes the B-tree should
/// persist in the leaf cell value slot. May allocate overflow pages
/// through `pager`.
pub fn encode<P: PageStore>(pager: &P, value: &[u8]) -> Result<Vec<u8>, ValueLayoutError> {
    if value.len() > MAX_VALUE_SIZE {
        return Err(ValueLayoutError::ValueTooLarge(value.len()));
    }

    // Step 1: small values short-circuit to inline raw. No LZ4 work, no
    // overflow allocation — preserves the legacy hot path for tiny
    // values like entity IDs or short JSON.
    if value.len() <= COMPRESSION_THRESHOLD {
        return Ok(reddb_file::encode_btree_inline_raw(value));
    }

    // Step 2: above the threshold, try LZ4. The codec returns Raw when
    // compression would not shrink the input.
    let (codec_flag, codec_bytes) = value_codec::encode(value);

    // Step 3: share leaf space for either selected representation. Keep the
    // previous compression trigger even when a medium raw value now fits.
    if codec_bytes.len() <= OVERFLOW_THRESHOLD {
        return Ok(match codec_flag {
            value_codec::ValueFlag::Raw => reddb_file::encode_btree_inline_raw(&codec_bytes),
            value_codec::ValueFlag::Lz4 => reddb_file::encode_btree_inline_compressed(&codec_bytes),
        });
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
pub fn decode<P: PageStore>(pager: &P, stored: &[u8]) -> Result<Vec<u8>, ValueLayoutError> {
    decode_with_budget(pager, stored, usize::MAX)
}

/// Bound allocations before reading an overflow chain or expanding compressed bytes.
/// Compressed overflow retains both the encoded chain and its decoded output.
pub fn decode_with_budget<P: PageStore>(
    pager: &P,
    stored: &[u8],
    limit_bytes: usize,
) -> Result<Vec<u8>, ValueLayoutError> {
    match reddb_file::decode_btree_value_cell(stored)? {
        BTreeValueCell::Pointer {
            is_compressed,
            head_page_id,
            total_len,
        } => {
            let encoded_bytes = usize::try_from(total_len)
                .map_err(|_| ValueLayoutError::ValueTooLarge(usize::MAX))?;
            check_read_budget(encoded_bytes, limit_bytes)?;
            let chain = OverflowChain::new(pager);
            let chain_bytes = chain.read(head_page_id, total_len)?;
            if is_compressed {
                let decoded_bytes = compressed_output_bytes(&chain_bytes)?;
                check_read_budget(encoded_bytes.saturating_add(decoded_bytes), limit_bytes)?;
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
        } => {
            let decoded_bytes = if is_compressed {
                compressed_output_bytes(payload)?
            } else {
                payload.len()
            };
            check_read_budget(decoded_bytes, limit_bytes)?;
            Ok(reddb_file::decode_btree_inline_payload(
                is_compressed,
                payload,
            )?)
        }
    }
}

fn compressed_output_bytes(bytes: &[u8]) -> Result<usize, ValueLayoutError> {
    value_codec::decoded_length(value_codec::ValueFlag::Lz4, bytes).map_err(ValueLayoutError::Codec)
}

fn check_read_budget(required_bytes: usize, limit_bytes: usize) -> Result<(), ValueLayoutError> {
    if required_bytes > limit_bytes {
        return Err(ValueLayoutError::ReadLimitExceeded {
            required_bytes,
            limit_bytes,
        });
    }
    Ok(())
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
