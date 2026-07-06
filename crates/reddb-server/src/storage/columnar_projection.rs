//! Columnar analytics projection — first end-to-end tracer (ADR 0069, #1766).
//!
//! This module is the initial vertical slice of the *columnar analytics
//! projection*: a **derived, disposable** columnar representation of one
//! append-only collection with a minimal scalar type set. It composes the
//! authorities that already exist rather than inventing new persistence:
//!
//! * columnar segments are the native `RDCC` column-block
//!   ([`write_column_block`]/[`read_column_block`]), so every segment carries
//!   the standard CRC-32 checksum baked into that frame;
//! * each segment is sealed under the crypto **page envelope**
//!   ([`reddb_crypto::encrypt_page`]/[`reddb_crypto::decrypt_page`], ADR 0054);
//! * the [`ProjectionManifest`] records every segment as *derived* — so
//!   backup/restore skip or rebuild them and recovery never depends on them.
//!
//! The contract this slice fixes (ADR 0069):
//!
//! 1. **Derived on the checkpoint path.** [`ColumnarProjection::emit_at_checkpoint`]
//!    is the *only* producer of columnar segments. The write path
//!    ([`AppendOnlyCollection::append`]) never dual-writes columnar.
//! 2. **Transcoding budget.** A checkpoint that cannot afford to transcode the
//!    whole un-materialized tail transcodes a prefix within budget and *always
//!    completes*; the deferred rows are picked up by the next checkpoint.
//! 3. **One consistency class — the LSN-pinned analytical scan.**
//!    [`ColumnarProjection::analytical_scan`] reads columnar segments up to the
//!    last materialized LSN and concatenates the un-materialized tail through
//!    the normal row read path ([`AppendOnlyCollection::row_scan`]), all under a
//!    single pinned LSN. A row committed after the last checkpoint is therefore
//!    immediately visible; `AS OF` composes by pinning a historical LSN.
//!
//! Scope is deliberately narrow: one append-only collection, scalar columns
//! (`Integer`/`Timestamp`/`Float`/`Boolean`/`Text`), in-process segment store.
//! Wiring the emitter into the live `SegmentManager` checkpoint and the query
//! executor's scan is the follow-up slice; the equivalence/freshness/budget
//! oracles here prove the loop is correct end to end first.

use crate::storage::schema::types::{DataType, Value};
use crate::storage::unified::{
    read_column_block, write_column_block, ColumnBlockError, ColumnInput, ColumnSemantics,
};

/// Rows per sparse granule mark inside a sealed segment. Mirrors the default
/// the timeseries seal path uses; the exact value only affects skip-index
/// granularity, never correctness of the round-trip.
const GRANULE_SIZE: u32 = 128;

/// A single append-only row: one scalar [`Value`] per projected column, in
/// schema order. This is the source-of-truth shape the row read path yields
/// and the columnar scan must reproduce bit-for-bit.
pub type Row = Vec<Value>;

/// One projected column's identity and declared scalar type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionColumn {
    /// Stable column id — the key the `RDCC` directory addresses columns by.
    pub column_id: u32,
    /// Declared scalar type. Only the minimal set is accepted at emit time.
    pub data_type: DataType,
}

/// The scalar schema of the projected append-only collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSchema {
    pub columns: Vec<ProjectionColumn>,
}

impl ProjectionSchema {
    pub fn new(columns: Vec<ProjectionColumn>) -> Self {
        Self { columns }
    }

    fn width(&self) -> usize {
        self.columns.len()
    }
}

/// Errors from projection emit/scan. Every variant fails closed — a corrupt
/// or unsupported input is rejected, never silently mis-decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionError {
    /// A value's runtime type did not match the column's declared type.
    TypeMismatch {
        column: usize,
        expected: DataType,
        found: &'static str,
    },
    /// The declared column type is outside this slice's minimal scalar set.
    UnsupportedType(DataType),
    /// A row's arity did not match the schema width.
    RowWidth { expected: usize, found: usize },
    /// LSNs must be strictly increasing on the append-only log.
    NonMonotonicLsn { last: u64, next: u64 },
    /// The sealed `RDCC` frame failed to decode (bad magic, CRC, directory…).
    Block(ColumnBlockError),
    /// The crypto page envelope failed to open (wrong key, tampering, clip).
    Envelope(String),
    /// A decoded column stream was ragged for its fixed-width type, or a
    /// column expected by the schema was absent from the segment.
    CorruptSegment(&'static str),
}

