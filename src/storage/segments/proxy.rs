//! Proxy data segment - stores intercepted proxy connections and traffic
//!
//! This segment stores:
//! - Connection metadata (source/dest, timing, bytes transferred)
//! - HTTP requests and responses
//! - WebSocket frames
//!
//! Data is indexed by connection_id for fast lookups.

use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::primitives::encoding::{read_varu32, write_varu32, DecodeError};
use crate::storage::records::{
    ProxyConnectionRecord, ProxyHttpRequestRecord, ProxyHttpResponseRecord, ProxyWebSocketRecord,
};

/// Direction constants for WebSocket frames
pub const DIRECTION_CLIENT_TO_SERVER: u8 = 0;
pub const DIRECTION_SERVER_TO_CLIENT: u8 = 1;

/// Protocol constants
pub const PROTOCOL_TLS: u8 = 2;

// ============================================================================
// Connection Segment
// ============================================================================

#[derive(Debug, Clone, Copy)]
struct ConnectionDirEntry {
    connection_id: u64,
    payload_offset: u64,
    payload_len: u64,
}

impl ConnectionDirEntry {
    const SIZE: usize = 8 + 8 + 8; // connection_id + offset + len

    fn write_all(entries: &[Self], buf: &mut Vec<u8>) {
        for entry in entries {
            buf.extend_from_slice(&entry.connection_id.to_le_bytes());
            buf.extend_from_slice(&entry.payload_offset.to_le_bytes());
            buf.extend_from_slice(&entry.payload_len.to_le_bytes());
        }
    }

    fn read_all(bytes: &[u8], count: usize) -> Result<Vec<Self>, DecodeError> {
        if bytes.len() != count * Self::SIZE {
            return Err(DecodeError("invalid connection directory size"));
        }
        let mut entries = Vec::with_capacity(count);
        let mut offset = 0usize;
        for _ in 0..count {
            let connection_id = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let payload_offset = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let payload_len = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            entries.push(Self {
                connection_id,
                payload_offset,
                payload_len,
            });
        }
        Ok(entries)
    }
}

#[derive(Debug, Clone, Copy)]
struct ProxySegmentHeader {
    connection_count: u32,
    request_count: u32,
    response_count: u32,
    websocket_count: u32,
    connection_dir_len: u64,
    connection_payload_len: u64,
    request_payload_len: u64,
    response_payload_len: u64,
    websocket_payload_len: u64,
}

