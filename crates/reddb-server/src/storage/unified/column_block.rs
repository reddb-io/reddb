//! Sealed hypertable chunk — columnar on-disk layout v1 (`RDCC`).
//!
//! This is the **physical columnar form of a sealed hypertable chunk**,
//! not a parallel object: PRD #850 rejects a standalone "columnar
//! segment" as a bloat vector. The sealed chunk *is* the columnar
//! segment, and this module defines its durable byte contract — emitted
//! as a [`PageType::ColumnBlock`](crate::storage::engine::PageType) page.
//!
//! It is the first real caller of [`segment_codec`](super::segment_codec):
//! every column stream is one `segment_codec` pipeline output. The codec
//! chain is chosen **per column** from its [`ColumnSemantics`] via
//! [`select_codecs`](super::segment_codec::select_codecs) (#853) — the
//! directory's `codec` byte records the leading (semantic) codec so the
//! sealed chunk is self-describing. A column's **sparse granule index**
//! (one min/max mark per `granule_size` rows — #854) is stored as a
//! self-describing blob whose offset/length live in the directory's
//! `granule_index_off`/`granule_index_len` (zeroed when a column has no
//! index, e.g. variable-width streams). A column's **per-granule bloom skip
//! index** (one split-block bloom per `granule_size` rows — #855) is stored
//! the same way, pointed at by the directory's now-live `bloom_off`/
//! `bloom_len` (zeroed when a column has no bloom). The bloom serves
//! equality/point predicates with a false-positives-only contract; min/max
//! serves ranges. All extensions live inside this envelope without forcing a
//! format v2.
//!
//! # Layout (`RDCC`)
//!
//! ```text
//! Header (52 bytes)
//!   magic            b"RDCC"   (4)
//!   format_version   u16  = 1
//!   flags            u16  = 0           reserved
//!   chunk_id         u64
//!   schema_ref       u64                catalog schema id column_ids resolve against
//!   row_count        u64
//!   column_count     u32
//!   min_ts_ns        u64                mirror ChunkMeta → self-describing read
//!   max_ts_ns        u64
//!
//! Column directory   (column_count entries, 54 bytes each, at offset 52)
//!   column_id            u32
//!   logical_type         u8             Value type tag (DataType::to_byte)
//!   codec                u8             ColumnCodec tag (segment_codec)
//!   stream_offset        u64            byte offset of this column's stream within the block
//!   stream_len           u64
//!   granule_index_off    u64            byte offset of this column's granule index (0 = none) #854
//!   granule_index_len    u64            length of the granule index blob (0 = none)
//!   bloom_off            u64            byte offset of this column's granule bloom (0 = none) #855
//!   bloom_len            u64            length of the granule bloom blob (0 = none)
//!
//! Column streams        column_count segment_codec runs, back-to-back
//! Granule indexes       per-column granule-index blobs, back-to-back (#854)
//! Granule blooms        per-column granule-bloom blobs, back-to-back (#855)
//!
//! Granule index blob (per indexed column)
//!   granule_size_rows    u32            rows per mark (last mark may be shorter)
//!   value_width          u32            bytes per min/max value (8 for u64/f64)
//!   granule_count        u32
//!   per granule:  min[value_width]  max[value_width]   raw column-encoded bytes
//!
//! Granule bloom blob (per indexed column)
//!   granule_size_rows    u32            rows per bloom (last bloom may cover fewer)
//!   granule_count        u32
//!   per granule:  num_blocks u32        then num_blocks × 32 bytes of bloom words
//!
//! Footer (24 bytes)
//!   col_directory_off    u64
//!   col_directory_len    u64
//!   crc32                u32            over header+directory+streams
//!   magic_tail           b"RDCC"  (4)
//! ```

use super::segment_codec::{
    decode_bytes, decode_bytes_to_u64, encode_bytes, select_codecs, CodecError, ColumnCodec,
    ColumnSemantics,
};
use crate::storage::primitives::split_block_bloom::{hash_bytes_u32, SplitBlockBloom};

pub use reddb_file::{COLUMN_BLOCK_MAGIC, COLUMN_BLOCK_VERSION_V1};

const HEADER_LEN: usize = reddb_file::COLUMN_BLOCK_HEADER_LEN;
const DIR_ENTRY_LEN: usize = reddb_file::COLUMN_BLOCK_DIR_ENTRY_LEN;
const FOOTER_LEN: usize = reddb_file::COLUMN_BLOCK_FOOTER_LEN;

/// A logical column handed to [`write_column_block`]. `data` is the
/// column's raw, uncompressed, little-endian value bytes; the writer runs
/// it through the `segment_codec` pipeline to produce the stored stream.
#[derive(Debug, Clone)]
pub struct ColumnInput<'a> {
    /// Stable column id resolved against `schema_ref`.
    pub column_id: u32,
    /// Logical type tag (`DataType::to_byte()`).
    pub logical_type: u8,
    /// The column's role — drives per-column codec selection (#853).
    /// Callers that have no semantic hint pass [`ColumnSemantics::Generic`].
    pub semantics: ColumnSemantics,
    /// Raw little-endian column bytes (pre-compression).
    pub data: &'a [u8],
}

/// One decoded column produced by [`read_column_block`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedColumn {
    pub column_id: u32,
    pub logical_type: u8,
    /// Codec tag recorded in the directory for this column.
    pub codec_tag: u8,
    /// Decompressed raw little-endian column bytes — identical to the
    /// `ColumnInput::data` the writer was given.
    pub data: Vec<u8>,
    /// Sparse granule index for this column (#854), or `None` when the
    /// directory recorded a zero-length granule slice (variable-width /
    /// non-numeric columns, or `granule_size == 0` at write time).
    pub granule_index: Option<GranuleIndex>,
    /// Per-granule bloom skip index for this column (#855), or `None` when
    /// the directory recorded a zero-length bloom slice. Serves
    /// equality/point predicates with a false-positives-only contract.
    pub granule_bloom: Option<GranuleBloom>,
}

