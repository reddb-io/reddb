//! RedDB Wire Protocol — binary TCP, zero JSON overhead.
//!
//! Frame: [total_len: u32 LE][msg_type: u8][payload...]
//!
//! Message types (client → server):
//!   0x01 Query       [sql_bytes...]
//!   0x04 BulkInsert  [coll_len:u16][coll_bytes][n:u32][json_len:u32 + json_bytes]...
//!
//! Message types (server → client):
//!   0x02 Result      [ncols:u16][col_name_len:u16 + col_name]...[nrows:u32][row...]
//!                     row = [val_type:u8 + val_data]... per column
//!   0x03 Error       [error_bytes...]
//!   0x05 BulkOk      [count:u64]

// --- Message type constants ---
pub const MSG_QUERY: u8 = 0x01;
pub const MSG_RESULT: u8 = 0x02;
pub const MSG_ERROR: u8 = 0x03;
pub const MSG_BULK_INSERT: u8 = 0x04;
pub const MSG_BULK_OK: u8 = 0x05;
pub const MSG_BULK_INSERT_BINARY: u8 = 0x06;
pub const MSG_QUERY_BINARY: u8 = 0x07;

/// Fast-path bulk insert: payload is `[coll_len u16][coll][ncols u16]
/// ([col_name u16 len + bytes])*ncols [nrows u32] ([val_tag u8 +
/// val_data]*ncols)*nrows`, IDENTICAL to `MSG_BULK_INSERT_BINARY`.
/// The only difference from 0x06 is semantic: the caller guarantees
/// every value already matches the declared column type and that
/// contract / uniqueness rules either don't apply or were already
/// checked client-side. The server skips
/// `normalize_row_fields_for_contract`, `enforce_row_uniqueness`
/// and `enforce_row_batch_uniqueness` on the whole batch, cutting
/// 15-column typed inserts from O(nrows × ncols) contract work
/// down to O(nrows) serialise-and-insert. Intended for typed-bench
/// workloads and driver-generated inserts where types were already
/// validated before the send. Old servers that don't know 0x08
/// reply with `MSG_ERROR "unknown message type"` so clients can
/// fall back to the safe path.
pub const MSG_BULK_INSERT_PREVALIDATED: u8 = 0x08;

// ── Streaming bulk insert (PG COPY-equivalent) ─────────────────────
//
// The prevalidated path (0x08) still pays one TCP round-trip per
// batch. For a 25k-row typed_insert chunked at BULK_BATCH_SIZE=1000
// that's 25 round-trips — each with its own 3ms wire latency and
// full schema re-declaration. PG's COPY BINARY sends the schema
// once and streams all rows in a single logical transaction, which
// is why PG typed_insert hits 120k ops/s versus our ~11k.
//
// The streaming protocol closes that gap:
//
//   client → MSG_BULK_STREAM_START  (collection + schema, once)
//   client → MSG_BULK_STREAM_ROWS   (nrows + row data, 1…N times)
//   client → MSG_BULK_STREAM_COMMIT (empty; server finalises)
//   server → MSG_BULK_OK            (total inserted count)
//
// Schema sent once, commit round-trip amortised across every ROWS
// frame. Rows are accumulated server-side in one `Vec<Vec<Value>>`
// and handed to the columnar pre-validated insert path as a single
// batch — same server-side semantics as 0x08, just amortised wire
// framing. If the caller needs lower peak memory it can still
// split with a COMMIT per N rows.
pub const MSG_BULK_STREAM_START: u8 = 0x09;
pub const MSG_BULK_STREAM_ROWS: u8 = 0x0A;
pub const MSG_BULK_STREAM_COMMIT: u8 = 0x0B;
/// Intermediate ack sent back for START + ROWS frames so the
/// client can pipeline safely (it always receives a frame per
/// frame sent) without conflating progress with the terminal
/// MSG_BULK_OK at COMMIT time.
pub const MSG_BULK_STREAM_ACK: u8 = 0x0C;

