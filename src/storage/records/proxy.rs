//! Proxy interception record types

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::storage::primitives::encoding::{read_varu32, write_varu32, DecodeError};

use super::helpers::{read_optional_string, read_string, write_optional_string, write_string};

/// Proxy connection record - tracks intercepted connections
#[derive(Debug, Clone)]
pub struct ProxyConnectionRecord {
    /// Unique connection ID
    pub connection_id: u64,
    /// Source IP:port
    pub src_ip: IpAddr,
    pub src_port: u16,
    /// Destination host (domain or IP)
    pub dst_host: String,
    pub dst_port: u16,
    /// Protocol (TCP=0, UDP=1)
    pub protocol: u8,
    /// Connection start timestamp
    pub started_at: u32,
    /// Connection end timestamp (0 if still active)
    pub ended_at: u32,
    /// Total bytes sent (client -> target)
    pub bytes_sent: u64,
    /// Total bytes received (target -> client)
    pub bytes_received: u64,
    /// TLS intercepted (true if MITM'd)
    pub tls_intercepted: bool,
}

impl ProxyConnectionRecord {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Connection ID (8 bytes)
        buf.extend_from_slice(&self.connection_id.to_le_bytes());

        // Source IP
        match self.src_ip {
            IpAddr::V4(ip) => {
                buf.push(4);
                buf.extend_from_slice(&ip.octets());
            }
            IpAddr::V6(ip) => {
                buf.push(6);
                buf.extend_from_slice(&ip.octets());
            }
        }

        // Source port (2 bytes)
        buf.extend_from_slice(&self.src_port.to_le_bytes());

        // Destination host (length-prefixed string)
        write_string(&mut buf, &self.dst_host);

        // Destination port (2 bytes)
        buf.extend_from_slice(&self.dst_port.to_le_bytes());

        // Protocol (1 byte)
        buf.push(self.protocol);

        // Timestamps (4 bytes each)
        buf.extend_from_slice(&self.started_at.to_le_bytes());
        buf.extend_from_slice(&self.ended_at.to_le_bytes());

        // Bytes sent/received (8 bytes each)
        buf.extend_from_slice(&self.bytes_sent.to_le_bytes());
        buf.extend_from_slice(&self.bytes_received.to_le_bytes());

        // TLS intercepted (1 byte)
        buf.push(if self.tls_intercepted { 1 } else { 0 });

        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < 8 {
            return Err(DecodeError("truncated proxy connection record"));
        }

        let mut pos = 0;

        // Connection ID
        let connection_id = u64::from_le_bytes([
            bytes[pos],
            bytes[pos + 1],
            bytes[pos + 2],
            bytes[pos + 3],
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]);
        pos += 8;

        // Source IP
        if pos >= bytes.len() {
            return Err(DecodeError("truncated src_ip version"));
        }
        let ip_version = bytes[pos];
        pos += 1;

        let src_ip = match ip_version {
            4 => {
                if bytes.len() < pos + 4 {
                    return Err(DecodeError("truncated src IPv4"));
                }
                let octets = [bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]];
                pos += 4;
                IpAddr::V4(Ipv4Addr::from(octets))
            }
            6 => {
                if bytes.len() < pos + 16 {
                    return Err(DecodeError("truncated src IPv6"));
                }
                let mut octets = [0u8; 16];
                octets.copy_from_slice(&bytes[pos..pos + 16]);
                pos += 16;
                IpAddr::V6(Ipv6Addr::from(octets))
            }
            _ => return Err(DecodeError("invalid IP version")),
        };

        // Source port
        if bytes.len() < pos + 2 {
            return Err(DecodeError("truncated src port"));
        }
        let src_port = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]);
        pos += 2;

        // Destination host
        let dst_host = read_string(bytes, &mut pos)?;

        // Destination port
        if bytes.len() < pos + 2 {
            return Err(DecodeError("truncated dst port"));
        }
        let dst_port = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]);
        pos += 2;

        // Protocol
        if pos >= bytes.len() {
            return Err(DecodeError("truncated protocol"));
        }
        let protocol = bytes[pos];
        pos += 1;

        // Timestamps
        if bytes.len() < pos + 8 {
            return Err(DecodeError("truncated timestamps"));
        }
        let started_at =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        pos += 4;
        let ended_at =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        pos += 4;

        // Bytes sent/received
        if bytes.len() < pos + 16 {
            return Err(DecodeError("truncated byte counters"));
        }
        let bytes_sent = u64::from_le_bytes([
            bytes[pos],
            bytes[pos + 1],
            bytes[pos + 2],
            bytes[pos + 3],
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]);
        pos += 8;
        let bytes_received = u64::from_le_bytes([
            bytes[pos],
            bytes[pos + 1],
            bytes[pos + 2],
            bytes[pos + 3],
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]);
        pos += 8;

        // TLS intercepted
        if pos >= bytes.len() {
            return Err(DecodeError("truncated tls flag"));
        }
        let tls_intercepted = bytes[pos] != 0;

        Ok(Self {
            connection_id,
            src_ip,
            src_port,
            dst_host,
            dst_port,
            protocol,
            started_at,
            ended_at,
            bytes_sent,
            bytes_received,
            tls_intercepted,
        })
    }
}

