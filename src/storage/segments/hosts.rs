use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use crate::storage::primitives::encoding::{read_varu32, write_varu32, DecodeError, IpKey};
use crate::storage::records::HostIntelRecord;

#[derive(Debug, Clone, Copy)]
struct HostDirEntry {
    key: IpKey,
    payload_offset: u64,
    payload_len: u64,
}

impl HostDirEntry {
    const SIZE: usize = 1 + 16 + 8 + 8;

    fn write_all(entries: &[Self], buf: &mut Vec<u8>) {
        for entry in entries {
            buf.push(entry.key.len);
            buf.extend_from_slice(&entry.key.bytes);
            buf.extend_from_slice(&entry.payload_offset.to_le_bytes());
            buf.extend_from_slice(&entry.payload_len.to_le_bytes());
        }
    }

    fn read_all(bytes: &[u8], count: usize) -> Result<Vec<Self>, DecodeError> {
        if bytes.len() != count * Self::SIZE {
            return Err(DecodeError("invalid host directory size"));
        }
        let mut entries = Vec::with_capacity(count);
        let mut offset = 0usize;
        for _ in 0..count {
            let len = bytes[offset];
            offset += 1;
            let mut raw = [0u8; 16];
            raw.copy_from_slice(&bytes[offset..offset + 16]);
            offset += 16;
            let payload_offset = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let payload_len = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            entries.push(Self {
                key: IpKey { bytes: raw, len },
                payload_offset,
                payload_len,
            });
        }
        Ok(entries)
    }
}

#[derive(Debug, Clone, Copy)]
struct HostSegmentHeader {
    record_count: u32,
    directory_len: u64,
    payload_len: u64,
}

