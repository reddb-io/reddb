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
    pub(crate) fn read_from(stream: &mut TcpStream, max_body_bytes: usize) -> io::Result<Self> {
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
}

impl HttpResponse {
    pub(crate) fn to_http_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let header = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.status,
            status_text(self.status),
            self.content_type,
            self.body.len()
        );
        bytes.extend_from_slice(header.as_bytes());
        bytes.extend_from_slice(&self.body);
        bytes
    }
}

pub(crate) fn json_ok(message: impl Into<String>) -> HttpResponse {
    let mut object = Map::new();
    object.insert("ok".to_string(), JsonValue::Bool(true));
    object.insert("message".to_string(), JsonValue::String(message.into()));
    json_response(200, JsonValue::Object(object))
}

pub(crate) fn json_error(status: u16, message: impl Into<String>) -> HttpResponse {
    let mut object = Map::new();
    object.insert("ok".to_string(), JsonValue::Bool(false));
    object.insert("error".to_string(), JsonValue::String(message.into()));
    json_response(status, JsonValue::Object(object))
}

pub(crate) fn json_response(status: u16, value: JsonValue) -> HttpResponse {
    HttpResponse {
        status,
        body: value.to_string_compact().into_bytes(),
        content_type: "application/json",
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