/// A fully decoded column block: the self-describing header plus every
/// column's raw bytes, ready to transpose back into rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedColumnBlock {
    pub chunk_id: u64,
    pub schema_ref: u64,
    pub row_count: u64,
    pub min_ts_ns: u64,
    pub max_ts_ns: u64,
    pub columns: Vec<DecodedColumn>,
}

/// Failures decoding (or, rarely, encoding) a column block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnBlockError {
    /// Buffer shorter than the fixed header/footer it must contain.
    Truncated,
    /// Leading magic was not `RDCC`.
    BadMagic([u8; 4]),
    /// Trailing magic was not `RDCC` (block was clipped or corrupt).
    BadTailMagic([u8; 4]),
    /// `format_version` is newer than this build understands.
    UnsupportedVersion(u16),
    /// A directory entry points outside the block's byte range.
    BadDirectory,
    /// CRC32 over header+directory+streams did not match the footer.
    ChecksumMismatch { expected: u32, actual: u32 },
    /// A column stream failed to decode through `segment_codec`.
    Codec(CodecError),
}

impl std::fmt::Display for ColumnBlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "column block truncated"),
            Self::BadMagic(m) => write!(f, "bad column block magic: {m:?}"),
            Self::BadTailMagic(m) => write!(f, "bad column block tail magic: {m:?}"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported column block version: {v}"),
            Self::BadDirectory => write!(f, "column directory entry out of range"),
            Self::ChecksumMismatch { expected, actual } => write!(
                f,
                "column block checksum mismatch: expected 0x{expected:08X}, got 0x{actual:08X}"
            ),
            Self::Codec(e) => write!(f, "column stream codec error: {e}"),
        }
    }
}

impl std::error::Error for ColumnBlockError {}

impl From<CodecError> for ColumnBlockError {
    fn from(e: CodecError) -> Self {
        Self::Codec(e)
    }
}

impl From<reddb_file::ColumnBlockFrameError> for ColumnBlockError {
    fn from(e: reddb_file::ColumnBlockFrameError) -> Self {
        match e {
            reddb_file::ColumnBlockFrameError::Truncated => Self::Truncated,
            reddb_file::ColumnBlockFrameError::BadMagic(magic) => Self::BadMagic(magic),
            reddb_file::ColumnBlockFrameError::BadTailMagic(magic) => Self::BadTailMagic(magic),
            reddb_file::ColumnBlockFrameError::UnsupportedVersion(version) => {
                Self::UnsupportedVersion(version)
            }
            reddb_file::ColumnBlockFrameError::BadDirectory => Self::BadDirectory,
            reddb_file::ColumnBlockFrameError::ChecksumMismatch { expected, actual } => {
                Self::ChecksumMismatch { expected, actual }
            }
        }
    }
}

/// Per-granule min/max statistics for one column. `min`/`max` are raw
/// value bytes in the *same* little-endian encoding as the column data,
/// so a reader interprets them with the column's logical type — the index
/// itself stays type-agnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GranuleStats {
    pub min: Vec<u8>,
    pub max: Vec<u8>,
}

/// Sparse granule index over one column: one [`GranuleStats`] mark per
/// `granule_size` rows (PRD #850 Phase 1, #854). It is RAM-resident — the
/// reader prunes granules whose `[min, max]` cannot match a range/point
/// predicate and materialises only the survivors. There is no dense
/// per-key index; this is the chunk's BRIN-style skip index made granular.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GranuleIndex {
    /// Rows per granule mark. The final granule may hold fewer rows.
    pub granule_size: u32,
    /// Width in bytes of each min/max value (8 for u64/f64 columns).
    pub value_width: u32,
    /// One min/max pair per granule, in row order.
    pub granules: Vec<GranuleStats>,
}

impl GranuleIndex {
    /// Number of granule marks.
    pub fn granule_count(&self) -> usize {
        self.granules.len()
    }

    /// Row range `[start, end)` covered by granule `i`, clamped to
    /// `row_count`. Granules tile the column in `granule_size` strides, so
    /// the range is derived rather than stored.
    pub fn row_range(&self, i: usize, row_count: usize) -> (usize, usize) {
        let g = (self.granule_size as usize).max(1);
        let start = i.saturating_mul(g).min(row_count);
        let end = (i + 1).saturating_mul(g).min(row_count);
        (start, end)
    }

    /// Indices of granules whose `[min, max]` interval may satisfy a
    /// predicate, per the caller-supplied `overlaps(min, max)` test.
    /// Granules the test rejects are pruned. The test receives the raw
    /// min/max bytes and interprets them with the column's logical type.
    pub fn surviving_granules<F>(&self, overlaps: F) -> Vec<usize>
    where
        F: Fn(&[u8], &[u8]) -> bool,
    {
        (0..self.granules.len())
            .filter(|&i| overlaps(&self.granules[i].min, &self.granules[i].max))
            .collect()
    }

    /// Serialize to the self-describing blob the column directory points at.
    fn serialize(&self) -> Vec<u8> {
        let granules = self
            .granules
            .iter()
            .map(|g| reddb_file::ColumnBlockGranuleStats {
                min: g.min.clone(),
                max: g.max.clone(),
            })
            .collect();
        reddb_file::encode_column_block_granule_index_blob(&reddb_file::ColumnBlockGranuleIndex {
            granule_size: self.granule_size,
            value_width: self.value_width,
            granules,
        })
    }