// ── Prepared statements (PG-style Prepare/Execute) ────────────────
//
// The plan cache already parameterizes literals and caches by shape
// key, but every MSG_QUERY_BINARY still pays: (a) the byte-scan
// `normalize_and_extract` that builds the shape key from text, (b) a
// `HashMap<String, CachedPlan>` lookup, and (c) Arc clones. On a tight
// `select_point` loop that's ~50-100µs per call, dominated by the
// parse/normalize work.
//
// Prepared statements skip all three by letting the client allocate a
// per-connection `stmt_id: u32` at PREPARE time (parse + parameterize
// once) and later reference the compiled shape by that integer at
// EXECUTE time (bind + run, no text involved).
//
//   client → MSG_PREPARE          [stmt_id u32][sql_len u32][sql]
//   server → MSG_PREPARED_OK      [stmt_id u32][param_count u16]
//   client → MSG_EXECUTE_PREPARED [stmt_id u32][nparams u16](val_tag+data)*
//   server → MSG_RESULT / MSG_ERROR  (same shape as MSG_QUERY_BINARY)
//   client → MSG_DEALLOCATE       [stmt_id u32]
//   server → (no response on success; MSG_ERROR on unknown id)
//
// State is per-connection. Disconnect drops all prepared statements
// the connection owned. Clients that don't implement these messages
// are unaffected — old servers reply `unknown message type` so the
// client transparently falls back to MSG_QUERY_BINARY.
pub const MSG_PREPARE: u8 = 0x0D;
pub const MSG_PREPARED_OK: u8 = 0x0E;
pub const MSG_EXECUTE_PREPARED: u8 = 0x0F;
pub const MSG_DEALLOCATE: u8 = 0x10;

// ── Cursors (server-side paginated SELECT) ────────────────────────
//
// Large SELECTs force one of two bad choices today: return every row
// in a single MSG_RESULT frame (O(result) memory on both client and
// server) or paginate via LIMIT/OFFSET (O(offset) re-scan per page).
// Cursors cut both: DECLARE runs the query once and parks the result,
// each FETCH slices the next N rows without re-executing.
//
// V1 materialises the full result set at DECLARE and serves FETCH
// from the buffered Vec. Trades peak memory for scan-once semantics —
// fine for bench scenarios and the typical range-pagination pattern.
// Streaming variant with snapshot pinning lands in V2 alongside the
// MVCC context capture work.
//
//   client → MSG_DECLARE_CURSOR [cursor_id u32][sql_len u32][sql]
//   server → MSG_CURSOR_OK      [cursor_id u32][ncols u16]
//                               ([col_len u16][col_name])*ncols
//                               [total_rows u64]
//   client → MSG_FETCH          [cursor_id u32][max_rows u32]
//   server → MSG_CURSOR_BATCH   [cursor_id u32][nrows u32][has_more u8]
//                               ([val_tag u8][val_data])* per row/col
//   client → MSG_CLOSE_CURSOR   [cursor_id u32]
//   server → (no response; MSG_ERROR on unknown id)
//
// State is per-connection. Disconnect drops every cursor the
// connection owned. Max 16 open cursors per connection — DECLAREs
// past the cap return MSG_ERROR so runaway clients can't OOM the
// server.
pub const MSG_DECLARE_CURSOR: u8 = 0x11;
pub const MSG_CURSOR_OK: u8 = 0x12;
pub const MSG_FETCH: u8 = 0x13;
pub const MSG_CURSOR_BATCH: u8 = 0x14;
pub const MSG_CLOSE_CURSOR: u8 = 0x15;

// --- Value type tags ---
pub const VAL_NULL: u8 = 0;
pub const VAL_I64: u8 = 1;
pub const VAL_F64: u8 = 2;
pub const VAL_TEXT: u8 = 3;
pub const VAL_BOOL: u8 = 4;
pub const VAL_U64: u8 = 5;

use crate::storage::schema::Value;

/// Write a frame header: [total_len: u32 LE][msg_type: u8]
#[inline]
pub fn write_frame_header(buf: &mut Vec<u8>, msg_type: u8, payload_len: u32) {
    let total = payload_len + 1; // +1 for msg_type
    buf.extend_from_slice(&total.to_le_bytes());
    buf.push(msg_type);
}

