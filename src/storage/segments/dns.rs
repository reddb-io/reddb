use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::primitives::encoding::{
    read_string, read_varu32, write_string, write_varu32, DecodeError,
};
use crate::storage::records::{DnsRecordData, DnsRecordType};
use crate::storage::segments::utils::StringTable;

fn encode_string_table(table: &StringTable) -> Vec<u8> {
    let mut buf = Vec::new();
    write_varu32(&mut buf, table.len() as u32);
    for value in table.entries() {
        write_string(&mut buf, value);
    }
    buf
}

fn decode_string_table(bytes: &[u8]) -> Result<StringTable, DecodeError> {
    let mut pos = 0usize;
    let count = read_varu32(bytes, &mut pos)? as usize;
    let mut table = StringTable::new();
    for _ in 0..count {
        let value = read_string(bytes, &mut pos)?;
        table.intern(value);
    }
    Ok(table)
}

#[derive(Debug, Clone)]
struct Entry {
    domain_id: u32,
    value_id: u32,
    record_type: DnsRecordType,
    ttl: u32,
    timestamp: u32,
}

#[derive(Debug, Clone, Copy)]
struct DomainRange {
    start: usize,
    len: usize,
}

#[derive(Debug, Clone, Copy)]
struct DnsDirEntry {
    domain_id: u32,
    record_count: u32,
    payload_offset: u64,
    payload_len: u64,
}

impl DnsDirEntry {
    const SIZE: usize = 4 + 4 + 8 + 8;

    fn write_all(entries: &[Self], buf: &mut Vec<u8>) {
        for entry in entries {
            buf.extend_from_slice(&entry.domain_id.to_le_bytes());
            buf.extend_from_slice(&entry.record_count.to_le_bytes());
            buf.extend_from_slice(&entry.payload_offset.to_le_bytes());
            buf.extend_from_slice(&entry.payload_len.to_le_bytes());
        }
    }

    fn read_all(bytes: &[u8], count: usize) -> Result<Vec<Self>, DecodeError> {
        if bytes.len() != count * Self::SIZE {
            return Err(DecodeError("invalid dns directory size"));
        }
        let mut entries = Vec::with_capacity(count);
        let mut offset = 0usize;
        for _ in 0..count {
            let domain_id = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            offset += 4;
            let record_count = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            offset += 4;
            let payload_offset = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let payload_len = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            entries.push(Self {
                domain_id,
                record_count,
                payload_offset,
                payload_len,
            });
        }
        Ok(entries)
    }
}

#[derive(Debug, Clone, Copy)]
struct DnsSegmentHeader {
    domain_count: u32,
    record_count: u32,
    directory_len: u64,
    payload_len: u64,
    strings_len: u64,
}

