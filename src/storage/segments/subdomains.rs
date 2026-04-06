use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use crate::storage::primitives::encoding::{
    read_ip, read_string, read_varu32, write_ip, write_string, write_varu32, DecodeError,
};
use crate::storage::records::{SubdomainRecord, SubdomainSource};
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
    label_id: u32,
    ips: Vec<IpAddr>,
    source: SubdomainSource,
    timestamp: u32,
}

#[derive(Debug, Clone, Copy)]
struct DomainRange {
    start: usize,
    len: usize,
}

#[derive(Debug, Clone, Copy)]
struct SubdomainDirEntry {
    domain_id: u32,
    record_count: u32,
    payload_offset: u64,
    payload_len: u64,
}

impl SubdomainDirEntry {
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
            return Err(DecodeError("invalid subdomain directory size"));
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
struct SubdomainSegmentHeader {
    domain_count: u32,
    record_count: u32,
    directory_len: u64,
    payload_len: u64,
    strings_len: u64,
}

impl SubdomainSegmentHeader {
    const MAGIC: [u8; 4] = *b"SD01";
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
            return Err(DecodeError("subdomain header too small"));
        }
        if bytes[0..4] != Self::MAGIC {
            return Err(DecodeError("invalid subdomain segment magic"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != Self::VERSION {
            return Err(DecodeError("unsupported subdomain segment version"));
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
pub struct SubdomainSegment {
    strings: StringTable,
    records: Vec<Entry>,
    domain_index: HashMap<u32, DomainRange>,
    sorted: bool,
}

pub struct SubdomainSegmentView {
    strings: StringTable,
    directory: Vec<SubdomainDirEntry>,
    data: Arc<Vec<u8>>,
    payload_offset: usize,
    payload_len: usize,
}

impl SubdomainSegment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn insert(
        &mut self,
        domain: &str,
        subdomain: &str,
        ips: Vec<IpAddr>,
        source: SubdomainSource,
        timestamp: u32,
    ) {
        let domain_id = self.strings.intern(domain);
        let label_id = self.strings.intern(subdomain);
        self.records.push(Entry {
            domain_id,
            label_id,
            ips,
            source,
            timestamp,
        });
        self.sorted = false;
    }

    fn ensure_index(&mut self) {
        if self.sorted {
            return;
        }
        self.records
            .sort_by(|a, b| match a.domain_id.cmp(&b.domain_id) {
                std::cmp::Ordering::Equal => a.label_id.cmp(&b.label_id),
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

    pub fn get_by_domain(&mut self, domain: &str) -> Vec<SubdomainRecord> {
        self.ensure_index();
        let Some(domain_id) = self.strings.get_id(domain) else {
            return Vec::new();
        };
        let Some(range) = self.domain_index.get(&domain_id) else {
            return Vec::new();
        };
        self.records[range.start..range.start + range.len]
            .iter()
            .map(|entry| SubdomainRecord {
                subdomain: self.strings.get(entry.label_id).to_string(),
                ips: entry.ips.clone(),
                source: entry.source,
                timestamp: entry.timestamp,
            })
            .collect()
    }

    pub fn all_records(&mut self) -> Vec<SubdomainRecord> {
        self.ensure_index();
        self.records
            .iter()
            .map(|entry| SubdomainRecord {
                subdomain: self.strings.get(entry.label_id).to_string(),
                ips: entry.ips.clone(),
                source: entry.source,
                timestamp: entry.timestamp,
            })
            .collect()
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
            .map(|(id, range)| (*id, range.clone()))
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
                write_varu32(&mut block, entry.label_id);
                write_varu32(&mut block, entry.timestamp);
                block.push(entry.source as u8);
                write_varu32(&mut block, entry.ips.len() as u32);
                for ip in &entry.ips {
                    write_ip(&mut block, ip);
                }
            }

            let block_len = block.len() as u64;
            payload.extend_from_slice(&block);
            directory.push(SubdomainDirEntry {
                domain_id,
                record_count: records_slice.len() as u32,
                payload_offset: start_offset,
                payload_len: block_len,
            });
        }

        let string_section = encode_string_table(&self.strings);

        let directory_len = (directory.len() * SubdomainDirEntry::SIZE) as u64;
        let payload_len = payload.len() as u64;
        let strings_len = string_section.len() as u64;

        let header = SubdomainSegmentHeader {
            domain_count: directory.len() as u32,
            record_count: self.records.len() as u32,
            directory_len,
            payload_len,
            strings_len,
        };

        out.clear();
        out.reserve(
            SubdomainSegmentHeader::SIZE
                + directory.len() * SubdomainDirEntry::SIZE
                + payload.len()
                + string_section.len(),
        );
        header.write(out);
        SubdomainDirEntry::write_all(&directory, out);
        out.extend_from_slice(&payload);
        out.extend_from_slice(&string_section);
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < SubdomainSegmentHeader::SIZE {
            return Err(DecodeError("subdomain segment too small"));
        }
        let header = SubdomainSegmentHeader::read(bytes)?;

        let mut offset = SubdomainSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("subdomain directory overflow"))?;
        if dir_end > bytes.len() {
            return Err(DecodeError("subdomain directory out of bounds"));
        }
        let directory_bytes = &bytes[offset..dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("subdomain payload overflow"))?;
        if payload_end > bytes.len() {
            return Err(DecodeError("subdomain payload out of bounds"));
        }
        let payload_bytes = &bytes[offset..payload_end];
        offset = payload_end;

        let strings_end = offset
            .checked_add(header.strings_len as usize)
            .ok_or(DecodeError("subdomain strings overflow"))?;
        if strings_end > bytes.len() {
            return Err(DecodeError("subdomain string table out of bounds"));
        }
        let strings_bytes = &bytes[offset..strings_end];

        let directory = SubdomainDirEntry::read_all(directory_bytes, header.domain_count as usize)?;
        let strings = decode_string_table(strings_bytes)?;

        let mut records = Vec::with_capacity(header.record_count as usize);
        let mut domain_index = HashMap::with_capacity(directory.len());
        for entry in &directory {
            let start_index = records.len();
            let mut cursor = entry.payload_offset as usize;
            let end = cursor + entry.payload_len as usize;
            if end > payload_bytes.len() {
                return Err(DecodeError("subdomain payload out of bounds"));
            }
            let record_count = read_varu32(payload_bytes, &mut cursor)? as usize;
            for _ in 0..record_count {
                let label_id = read_varu32(payload_bytes, &mut cursor)?;
                let timestamp = read_varu32(payload_bytes, &mut cursor)?;
                if cursor >= payload_bytes.len() {
                    return Err(DecodeError("unexpected eof (subdomain source)"));
                }
                let source = decode_source(payload_bytes[cursor])?;
                cursor += 1;
                let ip_count = read_varu32(payload_bytes, &mut cursor)? as usize;
                let mut ips = Vec::with_capacity(ip_count);
                for _ in 0..ip_count {
                    let ip = read_ip(payload_bytes, &mut cursor)?;
                    ips.push(ip);
                }
                records.push(Entry {
                    domain_id: entry.domain_id,
                    label_id,
                    ips,
                    source,
                    timestamp,
                });
            }
            if record_count as u32 != entry.record_count {
                return Err(DecodeError("subdomain record count mismatch"));
            }
            if cursor != end {
                return Err(DecodeError("subdomain payload length mismatch"));
            }
            domain_index.insert(
                entry.domain_id,
                DomainRange {
                    start: start_index,
                    len: record_count,
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

fn decode_source(byte: u8) -> Result<SubdomainSource, DecodeError> {
    match byte {
        0 => Ok(SubdomainSource::DnsBruteforce),
        1 => Ok(SubdomainSource::CertTransparency),
        2 => Ok(SubdomainSource::SearchEngine),
        3 => Ok(SubdomainSource::WebCrawl),
        _ => Err(DecodeError("invalid subdomain source")),
    }
}

impl SubdomainSegmentView {
    pub fn from_arc(
        data: Arc<Vec<u8>>,
        segment_offset: usize,
        segment_len: usize,
    ) -> Result<Self, DecodeError> {
        if segment_offset + segment_len > data.len() {
            return Err(DecodeError("subdomain segment out of bounds"));
        }
        let bytes = &data[segment_offset..segment_offset + segment_len];
        if bytes.len() < SubdomainSegmentHeader::SIZE {
            return Err(DecodeError("subdomain segment too small"));
        }
        let header = SubdomainSegmentHeader::read(bytes)?;

        let mut offset = SubdomainSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("subdomain directory overflow"))?;
        if segment_offset + dir_end > data.len() {
            return Err(DecodeError("subdomain directory out of bounds"));
        }
        let directory_bytes = &data[segment_offset + offset..segment_offset + dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("subdomain payload overflow"))?;
        if segment_offset + payload_end > data.len() {
            return Err(DecodeError("subdomain payload out of bounds"));
        }
        let payload_offset = segment_offset + offset;
        offset = payload_end;

        let strings_offset = segment_offset + offset;
        let strings_end_abs = strings_offset
            .checked_add(header.strings_len as usize)
            .ok_or(DecodeError("subdomain strings overflow"))?;
        if strings_end_abs > data.len() {
            return Err(DecodeError("subdomain string table out of bounds"));
        }
        let strings_bytes = &data[strings_offset..strings_end_abs];

        let directory = SubdomainDirEntry::read_all(directory_bytes, header.domain_count as usize)?;
        let strings = decode_string_table(strings_bytes)?;

        Ok(Self {
            strings,
            directory,
            data,
            payload_offset,
            payload_len: header.payload_len as usize,
        })
    }

    pub fn records_for_domain(&self, domain: &str) -> Result<Vec<SubdomainRecord>, DecodeError> {
        let Some(domain_id) = self.strings.get_id(domain) else {
            return Ok(Vec::new());
        };

        let Some(entry) = self.directory.iter().find(|dir| dir.domain_id == domain_id) else {
            return Ok(Vec::new());
        };
        let payload = &self.data[self.payload_offset..self.payload_offset + self.payload_len];
        decode_records_block(
            payload,
            entry.payload_offset,
            entry.payload_len,
            entry.record_count,
            &self.strings,
        )
    }

    pub fn records_with_prefix(&self, prefix: &str) -> Result<Vec<SubdomainRecord>, DecodeError> {
        let payload = &self.data[self.payload_offset..self.payload_offset + self.payload_len];
        let mut matches = Vec::new();

        for entry in &self.directory {
            let mut records = decode_records_block(
                payload,
                entry.payload_offset,
                entry.payload_len,
                entry.record_count,
                &self.strings,
            )?;
            records.retain(|record| record.subdomain.starts_with(prefix));
            matches.extend(records);
        }

        Ok(matches)
    }
}

fn decode_records_block(
    payload: &[u8],
    offset: u64,
    length: u64,
    expected_count: u32,
    strings: &StringTable,
) -> Result<Vec<SubdomainRecord>, DecodeError> {
    let mut cursor = offset as usize;
    let end = cursor + length as usize;
    if end > payload.len() {
        return Err(DecodeError("subdomain payload out of bounds"));
    }

    let count = read_varu32(payload, &mut cursor)? as usize;
    if count as u32 != expected_count {
        return Err(DecodeError("subdomain record count mismatch"));
    }

    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let label_id = read_varu32(payload, &mut cursor)?;
        if (label_id as usize) >= strings.len() {
            return Err(DecodeError("invalid subdomain label id"));
        }
        let timestamp = read_varu32(payload, &mut cursor)?;
        if cursor >= payload.len() {
            return Err(DecodeError("unexpected eof (subdomain source)"));
        }
        let source = decode_source(payload[cursor])?;
        cursor += 1;
        let ip_count = read_varu32(payload, &mut cursor)? as usize;
        let mut ips = Vec::with_capacity(ip_count);
        for _ in 0..ip_count {
            let ip = read_ip(payload, &mut cursor)?;
            ips.push(ip);
        }
        records.push(SubdomainRecord {
            subdomain: strings.get(label_id).to_string(),
            ips,
            source,
            timestamp,
        });
    }

    if cursor != end {
        return Err(DecodeError("subdomain payload length mismatch"));
    }

    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn subdomain_segment_roundtrip() {
        let mut segment = SubdomainSegment::new();
        segment.insert(
            "example.com",
            "api.example.com",
            vec![IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1))],
            SubdomainSource::DnsBruteforce,
            123,
        );
        segment.insert(
            "example.com",
            "mail.example.com",
            vec![IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 2))],
            SubdomainSource::SearchEngine,
            124,
        );
        segment.insert(
            "example.net",
            "www.example.net",
            vec![IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, 5))],
            SubdomainSource::CertTransparency,
            200,
        );

        let encoded = segment.serialize();
        let mut decoded = SubdomainSegment::deserialize(&encoded).expect("decode");

        let records = decoded.get_by_domain("example.com");
        assert_eq!(records.len(), 2);
        assert!(records.iter().any(|r| r.subdomain == "api.example.com"));
        assert!(records.iter().any(|r| r.subdomain == "mail.example.com"));

        let net_records = decoded.get_by_domain("example.net");
        assert_eq!(net_records.len(), 1);
        assert_eq!(net_records[0].subdomain, "www.example.net");
    }

    #[test]
    fn subdomain_segment_view_reads_domain() {
        let mut segment = SubdomainSegment::new();
        segment.insert(
            "example.com",
            "api.example.com",
            vec![IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 10))],
            SubdomainSource::DnsBruteforce,
            321,
        );
        segment.insert(
            "example.com",
            "static.example.com",
            vec![IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 30))],
            SubdomainSource::WebCrawl,
            322,
        );
        segment.insert(
            "example.org",
            "www.example.org",
            vec![IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 5))],
            SubdomainSource::SearchEngine,
            400,
        );

        let encoded = segment.serialize();
        let data = Arc::new(encoded);
        let view =
            SubdomainSegmentView::from_arc(Arc::clone(&data), 0, data.len()).expect("view loaded");
        let records = view.records_for_domain("example.com").expect("records");
        assert_eq!(records.len(), 2);
        assert!(records.iter().any(|r| r.subdomain == "api.example.com"));
        assert!(records.iter().any(|r| r.subdomain == "static.example.com"));
        let org_records = view.records_for_domain("example.org").expect("records");
        assert_eq!(org_records.len(), 1);
        assert_eq!(org_records[0].subdomain, "www.example.org");
    }

    #[test]
    fn roundtrip_subdomains() {
        let mut segment = SubdomainSegment::new();
        segment.insert(
            "example.com",
            "api.example.com",
            vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))],
            SubdomainSource::DnsBruteforce,
            100,
        );
        segment.insert(
            "example.com",
            "cdn.example.com",
            vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3))],
            SubdomainSource::WebCrawl,
            120,
        );
        segment.insert(
            "corp.local",
            "vpn.corp.local",
            vec![IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))],
            SubdomainSource::SearchEngine,
            200,
        );

        let encoded = segment.serialize();
        let mut decoded = SubdomainSegment::deserialize(&encoded).expect("decode");
        let subs = decoded.get_by_domain("example.com");
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].subdomain, "api.example.com");
    }
}