    /// Inverse of [`GranuleIndex::serialize`]. A malformed blob is reported
    /// as [`ColumnBlockError::BadDirectory`] — the index lives inside the
    /// CRC-covered region, so corruption is caught upstream; this guards
    /// only against internally-inconsistent lengths.
    fn deserialize(bytes: &[u8]) -> Result<GranuleIndex, ColumnBlockError> {
        let decoded = reddb_file::decode_column_block_granule_index_blob(bytes)?;
        let granules = decoded
            .granules
            .into_iter()
            .map(|g| GranuleStats {
                min: g.min,
                max: g.max,
            })
            .collect();
        Ok(GranuleIndex {
            granule_size: decoded.granule_size,
            value_width: decoded.value_width,
            granules,
        })
    }
}

/// Per-granule bloom skip index over one column (#855): one
/// [`SplitBlockBloom`] per `granule_size` rows, tiled on the *same* granule
/// boundaries as [`GranuleIndex`] so `row_range` is shared. Where the min/max
/// index serves range predicates, the bloom serves **equality/point**
/// predicates: a granule is probed for the target value and kept only if the
/// bloom *may* contain it. The split-block bloom never reports a false
/// negative, so a granule that actually holds the value always probes true —
/// the pruner therefore over-includes (false positives) but never
/// under-includes (PRD #850 Phase 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GranuleBloom {
    /// Rows per granule bloom. The final granule may cover fewer rows.
    pub granule_size: u32,
    /// One bloom per granule, in row order.
    pub blooms: Vec<SplitBlockBloom>,
}

impl GranuleBloom {
    /// Number of granule blooms.
    pub fn granule_count(&self) -> usize {
        self.blooms.len()
    }

    /// Row range `[start, end)` covered by granule `i`, clamped to
    /// `row_count` — identical tiling to [`GranuleIndex::row_range`].
    pub fn row_range(&self, i: usize, row_count: usize) -> (usize, usize) {
        let g = (self.granule_size as usize).max(1);
        let start = i.saturating_mul(g).min(row_count);
        let end = (i + 1).saturating_mul(g).min(row_count);
        (start, end)
    }

    /// Indices of granules whose bloom **may** contain `key_bytes`, i.e. the
    /// granules an equality predicate cannot prove empty. `key_bytes` are the
    /// raw little-endian value bytes, folded to a `u32` with the same
    /// [`hash_bytes_u32`] the writer used. A granule the bloom rejects is
    /// definitely absent and pruned; survivors may be false positives.
    pub fn surviving_granules(&self, key_bytes: &[u8]) -> Vec<usize> {
        let key = hash_bytes_u32(key_bytes);
        (0..self.blooms.len())
            .filter(|&i| self.blooms[i].probe(key))
            .collect()
    }

    /// Serialize to the self-describing blob the column directory points at.
    /// Each granule's bloom carries its own block count, so granules of
    /// different row spans (e.g. a short final granule) serialize cleanly.
    fn serialize(&self) -> Vec<u8> {
        let bloom_bytes: Vec<Vec<u8>> = self.blooms.iter().map(SplitBlockBloom::to_bytes).collect();
        let bloom_refs: Vec<&[u8]> = bloom_bytes.iter().map(Vec::as_slice).collect();
        reddb_file::encode_column_block_granule_bloom_blob(self.granule_size, &bloom_refs)
    }

    /// Inverse of [`GranuleBloom::serialize`]. Like the granule index it
    /// lives inside the CRC-covered region, so this only guards against
    /// internally-inconsistent lengths, reported as
    /// [`ColumnBlockError::BadDirectory`].
    fn deserialize(bytes: &[u8]) -> Result<GranuleBloom, ColumnBlockError> {
        let decoded = reddb_file::decode_column_block_granule_bloom_blob(bytes)?;
        let mut blooms = Vec::with_capacity(decoded.blooms.len());
        for bloom_bytes in decoded.blooms {
            let bloom =
                SplitBlockBloom::from_bytes(bloom_bytes).ok_or(ColumnBlockError::BadDirectory)?;
            blooms.push(bloom);
        }
        Ok(GranuleBloom {
            granule_size: decoded.granule_size,
            blooms,
        })
    }
}

/// Build a per-granule bloom index over a fixed-width numeric column, on the
/// same granule boundaries as [`build_granule_index`]. Returns `None` under
/// the same conditions (zero stride, empty/ragged data, non-numeric column),
/// so the directory's bloom slice stays zero-length in lockstep with the
/// min/max slice. Every value in a granule is folded through
/// [`hash_bytes_u32`] and inserted, so the bloom probes true for any value it
/// holds — the no-false-negative property the pruner relies on.
fn build_granule_bloom(logical_type: u8, granule_size: u32, data: &[u8]) -> Option<GranuleBloom> {
    if granule_size == 0 {
        return None;
    }
    NumKind::from_logical(logical_type)?;
    if data.is_empty() || !data.len().is_multiple_of(8) {
        return None;
    }
    let n = data.len() / 8;
    let g = granule_size as usize;
    let mut blooms = Vec::with_capacity(n.div_ceil(g));
    let mut start = 0usize;
    while start < n {
        let end = (start + g).min(n);
        let mut bloom = SplitBlockBloom::with_capacity(end - start);
        for v in data[start * 8..end * 8].chunks_exact(8) {
            bloom.insert(hash_bytes_u32(v));
        }
        blooms.push(bloom);
        start = end;
    }
    Some(GranuleBloom {
        granule_size,
        blooms,
    })
}

