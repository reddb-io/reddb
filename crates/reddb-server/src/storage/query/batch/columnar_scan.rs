//! Columnar chunk → `ColumnBatch` bridge (#856, PRD #850 Phase 2).
//!
//! Connects the sealed columnar **chunk** decode (`RDCC` `ColumnBlock`,
//! #852) to the vectorised [`ColumnBatch`](super::ColumnBatch) reader that
//! has lived self-contained in `storage/query/batch/` since the batch
//! sprint. An analytical scan over a columnar `Metrics`/`TimeSeries` chunk
//! now decodes straight into one typed [`ColumnVector`] per referenced
//! column — the same column-at-a-time layout the batch operators consume —
//! instead of the row-at-a-time `Vec<TimeSeriesPoint>` accumulation
//! [`points_from_column_block`](crate::storage::timeseries::chunk::points_from_column_block)
//! produces.
//!
//! Two properties are load-bearing for the Phase 2 gate:
//!
//! * **Parity.** The values materialised here are bit-for-bit the values the
//!   row path materialises — the batch path reinterprets the *same* raw
//!   little-endian column bytes [`read_column_block`] hands back. No second
//!   decoder, no divergent rounding.
//! * **Projection pushdown.** Only the columns named in `projection` are
//!   decoded; unreferenced column streams are never run through the codec
//!   chain (via [`read_column_block_projected`]). A scan that touches one
//!   column out of N pays for one column.
//!
//! Scope: this is the read/decode wiring only. The live INSERT→seal runtime
//! call-site is owned by #861; full operator vectorisation across the SQL
//! executor is explicitly out of scope (PRD #850).

use std::sync::Arc;

use super::column_batch::{ColumnBatch, ColumnKind, ColumnVector, Field, Schema};
use crate::storage::schema::types::DataType;
use crate::storage::unified::column_block::{read_column_block_projected, ColumnBlockError};

/// Failures decoding a columnar chunk into a [`ColumnBatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnarScanError {
    /// The underlying `RDCC` block was malformed (bad magic, CRC, directory…).
    Block(ColumnBlockError),
    /// A requested column id was not present in the chunk's directory.
    MissingColumn(u32),
    /// The column's logical type has no `ColumnBatch` representation
    /// (the batch layer carries only Int64/Float64/Bool/Text). Carries the
    /// `DataType::to_byte()` tag that was rejected.
    UnsupportedLogicalType(u8),
    /// A fixed-width numeric stream's byte length was not a multiple of the
    /// element width — the chunk is corrupt.
    RaggedStream { column_id: u32, len: usize },
}

impl From<ColumnBlockError> for ColumnarScanError {
    fn from(e: ColumnBlockError) -> Self {
        ColumnarScanError::Block(e)
    }
}

/// Map a stored `RDCC` logical-type tag to the batch layer's column kind.
/// The batch executor reasons over four physical kinds; every fixed-width
/// integer-family type collapses to `Int64` (8-byte LE reinterpret) and the
/// float types to `Float64`. `None` for any type the batch layer can't hold.
fn kind_for_logical_type(tag: u8) -> Option<ColumnKind> {
    match DataType::from_byte(tag)? {
        // 8-byte integer-family streams. Unsigned values are reinterpreted
        // through the same little-endian bytes; the row path does the same.
        DataType::Integer
        | DataType::UnsignedInteger
        | DataType::Timestamp
        | DataType::Duration => Some(ColumnKind::Int64),
        DataType::Float => Some(ColumnKind::Float64),
        DataType::Boolean => Some(ColumnKind::Bool),
        DataType::Text => Some(ColumnKind::Text),
        _ => None,
    }
}

/// Decode the fixed-width numeric raw bytes of one column into a typed
/// [`ColumnVector`]. Caller guarantees `kind` is a numeric kind.
fn numeric_vector(
    column_id: u32,
    kind: &ColumnKind,
    raw: &[u8],
) -> Result<ColumnVector, ColumnarScanError> {
    if !raw.len().is_multiple_of(8) {
        return Err(ColumnarScanError::RaggedStream {
            column_id,
            len: raw.len(),
        });
    }
    let n = raw.len() / 8;
    Ok(match kind {
        ColumnKind::Int64 => ColumnVector::Int64 {
            data: le_bytes_to_i64_vec(raw, n),
            validity: None,
        },
        ColumnKind::Float64 => ColumnVector::Float64 {
            data: le_bytes_to_f64_vec(raw, n),
            validity: None,
        },
        // Bool/Text never reach here (the caller routes them away); keep the
        // match exhaustive without inventing a decode.
        other => {
            return Err(ColumnarScanError::UnsupportedLogicalType(match other {
                ColumnKind::Bool => DataType::Boolean.to_byte(),
                ColumnKind::Text => DataType::Text.to_byte(),
                _ => unreachable!(),
            }))
        }
    })
}