impl std::fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TypeMismatch {
                column,
                expected,
                found,
            } => write!(
                f,
                "projection column {column}: expected {expected}, found {found}"
            ),
            Self::UnsupportedType(dt) => write!(f, "projection: unsupported column type {dt}"),
            Self::RowWidth { expected, found } => {
                write!(
                    f,
                    "projection row width mismatch: expected {expected}, got {found}"
                )
            }
            Self::NonMonotonicLsn { last, next } => {
                write!(f, "projection: non-monotonic lsn {next} after {last}")
            }
            Self::Block(e) => write!(f, "projection segment decode failed: {e}"),
            Self::Envelope(detail) => write!(f, "projection segment envelope failed: {detail}"),
            Self::CorruptSegment(detail) => write!(f, "projection segment corrupt: {detail}"),
        }
    }
}

impl std::error::Error for ProjectionError {}

impl From<ColumnBlockError> for ProjectionError {
    fn from(e: ColumnBlockError) -> Self {
        Self::Block(e)
    }
}

/// One append-only row stamped with its commit LSN.
#[derive(Debug, Clone, PartialEq)]
struct LsnRow {
    lsn: u64,
    row: Row,
}

/// The append-only collection: the **sole source of truth** for this slice.
/// Rows are appended in strictly increasing LSN order; the write path only
/// ever grows this log and never touches columnar state (ADR 0069 §2).
#[derive(Debug, Clone)]
pub struct AppendOnlyCollection {
    schema: ProjectionSchema,
    rows: Vec<LsnRow>,
    last_lsn: u64,
}

impl AppendOnlyCollection {
    pub fn new(schema: ProjectionSchema) -> Self {
        Self {
            schema,
            rows: Vec::new(),
            last_lsn: 0,
        }
    }

    pub fn schema(&self) -> &ProjectionSchema {
        &self.schema
    }

    /// The highest LSN committed so far (0 when empty). A caller pins this to
    /// scan "as of now".
    pub fn latest_lsn(&self) -> u64 {
        self.last_lsn
    }

    /// Append one row at `lsn`. This is the transactional write path: it grows
    /// the row log only — never the columnar projection.
    pub fn append(&mut self, lsn: u64, row: Row) -> Result<(), ProjectionError> {
        if row.len() != self.schema.width() {
            return Err(ProjectionError::RowWidth {
                expected: self.schema.width(),
                found: row.len(),
            });
        }
        if lsn <= self.last_lsn {
            return Err(ProjectionError::NonMonotonicLsn {
                last: self.last_lsn,
                next: lsn,
            });
        }
        for (idx, value) in row.iter().enumerate() {
            check_value_type(idx, &self.schema.columns[idx].data_type, value)?;
        }
        self.last_lsn = lsn;
        self.rows.push(LsnRow { lsn, row });
        Ok(())
    }

    /// The row read path — the equivalence oracle. Returns every row visible
    /// at `pinned_lsn` (lsn ≤ pinned), in commit order. This is what an
    /// ordinary MVCC snapshot scan would yield.
    pub fn row_scan(&self, pinned_lsn: u64) -> Vec<Row> {
        self.rows
            .iter()
            .filter(|r| r.lsn <= pinned_lsn)
            .map(|r| r.row.clone())
            .collect()
    }

    /// Rows in the half-open LSN window `(after, up_to]`, in commit order.
    fn rows_between(&self, after: u64, up_to: u64) -> impl Iterator<Item = &LsnRow> {
        self.rows
            .iter()
            .filter(move |r| r.lsn > after && r.lsn <= up_to)
    }
}

/// A manifest record for one emitted columnar segment. Always `derived`:
/// backup/restore skip these and recovery rebuilds rather than restores them
/// (ADR 0069 §1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSegmentEntry {
    pub segment_id: u64,
    /// Inclusive LSN range this segment materializes.
    pub first_lsn: u64,
    pub last_lsn: u64,
    pub row_count: u64,
    /// CRC-32 over the sealed (post-envelope) bytes — the "standard checksum"
    /// that guards the segment file before it is ever decrypted.
    pub sealed_crc32: u32,
    /// Always true: a projection segment is never source of truth.
    pub derived: bool,
}

/// The projection's operational manifest: the ordered set of derived segment
/// entries plus the materialization watermark (the last LSN a segment covers).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectionManifest {
    segments: Vec<ProjectionSegmentEntry>,
    last_materialized_lsn: u64,
}

