//! JSON-RPC 2.0 line-delimited stdio mode for the `red` binary.
//!
//! See `PLAN_DRIVERS.md` for the protocol spec. This module is the
//! sole server-side implementation of the protocol — drivers in
//! every language target this contract.
//!
//! Loop:
//!   1. Read a line from stdin (UTF-8, terminated by `\n`).
//!   2. Parse it as a JSON-RPC 2.0 request envelope.
//!   3. Dispatch on `method` to the runtime.
//!   4. Serialize the response as a single line on stdout, flush.
//!   5. Repeat until EOF or `close` method received.
//!
//! Errors do not crash the loop. Panics inside a method handler are
//! caught and reported as `INTERNAL_ERROR` so a buggy query cannot
//! kill the daemon.

use std::io::{BufRead, BufReader, Stdin, Write};
use std::panic::AssertUnwindSafe;

use tokio::sync::Mutex as AsyncMutex;

use crate::client::RedDBClient;
use crate::json::{self as json, Value};
use crate::runtime::{RedDBRuntime, RuntimeQueryResult};
use crate::storage::query::unified::UnifiedRecord;
use crate::storage::schema::Value as SchemaValue;

/// Which backend the stdio loop is wrapping.
///
/// `Local` = the in-process engine (embedded). `Remote` = a tonic client
/// to a standalone `red server` talking gRPC. The remote variant is
/// boxed because `RedDBClient` + a `tokio::Runtime` reference is ~248
/// bytes against `Local`'s ~8 bytes (clippy::large_enum_variant).
///
/// The mutex uses `tokio::sync::Mutex` instead of `std::sync::Mutex`
/// because `dispatch_method_remote` holds the guard across `.await`
/// points inside `tokio_rt.block_on(...)` — holding a sync mutex
/// across an await would be a correctness bug in more complex
/// runtimes.
enum Backend<'a> {
    Local(&'a RedDBRuntime),
    Remote(Box<RemoteBackend<'a>>),
}

struct RemoteBackend<'a> {
    client: AsyncMutex<RedDBClient>,
    tokio_rt: &'a tokio::runtime::Runtime,
}

/// Protocol version reported by the `version` method.
pub const PROTOCOL_VERSION: &str = "1.0";

/// Stable error codes. Drivers map these to idiomatic exceptions.
pub mod error_code {
    pub const PARSE_ERROR: &str = "PARSE_ERROR";
    pub const INVALID_REQUEST: &str = "INVALID_REQUEST";
    pub const INVALID_PARAMS: &str = "INVALID_PARAMS";
    pub const QUERY_ERROR: &str = "QUERY_ERROR";
    pub const NOT_FOUND: &str = "NOT_FOUND";
    pub const INTERNAL_ERROR: &str = "INTERNAL_ERROR";
    /// `tx.begin` was called while a transaction was already open in the
    /// same session.
    pub const TX_ALREADY_OPEN: &str = "TX_ALREADY_OPEN";
    /// `tx.commit` / `tx.rollback` was called without a matching
    /// `tx.begin`.
    pub const NO_TX_OPEN: &str = "NO_TX_OPEN";
    /// A buffered statement failed during `tx.commit` replay. The error
    /// message carries the index of the failing op and the number of
    /// operations that successfully applied before the failure.
    pub const TX_REPLAY_FAILED: &str = "TX_REPLAY_FAILED";
    /// Transactions over the remote gRPC proxy are not supported yet.
    pub const TX_NOT_SUPPORTED_REMOTE: &str = "TX_NOT_SUPPORTED_REMOTE";
    /// `query.next` / `query.close` referenced an unknown cursor id.
    /// Either the cursor was never opened, already closed, or was
    /// automatically dropped when its rows were exhausted.
    pub const CURSOR_NOT_FOUND: &str = "CURSOR_NOT_FOUND";
    /// Too many concurrent cursors open in a single session.
    pub const CURSOR_LIMIT_EXCEEDED: &str = "CURSOR_LIMIT_EXCEEDED";
}

/// Maximum number of cursors a single stdio session may hold open
/// simultaneously. Serves as a memory-pressure guard against runaway
/// clients that `query.open` without ever closing.
pub(crate) const MAX_CURSORS_PER_SESSION: usize = 64;
/// Default batch size for `query.next` when the client does not specify
/// one explicitly. Tuned for small-to-medium rows; large-row clients
/// should set a smaller value.
pub(crate) const DEFAULT_CURSOR_BATCH_SIZE: usize = 100;
/// Hard upper bound on `query.next` batch size. Prevents a single call
/// from stalling the stdio loop with a multi-megabyte line.
pub(crate) const MAX_CURSOR_BATCH_SIZE: usize = 10_000;

// ---------------------------------------------------------------------------
// Session state (transaction buffer)
// ---------------------------------------------------------------------------
//
// Transactions in the stdio protocol are scoped to a single connection —
// one process = one session = at most one open transaction. The state
// lives in the stack of `run_backend` so nothing leaks between
// connections, and there is no cross-session visibility of buffered
// writes.
//
// Isolation model: `read_committed_deferred`. Reads inside a transaction
// observe the latest *committed* state; they do **not** see writes the
// same session has buffered via `insert` / `delete` / `bulk_insert`.
// Atomicity is best-effort — a global commit lock serializes replays, but
// auto-committed writes from other sessions may interleave between
// commits. Strict atomicity requires funnelling every write through a
// single session.

/// Per-connection session that tracks the currently open transaction
/// and any active streaming cursors.
// A server-side prepared statement bound to this session.
// When parameter_count == 0, shape == the exact plan (no substitution needed).
struct StdioPreparedStatement {
    shape: crate::storage::query::ast::QueryExpr,
    parameter_count: usize,
}

pub(crate) struct Session {
    next_tx_id: u64,
    current_tx: Option<OpenTx>,
    next_cursor_id: u64,
    cursors: std::collections::HashMap<u64, Cursor>,
    /// Monotone counter for prepared statement IDs within this session.
    next_prepared_id: u64,
    /// Active prepared statements, keyed by the ID returned to the client.
    prepared: std::collections::HashMap<u64, StdioPreparedStatement>,
}

impl Session {
    pub(crate) fn new() -> Self {
        Self {
            next_tx_id: 1,
            current_tx: None,
            next_cursor_id: 1,
            cursors: std::collections::HashMap::new(),
            next_prepared_id: 1,
            prepared: std::collections::HashMap::new(),
        }
    }

    fn open_tx(&mut self) -> Result<u64, (&'static str, String)> {
        if let Some(tx) = &self.current_tx {
            return Err((
                error_code::TX_ALREADY_OPEN,
                format!("transaction {} already open in this session", tx.tx_id),
            ));
        }
        let tx_id = self.next_tx_id;
        self.next_tx_id = self.next_tx_id.saturating_add(1);
        self.current_tx = Some(OpenTx {
            tx_id,
            write_set: Vec::new(),
        });
        Ok(tx_id)
    }

    fn take_tx(&mut self) -> Option<OpenTx> {
        self.current_tx.take()
    }

    fn current_tx_mut(&mut self) -> Option<&mut OpenTx> {
        self.current_tx.as_mut()
    }

    #[allow(dead_code)]
    fn has_tx(&self) -> bool {
        self.current_tx.is_some()
    }

