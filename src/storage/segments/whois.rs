use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::primitives::encoding::{
    read_string, read_varu32, write_string, write_varu32, DecodeError,
};
use crate::storage::records::WhoisRecord;
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
    registrar_id: u32,
    created: u32,
    expires: u32,
    timestamp: u32,
    nameserver_ids: Vec<u32>,
}

#[derive(Debug, Clone, Copy)]
struct WhoisDirEntry {
    domain_id: u32,
    payload_offset: u64,
    payload_len: u64,
}

impl WhoisDirEntry {
    const SIZE: usize = 4 + 8 + 8;

    fn write_all(entries: &[Self], buf: &mut Vec<u8>) {
        for entry in entries {
            buf.extend_from_slice(&entry.domain_id.to_le_bytes());
            buf.extend_from_slice(&entry.payload_offset.to_le_bytes());
            buf.extend_from_slice(&entry.payload_len.to_le_bytes());
        }
    }

    fn read_all(bytes: &[u8], count: usize) -> Result<Vec<Self>, DecodeError> {
        if bytes.len() != count * Self::SIZE {
            return Err(DecodeError("invalid whois directory size"));
        }
        let mut entries = Vec::with_capacity(count);
        let mut offset = 0usize;
        for _ in 0..count {
            let domain_id = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            offset += 4;
            let payload_offset = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let payload_len = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            entries.push(Self {
                domain_id,
                payload_offset,
                payload_len,
            });
        }
        Ok(entries)
    }
}

#[derive(Debug, Clone, Copy)]
struct WhoisSegmentHeader {
    record_count: u32,
    directory_len: u64,
    payload_len: u64,
    strings_len: u64,
}

