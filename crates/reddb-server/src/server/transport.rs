use super::*;

pub(crate) fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "OK",
    }
}

pub(crate) struct HttpRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) query: BTreeMap<String, String>,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: Vec<u8>,
}

impl HttpRequest {
    pub(crate) fn read_from<S: Read>(stream: &mut S, max_body_bytes: usize) -> io::Result<Self> {
        let mut buffer = Vec::with_capacity(4096);
        let mut chunk = [0_u8; 2048];
        let header_end = loop {
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed before request headers",
                ));
            }
            buffer.extend_from_slice(&chunk[..read]);
            if buffer.len() > max_body_bytes.saturating_add(16 * 1024) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "request headers too large",
                ));
            }
            if let Some(position) = find_header_end(&buffer) {
                break position;
            }
        };

        let (method, target, headers) = {
            let head = String::from_utf8_lossy(&buffer[..header_end]);
            let mut lines = head.lines();
            let request_line = lines.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "missing request line")
            })?;
            let mut request_parts = request_line.split_whitespace();
            let method = request_parts
                .next()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing method"))?
                .to_string();
            let target = request_parts
                .next()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing path"))?
                .to_string();

            let mut headers = BTreeMap::new();
            for line in lines {
                if let Some((name, value)) = line.split_once(':') {
                    headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
                }
            }
            (method, target, headers)
        };

        let content_length = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        if content_length > max_body_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request body exceeds configured limit",
            ));
        }

        let total_needed = header_end + 4 + content_length;
        while buffer.len() < total_needed {
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed before request body",
                ));
            }
            buffer.extend_from_slice(&chunk[..read]);
        }

        let body = buffer[header_end + 4..total_needed].to_vec();
        let (path, query) = split_target(&target);

        Ok(Self {
            method,
            path,
            query,
            headers,
            body,
        })
    }
}

pub(crate) struct HttpResponse {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
    pub(crate) content_type: &'static str,
    /// Extra response headers beyond the hard-coded
    /// Content-Type / Content-Length / Connection trio. Values are
    /// `http::HeaderValue` rather than raw strings so the only path
    /// to populate this collection is through
    /// [`super::header_escape_guard::HeaderEscapeGuard`] (issue #176,
    /// ADR 0010). Header *names* live in source code and are
    /// accepted as `&'static str` — the guard owns *value* escape,
    /// not name escape.
    pub(crate) extra_headers: Vec<(&'static str, http::HeaderValue)>,
}

impl HttpResponse {
    pub(crate) fn to_http_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let header = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.status,
            status_text(self.status),
            self.content_type,
            self.body.len()
        );
        bytes.extend_from_slice(header.as_bytes());
        for (name, value) in &self.extra_headers {
            bytes.extend_from_slice(name.as_bytes());
            bytes.extend_from_slice(b": ");
            bytes.extend_from_slice(value.as_bytes());
            bytes.extend_from_slice(b"\r\n");
        }
        bytes.extend_from_slice(b"\r\n");
        bytes.extend_from_slice(&self.body);
        bytes
    }

    /// Attach a guard-validated header to this response.
    ///
    /// The value is already typed as `http::HeaderValue`, which means
    /// the only callers that can reach this method are ones who
    /// routed through `HeaderEscapeGuard::header_value`. There is no
    /// raw-string overload by design (#176 / ADR 0010).
    #[allow(dead_code)]
    pub(crate) fn with_header(mut self, name: &'static str, value: http::HeaderValue) -> Self {
        self.extra_headers.push((name, value));
        self
    }
}

pub(crate) fn json_ok(message: impl Into<String>) -> HttpResponse {
    let mut object = Map::new();
    object.insert("ok".to_string(), JsonValue::Bool(true));
    // `message` is caller-influenced in many call sites (it surfaces
    // request-derived strings back to the client). Route through the
    // JSON-boundary guard so the field round-trips through the
    // canonical encoder rather than being string-concatenated. See
    // ADR 0010 §3 / issue #178.
    let message = message.into();
    object.insert(
        "message".to_string(),
        crate::json_field::SerializedJsonField::tainted(&message),
    );
    json_response(200, JsonValue::Object(object))
}

pub(crate) fn json_error(status: u16, message: impl Into<String>) -> HttpResponse {
    let mut object = Map::new();
    object.insert("ok".to_string(), JsonValue::Bool(false));
    // `message` is caller-influenced in nearly every call site —
    // notably SQL parser errors (F-05) interpolate user fragments via
    // bare `format!` upstream, and the resulting `Display` string
    // reaches us here. Route through the JSON-boundary guard so the
    // field round-trips and any embedded `"`, control bytes, or
    // CRLF cannot terminate the field early. See ADR 0010 §3,
    // F-05 in `docs/security/serialization-boundary-audit-2026-05-06.md`,
    // and issue #178.
    let message = message.into();
    object.insert(
        "error".to_string(),
        crate::json_field::SerializedJsonField::tainted(&message),
    );
    json_response(status, JsonValue::Object(object))
}

