//! PostgreSQL v3 wire protocol message framing (Phase 3.1 PG parity).
//!
//! Implements the bits of the PG v3 protocol RedDB needs for simple
//! query support: startup negotiation, cleartext password authentication, the
//! simple query flow (`Q` → `T`/`D`*/`C`/`Z`), and error reporting.
//!
//! The full PG reference lives at:
//! <https://www.postgresql.org/docs/current/protocol-message-formats.html>
//!
//! # Frame format (v3)
//!
//! After the startup message, every frame is:
//! ```text
//! [u8 type] [i32 length (includes itself)] [payload]
//! ```
//! Frames are big-endian. We use `tokio::io::AsyncRead/Write` so the
//! listener can plug into the same task model as the existing wire
//! binary protocol.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Protocol version constant: 3.0 → 196608 (major<<16 | minor).
pub const PG_PROTOCOL_V3: u32 = 3 << 16;

/// Special startup-phase requests that share the StartupMessage length
/// header. The PG reference calls out three: SSLRequest (80877103),
/// GSSENCRequest (80877104), CancelRequest (80877102).
pub const PG_SSL_REQUEST: u32 = 80877103;
pub const PG_GSSENC_REQUEST: u32 = 80877104;
pub const PG_CANCEL_REQUEST: u32 = 80877102;

/// Error type surfaced by the framing layer. Wraps IO errors plus
/// structural validation failures (bad message tag, truncated frame).
#[derive(Debug)]
pub enum PgWireError {
    Io(io::Error),
    Protocol(String),
    /// Client closed the connection cleanly (EOF before a frame).
    Eof,
}

impl From<io::Error> for PgWireError {
    fn from(err: io::Error) -> Self {
        if err.kind() == io::ErrorKind::UnexpectedEof {
            PgWireError::Eof
        } else {
            PgWireError::Io(err)
        }
    }
}

impl std::fmt::Display for PgWireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PgWireError::Io(e) => write!(f, "pg wire io: {e}"),
            PgWireError::Protocol(m) => write!(f, "pg wire protocol: {m}"),
            PgWireError::Eof => write!(f, "pg wire eof"),
        }
    }
}

impl std::error::Error for PgWireError {}

/// Frontend (client → server) messages we parse.
#[derive(Debug, Clone)]
pub enum FrontendMessage {
    /// Pre-handshake StartupMessage payload (parameters map).
    Startup(StartupParams),
    /// SSL handshake request — we reject with 'N' (not supported).
    SslRequest,
    /// GSSAPI encryption request — we reject with 'N'.
    GssEncRequest,
    /// `Q` — simple query.
    Query(String),
    /// `P` — Parse: name a prepared statement and its SQL text.
    Parse(ParseMessage),
    /// `B` — Bind: attach concrete parameter bytes to a named statement.
    Bind(BindMessage),
    /// `D` — Describe a prepared statement or portal.
    Describe(DescribeMessage),
    /// `E` — Execute a bound portal.
    Execute(ExecuteMessage),
    /// `C` — Close a prepared statement or portal.
    Close(CloseMessage),
    /// `p` — password / SASL response.
    PasswordMessage(Vec<u8>),
    /// `X` — Terminate.
    Terminate,
    /// `H` — Flush. Send buffered results.
    Flush,
    /// `S` — Sync. End of extended query batch.
    Sync,
    /// Any other frame we don't implement yet; carries the raw tag for
    /// logging / ErrorResponse reply.
    Unknown { tag: u8, payload: Vec<u8> },
}