/// Convert a slice of little-endian 8-byte values to `Vec<i64>` (#962 fast path).
/// On LE targets a single memcpy suffices; on BE each element is byte-swapped.
fn le_bytes_to_i64_vec(raw: &[u8], n: usize) -> Vec<i64> {
    #[cfg(target_endian = "little")]
    {
        // SAFETY: `raw` holds `n * 8` valid bytes. `Vec<i64>` is freshly allocated
        // so source and destination never overlap. On LE platforms the bit pattern
        // of LE-encoded i64 bytes equals the native i64 representation.
        let mut v: Vec<i64> = Vec::with_capacity(n);
        unsafe {
            std::ptr::copy_nonoverlapping(raw.as_ptr(), v.as_mut_ptr() as *mut u8, n * 8);
            v.set_len(n);
        }
        v
    }
    #[cfg(not(target_endian = "little"))]
    raw.chunks_exact(8)
        .map(|b| i64::from_le_bytes(b.try_into().unwrap()))
        .collect()
}

/// Convert a slice of little-endian 8-byte values to `Vec<f64>` (#962 fast path).
fn le_bytes_to_f64_vec(raw: &[u8], n: usize) -> Vec<f64> {
    #[cfg(target_endian = "little")]
    {
        // SAFETY: same as `le_bytes_to_i64_vec`. Every 8-byte pattern is a valid
        // f64 bit pattern (including NaN / ±inf / ±0), so no invalid values can
        // be produced.
        let mut v: Vec<f64> = Vec::with_capacity(n);
        unsafe {
            std::ptr::copy_nonoverlapping(raw.as_ptr(), v.as_mut_ptr() as *mut u8, n * 8);
            v.set_len(n);
        }
        v
    }
    #[cfg(not(target_endian = "little"))]
    raw.chunks_exact(8)
        .map(|b| f64::from_le_bytes(b.try_into().unwrap()))
        .collect()
}