impl ProjectionManifest {
    /// The materialization watermark: columnar segments cover every LSN up to
    /// and including this value; everything past it is the un-materialized
    /// tail served by the row path.
    pub fn last_materialized_lsn(&self) -> u64 {
        self.last_materialized_lsn
    }

    pub fn segments(&self) -> &[ProjectionSegmentEntry] {
        &self.segments
    }

    /// Every projection entry is derived — this is an invariant, asserted so a
    /// future edit that forgets to mark an entry derived fails a test.
    pub fn all_derived(&self) -> bool {
        self.segments.iter().all(|s| s.derived)
    }

    /// What backup/restore must persist: nothing. Projection segments are
    /// rebuildable, so a backup that skips them loses no truth (ADR 0069 §1).
    pub fn durable_entries(&self) -> impl Iterator<Item = &ProjectionSegmentEntry> {
        self.segments.iter().filter(|s| !s.derived)
    }
}

/// Knobs for a checkpoint emission.
#[derive(Debug, Clone, Copy)]
pub struct TranscodeBudget {
    /// Maximum rows this checkpoint may transcode into columnar segments.
    /// A checkpoint always completes: rows beyond the budget are deferred.
    pub max_rows: u64,
    /// Rows per emitted segment (the last segment may be shorter).
    pub segment_rows: u64,
    /// Size floor: if the un-materialized tail up to the checkpoint boundary is
    /// smaller than this, materialization is skipped this round — the tail
    /// stays visible through the row path, so correctness is unaffected while
    /// bookkeeping is saved (ADR 0069 §6).
    pub size_floor_rows: u64,
}

impl Default for TranscodeBudget {
    fn default() -> Self {
        Self {
            max_rows: u64::MAX,
            segment_rows: 1024,
            size_floor_rows: 1,
        }
    }
}

/// Outcome of one [`ColumnarProjection::emit_at_checkpoint`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmitOutcome {
    pub segments_emitted: usize,
    pub rows_materialized: u64,
    /// Rows within the checkpoint boundary the budget could not afford; the
    /// next checkpoint picks them up.
    pub rows_deferred: u64,
    /// True when the budget stopped emission before draining the tail.
    pub budget_exhausted: bool,
    /// True when the size floor skipped materialization this round.
    pub floor_skipped: bool,
}

/// A sealed segment kept in the projection's in-process store: its manifest
/// entry plus the sealed (enveloped) bytes.
#[derive(Debug, Clone)]
struct StoredSegment {
    entry: ProjectionSegmentEntry,
    sealed: Vec<u8>,
}

/// The columnar analytics projection for one append-only collection.
#[derive(Debug, Clone)]
pub struct ColumnarProjection {
    schema: ProjectionSchema,
    key: [u8; 32],
    segments: Vec<StoredSegment>,
    manifest: ProjectionManifest,
    next_segment_id: u64,
}

impl ColumnarProjection {
    /// Create a projection over `schema`, sealing segments under `key`.
    pub fn new(schema: ProjectionSchema, key: [u8; 32]) -> Self {
        Self {
            schema,
            key,
            segments: Vec::new(),
            manifest: ProjectionManifest::default(),
            next_segment_id: 1,
        }
    }

    pub fn manifest(&self) -> &ProjectionManifest {
        &self.manifest
    }

    /// Drop every materialized segment. Repair is always "regenerate": after a
    /// drop, the next `emit_at_checkpoint` rebuilds the projection from the row
    /// log, and reads keep working meanwhile (served entirely by the row tail).
    pub fn drop_projection(&mut self) {
        self.segments.clear();
        self.manifest = ProjectionManifest::default();
    }

    /// Emit columnar segments for the un-materialized tail up to `checkpoint_lsn`,
    /// bounded by `budget`. Never fails for lack of budget: it transcodes a
    /// prefix and defers the rest. This is the *only* producer of columnar
    /// segments (ADR 0069 §2).
    pub fn emit_at_checkpoint(
        &mut self,
        collection: &AppendOnlyCollection,
        checkpoint_lsn: u64,
        budget: TranscodeBudget,
    ) -> Result<EmitOutcome, ProjectionError> {
        let watermark = self.manifest.last_materialized_lsn;
        let pending: Vec<&LsnRow> = collection.rows_between(watermark, checkpoint_lsn).collect();
        let available = pending.len() as u64;

        if available < budget.size_floor_rows.max(1) {
            return Ok(EmitOutcome {
                segments_emitted: 0,
                rows_materialized: 0,
                rows_deferred: available,
                budget_exhausted: false,
                floor_skipped: available > 0,
            });
        }

        let to_materialize = available.min(budget.max_rows) as usize;
        let segment_rows = budget.segment_rows.max(1) as usize;
        let mut segments_emitted = 0usize;

        for chunk in pending[..to_materialize].chunks(segment_rows) {
            let entry = self.seal_segment(chunk)?;
            self.manifest.last_materialized_lsn = entry.last_lsn;
            self.manifest.segments.push(entry);
            segments_emitted += 1;
        }

        let rows_deferred = available - to_materialize as u64;
        Ok(EmitOutcome {
            segments_emitted,
            rows_materialized: to_materialize as u64,
            rows_deferred,
            budget_exhausted: rows_deferred > 0,
            floor_skipped: false,
        })
    }