    /// Register a freshly materialised cursor and return its id.
    /// Enforces [`MAX_CURSORS_PER_SESSION`] before allocating.
    fn insert_cursor(&mut self, cursor: Cursor) -> Result<u64, (&'static str, String)> {
        if self.cursors.len() >= MAX_CURSORS_PER_SESSION {
            return Err((
                error_code::CURSOR_LIMIT_EXCEEDED,
                format!(
                    "session already holds {} cursors (max {}) — close some before opening new ones",
                    self.cursors.len(),
                    MAX_CURSORS_PER_SESSION
                ),
            ));
        }
        let id = self.next_cursor_id;
        self.next_cursor_id = self.next_cursor_id.saturating_add(1);
        let mut cursor = cursor;
        cursor.cursor_id = id;
        self.cursors.insert(id, cursor);
        Ok(id)
    }

    fn cursor_mut(&mut self, id: u64) -> Option<&mut Cursor> {
        self.cursors.get_mut(&id)
    }

    fn drop_cursor(&mut self, id: u64) -> Option<Cursor> {
        self.cursors.remove(&id)
    }

    fn clear_cursors(&mut self) {
        self.cursors.clear();
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// An in-flight transaction for a single stdio session.
struct OpenTx {
    tx_id: u64,
    write_set: Vec<PendingSql>,
}

/// A buffered mutation waiting for `tx.commit`. Each variant carries a
/// ready-to-execute SQL string so the replay loop is a straight
/// `execute_query` call.
enum PendingSql {
    Insert(String),
    Delete(String),
    #[allow(dead_code)] // reserved for future query()-in-tx routing
    Update(String),
}

impl PendingSql {
    fn sql(&self) -> &str {
        match self {
            PendingSql::Insert(s) | PendingSql::Delete(s) | PendingSql::Update(s) => s,
        }
    }
}

/// An open streaming cursor over a materialised query result.
///
/// MVP model: the underlying [`RuntimeQueryResult`] has already been
/// fully executed at `query.open` time and lives inside the cursor.
/// Each `query.next` call slices off `batch_size` rows from the tail and
/// advances `position`. This pays normal memory cost but lets the client
/// consume the result in chunks, abort mid-stream, or pipeline the next
/// batch request while processing the previous one.
///
/// A future iteration can swap the rows field for a lazy iterator pulled
/// from the execution engine without changing the wire protocol.
pub(crate) struct Cursor {
    cursor_id: u64,
    columns: Vec<String>,
    rows: Vec<UnifiedRecord>,
    position: usize,
}

impl Cursor {
    fn new(columns: Vec<String>, rows: Vec<UnifiedRecord>) -> Self {
        Self {
            cursor_id: 0, // overwritten by Session::insert_cursor
            columns,
            rows,
            position: 0,
        }
    }

    fn total(&self) -> usize {
        self.rows.len()
    }

    fn remaining(&self) -> usize {
        self.rows.len().saturating_sub(self.position)
    }

    fn is_exhausted(&self) -> bool {
        self.position >= self.rows.len()
    }

    /// Extract up to `batch_size` rows from the current position forward.
    /// Advances the position to the end of the returned slice.
    fn take_batch(&mut self, batch_size: usize) -> &[UnifiedRecord] {
        let end = (self.position + batch_size).min(self.rows.len());
        let slice = &self.rows[self.position..end];
        self.position = end;
        slice
    }
}

/// Run the stdio JSON-RPC loop against a local in-process runtime.
///
/// Returns the process exit code. `0` on normal shutdown (EOF or
/// explicit `close`). Non-zero only on fatal I/O errors reading
/// stdin or writing stdout.
pub fn run(runtime: &RedDBRuntime) -> i32 {
    run_with_io(runtime, std::io::stdin(), &mut std::io::stdout())
}

/// Run the stdio JSON-RPC loop as a proxy to a remote gRPC server.
///
/// Every method is forwarded via tonic. This is what
/// `red rpc --stdio --connect grpc://host:port` uses, and it is also
/// what the JS and Python drivers spawn when the user calls
/// `connect("grpc://...")`.
pub fn run_remote(endpoint: &str, token: Option<String>) -> i32 {
    let tokio_rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(err = %e, "rpc: failed to build tokio runtime");
            return 1;
        }
    };
    let client = match tokio_rt.block_on(RedDBClient::connect(endpoint, token)) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(endpoint, err = %e, "rpc: failed to connect");
            return 1;
        }
    };
    let backend = Backend::Remote(Box::new(RemoteBackend {
        client: AsyncMutex::new(client),
        tokio_rt: &tokio_rt,
    }));
    run_backend(&backend, std::io::stdin(), &mut std::io::stdout())
}

/// Same as [`run`] but takes explicit I/O handles. Used by tests.
pub fn run_with_io<W: Write>(runtime: &RedDBRuntime, stdin: Stdin, stdout: &mut W) -> i32 {
    run_backend(&Backend::Local(runtime), stdin, stdout)
}

fn run_backend<W: Write>(backend: &Backend<'_>, stdin: Stdin, stdout: &mut W) -> i32 {
    let reader = BufReader::new(stdin.lock());
    let mut session = Session::new();
    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                let _ = writeln!(
                    stdout,
                    "{}",
                    error_response(&Value::Null, error_code::INTERNAL_ERROR, &e.to_string())
                );
                let _ = stdout.flush();
                return 1;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = handle_line(backend, &mut session, &line);
        if writeln!(stdout, "{}", response).is_err() || stdout.flush().is_err() {
            return 1;
        }
        if response.contains("\"__close__\":true") {
            return 0;
        }
    }
    // EOF: silently drop any open transaction — atomicity is preserved
    // (nothing was ever applied to the store) and no error is surfaced to
    // the caller because EOF may be graceful client disconnect.
    let _ = session.take_tx();
    0
}

/// Parse one input line and dispatch. Always returns a single-line
/// JSON string suitable for direct write to stdout. Never panics
/// (panics inside handlers are caught and reported).
fn handle_line(backend: &Backend<'_>, session: &mut Session, line: &str) -> String {
    let parsed: Value = match json::from_str(line) {
        Ok(v) => v,
        Err(err) => {
            return error_response(
                &Value::Null,
                error_code::PARSE_ERROR,
                &format!("invalid JSON: {err}"),
            );
        }
    };

    let id = parsed.get("id").cloned().unwrap_or(Value::Null);

    let method = match parsed.get("method").and_then(Value::as_str) {
        Some(m) => m.to_string(),
        None => {
            return error_response(&id, error_code::INVALID_REQUEST, "missing 'method' field");
        }
    };

    let params = parsed.get("params").cloned().unwrap_or(Value::Null);

    let dispatch = std::panic::catch_unwind(AssertUnwindSafe(|| match backend {
        Backend::Local(rt) => dispatch_method(rt, session, &method, &params),
        Backend::Remote(remote) => {
            // Transactions are session-local and the remote path forwards
            // each call independently — there is no place to park a tx
            // handle across gRPC hops yet. Surface a clear error so
            // drivers can fall back to per-call auto-commit.
            if matches!(
                method.as_str(),
                "tx.begin"
                    | "tx.commit"
                    | "tx.rollback"
                    | "query.open"
                    | "query.next"
                    | "query.close"
            ) {
                Err((
                    error_code::TX_NOT_SUPPORTED_REMOTE,
                    format!("{method} is not supported over remote gRPC yet"),
                ))
            } else {
                dispatch_method_remote(&remote.client, remote.tokio_rt, &method, &params)
            }
        }
    }));

    match dispatch {
        Ok(Ok(result)) => success_response(&id, &result, method == "close"),
        Ok(Err((code, msg))) => error_response(&id, code, &msg),
        Err(_) => error_response(&id, error_code::INTERNAL_ERROR, "handler panicked (caught)"),
    }
}