impl WhoisSegmentHeader {
    const MAGIC: [u8; 4] = *b"WH01";
    const VERSION: u16 = 1;
    const SIZE: usize = 4 + 2 + 2 + 4 + 8 + 8 + 8;

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&Self::MAGIC);
        buf.extend_from_slice(&Self::VERSION.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&self.record_count.to_le_bytes());
        buf.extend_from_slice(&self.directory_len.to_le_bytes());
        buf.extend_from_slice(&self.payload_len.to_le_bytes());
        buf.extend_from_slice(&self.strings_len.to_le_bytes());
    }

    fn read(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < Self::SIZE {
            return Err(DecodeError("whois header too small"));
        }
        if bytes[0..4] != Self::MAGIC {
            return Err(DecodeError("invalid whois segment magic"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != Self::VERSION {
            return Err(DecodeError("unsupported whois segment version"));
        }
        let record_count = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let directory_len = u64::from_le_bytes(bytes[12..20].try_into().unwrap());
        let payload_len = u64::from_le_bytes(bytes[20..28].try_into().unwrap());
        let strings_len = u64::from_le_bytes(bytes[28..36].try_into().unwrap());

        Ok(Self {
            record_count,
            directory_len,
            payload_len,
            strings_len,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct WhoisSegment {
    strings: StringTable,
    entries: Vec<Entry>,
    index: HashMap<u32, usize>,
}

pub struct WhoisSegmentView {
    strings: StringTable,
    directory: Vec<WhoisDirEntry>,
    data: Arc<Vec<u8>>,
    payload_offset: usize,
    payload_len: usize,
}

impl WhoisSegment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn insert(
        &mut self,
        domain: &str,
        registrar: &str,
        created: u32,
        expires: u32,
        nameservers: Vec<String>,
        timestamp: u32,
    ) {
        let domain_id = self.strings.intern(domain);
        let registrar_id = self.strings.intern(registrar);
        let ns_ids = nameservers
            .into_iter()
            .map(|ns| self.strings.intern(ns))
            .collect();

        let entry = Entry {
            domain_id,
            registrar_id,
            created,
            expires,
            timestamp,
            nameserver_ids: ns_ids,
        };

        match self.index.get(&domain_id).cloned() {
            Some(pos) => {
                self.entries[pos] = entry;
            }
            None => {
                let pos = self.entries.len();
                self.entries.push(entry);
                self.index.insert(domain_id, pos);
            }
        }
    }

    pub fn get(&self, domain: &str) -> Option<WhoisRecord> {
        let domain_id = self.strings.get_id(domain)?;
        let idx = *self.index.get(&domain_id)?;
        Some(self.to_record(&self.entries[idx]))
    }

    pub fn iter(&self) -> impl Iterator<Item = WhoisRecord> + '_ {
        self.entries.iter().map(|entry| self.to_record(entry))
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.serialize_into(&mut buf);
        buf
    }

    pub fn serialize_into(&self, out: &mut Vec<u8>) {
        let mut entries: Vec<&Entry> = self.entries.iter().collect();
        entries.sort_by_key(|entry| entry.domain_id);

        let mut directory = Vec::with_capacity(entries.len());
        let mut payload = Vec::new();

        for entry in entries {
            let start_offset = payload.len() as u64;
            write_varu32(&mut payload, entry.registrar_id);
            write_varu32(&mut payload, entry.created);
            write_varu32(&mut payload, entry.expires);
            write_varu32(&mut payload, entry.timestamp);
            write_varu32(&mut payload, entry.nameserver_ids.len() as u32);
            for ns in &entry.nameserver_ids {
                write_varu32(&mut payload, *ns);
            }
            let block_len = payload.len() as u64 - start_offset;
            directory.push(WhoisDirEntry {
                domain_id: entry.domain_id,
                payload_offset: start_offset,
                payload_len: block_len,
            });
        }

        let string_section = encode_string_table(&self.strings);
        let directory_len = (directory.len() * WhoisDirEntry::SIZE) as u64;
        let payload_len = payload.len() as u64;
        let strings_len = string_section.len() as u64;

        let header = WhoisSegmentHeader {
            record_count: self.entries.len() as u32,
            directory_len,
            payload_len,
            strings_len,
        };

        out.clear();
        out.reserve(
            WhoisSegmentHeader::SIZE
                + directory.len() * WhoisDirEntry::SIZE
                + payload.len()
                + string_section.len(),
        );
        header.write(out);
        WhoisDirEntry::write_all(&directory, out);
        out.extend_from_slice(&payload);
        out.extend_from_slice(&string_section);
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < WhoisSegmentHeader::SIZE {
            return Err(DecodeError("whois segment too small"));
        }
        let header = WhoisSegmentHeader::read(bytes)?;

        let mut offset = WhoisSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("whois directory overflow"))?;
        if dir_end > bytes.len() {
            return Err(DecodeError("whois directory out of bounds"));
        }
        let directory_bytes = &bytes[offset..dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("whois payload overflow"))?;
        if payload_end > bytes.len() {
            return Err(DecodeError("whois payload out of bounds"));
        }
        let payload_bytes = &bytes[offset..payload_end];
        offset = payload_end;

        let strings_end = offset
            .checked_add(header.strings_len as usize)
            .ok_or(DecodeError("whois string table overflow"))?;
        if strings_end > bytes.len() {
            return Err(DecodeError("whois string table out of bounds"));
        }
        let strings_bytes = &bytes[offset..strings_end];

        let strings = decode_string_table(strings_bytes)?;
        let directory = WhoisDirEntry::read_all(directory_bytes, header.record_count as usize)?;

        let mut entries = Vec::with_capacity(header.record_count as usize);
        let mut index = HashMap::with_capacity(directory.len());

        for entry in &directory {
            let mut cursor = entry.payload_offset as usize;
            let end = cursor + entry.payload_len as usize;
            if end > payload_bytes.len() {
                return Err(DecodeError("whois payload slice out of bounds"));
            }

            let registrar_id = read_varu32(payload_bytes, &mut cursor)?;
            let created = read_varu32(payload_bytes, &mut cursor)?;
            let expires = read_varu32(payload_bytes, &mut cursor)?;
            let timestamp = read_varu32(payload_bytes, &mut cursor)?;
            let ns_count = read_varu32(payload_bytes, &mut cursor)? as usize;
            let mut nameserver_ids = Vec::with_capacity(ns_count);
            for _ in 0..ns_count {
                nameserver_ids.push(read_varu32(payload_bytes, &mut cursor)?);
            }

            if cursor != end {
                return Err(DecodeError("whois payload length mismatch"));
            }

            let pos = entries.len();
            entries.push(Entry {
                domain_id: entry.domain_id,
                registrar_id,
                created,
                expires,
                timestamp,
                nameserver_ids,
            });
            index.insert(entry.domain_id, pos);
        }

        Ok(Self {
            strings,
            entries,
            index,
        })
    }

    fn to_record(&self, entry: &Entry) -> WhoisRecord {
        WhoisRecord {
            domain: self.strings.get(entry.domain_id).to_string(),
            registrar: self.strings.get(entry.registrar_id).to_string(),
            created_date: entry.created,
            expires_date: entry.expires,
            nameservers: entry
                .nameserver_ids
                .iter()
                .map(|id| self.strings.get(*id).to_string())
                .collect(),
            timestamp: entry.timestamp,
        }
    }
}

impl WhoisSegmentView {
    pub fn from_arc(
        data: Arc<Vec<u8>>,
        segment_offset: usize,
        segment_len: usize,
    ) -> Result<Self, DecodeError> {
        if segment_offset + segment_len > data.len() {
            return Err(DecodeError("whois segment out of bounds"));
        }
        let bytes = &data[segment_offset..segment_offset + segment_len];
        if bytes.len() < WhoisSegmentHeader::SIZE {
            return Err(DecodeError("whois segment too small"));
        }
        let header = WhoisSegmentHeader::read(bytes)?;

        let mut offset = WhoisSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("whois directory overflow"))?;
        if segment_offset + dir_end > data.len() {
            return Err(DecodeError("whois directory out of bounds"));
        }
        let directory_bytes = &data[segment_offset + offset..segment_offset + dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("whois payload overflow"))?;
        if segment_offset + payload_end > data.len() {
            return Err(DecodeError("whois payload out of bounds"));
        }
        let payload_offset = segment_offset + offset;
        offset = payload_end;

        let strings_offset = segment_offset + offset;
        let strings_end_abs = strings_offset
            .checked_add(header.strings_len as usize)
            .ok_or(DecodeError("whois string table overflow"))?;
        if strings_end_abs > data.len() {
            return Err(DecodeError("whois string table out of bounds"));
        }
        let strings_bytes = &data[strings_offset..strings_end_abs];

        let strings = decode_string_table(strings_bytes)?;
        let mut directory = WhoisDirEntry::read_all(directory_bytes, header.record_count as usize)?;
        directory.sort_by_key(|entry| entry.domain_id);

        Ok(Self {
            strings,
            directory,
            data,
            payload_offset,
            payload_len: header.payload_len as usize,
        })
    }

    pub fn get(&self, domain: &str) -> Result<Option<WhoisRecord>, DecodeError> {
        let Some(domain_id) = self.strings.get_id(domain) else {
            return Ok(None);
        };
        let Some(dir) = self
            .directory
            .iter()
            .find(|entry| entry.domain_id == domain_id)
        else {
            return Ok(None);
        };

        let payload = &self.data[self.payload_offset..self.payload_offset + self.payload_len];
        let mut cursor = dir.payload_offset as usize;
        let end = cursor + dir.payload_len as usize;
        if end > payload.len() {
            return Err(DecodeError("whois payload slice out of bounds"));
        }

        let registrar_id = read_varu32(payload, &mut cursor)?;
        let created = read_varu32(payload, &mut cursor)?;
        let expires = read_varu32(payload, &mut cursor)?;
        let timestamp = read_varu32(payload, &mut cursor)?;
        let ns_count = read_varu32(payload, &mut cursor)? as usize;
        let mut nameservers = Vec::with_capacity(ns_count);
        for _ in 0..ns_count {
            nameservers.push(read_varu32(payload, &mut cursor)?);
        }

        if cursor != end {
            return Err(DecodeError("whois payload length mismatch"));
        }

        let record = WhoisRecord {
            domain: domain.to_string(),
            registrar: self.strings.get(registrar_id).to_string(),
            created_date: created,
            expires_date: expires,
            nameservers: nameservers
                .iter()
                .map(|id| self.strings.get(*id).to_string())
                .collect(),
            timestamp,
        };

        Ok(Some(record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_whois() {
        let mut segment = WhoisSegment::new();
        segment.insert(
            "example.com",
            "Example Registrar",
            1_600_000_000,
            1_900_000_000,
            vec!["ns1.example.com".into(), "ns2.example.com".into()],
            1_700_000_000,
        );

        let encoded = segment.serialize();
        let decoded = WhoisSegment::deserialize(&encoded).expect("decode");
        let rec = decoded.get("example.com").expect("entry");
        assert_eq!(rec.registrar, "Example Registrar");
        assert_eq!(rec.nameservers.len(), 2);
    }

    #[test]
    fn view_reads_whois_record() {
        let mut segment = WhoisSegment::new();
        segment.insert(
            "example.com",
            "Example Registrar",
            1_600_000_000,
            1_900_000_000,
            vec!["ns1.example.com".into()],
            1_700_000_000,
        );

        let encoded = segment.serialize();
        let data = Arc::new(encoded);
        let view =
            WhoisSegmentView::from_arc(Arc::clone(&data), 0, data.len()).expect("view loaded");
        let record = view.get("example.com").expect("result");
        assert!(record.is_some());
        let record = record.unwrap();
        assert_eq!(record.registrar, "Example Registrar");
    }
}
