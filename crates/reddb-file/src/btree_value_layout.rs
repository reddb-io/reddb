//! Persisted value-cell layout for B-tree leaf payloads.
//!
//! This module owns the byte contract only. Runtime decisions such as page
//! allocation, overflow-chain IO, and MVCC stay with the storage engine.

use crate::vector_value_codec;

/// At or below this length a B-tree value stores inline in the leaf.
pub const BTREE_VALUE_OVERFLOW_THRESHOLD: usize = 1024;

/// Hard upper bound on logical B-tree value size: 2^28 = 256 MiB.
pub const BTREE_VALUE_MAX_SIZE: usize = 256 * 1024 * 1024;

const FLAG_POINTER: u8 = 0b0000_0001;
const FLAG_COMPRESSED: u8 = 0b0000_0010;
const FLAG_RESERVED_MASK: u8 = !(FLAG_POINTER | FLAG_COMPRESSED);

const POINTER_PAYLOAD_LEN: usize = 4 + 8;

/// Total stored bytes for a pointer cell: flag byte + pointer payload.
pub const BTREE_VALUE_POINTER_CELL_LEN: usize = 1 + POINTER_PAYLOAD_LEN;

/// Decoded persisted B-tree value-cell shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BTreeValueCell<'a> {
    Inline {
        is_compressed: bool,
        payload: &'a [u8],
    },
    Pointer {
        is_compressed: bool,
        head_page_id: u32,
        total_len: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BTreeValueCellError {
    UnknownFlag(u8),
    TruncatedPointer { got: usize },
}

impl std::fmt::Display for BTreeValueCellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownFlag(b) => write!(f, "unknown B-tree value-cell flag byte: 0b{b:08b}"),
            Self::TruncatedPointer { got } => write!(
                f,
                "B-tree value pointer cell truncated: need {POINTER_PAYLOAD_LEN} bytes after flag, got {got}"
            ),
        }
    }
}

impl std::error::Error for BTreeValueCellError {}

/// Encode an inline raw value-cell payload.
pub fn encode_btree_inline_raw(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + value.len());
    out.push(0);
    out.extend_from_slice(value);
    out
}

/// Encode an inline compressed value-cell payload.
pub fn encode_btree_inline_compressed(codec_bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + codec_bytes.len());
    out.push(FLAG_COMPRESSED);
    out.extend_from_slice(codec_bytes);
    out
}

/// Encode an overflow pointer value-cell payload.
pub fn encode_btree_pointer(head_page_id: u32, total_len: u64, is_compressed: bool) -> Vec<u8> {
    let mut flag = FLAG_POINTER;
    if is_compressed {
        flag |= FLAG_COMPRESSED;
    }

    let mut out = Vec::with_capacity(BTREE_VALUE_POINTER_CELL_LEN);
    out.push(flag);
    out.extend_from_slice(&head_page_id.to_le_bytes());
    out.extend_from_slice(&total_len.to_le_bytes());
    out
}

/// Decode the persisted value-cell shape without materialising overflow data.
pub fn decode_btree_value_cell(stored: &[u8]) -> Result<BTreeValueCell<'_>, BTreeValueCellError> {
    if stored.is_empty() {
        return Ok(BTreeValueCell::Inline {
            is_compressed: false,
            payload: &[],
        });
    }

    let flag = stored[0];
    if flag & FLAG_RESERVED_MASK != 0 {
        return Err(BTreeValueCellError::UnknownFlag(flag));
    }

    let is_pointer = flag & FLAG_POINTER != 0;
    let is_compressed = flag & FLAG_COMPRESSED != 0;
    let payload = &stored[1..];

    if !is_pointer {
        return Ok(BTreeValueCell::Inline {
            is_compressed,
            payload,
        });
    }

    if payload.len() != POINTER_PAYLOAD_LEN {
        return Err(BTreeValueCellError::TruncatedPointer { got: payload.len() });
    }

    let head_page_id = u32::from_le_bytes(
        payload[0..4]
            .try_into()
            .expect("pointer head length checked"),
    );
    let total_len = u64::from_le_bytes(payload[4..12].try_into().expect("pointer length checked"));

    Ok(BTreeValueCell::Pointer {
        is_compressed,
        head_page_id,
        total_len,
    })
}

/// Return the overflow head page id when `stored` is a valid pointer cell.
pub fn btree_value_pointer_head(stored: &[u8]) -> Option<u32> {
    match decode_btree_value_cell(stored).ok()? {
        BTreeValueCell::Pointer { head_page_id, .. } => Some(head_page_id),
        BTreeValueCell::Inline { .. } => None,
    }
}

/// Return the value-cell size the B-tree stores for the chosen codec length.
pub fn btree_projected_cell_len(input_len: usize, codec_len: usize) -> usize {
    if input_len <= BTREE_VALUE_OVERFLOW_THRESHOLD {
        return 1 + input_len;
    }

    if codec_len < input_len && codec_len <= BTREE_VALUE_OVERFLOW_THRESHOLD {
        1 + codec_len
    } else {
        BTREE_VALUE_POINTER_CELL_LEN
    }
}

/// Decode an inline payload using the shared persisted value codec.
pub fn decode_btree_inline_payload(
    is_compressed: bool,
    payload: &[u8],
) -> Result<Vec<u8>, vector_value_codec::ValueCodecError> {
    let flag = if is_compressed {
        vector_value_codec::ValueFlag::Lz4
    } else {
        vector_value_codec::ValueFlag::Raw
    };
    vector_value_codec::decode(flag, payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_raw_cell_round_trips() {
        let stored = encode_btree_inline_raw(b"abc");
        assert_eq!(stored, vec![0, b'a', b'b', b'c']);
        assert_eq!(
            decode_btree_value_cell(&stored).unwrap(),
            BTreeValueCell::Inline {
                is_compressed: false,
                payload: b"abc",
            }
        );
    }

    #[test]
    fn inline_compressed_cell_round_trips_shape() {
        let stored = encode_btree_inline_compressed(&[1, 2, 3]);
        assert_eq!(
            decode_btree_value_cell(&stored).unwrap(),
            BTreeValueCell::Inline {
                is_compressed: true,
                payload: &[1, 2, 3],
            }
        );
    }

    #[test]
    fn pointer_cell_round_trips_little_endian_payload() {
        let stored = encode_btree_pointer(0x0102_0304, 0x1112_1314_1516_1718, true);
        assert_eq!(stored.len(), BTREE_VALUE_POINTER_CELL_LEN);
        assert_eq!(
            decode_btree_value_cell(&stored).unwrap(),
            BTreeValueCell::Pointer {
                is_compressed: true,
                head_page_id: 0x0102_0304,
                total_len: 0x1112_1314_1516_1718,
            }
        );
        assert_eq!(btree_value_pointer_head(&stored), Some(0x0102_0304));
    }

    #[test]
    fn malformed_pointer_has_no_head() {
        assert_eq!(btree_value_pointer_head(&[FLAG_POINTER, 1, 2]), None);
        assert_eq!(btree_value_pointer_head(&[0b0000_0100]), None);
    }

    #[test]
    fn projected_len_matches_inline_and_pointer_cases() {
        assert_eq!(btree_projected_cell_len(10, 10), 11);
        assert_eq!(btree_projected_cell_len(2048, 100), 101);
        assert_eq!(
            btree_projected_cell_len(2048, 2048),
            BTREE_VALUE_POINTER_CELL_LEN
        );
    }
}