    /// Transcode one contiguous run of rows into a sealed columnar segment and
    /// record its derived manifest entry.
    fn seal_segment(
        &mut self,
        rows: &[&LsnRow],
    ) -> Result<ProjectionSegmentEntry, ProjectionError> {
        let segment_id = self.next_segment_id;
        self.next_segment_id += 1;
        let first_lsn = rows.first().map(|r| r.lsn).unwrap_or(0);
        let last_lsn = rows.last().map(|r| r.lsn).unwrap_or(0);

        // Transpose rows → per-column raw little-endian byte streams.
        let mut column_bytes: Vec<Vec<u8>> = vec![Vec::new(); self.schema.columns.len()];
        for lsn_row in rows {
            for (idx, value) in lsn_row.row.iter().enumerate() {
                let dt = self.schema.columns[idx].data_type;
                check_value_type(idx, &dt, value)?;
                encode_cell(dt, value, &mut column_bytes[idx])?;
            }
        }

        let inputs: Vec<ColumnInput<'_>> = self
            .schema
            .columns
            .iter()
            .zip(column_bytes.iter())
            .map(|(col, data)| ColumnInput {
                column_id: col.column_id,
                logical_type: col.data_type.to_byte(),
                semantics: ColumnSemantics::Generic,
                data,
            })
            .collect();

        // The RDCC frame carries its own CRC-32 (checksum coverage).
        let frame = write_column_block(
            segment_id,
            self.schema.columns.len() as u64,
            rows.len() as u64,
            first_lsn,
            last_lsn,
            GRANULE_SIZE,
            &inputs,
        )?;

        // Seal under the crypto page envelope (ADR 0054); bind the segment id
        // as the page-id AAD so a swapped segment fails the tag check.
        let sealed = reddb_crypto::encrypt_page(&self.key, segment_id as u32, &frame)
            .map_err(|e| ProjectionError::Envelope(e.to_string()))?;
        let sealed_crc32 = crc32fast::hash(&sealed);

        let entry = ProjectionSegmentEntry {
            segment_id,
            first_lsn,
            last_lsn,
            row_count: rows.len() as u64,
            sealed_crc32,
            derived: true,
        };
        self.segments.push(StoredSegment {
            entry: entry.clone(),
            sealed,
        });
        Ok(entry)
    }

    /// The LSN-pinned analytical scan (ADR 0069 §4). Under a single pinned LSN:
    /// decode columnar segments up to the last materialized LSN, then
    /// concatenate the un-materialized tail through the row read path. The
    /// result is identical, row-for-row and in order, to
    /// [`AppendOnlyCollection::row_scan`] at the same `pinned_lsn` — one
    /// consistency class, always fresh; `AS OF` is just a historical pin.
    pub fn analytical_scan(
        &self,
        collection: &AppendOnlyCollection,
        pinned_lsn: u64,
    ) -> Result<Vec<Row>, ProjectionError> {
        let columnar_ceiling = self.manifest.last_materialized_lsn.min(pinned_lsn);

        let mut out = Vec::new();
        let mut tail_start = 0u64;
        for stored in &self.segments {
            if stored.entry.last_lsn > columnar_ceiling {
                continue;
            }
            self.verify_and_decode_segment(stored, &mut out)?;
            tail_start = tail_start.max(stored.entry.last_lsn);
        }

        // Un-materialized tail: everything past the last included segment up to
        // the pin, straight from the row path. Fresh rows land here.
        for lsn_row in collection.rows_between(tail_start, pinned_lsn) {
            out.push(lsn_row.row.clone());
        }
        Ok(out)
    }