impl ProxySegmentHeader {
    const MAGIC: [u8; 4] = *b"PX01";
    const VERSION: u16 = 1;
    // 4 magic + 2 version + 2 reserved + 4*4 counts + 5*8 lengths = 60 bytes
    const SIZE: usize = 4 + 2 + 2 + 16 + 40;

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&Self::MAGIC);
        buf.extend_from_slice(&Self::VERSION.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // reserved
        buf.extend_from_slice(&self.connection_count.to_le_bytes());
        buf.extend_from_slice(&self.request_count.to_le_bytes());
        buf.extend_from_slice(&self.response_count.to_le_bytes());
        buf.extend_from_slice(&self.websocket_count.to_le_bytes());
        buf.extend_from_slice(&self.connection_dir_len.to_le_bytes());
        buf.extend_from_slice(&self.connection_payload_len.to_le_bytes());
        buf.extend_from_slice(&self.request_payload_len.to_le_bytes());
        buf.extend_from_slice(&self.response_payload_len.to_le_bytes());
        buf.extend_from_slice(&self.websocket_payload_len.to_le_bytes());
    }

    fn read(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < Self::SIZE {
            return Err(DecodeError("proxy header too small"));
        }
        if bytes[0..4] != Self::MAGIC {
            return Err(DecodeError("invalid proxy segment magic"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != Self::VERSION {
            return Err(DecodeError("unsupported proxy segment version"));
        }
        // bytes[6..8] reserved
        let connection_count = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let request_count = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let response_count = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
        let websocket_count = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        let connection_dir_len = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        let connection_payload_len = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
        let request_payload_len = u64::from_le_bytes(bytes[40..48].try_into().unwrap());
        let response_payload_len = u64::from_le_bytes(bytes[48..56].try_into().unwrap());
        let websocket_payload_len = u64::from_le_bytes(bytes[56..64].try_into().unwrap());

        Ok(Self {
            connection_count,
            request_count,
            response_count,
            websocket_count,
            connection_dir_len,
            connection_payload_len,
            request_payload_len,
            response_payload_len,
            websocket_payload_len,
        })
    }
}

/// Mutable builder + in-memory view for proxy data
#[derive(Debug, Default, Clone)]
pub struct ProxySegment {
    connections: Vec<ProxyConnectionRecord>,
    requests: Vec<ProxyHttpRequestRecord>,
    responses: Vec<ProxyHttpResponseRecord>,
    websockets: Vec<ProxyWebSocketRecord>,
    // Index by connection_id
    conn_index: HashMap<u64, usize>,
    sorted: bool,
}

impl ProxySegment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_connection(&mut self, record: ProxyConnectionRecord) {
        let idx = self.connections.len();
        self.conn_index.insert(record.connection_id, idx);
        self.connections.push(record);
        self.sorted = false;
    }

    pub fn push_request(&mut self, record: ProxyHttpRequestRecord) {
        self.requests.push(record);
        self.sorted = false;
    }

    pub fn push_response(&mut self, record: ProxyHttpResponseRecord) {
        self.responses.push(record);
        self.sorted = false;
    }

    pub fn push_websocket(&mut self, record: ProxyWebSocketRecord) {
        self.websockets.push(record);
        self.sorted = false;
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    pub fn request_count(&self) -> usize {
        self.requests.len()
    }

    pub fn response_count(&self) -> usize {
        self.responses.len()
    }

    pub fn websocket_count(&self) -> usize {
        self.websockets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
            && self.requests.is_empty()
            && self.responses.is_empty()
            && self.websockets.is_empty()
    }

    fn ensure_sorted(&mut self) {
        if self.sorted {
            return;
        }
        // Sort by connection_id
        self.connections.sort_by_key(|c| c.connection_id);
        self.requests
            .sort_by_key(|r| (r.connection_id, r.request_seq));
        self.responses
            .sort_by_key(|r| (r.connection_id, r.request_seq));
        self.websockets
            .sort_by_key(|w| (w.connection_id, w.frame_seq));

        // Rebuild index
        self.conn_index.clear();
        for (idx, conn) in self.connections.iter().enumerate() {
            self.conn_index.insert(conn.connection_id, idx);
        }
        self.sorted = true;
    }

    pub fn get_connection(&mut self, connection_id: u64) -> Option<&ProxyConnectionRecord> {
        self.ensure_sorted();
        self.conn_index
            .get(&connection_id)
            .map(|&idx| &self.connections[idx])
    }

    pub fn get_requests_for_connection(
        &mut self,
        connection_id: u64,
    ) -> Vec<&ProxyHttpRequestRecord> {
        self.ensure_sorted();
        self.requests
            .iter()
            .filter(|r| r.connection_id == connection_id)
            .collect()
    }

    pub fn get_responses_for_connection(
        &mut self,
        connection_id: u64,
    ) -> Vec<&ProxyHttpResponseRecord> {
        self.ensure_sorted();
        self.responses
            .iter()
            .filter(|r| r.connection_id == connection_id)
            .collect()
    }

    pub fn get_websockets_for_connection(
        &mut self,
        connection_id: u64,
    ) -> Vec<&ProxyWebSocketRecord> {
        self.ensure_sorted();
        self.websockets
            .iter()
            .filter(|w| w.connection_id == connection_id)
            .collect()
    }

    pub fn all_connections(&mut self) -> Vec<ProxyConnectionRecord> {
        self.ensure_sorted();
        self.connections.clone()
    }

    pub fn all_requests(&mut self) -> Vec<ProxyHttpRequestRecord> {
        self.ensure_sorted();
        self.requests.clone()
    }

    pub fn all_responses(&mut self) -> Vec<ProxyHttpResponseRecord> {
        self.ensure_sorted();
        self.responses.clone()
    }

    pub fn serialize(&mut self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.serialize_into(&mut buf);
        buf
    }

    pub fn serialize_into(&mut self, out: &mut Vec<u8>) {
        self.ensure_sorted();

        // Build connection directory and payload
        let mut conn_directory = Vec::with_capacity(self.connections.len());
        let mut conn_payload = Vec::new();

        for conn in &self.connections {
            let start_offset = conn_payload.len() as u64;
            let bytes = conn.to_bytes();
            conn_payload.extend_from_slice(&bytes);
            let payload_len = bytes.len() as u64;
            conn_directory.push(ConnectionDirEntry {
                connection_id: conn.connection_id,
                payload_offset: start_offset,
                payload_len,
            });
        }

        // Build request payload
        let mut request_payload = Vec::new();
        write_varu32(&mut request_payload, self.requests.len() as u32);
        for req in &self.requests {
            let bytes = req.to_bytes();
            write_varu32(&mut request_payload, bytes.len() as u32);
            request_payload.extend_from_slice(&bytes);
        }

        // Build response payload
        let mut response_payload = Vec::new();
        write_varu32(&mut response_payload, self.responses.len() as u32);
        for resp in &self.responses {
            let bytes = resp.to_bytes();
            write_varu32(&mut response_payload, bytes.len() as u32);
            response_payload.extend_from_slice(&bytes);
        }

        // Build websocket payload
        let mut websocket_payload = Vec::new();
        write_varu32(&mut websocket_payload, self.websockets.len() as u32);
        for ws in &self.websockets {
            let bytes = ws.to_bytes();
            write_varu32(&mut websocket_payload, bytes.len() as u32);
            websocket_payload.extend_from_slice(&bytes);
        }

        let header = ProxySegmentHeader {
            connection_count: self.connections.len() as u32,
            request_count: self.requests.len() as u32,
            response_count: self.responses.len() as u32,
            websocket_count: self.websockets.len() as u32,
            connection_dir_len: (conn_directory.len() * ConnectionDirEntry::SIZE) as u64,
            connection_payload_len: conn_payload.len() as u64,
            request_payload_len: request_payload.len() as u64,
            response_payload_len: response_payload.len() as u64,
            websocket_payload_len: websocket_payload.len() as u64,
        };

        out.clear();
        out.reserve(
            ProxySegmentHeader::SIZE
                + conn_directory.len() * ConnectionDirEntry::SIZE
                + conn_payload.len()
                + request_payload.len()
                + response_payload.len()
                + websocket_payload.len(),
        );

        header.write(out);
        ConnectionDirEntry::write_all(&conn_directory, out);
        out.extend_from_slice(&conn_payload);
        out.extend_from_slice(&request_payload);
        out.extend_from_slice(&response_payload);
        out.extend_from_slice(&websocket_payload);
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < ProxySegmentHeader::SIZE {
            return Err(DecodeError("proxy segment too small"));
        }
        let header = ProxySegmentHeader::read(bytes)?;

        let mut offset = ProxySegmentHeader::SIZE;

        // Read connection directory
        let dir_end = offset
            .checked_add(header.connection_dir_len as usize)
            .ok_or(DecodeError("connection directory overflow"))?;
        if dir_end > bytes.len() {
            return Err(DecodeError("connection directory out of bounds"));
        }
        let directory_bytes = &bytes[offset..dir_end];
        offset = dir_end;

        let directory =
            ConnectionDirEntry::read_all(directory_bytes, header.connection_count as usize)?;

        // Read connection payload
        let conn_payload_end = offset
            .checked_add(header.connection_payload_len as usize)
            .ok_or(DecodeError("connection payload overflow"))?;
        if conn_payload_end > bytes.len() {
            return Err(DecodeError("connection payload out of bounds"));
        }
        let conn_payload = &bytes[offset..conn_payload_end];
        offset = conn_payload_end;

        // Read request payload
        let req_payload_end = offset
            .checked_add(header.request_payload_len as usize)
            .ok_or(DecodeError("request payload overflow"))?;
        if req_payload_end > bytes.len() {
            return Err(DecodeError("request payload out of bounds"));
        }
        let request_payload = &bytes[offset..req_payload_end];
        offset = req_payload_end;

        // Read response payload
        let resp_payload_end = offset
            .checked_add(header.response_payload_len as usize)
            .ok_or(DecodeError("response payload overflow"))?;
        if resp_payload_end > bytes.len() {
            return Err(DecodeError("response payload out of bounds"));
        }
        let response_payload = &bytes[offset..resp_payload_end];
        offset = resp_payload_end;

        // Read websocket payload
        let ws_payload_end = offset
            .checked_add(header.websocket_payload_len as usize)
            .ok_or(DecodeError("websocket payload overflow"))?;
        if ws_payload_end > bytes.len() {
            return Err(DecodeError("websocket payload out of bounds"));
        }
        let websocket_payload = &bytes[offset..ws_payload_end];

        // Decode connections
        let mut connections = Vec::with_capacity(header.connection_count as usize);
        let mut conn_index = HashMap::with_capacity(header.connection_count as usize);

        for entry in &directory {
            let start = entry.payload_offset as usize;
            let end = start + entry.payload_len as usize;
            if end > conn_payload.len() {
                return Err(DecodeError("connection record out of bounds"));
            }
            let conn = ProxyConnectionRecord::from_bytes(&conn_payload[start..end])?;
            let idx = connections.len();
            conn_index.insert(conn.connection_id, idx);
            connections.push(conn);
        }

        // Decode requests
        let requests =
            decode_records::<ProxyHttpRequestRecord>(request_payload, header.request_count)?;

        // Decode responses
        let responses =
            decode_records::<ProxyHttpResponseRecord>(response_payload, header.response_count)?;

        // Decode websockets
        let websockets =
            decode_records::<ProxyWebSocketRecord>(websocket_payload, header.websocket_count)?;

        Ok(Self {
            connections,
            requests,
            responses,
            websockets,
            conn_index,
            sorted: true,
        })
    }
}

/// Read-only view over serialized proxy data
pub struct ProxySegmentView {
    data: Arc<Vec<u8>>,
    header: ProxySegmentHeader,
    directory: Vec<ConnectionDirEntry>,
    conn_payload_offset: usize,
    request_payload_offset: usize,
    response_payload_offset: usize,
    websocket_payload_offset: usize,
}

impl ProxySegmentView {
    pub fn from_arc(
        data: Arc<Vec<u8>>,
        segment_offset: usize,
        segment_len: usize,
    ) -> Result<Self, DecodeError> {
        if segment_offset + segment_len > data.len() {
            return Err(DecodeError("proxy segment out of bounds"));
        }

        let bytes = &data[segment_offset..segment_offset + segment_len];
        if bytes.len() < ProxySegmentHeader::SIZE {
            return Err(DecodeError("proxy segment too small"));
        }
        let header = ProxySegmentHeader::read(bytes)?;

        let mut offset = ProxySegmentHeader::SIZE;

        // Read directory
        let dir_end = offset
            .checked_add(header.connection_dir_len as usize)
            .ok_or(DecodeError("connection directory overflow"))?;
        if segment_offset + dir_end > data.len() {
            return Err(DecodeError("connection directory out of bounds"));
        }
        let directory_bytes = &data[segment_offset + offset..segment_offset + dir_end];
        let mut directory =
            ConnectionDirEntry::read_all(directory_bytes, header.connection_count as usize)?;
        directory.sort_by_key(|e| e.connection_id);
        offset = dir_end;

        let conn_payload_offset = segment_offset + offset;
        offset += header.connection_payload_len as usize;

        let request_payload_offset = segment_offset + offset;
        offset += header.request_payload_len as usize;

        let response_payload_offset = segment_offset + offset;
        offset += header.response_payload_len as usize;

        let websocket_payload_offset = segment_offset + offset;

        Ok(Self {
            data,
            header,
            directory,
            conn_payload_offset,
            request_payload_offset,
            response_payload_offset,
            websocket_payload_offset,
        })
    }

    pub fn get_connection(
        &self,
        connection_id: u64,
    ) -> Result<Option<ProxyConnectionRecord>, DecodeError> {
        match self
            .directory
            .binary_search_by_key(&connection_id, |e| e.connection_id)
        {
            Ok(idx) => {
                let entry = &self.directory[idx];
                let start = self.conn_payload_offset + entry.payload_offset as usize;
                let end = start + entry.payload_len as usize;
                if end > self.data.len() {
                    return Err(DecodeError("connection record out of bounds"));
                }
                let conn = ProxyConnectionRecord::from_bytes(&self.data[start..end])?;
                Ok(Some(conn))
            }
            Err(_) => Ok(None),
        }
    }

    pub fn connection_count(&self) -> u32 {
        self.header.connection_count
    }

    pub fn request_count(&self) -> u32 {
        self.header.request_count
    }

    pub fn response_count(&self) -> u32 {
        self.header.response_count
    }

    pub fn websocket_count(&self) -> u32 {
        self.header.websocket_count
    }

    pub fn all_connections(&self) -> Result<Vec<ProxyConnectionRecord>, DecodeError> {
        let mut connections = Vec::with_capacity(self.header.connection_count as usize);
        for entry in &self.directory {
            let start = self.conn_payload_offset + entry.payload_offset as usize;
            let end = start + entry.payload_len as usize;
            if end > self.data.len() {
                return Err(DecodeError("connection record out of bounds"));
            }
            let conn = ProxyConnectionRecord::from_bytes(&self.data[start..end])?;
            connections.push(conn);
        }
        Ok(connections)
    }

    pub fn all_requests(&self) -> Result<Vec<ProxyHttpRequestRecord>, DecodeError> {
        let payload = &self.data[self.request_payload_offset..];
        decode_records::<ProxyHttpRequestRecord>(payload, self.header.request_count)
    }

    pub fn all_responses(&self) -> Result<Vec<ProxyHttpResponseRecord>, DecodeError> {
        let payload = &self.data[self.response_payload_offset..];
        decode_records::<ProxyHttpResponseRecord>(payload, self.header.response_count)
    }

    pub fn all_websockets(&self) -> Result<Vec<ProxyWebSocketRecord>, DecodeError> {
        let payload = &self.data[self.websocket_payload_offset..];
        decode_records::<ProxyWebSocketRecord>(payload, self.header.websocket_count)
    }
}

/// Trait for records that can be decoded from bytes
trait FromBytes: Sized {
    fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError>;
}

impl FromBytes for ProxyHttpRequestRecord {
    fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        ProxyHttpRequestRecord::from_bytes(bytes)
    }
}

impl FromBytes for ProxyHttpResponseRecord {
    fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        ProxyHttpResponseRecord::from_bytes(bytes)
    }
}