/// Type-aware ordering for the fixed-width numeric columns a granule index
/// covers. The min/max bytes are stored in the column's own little-endian
/// encoding, but the *ordering* differs per type (raw byte order is wrong
/// for signed ints and floats), so the index is built and compared through
/// this kind.
#[derive(Debug, Clone, Copy)]
enum NumKind {
    I64,
    U64,
    F64,
}

impl NumKind {
    /// Map a [`DataType::to_byte`](crate::storage::schema::types::DataType::to_byte)
    /// tag to a fixed-width numeric kind, or `None` for variable-width /
    /// non-numeric columns (which get no granule index in v1).
    fn from_logical(logical_type: u8) -> Option<NumKind> {
        match logical_type {
            // Integer / Timestamp / Duration are all i64-shaped on the wire.
            1 | 7 | 8 => Some(NumKind::I64),
            2 => Some(NumKind::U64), // UnsignedInteger
            3 => Some(NumKind::F64), // Float
            _ => None,
        }
    }
}

/// Build a sparse granule index over a fixed-width numeric column. Returns
/// `None` for `granule_size == 0`, empty/ragged data, or a column type
/// that has no total numeric order (variable-width streams are skipped in
/// v1 — the directory slice stays zero-length). min/max are computed under
/// the type's correct ordering and stored as raw 8-byte little-endian.
fn build_granule_index(logical_type: u8, granule_size: u32, data: &[u8]) -> Option<GranuleIndex> {
    if granule_size == 0 {
        return None;
    }
    let kind = NumKind::from_logical(logical_type)?;
    if data.is_empty() || !data.len().is_multiple_of(8) {
        return None;
    }
    let n = data.len() / 8;
    let g = granule_size as usize;
    let mut granules = Vec::with_capacity(n.div_ceil(g));
    let mut start = 0usize;
    while start < n {
        let end = (start + g).min(n);
        let granule = &data[start * 8..end * 8];
        let (min, max) = granule_min_max(kind, granule);
        granules.push(GranuleStats {
            min: min.to_vec(),
            max: max.to_vec(),
        });
        start = end;
    }
    Some(GranuleIndex {
        granule_size,
        value_width: 8,
        granules,
    })
}

/// min/max over one granule's worth of 8-byte numeric values, returned as
/// the raw little-endian bytes of the extreme elements (so they re-encode
/// identically to the column). `slice.len()` is a non-zero multiple of 8.
fn granule_min_max(kind: NumKind, slice: &[u8]) -> ([u8; 8], [u8; 8]) {
    let mut elems = slice.chunks_exact(8);
    let first: [u8; 8] = elems.next().unwrap().try_into().unwrap();
    let mut min = first;
    let mut max = first;
    for e in elems {
        let cur: [u8; 8] = e.try_into().unwrap();
        if num_lt(kind, &cur, &min) {
            min = cur;
        }
        if num_lt(kind, &max, &cur) {
            max = cur;
        }
    }
    (min, max)
}

/// `a < b` for two 8-byte little-endian values under `kind`'s ordering.
/// Floats use [`f64::total_cmp`] so NaN/±0 are ordered deterministically.
fn num_lt(kind: NumKind, a: &[u8; 8], b: &[u8; 8]) -> bool {
    match kind {
        NumKind::I64 => i64::from_le_bytes(*a) < i64::from_le_bytes(*b),
        NumKind::U64 => u64::from_le_bytes(*a) < u64::from_le_bytes(*b),
        NumKind::F64 => f64::from_le_bytes(*a)
            .total_cmp(&f64::from_le_bytes(*b))
            .is_lt(),
    }
}