/// Proxy HTTP request record - intercepted HTTP request
#[derive(Debug, Clone)]
pub struct ProxyHttpRequestRecord {
    /// Reference to connection ID
    pub connection_id: u64,
    /// Request sequence number within connection
    pub request_seq: u32,
    /// HTTP method (GET, POST, etc)
    pub method: String,
    /// Request path (URL path + query string)
    pub path: String,
    /// HTTP version
    pub http_version: String,
    /// Host header value
    pub host: String,
    /// Request headers (key-value pairs)
    pub headers: Vec<(String, String)>,
    /// Request body (may be empty)
    pub body: Vec<u8>,
    /// Request timestamp
    pub timestamp: u32,
    /// Client IP address making this request
    pub client_addr: Option<String>,
}

impl ProxyHttpRequestRecord {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Connection ID + request seq
        buf.extend_from_slice(&self.connection_id.to_le_bytes());
        buf.extend_from_slice(&self.request_seq.to_le_bytes());

        // Method, path, version, host
        write_string(&mut buf, &self.method);
        write_string(&mut buf, &self.path);
        write_string(&mut buf, &self.http_version);
        write_string(&mut buf, &self.host);

        // Headers count + headers
        write_varu32(&mut buf, self.headers.len() as u32);
        for (key, value) in &self.headers {
            write_string(&mut buf, key);
            write_string(&mut buf, value);
        }

        // Body length + body
        write_varu32(&mut buf, self.body.len() as u32);
        buf.extend_from_slice(&self.body);

        // Timestamp
        buf.extend_from_slice(&self.timestamp.to_le_bytes());

        // Client addr
        write_optional_string(&mut buf, &self.client_addr);

        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < 12 {
            return Err(DecodeError("truncated http request record"));
        }

        let mut pos = 0;

        // Connection ID
        let connection_id = u64::from_le_bytes([
            bytes[pos],
            bytes[pos + 1],
            bytes[pos + 2],
            bytes[pos + 3],
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]);
        pos += 8;

        // Request seq
        let request_seq =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        pos += 4;

        // Strings
        let method = read_string(bytes, &mut pos)?;
        let path = read_string(bytes, &mut pos)?;
        let http_version = read_string(bytes, &mut pos)?;
        let host = read_string(bytes, &mut pos)?;

        // Headers
        let header_count = read_varu32(bytes, &mut pos)? as usize;
        let mut headers = Vec::with_capacity(header_count);
        for _ in 0..header_count {
            let key = read_string(bytes, &mut pos)?;
            let value = read_string(bytes, &mut pos)?;
            headers.push((key, value));
        }

        // Body
        let body_len = read_varu32(bytes, &mut pos)? as usize;
        if bytes.len() < pos + body_len {
            return Err(DecodeError("truncated body"));
        }
        let body = bytes[pos..pos + body_len].to_vec();
        pos += body_len;

        // Timestamp
        if bytes.len() < pos + 4 {
            return Err(DecodeError("truncated timestamp"));
        }
        let timestamp =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        pos += 4;

        // Client addr
        let client_addr = read_optional_string(bytes, &mut pos)?;

        Ok(Self {
            connection_id,
            request_seq,
            method,
            path,
            http_version,
            host,
            headers,
            body,
            timestamp,
            client_addr,
        })
    }
}

/// Proxy HTTP response record - intercepted HTTP response
#[derive(Debug, Clone)]
pub struct ProxyHttpResponseRecord {
    /// Reference to connection ID
    pub connection_id: u64,
    /// Request sequence number this responds to
    pub request_seq: u32,
    /// HTTP status code
    pub status_code: u16,
    /// Status text (e.g., "OK", "Not Found")
    pub status_text: String,
    /// HTTP version
    pub http_version: String,
    /// Response headers
    pub headers: Vec<(String, String)>,
    /// Response body (may be truncated for large responses)
    pub body: Vec<u8>,
    /// Response timestamp
    pub timestamp: u32,
    /// Content-Type header value
    pub content_type: Option<String>,
}