#[derive(Debug, Clone)]
pub struct ParseMessage {
    pub statement: String,
    pub query: String,
    pub param_type_oids: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct BindMessage {
    pub portal: String,
    pub statement: String,
    pub param_format_codes: Vec<i16>,
    pub params: Vec<Option<Vec<u8>>>,
    pub result_format_codes: Vec<i16>,
}

#[derive(Debug, Clone)]
pub struct DescribeMessage {
    pub target: DescribeTarget,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescribeTarget {
    Statement,
    Portal,
}

#[derive(Debug, Clone)]
pub struct ExecuteMessage {
    pub portal: String,
    pub max_rows: u32,
}

#[derive(Debug, Clone)]
pub struct CloseMessage {
    pub target: DescribeTarget,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct StartupParams {
    /// Key/value pairs from the startup message (user, database, etc.).
    pub params: Vec<(String, String)>,
}

impl StartupParams {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Backend (server → client) messages we emit.
#[derive(Debug, Clone)]
pub enum BackendMessage {
    /// `R` — AuthenticationOk (subtype 0).
    AuthenticationOk,
    /// `R` — AuthenticationCleartextPassword (subtype 3).
    AuthenticationCleartextPassword,
    /// `S` — ParameterStatus (server_version, client_encoding, ...).
    ParameterStatus { name: String, value: String },
    /// `K` — BackendKeyData (cancel key).
    BackendKeyData { pid: u32, key: u32 },
    /// `Z` — ReadyForQuery. Status: 'I' idle, 'T' in-txn, 'E' failed-txn.
    ReadyForQuery(TransactionStatus),
    /// `T` — RowDescription.
    RowDescription(Vec<ColumnDescriptor>),
    /// `D` — DataRow. Each field is `Some(bytes)` or `None` (NULL).
    DataRow(Vec<Option<Vec<u8>>>),
    /// `C` — CommandComplete (e.g. "SELECT 3", "INSERT 0 1").
    CommandComplete(String),
    /// `1` — ParseComplete.
    ParseComplete,
    /// `2` — BindComplete.
    BindComplete,
    /// `3` — CloseComplete.
    CloseComplete,
    /// `t` — ParameterDescription.
    ParameterDescription(Vec<u32>),
    /// `n` — NoData.
    NoData,
    /// `E` — ErrorResponse with severity + code + message.
    ErrorResponse {
        severity: String,
        code: String,
        message: String,
    },
    /// `N` — NoticeResponse (non-fatal).
    NoticeResponse { message: String },
    /// `I` — EmptyQueryResponse.
    EmptyQueryResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionStatus {
    /// Not inside a transaction.
    Idle,
    /// Inside a transaction block.
    InTransaction,
    /// Failed transaction, awaiting ROLLBACK.
    Failed,
}

impl TransactionStatus {
    pub fn as_byte(self) -> u8 {
        match self {
            TransactionStatus::Idle => b'I',
            TransactionStatus::InTransaction => b'T',
            TransactionStatus::Failed => b'E',
        }
    }
}

#[derive(Debug, Clone)]
pub struct ColumnDescriptor {
    pub name: String,
    /// Table OID (0 when not from a real table — common for computed columns).
    pub table_oid: u32,
    /// Column attribute number within the table (0 when synthetic).
    pub column_attr: i16,
    /// PG type OID (`pg_type.oid`).
    pub type_oid: u32,
    /// Fixed size of the data type, or -1 for variable length.
    pub type_size: i16,
    /// Type modifier (e.g. VARCHAR(n) → n+4). -1 when unused.
    pub type_mod: i32,
    /// Format code: 0 = text, 1 = binary. We always emit text in 3.1.
    pub format: i16,
}

// ────────────────────────────────────────────────────────────────────
// Frontend parsing
// ────────────────────────────────────────────────────────────────────

/// Read the initial StartupMessage (or SSL/GSS request). The startup
/// frame has no type byte — just a length prefix followed by the
/// payload. Returns either a decoded Startup/SSL/GSS message or an error.
pub async fn read_startup<R: AsyncRead + Unpin>(
    stream: &mut R,
) -> Result<FrontendMessage, PgWireError> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if !(8..=65536).contains(&len) {
        return Err(PgWireError::Protocol(format!(
            "startup length {len} out of range"
        )));
    }
    let body_len = (len as usize) - 4;
    let mut body = vec![0u8; body_len];
    stream.read_exact(&mut body).await?;
    if body_len < 4 {
        return Err(PgWireError::Protocol("startup payload too short".into()));
    }
    let version = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);