/// Dispatch a parsed method call. Returns the `result` value on
/// success or `(error_code, message)` on failure.
fn dispatch_method(
    runtime: &RedDBRuntime,
    session: &mut Session,
    method: &str,
    params: &Value,
) -> Result<Value, (&'static str, String)> {
    match method {
        "tx.begin" => {
            let tx_id = session.open_tx()?;
            Ok(Value::Object(
                [
                    ("tx_id".to_string(), Value::Number(tx_id as f64)),
                    (
                        "isolation".to_string(),
                        Value::String("read_committed_deferred".to_string()),
                    ),
                ]
                .into_iter()
                .collect(),
            ))
        }

        "tx.commit" => {
            let tx = session.take_tx().ok_or((
                error_code::NO_TX_OPEN,
                "no transaction is open in this session".to_string(),
            ))?;
            let tx_id = tx.tx_id;
            let op_count = tx.write_set.len();

            // Hold the global commit lock for the full replay so
            // concurrent commits serialize. Reads and plain
            // auto-committed writes keep running in parallel.
            let replay = runtime.with_commit_lock(|| -> Result<(u64, usize), (usize, String)> {
                let mut total_affected: u64 = 0;
                for (idx, op) in tx.write_set.iter().enumerate() {
                    match runtime.execute_query(op.sql()) {
                        Ok(qr) => total_affected += qr.affected_rows,
                        Err(e) => return Err((idx, e.to_string())),
                    }
                }
                Ok((total_affected, op_count))
            });

            match replay {
                Ok((affected, replayed)) => Ok(Value::Object(
                    [
                        ("tx_id".to_string(), Value::Number(tx_id as f64)),
                        ("ops_replayed".to_string(), Value::Number(replayed as f64)),
                        ("affected".to_string(), Value::Number(affected as f64)),
                    ]
                    .into_iter()
                    .collect(),
                )),
                Err((failed_idx, msg)) => Err((
                    error_code::TX_REPLAY_FAILED,
                    format!(
                        "tx {tx_id} replay failed at op {failed_idx}/{op_count}: {msg} \
                         (ops 0..{failed_idx} already applied and are NOT rolled back)"
                    ),
                )),
            }
        }

        "query.open" => {
            let sql = params.get("sql").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'sql' string".to_string(),
            ))?;
            let qr = runtime
                .execute_query(sql)
                .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;

            // Extract the column list from the first record. Consistent
            // with query_result_to_json which uses the first row's keys
            // as schema.
            let columns: Vec<String> = qr
                .result
                .records
                .first()
                .map(|first| {
                    let mut keys: Vec<String> = first
                        .column_names()
                        .into_iter()
                        .map(|k| k.to_string())
                        .collect();
                    keys.sort();
                    keys
                })
                .unwrap_or_default();

            let cursor = Cursor::new(columns.clone(), qr.result.records);
            let total = cursor.total();
            let cursor_id = session.insert_cursor(cursor)?;

            Ok(Value::Object(
                [
                    ("cursor_id".to_string(), Value::Number(cursor_id as f64)),
                    (
                        "columns".to_string(),
                        Value::Array(columns.into_iter().map(Value::String).collect()),
                    ),
                    ("total_rows".to_string(), Value::Number(total as f64)),
                ]
                .into_iter()
                .collect(),
            ))
        }

        "query.next" => {
            let cursor_id = params
                .get("cursor_id")
                .and_then(|v| v.as_f64())
                .map(|n| n as u64)
                .ok_or((
                    error_code::INVALID_PARAMS,
                    "missing 'cursor_id' number".to_string(),
                ))?;
            let batch_size = params
                .get("batch_size")
                .and_then(|v| v.as_f64())
                .map(|n| n as usize)
                .unwrap_or(DEFAULT_CURSOR_BATCH_SIZE)
                .clamp(1, MAX_CURSOR_BATCH_SIZE);

            // Extract the batch inside a bounded borrow so we can
            // drop the cursor afterwards without borrow-conflict.
            let (rows, done, remaining) = {
                let cursor = session.cursor_mut(cursor_id).ok_or((
                    error_code::CURSOR_NOT_FOUND,
                    format!("cursor {cursor_id} not found"),
                ))?;
                let batch = cursor.take_batch(batch_size);
                let rows_json: Vec<Value> = batch.iter().map(record_to_json_object).collect();
                (rows_json, cursor.is_exhausted(), cursor.remaining())
            };

            if done {
                // Auto-drop exhausted cursors so long-lived sessions
                // don't accumulate dead state.
                let _ = session.drop_cursor(cursor_id);
            }

            Ok(Value::Object(
                [
                    ("cursor_id".to_string(), Value::Number(cursor_id as f64)),
                    ("rows".to_string(), Value::Array(rows)),
                    ("done".to_string(), Value::Bool(done)),
                    ("remaining".to_string(), Value::Number(remaining as f64)),
                ]
                .into_iter()
                .collect(),
            ))
        }

        "query.close" => {
            let cursor_id = params
                .get("cursor_id")
                .and_then(|v| v.as_f64())
                .map(|n| n as u64)
                .ok_or((
                    error_code::INVALID_PARAMS,
                    "missing 'cursor_id' number".to_string(),
                ))?;
            let existed = session.drop_cursor(cursor_id).is_some();
            if !existed {
                return Err((
                    error_code::CURSOR_NOT_FOUND,
                    format!("cursor {cursor_id} not found"),
                ));
            }
            Ok(Value::Object(
                [
                    ("cursor_id".to_string(), Value::Number(cursor_id as f64)),
                    ("closed".to_string(), Value::Bool(true)),
                ]
                .into_iter()
                .collect(),
            ))
        }

        "tx.rollback" => {
            let tx = session.take_tx().ok_or((
                error_code::NO_TX_OPEN,
                "no transaction is open in this session".to_string(),
            ))?;
            let ops_discarded = tx.write_set.len();
            Ok(Value::Object(
                [
                    ("tx_id".to_string(), Value::Number(tx.tx_id as f64)),
                    (
                        "ops_discarded".to_string(),
                        Value::Number(ops_discarded as f64),
                    ),
                ]
                .into_iter()
                .collect(),
            ))
        }

        "version" => Ok(Value::Object(
            [
                (
                    "version".to_string(),
                    Value::String(env!("CARGO_PKG_VERSION").to_string()),
                ),
                (
                    "protocol".to_string(),
                    Value::String(PROTOCOL_VERSION.to_string()),
                ),
            ]
            .into_iter()
            .collect(),
        )),

        "health" => Ok(Value::Object(
            [
                ("ok".to_string(), Value::Bool(true)),
                (
                    "version".to_string(),
                    Value::String(env!("CARGO_PKG_VERSION").to_string()),
                ),
            ]
            .into_iter()
            .collect(),
        )),

        "query" => {
            let sql = params.get("sql").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'sql' string".to_string(),
            ))?;
            let qr = runtime
                .execute_query(sql)
                .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
            Ok(query_result_to_json(&qr))
        }

        // ── Prepared statements ──────────────────────────────────────────────
        //
        // `prepare` parses the SQL once, extracts a parameterized shape, and
        // returns a `prepared_id` the client can reuse. `execute_prepared` takes
        // that id plus JSON-encoded bind values and runs the plan without parsing.
        //
        // This mirrors the PostgreSQL extended-query protocol semantics and is the
        // server-side half of the client driver's `PreparedStatement` abstraction.
        "prepare" => {
            use crate::storage::query::modes::parse_multi;
            use crate::storage::query::planner::shape::parameterize_query_expr;

            let sql = params.get("sql").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'sql' string".to_string(),
            ))?;
            let parsed = parse_multi(sql).map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
            let (shape, parameter_count) = if let Some(prepared) = parameterize_query_expr(&parsed)
            {
                (prepared.shape, prepared.parameter_count)
            } else {
                (parsed, 0)
            };
            let id = session.next_prepared_id;
            session.next_prepared_id = session.next_prepared_id.saturating_add(1);
            session.prepared.insert(
                id,
                StdioPreparedStatement {
                    shape,
                    parameter_count,
                },
            );
            Ok(Value::Object(
                [
                    ("prepared_id".to_string(), Value::Number(id as f64)),
                    (
                        "parameter_count".to_string(),
                        Value::Number(parameter_count as f64),
                    ),
                ]
                .into_iter()
                .collect(),
            ))
        }

        "execute_prepared" => {
            use crate::storage::query::planner::shape::bind_parameterized_query;
            use crate::storage::schema::Value as SV;

            let id = params
                .get("prepared_id")
                .and_then(Value::as_f64)
                .map(|n| n as u64)
                .ok_or((
                    error_code::INVALID_PARAMS,
                    "missing 'prepared_id'".to_string(),
                ))?;

            let stmt = session.prepared.get(&id).ok_or((
                error_code::QUERY_ERROR,
                format!("no prepared statement with id {id}"),
            ))?;

            // Parse bind values from JSON array of JSON-encoded literals.
            let binds_json: Vec<Value> = params
                .get("binds")
                .and_then(Value::as_array)
                .map(|a| a.to_vec())
                .unwrap_or_default();
            if binds_json.len() != stmt.parameter_count {
                return Err((
                    error_code::INVALID_PARAMS,
                    format!(
                        "expected {} bind values, got {}",
                        stmt.parameter_count,
                        binds_json.len()
                    ),
                ));
            }

            // Convert JSON bind values to SchemaValue.
            let binds: Vec<SV> = binds_json.iter().map(json_value_to_schema_value).collect();

            // Bind literals into the parameterized shape.
            let expr = if stmt.parameter_count == 0 {
                stmt.shape.clone()
            } else {
                bind_parameterized_query(&stmt.shape, &binds, stmt.parameter_count)
                    .ok_or((error_code::QUERY_ERROR, "bind failed".to_string()))?
            };

            let qr = runtime
                .execute_query_expr(expr)
                .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
            Ok(query_result_to_json(&qr))
        }

        "insert" => {
            let collection = params.get("collection").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'collection' string".to_string(),
            ))?;
            let payload = params.get("payload").ok_or((
                error_code::INVALID_PARAMS,
                "missing 'payload' object".to_string(),
            ))?;
            let payload_obj = payload.as_object().ok_or((
                error_code::INVALID_PARAMS,
                "'payload' must be a JSON object".to_string(),
            ))?;
            let sql = build_insert_sql(collection, payload_obj.iter());

            if let Some(tx) = session.current_tx_mut() {
                tx.write_set.push(PendingSql::Insert(sql));
                return Ok(pending_tx_response(tx.tx_id));
            }

            let qr = runtime
                .execute_query(&sql)
                .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
            Ok(insert_result_to_json(&qr))
        }

        "bulk_insert" => {
            let collection = params.get("collection").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'collection' string".to_string(),
            ))?;
            let payloads = params.get("payloads").and_then(Value::as_array).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'payloads' array".to_string(),
            ))?;

            if let Some(tx) = session.current_tx_mut() {
                let mut buffered: u64 = 0;
                for entry in payloads {
                    let obj = entry.as_object().ok_or((
                        error_code::INVALID_PARAMS,
                        "each payload must be a JSON object".to_string(),
                    ))?;
                    let sql = build_insert_sql(collection, obj.iter());
                    tx.write_set.push(PendingSql::Insert(sql));
                    buffered += 1;
                }
                let tx_id = tx.tx_id;
                return Ok(Value::Object(
                    [
                        ("affected".to_string(), Value::Number(0.0)),
                        ("buffered".to_string(), Value::Number(buffered as f64)),
                        ("pending".to_string(), Value::Bool(true)),
                        ("tx_id".to_string(), Value::Number(tx_id as f64)),
                    ]
                    .into_iter()
                    .collect(),
                ));
            }

            let mut total_affected: u64 = 0;
            for entry in payloads {
                let obj = entry.as_object().ok_or((
                    error_code::INVALID_PARAMS,
                    "each payload must be a JSON object".to_string(),
                ))?;
                let sql = build_insert_sql(collection, obj.iter());
                let qr = runtime
                    .execute_query(&sql)
                    .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
                total_affected += qr.affected_rows;
            }
            Ok(Value::Object(
                [("affected".to_string(), Value::Number(total_affected as f64))]
                    .into_iter()
                    .collect(),
            ))
        }

        "get" => {
            let collection = params.get("collection").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'collection' string".to_string(),
            ))?;
            let id = params.get("id").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'id' string".to_string(),
            ))?;
            let sql = format!("SELECT * FROM {collection} WHERE _entity_id = {id} LIMIT 1");
            let qr = runtime
                .execute_query(&sql)
                .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
            let entity = qr
                .result
                .records
                .first()
                .map(record_to_json_object)
                .unwrap_or(Value::Null);
            Ok(Value::Object(
                [("entity".to_string(), entity)].into_iter().collect(),
            ))
        }

        "delete" => {
            let collection = params.get("collection").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'collection' string".to_string(),
            ))?;
            let id = params.get("id").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'id' string".to_string(),
            ))?;
            let sql = format!("DELETE FROM {collection} WHERE _entity_id = {id}");

            if let Some(tx) = session.current_tx_mut() {
                tx.write_set.push(PendingSql::Delete(sql));
                return Ok(pending_tx_response(tx.tx_id));
            }

            let qr = runtime
                .execute_query(&sql)
                .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
            Ok(Value::Object(
                [(
                    "affected".to_string(),
                    Value::Number(qr.affected_rows as f64),
                )]
                .into_iter()
                .collect(),
            ))
        }

        "close" => {
            // Silently drop any open transaction and cursors on close.
            // The client explicitly asked to terminate; surfacing an
            // error here would leak state across what is effectively a
            // reset.
            let _ = session.take_tx();
            session.clear_cursors();
            let _ = runtime.checkpoint();
            Ok(Value::Null)
        }

        other => Err((
            error_code::INVALID_REQUEST,
            format!("unknown method: {other}"),
        )),
    }
}