/// Serialize `columns` into the v1 `RDCC` layout. Each column's raw bytes
/// run through the codec chain [`select_codecs`] picks from its
/// [`ColumnSemantics`]; the directory records the *leading* (semantic)
/// codec tag. The reader decodes from each stream's own self-describing
/// header, so #853 changed only write-time selection, never the read path.
pub fn write_column_block(
    chunk_id: u64,
    schema_ref: u64,
    row_count: u64,
    min_ts_ns: u64,
    max_ts_ns: u64,
    granule_size: u32,
    columns: &[ColumnInput<'_>],
) -> Result<Vec<u8>, ColumnBlockError> {
    let column_count = columns.len();
    let dir_off = HEADER_LEN;
    let dir_len = column_count * DIR_ENTRY_LEN;
    let streams_off = dir_off + dir_len;

    // Encode every column stream first so we know each length/offset.
    // The codec chain is chosen per column from its semantics; the tag we
    // record in the directory is the *leading* (semantic) codec — the one
    // that characterises the column. An empty chain (never produced by
    // `select_codecs`) records `None`. Alongside each stream we build the
    // column's sparse granule index (#854) from the *raw* (pre-codec)
    // bytes, so min/max reflect real values; variable-width columns get
    // `None` and a zero-length directory slice.
    let mut streams: Vec<Vec<u8>> = Vec::with_capacity(column_count);
    let mut codec_tags: Vec<u8> = Vec::with_capacity(column_count);
    let mut granule_blobs: Vec<Vec<u8>> = Vec::with_capacity(column_count);
    let mut bloom_blobs: Vec<Vec<u8>> = Vec::with_capacity(column_count);
    for col in columns {
        let codecs = select_codecs(col.logical_type, col.semantics);
        codec_tags.push(codecs.first().unwrap_or(&ColumnCodec::None).tag());
        streams.push(encode_bytes(&codecs, col.data)?);
        granule_blobs.push(
            build_granule_index(col.logical_type, granule_size, col.data)
                .map(|gi| gi.serialize())
                .unwrap_or_default(),
        );
        // Per-granule bloom skip index (#855), on the same granule
        // boundaries as the min/max index; equality predicates probe it.
        bloom_blobs.push(
            build_granule_bloom(col.logical_type, granule_size, col.data)
                .map(|gb| gb.serialize())
                .unwrap_or_default(),
        );
    }

    let parts: Vec<reddb_file::ColumnBlockPart<'_>> = columns
        .iter()
        .zip(streams.iter())
        .zip(codec_tags.iter())
        .zip(granule_blobs.iter())
        .zip(bloom_blobs.iter())
        .map(
            |((((col, stream), codec_tag), granule), bloom)| reddb_file::ColumnBlockPart {
                column_id: col.column_id,
                logical_type: col.logical_type,
                codec_tag: *codec_tag,
                stream,
                granule_index: granule,
                granule_bloom: bloom,
            },
        )
        .collect();

    Ok(reddb_file::encode_column_block_frame(
        chunk_id, schema_ref, row_count, min_ts_ns, max_ts_ns, &parts,
    ))
}

/// Peek the RDCC **format version** from a column-block buffer without
/// decoding it. Returns `Some(version)` when `bytes` opens with the `RDCC`
/// magic, or `None` when it does not (too short, or the leading magic is
/// not `RDCC` — i.e. the buffer is not the columnar form at all).
///
/// This is the read-bridge's format detector (#861): a chunk's stored
/// bytes are classified — and a columnar chunk's version gated — before
/// dispatching to the columnar reader, so an unknown future layout is
/// rejected rather than misread. It touches only the 6-byte magic+version
/// prefix; no CRC, directory, or stream work.
pub fn peek_column_block_version(bytes: &[u8]) -> Option<u16> {
    reddb_file::peek_column_block_version(bytes)
}

/// Decode a v1 `RDCC` block: verify both magics, the version, and the
/// CRC, then decode each column stream back to its raw bytes.
pub fn read_column_block(bytes: &[u8]) -> Result<DecodedColumnBlock, ColumnBlockError> {
    read_column_block_filtered(bytes, None)
}

/// Projection-pushdown decode (#856): like [`read_column_block`] but only
/// the columns whose `column_id` appears in `want` have their stream run
/// through `decode_bytes` (and their granule index / bloom parsed). Columns
/// outside `want` are skipped entirely — their compressed bytes are never
/// touched — so an analytical scan that references a subset of columns pays
/// only for the columns it reads. The returned
/// [`DecodedColumnBlock::columns`] holds exactly the wanted columns, in
/// directory order. The whole-block CRC is still verified (integrity is not
/// negotiable), but per-column decode work is elided for unwanted columns.
pub fn read_column_block_projected(
    bytes: &[u8],
    want: &[u32],
) -> Result<DecodedColumnBlock, ColumnBlockError> {
    read_column_block_filtered(bytes, Some(want))
}

/// Shared decode core. `want == None` decodes every column (the eager
/// [`read_column_block`] contract); `want == Some(ids)` decodes only the
/// columns whose id is in `ids` (the projected #856 contract).
fn read_column_block_filtered(
    bytes: &[u8],
    want: Option<&[u32]>,
) -> Result<DecodedColumnBlock, ColumnBlockError> {
    let frame = reddb_file::decode_column_block_frame(bytes)?;

    let mut columns = Vec::with_capacity(frame.columns.len());
    for col in frame.columns {
        // Projection pushdown (#856): skip columns the caller did not ask
        // for *before* the expensive `decode_bytes` / granule / bloom parse.
        // Offset bounds are still validated above so a malformed directory is
        // rejected whether or not the column is wanted.
        if want.is_some_and(|ids| !ids.contains(&col.column_id)) {
            continue;
        }
        // Decode by the recorded stream (its own segment_codec header
        // carries the codec); the directory tag is bookkeeping for #853.
        let data = decode_bytes(col.stream)?;
        // Parse the sparse granule index (#854) when present. A
        // zero-length slice means the column was written without an index.
        let granule_index = col
            .granule_index
            .map(GranuleIndex::deserialize)
            .transpose()?;
        // Parse the per-granule bloom skip index (#855) when present. A
        // zero-length slice means the column was written without a bloom.
        let granule_bloom = col
            .granule_bloom
            .map(GranuleBloom::deserialize)
            .transpose()?;
        columns.push(DecodedColumn {
            column_id: col.column_id,
            logical_type: col.logical_type,
            codec_tag: col.codec_tag,
            data,
            granule_index,
            granule_bloom,
        });
    }

    Ok(DecodedColumnBlock {
        chunk_id: frame.chunk_id,
        schema_ref: frame.schema_ref,
        row_count: frame.row_count,
        min_ts_ns: frame.min_ts_ns,
        max_ts_ns: frame.max_ts_ns,
        columns,
    })
}

/// One projected column decoded for the vectorised batch reader (#962):
/// either an 8-byte-aligned `Vec<u64>` produced directly by the codec (numeric
/// inner codec — no `Vec<u8>` → typed-`Vec` copy) or the raw little-endian
/// bytes (`decode_bytes` fallback for non-numeric inner codecs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectedColumnData {
    /// Fixed-width 8-byte values, ready to reinterpret as `i64`/`f64`.
    Words(Vec<u64>),
    /// Raw little-endian column bytes (codec did not emit u64 words).
    Bytes(Vec<u8>),
}

/// A projected column for the batch reader: its id, stored logical-type tag,
/// and decoded payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedColumn {
    pub column_id: u32,
    pub logical_type: u8,
    pub data: ProjectedColumnData,
}

