//! On-disk page format for the vector B-tree large-value path.
//!
//! Self-contained format module: knows nothing about pagers, MVCC, or
//! the overflow chain. Subsequent slices wire this format into the
//! engine; this slice lands the format itself with round-trip and v1
//! backward-read coverage.
//!
//! Two changes relative to v1:
//!
//! 1. **`PageType::Overflow`** so deserialisation can tell overflow
//!    pages from leaf / internal / free pages.
//! 2. **Two leaf-cell flag bits** — `pointer` (vs inline) and
//!    `compressed` (vs raw) — encoding the four shapes the read path
//!    must dispatch on:
//!      - inline + raw    → bytes-as-stored
//!      - inline + compressed → decode then return
//!      - pointer + raw   → follow pointer then return
//!      - pointer + compressed → follow pointer then decode
//!
//! V1 cells have no flag byte. The loader infers `(inline, raw)` for
//! every v1 cell so existing files keep reading byte-identically.
//! New writes always emit v2.
//!
//! The version is exposed as a constant — callers must read it from
//! [`FORMAT_VERSION`] / [`FORMAT_VERSION_V1`] rather than hard-coding.

use std::fmt;

/// Legacy on-disk format. Cells are stored as `[key_len: u16,
/// value_len: u32, key, value]` with no flag byte; the loader infers
/// `(inline, raw)` for every cell. v1 files keep reading correctly
/// under v2 code.
pub const FORMAT_VERSION_V1: u16 = 1;

/// Current on-disk format. Adds `PageType::Overflow` and a one-byte
/// flag prefix on every leaf cell.
pub const FORMAT_VERSION_V2: u16 = 2;

/// Format version stamped into freshly-written page headers. Always
/// the latest version the code knows how to write.
pub const FORMAT_VERSION: u16 = FORMAT_VERSION_V2;

/// Size of an encoded page header in bytes.
pub const PAGE_HEADER_SIZE: usize = 5;

/// Cell flag byte layout for v2 leaf cells. Bit 0 = pointer, bit 1 =
/// compressed. Higher bits are reserved and must be zero on disk —
/// the decoder rejects unknown bits so a future format extension
/// fails loudly instead of being silently misread.
const FLAG_POINTER: u8 = 0b0000_0001;
const FLAG_COMPRESSED: u8 = 0b0000_0010;
const FLAG_RESERVED_MASK: u8 = !(FLAG_POINTER | FLAG_COMPRESSED);

/// Type of a vector B-tree page. The byte encoding is part of the
/// stable on-disk contract — do not reorder existing variants.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PageType {
    /// Free page available for allocation.
    Free = 0,
    /// Leaf page holding key-value cells.
    Leaf = 1,
    /// Internal (interior) page holding routing keys.
    Internal = 2,
    /// Overflow page — continuation of a spilled large value.
    /// Added in v2 so the engine can dispatch on page type without
    /// touching the cell payload.
    Overflow = 3,
}

impl PageType {
    /// Decode a page-type byte. Unknown bytes are rejected so format
    /// drift fails loudly at read time.
    pub fn from_byte(b: u8) -> Result<Self, PageFormatError> {
        match b {
            0 => Ok(PageType::Free),
            1 => Ok(PageType::Leaf),
            2 => Ok(PageType::Internal),
            3 => Ok(PageType::Overflow),
            other => Err(PageFormatError::UnknownPageType(other)),
        }
    }

    /// Encode as the on-disk byte.
    #[inline]
    pub fn to_byte(self) -> u8 {
        self as u8
    }
}

/// Leaf-cell flag bits. Each bit is independent — the four
/// combinations describe how the read path interprets the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LeafCellFlags {
    /// `true` → payload is a pointer to an overflow chain head.
    /// `false` → payload bytes live in the cell.
    pub is_pointer: bool,
    /// `true` → payload (or what the pointer resolves to) is
    /// compressed and must be decoded before return.
    pub is_compressed: bool,
}

impl LeafCellFlags {
    /// `(inline, raw)` — the v1-equivalent shape. Used as the
    /// inferred flag for every cell read out of a v1 page.
    pub const INLINE_RAW: Self = LeafCellFlags {
        is_pointer: false,
        is_compressed: false,
    };

