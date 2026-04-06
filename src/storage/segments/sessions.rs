//! SessionSegment - Storage for active shell session records
//!
//! Indexed by session ID for direct access and by target for listing sessions.

use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::primitives::encoding::{read_string, write_string, DecodeError};
use crate::storage::records::{SessionRecord, SessionStatus};

#[derive(Debug, Clone, Copy)]
struct SessionSegmentHeader {
    session_count: u32,
    directory_len: u64,
    payload_len: u64,
}

impl SessionSegmentHeader {
    const MAGIC: [u8; 4] = *b"SS01";
    const VERSION: u16 = 1;
    const SIZE: usize = 4 + 2 + 2 + 4 + 8 + 8;

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&Self::MAGIC);
        buf.extend_from_slice(&Self::VERSION.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // reserved
        buf.extend_from_slice(&self.session_count.to_le_bytes());
        buf.extend_from_slice(&self.directory_len.to_le_bytes());
        buf.extend_from_slice(&self.payload_len.to_le_bytes());
    }

    fn read(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < Self::SIZE {
            return Err(DecodeError("session header too small"));
        }
        if bytes[0..4] != Self::MAGIC {
            return Err(DecodeError("invalid session segment magic"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != Self::VERSION {
            return Err(DecodeError("unsupported session segment version"));
        }
        let session_count = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let directory_len = u64::from_le_bytes(bytes[12..20].try_into().unwrap());
        let payload_len = u64::from_le_bytes(bytes[20..28].try_into().unwrap());

        Ok(Self {
            session_count,
            directory_len,
            payload_len,
        })
    }
}

/// Segment for storing session records
#[derive(Debug, Default, Clone)]
pub struct SessionSegment {
    records: Vec<SessionRecord>,
    /// Index by session ID for O(1) lookup
    id_index: HashMap<String, usize>,
    /// Index by target for listing sessions per target
    target_index: HashMap<String, Vec<usize>>,
}

impl SessionSegment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, record: SessionRecord) {
        let idx = self.records.len();
        self.id_index.insert(record.id.clone(), idx);
        self.target_index
            .entry(record.target.clone())
            .or_default()
            .push(idx);
        self.records.push(record);
    }

    /// Update an existing session by ID
    pub fn update(&mut self, record: SessionRecord) {
        if let Some(&idx) = self.id_index.get(&record.id) {
            // Update target index if target changed
            let old_target = &self.records[idx].target;
            if old_target != &record.target {
                // Remove from old target index
                if let Some(indices) = self.target_index.get_mut(old_target) {
                    indices.retain(|&i| i != idx);
                }
                // Add to new target index
                self.target_index
                    .entry(record.target.clone())
                    .or_default()
                    .push(idx);
            }
            self.records[idx] = record;
        } else {
            // If not found, insert as new
            self.push(record);
        }
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Get session by ID
    pub fn get_by_id(&self, id: &str) -> Option<SessionRecord> {
        self.id_index.get(id).map(|&idx| self.records[idx].clone())
    }

    /// Get all sessions for a target
    pub fn get_by_target(&self, target: &str) -> Vec<SessionRecord> {
        if let Some(indices) = self.target_index.get(target) {
            indices.iter().map(|&i| self.records[i].clone()).collect()
        } else {
            Vec::new()
        }
    }

    /// Get all active sessions
    pub fn get_active(&self) -> Vec<SessionRecord> {
        self.records
            .iter()
            .filter(|r| r.status == SessionStatus::Active)
            .cloned()
            .collect()
    }

    /// Get all records
    pub fn all_records(&self) -> Vec<SessionRecord> {
        self.records.clone()
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Simple format: Directory lists session IDs with offsets, Payload has records
        let mut directory = Vec::new();
        let mut payload = Vec::new();

        // Sort by session ID for deterministic output
        let mut ids: Vec<_> = self.id_index.keys().collect();
        ids.sort();

        for id in ids {
            let &idx = &self.id_index[id];
            let start_offset = payload.len() as u64;

            let rec_bytes = self.records[idx].to_bytes();
            let rec_len = rec_bytes.len() as u64;
            payload.extend_from_slice(&rec_bytes);

            // Directory entry: ID string, offset, len
            write_string(&mut directory, id);
            directory.extend_from_slice(&start_offset.to_le_bytes());
            directory.extend_from_slice(&rec_len.to_le_bytes());
        }

        let header = SessionSegmentHeader {
            session_count: self.records.len() as u32,
            directory_len: directory.len() as u64,
            payload_len: payload.len() as u64,
        };

        header.write(&mut buf);
        buf.extend_from_slice(&directory);
        buf.extend_from_slice(&payload);

        buf
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < SessionSegmentHeader::SIZE {
            return Err(DecodeError("segment too small"));
        }
        let header = SessionSegmentHeader::read(bytes)?;

        let dir_start = SessionSegmentHeader::SIZE;
        let dir_end = dir_start + header.directory_len as usize;
        let payload_start = dir_end;
        let payload_end = payload_start + header.payload_len as usize;

        if bytes.len() < payload_end {
            return Err(DecodeError("segment truncated"));
        }

        let dir_bytes = &bytes[dir_start..dir_end];
        let payload_bytes = &bytes[payload_start..payload_end];

        let mut records = Vec::with_capacity(header.session_count as usize);
        let mut id_index = HashMap::with_capacity(header.session_count as usize);
        let mut target_index: HashMap<String, Vec<usize>> = HashMap::new();

        let mut dir_pos = 0;
        for _ in 0..header.session_count {
            let id = read_string(dir_bytes, &mut dir_pos)?.to_string();

            if dir_pos + 8 + 8 > dir_bytes.len() {
                return Err(DecodeError("directory truncated"));
            }

            let offset = u64::from_le_bytes(dir_bytes[dir_pos..dir_pos + 8].try_into().unwrap());
            dir_pos += 8;
            let len = u64::from_le_bytes(dir_bytes[dir_pos..dir_pos + 8].try_into().unwrap());
            dir_pos += 8;

            let rec_start = offset as usize;
            let rec_end = rec_start + len as usize;
            if rec_end > payload_bytes.len() {
                return Err(DecodeError("record out of bounds"));
            }

            let record = SessionRecord::from_bytes(&payload_bytes[rec_start..rec_end])?;
            let idx = records.len();

            id_index.insert(id, idx);
            target_index
                .entry(record.target.clone())
                .or_default()
                .push(idx);
            records.push(record);
        }

        Ok(Self {
            records,
            id_index,
            target_index,
        })
    }
}

/// Read-only view for memory-mapped access
pub struct SessionSegmentView {
    data: Arc<Vec<u8>>,
    directory: HashMap<String, (u64, u64)>, // ID -> (Offset, Len)
    payload_start: usize,
}

impl SessionSegmentView {
    pub fn from_arc(data: Arc<Vec<u8>>, offset: usize, len: usize) -> Result<Self, DecodeError> {
        let slice = &data[offset..offset + len];
        if slice.len() < SessionSegmentHeader::SIZE {
            return Err(DecodeError("view too small"));
        }
        let header = SessionSegmentHeader::read(slice)?;

        let dir_start = offset + SessionSegmentHeader::SIZE;
        let dir_end = dir_start + header.directory_len as usize;

        let payload_start = dir_end;

        if dir_end > offset + len {
            return Err(DecodeError("directory out of bounds"));
        }

        let dir_bytes = &data[dir_start..dir_end];
        let mut directory = HashMap::with_capacity(header.session_count as usize);
        let mut pos = 0;

        for _ in 0..header.session_count {
            let id = read_string(dir_bytes, &mut pos)?.to_string();
            let p_offset = u64::from_le_bytes(dir_bytes[pos..pos + 8].try_into().unwrap());
            pos += 8;
            let p_len = u64::from_le_bytes(dir_bytes[pos..pos + 8].try_into().unwrap());
            pos += 8;

            directory.insert(id, (p_offset, p_len));
        }

        Ok(Self {
            data,
            directory,
            payload_start,
        })
    }

    pub fn get_by_id(&self, id: &str) -> Result<Option<SessionRecord>, DecodeError> {
        if let Some(&(offset, len)) = self.directory.get(id) {
            let abs_start = self.payload_start + offset as usize;
            let abs_end = abs_start + len as usize;

            if abs_end > self.data.len() {
                return Err(DecodeError("payload read out of bounds"));
            }

            let rec = SessionRecord::from_bytes(&self.data[abs_start..abs_end])?;
            Ok(Some(rec))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_segment_roundtrip() {
        let mut segment = SessionSegment::new();

        segment.push(SessionRecord {
            id: "sess-001".to_string(),
            target: "192.168.1.1".to_string(),
            shell_type: "tcp".to_string(),
            local_port: 4444,
            remote_ip: "192.168.1.1".to_string(),
            status: SessionStatus::Active,
            created_at: 1700000000,
            last_activity: 1700000100,
        });

        segment.push(SessionRecord {
            id: "sess-002".to_string(),
            target: "192.168.1.1".to_string(),
            shell_type: "http".to_string(),
            local_port: 8080,
            remote_ip: "192.168.1.1".to_string(),
            status: SessionStatus::Closed,
            created_at: 1700000200,
            last_activity: 1700000300,
        });

        segment.push(SessionRecord {
            id: "sess-003".to_string(),
            target: "example.com".to_string(),
            shell_type: "dns".to_string(),
            local_port: 53,
            remote_ip: "93.184.216.34".to_string(),
            status: SessionStatus::Active,
            created_at: 1700000400,
            last_activity: 1700000500,
        });

        let bytes = segment.serialize();
        let restored = SessionSegment::deserialize(&bytes).unwrap();

        assert_eq!(restored.len(), 3);
        assert!(restored.get_by_id("sess-001").is_some());
        assert!(restored.get_by_id("sess-002").is_some());
        assert_eq!(restored.get_by_target("192.168.1.1").len(), 2);
        assert_eq!(restored.get_active().len(), 2);
    }
}