    /// Verify a sealed segment (envelope + `RDCC` CRC), decode it, and push its
    /// rows onto `out` in stored order.
    fn verify_and_decode_segment(
        &self,
        stored: &StoredSegment,
        out: &mut Vec<Row>,
    ) -> Result<(), ProjectionError> {
        if crc32fast::hash(&stored.sealed) != stored.entry.sealed_crc32 {
            return Err(ProjectionError::CorruptSegment("sealed checksum mismatch"));
        }
        let frame =
            reddb_crypto::decrypt_page(&self.key, stored.entry.segment_id as u32, &stored.sealed)
                .map_err(|e| ProjectionError::Envelope(e.to_string()))?;
        // `read_column_block` verifies the RDCC CRC before handing back streams.
        let block = read_column_block(&frame)?;
        let row_count = block.row_count as usize;

        // Decode each schema column, matched by stable column id.
        let mut decoded_columns: Vec<Vec<Value>> = Vec::with_capacity(self.schema.columns.len());
        for col in &self.schema.columns {
            let decoded = block
                .columns
                .iter()
                .find(|c| c.column_id == col.column_id)
                .ok_or(ProjectionError::CorruptSegment("missing column"))?;
            decoded_columns.push(decode_column(col.data_type, &decoded.data, row_count)?);
        }

        for r in 0..row_count {
            let mut row = Vec::with_capacity(self.schema.columns.len());
            for column in &decoded_columns {
                row.push(column[r].clone());
            }
            out.push(row);
        }
        Ok(())
    }
}

/// The minimal scalar set this slice materializes. Anything else is rejected
/// at emit time rather than silently mis-encoded.
fn check_value_type(column: usize, dt: &DataType, value: &Value) -> Result<(), ProjectionError> {
    let ok = matches!(
        (dt, value),
        (DataType::Integer, Value::Integer(_))
            | (DataType::Timestamp, Value::Timestamp(_))
            | (DataType::Float, Value::Float(_))
            | (DataType::Boolean, Value::Boolean(_))
            | (DataType::Text, Value::Text(_))
    );
    if ok {
        return Ok(());
    }
    if !is_supported_type(dt) {
        return Err(ProjectionError::UnsupportedType(*dt));
    }
    Err(ProjectionError::TypeMismatch {
        column,
        expected: *dt,
        found: value_kind(value),
    })
}

fn is_supported_type(dt: &DataType) -> bool {
    matches!(
        dt,
        DataType::Integer
            | DataType::Timestamp
            | DataType::Float
            | DataType::Boolean
            | DataType::Text
    )
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Integer(_) => "Integer",
        Value::Timestamp(_) => "Timestamp",
        Value::Float(_) => "Float",
        Value::Boolean(_) => "Boolean",
        Value::Text(_) => "Text",
        _ => "unsupported",
    }
}

/// Append one scalar value's raw little-endian bytes to its column stream.
/// Fixed-width types write their native width; `Text` is length-prefixed
/// (`u32` LE length ‖ utf-8) so a variable-width column round-trips exactly.
fn encode_cell(dt: DataType, value: &Value, out: &mut Vec<u8>) -> Result<(), ProjectionError> {
    match (dt, value) {
        (DataType::Integer, Value::Integer(v)) => out.extend_from_slice(&v.to_le_bytes()),
        (DataType::Timestamp, Value::Timestamp(v)) => out.extend_from_slice(&v.to_le_bytes()),
        (DataType::Float, Value::Float(v)) => out.extend_from_slice(&v.to_le_bytes()),
        (DataType::Boolean, Value::Boolean(v)) => out.push(u8::from(*v)),
        (DataType::Text, Value::Text(s)) => {
            let bytes = s.as_bytes();
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
        }
        _ => return Err(ProjectionError::UnsupportedType(dt)),
    }
    Ok(())
}

/// Decode a column's raw stream back into `row_count` scalar values, the exact
/// inverse of [`encode_cell`].
fn decode_column(
    dt: DataType,
    data: &[u8],
    row_count: usize,
) -> Result<Vec<Value>, ProjectionError> {
    match dt {
        DataType::Integer => decode_i64(data, row_count, Value::Integer),
        DataType::Timestamp => decode_i64(data, row_count, Value::Timestamp),
        DataType::Float => {
            if data.len() != row_count * 8 {
                return Err(ProjectionError::CorruptSegment("ragged float stream"));
            }
            Ok(data
                .chunks_exact(8)
                .map(|b| Value::Float(f64::from_le_bytes(to_8(b))))
                .collect())
        }
        DataType::Boolean => {
            if data.len() != row_count {
                return Err(ProjectionError::CorruptSegment("ragged bool stream"));
            }
            Ok(data.iter().map(|b| Value::Boolean(*b != 0)).collect())
        }
        DataType::Text => decode_text(data, row_count),
        other => Err(ProjectionError::UnsupportedType(other)),
    }
}

