use std::cmp::Ordering;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use crate::storage::primitives::encoding::{read_varu32, write_varu32, DecodeError, IpKey};
use crate::storage::records::{PortScanRecord, PortStatus};

#[derive(Debug, Clone)]
struct PortEntry {
    ip: IpAddr,
    port: u16,
    status: PortStatus,
    service_id: u8,
    timestamp: u32,
}

impl From<PortScanRecord> for PortEntry {
    fn from(rec: PortScanRecord) -> Self {
        Self {
            ip: rec.ip,
            port: rec.port,
            status: rec.status,
            service_id: rec.service_id,
            timestamp: rec.timestamp,
        }
    }
}

impl From<&PortEntry> for PortScanRecord {
    fn from(entry: &PortEntry) -> Self {
        Self {
            ip: entry.ip,
            port: entry.port,
            status: entry.status,
            service_id: entry.service_id,
            timestamp: entry.timestamp,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct IpRange {
    start: usize,
    len: usize,
}

#[derive(Debug, Clone, Copy)]
struct PortDirEntry {
    key: IpKey,
    record_count: u32,
    payload_offset: u64,
    payload_len: u64,
}

impl PortDirEntry {
    const SIZE: usize = 1 + 16 + 4 + 8 + 8;

    fn write_all(entries: &[Self], buf: &mut Vec<u8>) {
        for entry in entries {
            buf.push(entry.key.len);
            buf.extend_from_slice(&entry.key.bytes);
            buf.extend_from_slice(&entry.record_count.to_le_bytes());
            buf.extend_from_slice(&entry.payload_offset.to_le_bytes());
            buf.extend_from_slice(&entry.payload_len.to_le_bytes());
        }
    }

    fn read_all(bytes: &[u8], count: usize) -> Result<Vec<Self>, DecodeError> {
        if bytes.len() != count * Self::SIZE {
            return Err(DecodeError("invalid port directory size"));
        }
        let mut entries = Vec::with_capacity(count);
        let mut offset = 0usize;
        for _ in 0..count {
            let len = bytes[offset];
            offset += 1;
            let mut raw = [0u8; 16];
            raw.copy_from_slice(&bytes[offset..offset + 16]);
            offset += 16;
            let record_count = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            offset += 4;
            let payload_offset = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let payload_len = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            entries.push(Self {
                key: IpKey { bytes: raw, len },
                record_count,
                payload_offset,
                payload_len,
            });
        }
        Ok(entries)
    }
}

#[derive(Debug, Clone, Copy)]
struct PortSegmentHeader {
    ip_count: u32,
    record_count: u32,
    directory_len: u64,
    payload_len: u64,
}

impl PortSegmentHeader {
    const MAGIC: [u8; 4] = *b"PT01";
    const VERSION: u16 = 1;
    const SIZE: usize = 4 + 2 + 2 + 4 + 4 + 8 + 8;

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&Self::MAGIC);
        buf.extend_from_slice(&Self::VERSION.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // reserved
        buf.extend_from_slice(&self.ip_count.to_le_bytes());
        buf.extend_from_slice(&self.record_count.to_le_bytes());
        buf.extend_from_slice(&self.directory_len.to_le_bytes());
        buf.extend_from_slice(&self.payload_len.to_le_bytes());
    }

    fn read(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < Self::SIZE {
            return Err(DecodeError("port header too small"));
        }
        if bytes[0..4] != Self::MAGIC {
            return Err(DecodeError("invalid port segment magic"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != Self::VERSION {
            return Err(DecodeError("unsupported port segment version"));
        }
        let ip_count = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let record_count = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let directory_len = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let payload_len = u64::from_le_bytes(bytes[24..32].try_into().unwrap());

        Ok(Self {
            ip_count,
            record_count,
            directory_len,
            payload_len,
        })
    }
}

/// Mutable builder + in-memory view for port scan data.
#[derive(Debug, Default, Clone)]
pub struct PortSegment {
    records: Vec<PortEntry>,
    index: HashMap<IpKey, IpRange>,
    sorted: bool,
}

pub struct PortSegmentView {
    data: Arc<Vec<u8>>,
    payload_offset: usize,
    payload_len: usize,
    directory: Vec<PortDirEntry>,
}

impl PortSegment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, record: PortScanRecord) {
        self.records.push(PortEntry::from(record));
        self.sorted = false;
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn ensure_index(&mut self) {
        if self.sorted {
            return;
        }
        self.records
            .sort_by(|a, b| match IpKey::from(&a.ip).cmp(&IpKey::from(&b.ip)) {
                Ordering::Equal => a.port.cmp(&b.port),
                other => other,
            });
        self.index.clear();
        let mut current_key = None;
        let mut start = 0usize;
        for (idx, entry) in self.records.iter().enumerate() {
            let key = IpKey::from(&entry.ip);
            match current_key {
                Some(active) if active == key => {}
                Some(active) => {
                    self.index.insert(
                        active,
                        IpRange {
                            start,
                            len: idx - start,
                        },
                    );
                    current_key = Some(key);
                    start = idx;
                }
                None => {
                    current_key = Some(key);
                    start = idx;
                }
            }
        }
        if let Some(active) = current_key {
            self.index.insert(
                active,
                IpRange {
                    start,
                    len: self.records.len() - start,
                },
            );
        }
        self.sorted = true;
    }

    pub fn find(&mut self, ip: IpAddr, port: u16) -> Option<PortScanRecord> {
        self.ensure_index();
        let key = IpKey::from(&ip);
        let range = self.index.get(&key)?;
        let slice = &self.records[range.start..range.start + range.len];
        let idx = slice.binary_search_by(|entry| entry.port.cmp(&port)).ok()?;
        Some(PortScanRecord::from(&slice[idx]))
    }

    pub fn get_open_ports(&mut self, ip: IpAddr) -> Vec<u16> {
        self.ensure_index();
        let key = IpKey::from(&ip);
        match self.index.get(&key) {
            Some(range) => self.records[range.start..range.start + range.len]
                .iter()
                .filter(|entry| matches!(entry.status, PortStatus::Open))
                .map(|entry| entry.port)
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn iter_ip(&mut self, ip: IpAddr) -> Vec<PortScanRecord> {
        self.ensure_index();
        let key = IpKey::from(&ip);
        match self.index.get(&key) {
            Some(range) => self.records[range.start..range.start + range.len]
                .iter()
                .map(PortScanRecord::from)
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn all_records(&mut self) -> Vec<PortScanRecord> {
        self.ensure_index();
        self.records.iter().map(PortScanRecord::from).collect()
    }

    pub fn iter(&mut self) -> PortIter<'_> {
        self.ensure_index();
        PortIter {
            segment: self,
            pos: 0,
        }
    }

    pub fn serialize(&mut self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.serialize_into(&mut buf);
        buf
    }

    pub fn serialize_into(&mut self, out: &mut Vec<u8>) {
        self.ensure_index();

        let mut entries: Vec<(IpKey, IpRange)> = self
            .index
            .iter()
            .map(|(key, range)| (*key, *range))
            .collect();
        entries.sort_by_key(|(key, _)| *key);

        let mut directory = Vec::with_capacity(entries.len());
        let mut payload = Vec::new();

        for (key, range) in entries {
            let start_offset = payload.len() as u64;
            let records_slice = &self.records[range.start..range.start + range.len];

            let mut block = Vec::new();
            write_varu32(&mut block, records_slice.len() as u32);
            for entry in records_slice {
                block.extend_from_slice(&entry.port.to_le_bytes());
                block.push(encode_status(entry.status));
                block.push(entry.service_id);
                block.extend_from_slice(&entry.timestamp.to_le_bytes());
            }

            let block_len = block.len() as u64;
            payload.extend_from_slice(&block);
            directory.push(PortDirEntry {
                key,
                record_count: records_slice.len() as u32,
                payload_offset: start_offset,
                payload_len: block_len,
            });
        }

        let directory_len = (directory.len() * PortDirEntry::SIZE) as u64;
        let payload_len = payload.len() as u64;

        let header = PortSegmentHeader {
            ip_count: directory.len() as u32,
            record_count: self.records.len() as u32,
            directory_len,
            payload_len,
        };

        out.clear();
        out.reserve(PortSegmentHeader::SIZE + directory.len() * PortDirEntry::SIZE + payload.len());
        header.write(out);
        PortDirEntry::write_all(&directory, out);
        out.extend_from_slice(&payload);
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < PortSegmentHeader::SIZE {
            return Err(DecodeError("port segment too small"));
        }
        let header = PortSegmentHeader::read(bytes)?;

        let mut offset = PortSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("port directory overflow"))?;
        if dir_end > bytes.len() {
            return Err(DecodeError("port directory out of bounds"));
        }
        let directory_bytes = &bytes[offset..dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("port payload overflow"))?;
        if payload_end > bytes.len() {
            return Err(DecodeError("port payload out of bounds"));
        }
        let payload_bytes = &bytes[offset..payload_end];

        let directory = PortDirEntry::read_all(directory_bytes, header.ip_count as usize)?;

        let mut records = Vec::with_capacity(header.record_count as usize);
        let mut index = HashMap::with_capacity(directory.len());

        for entry in &directory {
            let ip = entry.key.to_ip();
            let start_index = records.len();
            let decoded = decode_block_entries(
                payload_bytes,
                entry.payload_offset,
                entry.payload_len,
                entry.record_count,
                ip,
            )?;
            records.extend(decoded);
            index.insert(
                entry.key,
                IpRange {
                    start: start_index,
                    len: entry.record_count as usize,
                },
            );
        }

        Ok(Self {
            records,
            index,
            sorted: true,
        })
    }
}

impl PortSegmentView {
    pub fn from_arc(
        data: Arc<Vec<u8>>,
        segment_offset: usize,
        segment_len: usize,
    ) -> Result<Self, DecodeError> {
        if segment_offset + segment_len > data.len() {
            return Err(DecodeError("port segment out of bounds"));
        }

        let bytes = &data[segment_offset..segment_offset + segment_len];
        if bytes.len() < PortSegmentHeader::SIZE {
            return Err(DecodeError("port segment too small"));
        }
        let header = PortSegmentHeader::read(bytes)?;

        let mut offset = PortSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("port directory overflow"))?;
        if segment_offset + dir_end > data.len() {
            return Err(DecodeError("port directory out of bounds"));
        }
        let directory_bytes = &data[segment_offset + offset..segment_offset + dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("port payload overflow"))?;
        if segment_offset + payload_end > data.len() {
            return Err(DecodeError("port payload out of bounds"));
        }
        let payload_start = segment_offset + offset;
        let mut directory = PortDirEntry::read_all(directory_bytes, header.ip_count as usize)?;
        directory.sort_by_key(|entry| entry.key);

        Ok(Self {
            data,
            payload_offset: payload_start,
            payload_len: header.payload_len as usize,
            directory,
        })
    }

    pub fn records_for_ip(&self, ip: &IpAddr) -> Result<Vec<PortScanRecord>, DecodeError> {
        let key = IpKey::from(ip);
        match self.directory.binary_search_by_key(&key, |entry| entry.key) {
            Ok(idx) => {
                let entry = &self.directory[idx];
                let payload =
                    &self.data[self.payload_offset..self.payload_offset + self.payload_len];
                decode_block_records(
                    payload,
                    entry.payload_offset,
                    entry.payload_len,
                    entry.record_count,
                    *ip,
                )
            }
            Err(_) => Ok(Vec::new()),
        }
    }

    pub fn records_in_range(
        &self,
        start: &IpAddr,
        end: &IpAddr,
    ) -> Result<Vec<PortScanRecord>, DecodeError> {
        let start_key = IpKey::from(start);
        let end_key = IpKey::from(end);
        if start_key > end_key {
            return Ok(Vec::new());
        }

        let payload = &self.data[self.payload_offset..self.payload_offset + self.payload_len];
        let start_idx = match self
            .directory
            .binary_search_by_key(&start_key, |entry| entry.key)
        {
            Ok(idx) | Err(idx) => idx,
        };

        let mut results = Vec::new();
        for entry in self.directory[start_idx..].iter() {
            if entry.key > end_key {
                break;
            }
            let ip = entry.key.to_ip();
            let mut decoded = decode_block_records(
                payload,
                entry.payload_offset,
                entry.payload_len,
                entry.record_count,
                ip,
            )?;
            results.append(&mut decoded);
        }

        Ok(results)
    }
}

pub struct PortIter<'a> {
    segment: &'a mut PortSegment,
    pos: usize,
}

impl<'a> Iterator for PortIter<'a> {
    type Item = PortScanRecord;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.segment.records.len() {
            return None;
        }
        let record = PortScanRecord::from(&self.segment.records[self.pos]);
        self.pos += 1;
        Some(record)
    }
}

fn decode_block_entries(
    payload: &[u8],
    offset: u64,
    length: u64,
    expected_count: u32,
    ip: IpAddr,
) -> Result<Vec<PortEntry>, DecodeError> {
    let mut cursor = offset as usize;
    let end = cursor + length as usize;
    if end > payload.len() {
        return Err(DecodeError("port payload slice out of bounds"));
    }

    let count = read_varu32(payload, &mut cursor)? as usize;
    if count as u32 != expected_count {
        return Err(DecodeError("port record count mismatch"));
    }

    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        if cursor + 2 > end {
            return Err(DecodeError("unexpected eof (port value)"));
        }
        let port = u16::from_le_bytes(payload[cursor..cursor + 2].try_into().unwrap());
        cursor += 2;
        if cursor + 2 > end {
            return Err(DecodeError("unexpected eof (port status/service)"));
        }
        let status = decode_status(payload[cursor])?;
        let service_id = payload[cursor + 1];
        cursor += 2;
        if cursor + 4 > end {
            return Err(DecodeError("unexpected eof (port timestamp)"));
        }
        let timestamp = u32::from_le_bytes(payload[cursor..cursor + 4].try_into().unwrap());
        cursor += 4;
        records.push(PortEntry {
            ip,
            port,
            status,
            service_id,
            timestamp,
        });
    }

    if cursor != end {
        return Err(DecodeError("port payload length mismatch"));
    }

    Ok(records)
}

fn decode_block_records(
    payload: &[u8],
    offset: u64,
    length: u64,
    expected_count: u32,
    ip: IpAddr,
) -> Result<Vec<PortScanRecord>, DecodeError> {
    let entries = decode_block_entries(payload, offset, length, expected_count, ip)?;
    Ok(entries.iter().map(PortScanRecord::from).collect())
}

fn decode_status(byte: u8) -> Result<PortStatus, DecodeError> {
    match byte {
        0 => Ok(PortStatus::Open),
        1 => Ok(PortStatus::Closed),
        2 => Ok(PortStatus::Filtered),
        3 => Ok(PortStatus::OpenFiltered),
        _ => Err(DecodeError("invalid port status")),
    }
}

fn encode_status(status: PortStatus) -> u8 {
    match status {
        PortStatus::Open => 0,
        PortStatus::Closed => 1,
        PortStatus::Filtered => 2,
        PortStatus::OpenFiltered => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_segment_roundtrip() {
        let mut segment = PortSegment::new();
        segment.push(PortScanRecord {
            ip: "192.0.2.10".parse().unwrap(),
            port: 22,
            status: PortStatus::Open,
            service_id: 1,
            timestamp: 1_700_000_000,
        });
        segment.push(PortScanRecord {
            ip: "192.0.2.10".parse().unwrap(),
            port: 80,
            status: PortStatus::Closed,
            service_id: 2,
            timestamp: 1_700_000_100,
        });
        segment.push(PortScanRecord {
            ip: "2001:db8::1".parse().unwrap(),
            port: 443,
            status: PortStatus::Open,
            service_id: 3,
            timestamp: 1_700_000_200,
        });

        let bytes = segment.serialize();
        let mut decoded = PortSegment::deserialize(&bytes).expect("decode");

        assert_eq!(decoded.len(), 3);
        let open_ports = decoded.get_open_ports("192.0.2.10".parse().unwrap());
        assert_eq!(open_ports, vec![22]);
        let ipv6_ports = decoded.get_open_ports("2001:db8::1".parse().unwrap());
        assert_eq!(ipv6_ports, vec![443]);
    }

    #[test]
    fn port_segment_view_reads_ip() {
        let mut segment = PortSegment::new();
        segment.push(PortScanRecord {
            ip: "192.0.2.55".parse().unwrap(),
            port: 8080,
            status: PortStatus::Open,
            service_id: 10,
            timestamp: 12345,
        });
        segment.push(PortScanRecord {
            ip: "192.0.2.55".parse().unwrap(),
            port: 8443,
            status: PortStatus::Filtered,
            service_id: 11,
            timestamp: 12350,
        });

        let bytes = segment.serialize();
        let data = Arc::new(bytes);
        let view =
            PortSegmentView::from_arc(Arc::clone(&data), 0, data.len()).expect("view loaded");
        let records = view
            .records_for_ip(&"192.0.2.55".parse().unwrap())
            .expect("records");
        assert_eq!(records.len(), 2);
        assert!(records.iter().any(|r| r.port == 8080));
    }
}