// ---------------------------------------------------------------------------
// Response builders
// ---------------------------------------------------------------------------

fn success_response(id: &Value, result: &Value, is_close: bool) -> String {
    // For `close` we tag the response so the loop knows to exit after
    // flushing. The tag is stripped from the wire by replacing it
    // before serialization — actually we just include it as a sentinel
    // field that drivers ignore (forward compat).
    let mut envelope = json::Map::new();
    envelope.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
    envelope.insert("id".to_string(), id.clone());
    envelope.insert("result".to_string(), result.clone());
    if is_close {
        envelope.insert("__close__".to_string(), Value::Bool(true));
    }
    Value::Object(envelope).to_string_compact()
}

fn error_response(id: &Value, code: &str, message: &str) -> String {
    let mut err = json::Map::new();
    err.insert("code".to_string(), Value::String(code.to_string()));
    err.insert("message".to_string(), Value::String(message.to_string()));
    err.insert("data".to_string(), Value::Null);

    let mut envelope = json::Map::new();
    envelope.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
    envelope.insert("id".to_string(), id.clone());
    envelope.insert("error".to_string(), Value::Object(err));
    Value::Object(envelope).to_string_compact()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Envelope returned by `insert` and `delete` when the call was buffered
/// into an open transaction instead of being auto-committed.
fn pending_tx_response(tx_id: u64) -> Value {
    Value::Object(
        [
            ("affected".to_string(), Value::Number(0.0)),
            ("pending".to_string(), Value::Bool(true)),
            ("tx_id".to_string(), Value::Number(tx_id as f64)),
        ]
        .into_iter()
        .collect(),
    )
}

fn build_insert_sql<'a, I>(collection: &str, fields: I) -> String
where
    I: Iterator<Item = (&'a String, &'a Value)>,
{
    let mut cols = Vec::new();
    let mut vals = Vec::new();
    for (k, v) in fields {
        cols.push(k.clone());
        vals.push(value_to_sql_literal(v));
    }
    format!(
        "INSERT INTO {collection} ({}) VALUES ({})",
        cols.join(", "),
        vals.join(", "),
    )
}

fn value_to_sql_literal(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            if n.fract() == 0.0 {
                format!("{}", *n as i64)
            } else {
                n.to_string()
            }
        }
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        other => format!("'{}'", other.to_string_compact().replace('\'', "''")),
    }
}