    /// Encode the flag bits as the on-disk byte.
    pub fn to_byte(self) -> u8 {
        let mut b = 0u8;
        if self.is_pointer {
            b |= FLAG_POINTER;
        }
        if self.is_compressed {
            b |= FLAG_COMPRESSED;
        }
        b
    }

    /// Decode a flag byte. Reserved bits must be zero — non-zero
    /// reserved bits indicate format drift and are rejected.
    pub fn from_byte(b: u8) -> Result<Self, PageFormatError> {
        if b & FLAG_RESERVED_MASK != 0 {
            return Err(PageFormatError::UnknownCellFlags(b));
        }
        Ok(LeafCellFlags {
            is_pointer: b & FLAG_POINTER != 0,
            is_compressed: b & FLAG_COMPRESSED != 0,
        })
    }
}

/// Decoded page header. Encoded on disk as
/// `[version: u16 LE, page_type: u8, cell_count: u16 LE]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageHeader {
    pub version: u16,
    pub page_type: PageType,
    pub cell_count: u16,
}

impl PageHeader {
    /// Build a fresh header at the current format version.
    pub fn new(page_type: PageType, cell_count: u16) -> Self {
        Self {
            version: FORMAT_VERSION,
            page_type,
            cell_count,
        }
    }

    /// Serialise into the first [`PAGE_HEADER_SIZE`] bytes of `out`.
    pub fn encode(&self, out: &mut [u8]) -> Result<(), PageFormatError> {
        if out.len() < PAGE_HEADER_SIZE {
            return Err(PageFormatError::ShortBuffer {
                need: PAGE_HEADER_SIZE,
                got: out.len(),
            });
        }
        out[0..2].copy_from_slice(&self.version.to_le_bytes());
        out[2] = self.page_type.to_byte();
        out[3..5].copy_from_slice(&self.cell_count.to_le_bytes());
        Ok(())
    }

    /// Parse the first [`PAGE_HEADER_SIZE`] bytes of `bytes`. Versions
    /// newer than [`FORMAT_VERSION`] are rejected — we never silently
    /// read a format we cannot write.
    pub fn decode(bytes: &[u8]) -> Result<Self, PageFormatError> {
        if bytes.len() < PAGE_HEADER_SIZE {
            return Err(PageFormatError::ShortBuffer {
                need: PAGE_HEADER_SIZE,
                got: bytes.len(),
            });
        }
        let version = u16::from_le_bytes([bytes[0], bytes[1]]);
        if version == 0 || version > FORMAT_VERSION {
            return Err(PageFormatError::UnsupportedVersion(version));
        }
        let page_type = PageType::from_byte(bytes[2])?;
        let cell_count = u16::from_le_bytes([bytes[3], bytes[4]]);
        Ok(Self {
            version,
            page_type,
            cell_count,
        })
    }
}

/// View of a decoded leaf cell. The payload slice borrows from the
/// underlying buffer so decode is allocation-free; callers materialise
/// (e.g. follow pointer + decompress) downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeafCell<'a> {
    pub flags: LeafCellFlags,
    pub key: &'a [u8],
    pub payload: &'a [u8],
}