pub(crate) fn json_error_code(
    status: u16,
    code: impl Into<String>,
    message: impl Into<String>,
) -> HttpResponse {
    let code = code.into();
    let message = message.into();
    let mut object = Map::new();
    object.insert("ok".to_string(), JsonValue::Bool(false));
    object.insert("code".to_string(), JsonValue::String(code));
    object.insert(
        "error".to_string(),
        crate::json_field::SerializedJsonField::tainted(&message),
    );
    object.insert(
        "message".to_string(),
        crate::json_field::SerializedJsonField::tainted(&message),
    );
    json_response(status, JsonValue::Object(object))
}

pub(crate) fn json_response(status: u16, value: JsonValue) -> HttpResponse {
    HttpResponse {
        status,
        body: value.to_string_compact().into_bytes(),
        content_type: "application/json",
        extra_headers: Vec::new(),
    }
}

/// Map a `RedDBError` to an HTTP status + display string.
///
/// Single source of truth for the engine's error → HTTP-status table.
/// Handlers that route through `run_use_case` get this mapping for
/// free; handlers that hand-roll status codes should migrate to keep
/// the wire surface uniform.
///
/// `QuotaExceeded` payloads are expected to follow the
/// `quota_exceeded:<bucket>:<current>:<max>` shape — the bucket
/// prefix decides which 4xx/5xx the operator sees:
/// - `:storage:*` → 507
/// - `:rate:*`    → 429
/// - `:duration:*`→ 504
/// - `:payload:*` → 413
pub(crate) fn map_runtime_error(err: &crate::api::RedDBError) -> (u16, String) {
    use crate::api::RedDBError::*;
    let msg = err.to_string();
    let status = match err {
        NotFound(_) => 404,
        ReadOnly(_) => 403,
        // Issue #524 — chain-INSERT conflict surfaces as a 409 so a polite
        // client can retry against the advanced tip. The body still carries
        // the marker so deeper handlers can parse the tip payload back out.
        InvalidOperation(msg) if msg.starts_with("BlockchainConflict:") => 409,
        InvalidOperation(msg) if msg.starts_with("BlockchainCollectionImmutable") => 409,
        // Issue #522 — Signed Writes verification failures. 401 for the
        // three "the request is well-formed but the credential check
        // failed" cases; 400 for the bytes-on-the-wire problems
        // (missing/length/encoding).
        InvalidOperation(msg) if msg.starts_with("SignedWriteError:UnknownSigner") => 401,
        InvalidOperation(msg) if msg.starts_with("SignedWriteError:RevokedSigner") => 401,
        InvalidOperation(msg) if msg.starts_with("SignedWriteError:InvalidSignature") => 401,
        InvalidConfig(_) | InvalidOperation(_) => 400,
        Query(msg) if msg.starts_with("ask_provider_failover_exhausted:") => 503,
        Query(msg) if msg.starts_with("ask_primary_sync_unavailable:") => 503,
        Query(_) => 400,
        Validation { .. } => 422,
        FeatureNotEnabled(_) => 501,
        SchemaVersionMismatch { .. } => 409,
        QuotaExceeded(payload) => {
            let body = payload.strip_prefix("quota_exceeded:").unwrap_or(payload);
            if body.starts_with("storage") {
                507
            } else if body.starts_with("rate") {
                429
            } else if body.starts_with("duration") {
                504
            } else if body.starts_with("payload") {
                413
            } else {
                429
            }
        }
        Engine(_) | Catalog(_) | Io(_) | VersionUnavailable | Internal(_) => 500,
    };
    (status, msg)
}