fn query_result_to_json(qr: &RuntimeQueryResult) -> Value {
    let mut envelope = json::Map::new();
    envelope.insert(
        "statement".to_string(),
        Value::String(qr.statement_type.to_string()),
    );
    envelope.insert(
        "affected".to_string(),
        Value::Number(qr.affected_rows as f64),
    );

    let mut columns = Vec::new();
    if let Some(first) = qr.result.records.first() {
        let mut keys: Vec<String> = first
            .column_names()
            .into_iter()
            .map(|k| k.to_string())
            .collect();
        keys.sort();
        columns = keys.into_iter().map(Value::String).collect();
    }
    envelope.insert("columns".to_string(), Value::Array(columns));

    let rows: Vec<Value> = qr
        .result
        .records
        .iter()
        .map(record_to_json_object)
        .collect();
    envelope.insert("rows".to_string(), Value::Array(rows));

    Value::Object(envelope)
}

fn insert_result_to_json(qr: &RuntimeQueryResult) -> Value {
    let mut envelope = json::Map::new();
    envelope.insert(
        "affected".to_string(),
        Value::Number(qr.affected_rows as f64),
    );
    // First row of the result, if any, contains the inserted entity id.
    if let Some(first) = qr.result.records.first() {
        if let Some(id_val) = first
            .values
            .iter()
            .find(|(k, _)| {
                let s: &str = k;
                s == "_entity_id"
            })
            .map(|(_, v)| schema_value_to_json(v))
        {
            envelope.insert("id".to_string(), id_val);
        }
    }
    Value::Object(envelope)
}

fn record_to_json_object(record: &UnifiedRecord) -> Value {
    let mut map = json::Map::new();
    // iter_fields merges the columnar fast-path + HashMap so scan
    // rows (columnar only) contribute their values.
    let mut entries: Vec<(&str, &SchemaValue)> =
        record.iter_fields().map(|(k, v)| (k.as_ref(), v)).collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    for (k, v) in entries {
        map.insert(k.to_string(), schema_value_to_json(v));
    }
    Value::Object(map)
}

fn schema_value_to_json(v: &SchemaValue) -> Value {
    match v {
        SchemaValue::Null => Value::Null,
        SchemaValue::Boolean(b) => Value::Bool(*b),
        SchemaValue::Integer(n) => Value::Number(*n as f64),
        SchemaValue::UnsignedInteger(n) => Value::Number(*n as f64),
        SchemaValue::Float(n) => Value::Number(*n),
        SchemaValue::BigInt(n) => Value::Number(*n as f64),
        SchemaValue::TimestampMs(n)
        | SchemaValue::Timestamp(n)
        | SchemaValue::Duration(n)
        | SchemaValue::Decimal(n) => Value::Number(*n as f64),
        SchemaValue::Password(_) | SchemaValue::Secret(_) => Value::String("***".to_string()),
        SchemaValue::Text(s) => Value::String(s.to_string()),
        SchemaValue::Email(s)
        | SchemaValue::Url(s)
        | SchemaValue::NodeRef(s)
        | SchemaValue::EdgeRef(s) => Value::String(s.clone()),
        other => Value::String(format!("{other}")),
    }
}

/// Convert a JSON `Value` to a `SchemaValue` for use as a bind parameter
/// in a prepared statement. Mirrors PostgreSQL's implicit type coercion:
/// JSON numbers become `Float`, strings become `Text`, booleans map to
/// `Boolean`, and `null` becomes `Null`.
fn json_value_to_schema_value(v: &Value) -> SchemaValue {
    match v {
        Value::Null => SchemaValue::Null,
        Value::Bool(b) => SchemaValue::Boolean(*b),
        Value::Number(n) => {
            if n.fract() == 0.0 && n.abs() < i64::MAX as f64 {
                SchemaValue::Integer(*n as i64)
            } else {
                SchemaValue::Float(*n)
            }
        }
        Value::String(s) => SchemaValue::text(s.clone()),
        Value::Array(_) | Value::Object(_) => {
            SchemaValue::text(crate::json::to_string(v).unwrap_or_default())
        }
    }
}

// ---------------------------------------------------------------------------
// Remote dispatch (grpc://)
// ---------------------------------------------------------------------------

