//! PlaybookSegment - Storage for playbook execution history
//!
//! Indexed by playbook name and target for querying execution history.

use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::primitives::encoding::{
    read_string, read_varu32, write_string, write_varu32, DecodeError,
};
use crate::storage::records::{PlaybookRunRecord, PlaybookStatus};

#[derive(Debug, Clone, Copy)]
struct PlaybookSegmentHeader {
    run_count: u32,
    playbook_count: u32,
    directory_len: u64,
    payload_len: u64,
}

impl PlaybookSegmentHeader {
    const MAGIC: [u8; 4] = *b"PB01";
    const VERSION: u16 = 1;
    const SIZE: usize = 4 + 2 + 2 + 4 + 4 + 8 + 8;

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&Self::MAGIC);
        buf.extend_from_slice(&Self::VERSION.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // reserved
        buf.extend_from_slice(&self.run_count.to_le_bytes());
        buf.extend_from_slice(&self.playbook_count.to_le_bytes());
        buf.extend_from_slice(&self.directory_len.to_le_bytes());
        buf.extend_from_slice(&self.payload_len.to_le_bytes());
    }

    fn read(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < Self::SIZE {
            return Err(DecodeError("playbook header too small"));
        }
        if bytes[0..4] != Self::MAGIC {
            return Err(DecodeError("invalid playbook segment magic"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != Self::VERSION {
            return Err(DecodeError("unsupported playbook segment version"));
        }
        let run_count = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let playbook_count = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let directory_len = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let payload_len = u64::from_le_bytes(bytes[24..32].try_into().unwrap());

        Ok(Self {
            run_count,
            playbook_count,
            directory_len,
            payload_len,
        })
    }
}

/// Segment for storing playbook execution history
#[derive(Debug, Default, Clone)]
pub struct PlaybookSegment {
    records: Vec<PlaybookRunRecord>,
    /// Index by playbook name -> indices
    playbook_index: HashMap<String, Vec<usize>>,
    /// Index by target -> indices
    target_index: HashMap<String, Vec<usize>>,
}

impl PlaybookSegment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, record: PlaybookRunRecord) {
        let idx = self.records.len();
        self.playbook_index
            .entry(record.playbook_name.clone())
            .or_default()
            .push(idx);
        self.target_index
            .entry(record.target.clone())
            .or_default()
            .push(idx);
        self.records.push(record);
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Get all runs of a specific playbook
    pub fn get_by_playbook(&self, name: &str) -> Vec<PlaybookRunRecord> {
        if let Some(indices) = self.playbook_index.get(name) {
            indices.iter().map(|&i| self.records[i].clone()).collect()
        } else {
            Vec::new()
        }
    }

    /// Get all playbook runs for a target
    pub fn get_by_target(&self, target: &str) -> Vec<PlaybookRunRecord> {
        if let Some(indices) = self.target_index.get(target) {
            indices.iter().map(|&i| self.records[i].clone()).collect()
        } else {
            Vec::new()
        }
    }

    /// Get completed runs only
    pub fn get_completed(&self) -> Vec<PlaybookRunRecord> {
        self.records
            .iter()
            .filter(|r| r.status == PlaybookStatus::Completed)
            .cloned()
            .collect()
    }

    /// Get failed runs only
    pub fn get_failed(&self) -> Vec<PlaybookRunRecord> {
        self.records
            .iter()
            .filter(|r| r.status == PlaybookStatus::Failed)
            .cloned()
            .collect()
    }

    /// Get currently running playbooks
    pub fn get_running(&self) -> Vec<PlaybookRunRecord> {
        self.records
            .iter()
            .filter(|r| r.status == PlaybookStatus::Running)
            .cloned()
            .collect()
    }

    /// Get all records
    pub fn all_records(&self) -> Vec<PlaybookRunRecord> {
        self.records.clone()
    }

    /// Get latest run for a playbook (by started_at timestamp)
    pub fn get_latest_run(&self, playbook_name: &str) -> Option<PlaybookRunRecord> {
        self.playbook_index.get(playbook_name).and_then(|indices| {
            indices
                .iter()
                .map(|&i| &self.records[i])
                .max_by_key(|r| r.started_at)
                .cloned()
        })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Directory indexed by playbook name
        let mut directory = Vec::new();
        let mut payload = Vec::new();

        // Sort playbook names for deterministic output
        let mut playbooks: Vec<_> = self.playbook_index.keys().collect();
        playbooks.sort();

        for playbook in playbooks {
            let indices = &self.playbook_index[playbook];
            let start_offset = payload.len() as u64;

            // Write records for this playbook into payload
            let mut block = Vec::new();
            for &idx in indices {
                let rec_bytes = self.records[idx].to_bytes();
                write_varu32(&mut block, rec_bytes.len() as u32);
                block.extend_from_slice(&rec_bytes);
            }
            let block_len = block.len() as u64;
            payload.extend_from_slice(&block);

            // Add directory entry
            write_string(&mut directory, playbook);
            directory.extend_from_slice(&(indices.len() as u32).to_le_bytes()); // count
            directory.extend_from_slice(&start_offset.to_le_bytes()); // offset
            directory.extend_from_slice(&block_len.to_le_bytes()); // len
        }

        let header = PlaybookSegmentHeader {
            run_count: self.records.len() as u32,
            playbook_count: self.playbook_index.len() as u32,
            directory_len: directory.len() as u64,
            payload_len: payload.len() as u64,
        };

        header.write(&mut buf);
        buf.extend_from_slice(&directory);
        buf.extend_from_slice(&payload);

        buf
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < PlaybookSegmentHeader::SIZE {
            return Err(DecodeError("segment too small"));
        }
        let header = PlaybookSegmentHeader::read(bytes)?;

        let dir_start = PlaybookSegmentHeader::SIZE;
        let dir_end = dir_start + header.directory_len as usize;
        let payload_start = dir_end;
        let payload_end = payload_start + header.payload_len as usize;

        if bytes.len() < payload_end {
            return Err(DecodeError("segment truncated"));
        }

        let dir_bytes = &bytes[dir_start..dir_end];
        let payload_bytes = &bytes[payload_start..payload_end];

        let mut records = Vec::with_capacity(header.run_count as usize);
        let mut playbook_index = HashMap::with_capacity(header.playbook_count as usize);
        let mut target_index: HashMap<String, Vec<usize>> = HashMap::new();

        let mut dir_pos = 0;
        for _ in 0..header.playbook_count {
            let playbook_name = read_string(dir_bytes, &mut dir_pos)?.to_string();

            if dir_pos + 4 + 8 + 8 > dir_bytes.len() {
                return Err(DecodeError("directory truncated"));
            }

            let count = u32::from_le_bytes(dir_bytes[dir_pos..dir_pos + 4].try_into().unwrap());
            dir_pos += 4;
            let offset = u64::from_le_bytes(dir_bytes[dir_pos..dir_pos + 8].try_into().unwrap());
            dir_pos += 8;
            let len = u64::from_le_bytes(dir_bytes[dir_pos..dir_pos + 8].try_into().unwrap());
            dir_pos += 8;

            let mut pb_indices = Vec::with_capacity(count as usize);

            // Decode Payload Block
            let block_start = offset as usize;
            let block_end = block_start + len as usize;
            if block_end > payload_bytes.len() {
                return Err(DecodeError("payload block out of bounds"));
            }

            let mut block_pos = block_start;

            for _ in 0..count {
                let rec_len = read_varu32(payload_bytes, &mut block_pos)? as usize;
                if block_pos + rec_len > block_end {
                    return Err(DecodeError("record out of block bounds"));
                }
                let rec_bytes = &payload_bytes[block_pos..block_pos + rec_len];
                let record = PlaybookRunRecord::from_bytes(rec_bytes)?;
                block_pos += rec_len;

                let idx = records.len();
                target_index
                    .entry(record.target.clone())
                    .or_default()
                    .push(idx);
                records.push(record);
                pb_indices.push(idx);
            }

            playbook_index.insert(playbook_name, pb_indices);
        }

        Ok(Self {
            records,
            playbook_index,
            target_index,
        })
    }
}

/// Read-only view for memory-mapped access
pub struct PlaybookSegmentView {
    data: Arc<Vec<u8>>,
    directory: HashMap<String, (u64, u64, u32)>, // PlaybookName -> (Offset, Len, Count)
    payload_start: usize,
}

impl PlaybookSegmentView {
    pub fn from_arc(data: Arc<Vec<u8>>, offset: usize, len: usize) -> Result<Self, DecodeError> {
        let slice = &data[offset..offset + len];
        if slice.len() < PlaybookSegmentHeader::SIZE {
            return Err(DecodeError("view too small"));
        }
        let header = PlaybookSegmentHeader::read(slice)?;

        let dir_start = offset + PlaybookSegmentHeader::SIZE;
        let dir_end = dir_start + header.directory_len as usize;

        let payload_start = dir_end;

        if dir_end > offset + len {
            return Err(DecodeError("directory out of bounds"));
        }

        let dir_bytes = &data[dir_start..dir_end];
        let mut directory = HashMap::with_capacity(header.playbook_count as usize);
        let mut pos = 0;

        for _ in 0..header.playbook_count {
            let playbook = read_string(dir_bytes, &mut pos)?.to_string();
            let count = u32::from_le_bytes(dir_bytes[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let p_offset = u64::from_le_bytes(dir_bytes[pos..pos + 8].try_into().unwrap());
            pos += 8;
            let p_len = u64::from_le_bytes(dir_bytes[pos..pos + 8].try_into().unwrap());
            pos += 8;

            directory.insert(playbook, (p_offset, p_len, count));
        }

        Ok(Self {
            data,
            directory,
            payload_start,
        })
    }

    pub fn get_by_playbook(&self, name: &str) -> Result<Vec<PlaybookRunRecord>, DecodeError> {
        if let Some(&(offset, len, count)) = self.directory.get(name) {
            let abs_start = self.payload_start + offset as usize;
            let abs_end = abs_start + len as usize;

            if abs_end > self.data.len() {
                return Err(DecodeError("payload read out of bounds"));
            }

            let block = &self.data[abs_start..abs_end];
            let mut pos = 0;
            let mut results = Vec::with_capacity(count as usize);

            for _ in 0..count {
                let rec_len = read_varu32(block, &mut pos)? as usize;
                if pos + rec_len > block.len() {
                    return Err(DecodeError("record read out of bounds"));
                }
                let rec = PlaybookRunRecord::from_bytes(&block[pos..pos + rec_len])?;
                pos += rec_len;
                results.push(rec);
            }

            Ok(results)
        } else {
            Ok(Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::records::StepResult;

    #[test]
    fn test_playbook_segment_roundtrip() {
        let mut segment = PlaybookSegment::new();

        segment.push(PlaybookRunRecord {
            playbook_name: "web-app-pentest".to_string(),
            target: "example.com".to_string(),
            status: PlaybookStatus::Completed,
            current_phase: 5,
            started_at: 1700000000,
            completed_at: Some(1700001000),
            results: vec![
                StepResult {
                    name: "port_scan".to_string(),
                    status: "success".to_string(),
                    output: Some("Found 5 open ports".to_string()),
                },
                StepResult {
                    name: "vuln_scan".to_string(),
                    status: "success".to_string(),
                    output: Some("Found 2 CVEs".to_string()),
                },
            ],
        });

        segment.push(PlaybookRunRecord {
            playbook_name: "web-app-pentest".to_string(),
            target: "test.com".to_string(),
            status: PlaybookStatus::Failed,
            current_phase: 2,
            started_at: 1700002000,
            completed_at: Some(1700002500),
            results: vec![StepResult {
                name: "port_scan".to_string(),
                status: "failed".to_string(),
                output: Some("Connection timeout".to_string()),
            }],
        });

        segment.push(PlaybookRunRecord {
            playbook_name: "linux-privesc".to_string(),
            target: "192.168.1.50".to_string(),
            status: PlaybookStatus::Running,
            current_phase: 3,
            started_at: 1700003000,
            completed_at: None,
            results: vec![],
        });

        let bytes = segment.serialize();
        let restored = PlaybookSegment::deserialize(&bytes).unwrap();

        assert_eq!(restored.len(), 3);
        assert_eq!(restored.get_by_playbook("web-app-pentest").len(), 2);
        assert_eq!(restored.get_by_playbook("linux-privesc").len(), 1);
        assert_eq!(restored.get_by_target("example.com").len(), 1);
        assert_eq!(restored.get_completed().len(), 1);
        assert_eq!(restored.get_failed().len(), 1);
        assert_eq!(restored.get_running().len(), 1);

        let latest = restored.get_latest_run("web-app-pentest").unwrap();
        assert_eq!(latest.target, "test.com");
    }
}
