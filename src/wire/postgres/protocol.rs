//! PostgreSQL v3 wire protocol message framing (Phase 3.1 PG parity).
//!
//! Implements the bits of the PG v3 protocol RedDB needs for simple
//! query support: startup negotiation, authentication (trust), the
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
pub const PG_PROTOCOL_V3: u32 = (3 << 16) | 0;

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
    /// `p` — password / SASL response. Payload is ignored for `trust` auth.
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
    if len < 4 || len > 1_048_576 {
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

fn encode_backend(msg: &BackendMessage) -> (u8, Vec<u8>) {
    match msg {
        BackendMessage::AuthenticationOk => {
            // Subtype 0 = AuthenticationOk.
            (b'R', vec![0, 0, 0, 0])
        }
        BackendMessage::ParameterStatus { name, value } => {
            let mut buf = Vec::with_capacity(name.len() + value.len() + 2);
            buf.extend_from_slice(name.as_bytes());
            buf.push(0);
            buf.extend_from_slice(value.as_bytes());
            buf.push(0);
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
                buf.extend_from_slice(col.name.as_bytes());
                buf.push(0);
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
                        buf.extend_from_slice(&(bytes.len() as i32).to_be_bytes());
                        buf.extend_from_slice(bytes);
                    }
                }
            }
            (b'D', buf)
        }
        BackendMessage::CommandComplete(tag) => {
            let mut buf = Vec::with_capacity(tag.len() + 1);
            buf.extend_from_slice(tag.as_bytes());
            buf.push(0);
            (b'C', buf)
        }
        BackendMessage::ErrorResponse {
            severity,
            code,
            message,
        } => {
            let mut buf = Vec::new();
            // Field 'S' = severity (ERROR, FATAL, PANIC, ...)
            buf.push(b'S');
            buf.extend_from_slice(severity.as_bytes());
            buf.push(0);
            // Field 'V' = non-localized severity (PG 9.6+).
            buf.push(b'V');
            buf.extend_from_slice(severity.as_bytes());
            buf.push(0);
            // Field 'C' = SQLSTATE.
            buf.push(b'C');
            buf.extend_from_slice(code.as_bytes());
            buf.push(0);
            // Field 'M' = human message.
            buf.push(b'M');
            buf.extend_from_slice(message.as_bytes());
            buf.push(0);
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
            buf.extend_from_slice(message.as_bytes());
            buf.push(0);
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
}