impl FromBytes for ProxyWebSocketRecord {
    fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        ProxyWebSocketRecord::from_bytes(bytes)
    }
}

fn decode_records<T: FromBytes>(
    payload: &[u8],
    expected_count: u32,
) -> Result<Vec<T>, DecodeError> {
    if payload.is_empty() && expected_count == 0 {
        return Ok(Vec::new());
    }

    let mut cursor = 0usize;
    let count = read_varu32(payload, &mut cursor)? as usize;
    if count as u32 != expected_count {
        return Err(DecodeError("record count mismatch"));
    }

    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let len = read_varu32(payload, &mut cursor)? as usize;
        if cursor + len > payload.len() {
            return Err(DecodeError("record out of bounds"));
        }
        let record = T::from_bytes(&payload[cursor..cursor + len])?;
        cursor += len;
        records.push(record);
    }

    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn make_test_connection(id: u64) -> ProxyConnectionRecord {
        ProxyConnectionRecord {
            connection_id: id,
            src_ip: "192.168.1.100".parse::<IpAddr>().unwrap(),
            src_port: 50000 + (id as u16),
            dst_host: format!("example{}.com", id),
            dst_port: 443,
            protocol: PROTOCOL_TLS,
            started_at: 1700000000 + (id as u32),
            ended_at: 1700000100 + (id as u32),
            bytes_sent: 1024 * id,
            bytes_received: 2048 * id,
            tls_intercepted: true,
        }
    }

    fn make_test_request(conn_id: u64, seq: u32) -> ProxyHttpRequestRecord {
        ProxyHttpRequestRecord {
            connection_id: conn_id,
            request_seq: seq,
            method: "GET".to_string(),
            path: format!("/api/test{}", seq),
            http_version: "HTTP/1.1".to_string(),
            host: format!("example{}.com", conn_id),
            headers: vec![
                ("User-Agent".to_string(), "reddb/1.0".to_string()),
                ("Accept".to_string(), "*/*".to_string()),
            ],
            body: Vec::new(),
            timestamp: 1700000010 + seq,
            client_addr: Some("192.168.1.100:50000".to_string()),
        }
    }

    fn make_test_response(conn_id: u64, seq: u32) -> ProxyHttpResponseRecord {
        ProxyHttpResponseRecord {
            connection_id: conn_id,
            request_seq: seq,
            status_code: 200,
            status_text: "OK".to_string(),
            http_version: "HTTP/1.1".to_string(),
            headers: vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Content-Length".to_string(), "42".to_string()),
            ],
            body: b"{\"success\": true}".to_vec(),
            timestamp: 1700000015 + seq,
            content_type: Some("application/json".to_string()),
        }
    }

    fn make_test_websocket(conn_id: u64, seq: u64) -> ProxyWebSocketRecord {
        ProxyWebSocketRecord {
            connection_id: conn_id,
            frame_seq: seq,
            direction: if seq % 2 == 0 {
                DIRECTION_CLIENT_TO_SERVER
            } else {
                DIRECTION_SERVER_TO_CLIENT
            },
            opcode: 1, // Text frame
            payload: format!("message {}", seq).into_bytes(),
            timestamp: 1700000020 + (seq as u32),
        }
    }

    #[test]
    fn test_proxy_segment_empty() {
        let mut segment = ProxySegment::new();
        assert!(segment.is_empty());
        assert_eq!(segment.connection_count(), 0);
        assert_eq!(segment.request_count(), 0);
        assert_eq!(segment.response_count(), 0);
        assert_eq!(segment.websocket_count(), 0);
    }

    #[test]
    fn test_proxy_segment_push() {
        let mut segment = ProxySegment::new();
        segment.push_connection(make_test_connection(1));
        segment.push_request(make_test_request(1, 1));
        segment.push_response(make_test_response(1, 1));
        segment.push_websocket(make_test_websocket(1, 1));

        assert!(!segment.is_empty());
        assert_eq!(segment.connection_count(), 1);
        assert_eq!(segment.request_count(), 1);
        assert_eq!(segment.response_count(), 1);
        assert_eq!(segment.websocket_count(), 1);
    }

    #[test]
    fn test_proxy_segment_get_connection() {
        let mut segment = ProxySegment::new();
        segment.push_connection(make_test_connection(1));
        segment.push_connection(make_test_connection(2));
        segment.push_connection(make_test_connection(3));

        let conn = segment.get_connection(2);
        assert!(conn.is_some());
        assert_eq!(conn.unwrap().connection_id, 2);

        let conn = segment.get_connection(999);
        assert!(conn.is_none());
    }

    #[test]
    fn test_proxy_segment_get_requests_for_connection() {
        let mut segment = ProxySegment::new();
        segment.push_connection(make_test_connection(1));
        segment.push_request(make_test_request(1, 1));
        segment.push_request(make_test_request(1, 2));
        segment.push_request(make_test_request(2, 1)); // different connection

        let requests = segment.get_requests_for_connection(1);
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|r| r.connection_id == 1));
    }

    #[test]
    fn test_proxy_segment_get_responses_for_connection() {
        let mut segment = ProxySegment::new();
        segment.push_response(make_test_response(1, 1));
        segment.push_response(make_test_response(1, 2));
        segment.push_response(make_test_response(2, 1));

        let responses = segment.get_responses_for_connection(1);
        assert_eq!(responses.len(), 2);
        assert!(responses.iter().all(|r| r.connection_id == 1));
    }

    #[test]
    fn test_proxy_segment_get_websockets_for_connection() {
        let mut segment = ProxySegment::new();
        segment.push_websocket(make_test_websocket(1, 1));
        segment.push_websocket(make_test_websocket(1, 2));
        segment.push_websocket(make_test_websocket(1, 3));
        segment.push_websocket(make_test_websocket(2, 1));

        let websockets = segment.get_websockets_for_connection(1);
        assert_eq!(websockets.len(), 3);
        assert!(websockets.iter().all(|w| w.connection_id == 1));
    }

    #[test]
    fn test_proxy_segment_roundtrip_empty() {
        let mut segment = ProxySegment::new();
        let bytes = segment.serialize();
        let decoded = ProxySegment::deserialize(&bytes).expect("decode");

        assert!(decoded.is_empty());
    }

    #[test]
    fn test_proxy_segment_roundtrip_connections_only() {
        let mut segment = ProxySegment::new();
        segment.push_connection(make_test_connection(1));
        segment.push_connection(make_test_connection(2));
        segment.push_connection(make_test_connection(3));

        let bytes = segment.serialize();
        let mut decoded = ProxySegment::deserialize(&bytes).expect("decode");

        assert_eq!(decoded.connection_count(), 3);
        let conn = decoded.get_connection(2);
        assert!(conn.is_some());
        assert_eq!(conn.unwrap().connection_id, 2);
        assert_eq!(conn.unwrap().dst_host, "example2.com");
    }

    #[test]
    fn test_proxy_segment_roundtrip_full() {
        let mut segment = ProxySegment::new();

        // Add multiple connections
        for i in 1..=3 {
            segment.push_connection(make_test_connection(i));
        }

        // Add requests and responses for each connection
        for conn_id in 1..=3 {
            for seq in 1..=2 {
                segment.push_request(make_test_request(conn_id, seq));
                segment.push_response(make_test_response(conn_id, seq));
            }
        }

        // Add websocket frames
        for conn_id in 1..=2 {
            for seq in 1..=3 {
                segment.push_websocket(make_test_websocket(conn_id, seq));
            }
        }

        let bytes = segment.serialize();
        let mut decoded = ProxySegment::deserialize(&bytes).expect("decode");

        assert_eq!(decoded.connection_count(), 3);
        assert_eq!(decoded.request_count(), 6);
        assert_eq!(decoded.response_count(), 6);
        assert_eq!(decoded.websocket_count(), 6);

        // Verify connection
        let conn = decoded.get_connection(2).unwrap();
        assert_eq!(conn.dst_host, "example2.com");

        // Verify requests
        let requests = decoded.get_requests_for_connection(1);
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "GET");

        // Verify responses
        let responses = decoded.get_responses_for_connection(1);
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0].status_code, 200);

        // Verify websockets
        let websockets = decoded.get_websockets_for_connection(1);
        assert_eq!(websockets.len(), 3);
    }

    #[test]
    fn test_proxy_segment_view() {
        let mut segment = ProxySegment::new();
        segment.push_connection(make_test_connection(1));
        segment.push_connection(make_test_connection(2));
        segment.push_request(make_test_request(1, 1));
        segment.push_response(make_test_response(1, 1));
        segment.push_websocket(make_test_websocket(1, 1));

        let bytes = segment.serialize();
        let data = Arc::new(bytes);
        let view = ProxySegmentView::from_arc(Arc::clone(&data), 0, data.len()).expect("view");

        assert_eq!(view.connection_count(), 2);
        assert_eq!(view.request_count(), 1);
        assert_eq!(view.response_count(), 1);
        assert_eq!(view.websocket_count(), 1);

        // Test get_connection
        let conn = view.get_connection(1).expect("get").unwrap();
        assert_eq!(conn.connection_id, 1);

        let conn = view.get_connection(999).expect("get");
        assert!(conn.is_none());
    }

    #[test]
    fn test_proxy_segment_view_all_records() {
        let mut segment = ProxySegment::new();
        segment.push_connection(make_test_connection(1));
        segment.push_connection(make_test_connection(2));
        segment.push_request(make_test_request(1, 1));
        segment.push_request(make_test_request(2, 1));
        segment.push_response(make_test_response(1, 1));
        segment.push_websocket(make_test_websocket(1, 1));
        segment.push_websocket(make_test_websocket(1, 2));

        let bytes = segment.serialize();
        let data = Arc::new(bytes);
        let view = ProxySegmentView::from_arc(Arc::clone(&data), 0, data.len()).expect("view");

        let connections = view.all_connections().expect("all connections");
        assert_eq!(connections.len(), 2);

        let requests = view.all_requests().expect("all requests");
        assert_eq!(requests.len(), 2);

        let responses = view.all_responses().expect("all responses");
        assert_eq!(responses.len(), 1);

        let websockets = view.all_websockets().expect("all websockets");
        assert_eq!(websockets.len(), 2);
    }

    #[test]
    fn test_proxy_segment_sorting() {
        let mut segment = ProxySegment::new();

        // Add out of order
        segment.push_connection(make_test_connection(3));
        segment.push_connection(make_test_connection(1));
        segment.push_connection(make_test_connection(2));

        segment.push_request(make_test_request(2, 2));
        segment.push_request(make_test_request(1, 1));
        segment.push_request(make_test_request(2, 1));

        // After ensure_sorted, should be ordered
        let connections = segment.all_connections();
        assert_eq!(connections[0].connection_id, 1);
        assert_eq!(connections[1].connection_id, 2);
        assert_eq!(connections[2].connection_id, 3);

        let requests = segment.all_requests();
        assert_eq!(requests[0].connection_id, 1);
        assert_eq!(requests[1].connection_id, 2);
        assert_eq!(requests[1].request_seq, 1);
        assert_eq!(requests[2].connection_id, 2);
        assert_eq!(requests[2].request_seq, 2);
    }

    #[test]
    fn test_proxy_segment_large_bodies() {
        let mut segment = ProxySegment::new();
        segment.push_connection(make_test_connection(1));

        // Large request body
        let mut req = make_test_request(1, 1);
        req.body = vec![0xAB; 10000];
        segment.push_request(req);

        // Large response body
        let mut resp = make_test_response(1, 1);
        resp.body = vec![0xCD; 20000];
        segment.push_response(resp);

        // Large websocket payload
        let mut ws = make_test_websocket(1, 1);
        ws.payload = vec![0xEF; 5000];
        segment.push_websocket(ws);

        let bytes = segment.serialize();
        let mut decoded = ProxySegment::deserialize(&bytes).expect("decode");

        let requests = decoded.all_requests();
        assert_eq!(requests[0].body.len(), 10000);
        assert!(requests[0].body.iter().all(|&b| b == 0xAB));

        let responses = decoded.all_responses();
        assert_eq!(responses[0].body.len(), 20000);
        assert!(responses[0].body.iter().all(|&b| b == 0xCD));

        let websockets = decoded.get_websockets_for_connection(1);
        assert_eq!(websockets[0].payload.len(), 5000);
        assert!(websockets[0].payload.iter().all(|&b| b == 0xEF));
    }

    #[test]
    fn test_proxy_segment_header_roundtrip() {
        let header = ProxySegmentHeader {
            connection_count: 10,
            request_count: 25,
            response_count: 25,
            websocket_count: 100,
            connection_dir_len: 240,
            connection_payload_len: 1000,
            request_payload_len: 5000,
            response_payload_len: 10000,
            websocket_payload_len: 2000,
        };

        let mut buf = Vec::new();
        header.write(&mut buf);

        let decoded = ProxySegmentHeader::read(&buf).expect("decode header");
        assert_eq!(decoded.connection_count, 10);
        assert_eq!(decoded.request_count, 25);
        assert_eq!(decoded.response_count, 25);
        assert_eq!(decoded.websocket_count, 100);
        assert_eq!(decoded.connection_dir_len, 240);
        assert_eq!(decoded.connection_payload_len, 1000);
        assert_eq!(decoded.request_payload_len, 5000);
        assert_eq!(decoded.response_payload_len, 10000);
        assert_eq!(decoded.websocket_payload_len, 2000);
    }

    #[test]
    fn test_connection_dir_entry_roundtrip() {
        let entries = vec![
            ConnectionDirEntry {
                connection_id: 1,
                payload_offset: 0,
                payload_len: 100,
            },
            ConnectionDirEntry {
                connection_id: 2,
                payload_offset: 100,
                payload_len: 150,
            },
            ConnectionDirEntry {
                connection_id: 3,
                payload_offset: 250,
                payload_len: 200,
            },
        ];

        let mut buf = Vec::new();
        ConnectionDirEntry::write_all(&entries, &mut buf);

        let decoded = ConnectionDirEntry::read_all(&buf, 3).expect("decode");
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0].connection_id, 1);
        assert_eq!(decoded[1].payload_offset, 100);
        assert_eq!(decoded[2].payload_len, 200);
    }

    #[test]
    fn test_proxy_segment_invalid_magic() {
        let mut bytes = vec![0u8; ProxySegmentHeader::SIZE];
        bytes[0..4].copy_from_slice(b"XXXX"); // Invalid magic

        let result = ProxySegment::deserialize(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_proxy_segment_invalid_version() {
        let mut bytes = vec![0u8; ProxySegmentHeader::SIZE];
        bytes[0..4].copy_from_slice(&ProxySegmentHeader::MAGIC);
        bytes[4..6].copy_from_slice(&99u16.to_le_bytes()); // Invalid version

        let result = ProxySegment::deserialize(&bytes);
        assert!(result.is_err());
    }
}