/// Encode a Value to wire format bytes, appending to buf.
#[inline]
pub fn encode_value(buf: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => buf.push(VAL_NULL),
        Value::Integer(n) => {
            buf.push(VAL_I64);
            buf.extend_from_slice(&n.to_le_bytes());
        }
        Value::UnsignedInteger(n) => {
            buf.push(VAL_U64);
            buf.extend_from_slice(&n.to_le_bytes());
        }
        Value::Float(f) => {
            buf.push(VAL_F64);
            buf.extend_from_slice(&f.to_le_bytes());
        }
        Value::Text(s) => {
            buf.push(VAL_TEXT);
            let bytes = s.as_bytes();
            buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(bytes);
        }
        Value::Boolean(b) => {
            buf.push(VAL_BOOL);
            buf.push(*b as u8);
        }
        Value::Timestamp(t) => {
            buf.push(VAL_U64);
            buf.extend_from_slice(&t.to_le_bytes());
        }
        _ => buf.push(VAL_NULL),
    }
}

/// Decode a Value from wire bytes at the given position.
#[inline]
pub fn decode_value(data: &[u8], pos: &mut usize) -> Value {
    try_decode_value(data, pos).unwrap_or(Value::Null)
}

#[inline]
pub fn try_decode_value(data: &[u8], pos: &mut usize) -> Result<Value, &'static str> {
    if *pos >= data.len() {
        return Err("missing value tag");
    }

    let tag = data[*pos];
    *pos += 1;

    match tag {
        VAL_NULL => Ok(Value::Null),
        VAL_I64 => Ok(Value::Integer(i64::from_le_bytes(read_array::<8>(
            data,
            pos,
            "truncated i64 value",
        )?))),
        VAL_U64 => Ok(Value::UnsignedInteger(u64::from_le_bytes(read_array::<8>(
            data,
            pos,
            "truncated u64 value",
        )?))),
        VAL_F64 => Ok(Value::Float(f64::from_le_bytes(read_array::<8>(
            data,
            pos,
            "truncated f64 value",
        )?))),
        VAL_TEXT => {
            let len =
                u32::from_le_bytes(read_array::<4>(data, pos, "truncated text length")?) as usize;
            let bytes = read_bytes(data, pos, len, "truncated text value")?;
            // Avoid the double allocation the previous code paid:
            //   bytes → String (via Cow::to_string) → Arc<str> (via Value::text).
            // The common case (valid UTF-8 from a well-behaved client) borrows
            // without allocating from the Cow, then `Arc::<str>::from(&str)`
            // copies once into the shared ref-counted buffer.
            let cow = std::string::String::from_utf8_lossy(bytes);
            Ok(Value::text(std::sync::Arc::<str>::from(cow.as_ref())))
        }
        VAL_BOOL => {
            let bytes = read_bytes(data, pos, 1, "truncated bool value")?;
            Ok(Value::Boolean(bytes[0] != 0))
        }
        _ => Err("unknown value tag"),
    }
}

#[inline]
fn read_bytes<'a>(
    data: &'a [u8],
    pos: &mut usize,
    len: usize,
    err: &'static str,
) -> Result<&'a [u8], &'static str> {
    let end = pos.saturating_add(len);
    if end > data.len() {
        return Err(err);
    }
    let bytes = &data[*pos..end];
    *pos = end;
    Ok(bytes)
}

#[inline]
fn read_array<const N: usize>(
    data: &[u8],
    pos: &mut usize,
    err: &'static str,
) -> Result<[u8; N], &'static str> {
    let bytes = read_bytes(data, pos, N, err)?;
    let mut array = [0u8; N];
    array.copy_from_slice(bytes);
    Ok(array)
}

/// Encode a column name to wire format.
#[inline]
pub fn encode_column_name(buf: &mut Vec<u8>, name: &str) {
    let bytes = name.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(bytes);
}
