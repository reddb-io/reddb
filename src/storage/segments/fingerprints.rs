use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::primitives::encoding::{
    read_string, read_varu32, write_string, write_varu32, DecodeError,
};
use crate::storage::records::FingerprintRecord;

fn read_fixed<const N: usize>(
    bytes: &[u8],
    offset: usize,
    what: &'static str,
) -> Result<[u8; N], DecodeError> {
    let end = offset + N;
    let slice = bytes.get(offset..end).ok_or(DecodeError(what))?;
    let mut raw = [0u8; N];
    raw.copy_from_slice(slice);
    Ok(raw)
}

fn read_u16_le(bytes: &[u8], offset: usize, what: &'static str) -> Result<u16, DecodeError> {
    Ok(u16::from_le_bytes(read_fixed::<2>(bytes, offset, what)?))
}

fn read_u32_le(bytes: &[u8], offset: usize, what: &'static str) -> Result<u32, DecodeError> {
    Ok(u32::from_le_bytes(read_fixed::<4>(bytes, offset, what)?))
}

fn read_u64_le(bytes: &[u8], offset: usize, what: &'static str) -> Result<u64, DecodeError> {
    Ok(u64::from_le_bytes(read_fixed::<8>(bytes, offset, what)?))
}

#[derive(Debug, Clone)]
struct FingerprintDirEntry {
    host_hash: u64, // Use hash for fixed size directory entries? Or length prefixed string?
    // Let's use variable length string for simplicity and exact matching first.
    // Actually, for "Directory" standard, fixed size is better.
    // But keys are strings (hosts).
    // Let's stick to the pattern: Directory has metadata, Payload has data.
    // Directory maps Key -> Offset/Length.
    key_offset: u64, // Offset to the host string in the key section?
    // Simplification: Let's store keys in the directory block if they are short,
    // or pointers if they are long.
    // BETTER: Standard RedDB approach -> Directory maps (Key -> Range).
    payload_offset: u64,
    payload_len: u64,
    record_count: u32,
}

// Simple implementations for now: In-memory only logic that serializes to a linear format.
// Format:
// [Header]
// [Count]
// [Record 1 Length] [Record 1 Data]
// [Record 2 Length] [Record 2 Data]
// ...
//
// This is not indexed but it's a start.
// Wait, I should try to be consistent with `ports.rs`.
// `ports.rs` uses: Header -> Directory -> Payload.
// Directory Entry: Key (16 bytes IP) -> Offset/Len.
// Here Key is String. String keys are variable length.
//
// Let's define the format:
// [Header]
// [Directory Length]
// [Directory Entry 1] ...
//    [Host Length] [Host Bytes] [Record Count] [Offset] [Len]
// [Payload]

#[derive(Debug, Clone, Copy)]
struct FingerprintSegmentHeader {
    host_count: u32,
    record_count: u32,
    directory_len: u64,
    payload_len: u64,
}