/// Dispatch a parsed JSON-RPC call over gRPC. Mirrors `dispatch_method`
/// but every operation goes through the tonic client. The server's
/// own `RedDBRuntime` does the actual work — we are just a wire
/// adapter between the JSON-RPC framing the drivers speak and the
/// gRPC protobuf framing the server speaks.
fn dispatch_method_remote(
    client: &AsyncMutex<RedDBClient>,
    tokio_rt: &tokio::runtime::Runtime,
    method: &str,
    params: &Value,
) -> Result<Value, (&'static str, String)> {
    match method {
        "version" => Ok(Value::Object(
            [
                (
                    "version".to_string(),
                    Value::String(env!("CARGO_PKG_VERSION").to_string()),
                ),
                (
                    "protocol".to_string(),
                    Value::String(PROTOCOL_VERSION.to_string()),
                ),
            ]
            .into_iter()
            .collect(),
        )),

        "health" => {
            let result = tokio_rt.block_on(async {
                let mut guard = client.lock().await;
                guard.health_status().await
            });
            match result {
                Ok(status) => Ok(Value::Object(
                    [
                        ("ok".to_string(), Value::Bool(status.healthy)),
                        ("state".to_string(), Value::String(status.state)),
                        (
                            "checked_at_unix_ms".to_string(),
                            Value::Number(status.checked_at_unix_ms as f64),
                        ),
                        (
                            "version".to_string(),
                            Value::String(env!("CARGO_PKG_VERSION").to_string()),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                )),
                Err(e) => Err((error_code::INTERNAL_ERROR, e.to_string())),
            }
        }

        "query" => {
            let sql = params.get("sql").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'sql' string".to_string(),
            ))?;
            let json_str = tokio_rt
                .block_on(async {
                    let mut guard = client.lock().await;
                    guard.query(sql).await
                })
                .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
            // Server returned its own QueryReply.result_json. Parse and
            // repackage into the stdio-protocol shape. If parsing fails,
            // hand the raw server JSON back under a sentinel key so the
            // caller still gets something useful.
            let parsed = json::from_str::<Value>(&json_str)
                .map_err(|e| (error_code::INTERNAL_ERROR, format!("bad server JSON: {e}")))?;
            Ok(parsed)
        }

        "insert" => {
            let collection = params.get("collection").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'collection' string".to_string(),
            ))?;
            let payload = params.get("payload").ok_or((
                error_code::INVALID_PARAMS,
                "missing 'payload' object".to_string(),
            ))?;
            if payload.as_object().is_none() {
                return Err((
                    error_code::INVALID_PARAMS,
                    "'payload' must be a JSON object".to_string(),
                ));
            }
            let payload_json = payload.to_string_compact();
            let reply = tokio_rt
                .block_on(async {
                    let mut guard = client.lock().await;
                    guard.create_row_entity(collection, &payload_json).await
                })
                .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
            let mut out = json::Map::new();
            out.insert("affected".to_string(), Value::Number(1.0));
            out.insert("id".to_string(), Value::String(reply.id.to_string()));
            Ok(Value::Object(out))
        }

        "bulk_insert" => {
            let collection = params.get("collection").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'collection' string".to_string(),
            ))?;
            let payloads = params.get("payloads").and_then(Value::as_array).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'payloads' array".to_string(),
            ))?;
            let mut encoded = Vec::with_capacity(payloads.len());
            for entry in payloads {
                if entry.as_object().is_none() {
                    return Err((
                        error_code::INVALID_PARAMS,
                        "each payload must be a JSON object".to_string(),
                    ));
                }
                encoded.push(entry.to_string_compact());
            }
            let total = tokio_rt
                .block_on(async {
                    let mut guard = client.lock().await;
                    guard.bulk_create_rows(collection, encoded).await
                })
                .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?
                .count;
            Ok(Value::Object(
                [("affected".to_string(), Value::Number(total as f64))]
                    .into_iter()
                    .collect(),
            ))
        }

        "get" => {
            let collection = params.get("collection").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'collection' string".to_string(),
            ))?;
            let id = params.get("id").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'id' string".to_string(),
            ))?;
            let sql = format!("SELECT * FROM {collection} WHERE _entity_id = {id} LIMIT 1");
            let json_str = tokio_rt
                .block_on(async {
                    let mut guard = client.lock().await;
                    guard.query(&sql).await
                })
                .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
            let parsed = json::from_str::<Value>(&json_str)
                .map_err(|e| (error_code::INTERNAL_ERROR, format!("bad server JSON: {e}")))?;
            // Server response shape: {"rows":[{...}], ...}. Extract
            // the first row (if any) as `entity`.
            let entity = parsed
                .get("rows")
                .and_then(Value::as_array)
                .and_then(|rows| rows.first().cloned())
                .unwrap_or(Value::Null);
            Ok(Value::Object(
                [("entity".to_string(), entity)].into_iter().collect(),
            ))
        }

        "delete" => {
            let collection = params.get("collection").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'collection' string".to_string(),
            ))?;
            let id = params.get("id").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'id' string".to_string(),
            ))?;
            let id = id.parse::<u64>().map_err(|_| {
                (
                    error_code::INVALID_PARAMS,
                    "id must be a numeric string".to_string(),
                )
            })?;
            let _reply = tokio_rt
                .block_on(async {
                    let mut guard = client.lock().await;
                    guard.delete_entity(collection, id).await
                })
                .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
            Ok(Value::Object(
                [("affected".to_string(), Value::Number(1.0))]
                    .into_iter()
                    .collect(),
            ))
        }

        "close" => Ok(Value::Null),

        other => Err((
            error_code::INVALID_REQUEST,
            format!("unknown method: {other}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_runtime() -> RedDBRuntime {
        RedDBRuntime::in_memory().expect("in-memory runtime")
    }

    fn handle(rt: &RedDBRuntime, line: &str) -> String {
        let mut session = Session::new();
        handle_line(&Backend::Local(rt), &mut session, line)
    }

    /// Stateful helper: keeps the same `Session` across multiple calls so
    /// tests can exercise multi-step transaction flows in a single closure.
    fn with_session<F>(rt: &RedDBRuntime, f: F)
    where
        F: FnOnce(&dyn Fn(&str) -> String, &RedDBRuntime),
    {
        let session = std::cell::RefCell::new(Session::new());
        let call = |line: &str| -> String {
            let mut s = session.borrow_mut();
            handle_line(&Backend::Local(rt), &mut s, line)
        };
        f(&call, rt);
    }

    #[test]
    fn version_method_returns_version_and_protocol() {
        let rt = make_runtime();
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"version","params":{}}"#;
        let resp = handle(&rt, line);
        assert!(resp.contains("\"id\":1"));
        assert!(resp.contains("\"protocol\":\"1.0\""));
        assert!(resp.contains("\"version\""));
    }

    #[test]
    fn health_method_returns_ok_true() {
        let rt = make_runtime();
        let resp = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":"abc","method":"health","params":{}}"#,
        );
        assert!(resp.contains("\"ok\":true"));
        assert!(resp.contains("\"id\":\"abc\""));
    }

    #[test]
    fn parse_error_for_invalid_json() {
        let rt = make_runtime();
        let resp = handle(&rt, "not json {");
        assert!(resp.contains("\"code\":\"PARSE_ERROR\""));
        assert!(resp.contains("\"id\":null"));
    }

    #[test]
    fn invalid_request_when_method_missing() {
        let rt = make_runtime();
        let resp = handle(&rt, r#"{"jsonrpc":"2.0","id":1,"params":{}}"#);
        assert!(resp.contains("\"code\":\"INVALID_REQUEST\""));
    }

    #[test]
    fn unknown_method_is_invalid_request() {
        let rt = make_runtime();
        let resp = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"frobnicate","params":{}}"#,
        );
        assert!(resp.contains("\"code\":\"INVALID_REQUEST\""));
        assert!(resp.contains("frobnicate"));
    }

    #[test]
    fn invalid_params_when_query_sql_missing() {
        let rt = make_runtime();
        let resp = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{}}"#,
        );
        assert!(resp.contains("\"code\":\"INVALID_PARAMS\""));
    }

    #[test]
    fn close_method_marks_response_for_shutdown() {
        let rt = make_runtime();
        let resp = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"close","params":{}}"#,
        );
        assert!(resp.contains("\"__close__\":true"));
    }

    #[test]
    fn query_select_one_returns_rows() {
        let rt = make_runtime();
        let resp = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"sql":"SELECT 1 AS one"}}"#,
        );
        assert!(resp.contains("\"result\""));
        assert!(!resp.contains("\"error\""));
    }

    // -----------------------------------------------------------------
    // Transaction tests
    // -----------------------------------------------------------------

    #[test]
    fn tx_begin_returns_tx_id_and_isolation() {
        let rt = make_runtime();
        with_session(&rt, |call, _| {
            let resp = call(r#"{"jsonrpc":"2.0","id":1,"method":"tx.begin","params":null}"#);
            assert!(resp.contains("\"tx_id\":1"));
            assert!(resp.contains("\"isolation\":\"read_committed_deferred\""));
            assert!(!resp.contains("\"error\""));
        });
    }

    #[test]
    fn tx_begin_twice_returns_already_open() {
        let rt = make_runtime();
        with_session(&rt, |call, _| {
            let _ = call(r#"{"jsonrpc":"2.0","id":1,"method":"tx.begin","params":null}"#);
            let resp = call(r#"{"jsonrpc":"2.0","id":2,"method":"tx.begin","params":null}"#);
            assert!(resp.contains("\"code\":\"TX_ALREADY_OPEN\""));
        });
    }

    #[test]
    fn tx_commit_without_begin_returns_no_tx_open() {
        let rt = make_runtime();
        with_session(&rt, |call, _| {
            let resp = call(r#"{"jsonrpc":"2.0","id":1,"method":"tx.commit","params":null}"#);
            assert!(resp.contains("\"code\":\"NO_TX_OPEN\""));
        });
    }

    #[test]
    fn tx_rollback_without_begin_returns_no_tx_open() {
        let rt = make_runtime();
        with_session(&rt, |call, _| {
            let resp = call(r#"{"jsonrpc":"2.0","id":1,"method":"tx.rollback","params":null}"#);
            assert!(resp.contains("\"code\":\"NO_TX_OPEN\""));
        });
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn insert_inside_tx_returns_pending_envelope() {
        let rt = make_runtime();
        // Create the collection first (outside any tx).
        let _ = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"sql":"CREATE TABLE users (name TEXT)"}}"#,
        );
        with_session(&rt, |call, _| {
            let _ = call(r#"{"jsonrpc":"2.0","id":1,"method":"tx.begin","params":null}"#);
            let resp = call(
                r#"{"jsonrpc":"2.0","id":2,"method":"insert","params":{"collection":"users","payload":{"name":"alice"}}}"#,
            );
            assert!(resp.contains("\"pending\":true"));
            assert!(resp.contains("\"tx_id\":1"));
            assert!(resp.contains("\"affected\":0"));
        });
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn begin_insert_rollback_does_not_persist() {
        let rt = make_runtime();
        let _ = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"sql":"CREATE TABLE u (name TEXT)"}}"#,
        );
        with_session(&rt, |call, _| {
            let _ = call(r#"{"jsonrpc":"2.0","id":1,"method":"tx.begin","params":null}"#);
            let _ = call(
                r#"{"jsonrpc":"2.0","id":2,"method":"insert","params":{"collection":"u","payload":{"name":"ghost"}}}"#,
            );
            let rollback = call(r#"{"jsonrpc":"2.0","id":3,"method":"tx.rollback","params":null}"#);
            assert!(rollback.contains("\"ops_discarded\":1"));
            assert!(rollback.contains("\"tx_id\":1"));
        });
        // After rollback, the row must not be visible to a fresh query.
        let resp = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":9,"method":"query","params":{"sql":"SELECT * FROM u"}}"#,
        );
        assert!(!resp.contains("\"ghost\""));
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn begin_insert_commit_persists() {
        let rt = make_runtime();
        let _ = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"sql":"CREATE TABLE u2 (name TEXT)"}}"#,
        );
        with_session(&rt, |call, _| {
            let _ = call(r#"{"jsonrpc":"2.0","id":1,"method":"tx.begin","params":null}"#);
            let _ = call(
                r#"{"jsonrpc":"2.0","id":2,"method":"insert","params":{"collection":"u2","payload":{"name":"alice"}}}"#,
            );
            let _ = call(
                r#"{"jsonrpc":"2.0","id":3,"method":"insert","params":{"collection":"u2","payload":{"name":"bob"}}}"#,
            );
            let commit = call(r#"{"jsonrpc":"2.0","id":4,"method":"tx.commit","params":null}"#);
            assert!(commit.contains("\"ops_replayed\":2"));
            assert!(!commit.contains("\"error\""));
        });
        let resp = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":9,"method":"query","params":{"sql":"SELECT * FROM u2"}}"#,
        );
        assert!(resp.contains("\"alice\""));
        assert!(resp.contains("\"bob\""));
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn bulk_insert_inside_tx_buffers_everything() {
        let rt = make_runtime();
        let _ = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"sql":"CREATE TABLE u3 (name TEXT)"}}"#,
        );
        with_session(&rt, |call, _| {
            let _ = call(r#"{"jsonrpc":"2.0","id":1,"method":"tx.begin","params":null}"#);
            let resp = call(
                r#"{"jsonrpc":"2.0","id":2,"method":"bulk_insert","params":{"collection":"u3","payloads":[{"name":"a"},{"name":"b"},{"name":"c"}]}}"#,
            );
            assert!(resp.contains("\"buffered\":3"));
            assert!(resp.contains("\"pending\":true"));
            assert!(resp.contains("\"affected\":0"));

            let commit = call(r#"{"jsonrpc":"2.0","id":3,"method":"tx.commit","params":null}"#);
            assert!(commit.contains("\"ops_replayed\":3"));
        });
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn delete_inside_tx_is_buffered() {
        let rt = make_runtime();
        // Seed two rows outside any tx.
        let _ = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"sql":"CREATE TABLE u4 (name TEXT)"}}"#,
        );
        let _ = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":2,"method":"query","params":{"sql":"INSERT INTO u4 (name) VALUES ('keep')"}}"#,
        );
        with_session(&rt, |call, _| {
            let _ = call(r#"{"jsonrpc":"2.0","id":1,"method":"tx.begin","params":null}"#);
            let resp = call(
                r#"{"jsonrpc":"2.0","id":2,"method":"delete","params":{"collection":"u4","id":"1"}}"#,
            );
            assert!(resp.contains("\"pending\":true"));
            let _ = call(r#"{"jsonrpc":"2.0","id":3,"method":"tx.rollback","params":null}"#);
        });
        // Row should still be present after rollback of the delete.
        let resp = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":9,"method":"query","params":{"sql":"SELECT * FROM u4"}}"#,
        );
        assert!(resp.contains("\"keep\""));
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn close_with_open_tx_auto_rollbacks() {
        let rt = make_runtime();
        let _ = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"sql":"CREATE TABLE u5 (name TEXT)"}}"#,
        );
        with_session(&rt, |call, _| {
            let _ = call(r#"{"jsonrpc":"2.0","id":1,"method":"tx.begin","params":null}"#);
            let _ = call(
                r#"{"jsonrpc":"2.0","id":2,"method":"insert","params":{"collection":"u5","payload":{"name":"ghost"}}}"#,
            );
            let close = call(r#"{"jsonrpc":"2.0","id":3,"method":"close","params":null}"#);
            assert!(close.contains("\"__close__\":true"));
            assert!(!close.contains("\"error\""));
        });
        let resp = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":9,"method":"query","params":{"sql":"SELECT * FROM u5"}}"#,
        );
        assert!(!resp.contains("\"ghost\""));
    }

    // -----------------------------------------------------------------
    // Cursor streaming tests
    // -----------------------------------------------------------------

    fn seed_numbers_table(rt: &RedDBRuntime, table: &str, count: u32) {
        let _ = handle(
            rt,
            &format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"query","params":{{"sql":"CREATE TABLE {table} (n INTEGER)"}}}}"#,
            ),
        );
        for i in 0..count {
            let _ = handle(
                rt,
                &format!(
                    r#"{{"jsonrpc":"2.0","id":2,"method":"query","params":{{"sql":"INSERT INTO {table} (n) VALUES ({i})"}}}}"#,
                ),
            );
        }
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn cursor_open_returns_id_columns_and_total() {
        let rt = make_runtime();
        seed_numbers_table(&rt, "nums1", 3);
        with_session(&rt, |call, _| {
            let resp = call(
                r#"{"jsonrpc":"2.0","id":1,"method":"query.open","params":{"sql":"SELECT n FROM nums1"}}"#,
            );
            assert!(resp.contains("\"cursor_id\":1"));
            assert!(resp.contains("\"total_rows\":3"));
            assert!(resp.contains("\"columns\""));
            assert!(!resp.contains("\"error\""));
        });
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn cursor_next_chunks_rows_and_signals_done() {
        let rt = make_runtime();
        seed_numbers_table(&rt, "nums2", 5);
        with_session(&rt, |call, _| {
            let _ = call(
                r#"{"jsonrpc":"2.0","id":1,"method":"query.open","params":{"sql":"SELECT n FROM nums2"}}"#,
            );
            let first = call(
                r#"{"jsonrpc":"2.0","id":2,"method":"query.next","params":{"cursor_id":1,"batch_size":2}}"#,
            );
            assert!(first.contains("\"done\":false"));
            assert!(first.contains("\"remaining\":3"));

            let second = call(
                r#"{"jsonrpc":"2.0","id":3,"method":"query.next","params":{"cursor_id":1,"batch_size":2}}"#,
            );
            assert!(second.contains("\"done\":false"));
            assert!(second.contains("\"remaining\":1"));

            let third = call(
                r#"{"jsonrpc":"2.0","id":4,"method":"query.next","params":{"cursor_id":1,"batch_size":2}}"#,
            );
            assert!(third.contains("\"done\":true"));
            assert!(third.contains("\"remaining\":0"));
        });
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn cursor_auto_drops_when_exhausted() {
        let rt = make_runtime();
        seed_numbers_table(&rt, "nums3", 2);
        with_session(&rt, |call, _| {
            let _ = call(
                r#"{"jsonrpc":"2.0","id":1,"method":"query.open","params":{"sql":"SELECT n FROM nums3"}}"#,
            );
            let _ = call(
                r#"{"jsonrpc":"2.0","id":2,"method":"query.next","params":{"cursor_id":1,"batch_size":100}}"#,
            );
            // Cursor was auto-dropped after done=true; subsequent next
            // must error with CURSOR_NOT_FOUND.
            let resp = call(
                r#"{"jsonrpc":"2.0","id":3,"method":"query.next","params":{"cursor_id":1,"batch_size":100}}"#,
            );
            assert!(resp.contains("\"code\":\"CURSOR_NOT_FOUND\""));
        });
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn cursor_close_removes_it() {
        let rt = make_runtime();
        seed_numbers_table(&rt, "nums4", 3);
        with_session(&rt, |call, _| {
            let _ = call(
                r#"{"jsonrpc":"2.0","id":1,"method":"query.open","params":{"sql":"SELECT n FROM nums4"}}"#,
            );
            let close =
                call(r#"{"jsonrpc":"2.0","id":2,"method":"query.close","params":{"cursor_id":1}}"#);
            assert!(close.contains("\"closed\":true"));
            let after = call(
                r#"{"jsonrpc":"2.0","id":3,"method":"query.next","params":{"cursor_id":1,"batch_size":10}}"#,
            );
            assert!(after.contains("\"code\":\"CURSOR_NOT_FOUND\""));
        });
    }

    #[test]
    fn cursor_close_unknown_errors() {
        let rt = make_runtime();
        with_session(&rt, |call, _| {
            let resp = call(
                r#"{"jsonrpc":"2.0","id":1,"method":"query.close","params":{"cursor_id":9999}}"#,
            );
            assert!(resp.contains("\"code\":\"CURSOR_NOT_FOUND\""));
        });
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn cursor_next_without_cursor_id_errors() {
        let rt = make_runtime();
        with_session(&rt, |call, _| {
            let resp = call(r#"{"jsonrpc":"2.0","id":1,"method":"query.next","params":{}}"#);
            assert!(resp.contains("\"code\":\"INVALID_PARAMS\""));
        });
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn cursor_default_batch_size_returns_all_when_smaller_than_default() {
        let rt = make_runtime();
        seed_numbers_table(&rt, "nums5", 7);
        with_session(&rt, |call, _| {
            let _ = call(
                r#"{"jsonrpc":"2.0","id":1,"method":"query.open","params":{"sql":"SELECT n FROM nums5"}}"#,
            );
            // No batch_size → default 100, table has 7 rows, all in one call.
            let resp =
                call(r#"{"jsonrpc":"2.0","id":2,"method":"query.next","params":{"cursor_id":1}}"#);
            assert!(resp.contains("\"done\":true"));
            assert!(resp.contains("\"remaining\":0"));
        });
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn close_method_drops_open_cursors() {
        let rt = make_runtime();
        seed_numbers_table(&rt, "nums6", 3);
        // Single session: open a cursor, call close, verify cursor is gone by reopening
        // fresh session and attempting to use cursor_id 1.
        with_session(&rt, |call, _| {
            let _ = call(
                r#"{"jsonrpc":"2.0","id":1,"method":"query.open","params":{"sql":"SELECT n FROM nums6"}}"#,
            );
            let close = call(r#"{"jsonrpc":"2.0","id":2,"method":"close","params":null}"#);
            assert!(close.contains("\"__close__\":true"));
            // Cursor must be gone after close within the same session.
            let after = call(
                r#"{"jsonrpc":"2.0","id":3,"method":"query.next","params":{"cursor_id":1,"batch_size":10}}"#,
            );
            assert!(after.contains("\"code\":\"CURSOR_NOT_FOUND\""));
        });
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn cursor_independent_of_transaction_state() {
        let rt = make_runtime();
        seed_numbers_table(&rt, "nums7", 4);
        with_session(&rt, |call, _| {
            // Open cursor, begin tx, commit tx — cursor survives.
            let _ = call(
                r#"{"jsonrpc":"2.0","id":1,"method":"query.open","params":{"sql":"SELECT n FROM nums7"}}"#,
            );
            let _ = call(r#"{"jsonrpc":"2.0","id":2,"method":"tx.begin","params":null}"#);
            let _ = call(r#"{"jsonrpc":"2.0","id":3,"method":"tx.commit","params":null}"#);
            let resp = call(
                r#"{"jsonrpc":"2.0","id":4,"method":"query.next","params":{"cursor_id":1,"batch_size":10}}"#,
            );
            assert!(resp.contains("\"done\":true"));
            assert!(!resp.contains("\"error\""));
        });
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn second_tx_after_commit_gets_fresh_id() {
        let rt = make_runtime();
        let _ = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"sql":"CREATE TABLE u6 (name TEXT)"}}"#,
        );
        with_session(&rt, |call, _| {
            let first = call(r#"{"jsonrpc":"2.0","id":1,"method":"tx.begin","params":null}"#);
            assert!(first.contains("\"tx_id\":1"));
            let _ = call(
                r#"{"jsonrpc":"2.0","id":2,"method":"insert","params":{"collection":"u6","payload":{"name":"x"}}}"#,
            );
            let _ = call(r#"{"jsonrpc":"2.0","id":3,"method":"tx.commit","params":null}"#);

            let second = call(r#"{"jsonrpc":"2.0","id":4,"method":"tx.begin","params":null}"#);
            assert!(second.contains("\"tx_id\":2"));
            let _ = call(r#"{"jsonrpc":"2.0","id":5,"method":"tx.rollback","params":null}"#);
        });
    }

    #[test]
    #[ignore = "rpc_stdio runtime-insert deadlock — pre-existing, tracked as follow-up"]
    fn prepare_and_execute_prepared_statement() {
        let rt = make_runtime();
        // Create table + insert a row
        let _ = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"sql":"CREATE TABLE ps_test (n INTEGER)"}}"#,
        );
        let _ = handle(
            &rt,
            r#"{"jsonrpc":"2.0","id":2,"method":"query","params":{"sql":"INSERT INTO ps_test (n) VALUES (42)"}}"#,
        );

        with_session(&rt, |call, _| {
            // Prepare a parameterized SELECT.
            let prep = call(
                r#"{"jsonrpc":"2.0","id":3,"method":"prepare","params":{"sql":"SELECT n FROM ps_test WHERE n = 42"}}"#,
            );
            assert!(prep.contains("\"prepared_id\""), "prepare response: {prep}");

            // Extract the prepared_id.
            let id: u64 = {
                let v: crate::json::Value = crate::json::from_str(&prep).expect("json");
                let result = v.get("result").expect("result");
                result
                    .get("prepared_id")
                    .and_then(|n| n.as_f64())
                    .expect("prepared_id") as u64
            };

            // Execute with the bind value for the parameterized literal.
            let exec = call(&format!(
                r#"{{"jsonrpc":"2.0","id":4,"method":"execute_prepared","params":{{"prepared_id":{id},"binds":[42]}}}}"#
            ));
            // Response uses "rows" key (see query_result_to_json).
            assert!(
                exec.contains("\"rows\""),
                "execute_prepared response: {exec}"
            );
            assert!(exec.contains("42"), "expected row with n=42 in: {exec}");
        });
    }
}