    match version {
        PG_SSL_REQUEST => Ok(FrontendMessage::SslRequest),
        PG_GSSENC_REQUEST => Ok(FrontendMessage::GssEncRequest),
        PG_PROTOCOL_V3 => {
            // Parameter map is a run of null-terminated strings terminated
            // by an empty string.
            let mut params: Vec<(String, String)> = Vec::new();
            let mut pos = 4usize;
            while pos < body_len {
                if body[pos] == 0 {
                    break;
                }
                let key = read_cstring(&body, &mut pos)?;
                if pos >= body_len {
                    return Err(PgWireError::Protocol(
                        "startup parameter missing value".into(),
                    ));
                }
                let value = read_cstring(&body, &mut pos)?;
                params.push((key, value));
            }
            Ok(FrontendMessage::Startup(StartupParams { params }))
        }
        // CancelRequest is sent on a fresh connection and doesn't produce
        // a response — surface as Unknown so caller can close.
        PG_CANCEL_REQUEST => Ok(FrontendMessage::Unknown {
            tag: b'K',
            payload: body,
        }),
        _ => Err(PgWireError::Protocol(format!(
            "unsupported protocol version {version}"
        ))),
    }
}

/// Read a regular tagged frame after the startup handshake.
pub async fn read_frame<R: AsyncRead + Unpin>(
    stream: &mut R,
) -> Result<FrontendMessage, PgWireError> {
    let mut tag_buf = [0u8; 1];
    match stream.read_exact(&mut tag_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(PgWireError::Eof),
        Err(e) => return Err(PgWireError::Io(e)),
    }
    let tag = tag_buf[0];

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if !(4..=1_048_576).contains(&len) {
        return Err(PgWireError::Protocol(format!(
            "frame length {len} out of bounds"
        )));
    }
    let payload_len = (len as usize) - 4;
    let mut payload = vec![0u8; payload_len];
    stream.read_exact(&mut payload).await?;

    Ok(match tag {
        b'Q' => {
            // Null-terminated SQL string.
            let mut pos = 0;
            let query = read_cstring(&payload, &mut pos)?;
            FrontendMessage::Query(query)
        }
        b'P' => FrontendMessage::Parse(parse_parse_message(&payload)?),
        b'B' => FrontendMessage::Bind(parse_bind_message(&payload)?),
        b'D' => FrontendMessage::Describe(parse_describe_message(&payload)?),
        b'E' => FrontendMessage::Execute(parse_execute_message(&payload)?),
        b'C' => FrontendMessage::Close(parse_close_message(&payload)?),
        b'p' => FrontendMessage::PasswordMessage(payload),
        b'X' => FrontendMessage::Terminate,
        b'H' => FrontendMessage::Flush,
        b'S' => FrontendMessage::Sync,
        other => FrontendMessage::Unknown {
            tag: other,
            payload,
        },
    })
}

// ────────────────────────────────────────────────────────────────────
// Backend emission
// ────────────────────────────────────────────────────────────────────

/// Emit a raw byte (used for the SSL/GSS negotiation response: 'N'
/// meaning "not supported, continue in plaintext").
pub async fn write_raw_byte<W: AsyncWrite + Unpin>(
    stream: &mut W,
    byte: u8,
) -> Result<(), PgWireError> {
    stream.write_all(&[byte]).await?;
    Ok(())
}

/// Serialize + send a backend message.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    stream: &mut W,
    msg: &BackendMessage,
) -> Result<(), PgWireError> {
    let (tag, payload) = encode_backend(msg);
    // Length includes the length field itself (4 bytes) + payload.
    let length = (payload.len() + 4) as u32;
    stream.write_all(&[tag]).await?;
    stream.write_all(&length.to_be_bytes()).await?;
    stream.write_all(&payload).await?;
    Ok(())
}

/// F-02 (audit doc, 2026-05-06):
/// PG3 wire encodes user-controlled bytes as `tag|value|NUL` C-strings
/// in `ErrorResponse`, `NoticeResponse`, `CommandComplete`,
/// `RowDescription` column names, and `ParameterStatus`. An embedded
/// NUL in a user-supplied message field truncates the C-string and
/// lets an attacker smuggle additional protocol fields into the frame.
///
/// Mitigation: every byte slice that gets followed by a `\0` terminator
/// passes through `sanitize_cstring_bytes` first, which substitutes the
/// Unicode replacement codepoint `U+FFFD` (3 UTF-8 bytes: `EF BF BD`)
/// for any embedded NUL byte. The substitution preserves the visible
/// shape of the message for debugging without giving an attacker a
/// path to inject a synthetic protocol field. Emitting `U+FFFD` is
/// safe for the PG client side: every PG client we know of reports
/// errors as opaque strings rather than parsing them.
fn sanitize_cstring_bytes(input: &[u8]) -> Vec<u8> {
    if !input.contains(&0) {
        return input.to_vec();
    }
    let mut out = Vec::with_capacity(input.len() + 8);
    for &b in input {
        if b == 0 {
            // U+FFFD REPLACEMENT CHARACTER (UTF-8 EF BF BD)
            out.extend_from_slice(&[0xEF, 0xBF, 0xBD]);
        } else {
            out.push(b);
        }
    }
    out
}

#[inline]
fn push_cstring(buf: &mut Vec<u8>, value: &str) {
    buf.extend_from_slice(&sanitize_cstring_bytes(value.as_bytes()));
    buf.push(0);
}

fn encode_backend(msg: &BackendMessage) -> (u8, Vec<u8>) {
    match msg {
        BackendMessage::AuthenticationOk => {
            // Subtype 0 = AuthenticationOk.
            (b'R', vec![0, 0, 0, 0])
        }
        BackendMessage::AuthenticationCleartextPassword => {
            // Subtype 3 = AuthenticationCleartextPassword.
            (b'R', vec![0, 0, 0, 3])
        }
        BackendMessage::ParameterStatus { name, value } => {
            let mut buf = Vec::with_capacity(name.len() + value.len() + 2);
            // F-02: name + value are user-controlled in some pathways.
            push_cstring(&mut buf, name);
            push_cstring(&mut buf, value);
            (b'S', buf)
        }
        BackendMessage::BackendKeyData { pid, key } => {
            let mut buf = Vec::with_capacity(8);
            buf.extend_from_slice(&pid.to_be_bytes());
            buf.extend_from_slice(&key.to_be_bytes());
            (b'K', buf)
        }
        BackendMessage::ReadyForQuery(status) => (b'Z', vec![status.as_byte()]),
        BackendMessage::RowDescription(cols) => {
            let mut buf = Vec::new();
            buf.extend_from_slice(&(cols.len() as i16).to_be_bytes());
            for col in cols {
                // F-02: column name is user-derived (SELECT ... AS "x\0y").
                push_cstring(&mut buf, &col.name);
                buf.extend_from_slice(&col.table_oid.to_be_bytes());
                buf.extend_from_slice(&col.column_attr.to_be_bytes());
                buf.extend_from_slice(&col.type_oid.to_be_bytes());
                buf.extend_from_slice(&col.type_size.to_be_bytes());
                buf.extend_from_slice(&col.type_mod.to_be_bytes());
                buf.extend_from_slice(&col.format.to_be_bytes());
            }
            (b'T', buf)
        }
        BackendMessage::DataRow(fields) => {
            let mut buf = Vec::new();
            buf.extend_from_slice(&(fields.len() as i16).to_be_bytes());
            for field in fields {
                match field {
                    None => {
                        // -1 length signals NULL.
                        buf.extend_from_slice(&(-1i32).to_be_bytes());
                    }
                    Some(bytes) => {
                        // DataRow uses length-prefixed bytes, NOT
                        // C-strings — embedded NULs are legal here
                        // and must NOT be sanitized.
                        buf.extend_from_slice(&(bytes.len() as i32).to_be_bytes());
                        buf.extend_from_slice(bytes);
                    }
                }
            }
            (b'D', buf)
        }
        BackendMessage::CommandComplete(tag) => {
            let mut buf = Vec::with_capacity(tag.len() + 1);
            // F-02: command tag includes user-influenced row counts /
            // statement classes; sanitize before NUL-terminating.
            push_cstring(&mut buf, tag);
            (b'C', buf)
        }
        BackendMessage::ParseComplete => (b'1', Vec::new()),
        BackendMessage::BindComplete => (b'2', Vec::new()),
        BackendMessage::CloseComplete => (b'3', Vec::new()),
        BackendMessage::ParameterDescription(oids) => {
            let mut buf = Vec::with_capacity(2 + oids.len() * 4);
            buf.extend_from_slice(&(oids.len() as i16).to_be_bytes());
            for oid in oids {
                buf.extend_from_slice(&oid.to_be_bytes());
            }
            (b't', buf)
        }
        BackendMessage::NoData => (b'n', Vec::new()),
        BackendMessage::ErrorResponse {
            severity,
            code,
            message,
        } => {
            let mut buf = Vec::new();
            // Field 'S' = severity (ERROR, FATAL, PANIC, ...)
            buf.push(b'S');
            push_cstring(&mut buf, severity);
            // Field 'V' = non-localized severity (PG 9.6+).
            buf.push(b'V');
            push_cstring(&mut buf, severity);
            // Field 'C' = SQLSTATE.
            buf.push(b'C');
            push_cstring(&mut buf, code);
            // Field 'M' = human message — F-02 primary attack surface.
            buf.push(b'M');
            push_cstring(&mut buf, message);
            // Trailing null terminator ends the field list.
            buf.push(0);
            (b'E', buf)
        }
        BackendMessage::NoticeResponse { message } => {
            let mut buf = Vec::new();
            buf.push(b'S');
            buf.extend_from_slice(b"NOTICE");
            buf.push(0);
            buf.push(b'M');
            // F-02: message is user-influenced.
            push_cstring(&mut buf, message);
            buf.push(0);
            (b'N', buf)
        }
        BackendMessage::EmptyQueryResponse => (b'I', Vec::new()),
    }
}

// ────────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────────

/// Read a C-string (null-terminated UTF-8) starting at `pos`. Advances
/// `pos` past the terminator. Returns `Protocol` error when malformed.
fn read_cstring(buf: &[u8], pos: &mut usize) -> Result<String, PgWireError> {
    let start = *pos;
    while *pos < buf.len() && buf[*pos] != 0 {
        *pos += 1;
    }
    if *pos >= buf.len() {
        return Err(PgWireError::Protocol("cstring missing terminator".into()));
    }
    let s = std::str::from_utf8(&buf[start..*pos])
        .map_err(|e| PgWireError::Protocol(format!("invalid utf8: {e}")))?
        .to_string();
    *pos += 1; // skip null
    Ok(s)
}

fn parse_parse_message(payload: &[u8]) -> Result<ParseMessage, PgWireError> {
    let mut pos = 0;
    let statement = read_cstring(payload, &mut pos)?;
    let query = read_cstring(payload, &mut pos)?;
    let nparams = read_i16(payload, &mut pos, "Parse parameter count")?;
    if nparams < 0 {
        return Err(PgWireError::Protocol(
            "negative Parse parameter count".into(),
        ));
    }
    let mut param_type_oids = Vec::with_capacity(nparams as usize);
    for _ in 0..nparams {
        param_type_oids.push(read_u32(payload, &mut pos, "Parse parameter type OID")?);
    }
    ensure_consumed(payload, pos, "Parse")?;
    Ok(ParseMessage {
        statement,
        query,
        param_type_oids,
    })
}

fn parse_bind_message(payload: &[u8]) -> Result<BindMessage, PgWireError> {
    let mut pos = 0;
    let portal = read_cstring(payload, &mut pos)?;
    let statement = read_cstring(payload, &mut pos)?;

    let nformats = read_i16(payload, &mut pos, "Bind format count")?;
    if nformats < 0 {
        return Err(PgWireError::Protocol("negative Bind format count".into()));
    }
    let mut param_format_codes = Vec::with_capacity(nformats as usize);
    for _ in 0..nformats {
        param_format_codes.push(read_i16(payload, &mut pos, "Bind format code")?);
    }

    let nparams = read_i16(payload, &mut pos, "Bind parameter count")?;
    if nparams < 0 {
        return Err(PgWireError::Protocol(
            "negative Bind parameter count".into(),
        ));
    }
    let mut params = Vec::with_capacity(nparams as usize);
    for _ in 0..nparams {
        let len = read_i32(payload, &mut pos, "Bind parameter length")?;
        if len == -1 {
            params.push(None);
        } else if len < -1 {
            return Err(PgWireError::Protocol(
                "invalid Bind parameter length".into(),
            ));
        } else {
            params.push(Some(
                read_bytes(payload, &mut pos, len as usize, "Bind parameter")?.to_vec(),
            ));
        }
    }

    let nresult_formats = read_i16(payload, &mut pos, "Bind result format count")?;
    if nresult_formats < 0 {
        return Err(PgWireError::Protocol(
            "negative Bind result format count".into(),
        ));
    }
    let mut result_format_codes = Vec::with_capacity(nresult_formats as usize);
    for _ in 0..nresult_formats {
        result_format_codes.push(read_i16(payload, &mut pos, "Bind result format code")?);
    }
    ensure_consumed(payload, pos, "Bind")?;

    Ok(BindMessage {
        portal,
        statement,
        param_format_codes,
        params,
        result_format_codes,
    })
}

fn parse_describe_message(payload: &[u8]) -> Result<DescribeMessage, PgWireError> {
    let mut pos = 0;
    let target = read_describe_target(payload, &mut pos, "Describe")?;
    let name = read_cstring(payload, &mut pos)?;
    ensure_consumed(payload, pos, "Describe")?;
    Ok(DescribeMessage { target, name })
}

fn parse_execute_message(payload: &[u8]) -> Result<ExecuteMessage, PgWireError> {
    let mut pos = 0;
    let portal = read_cstring(payload, &mut pos)?;
    let max_rows = read_u32(payload, &mut pos, "Execute max rows")?;
    ensure_consumed(payload, pos, "Execute")?;
    Ok(ExecuteMessage { portal, max_rows })
}

fn parse_close_message(payload: &[u8]) -> Result<CloseMessage, PgWireError> {
    let mut pos = 0;
    let target = read_describe_target(payload, &mut pos, "Close")?;
    let name = read_cstring(payload, &mut pos)?;
    ensure_consumed(payload, pos, "Close")?;
    Ok(CloseMessage { target, name })
}

fn read_describe_target(
    payload: &[u8],
    pos: &mut usize,
    frame: &'static str,
) -> Result<DescribeTarget, PgWireError> {
    let byte = *read_bytes(payload, pos, 1, frame)?
        .first()
        .expect("one target byte");
    match byte {
        b'S' => Ok(DescribeTarget::Statement),
        b'P' => Ok(DescribeTarget::Portal),
        other => Err(PgWireError::Protocol(format!(
            "{frame} target must be 'S' or 'P', got 0x{other:02x}"
        ))),
    }
}

fn read_i16(payload: &[u8], pos: &mut usize, field: &'static str) -> Result<i16, PgWireError> {
    let bytes = read_bytes(payload, pos, 2, field)?;
    Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_i32(payload: &[u8], pos: &mut usize, field: &'static str) -> Result<i32, PgWireError> {
    let bytes = read_bytes(payload, pos, 4, field)?;
    Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u32(payload: &[u8], pos: &mut usize, field: &'static str) -> Result<u32, PgWireError> {
    let bytes = read_bytes(payload, pos, 4, field)?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_bytes<'a>(
    payload: &'a [u8],
    pos: &mut usize,
    len: usize,
    field: &'static str,
) -> Result<&'a [u8], PgWireError> {
    let end = pos
        .checked_add(len)
        .ok_or_else(|| PgWireError::Protocol(format!("{field} length overflow")))?;
    if end > payload.len() {
        return Err(PgWireError::Protocol(format!("{field} truncated")));
    }
    let bytes = &payload[*pos..end];
    *pos = end;
    Ok(bytes)
}

fn ensure_consumed(payload: &[u8], pos: usize, frame: &'static str) -> Result<(), PgWireError> {
    if pos == payload.len() {
        Ok(())
    } else {
        Err(PgWireError::Protocol(format!(
            "{frame} had {} trailing bytes",
            payload.len() - pos
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parse_startup_v3() {
        // length (4) + version (4) + user\0val\0 + terminator\0
        let mut payload: Vec<u8> = Vec::new();
        payload.extend_from_slice(&PG_PROTOCOL_V3.to_be_bytes());
        payload.extend_from_slice(b"user\0alice\0");
        payload.push(0);
        let len = (4 + payload.len()) as u32;
        let mut frame = Vec::new();
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(&payload);

        let mut cursor = std::io::Cursor::new(frame);
        let msg = read_startup(&mut cursor).await.unwrap();
        match msg {
            FrontendMessage::Startup(params) => {
                assert_eq!(params.get("user"), Some("alice"));
            }
            other => panic!("expected Startup, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn parse_ssl_request() {
        let mut frame: Vec<u8> = Vec::new();
        frame.extend_from_slice(&8u32.to_be_bytes());
        frame.extend_from_slice(&PG_SSL_REQUEST.to_be_bytes());
        let mut cursor = std::io::Cursor::new(frame);
        assert!(matches!(
            read_startup(&mut cursor).await.unwrap(),
            FrontendMessage::SslRequest
        ));
    }

    #[tokio::test]
    async fn parse_query_frame() {
        let query = "SELECT 1\0";
        let mut frame = Vec::new();
        frame.push(b'Q');
        let len = (4 + query.len()) as u32;
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(query.as_bytes());
        let mut cursor = std::io::Cursor::new(frame);
        match read_frame(&mut cursor).await.unwrap() {
            FrontendMessage::Query(s) => assert_eq!(s, "SELECT 1"),
            other => panic!("expected Query, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn parse_extended_query_frames() {
        let mut parse_payload = Vec::new();
        push_test_cstring(&mut parse_payload, "");
        push_test_cstring(&mut parse_payload, "SELECT $1");
        parse_payload.extend_from_slice(&1i16.to_be_bytes());
        parse_payload.extend_from_slice(&23u32.to_be_bytes());
        let mut frame = tagged_frame(b'P', parse_payload);
        let mut cursor = std::io::Cursor::new(frame);
        match read_frame(&mut cursor).await.unwrap() {
            FrontendMessage::Parse(msg) => {
                assert_eq!(msg.statement, "");
                assert_eq!(msg.query, "SELECT $1");
                assert_eq!(msg.param_type_oids, vec![23]);
            }
            other => panic!("expected Parse, got {other:?}"),
        }

        let mut bind_payload = Vec::new();
        push_test_cstring(&mut bind_payload, "");
        push_test_cstring(&mut bind_payload, "");
        bind_payload.extend_from_slice(&1i16.to_be_bytes());
        bind_payload.extend_from_slice(&0i16.to_be_bytes());
        bind_payload.extend_from_slice(&1i16.to_be_bytes());
        bind_payload.extend_from_slice(&2i32.to_be_bytes());
        bind_payload.extend_from_slice(b"42");
        bind_payload.extend_from_slice(&0i16.to_be_bytes());
        frame = tagged_frame(b'B', bind_payload);
        let mut cursor = std::io::Cursor::new(frame);
        match read_frame(&mut cursor).await.unwrap() {
            FrontendMessage::Bind(msg) => {
                assert_eq!(msg.portal, "");
                assert_eq!(msg.statement, "");
                assert_eq!(msg.param_format_codes, vec![0]);
                assert_eq!(msg.params, vec![Some(b"42".to_vec())]);
                assert!(msg.result_format_codes.is_empty());
            }
            other => panic!("expected Bind, got {other:?}"),
        }

        let mut describe_payload = vec![b'P'];
        push_test_cstring(&mut describe_payload, "");
        let mut cursor = std::io::Cursor::new(tagged_frame(b'D', describe_payload));
        assert!(matches!(
            read_frame(&mut cursor).await.unwrap(),
            FrontendMessage::Describe(DescribeMessage {
                target: DescribeTarget::Portal,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn emit_ready_for_query() {
        let mut out: Vec<u8> = Vec::new();
        write_frame(
            &mut out,
            &BackendMessage::ReadyForQuery(TransactionStatus::Idle),
        )
        .await
        .unwrap();
        assert_eq!(out, vec![b'Z', 0, 0, 0, 5, b'I']);
    }

    #[tokio::test]
    async fn emit_row_description_and_data_row() {
        let mut out: Vec<u8> = Vec::new();
        write_frame(
            &mut out,
            &BackendMessage::RowDescription(vec![ColumnDescriptor {
                name: "id".to_string(),
                table_oid: 0,
                column_attr: 0,
                type_oid: 23,
                type_size: 4,
                type_mod: -1,
                format: 0,
            }]),
        )
        .await
        .unwrap();
        assert_eq!(out[0], b'T');

        let mut data: Vec<u8> = Vec::new();
        write_frame(
            &mut data,
            &BackendMessage::DataRow(vec![Some(b"42".to_vec()), None]),
        )
        .await
        .unwrap();
        assert_eq!(data[0], b'D');
    }

    #[tokio::test]
    async fn emit_extended_completion_frames() {
        let mut out = Vec::new();
        write_frame(&mut out, &BackendMessage::ParseComplete)
            .await
            .unwrap();
        write_frame(&mut out, &BackendMessage::BindComplete)
            .await
            .unwrap();
        write_frame(
            &mut out,
            &BackendMessage::ParameterDescription(vec![23, 25]),
        )
        .await
        .unwrap();
        write_frame(&mut out, &BackendMessage::NoData)
            .await
            .unwrap();
        write_frame(&mut out, &BackendMessage::CloseComplete)
            .await
            .unwrap();
        assert_eq!(collect_tags(&out), vec![b'1', b'2', b't', b'n', b'3']);
    }

    // ---------------------------------------------------------------
    // F-02 (audit doc 2026-05-06): NUL-injection rejection in PG3
    // C-string fields. Replacement codepoint U+FFFD is emitted
    // instead of the raw NUL so the field cannot be terminated
    // prematurely on the wire.
    // ---------------------------------------------------------------

    fn count_nul(buf: &[u8]) -> usize {
        buf.iter().filter(|&&b| b == 0).count()
    }

    #[tokio::test]
    async fn pg3_nul_error_response_message_field_sanitized() {
        let mut out: Vec<u8> = Vec::new();
        write_frame(
            &mut out,
            &BackendMessage::ErrorResponse {
                severity: "ERROR".to_string(),
                code: "42000".to_string(),
                message: "smuggled\0M\x00injection".to_string(),
            },
        )
        .await
        .unwrap();
        assert_eq!(out[0], b'E');
        // ErrorResponse body: 4 inner C-string terminators (S/V/C/M)
        // + 1 list-end terminator = 5 total NULs. The message field
        // had 2 raw NULs in it; if not sanitized we'd see 7 NULs.
        let body = &out[5..];
        assert_eq!(
            count_nul(body),
            5,
            "expected 5 NULs (4 field + 1 list-end), got {} :: body={:?}",
            count_nul(body),
            body
        );
        // U+FFFD must be present (EF BF BD).
        assert!(
            body.windows(3).any(|w| w == [0xEF, 0xBF, 0xBD]),
            "expected U+FFFD substitution in body"
        );
    }

    #[tokio::test]
    async fn pg3_nul_notice_response_sanitized() {
        let mut out: Vec<u8> = Vec::new();
        write_frame(
            &mut out,
            &BackendMessage::NoticeResponse {
                message: "evil\0field".to_string(),
            },
        )
        .await
        .unwrap();
        assert_eq!(out[0], b'N');
        let body = &out[5..];
        // 2 inner C-string terminators (S, M) + 1 list-end = 3 NULs.
        assert_eq!(count_nul(body), 3);
        assert!(body.windows(3).any(|w| w == [0xEF, 0xBF, 0xBD]));
    }

    #[tokio::test]
    async fn pg3_nul_command_complete_sanitized() {
        let mut out: Vec<u8> = Vec::new();
        write_frame(
            &mut out,
            &BackendMessage::CommandComplete("SELECT\0;DROP".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(out[0], b'C');
        let body = &out[5..];
        // CommandComplete = single C-string + terminator -> 1 NUL.
        assert_eq!(count_nul(body), 1);
    }

    #[tokio::test]
    async fn pg3_nul_row_description_column_name_sanitized() {
        let mut out: Vec<u8> = Vec::new();
        write_frame(
            &mut out,
            &BackendMessage::RowDescription(vec![ColumnDescriptor {
                name: "evil\0col".to_string(),
                table_oid: 0,
                column_attr: 0,
                type_oid: 23,
                type_size: 4,
                type_mod: -1,
                format: 0,
            }]),
        )
        .await
        .unwrap();
        assert_eq!(out[0], b'T');
        // The column-name region (after the i16 field count, before
        // the OIDs) must contain exactly one terminator, not two.
        let body = &out[5..];
        // Skip 2 bytes (column count i16); next bytes up to the
        // first NUL are the column name.
        let name_region = &body[2..];
        let first_nul = name_region.iter().position(|&b| b == 0).unwrap();
        assert!(
            name_region[..first_nul]
                .windows(3)
                .any(|w| w == [0xEF, 0xBF, 0xBD]),
            "U+FFFD missing from sanitized column name"
        );
    }

    #[test]
    fn sanitize_cstring_fastpath_no_nul() {
        let s = "no nuls here";
        let out = sanitize_cstring_bytes(s.as_bytes());
        assert_eq!(out, s.as_bytes());
    }

    #[test]
    fn sanitize_cstring_substitutes_nul_with_replacement_codepoint() {
        let s = b"a\0b\0c";
        let out = sanitize_cstring_bytes(s);
        // Each NUL becomes 3 bytes; total = 1 + 3 + 1 + 3 + 1 = 9.
        assert_eq!(out.len(), 9);
        assert!(!out.contains(&0));
        assert_eq!(&out[1..4], &[0xEF, 0xBF, 0xBD]);
        assert_eq!(&out[5..8], &[0xEF, 0xBF, 0xBD]);
    }

    fn tagged_frame(tag: u8, payload: Vec<u8>) -> Vec<u8> {
        let mut frame = vec![tag];
        frame.extend_from_slice(&((payload.len() + 4) as u32).to_be_bytes());
        frame.extend_from_slice(&payload);
        frame
    }

    fn push_test_cstring(out: &mut Vec<u8>, value: &str) {
        out.extend_from_slice(value.as_bytes());
        out.push(0);
    }

    fn collect_tags(bytes: &[u8]) -> Vec<u8> {
        let mut tags = Vec::new();
        let mut pos = 0;
        while pos < bytes.len() {
            tags.push(bytes[pos]);
            let len = u32::from_be_bytes([
                bytes[pos + 1],
                bytes[pos + 2],
                bytes[pos + 3],
                bytes[pos + 4],
            ]) as usize;
            pos += 1 + len;
        }
        tags
    }
}