/// Decode a sealed columnar chunk (`RDCC` bytes) into a [`ColumnBatch`],
/// materialising **only** the columns in `projection` (by stable column id,
/// in the given order). This is the vectorised counterpart to the
/// row-at-a-time `points_from_column_block`: identical values, column-major
/// layout, and unreferenced columns are never decoded.
///
/// Field names are synthesised as `col_{id}` — the `RDCC` block keys columns
/// by stable id, not by name; the batch operators address columns by index
/// or by the schema's `index_of`, so the synthetic name is purely a handle.
///
/// Errors if a requested id is absent ([`ColumnarScanError::MissingColumn`])
/// or carries a logical type the batch layer can't represent
/// ([`ColumnarScanError::UnsupportedLogicalType`]). Only numeric chunks
/// (Metrics/TimeSeries timestamp+value) are exercised today; Bool/Text map
/// to a kind but their stream decode is out of this slice's scope and is
/// rejected rather than silently mis-decoded.
pub fn column_batch_from_block(
    bytes: &[u8],
    projection: &[u32],
) -> Result<ColumnBatch, ColumnarScanError> {
    let block = read_column_block_projected(bytes, projection)?;

    let mut fields = Vec::with_capacity(projection.len());
    let mut columns = Vec::with_capacity(projection.len());

    // Honour the caller's projection order (the projected reader returns
    // columns in directory order, which may differ from the query order).
    for &id in projection {
        let col = block
            .columns
            .iter()
            .find(|c| c.column_id == id)
            .ok_or(ColumnarScanError::MissingColumn(id))?;
        let kind = kind_for_logical_type(col.logical_type)
            .ok_or(ColumnarScanError::UnsupportedLogicalType(col.logical_type))?;
        let vector = match kind {
            ColumnKind::Int64 | ColumnKind::Float64 => numeric_vector(id, &kind, &col.data)?,
            ColumnKind::Bool | ColumnKind::Text => {
                return Err(ColumnarScanError::UnsupportedLogicalType(col.logical_type))
            }
        };
        fields.push(Field {
            name: format!("col_{id}"),
            kind,
            nullable: false,
        });
        columns.push(vector);
    }

    let schema = Arc::new(Schema::new(fields));
    Ok(ColumnBatch::new(schema, columns))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::timeseries::chunk::{
        points_from_column_block, TimeSeriesChunk, COLUMNAR_TS_COLUMN_ID, COLUMNAR_VALUE_COLUMN_ID,
    };

    /// Seal a synthetic columnar chunk of `n` points and return its `RDCC`
    /// bytes — the same path Metrics/TimeSeries collections seal through.
    fn sealed_columnar_chunk(n: usize) -> Vec<u8> {
        // `with_max_points` so large measurement chunks aren't capped by the
        // default 1024-point auto-seal threshold.
        let mut chunk = TimeSeriesChunk::with_max_points("cpu.idle", Default::default(), n.max(1));
        for i in 0..n {
            assert!(chunk.append(
                1_700_000_000_000 + i as u64 * 1_000_000,
                95.0 + (i % 7) as f64 * 0.25
            ));
        }
        chunk.seal_columnar(7, 1).expect("seal columnar chunk")
    }

    #[test]
    fn scan_produces_results_through_the_column_batch_path() {
        // AC1: a scan over a columnar chunk yields results via ColumnBatch.
        let block = sealed_columnar_chunk(300);
        let batch =
            column_batch_from_block(&block, &[COLUMNAR_TS_COLUMN_ID, COLUMNAR_VALUE_COLUMN_ID])
                .expect("decode into ColumnBatch");
        assert_eq!(batch.len(), 300);
        assert_eq!(batch.schema.len(), 2);
        // Timestamp column is Int64 (u64 reinterpret), value column is Float64.
        assert!(matches!(batch.columns[0], ColumnVector::Int64 { .. }));
        assert!(matches!(batch.columns[1], ColumnVector::Float64 { .. }));
    }

    #[test]
    fn batch_path_is_value_for_value_identical_to_the_row_path() {
        // AC2: behavioural parity with the row-at-a-time path.
        let block = sealed_columnar_chunk(257);
        let row_points = points_from_column_block(&block).expect("row path");
        let batch =
            column_batch_from_block(&block, &[COLUMNAR_TS_COLUMN_ID, COLUMNAR_VALUE_COLUMN_ID])
                .expect("batch path");

        assert_eq!(batch.len(), row_points.len());
        for (i, p) in row_points.iter().enumerate() {
            // u64 timestamp survives the i64 reinterpret round-trip.
            let ts = match &batch.columns[0] {
                ColumnVector::Int64 { data, .. } => data[i] as u64,
                _ => unreachable!(),
            };
            let val = match &batch.columns[1] {
                ColumnVector::Float64 { data, .. } => data[i],
                _ => unreachable!(),
            };
            assert_eq!(ts, p.timestamp_ns, "timestamp parity at row {i}");
            assert_eq!(val, p.value, "value parity at row {i}");
        }
    }

    #[test]
    fn projection_decodes_only_referenced_columns() {
        // AC3: only the projected column is materialised.
        let block = sealed_columnar_chunk(128);
        let ts_only =
            column_batch_from_block(&block, &[COLUMNAR_TS_COLUMN_ID]).expect("ts-only projection");
        assert_eq!(ts_only.schema.len(), 1);
        assert_eq!(ts_only.columns.len(), 1);
        assert_eq!(ts_only.schema.index_of("col_0"), Some(0));
        assert_eq!(ts_only.schema.index_of("col_1"), None);

        let val_only = column_batch_from_block(&block, &[COLUMNAR_VALUE_COLUMN_ID])
            .expect("value-only projection");
        assert_eq!(val_only.schema.len(), 1);
        assert!(matches!(val_only.columns[0], ColumnVector::Float64 { .. }));
    }

    #[test]
    fn missing_column_is_an_error() {
        let block = sealed_columnar_chunk(16);
        // `ColumnBatch` isn't `PartialEq`, so match on the error arm directly
        // rather than comparing the whole `Result`.
        assert_eq!(
            column_batch_from_block(&block, &[42]).unwrap_err(),
            ColumnarScanError::MissingColumn(42)
        );
    }

    #[test]
    fn measured_row_vs_batch_decode_comparison() {
        // AC4 (Phase 2 gate): record a measured comparison of the columnar
        // batch decode vs the row-at-a-time decode over the same chunk.
        // This test never asserts on timing (wall-clock is machine- and
        // load-dependent and would be flaky); it asserts parity and prints
        // the measurement so the Phase 2 go/no-go has a number to read. The
        // figure is also captured in the issue envelope / commit message.
        use std::time::Instant;

        let n = 50_000;
        let block = sealed_columnar_chunk(n);
        let projection = [COLUMNAR_TS_COLUMN_ID, COLUMNAR_VALUE_COLUMN_ID];

        // Warm both paths once (codec setup, allocator) before timing.
        let _ = points_from_column_block(&block).unwrap();
        let _ = column_batch_from_block(&block, &projection).unwrap();

        let reps = 20;
        let t_row = Instant::now();
        let mut row_rows = 0usize;
        for _ in 0..reps {
            row_rows = points_from_column_block(&block).unwrap().len();
        }
        let row_elapsed = t_row.elapsed();

        let t_batch = Instant::now();
        let mut batch_rows = 0usize;
        for _ in 0..reps {
            batch_rows = column_batch_from_block(&block, &projection).unwrap().len();
        }
        let batch_elapsed = t_batch.elapsed();

        // Both paths see the same rows — the comparison is apples-to-apples.
        assert_eq!(row_rows, n);
        assert_eq!(batch_rows, n);

        let row_ns = row_elapsed.as_nanos() as f64 / reps as f64;
        let batch_ns = batch_elapsed.as_nanos() as f64 / reps as f64;
        eprintln!(
            "[#856 Phase 2 gate] columnar decode of {n} rows ({reps} reps): \
             row-path {row_ns:.0} ns/scan, batch-path {batch_ns:.0} ns/scan, \
             ratio {:.2}x (batch/row)",
            batch_ns / row_ns
        );
    }
}