impl HostSegmentHeader {
    const MAGIC: [u8; 4] = *b"HS01";
    const VERSION: u16 = 1;
    const SIZE: usize = 4 + 2 + 2 + 4 + 8 + 8;

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&Self::MAGIC);
        buf.extend_from_slice(&Self::VERSION.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&self.record_count.to_le_bytes());
        buf.extend_from_slice(&self.directory_len.to_le_bytes());
        buf.extend_from_slice(&self.payload_len.to_le_bytes());
    }

    fn read(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < Self::SIZE {
            return Err(DecodeError("host header too small"));
        }
        if bytes[0..4] != Self::MAGIC {
            return Err(DecodeError("invalid host segment magic"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != Self::VERSION {
            return Err(DecodeError("unsupported host segment version"));
        }
        let record_count = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let directory_len = u64::from_le_bytes(bytes[12..20].try_into().unwrap());
        let payload_len = u64::from_le_bytes(bytes[20..28].try_into().unwrap());
        Ok(Self {
            record_count,
            directory_len,
            payload_len,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct HostSegment {
    records: Vec<HostIntelRecord>,
    index: HashMap<IpKey, usize>,
    sorted: bool,
}

pub struct HostSegmentView {
    directory: Vec<HostDirEntry>,
    data: Arc<Vec<u8>>,
    payload_offset: usize,
    payload_len: usize,
}

impl HostSegment {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            index: HashMap::new(),
            sorted: true,
        }
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn insert(&mut self, record: HostIntelRecord) {
        let key = IpKey::from(&record.ip);
        match self.index.get(&key).cloned() {
            Some(idx) => {
                self.records[idx] = record;
            }
            None => {
                let idx = self.records.len();
                self.records.push(record);
                self.index.insert(key, idx);
            }
        }
        self.sorted = false;
    }

    pub fn get(&mut self, ip: IpAddr) -> Option<HostIntelRecord> {
        self.ensure_index();
        let key = IpKey::from(&ip);
        self.index.get(&key).map(|&idx| self.records[idx].clone())
    }

    pub fn all(&mut self) -> Vec<HostIntelRecord> {
        self.ensure_index();
        self.records.clone()
    }

    fn ensure_index(&mut self) {
        if self.sorted {
            return;
        }
        self.records.sort_by_key(|record| IpKey::from(&record.ip));
        self.index.clear();
        for (idx, record) in self.records.iter().enumerate() {
            self.index.insert(IpKey::from(&record.ip), idx);
        }
        self.sorted = true;
    }

    pub fn serialize(&mut self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.serialize_into(&mut buf);
        buf
    }

    pub fn serialize_into(&mut self, out: &mut Vec<u8>) {
        self.ensure_index();
        let mut directory = Vec::with_capacity(self.records.len());
        let mut payload = Vec::new();
        for record in &self.records {
            let key = IpKey::from(&record.ip);
            let start_offset = payload.len() as u64;
            let bytes = record.to_bytes();
            write_varu32(&mut payload, bytes.len() as u32);
            payload.extend_from_slice(&bytes);
            let block_len = payload.len() as u64 - start_offset;
            directory.push(HostDirEntry {
                key,
                payload_offset: start_offset,
                payload_len: block_len,
            });
        }

        let directory_len = (directory.len() * HostDirEntry::SIZE) as u64;
        let payload_len = payload.len() as u64;
        let header = HostSegmentHeader {
            record_count: self.records.len() as u32,
            directory_len,
            payload_len,
        };

        out.clear();
        out.reserve(HostSegmentHeader::SIZE + directory.len() * HostDirEntry::SIZE + payload.len());
        header.write(out);
        HostDirEntry::write_all(&directory, out);
        out.extend_from_slice(&payload);
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < HostSegmentHeader::SIZE {
            return Err(DecodeError("host segment too small"));
        }
        let header = HostSegmentHeader::read(bytes)?;

        let mut offset = HostSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("host directory overflow"))?;
        if dir_end > bytes.len() {
            return Err(DecodeError("host directory out of bounds"));
        }
        let directory_bytes = &bytes[offset..dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("host payload overflow"))?;
        if payload_end > bytes.len() {
            return Err(DecodeError("host payload out of bounds"));
        }
        let payload_bytes = &bytes[offset..payload_end];

        let directory = HostDirEntry::read_all(directory_bytes, header.record_count as usize)?;

        let mut records = Vec::with_capacity(header.record_count as usize);
        let mut index = HashMap::with_capacity(header.record_count as usize);

        for entry in directory {
            let mut cursor = entry.payload_offset as usize;
            let end = cursor + entry.payload_len as usize;
            if end > payload_bytes.len() {
                return Err(DecodeError("host payload slice out of bounds"));
            }
            let len = read_varu32(payload_bytes, &mut cursor)? as usize;
            if cursor + len > end {
                return Err(DecodeError("host record length mismatch"));
            }
            let record = HostIntelRecord::from_bytes(&payload_bytes[cursor..cursor + len])?;
            cursor += len;
            if cursor != end {
                return Err(DecodeError("host payload length mismatch"));
            }
            let key = IpKey::from(&record.ip);
            let idx = records.len();
            records.push(record);
            index.insert(key, idx);
        }

        Ok(Self {
            records,
            index,
            sorted: true,
        })
    }
}

impl HostSegment {
    fn to_record(entry: &HostIntelRecord) -> HostIntelRecord {
        entry.clone()
    }
}

impl HostSegmentView {
    pub fn from_arc(
        data: Arc<Vec<u8>>,
        segment_offset: usize,
        segment_len: usize,
    ) -> Result<Self, DecodeError> {
        if segment_offset + segment_len > data.len() {
            return Err(DecodeError("host segment out of bounds"));
        }
        let bytes = &data[segment_offset..segment_offset + segment_len];
        if bytes.len() < HostSegmentHeader::SIZE {
            return Err(DecodeError("host segment too small"));
        }
        let header = HostSegmentHeader::read(bytes)?;

        let mut offset = HostSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("host directory overflow"))?;
        if segment_offset + dir_end > data.len() {
            return Err(DecodeError("host directory out of bounds"));
        }
        let directory_bytes = &data[segment_offset + offset..segment_offset + dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("host payload overflow"))?;
        if segment_offset + payload_end > data.len() {
            return Err(DecodeError("host payload out of bounds"));
        }
        let payload_offset = segment_offset + offset;

        let mut directory = HostDirEntry::read_all(directory_bytes, header.record_count as usize)?;
        directory.sort_by_key(|entry| entry.key);

        Ok(Self {
            directory,
            data,
            payload_offset,
            payload_len: header.payload_len as usize,
        })
    }

    pub fn get(&self, ip: IpAddr) -> Result<Option<HostIntelRecord>, DecodeError> {
        let key = IpKey::from(&ip);
        match self.directory.binary_search_by_key(&key, |entry| entry.key) {
            Ok(idx) => {
                let entry = &self.directory[idx];
                let payload =
                    &self.data[self.payload_offset..self.payload_offset + self.payload_len];
                let mut cursor = entry.payload_offset as usize;
                let end = cursor + entry.payload_len as usize;
                if end > payload.len() {
                    return Err(DecodeError("host payload slice out of bounds"));
                }
                let len = read_varu32(payload, &mut cursor)? as usize;
                if cursor + len > end {
                    return Err(DecodeError("host record length mismatch"));
                }
                let record = HostIntelRecord::from_bytes(&payload[cursor..cursor + len])?;
                Ok(Some(record))
            }
            Err(_) => Ok(None),
        }
    }

    pub fn all(&self) -> Result<Vec<HostIntelRecord>, DecodeError> {
        let payload = &self.data[self.payload_offset..self.payload_offset + self.payload_len];
        let mut records = Vec::with_capacity(self.directory.len());
        for entry in &self.directory {
            let mut cursor = entry.payload_offset as usize;
            let end = cursor + entry.payload_len as usize;
            if end > payload.len() {
                return Err(DecodeError("host payload slice out of bounds"));
            }
            let len = read_varu32(payload, &mut cursor)? as usize;
            if cursor + len > end {
                return Err(DecodeError("host record length mismatch"));
            }
            let record = HostIntelRecord::from_bytes(&payload[cursor..cursor + len])?;
            records.push(record);
        }
        Ok(records)
    }
}

pub struct HostIter<'a> {
    segment: &'a mut HostSegment,
    pos: usize,
}

impl<'a> Iterator for HostIter<'a> {
    type Item = HostIntelRecord;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.segment.records.len() {
            return None;
        }
        let record = self.segment.records[self.pos].clone();
        self.pos += 1;
        Some(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn roundtrip_hosts() {
        let mut segment = HostSegment::new();
        segment.insert(HostIntelRecord {
            ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)),
            os_family: Some("linux".into()),
            confidence: 0.8,
            last_seen: 1,
            services: Vec::new(),
        });

        let mut encoded = segment.clone();
        let bytes = encoded.serialize();
        let mut decoded = HostSegment::deserialize(&bytes).expect("decode");
        let record = decoded
            .get(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)))
            .expect("record");
        assert_eq!(record.os_family.as_deref(), Some("linux"));
    }

    #[test]
    fn host_view_fetches_ip() {
        let mut segment = HostSegment::new();
        segment.insert(HostIntelRecord {
            ip: "203.0.113.1".parse().unwrap(),
            os_family: None,
            confidence: 0.5,
            last_seen: 2,
            services: Vec::new(),
        });

        let bytes = segment.serialize();
        let data = Arc::new(bytes);
        let view =
            HostSegmentView::from_arc(Arc::clone(&data), 0, data.len()).expect("view loaded");
        let record = view.get("203.0.113.1".parse().unwrap()).expect("result");
        assert!(record.is_some());
    }
}