fn decode_i64(
    data: &[u8],
    row_count: usize,
    wrap: fn(i64) -> Value,
) -> Result<Vec<Value>, ProjectionError> {
    if data.len() != row_count * 8 {
        return Err(ProjectionError::CorruptSegment("ragged i64 stream"));
    }
    Ok(data
        .chunks_exact(8)
        .map(|b| wrap(i64::from_le_bytes(to_8(b))))
        .collect())
}

fn decode_text(data: &[u8], row_count: usize) -> Result<Vec<Value>, ProjectionError> {
    let mut out = Vec::with_capacity(row_count);
    let mut cur = 0usize;
    for _ in 0..row_count {
        if cur + 4 > data.len() {
            return Err(ProjectionError::CorruptSegment("truncated text length"));
        }
        let len = u32::from_le_bytes(to_4(&data[cur..cur + 4])) as usize;
        cur += 4;
        let end = cur
            .checked_add(len)
            .filter(|e| *e <= data.len())
            .ok_or(ProjectionError::CorruptSegment("truncated text body"))?;
        let s = std::str::from_utf8(&data[cur..end])
            .map_err(|_| ProjectionError::CorruptSegment("invalid utf-8 in text column"))?;
        out.push(Value::Text(s.into()));
        cur = end;
    }
    Ok(out)
}

fn to_8(b: &[u8]) -> [u8; 8] {
    let mut a = [0u8; 8];
    a.copy_from_slice(b);
    a
}