/// Projection-pushdown decode tuned for the vectorised batch reader (#962):
/// like [`read_column_block_projected`] but, for columns whose innermost codec
/// is numeric, decodes straight into an aligned `Vec<u64>` so the batch layer
/// reinterprets the words as `i64`/`f64` without a second copy. Columns whose
/// inner codec is not numeric fall back to the raw-bytes decode. Granule and
/// bloom indices are not parsed — a full materialising scan does not consult
/// them — but the whole-block CRC is still verified by the frame decoder.
///
/// Returns exactly the wanted columns in directory order. Unwanted columns'
/// streams are never touched.
pub fn read_column_block_projected_typed(
    bytes: &[u8],
    want: &[u32],
) -> Result<Vec<ProjectedColumn>, ColumnBlockError> {
    let frame = reddb_file::decode_column_block_frame(bytes)?;
    let mut columns = Vec::with_capacity(want.len().min(frame.columns.len()));
    for col in frame.columns {
        if !want.contains(&col.column_id) {
            continue;
        }
        // Prefer the typed fast path; fall back to the byte decode when the
        // stream's inner codec cannot emit u64 words (Dict / Generic LZ4-only).
        let data = match decode_bytes_to_u64(col.stream)? {
            Some(words) => ProjectedColumnData::Words(words),
            None => ProjectedColumnData::Bytes(decode_bytes(col.stream)?),
        };
        columns.push(ProjectedColumn {
            column_id: col.column_id,
            logical_type: col.logical_type,
            data,
        });
    }
    Ok(columns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn u64_stream(values: &[u64]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn f64_stream(values: &[f64]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn round_trips_two_columns_value_for_value() {
        let ts: Vec<u64> = (0..500)
            .map(|i| 1_700_000_000_000 + i * 1_000_000)
            .collect();
        let vals: Vec<f64> = (0..500).map(|i| 95.0 + (i % 7) as f64 * 0.25).collect();
        let ts_raw = u64_stream(&ts);
        let val_raw = f64_stream(&vals);

        let block = write_column_block(
            42,
            7,
            ts.len() as u64,
            *ts.first().unwrap(),
            *ts.last().unwrap(),
            128,
            &[
                ColumnInput {
                    column_id: 0,
                    logical_type: 2,
                    semantics: ColumnSemantics::Timestamp,
                    data: &ts_raw,
                },
                ColumnInput {
                    column_id: 1,
                    logical_type: 3,
                    semantics: ColumnSemantics::Gauge,
                    data: &val_raw,
                },
            ],
        )
        .unwrap();

        let decoded = read_column_block(&block).unwrap();
        assert_eq!(decoded.chunk_id, 42);
        assert_eq!(decoded.schema_ref, 7);
        assert_eq!(decoded.row_count, 500);
        assert_eq!(decoded.min_ts_ns, *ts.first().unwrap());
        assert_eq!(decoded.max_ts_ns, *ts.last().unwrap());
        assert_eq!(decoded.columns.len(), 2);
        assert_eq!(decoded.columns[0].column_id, 0);
        assert_eq!(decoded.columns[0].logical_type, 2);
        // Per-column selection: the directory records the *leading*
        // semantic codec — DoubleDelta for the timestamp column, XOR for
        // the float gauge — not a single uniform ZSTD tag.
        assert_eq!(decoded.columns[0].codec_tag, ColumnCodec::DoubleDelta.tag());
        assert_eq!(decoded.columns[1].codec_tag, ColumnCodec::Xor.tag());
        // …yet both still decode byte-for-byte (criterion 1 + 2).
        assert_eq!(decoded.columns[0].data, ts_raw);
        assert_eq!(decoded.columns[1].data, val_raw);

        // Both numeric columns carry a sparse granule index (#854): 500
        // rows / 128 per granule → 4 marks (last short).
        for col in &decoded.columns {
            let gi = col
                .granule_index
                .as_ref()
                .expect("numeric column must carry a granule index");
            assert_eq!(gi.granule_size, 128);
            assert_eq!(gi.value_width, 8);
            assert_eq!(gi.granule_count(), 4);
        }
        // Timestamp granule mins are the per-granule first timestamps
        // (the column is monotonic), so granule 0's min is the global min.
        let ts_gi = decoded.columns[0].granule_index.as_ref().unwrap();
        assert_eq!(
            u64::from_le_bytes(ts_gi.granules[0].min.clone().try_into().unwrap()),
            *ts.first().unwrap()
        );
        assert_eq!(
            u64::from_le_bytes(ts_gi.granules[3].max.clone().try_into().unwrap()),
            *ts.last().unwrap()
        );
    }

    #[test]
    fn projected_read_decodes_only_wanted_columns() {
        let ts: Vec<u64> = (0..500)
            .map(|i| 1_700_000_000_000 + i * 1_000_000)
            .collect();
        let vals: Vec<f64> = (0..500).map(|i| 95.0 + (i % 7) as f64 * 0.25).collect();
        let ts_raw = u64_stream(&ts);
        let val_raw = f64_stream(&vals);

        let block = write_column_block(
            42,
            7,
            ts.len() as u64,
            *ts.first().unwrap(),
            *ts.last().unwrap(),
            128,
            &[
                ColumnInput {
                    column_id: 0,
                    logical_type: 2,
                    semantics: ColumnSemantics::Timestamp,
                    data: &ts_raw,
                },
                ColumnInput {
                    column_id: 1,
                    logical_type: 3,
                    semantics: ColumnSemantics::Gauge,
                    data: &val_raw,
                },
            ],
        )
        .unwrap();

        // Want only the value column (id 1): the block returns exactly that
        // one column, byte-for-byte, and the timestamp column is absent —
        // its stream was never run through decode_bytes (#856 pushdown).
        let projected = read_column_block_projected(&block, &[1]).unwrap();
        assert_eq!(projected.columns.len(), 1);
        assert_eq!(projected.columns[0].column_id, 1);
        assert_eq!(projected.columns[0].data, val_raw);
        // Header metadata is unaffected by projection.
        assert_eq!(projected.row_count, 500);

        // Projecting both ids is identical to the eager full read.
        let full = read_column_block(&block).unwrap();
        let both = read_column_block_projected(&block, &[0, 1]).unwrap();
        assert_eq!(both, full);

        // An unknown id yields no columns (but still verifies the CRC).
        let none = read_column_block_projected(&block, &[99]).unwrap();
        assert!(none.columns.is_empty());
    }

    fn str_stream(items: &[&str]) -> Vec<u8> {
        let mut out = (items.len() as u32).to_le_bytes().to_vec();
        for s in items {
            out.extend_from_slice(&(s.len() as u16).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        out
    }

    #[test]
    fn records_counter_and_low_cardinality_codecs_and_round_trips() {
        let counter = u64_stream(&(0..300).map(|i| (i * 5) as u64).collect::<Vec<_>>());
        let labels: Vec<&str> = (0..300).map(|i| ["a", "b", "c"][i % 3]).collect();
        let labels_raw = str_stream(&labels);

        let block = write_column_block(
            9,
            1,
            300,
            0,
            0,
            64,
            &[
                ColumnInput {
                    column_id: 10,
                    logical_type: 2,
                    semantics: ColumnSemantics::Counter,
                    data: &counter,
                },
                ColumnInput {
                    column_id: 11,
                    logical_type: 4,
                    semantics: ColumnSemantics::LowCardinality,
                    data: &labels_raw,
                },
            ],
        )
        .unwrap();

        let decoded = read_column_block(&block).unwrap();
        assert_eq!(decoded.columns[0].codec_tag, ColumnCodec::Delta.tag());
        assert_eq!(decoded.columns[1].codec_tag, ColumnCodec::Dict.tag());
        assert_eq!(decoded.columns[0].data, counter);
        assert_eq!(decoded.columns[1].data, labels_raw);
        // The numeric counter column is indexed; the variable-width
        // low-cardinality string column is not (zero-length slice → None).
        assert!(decoded.columns[0].granule_index.is_some());
        assert!(decoded.columns[1].granule_index.is_none());
    }

    #[test]
    fn header_carries_magic_and_version() {
        let block = write_column_block(1, 0, 0, 0, 0, 0, &[]).unwrap();
        assert_eq!(&block[0..4], &COLUMN_BLOCK_MAGIC);
        assert_eq!(
            u16::from_le_bytes([block[4], block[5]]),
            COLUMN_BLOCK_VERSION_V1
        );
        assert_eq!(&block[block.len() - 4..], &COLUMN_BLOCK_MAGIC);
        // Empty (zero-column) block still decodes.
        let decoded = read_column_block(&block).unwrap();
        assert!(decoded.columns.is_empty());
    }

    #[test]
    fn peek_version_classifies_rdcc_and_rejects_non_rdcc() {
        // A real block peeks as v1 without a full decode.
        let block = write_column_block(1, 0, 0, 0, 0, 0, &[]).unwrap();
        assert_eq!(
            peek_column_block_version(&block),
            Some(COLUMN_BLOCK_VERSION_V1)
        );
        // The peek reads the raw version word — even a future version the
        // full reader rejects is reported here, so the read-bridge can gate.
        let mut future = block.clone();
        future[4..6].copy_from_slice(&(COLUMN_BLOCK_VERSION_V1 + 7).to_le_bytes());
        assert_eq!(
            peek_column_block_version(&future),
            Some(COLUMN_BLOCK_VERSION_V1 + 7)
        );
        // Non-RDCC bytes (wrong magic) → not the columnar form.
        let mut wrong_magic = block.clone();
        wrong_magic[0] = b'X';
        assert_eq!(peek_column_block_version(&wrong_magic), None);
        // Row-form / arbitrary short bytes → not a column block.
        assert_eq!(peek_column_block_version(b"row-stored bytes"), None);
        assert_eq!(peek_column_block_version(&[]), None);
    }

    #[test]
    fn rejects_bad_leading_magic() {
        let mut block = write_column_block(1, 0, 0, 0, 0, 0, &[]).unwrap();
        block[0] = b'X';
        assert!(matches!(
            read_column_block(&block),
            Err(ColumnBlockError::BadMagic(_))
        ));
    }

    #[test]
    fn rejects_future_version() {
        let raw = u64_stream(&[1, 2, 3]);
        let mut block = write_column_block(
            1,
            0,
            3,
            1,
            3,
            0,
            &[ColumnInput {
                column_id: 0,
                logical_type: 2,
                semantics: ColumnSemantics::Generic,
                data: &raw,
            }],
        )
        .unwrap();
        block[4..6].copy_from_slice(&(COLUMN_BLOCK_VERSION_V1 + 1).to_le_bytes());
        assert!(matches!(
            read_column_block(&block),
            Err(ColumnBlockError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn detects_payload_corruption_via_crc() {
        let raw = f64_stream(&[1.5, 2.5, 3.5, 4.5]);
        let mut block = write_column_block(
            1,
            0,
            4,
            0,
            0,
            0,
            &[ColumnInput {
                column_id: 0,
                logical_type: 3,
                semantics: ColumnSemantics::Gauge,
                data: &raw,
            }],
        )
        .unwrap();
        // Flip a byte inside the stream region (after the header).
        block[HEADER_LEN + DIR_ENTRY_LEN] ^= 0xFF;
        assert!(matches!(
            read_column_block(&block),
            Err(ColumnBlockError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn granule_index_round_trips_and_prunes_on_u64() {
        // 250 u64 timestamps, 100 rows per granule → 3 marks (100/100/50).
        let ts: Vec<u64> = (0..250).map(|i| 1_000 + i * 10).collect();
        let raw = u64_stream(&ts);
        let block = write_column_block(
            1,
            0,
            ts.len() as u64,
            *ts.first().unwrap(),
            *ts.last().unwrap(),
            100,
            &[ColumnInput {
                column_id: 0,
                logical_type: 2,
                semantics: ColumnSemantics::Timestamp,
                data: &raw,
            }],
        )
        .unwrap();

        let decoded = read_column_block(&block).unwrap();
        let gi = decoded.columns[0].granule_index.as_ref().unwrap();
        assert_eq!(gi.granule_count(), 3);
        // Per-granule min/max for a monotonic column: [1000,1990],[2000,2990],[3000,3490].
        let as_u64 = |b: &[u8]| u64::from_le_bytes(b.try_into().unwrap());
        assert_eq!(as_u64(&gi.granules[0].min), 1_000);
        assert_eq!(as_u64(&gi.granules[0].max), 1_990);
        assert_eq!(as_u64(&gi.granules[2].min), 3_000);
        assert_eq!(as_u64(&gi.granules[2].max), 3_490);

        // A range hitting only the middle granule prunes the other two.
        let survivors = gi.surviving_granules(|min, max| {
            let (lo, hi) = (2_100u64, 2_200u64);
            as_u64(min) <= hi && as_u64(max) >= lo
        });
        assert_eq!(survivors, vec![1]);
        assert_eq!(gi.row_range(1, ts.len()), (100, 200));
    }

    #[test]
    fn granule_bloom_round_trips_and_prunes_on_equality() {
        // 250 u64 keys, 100 rows per granule → 3 granules (100/100/50). The
        // values are deliberately scattered so a target lands in exactly one.
        let keys: Vec<u64> = (0..250).map(|i| (i as u64) * 7 + 3).collect();
        let raw = u64_stream(&keys);
        let block = write_column_block(
            1,
            0,
            keys.len() as u64,
            *keys.first().unwrap(),
            *keys.last().unwrap(),
            100,
            &[ColumnInput {
                column_id: 0,
                logical_type: 2,
                semantics: ColumnSemantics::Timestamp,
                data: &raw,
            }],
        )
        .unwrap();

        let decoded = read_column_block(&block).unwrap();
        let gb = decoded.columns[0]
            .granule_bloom
            .as_ref()
            .expect("numeric column must carry a granule bloom");
        assert_eq!(gb.granule_count(), 3);
        assert_eq!(gb.granule_size, 100);

        // A value that lives in granule 1 (row 150 → key 150*7+3 = 1053) must
        // keep granule 1 (no false negative). Other granules may survive as
        // false positives, but the owning granule is never pruned.
        let target = keys[150];
        let survivors = gb.surviving_granules(&target.to_le_bytes());
        assert!(
            survivors.contains(&1),
            "granule holding the value was pruned: {survivors:?}"
        );
        assert_eq!(gb.row_range(1, keys.len()), (100, 200));

        // Every actually-present key probes true in its own granule — the
        // core no-false-negative guarantee across the serialized boundary.
        for (row, &k) in keys.iter().enumerate() {
            let g = row / 100;
            let survivors = gb.surviving_granules(&k.to_le_bytes());
            assert!(
                survivors.contains(&g),
                "key {k} at row {row} pruned from its granule {g}: {survivors:?}"
            );
        }
    }

    #[test]
    fn variable_width_column_gets_no_granule_bloom() {
        let labels: Vec<&str> = (0..120).map(|i| ["a", "b", "c"][i % 3]).collect();
        let labels_raw = str_stream(&labels);
        let block = write_column_block(
            9,
            1,
            120,
            0,
            0,
            64,
            &[ColumnInput {
                column_id: 11,
                logical_type: 4,
                semantics: ColumnSemantics::LowCardinality,
                data: &labels_raw,
            }],
        )
        .unwrap();
        let decoded = read_column_block(&block).unwrap();
        // Variable-width string column: no min/max index and no bloom.
        assert!(decoded.columns[0].granule_index.is_none());
        assert!(decoded.columns[0].granule_bloom.is_none());
    }

    proptest! {
        /// Soundness (criterion 2): a granule bloom may over-include but
        /// NEVER under-includes. For any column of u64 values and any probe
        /// key, every granule that actually *contains* that key survives the
        /// equality prune. Tested through the full write→read serialization
        /// path so the persisted bloom — not just an in-RAM one — is proven.
        #[test]
        fn bloom_never_prunes_a_granule_holding_the_key(
            values in proptest::collection::vec(0u64..5_000, 1..400usize),
            granule_size in 1u32..128,
            probe in 0u64..5_000,
        ) {
            let raw = u64_stream(&values);
            let block = write_column_block(
                1, 0, values.len() as u64, 0, 0, granule_size,
                &[ColumnInput {
                    column_id: 0,
                    logical_type: 2,
                    semantics: ColumnSemantics::Generic,
                    data: &raw,
                }],
            ).unwrap();
            let decoded = read_column_block(&block).unwrap();
            let gb = decoded.columns[0].granule_bloom.as_ref().unwrap();
            let survivors = gb.surviving_granules(&probe.to_le_bytes());
            let g = granule_size as usize;
            // Every granule that holds `probe` among its rows must survive.
            for (row, &v) in values.iter().enumerate() {
                if v == probe {
                    let gi = row / g;
                    prop_assert!(
                        survivors.contains(&gi),
                        "granule {gi} holds {probe} at row {row} but was pruned"
                    );
                }
            }
        }
    }

    #[test]
    fn rejects_truncated_buffer() {
        assert!(matches!(
            read_column_block(&[0u8; 8]),
            Err(ColumnBlockError::Truncated)
        ));
    }
}