impl ProxyHttpResponseRecord {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Connection ID + request seq
        buf.extend_from_slice(&self.connection_id.to_le_bytes());
        buf.extend_from_slice(&self.request_seq.to_le_bytes());

        // Status
        buf.extend_from_slice(&self.status_code.to_le_bytes());
        write_string(&mut buf, &self.status_text);
        write_string(&mut buf, &self.http_version);

        // Headers
        write_varu32(&mut buf, self.headers.len() as u32);
        for (key, value) in &self.headers {
            write_string(&mut buf, key);
            write_string(&mut buf, value);
        }

        // Body
        write_varu32(&mut buf, self.body.len() as u32);
        buf.extend_from_slice(&self.body);

        // Timestamp
        buf.extend_from_slice(&self.timestamp.to_le_bytes());

        // Content type
        write_optional_string(&mut buf, &self.content_type);

        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < 14 {
            return Err(DecodeError("truncated http response record"));
        }

        let mut pos = 0;

        // Connection ID
        let connection_id = u64::from_le_bytes([
            bytes[pos],
            bytes[pos + 1],
            bytes[pos + 2],
            bytes[pos + 3],
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]);
        pos += 8;

        // Request seq
        let request_seq =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        pos += 4;

        // Status
        let status_code = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]);
        pos += 2;

        let status_text = read_string(bytes, &mut pos)?;
        let http_version = read_string(bytes, &mut pos)?;

        // Headers
        let header_count = read_varu32(bytes, &mut pos)? as usize;
        let mut headers = Vec::with_capacity(header_count);
        for _ in 0..header_count {
            let key = read_string(bytes, &mut pos)?;
            let value = read_string(bytes, &mut pos)?;
            headers.push((key, value));
        }

        // Body
        let body_len = read_varu32(bytes, &mut pos)? as usize;
        if bytes.len() < pos + body_len {
            return Err(DecodeError("truncated body"));
        }
        let body = bytes[pos..pos + body_len].to_vec();
        pos += body_len;

        // Timestamp
        if bytes.len() < pos + 4 {
            return Err(DecodeError("truncated timestamp"));
        }
        let timestamp =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        pos += 4;

        // Content type
        let content_type = read_optional_string(bytes, &mut pos)?;

        Ok(Self {
            connection_id,
            request_seq,
            status_code,
            status_text,
            http_version,
            headers,
            body,
            timestamp,
            content_type,
        })
    }
}

/// Proxy WebSocket frame record
#[derive(Debug, Clone)]
pub struct ProxyWebSocketRecord {
    /// Reference to connection ID
    pub connection_id: u64,
    /// Frame sequence number
    pub frame_seq: u64,
    /// Direction: 0 = client->server, 1 = server->client
    pub direction: u8,
    /// Frame opcode (0=continuation, 1=text, 2=binary, 8=close, 9=ping, 10=pong)
    pub opcode: u8,
    /// Frame payload (may be truncated)
    pub payload: Vec<u8>,
    /// Frame timestamp
    pub timestamp: u32,
}

impl ProxyWebSocketRecord {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Connection ID + frame seq
        buf.extend_from_slice(&self.connection_id.to_le_bytes());
        buf.extend_from_slice(&self.frame_seq.to_le_bytes());

        // Direction + opcode
        buf.push(self.direction);
        buf.push(self.opcode);

        // Payload
        write_varu32(&mut buf, self.payload.len() as u32);
        buf.extend_from_slice(&self.payload);

        // Timestamp
        buf.extend_from_slice(&self.timestamp.to_le_bytes());

        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < 18 {
            return Err(DecodeError("truncated websocket record"));
        }

        let mut pos = 0;

        // Connection ID
        let connection_id = u64::from_le_bytes([
            bytes[pos],
            bytes[pos + 1],
            bytes[pos + 2],
            bytes[pos + 3],
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]);
        pos += 8;

        // Frame seq
        let frame_seq = u64::from_le_bytes([
            bytes[pos],
            bytes[pos + 1],
            bytes[pos + 2],
            bytes[pos + 3],
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]);
        pos += 8;

        // Direction + opcode
        let direction = bytes[pos];
        pos += 1;
        let opcode = bytes[pos];
        pos += 1;

        // Payload
        let payload_len = read_varu32(bytes, &mut pos)? as usize;
        if bytes.len() < pos + payload_len {
            return Err(DecodeError("truncated payload"));
        }
        let payload = bytes[pos..pos + payload_len].to_vec();
        pos += payload_len;

        // Timestamp
        if bytes.len() < pos + 4 {
            return Err(DecodeError("truncated timestamp"));
        }
        let timestamp =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);

        Ok(Self {
            connection_id,
            frame_seq,
            direction,
            opcode,
            payload,
            timestamp,
        })
    }
}