impl FingerprintSegmentHeader {
    const MAGIC: [u8; 4] = *b"FG01";
    const VERSION: u16 = 1;
    const SIZE: usize = 4 + 2 + 2 + 4 + 4 + 8 + 8;

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&Self::MAGIC);
        buf.extend_from_slice(&Self::VERSION.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // reserved
        buf.extend_from_slice(&self.host_count.to_le_bytes());
        buf.extend_from_slice(&self.record_count.to_le_bytes());
        buf.extend_from_slice(&self.directory_len.to_le_bytes());
        buf.extend_from_slice(&self.payload_len.to_le_bytes());
    }

    fn read(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < Self::SIZE {
            return Err(DecodeError("fingerprint header too small"));
        }
        if bytes[0..4] != Self::MAGIC {
            return Err(DecodeError("invalid fingerprint segment magic"));
        }
        let version = read_u16_le(bytes, 4, "fingerprint header truncated (version)")?;
        if version != Self::VERSION {
            return Err(DecodeError("unsupported fingerprint segment version"));
        }
        let host_count = read_u32_le(bytes, 8, "fingerprint header truncated (host count)")?;
        let record_count = read_u32_le(bytes, 12, "fingerprint header truncated (record count)")?;
        let directory_len = read_u64_le(bytes, 16, "fingerprint header truncated (directory len)")?;
        let payload_len = read_u64_le(bytes, 24, "fingerprint header truncated (payload len)")?;

        Ok(Self {
            host_count,
            record_count,
            directory_len,
            payload_len,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct FingerprintSegment {
    records: Vec<FingerprintRecord>,
    // In-memory index: Host -> indices in records vec
    index: HashMap<String, Vec<usize>>,
}

impl FingerprintSegment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, record: FingerprintRecord) {
        let idx = self.records.len();
        self.index.entry(record.host.clone()).or_default().push(idx);
        self.records.push(record);
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn get_by_host(&self, host: &str) -> Vec<FingerprintRecord> {
        if let Some(indices) = self.index.get(host) {
            indices.iter().map(|&i| self.records[i].clone()).collect()
        } else {
            Vec::new()
        }
    }

    pub fn all_records(&self) -> Vec<FingerprintRecord> {
        self.records.clone()
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Prepare Directory and Payload
        let mut directory = Vec::new();
        let mut payload = Vec::new();

        // Sort hosts for deterministic output
        let mut hosts: Vec<_> = self.index.keys().collect();
        hosts.sort();

        for host in hosts {
            let indices = &self.index[host];
            let start_offset = payload.len() as u64;

            // Write records for this host into payload
            let mut block = Vec::new();
            for &idx in indices {
                let rec_bytes = self.records[idx].to_bytes();
                write_varu32(&mut block, rec_bytes.len() as u32);
                block.extend_from_slice(&rec_bytes);
            }
            let block_len = block.len() as u64;
            payload.extend_from_slice(&block);

            // Add directory entry
            write_string(&mut directory, host);
            directory.extend_from_slice(&(indices.len() as u32).to_le_bytes()); // count
            directory.extend_from_slice(&start_offset.to_le_bytes()); // offset
            directory.extend_from_slice(&block_len.to_le_bytes()); // len
        }

        let header = FingerprintSegmentHeader {
            host_count: self.index.len() as u32,
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
        if bytes.len() < FingerprintSegmentHeader::SIZE {
            return Err(DecodeError("segment too small"));
        }
        let header = FingerprintSegmentHeader::read(bytes)?;

        let dir_start = FingerprintSegmentHeader::SIZE;
        let dir_end = dir_start + header.directory_len as usize;
        let payload_start = dir_end;
        let payload_end = payload_start + header.payload_len as usize;

        if bytes.len() < payload_end {
            return Err(DecodeError("segment truncated"));
        }

        let dir_bytes = &bytes[dir_start..dir_end];
        let payload_bytes = &bytes[payload_start..payload_end];

        let mut records = Vec::with_capacity(header.record_count as usize);
        let mut index = HashMap::with_capacity(header.host_count as usize);

        let mut dir_pos = 0;
        for _ in 0..header.host_count {
            let host = read_string(dir_bytes, &mut dir_pos)?.to_string();

            if dir_pos + 4 + 8 + 8 > dir_bytes.len() {
                return Err(DecodeError("directory truncated"));
            }

            let count = read_u32_le(
                dir_bytes,
                dir_pos,
                "fingerprint directory truncated (count)",
            )?;
            dir_pos += 4;
            let offset = read_u64_le(
                dir_bytes,
                dir_pos,
                "fingerprint directory truncated (offset)",
            )?;
            dir_pos += 8;
            let len = read_u64_le(dir_bytes, dir_pos, "fingerprint directory truncated (len)")?;
            dir_pos += 8;

            let mut host_indices = Vec::with_capacity(count as usize);

            // Decode Payload Block
            let block_start = offset as usize;
            let block_end = block_start + len as usize;
            if block_end > payload_bytes.len() {
                return Err(DecodeError("payload block out of bounds"));
            }

            let mut block_pos = block_start;
            let block = &payload_bytes;

            for _ in 0..count {
                let rec_len = read_varu32(block, &mut block_pos)? as usize;
                if block_pos + rec_len > block_end {
                    return Err(DecodeError("record out of block bounds"));
                }
                let rec_bytes = &block[block_pos..block_pos + rec_len];
                let record = FingerprintRecord::from_bytes(rec_bytes)?;
                block_pos += rec_len;

                records.push(record);
                host_indices.push(records.len() - 1);
            }

            index.insert(host, host_indices);
        }

        Ok(Self { records, index })
    }
}

pub struct FingerprintSegmentView {
    data: Arc<Vec<u8>>,
    // We parse directory on load for O(1) lookups
    directory: HashMap<String, (u64, u64, u32)>, // Offset, Len, Count
    payload_start: usize,
}

impl FingerprintSegmentView {
    pub fn from_arc(data: Arc<Vec<u8>>, offset: usize, len: usize) -> Result<Self, DecodeError> {
        let slice = &data[offset..offset + len];
        if slice.len() < FingerprintSegmentHeader::SIZE {
            return Err(DecodeError("view too small"));
        }
        let header = FingerprintSegmentHeader::read(slice)?;

        let dir_start = offset + FingerprintSegmentHeader::SIZE;
        let dir_end = dir_start + header.directory_len as usize;

        let payload_start = dir_end;

        if dir_end > offset + len {
            return Err(DecodeError("directory out of bounds"));
        }

        let dir_bytes = &data[dir_start..dir_end];
        let mut directory = HashMap::with_capacity(header.host_count as usize);
        let mut pos = 0;

        for _ in 0..header.host_count {
            let host = read_string(dir_bytes, &mut pos)?.to_string();
            let count = read_u32_le(
                dir_bytes,
                pos,
                "fingerprint view directory truncated (count)",
            )?;
            pos += 4;
            let p_offset = read_u64_le(
                dir_bytes,
                pos,
                "fingerprint view directory truncated (offset)",
            )?;
            pos += 8;
            let p_len = read_u64_le(dir_bytes, pos, "fingerprint view directory truncated (len)")?;
            pos += 8;

            directory.insert(host, (p_offset, p_len, count));
        }

        Ok(Self {
            data,
            directory,
            payload_start,
        })
    }

    pub fn get_by_host(&self, host: &str) -> Result<Vec<FingerprintRecord>, DecodeError> {
        if let Some(&(offset, len, count)) = self.directory.get(host) {
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
                let rec = FingerprintRecord::from_bytes(&block[pos..pos + rec_len])?;
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

    #[test]
    fn fingerprint_segment_header_rejects_truncated_bytes() {
        let truncated = vec![0u8; FingerprintSegmentHeader::SIZE - 1];
        assert!(FingerprintSegmentHeader::read(&truncated).is_err());
    }
}