impl DnsSegmentHeader {
    const MAGIC: [u8; 4] = *b"DN01";
    const VERSION: u16 = 1;
    const SIZE: usize = 4 + 2 + 2 + 4 + 4 + 8 + 8 + 8;

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&Self::MAGIC);
        buf.extend_from_slice(&Self::VERSION.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // reserved
        buf.extend_from_slice(&self.domain_count.to_le_bytes());
        buf.extend_from_slice(&self.record_count.to_le_bytes());
        buf.extend_from_slice(&self.directory_len.to_le_bytes());
        buf.extend_from_slice(&self.payload_len.to_le_bytes());
        buf.extend_from_slice(&self.strings_len.to_le_bytes());
    }

    fn read(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < Self::SIZE {
            return Err(DecodeError("dns header too small"));
        }
        if bytes[0..4] != Self::MAGIC {
            return Err(DecodeError("invalid dns segment magic"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != Self::VERSION {
            return Err(DecodeError("unsupported dns segment version"));
        }

        let domain_count = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let record_count = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let directory_len = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let payload_len = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        let strings_len = u64::from_le_bytes(bytes[32..40].try_into().unwrap());

        Ok(Self {
            domain_count,
            record_count,
            directory_len,
            payload_len,
            strings_len,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct DnsSegment {
    strings: StringTable,
    records: Vec<Entry>,
    domain_index: HashMap<u32, DomainRange>,
    sorted: bool,
}

pub struct DnsSegmentView {
    strings: StringTable,
    directory: Vec<DnsDirEntry>,
    data: Arc<Vec<u8>>,
    payload_offset: usize,
    payload_len: usize,
}

impl DnsSegment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn insert(&mut self, record: DnsRecordData) {
        let domain_id = self.strings.intern(&record.domain);
        let value_id = self.strings.intern(&record.value);
        self.records.push(Entry {
            domain_id,
            value_id,
            record_type: record.record_type,
            ttl: record.ttl,
            timestamp: record.timestamp,
        });
        self.sorted = false;
    }

    fn ensure_index(&mut self) {
        if self.sorted {
            return;
        }

        self.records
            .sort_by(|a, b| match a.domain_id.cmp(&b.domain_id) {
                std::cmp::Ordering::Equal => {
                    match (a.record_type as u8).cmp(&(b.record_type as u8)) {
                        std::cmp::Ordering::Equal => a.value_id.cmp(&b.value_id),
                        other => other,
                    }
                }
                other => other,
            });

        self.domain_index.clear();
        let mut current: Option<u32> = None;
        let mut start = 0usize;
        for (idx, entry) in self.records.iter().enumerate() {
            if current == Some(entry.domain_id) {
                continue;
            }
            if let Some(active) = current {
                self.domain_index.insert(
                    active,
                    DomainRange {
                        start,
                        len: idx - start,
                    },
                );
                start = idx;
            } else {
                start = idx;
            }
            current = Some(entry.domain_id);
        }
        if let Some(active) = current {
            self.domain_index.insert(
                active,
                DomainRange {
                    start,
                    len: self.records.len() - start,
                },
            );
        }
        self.sorted = true;
    }

    pub fn records_for_domain(&mut self, domain: &str) -> Vec<DnsRecordData> {
        self.ensure_index();
        let Some(domain_id) = self.strings.get_id(domain) else {
            return Vec::new();
        };
        let Some(range) = self.domain_index.get(&domain_id) else {
            return Vec::new();
        };

        self.records[range.start..range.start + range.len]
            .iter()
            .map(|entry| DnsRecordData {
                domain: self.strings.get(entry.domain_id).to_string(),
                record_type: entry.record_type,
                value: self.strings.get(entry.value_id).to_string(),
                ttl: entry.ttl,
                timestamp: entry.timestamp,
            })
            .collect()
    }

    pub fn all_records(&mut self) -> Vec<DnsRecordData> {
        self.ensure_index();
        self.records
            .iter()
            .map(|entry| DnsRecordData {
                domain: self.strings.get(entry.domain_id).to_string(),
                record_type: entry.record_type,
                value: self.strings.get(entry.value_id).to_string(),
                ttl: entry.ttl,
                timestamp: entry.timestamp,
            })
            .collect()
    }

    pub fn iter_mut(&mut self) -> DnsIter<'_> {
        self.ensure_index();
        DnsIter {
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

        let mut domain_entries: Vec<(u32, DomainRange)> = self
            .domain_index
            .iter()
            .map(|(id, range)| (*id, *range))
            .collect();
        domain_entries.sort_by_key(|(id, _)| *id);

        let mut directory = Vec::with_capacity(domain_entries.len());
        let mut payload = Vec::new();

        for (domain_id, range) in domain_entries {
            let start_offset = payload.len() as u64;
            let records_slice = &self.records[range.start..range.start + range.len];

            let mut block = Vec::new();
            write_varu32(&mut block, records_slice.len() as u32);
            for entry in records_slice {
                block.push(encode_type(entry.record_type));
                write_varu32(&mut block, entry.value_id);
                write_varu32(&mut block, entry.ttl);
                write_varu32(&mut block, entry.timestamp);
            }

            let block_len = block.len() as u64;
            payload.extend_from_slice(&block);
            directory.push(DnsDirEntry {
                domain_id,
                record_count: records_slice.len() as u32,
                payload_offset: start_offset,
                payload_len: block_len,
            });
        }

        let string_section = encode_string_table(&self.strings);

        let directory_len = (directory.len() * DnsDirEntry::SIZE) as u64;
        let payload_len = payload.len() as u64;
        let strings_len = string_section.len() as u64;

        let header = DnsSegmentHeader {
            domain_count: directory.len() as u32,
            record_count: self.records.len() as u32,
            directory_len,
            payload_len,
            strings_len,
        };

        out.clear();
        out.reserve(
            DnsSegmentHeader::SIZE
                + directory.len() * DnsDirEntry::SIZE
                + payload.len()
                + string_section.len(),
        );
        header.write(out);
        DnsDirEntry::write_all(&directory, out);
        out.extend_from_slice(&payload);
        out.extend_from_slice(&string_section);
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < DnsSegmentHeader::SIZE {
            return Err(DecodeError("dns segment too small"));
        }
        let header = DnsSegmentHeader::read(bytes)?;

        let mut offset = DnsSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("dns directory overflow"))?;
        if dir_end > bytes.len() {
            return Err(DecodeError("dns directory out of bounds"));
        }
        let directory_bytes = &bytes[offset..dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("dns payload overflow"))?;
        if payload_end > bytes.len() {
            return Err(DecodeError("dns payload out of bounds"));
        }
        let payload_bytes = &bytes[offset..payload_end];
        offset = payload_end;

        let strings_end = offset
            .checked_add(header.strings_len as usize)
            .ok_or(DecodeError("dns string table overflow"))?;
        if strings_end > bytes.len() {
            return Err(DecodeError("dns string table out of bounds"));
        }
        let strings_bytes = &bytes[offset..strings_end];

        let directory = DnsDirEntry::read_all(directory_bytes, header.domain_count as usize)?;
        let strings = decode_string_table(strings_bytes)?;

        let mut records = Vec::with_capacity(header.record_count as usize);
        let mut domain_index = HashMap::with_capacity(directory.len());

        for entry in &directory {
            let start_index = records.len();
            let parsed_records = decode_records_block_entries(
                payload_bytes,
                entry.payload_offset,
                entry.payload_len,
                entry.record_count,
                entry.domain_id,
                strings.len(),
            )?;
            records.extend(parsed_records);
            domain_index.insert(
                entry.domain_id,
                DomainRange {
                    start: start_index,
                    len: entry.record_count as usize,
                },
            );
        }

        Ok(Self {
            strings,
            records,
            domain_index,
            sorted: true,
        })
    }
}

impl DnsSegmentView {
    pub fn from_arc(
        data: Arc<Vec<u8>>,
        segment_offset: usize,
        segment_len: usize,
    ) -> Result<Self, DecodeError> {
        if segment_offset + segment_len > data.len() {
            return Err(DecodeError("dns segment out of bounds"));
        }
        let bytes = &data[segment_offset..segment_offset + segment_len];
        if bytes.len() < DnsSegmentHeader::SIZE {
            return Err(DecodeError("dns segment too small"));
        }
        let header = DnsSegmentHeader::read(bytes)?;

        let mut offset = DnsSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("dns directory overflow"))?;
        if segment_offset + dir_end > data.len() {
            return Err(DecodeError("dns directory out of bounds"));
        }
        let directory_bytes = &data[segment_offset + offset..segment_offset + dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("dns payload overflow"))?;
        if segment_offset + payload_end > data.len() {
            return Err(DecodeError("dns payload out of bounds"));
        }
        let payload_offset = segment_offset + offset;
        offset = payload_end;

        let strings_offset = segment_offset + offset;
        let strings_end_abs = strings_offset
            .checked_add(header.strings_len as usize)
            .ok_or(DecodeError("dns string table overflow"))?;
        if strings_end_abs > data.len() {
            return Err(DecodeError("dns string table out of bounds"));
        }
        let strings_bytes = &data[strings_offset..strings_end_abs];

        let directory = DnsDirEntry::read_all(directory_bytes, header.domain_count as usize)?;
        let strings = decode_string_table(strings_bytes)?;

        Ok(Self {
            strings,
            directory,
            data,
            payload_offset,
            payload_len: header.payload_len as usize,
        })
    }

    pub fn records_for_domain(&self, domain: &str) -> Result<Vec<DnsRecordData>, DecodeError> {
        let Some(domain_id) = self.strings.get_id(domain) else {
            return Ok(Vec::new());
        };
        let Some(entry) = self.directory.iter().find(|dir| dir.domain_id == domain_id) else {
            return Ok(Vec::new());
        };

        let payload = &self.data[self.payload_offset..self.payload_offset + self.payload_len];
        decode_records_block_data(
            payload,
            entry.payload_offset,
            entry.payload_len,
            entry.record_count,
            domain,
            &self.strings,
        )
    }

    pub fn records_with_domain_prefix(
        &self,
        prefix: &str,
    ) -> Result<Vec<DnsRecordData>, DecodeError> {
        let payload = &self.data[self.payload_offset..self.payload_offset + self.payload_len];
        let mut matches = Vec::new();
        for entry in &self.directory {
            let domain = self.strings.get(entry.domain_id);
            if !domain.starts_with(prefix) {
                continue;
            }
            let mut decoded = decode_records_block_data(
                payload,
                entry.payload_offset,
                entry.payload_len,
                entry.record_count,
                domain,
                &self.strings,
            )?;
            matches.append(&mut decoded);
        }
        Ok(matches)
    }
}

pub struct DnsIter<'a> {
    segment: &'a mut DnsSegment,
    pos: usize,
}

impl<'a> Iterator for DnsIter<'a> {
    type Item = DnsRecordData;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.segment.records.len() {
            return None;
        }
        let entry = &self.segment.records[self.pos];
        self.pos += 1;
        Some(DnsRecordData {
            domain: self.segment.strings.get(entry.domain_id).to_string(),
            record_type: entry.record_type,
            value: self.segment.strings.get(entry.value_id).to_string(),
            ttl: entry.ttl,
            timestamp: entry.timestamp,
        })
    }
}

fn encode_type(record_type: DnsRecordType) -> u8 {
    match record_type {
        DnsRecordType::A => 1,
        DnsRecordType::AAAA => 2,
        DnsRecordType::MX => 3,
        DnsRecordType::NS => 4,
        DnsRecordType::TXT => 5,
        DnsRecordType::CNAME => 6,
    }
}

fn decode_type(byte: u8) -> Result<DnsRecordType, DecodeError> {
    match byte {
        1 => Ok(DnsRecordType::A),
        2 => Ok(DnsRecordType::AAAA),
        3 => Ok(DnsRecordType::MX),
        4 => Ok(DnsRecordType::NS),
        5 => Ok(DnsRecordType::TXT),
        6 => Ok(DnsRecordType::CNAME),
        _ => Err(DecodeError("invalid dns record type")),
    }
}

fn decode_records_block_entries(
    payload: &[u8],
    offset: u64,
    length: u64,
    expected_count: u32,
    domain_id: u32,
    string_count: usize,
) -> Result<Vec<Entry>, DecodeError> {
    let mut cursor = offset as usize;
    let end = cursor + length as usize;
    if end > payload.len() {
        return Err(DecodeError("dns payload slice out of bounds"));
    }

    let count = read_varu32(payload, &mut cursor)? as usize;
    if count as u32 != expected_count {
        return Err(DecodeError("dns record count mismatch"));
    }

    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let record_type_byte = payload
            .get(cursor)
            .copied()
            .ok_or(DecodeError("unexpected eof (dns type)"))?;
        cursor += 1;
        let record_type = decode_type(record_type_byte)?;
        let value_id = read_varu32(payload, &mut cursor)?;
        if (value_id as usize) >= string_count {
            return Err(DecodeError("dns value id out of range"));
        }
        let ttl = read_varu32(payload, &mut cursor)?;
        let timestamp = read_varu32(payload, &mut cursor)?;
        records.push(Entry {
            domain_id,
            value_id,
            record_type,
            ttl,
            timestamp,
        });
    }

    if cursor != end {
        return Err(DecodeError("dns payload length mismatch"));
    }

    Ok(records)
}