fn to_4(b: &[u8]) -> [u8; 4] {
    let mut a = [0u8; 4];
    a.copy_from_slice(b);
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7u8; 32];

    fn schema() -> ProjectionSchema {
        ProjectionSchema::new(vec![
            ProjectionColumn {
                column_id: 0,
                data_type: DataType::Timestamp,
            },
            ProjectionColumn {
                column_id: 1,
                data_type: DataType::Integer,
            },
            ProjectionColumn {
                column_id: 2,
                data_type: DataType::Float,
            },
            ProjectionColumn {
                column_id: 3,
                data_type: DataType::Boolean,
            },
            ProjectionColumn {
                column_id: 4,
                data_type: DataType::Text,
            },
        ])
    }

    fn row(i: i64) -> Row {
        vec![
            Value::Timestamp(1_700_000_000 + i),
            Value::Integer(i * 3),
            Value::Float(i as f64 * 0.5),
            Value::Boolean(i % 2 == 0),
            Value::Text(format!("event-{i}").into()),
        ]
    }

    /// Fill a collection with rows at lsn = 1..=n.
    fn filled(n: i64) -> AppendOnlyCollection {
        let mut c = AppendOnlyCollection::new(schema());
        for i in 1..=n {
            c.append(i as u64, row(i)).expect("append");
        }
        c
    }

    #[test]
    fn write_path_never_dual_writes_columnar() {
        // AC1: appending rows produces no columnar segments; only an explicit
        // checkpoint emit does.
        let collection = filled(10);
        let projection = ColumnarProjection::new(schema(), KEY);
        assert_eq!(projection.manifest().segments().len(), 0);
        assert_eq!(projection.manifest().last_materialized_lsn(), 0);
        // The collection scan still returns everything via the row path.
        assert_eq!(collection.row_scan(collection.latest_lsn()).len(), 10);
    }

    #[test]
    fn equivalence_oracle_scan_matches_row_scan() {
        // AC2: projection scan == row scan under the same pinned snapshot.
        let collection = filled(500);
        let mut projection = ColumnarProjection::new(schema(), KEY);
        projection
            .emit_at_checkpoint(&collection, 300, TranscodeBudget::default())
            .expect("emit");

        let pinned = collection.latest_lsn();
        let via_projection = projection
            .analytical_scan(&collection, pinned)
            .expect("scan");
        let via_row = collection.row_scan(pinned);

        assert_eq!(via_projection.len(), 500);
        assert_eq!(
            via_projection, via_row,
            "projection scan must equal row scan"
        );
    }

    #[test]
    fn columnar_segments_are_actually_exercised() {
        // Guard: the equivalence must come from a real columnar decode, not an
        // empty projection that falls entirely through the row tail.
        let collection = filled(300);
        let mut projection = ColumnarProjection::new(schema(), KEY);
        let outcome = projection
            .emit_at_checkpoint(&collection, 300, TranscodeBudget::default())
            .expect("emit");
        assert!(outcome.segments_emitted >= 1);
        assert_eq!(projection.manifest().last_materialized_lsn(), 300);
        // Scanning exactly at the materialized ceiling uses only columnar rows.
        let scan = projection.analytical_scan(&collection, 300).expect("scan");
        assert_eq!(scan, collection.row_scan(300));
    }

    #[test]
    fn freshness_row_after_checkpoint_is_immediately_visible() {
        // AC3: a row inserted after the last checkpoint is visible to the
        // analytical scan with no re-materialization — one consistency class.
        let mut collection = filled(100);
        let mut projection = ColumnarProjection::new(schema(), KEY);
        projection
            .emit_at_checkpoint(&collection, 100, TranscodeBudget::default())
            .expect("emit");
        assert_eq!(projection.manifest().last_materialized_lsn(), 100);

        // Commit one more row *after* the checkpoint.
        collection.append(101, row(101)).expect("append fresh");

        let scan = projection.analytical_scan(&collection, 101).expect("scan");
        assert_eq!(scan.len(), 101, "fresh row must be visible immediately");
        assert_eq!(scan.last().cloned().unwrap(), row(101));
        // And the projection was NOT re-materialized to make it visible.
        assert_eq!(projection.manifest().last_materialized_lsn(), 100);
    }

    #[test]
    fn as_of_composes_by_pinning_a_historical_lsn() {
        // AC (ADR §4): AS OF is just a historical pin.
        let collection = filled(200);
        let mut projection = ColumnarProjection::new(schema(), KEY);
        projection
            .emit_at_checkpoint(&collection, 200, TranscodeBudget::default())
            .expect("emit");

        for pin in [1u64, 57, 150, 200] {
            let via_projection = projection.analytical_scan(&collection, pin).expect("scan");
            assert_eq!(
                via_projection,
                collection.row_scan(pin),
                "AS OF {pin} must match row scan"
            );
            assert_eq!(via_projection.len(), pin as usize);
        }
    }

    #[test]
    fn manifest_entries_are_derived_and_checksummed_and_enveloped() {
        // AC4: entries marked derived; chunks carry checksums + crypto envelope.
        let collection = filled(50);
        let mut projection = ColumnarProjection::new(schema(), KEY);
        projection
            .emit_at_checkpoint(&collection, 50, TranscodeBudget::default())
            .expect("emit");

        let manifest = projection.manifest();
        assert!(manifest.all_derived());
        assert_eq!(
            manifest.durable_entries().count(),
            0,
            "derived → backup skips"
        );
        for entry in manifest.segments() {
            assert!(entry.derived);
            assert_ne!(entry.sealed_crc32, 0);
        }

        // The stored bytes are the crypto envelope, not the raw RDCC frame:
        // a wrong key fails the GCM tag check (envelope is real).
        let seg = &projection.segments[0];
        let wrong = ColumnarProjection::new(schema(), [9u8; 32]);
        let err = wrong
            .verify_and_decode_segment(seg, &mut Vec::new())
            .expect_err("wrong key must fail");
        assert!(matches!(err, ProjectionError::Envelope(_)));
    }

    #[test]
    fn sealed_segment_tamper_is_rejected_by_checksum() {
        let collection = filled(20);
        let mut projection = ColumnarProjection::new(schema(), KEY);
        projection
            .emit_at_checkpoint(&collection, 20, TranscodeBudget::default())
            .expect("emit");
        // Flip a byte in the sealed segment; the manifest crc must catch it.
        let mut tampered = projection.segments[0].clone();
        tampered.sealed[0] ^= 0xFF;
        let err = projection
            .verify_and_decode_segment(&tampered, &mut Vec::new())
            .expect_err("tamper must be rejected");
        assert_eq!(
            err,
            ProjectionError::CorruptSegment("sealed checksum mismatch")
        );
    }

    #[test]
    fn transcoding_budget_completes_and_defers() {
        // AC5: checkpoint completes even when the budget is exhausted;
        // deferred rows are picked up by the next checkpoint.
        let collection = filled(1000);
        let mut projection = ColumnarProjection::new(schema(), KEY);

        let budget = TranscodeBudget {
            max_rows: 400,
            segment_rows: 128,
            size_floor_rows: 1,
        };
        let first = projection
            .emit_at_checkpoint(&collection, 1000, budget)
            .expect("first checkpoint");
        assert!(first.budget_exhausted);
        assert_eq!(first.rows_materialized, 400);
        assert_eq!(first.rows_deferred, 600);
        assert_eq!(projection.manifest().last_materialized_lsn(), 400);

        // Even mid-budget, the scan is still complete and correct: the deferred
        // tail is served by the row path.
        let mid = projection.analytical_scan(&collection, 1000).expect("scan");
        assert_eq!(mid, collection.row_scan(1000));
        assert_eq!(mid.len(), 1000);

        // Each subsequent checkpoint transcodes another budget's worth; the
        // deferred tail shrinks monotonically until it is drained. Every round
        // completes, and the scan stays correct throughout.
        let mut rounds = 1;
        loop {
            let out = projection
                .emit_at_checkpoint(&collection, 1000, budget)
                .expect("checkpoint");
            rounds += 1;
            assert_eq!(
                projection.analytical_scan(&collection, 1000).expect("scan"),
                collection.row_scan(1000)
            );
            if !out.budget_exhausted {
                assert_eq!(out.rows_deferred, 0);
                break;
            }
        }
        // 1000 rows / 400 per round → 3 checkpoints total.
        assert_eq!(rounds, 3);
        assert_eq!(projection.manifest().last_materialized_lsn(), 1000);

        let full = projection.analytical_scan(&collection, 1000).expect("scan");
        assert_eq!(full, collection.row_scan(1000));
    }

    #[test]
    fn size_floor_skips_tiny_materialization() {
        // ADR §6: a tail below the floor is skipped, still fully visible.
        let collection = filled(3);
        let mut projection = ColumnarProjection::new(schema(), KEY);
        let budget = TranscodeBudget {
            max_rows: u64::MAX,
            segment_rows: 128,
            size_floor_rows: 10,
        };
        let outcome = projection
            .emit_at_checkpoint(&collection, 3, budget)
            .expect("emit");
        assert!(outcome.floor_skipped);
        assert_eq!(outcome.segments_emitted, 0);
        assert_eq!(projection.manifest().last_materialized_lsn(), 0);
        // Still correct: everything served by the row path.
        assert_eq!(
            projection.analytical_scan(&collection, 3).expect("scan"),
            collection.row_scan(3)
        );
    }

    #[test]
    fn drop_and_rebuild_is_repair() {
        // ADR §1: repair is regenerate, not restore.
        let collection = filled(300);
        let mut projection = ColumnarProjection::new(schema(), KEY);
        projection
            .emit_at_checkpoint(&collection, 300, TranscodeBudget::default())
            .expect("emit");
        let before = projection.analytical_scan(&collection, 300).expect("scan");

        projection.drop_projection();
        assert_eq!(projection.manifest().segments().len(), 0);
        // Reads keep working while un-materialized (pure row tail).
        assert_eq!(
            projection.analytical_scan(&collection, 300).expect("scan"),
            collection.row_scan(300)
        );

        // Rebuild from the row log → identical scan.
        projection
            .emit_at_checkpoint(&collection, 300, TranscodeBudget::default())
            .expect("rebuild");
        let after = projection.analytical_scan(&collection, 300).expect("scan");
        assert_eq!(before, after);
    }

    #[test]
    fn append_rejects_non_monotonic_lsn_and_bad_arity() {
        let mut c = AppendOnlyCollection::new(schema());
        c.append(5, row(1)).expect("first");
        assert!(matches!(
            c.append(5, row(2)),
            Err(ProjectionError::NonMonotonicLsn { .. })
        ));
        assert!(matches!(
            c.append(6, vec![Value::Integer(1)]),
            Err(ProjectionError::RowWidth { .. })
        ));
    }

    #[test]
    fn unsupported_and_mismatched_types_are_rejected() {
        let s = ProjectionSchema::new(vec![ProjectionColumn {
            column_id: 0,
            data_type: DataType::Integer,
        }]);
        let mut c = AppendOnlyCollection::new(s);
        assert!(matches!(
            c.append(1, vec![Value::Text("x".into())]),
            Err(ProjectionError::TypeMismatch { .. })
        ));

        let s2 = ProjectionSchema::new(vec![ProjectionColumn {
            column_id: 0,
            data_type: DataType::Blob,
        }]);
        let mut c2 = AppendOnlyCollection::new(s2);
        assert!(matches!(
            c2.append(1, vec![Value::Blob(vec![1, 2, 3])]),
            Err(ProjectionError::UnsupportedType(_))
        ));
    }
}