/// Encode a v2 leaf cell into `out`. Format:
/// `[flags: u8, key_len: u16 LE, payload_len: u32 LE, key, payload]`.
pub fn encode_leaf_cell_v2(
    flags: LeafCellFlags,
    key: &[u8],
    payload: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), PageFormatError> {
    if key.len() > u16::MAX as usize {
        return Err(PageFormatError::FieldTooLarge {
            field: "key",
            len: key.len(),
        });
    }
    if payload.len() > u32::MAX as usize {
        return Err(PageFormatError::FieldTooLarge {
            field: "payload",
            len: payload.len(),
        });
    }
    out.reserve(1 + 2 + 4 + key.len() + payload.len());
    out.push(flags.to_byte());
    out.extend_from_slice(&(key.len() as u16).to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(key);
    out.extend_from_slice(payload);
    Ok(())
}

/// Encode a v1 leaf cell into `out`. Format:
/// `[key_len: u16 LE, payload_len: u32 LE, key, payload]` — no flag
/// byte. Only used by tests that build legacy fixtures; production
/// writes always go through [`encode_leaf_cell_v2`].
pub fn encode_leaf_cell_v1(
    key: &[u8],
    payload: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), PageFormatError> {
    if key.len() > u16::MAX as usize {
        return Err(PageFormatError::FieldTooLarge {
            field: "key",
            len: key.len(),
        });
    }
    if payload.len() > u32::MAX as usize {
        return Err(PageFormatError::FieldTooLarge {
            field: "payload",
            len: payload.len(),
        });
    }
    out.reserve(2 + 4 + key.len() + payload.len());
    out.extend_from_slice(&(key.len() as u16).to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(key);
    out.extend_from_slice(payload);
    Ok(())
}

/// Decode one leaf cell from the head of `bytes`, dispatching on
/// `version`. Returns the decoded cell and the number of bytes
/// consumed so callers can walk a packed cell stream.
///
/// For [`FORMAT_VERSION_V1`] there is no flag byte; the cell is
/// reported with [`LeafCellFlags::INLINE_RAW`] and the payload bytes
/// are returned byte-identically — that is the v1 read-compat
/// contract.
pub fn decode_leaf_cell(
    version: u16,
    bytes: &[u8],
) -> Result<(LeafCell<'_>, usize), PageFormatError> {
    match version {
        FORMAT_VERSION_V1 => decode_leaf_cell_v1(bytes),
        FORMAT_VERSION_V2 => decode_leaf_cell_v2(bytes),
        other => Err(PageFormatError::UnsupportedVersion(other)),
    }
}

fn decode_leaf_cell_v1(bytes: &[u8]) -> Result<(LeafCell<'_>, usize), PageFormatError> {
    if bytes.len() < 6 {
        return Err(PageFormatError::TruncatedCell);
    }
    let key_len = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
    let payload_len = u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]) as usize;
    let total = 6 + key_len + payload_len;
    if bytes.len() < total {
        return Err(PageFormatError::TruncatedCell);
    }
    let key = &bytes[6..6 + key_len];
    let payload = &bytes[6 + key_len..6 + key_len + payload_len];
    Ok((
        LeafCell {
            flags: LeafCellFlags::INLINE_RAW,
            key,
            payload,
        },
        total,
    ))
}

fn decode_leaf_cell_v2(bytes: &[u8]) -> Result<(LeafCell<'_>, usize), PageFormatError> {
    if bytes.len() < 7 {
        return Err(PageFormatError::TruncatedCell);
    }
    let flags = LeafCellFlags::from_byte(bytes[0])?;
    let key_len = u16::from_le_bytes([bytes[1], bytes[2]]) as usize;
    let payload_len = u32::from_le_bytes([bytes[3], bytes[4], bytes[5], bytes[6]]) as usize;
    let total = 7 + key_len + payload_len;
    if bytes.len() < total {
        return Err(PageFormatError::TruncatedCell);
    }
    let key = &bytes[7..7 + key_len];
    let payload = &bytes[7 + key_len..7 + key_len + payload_len];
    Ok((
        LeafCell {
            flags,
            key,
            payload,
        },
        total,
    ))
}

/// Errors returned by the page-format codec.
#[derive(Debug, PartialEq, Eq)]
pub enum PageFormatError {
    UnknownPageType(u8),
    UnknownCellFlags(u8),
    UnsupportedVersion(u16),
    ShortBuffer { need: usize, got: usize },
    TruncatedCell,
    FieldTooLarge { field: &'static str, len: usize },
}

impl fmt::Display for PageFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PageFormatError::UnknownPageType(b) => write!(f, "unknown page type byte: {}", b),
            PageFormatError::UnknownCellFlags(b) => {
                write!(f, "unknown leaf-cell flag bits: 0b{:08b}", b)
            }
            PageFormatError::UnsupportedVersion(v) => {
                write!(f, "unsupported page format version: {}", v)
            }
            PageFormatError::ShortBuffer { need, got } => {
                write!(f, "buffer too small: need {} bytes, got {}", need, got)
            }
            PageFormatError::TruncatedCell => write!(f, "leaf cell truncated"),
            PageFormatError::FieldTooLarge { field, len } => {
                write!(f, "{} length {} exceeds on-disk encoding limit", field, len)
            }
        }
    }
}