fn decode_records_block_data(
    payload: &[u8],
    offset: u64,
    length: u64,
    expected_count: u32,
    domain: &str,
    strings: &StringTable,
) -> Result<Vec<DnsRecordData>, DecodeError> {
    let mut cursor = offset as usize;
    let end = cursor + length as usize;
    if end > payload.len() {
        return Err(DecodeError("dns payload slice out of bounds"));
    }

    let count = read_varu32(payload, &mut cursor)? as usize;
    if count as u32 != expected_count {
        return Err(DecodeError("dns record count mismatch"));
    }

    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let record_type_byte = payload
            .get(cursor)
            .copied()
            .ok_or(DecodeError("unexpected eof (dns type)"))?;
        cursor += 1;
        let record_type = decode_type(record_type_byte)?;
        let value_id = read_varu32(payload, &mut cursor)?;
        if (value_id as usize) >= strings.len() {
            return Err(DecodeError("dns value id out of range"));
        }
        let ttl = read_varu32(payload, &mut cursor)?;
        let timestamp = read_varu32(payload, &mut cursor)?;
        records.push(DnsRecordData {
            domain: domain.to_string(),
            record_type,
            value: strings.get(value_id).to_string(),
            ttl,
            timestamp,
        });
    }

    if cursor != end {
        return Err(DecodeError("dns payload length mismatch"));
    }

    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_segment_roundtrip_and_index() {
        let mut segment = DnsSegment::new();
        segment.insert(DnsRecordData {
            domain: "example.com".into(),
            record_type: DnsRecordType::A,
            value: "93.184.216.34".into(),
            ttl: 300,
            timestamp: 1_700_000_000,
        });
        segment.insert(DnsRecordData {
            domain: "example.com".into(),
            record_type: DnsRecordType::MX,
            value: "mx1.example.com".into(),
            ttl: 600,
            timestamp: 1_700_000_100,
        });
        segment.insert(DnsRecordData {
            domain: "other.org".into(),
            record_type: DnsRecordType::AAAA,
            value: "2001:db8::1".into(),
            ttl: 1_200,
            timestamp: 1_700_000_200,
        });

        let mut before = segment.clone();
        let example_records = before.records_for_domain("example.com");
        assert_eq!(example_records.len(), 2);
        assert!(example_records
            .iter()
            .any(|r| matches!(r.record_type, DnsRecordType::MX)));

        let bytes = segment.serialize();
        let mut decoded = DnsSegment::deserialize(&bytes).expect("decode dns segment");
        let all = decoded.iter_mut().collect::<Vec<_>>();
        assert_eq!(all.len(), 3);

        let other = decoded.records_for_domain("other.org");
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].value, "2001:db8::1");
    }

    #[test]
    fn dns_segment_view_direct_access() {
        let mut segment = DnsSegment::new();
        segment.insert(DnsRecordData {
            domain: "example.com".into(),
            record_type: DnsRecordType::A,
            value: "93.184.216.34".into(),
            ttl: 300,
            timestamp: 1_700_000_000,
        });
        segment.insert(DnsRecordData {
            domain: "example.com".into(),
            record_type: DnsRecordType::TXT,
            value: "v=spf1 -all".into(),
            ttl: 1800,
            timestamp: 1_700_000_500,
        });
        segment.insert(DnsRecordData {
            domain: "api.example.com".into(),
            record_type: DnsRecordType::CNAME,
            value: "lb.example.net".into(),
            ttl: 400,
            timestamp: 1_700_001_000,
        });

        let bytes = segment.serialize();
        let data = Arc::new(bytes);
        let view = DnsSegmentView::from_arc(Arc::clone(&data), 0, data.len()).expect("view loaded");
        let records = view.records_for_domain("example.com").expect("records");
        assert_eq!(records.len(), 2);
        assert!(records.iter().any(|r| r.value == "v=spf1 -all"));

        let api_records = view.records_for_domain("api.example.com").expect("records");
        assert_eq!(api_records.len(), 1);
        assert_eq!(api_records[0].record_type, DnsRecordType::CNAME);
    }
}