/// Run a use-case closure and format the result.
///
/// Collapses the `parse → run → match → present` boilerplate scattered
/// across handler files into one Adapter. Cross-cutting concerns
/// (status mapping, latency tracing, audit hooks) live here, not
/// duplicated 14 ways.
pub(crate) fn run_use_case<O, F, P>(run: F, present: P) -> HttpResponse
where
    F: FnOnce() -> crate::api::RedDBResult<O>,
    P: FnOnce(&O) -> JsonValue,
{
    match run() {
        Ok(output) => json_response(200, present(&output)),
        Err(err) => {
            let (status, msg) = map_runtime_error(&err);
            json_error(status, msg)
        }
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use crate::api::RedDBError;

    #[test]
    fn map_runtime_error_covers_each_variant() {
        // Spot-check: status code is the contract, not the message.
        assert_eq!(map_runtime_error(&RedDBError::NotFound("x".into())).0, 404);
        assert_eq!(map_runtime_error(&RedDBError::ReadOnly("x".into())).0, 403);
        assert_eq!(
            map_runtime_error(&RedDBError::InvalidConfig("x".into())).0,
            400
        );
        assert_eq!(map_runtime_error(&RedDBError::Query("x".into())).0, 400);
        assert_eq!(
            map_runtime_error(&RedDBError::Query(
                "ask_primary_sync_unavailable: connect failed".into()
            ))
            .0,
            503
        );
        assert_eq!(
            map_runtime_error(&RedDBError::Validation {
                message: "x".into(),
                validation: JsonValue::Object(Map::new()),
            })
            .0,
            422
        );
        assert_eq!(
            map_runtime_error(&RedDBError::FeatureNotEnabled("x".into())).0,
            501
        );
        assert_eq!(
            map_runtime_error(&RedDBError::SchemaVersionMismatch {
                expected: 1,
                found: 2
            })
            .0,
            409
        );
        assert_eq!(
            map_runtime_error(&RedDBError::QuotaExceeded("storage:1:0".into())).0,
            507
        );
        assert_eq!(
            map_runtime_error(&RedDBError::QuotaExceeded("rate:1:0".into())).0,
            429
        );
        assert_eq!(
            map_runtime_error(&RedDBError::QuotaExceeded("duration:1:0".into())).0,
            504
        );
        assert_eq!(
            map_runtime_error(&RedDBError::QuotaExceeded("payload:1:0".into())).0,
            413
        );
        assert_eq!(map_runtime_error(&RedDBError::Internal("x".into())).0, 500);
        assert_eq!(map_runtime_error(&RedDBError::Engine("x".into())).0, 500);
        assert_eq!(map_runtime_error(&RedDBError::Catalog("x".into())).0, 500);
        assert_eq!(map_runtime_error(&RedDBError::VersionUnavailable).0, 500);
    }

    #[test]
    fn run_use_case_returns_200_with_presented_body() {
        let resp = run_use_case::<i32, _, _>(
            || Ok(7),
            |n| {
                let mut m = Map::new();
                m.insert("count".into(), JsonValue::Number(*n as f64));
                JsonValue::Object(m)
            },
        );
        assert_eq!(resp.status, 200);
        assert!(String::from_utf8_lossy(&resp.body).contains("\"count\":7"));
    }

    #[test]
    fn run_use_case_maps_err_through_helper() {
        let resp = run_use_case::<i32, _, _>(
            || Err(crate::api::RedDBError::NotFound("missing".into())),
            |_| JsonValue::Null,
        );
        assert_eq!(resp.status, 404);
        assert!(String::from_utf8_lossy(&resp.body).contains("not found"));
    }

    #[test]
    fn to_http_bytes_emits_extra_header_after_hardcoded_trio() {
        // The wire-format check: a guard-validated header lands on
        // the response between the Connection line and the body
        // separator, framed with `Name: value\r\n`. This test
        // pins the *byte sequence* so a future refactor that
        // reorders the framing surfaces here.
        use super::super::header_escape_guard::HeaderEscapeGuard;

        let value = HeaderEscapeGuard::header_value("max-age=31536000").unwrap();
        let resp = json_ok("hello").with_header("Strict-Transport-Security", value);
        let bytes = resp.to_http_bytes();
        let head = String::from_utf8_lossy(&bytes);
        assert!(
            head.contains("\r\nStrict-Transport-Security: max-age=31536000\r\n"),
            "expected guard-validated header in wire output, got: {head}"
        );
        // The canonical hardcoded headers still appear and the
        // header / body boundary is still a single double-CRLF.
        assert!(head.starts_with("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"));
        assert_eq!(head.matches("\r\n\r\n").count(), 1);
    }

    #[test]
    fn to_http_bytes_with_no_extra_headers_matches_legacy_framing() {
        // Regression guard: empty extra_headers must not emit
        // a stray blank line — the legacy callers all default to
        // `Vec::new()` and must produce byte-identical output to
        // the pre-#176 framing.
        let resp = json_ok("hi");
        let bytes = resp.to_http_bytes();
        let head = String::from_utf8_lossy(&bytes);
        // Exactly one CRLFCRLF separator between headers and body.
        assert_eq!(head.matches("\r\n\r\n").count(), 1);
        assert!(head
            .starts_with("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: "));
    }
}
pub(crate) fn split_target(target: &str) -> (String, BTreeMap<String, String>) {
    match target.split_once('?') {
        Some((path, raw_query)) => (path.to_string(), parse_query_string(raw_query)),
        None => (target.to_string(), BTreeMap::new()),
    }
}

pub(crate) fn parse_query_string(input: &str) -> BTreeMap<String, String> {
    let mut params = BTreeMap::new();
    for pair in input.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(key.to_string(), value.to_string());
    }
    params
}

pub(crate) fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}