impl std::error::Error for PageFormatError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_version_constant_is_v2() {
        // The constant is the single source of truth for new writes —
        // tests pin it so a stealth bump shows up here, not later in
        // a corrupt-file bug report.
        assert_eq!(FORMAT_VERSION, 2);
        assert_eq!(FORMAT_VERSION_V1, 1);
        assert_eq!(FORMAT_VERSION_V2, 2);
    }

    #[test]
    fn page_header_round_trips_overflow_type() {
        // Acceptance #1: PageType::Overflow round-trips through page
        // header serialisation.
        let header = PageHeader::new(PageType::Overflow, 0);
        let mut buf = [0u8; PAGE_HEADER_SIZE];
        header.encode(&mut buf).expect("encode");
        let decoded = PageHeader::decode(&buf).expect("decode");
        assert_eq!(decoded, header);
        assert_eq!(decoded.page_type, PageType::Overflow);
        assert_eq!(decoded.version, FORMAT_VERSION_V2);
    }

    #[test]
    fn page_header_round_trips_every_type() {
        for pt in [
            PageType::Free,
            PageType::Leaf,
            PageType::Internal,
            PageType::Overflow,
        ] {
            let header = PageHeader::new(pt, 42);
            let mut buf = [0u8; PAGE_HEADER_SIZE];
            header.encode(&mut buf).unwrap();
            let decoded = PageHeader::decode(&buf).unwrap();
            assert_eq!(decoded.page_type, pt);
            assert_eq!(decoded.cell_count, 42);
        }
    }

    #[test]
    fn page_header_rejects_unknown_type_byte() {
        let mut buf = [0u8; PAGE_HEADER_SIZE];
        buf[0..2].copy_from_slice(&FORMAT_VERSION_V2.to_le_bytes());
        buf[2] = 99;
        assert_eq!(
            PageHeader::decode(&buf).unwrap_err(),
            PageFormatError::UnknownPageType(99)
        );
    }

    #[test]
    fn page_header_rejects_version_newer_than_known() {
        let mut buf = [0u8; PAGE_HEADER_SIZE];
        buf[0..2].copy_from_slice(&7u16.to_le_bytes());
        buf[2] = PageType::Leaf.to_byte();
        assert_eq!(
            PageHeader::decode(&buf).unwrap_err(),
            PageFormatError::UnsupportedVersion(7)
        );
    }

    #[test]
    fn page_header_rejects_version_zero() {
        let mut buf = [0u8; PAGE_HEADER_SIZE];
        buf[2] = PageType::Leaf.to_byte();
        assert_eq!(
            PageHeader::decode(&buf).unwrap_err(),
            PageFormatError::UnsupportedVersion(0)
        );
    }

    #[test]
    fn page_header_decode_rejects_short_buffer() {
        let buf = [0u8; PAGE_HEADER_SIZE - 1];
        assert!(matches!(
            PageHeader::decode(&buf),
            Err(PageFormatError::ShortBuffer { .. })
        ));
    }

    #[test]
    fn leaf_cell_flags_byte_round_trip() {
        for is_pointer in [false, true] {
            for is_compressed in [false, true] {
                let flags = LeafCellFlags {
                    is_pointer,
                    is_compressed,
                };
                let b = flags.to_byte();
                assert_eq!(LeafCellFlags::from_byte(b).unwrap(), flags);
            }
        }
    }

    #[test]
    fn leaf_cell_flags_reject_reserved_bits() {
        // Acceptance: unknown bits in the flag byte are not silently
        // dropped. A future format extension setting bit 2 must blow
        // up under v2 code rather than be misread.
        for reserved in [0b0000_0100u8, 0b1000_0000, 0xFF] {
            assert_eq!(
                LeafCellFlags::from_byte(reserved).unwrap_err(),
                PageFormatError::UnknownCellFlags(reserved)
            );
        }
    }

    #[test]
    fn all_four_leaf_cell_shapes_round_trip() {
        // Acceptance #2: all four flag combinations round-trip with
        // their payload preserved byte-identically.
        let key = b"vec:42".as_slice();
        let payload = b"\xDE\xAD\xBE\xEF\x00\x01\x02\x03".as_slice();
        for flags in [
            LeafCellFlags {
                is_pointer: false,
                is_compressed: false,
            },
            LeafCellFlags {
                is_pointer: false,
                is_compressed: true,
            },
            LeafCellFlags {
                is_pointer: true,
                is_compressed: false,
            },
            LeafCellFlags {
                is_pointer: true,
                is_compressed: true,
            },
        ] {
            let mut buf = Vec::new();
            encode_leaf_cell_v2(flags, key, payload, &mut buf).unwrap();
            let (cell, consumed) = decode_leaf_cell(FORMAT_VERSION_V2, &buf).unwrap();
            assert_eq!(consumed, buf.len(), "consumed must equal encoded size");
            assert_eq!(cell.flags, flags);
            assert_eq!(cell.key, key);
            assert_eq!(cell.payload, payload);
        }
    }

    #[test]
    fn v1_cell_reads_as_inline_raw() {
        // Acceptance #3: v1 cells have no flag byte; the loader
        // infers (inline, raw) and returns payload byte-identically.
        let key = b"legacy-key".as_slice();
        let payload = b"\x00\xFF\x10\x20\x30".as_slice();
        let mut buf = Vec::new();
        encode_leaf_cell_v1(key, payload, &mut buf).unwrap();
        let (cell, consumed) = decode_leaf_cell(FORMAT_VERSION_V1, &buf).unwrap();
        assert_eq!(consumed, buf.len());
        assert_eq!(cell.flags, LeafCellFlags::INLINE_RAW);
        assert!(!cell.flags.is_pointer);
        assert!(!cell.flags.is_compressed);
        assert_eq!(cell.key, key);
        assert_eq!(cell.payload, payload);
    }

    #[test]
    fn v1_stream_of_cells_decodes_byte_identically() {
        // Acceptance #3 (stream form): a v1 page is a packed stream of
        // cells; walk the whole stream and confirm every cell comes
        // back inline+raw with original bytes.
        let cells: Vec<(&[u8], &[u8])> = vec![
            (b"k0", b"v0"),
            (b"k1", b"\x00\x01\x02"),
            (b"", b"empty-key"),
            (b"large", &[0xABu8; 300][..]),
        ];
        let mut buf = Vec::new();
        for (k, v) in &cells {
            encode_leaf_cell_v1(k, v, &mut buf).unwrap();
        }
        let mut cursor = 0;
        for (k, v) in &cells {
            let (cell, n) = decode_leaf_cell(FORMAT_VERSION_V1, &buf[cursor..]).unwrap();
            assert_eq!(cell.flags, LeafCellFlags::INLINE_RAW);
            assert_eq!(cell.key, *k);
            assert_eq!(cell.payload, *v);
            cursor += n;
        }
        assert_eq!(cursor, buf.len(), "stream fully consumed");
    }

    #[test]
    fn freshly_created_page_writes_v2_header() {
        // Acceptance #4 (write side): a freshly-created page header
        // pins version = v2 even when callers don't pass a version
        // explicitly.
        let header = PageHeader::new(PageType::Leaf, 0);
        assert_eq!(header.version, FORMAT_VERSION_V2);
        let mut buf = [0u8; PAGE_HEADER_SIZE];
        header.encode(&mut buf).unwrap();
        assert_eq!(u16::from_le_bytes([buf[0], buf[1]]), FORMAT_VERSION_V2);
    }

    #[test]
    fn v1_page_still_reads_after_partial_rewrites_in_place() {
        // Acceptance #4 (read side): a v1 file rewritten in-place
        // (some original cells, some freshly-written v1 cells) keeps
        // reading correctly. Updated cells stay v1-format because the
        // page header still says v1 — the v1 read path doesn't care
        // when each cell was written, only that none of them carry a
        // flag byte.
        let originals: Vec<(Vec<u8>, Vec<u8>)> = vec![
            (b"orig-a".to_vec(), b"value-a".to_vec()),
            (b"orig-b".to_vec(), b"value-b".to_vec()),
            (b"orig-c".to_vec(), b"value-c".to_vec()),
        ];
        let mut page_bytes = Vec::new();
        // Write a v1 page header so the page's format version is 1.
        let v1_header = PageHeader {
            version: FORMAT_VERSION_V1,
            page_type: PageType::Leaf,
            cell_count: originals.len() as u16,
        };
        let mut hdr_buf = [0u8; PAGE_HEADER_SIZE];
        v1_header.encode(&mut hdr_buf).unwrap();
        page_bytes.extend_from_slice(&hdr_buf);

        // Pack three v1 cells, then rewrite the middle one in-place
        // with a new payload — still v1, no flag byte.
        let mut cell_offsets = Vec::new();
        for (k, v) in &originals {
            cell_offsets.push(page_bytes.len());
            encode_leaf_cell_v1(k, v, &mut page_bytes).unwrap();
        }

        // Replace cell 1's payload in-place with one of the same length.
        // (Same length keeps offsets stable, which is the realistic
        // shape of an in-place rewrite — the only kind v1 supports
        // without restructuring the page.)
        let new_value = b"VALUE-B"; // same length as "value-b"
        assert_eq!(new_value.len(), originals[1].1.len());
        let rewrite_start = cell_offsets[1] + 2 + 4 + originals[1].0.len();
        page_bytes[rewrite_start..rewrite_start + new_value.len()].copy_from_slice(new_value);

        // Reopen: header says v1, so every cell — original or
        // rewritten — must read as (inline, raw) with the latest
        // bytes on disk.
        let header = PageHeader::decode(&page_bytes[..PAGE_HEADER_SIZE]).unwrap();
        assert_eq!(header.version, FORMAT_VERSION_V1);
        let mut cursor = PAGE_HEADER_SIZE;
        let expected: Vec<(&[u8], &[u8])> = vec![
            (&originals[0].0, &originals[0].1),
            (&originals[1].0, new_value),
            (&originals[2].0, &originals[2].1),
        ];
        for (k, v) in expected {
            let (cell, n) = decode_leaf_cell(header.version, &page_bytes[cursor..]).unwrap();
            assert_eq!(cell.flags, LeafCellFlags::INLINE_RAW);
            assert_eq!(cell.key, k);
            assert_eq!(cell.payload, v);
            cursor += n;
        }
        assert_eq!(cursor, page_bytes.len());
    }

    #[test]
    fn page_type_byte_values_are_stable() {
        // Pin the on-disk encoding so a future reorder of the enum
        // can't silently break v1 files. These bytes are the contract.
        assert_eq!(PageType::Free.to_byte(), 0);
        assert_eq!(PageType::Leaf.to_byte(), 1);
        assert_eq!(PageType::Internal.to_byte(), 2);
        assert_eq!(PageType::Overflow.to_byte(), 3);
    }

    #[test]
    fn decode_leaf_cell_rejects_truncation() {
        let mut buf = Vec::new();
        encode_leaf_cell_v2(LeafCellFlags::INLINE_RAW, b"abc", b"xyz", &mut buf).unwrap();
        for trunc in 0..buf.len() {
            assert_eq!(
                decode_leaf_cell(FORMAT_VERSION_V2, &buf[..trunc]).unwrap_err(),
                PageFormatError::TruncatedCell,
                "truncation at {} bytes must be rejected",
                trunc
            );
        }
    }

    #[test]
    fn decode_leaf_cell_unknown_version_rejected() {
        let buf = [0u8; 16];
        assert_eq!(
            decode_leaf_cell(99, &buf).unwrap_err(),
            PageFormatError::UnsupportedVersion(99)
        );
    }

    #[test]
    fn encoded_v2_cell_has_flag_byte_then_v1_layout() {
        // The encoded shape is the contract a future format-aware
        // tool will rely on. Pin it: byte 0 is flags, then v1 layout
        // follows verbatim.
        let mut v2 = Vec::new();
        encode_leaf_cell_v2(
            LeafCellFlags {
                is_pointer: true,
                is_compressed: false,
            },
            b"k",
            b"p",
            &mut v2,
        )
        .unwrap();
        let mut v1 = Vec::new();
        encode_leaf_cell_v1(b"k", b"p", &mut v1).unwrap();
        assert_eq!(v2[0], FLAG_POINTER);
        assert_eq!(&v2[1..], &v1[..]);
    }
}
