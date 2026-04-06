use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::primitives::encoding::{
    read_string, read_varu32, write_string, write_varu32, DecodeError,
};
use crate::storage::records::VulnerabilityRecord;

#[derive(Debug, Clone, Copy)]
struct VulnSegmentHeader {
    tech_count: u32,
    record_count: u32,
    directory_len: u64,
    payload_len: u64,
}

impl VulnSegmentHeader {
    const MAGIC: [u8; 4] = *b"VL01";
    const VERSION: u16 = 1;
    const SIZE: usize = 4 + 2 + 2 + 4 + 4 + 8 + 8;

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&Self::MAGIC);
        buf.extend_from_slice(&Self::VERSION.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&self.tech_count.to_le_bytes());
        buf.extend_from_slice(&self.record_count.to_le_bytes());
        buf.extend_from_slice(&self.directory_len.to_le_bytes());
        buf.extend_from_slice(&self.payload_len.to_le_bytes());
    }

    fn read(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < Self::SIZE {
            return Err(DecodeError("vuln header too small"));
        }
        if bytes[0..4] != Self::MAGIC {
            return Err(DecodeError("invalid vuln segment magic"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != Self::VERSION {
            return Err(DecodeError("unsupported vuln segment version"));
        }
        let tech_count = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let record_count = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let directory_len = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let payload_len = u64::from_le_bytes(bytes[24..32].try_into().unwrap());

        Ok(Self {
            tech_count,
            record_count,
            directory_len,
            payload_len,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct VulnSegment {
    records: Vec<VulnerabilityRecord>,
    // Index: Technology -> indices in records vec
    index: HashMap<String, Vec<usize>>,
}

impl VulnSegment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, record: VulnerabilityRecord) {
        let idx = self.records.len();
        self.index
            .entry(record.technology.clone())
            .or_default()
            .push(idx);
        self.records.push(record);
    }

    pub fn get_by_tech(&self, tech: &str) -> Vec<VulnerabilityRecord> {
        if let Some(indices) = self.index.get(tech) {
            indices.iter().map(|&i| self.records[i].clone()).collect()
        } else {
            Vec::new()
        }
    }

    pub fn get_all(&self) -> Vec<VulnerabilityRecord> {
        self.records.clone()
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut directory = Vec::new();
        let mut payload = Vec::new();

        let mut techs: Vec<_> = self.index.keys().collect();
        techs.sort();

        for tech in techs {
            let indices = &self.index[tech];
            let start_offset = payload.len() as u64;

            let mut block = Vec::new();
            for &idx in indices {
                let rec_bytes = self.records[idx].to_bytes();
                write_varu32(&mut block, rec_bytes.len() as u32);
                block.extend_from_slice(&rec_bytes);
            }
            let block_len = block.len() as u64;
            payload.extend_from_slice(&block);

            write_string(&mut directory, tech);
            directory.extend_from_slice(&(indices.len() as u32).to_le_bytes());
            directory.extend_from_slice(&start_offset.to_le_bytes());
            directory.extend_from_slice(&block_len.to_le_bytes());
        }

        let header = VulnSegmentHeader {
            tech_count: self.index.len() as u32,
            record_count: self.records.len() as u32,
            directory_len: directory.len() as u64,
            payload_len: payload.len() as u64,
        };

        header.write(&mut buf);
        buf.extend_from_slice(&directory);
        buf.extend_from_slice(&payload);
        buf
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < VulnSegmentHeader::SIZE {
            return Err(DecodeError("segment too small"));
        }
        let header = VulnSegmentHeader::read(bytes)?;

        let dir_start = VulnSegmentHeader::SIZE;
        let dir_end = dir_start + header.directory_len as usize;
        let payload_start = dir_end;
        let payload_end = payload_start + header.payload_len as usize;

        if bytes.len() < payload_end {
            return Err(DecodeError("segment truncated"));
        }

        let dir_bytes = &bytes[dir_start..dir_end];
        let payload_bytes = &bytes[payload_start..payload_end];

        let mut records = Vec::with_capacity(header.record_count as usize);
        let mut index = HashMap::with_capacity(header.tech_count as usize);

        let mut dir_pos = 0;
        for _ in 0..header.tech_count {
            let tech = read_string(dir_bytes, &mut dir_pos)?.to_string();

            let count = u32::from_le_bytes(dir_bytes[dir_pos..dir_pos + 4].try_into().unwrap());
            dir_pos += 4;
            let offset = u64::from_le_bytes(dir_bytes[dir_pos..dir_pos + 8].try_into().unwrap());
            dir_pos += 8;
            let _len = u64::from_le_bytes(dir_bytes[dir_pos..dir_pos + 8].try_into().unwrap());
            dir_pos += 8;

            let mut tech_indices = Vec::with_capacity(count as usize);

            let block_start = offset as usize;
            let block = &payload_bytes;
            let mut block_pos = block_start;

            for _ in 0..count {
                let rec_len = read_varu32(block, &mut block_pos)? as usize;
                let rec_bytes = &block[block_pos..block_pos + rec_len];
                let record = VulnerabilityRecord::from_bytes(rec_bytes)?;
                block_pos += rec_len;

                records.push(record);
                tech_indices.push(records.len() - 1);
            }

            index.insert(tech, tech_indices);
        }

        Ok(Self { records, index })
    }
}

pub struct VulnSegmentView {
    data: Arc<Vec<u8>>,
    directory: HashMap<String, (u64, u64, u32)>,
    payload_start: usize,
}

impl VulnSegmentView {
    pub fn from_arc(data: Arc<Vec<u8>>, offset: usize, len: usize) -> Result<Self, DecodeError> {
        let slice = &data[offset..offset + len];
        let header = VulnSegmentHeader::read(slice)?;

        let dir_start = offset + VulnSegmentHeader::SIZE;
        let dir_end = dir_start + header.directory_len as usize;
        let payload_start = dir_end;

        let dir_bytes = &data[dir_start..dir_end];
        let mut directory = HashMap::with_capacity(header.tech_count as usize);
        let mut pos = 0;

        for _ in 0..header.tech_count {
            let tech = read_string(dir_bytes, &mut pos)?.to_string();
            let count = u32::from_le_bytes(dir_bytes[pos..pos + 4].try_into().unwrap());
            pos += 4;
            let p_offset = u64::from_le_bytes(dir_bytes[pos..pos + 8].try_into().unwrap());
            pos += 8;
            let p_len = u64::from_le_bytes(dir_bytes[pos..pos + 8].try_into().unwrap());
            pos += 8;
            directory.insert(tech, (p_offset, p_len, count));
        }

        Ok(Self {
            data,
            directory,
            payload_start,
        })
    }

    pub fn get_by_tech(&self, tech: &str) -> Result<Vec<VulnerabilityRecord>, DecodeError> {
        if let Some(&(offset, len, count)) = self.directory.get(tech) {
            let abs_start = self.payload_start + offset as usize;
            let abs_end = abs_start + len as usize;
            let block = &self.data[abs_start..abs_end];

            let mut pos = 0;
            let mut results = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let rec_len = read_varu32(block, &mut pos)? as usize;
                let rec = VulnerabilityRecord::from_bytes(&block[pos..pos + rec_len])?;
                pos += rec_len;
                results.push(rec);
            }
            Ok(results)
        } else {
            Ok(Vec::new())
        }
    }
}
