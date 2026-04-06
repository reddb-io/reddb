use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::primitives::encoding::{
    read_string, read_varu32, write_string, write_varu32, DecodeError,
};
use crate::storage::records::{HttpHeadersRecord, HttpTlsSnapshot};
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

fn write_optional_varu32(buf: &mut Vec<u8>, value: Option<u32>) {
    match value {
        Some(v) => {
            buf.push(1);
            write_varu32(buf, v);
        }
        None => buf.push(0),
    }
}

fn read_optional_varu32(bytes: &[u8], pos: &mut usize) -> Result<Option<u32>, DecodeError> {
    if *pos >= bytes.len() {
        return Err(DecodeError("unexpected eof (optional flag)"));
    }
    let flag = bytes[*pos];
    *pos += 1;
    if flag != 0 {
        Ok(Some(read_varu32(bytes, pos)?))
    } else {
        Ok(None)
    }
}

#[derive(Debug, Clone)]
struct HeaderEntry {
    name_id: u32,
    value_id: u32,
}

#[derive(Debug, Clone)]
struct TlsEntry {
    authority_id: Option<u32>,
    version_id: Option<u32>,
    cipher_id: Option<u32>,
    alpn_id: Option<u32>,
    subject_ids: Vec<u32>,
    fingerprint_ids: Vec<u32>,
    ja3_id: Option<u32>,
    ja3s_id: Option<u32>,
    ja3_raw_id: Option<u32>,
    ja3s_raw_id: Option<u32>,
    certificate_ids: Vec<u32>,
}

#[derive(Debug, Clone)]
struct Entry {
    host_id: u32,
    url_id: u32,
    method_id: u32,
    scheme_id: u32,
    version_id: u32,
    status_code: u16,
    status_text_id: u32,
    server_id: Option<u32>,
    body_size: u32,
    timestamp: u32,
    headers: Vec<HeaderEntry>,
    tls: Option<TlsEntry>,
}

#[derive(Debug, Clone, Copy)]
struct HostRange {
    start: usize,
    len: usize,
}

#[derive(Debug, Clone, Copy)]
struct HttpDirEntry {
    host_id: u32,
    record_count: u32,
    payload_offset: u64,
    payload_len: u64,
}

impl HttpDirEntry {
    const SIZE: usize = 4 + 4 + 8 + 8;

    fn write_all(entries: &[Self], buf: &mut Vec<u8>) {
        for entry in entries {
            buf.extend_from_slice(&entry.host_id.to_le_bytes());
            buf.extend_from_slice(&entry.record_count.to_le_bytes());
            buf.extend_from_slice(&entry.payload_offset.to_le_bytes());
            buf.extend_from_slice(&entry.payload_len.to_le_bytes());
        }
    }

    fn read_all(bytes: &[u8], count: usize) -> Result<Vec<Self>, DecodeError> {
        if bytes.len() != count * Self::SIZE {
            return Err(DecodeError("invalid http directory size"));
        }
        let mut entries = Vec::with_capacity(count);
        let mut offset = 0usize;
        for _ in 0..count {
            let host_id = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            offset += 4;
            let record_count = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            offset += 4;
            let payload_offset = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let payload_len = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            entries.push(Self {
                host_id,
                record_count,
                payload_offset,
                payload_len,
            });
        }
        Ok(entries)
    }
}

#[derive(Debug, Clone, Copy)]
struct HttpSegmentHeader {
    version: u16,
    host_count: u32,
    record_count: u32,
    directory_len: u64,
    payload_len: u64,
    strings_len: u64,
}

impl HttpSegmentHeader {
    const MAGIC: [u8; 4] = *b"HT01";
    const VERSION: u16 = 3;
    const SIZE: usize = 4 + 2 + 2 + 4 + 4 + 8 + 8 + 8;

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&Self::MAGIC);
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // reserved
        buf.extend_from_slice(&self.host_count.to_le_bytes());
        buf.extend_from_slice(&self.record_count.to_le_bytes());
        buf.extend_from_slice(&self.directory_len.to_le_bytes());
        buf.extend_from_slice(&self.payload_len.to_le_bytes());
        buf.extend_from_slice(&self.strings_len.to_le_bytes());
    }

    fn read(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < Self::SIZE {
            return Err(DecodeError("http header too small"));
        }
        if bytes[0..4] != Self::MAGIC {
            return Err(DecodeError("invalid http segment magic"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != 1 && version != 2 && version != Self::VERSION {
            return Err(DecodeError("unsupported http segment version"));
        }
        let host_count = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let record_count = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let directory_len = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let payload_len = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        let strings_len = u64::from_le_bytes(bytes[32..40].try_into().unwrap());

        Ok(Self {
            version,
            host_count,
            record_count,
            directory_len,
            payload_len,
            strings_len,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct HttpSegment {
    strings: StringTable,
    entries: Vec<Entry>,
    host_index: HashMap<u32, HostRange>,
    sorted: bool,
}

pub struct HttpSegmentView {
    strings: StringTable,
    directory: Vec<HttpDirEntry>,
    data: Arc<Vec<u8>>,
    payload_offset: usize,
    payload_len: usize,
    version: u16,
}

pub struct HttpIter<'a> {
    segment: &'a mut HttpSegment,
    pos: usize,
}

impl HttpSegment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn insert(&mut self, mut record: HttpHeadersRecord) {
        if record.host.is_empty() {
            record.host = extract_host(&record.url).to_string();
        }
        if record.scheme.is_empty() {
            record.scheme = extract_scheme(&record.url).to_string();
        }

        let host_id = self.strings.intern(&record.host);
        let url_id = self.strings.intern(&record.url);
        let method_id = self.strings.intern(&record.method);
        let scheme_id = self.strings.intern(&record.scheme);
        let version_id = self.strings.intern(&record.http_version);
        let status_text_id = self.strings.intern(&record.status_text);
        let server_id = record
            .server
            .as_ref()
            .map(|value| self.strings.intern(value));
        let headers = record
            .headers
            .into_iter()
            .map(|(name, value)| HeaderEntry {
                name_id: self.strings.intern(name),
                value_id: self.strings.intern(value),
            })
            .collect();

        let tls = record.tls.map(|snapshot| TlsEntry {
            authority_id: snapshot
                .authority
                .as_ref()
                .map(|value| self.strings.intern(value)),
            version_id: snapshot
                .tls_version
                .as_ref()
                .map(|value| self.strings.intern(value)),
            cipher_id: snapshot
                .cipher
                .as_ref()
                .map(|value| self.strings.intern(value)),
            alpn_id: snapshot
                .alpn
                .as_ref()
                .map(|value| self.strings.intern(value)),
            subject_ids: snapshot
                .peer_subjects
                .into_iter()
                .map(|value| self.strings.intern(value))
                .collect(),
            fingerprint_ids: snapshot
                .peer_fingerprints
                .into_iter()
                .map(|value| self.strings.intern(value))
                .collect(),
            ja3_id: snapshot
                .ja3
                .as_ref()
                .map(|value| self.strings.intern(value)),
            ja3s_id: snapshot
                .ja3s
                .as_ref()
                .map(|value| self.strings.intern(value)),
            ja3_raw_id: snapshot
                .ja3_raw
                .as_ref()
                .map(|value| self.strings.intern(value)),
            ja3s_raw_id: snapshot
                .ja3s_raw
                .as_ref()
                .map(|value| self.strings.intern(value)),
            certificate_ids: snapshot
                .certificate_chain_pem
                .into_iter()
                .map(|value| self.strings.intern(value))
                .collect(),
        });

        self.entries.push(Entry {
            host_id,
            url_id,
            method_id,
            scheme_id,
            version_id,
            status_code: record.status_code,
            status_text_id,
            server_id,
            body_size: record.body_size,
            timestamp: record.timestamp,
            headers,
            tls,
        });
        self.sorted = false;
    }

    fn ensure_index(&mut self) {
        if self.sorted {
            return;
        }

        self.entries
            .sort_by(|a, b| match a.host_id.cmp(&b.host_id) {
                std::cmp::Ordering::Equal => a.url_id.cmp(&b.url_id),
                other => other,
            });

        self.host_index.clear();
        let mut current_host: Option<u32> = None;
        let mut start = 0usize;
        for (idx, entry) in self.entries.iter().enumerate() {
            if current_host == Some(entry.host_id) {
                continue;
            }
            if let Some(active) = current_host {
                self.host_index.insert(
                    active,
                    HostRange {
                        start,
                        len: idx - start,
                    },
                );
                start = idx;
            } else {
                start = idx;
            }
            current_host = Some(entry.host_id);
        }
        if let Some(active) = current_host {
            self.host_index.insert(
                active,
                HostRange {
                    start,
                    len: self.entries.len() - start,
                },
            );
        }

        self.sorted = true;
    }

    pub fn records_for_host(&mut self, host: &str) -> Vec<HttpHeadersRecord> {
        self.ensure_index();
        let Some(host_id) = self.strings.get_id(host) else {
            return Vec::new();
        };
        let Some(range) = self.host_index.get(&host_id) else {
            return Vec::new();
        };
        self.entries[range.start..range.start + range.len]
            .iter()
            .map(|entry| self.to_record(entry))
            .collect()
    }

    pub fn all_records(&mut self) -> Vec<HttpHeadersRecord> {
        self.ensure_index();
        self.entries
            .iter()
            .map(|entry| self.to_record(entry))
            .collect()
    }

    pub fn iter(&mut self) -> HttpIter<'_> {
        self.ensure_index();
        HttpIter {
            segment: self,
            pos: 0,
        }
    }

    pub fn iter_mut(&mut self) -> HttpIter<'_> {
        self.iter()
    }

    pub fn serialize(&mut self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.serialize_into(&mut buf);
        buf
    }

    pub fn serialize_into(&mut self, out: &mut Vec<u8>) {
        self.ensure_index();

        let mut hosts: Vec<(u32, HostRange)> = self
            .host_index
            .iter()
            .map(|(host_id, range)| (*host_id, *range))
            .collect();
        hosts.sort_by_key(|(host_id, _)| *host_id);

        let mut directory = Vec::with_capacity(hosts.len());
        let mut payload = Vec::new();

        for (host_id, range) in hosts {
            let start_offset = payload.len() as u64;
            let records_slice = &self.entries[range.start..range.start + range.len];

            let mut block = Vec::new();
            write_varu32(&mut block, records_slice.len() as u32);
            for entry in records_slice {
                write_varu32(&mut block, entry.url_id);
                write_varu32(&mut block, entry.method_id);
                write_varu32(&mut block, entry.scheme_id);
                write_varu32(&mut block, entry.version_id);
                write_varu32(&mut block, entry.status_code as u32);
                write_varu32(&mut block, entry.status_text_id);
                match entry.server_id {
                    Some(id) => {
                        block.push(1);
                        write_varu32(&mut block, id);
                    }
                    None => block.push(0),
                }
                write_varu32(&mut block, entry.body_size);
                write_varu32(&mut block, entry.timestamp);
                write_varu32(&mut block, entry.headers.len() as u32);
                for header in &entry.headers {
                    write_varu32(&mut block, header.name_id);
                    write_varu32(&mut block, header.value_id);
                }
                if let Some(tls) = &entry.tls {
                    block.push(1);
                    write_optional_varu32(&mut block, tls.authority_id);
                    write_optional_varu32(&mut block, tls.version_id);
                    write_optional_varu32(&mut block, tls.cipher_id);
                    write_optional_varu32(&mut block, tls.alpn_id);
                    write_varu32(&mut block, tls.subject_ids.len() as u32);
                    for id in &tls.subject_ids {
                        write_varu32(&mut block, *id);
                    }
                    write_varu32(&mut block, tls.fingerprint_ids.len() as u32);
                    for id in &tls.fingerprint_ids {
                        write_varu32(&mut block, *id);
                    }
                    write_optional_varu32(&mut block, tls.ja3_id);
                    write_optional_varu32(&mut block, tls.ja3s_id);
                    write_optional_varu32(&mut block, tls.ja3_raw_id);
                    write_optional_varu32(&mut block, tls.ja3s_raw_id);
                    write_varu32(&mut block, tls.certificate_ids.len() as u32);
                    for id in &tls.certificate_ids {
                        write_varu32(&mut block, *id);
                    }
                } else {
                    block.push(0);
                }
            }

            let block_len = block.len() as u64;
            payload.extend_from_slice(&block);
            directory.push(HttpDirEntry {
                host_id,
                record_count: records_slice.len() as u32,
                payload_offset: start_offset,
                payload_len: block_len,
            });
        }

        let string_section = encode_string_table(&self.strings);
        let directory_len = (directory.len() * HttpDirEntry::SIZE) as u64;
        let payload_len = payload.len() as u64;
        let strings_len = string_section.len() as u64;

        let header = HttpSegmentHeader {
            version: HttpSegmentHeader::VERSION,
            host_count: directory.len() as u32,
            record_count: self.entries.len() as u32,
            directory_len,
            payload_len,
            strings_len,
        };

        out.clear();
        out.reserve(
            HttpSegmentHeader::SIZE
                + directory.len() * HttpDirEntry::SIZE
                + payload.len()
                + string_section.len(),
        );
        header.write(out);
        HttpDirEntry::write_all(&directory, out);
        out.extend_from_slice(&payload);
        out.extend_from_slice(&string_section);
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < HttpSegmentHeader::SIZE {
            return Err(DecodeError("http segment too small"));
        }
        let header = HttpSegmentHeader::read(bytes)?;

        let mut offset = HttpSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("http directory overflow"))?;
        if dir_end > bytes.len() {
            return Err(DecodeError("http directory out of bounds"));
        }
        let directory_bytes = &bytes[offset..dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("http payload overflow"))?;
        if payload_end > bytes.len() {
            return Err(DecodeError("http payload out of bounds"));
        }
        let payload_bytes = &bytes[offset..payload_end];
        offset = payload_end;

        let strings_end = offset
            .checked_add(header.strings_len as usize)
            .ok_or(DecodeError("http string table overflow"))?;
        if strings_end > bytes.len() {
            return Err(DecodeError("http string table out of bounds"));
        }
        let strings_bytes = &bytes[offset..strings_end];

        let strings = decode_string_table(strings_bytes)?;
        let directory = HttpDirEntry::read_all(directory_bytes, header.host_count as usize)?;

        let mut entries = Vec::with_capacity(header.record_count as usize);
        let mut host_index = HashMap::with_capacity(directory.len());

        for entry in &directory {
            let mut cursor = entry.payload_offset as usize;
            let end = cursor + entry.payload_len as usize;
            if end > payload_bytes.len() {
                return Err(DecodeError("http payload slice out of bounds"));
            }
            let record_count = read_varu32(payload_bytes, &mut cursor)? as usize;
            if record_count as u32 != entry.record_count {
                return Err(DecodeError("http record count mismatch"));
            }

            let start_index = entries.len();
            for _ in 0..record_count {
                let record = decode_http_entry(payload_bytes, &mut cursor, header.version)?;
                entries.push(Entry {
                    host_id: entry.host_id,
                    ..record
                });
            }

            if cursor != end {
                return Err(DecodeError("http payload length mismatch"));
            }

            host_index.insert(
                entry.host_id,
                HostRange {
                    start: start_index,
                    len: record_count,
                },
            );
        }

        Ok(Self {
            strings,
            entries,
            host_index,
            sorted: true,
        })
    }

    fn to_record(&self, entry: &Entry) -> HttpHeadersRecord {
        HttpHeadersRecord {
            host: self.strings.get(entry.host_id).to_string(),
            url: self.strings.get(entry.url_id).to_string(),
            method: self.strings.get(entry.method_id).to_string(),
            scheme: self.strings.get(entry.scheme_id).to_string(),
            http_version: self.strings.get(entry.version_id).to_string(),
            status_code: entry.status_code,
            status_text: self.strings.get(entry.status_text_id).to_string(),
            server: entry.server_id.map(|id| self.strings.get(id).to_string()),
            body_size: entry.body_size,
            headers: entry
                .headers
                .iter()
                .map(|header| {
                    (
                        self.strings.get(header.name_id).to_string(),
                        self.strings.get(header.value_id).to_string(),
                    )
                })
                .collect(),
            timestamp: entry.timestamp,
            tls: entry.tls.as_ref().map(|tls| HttpTlsSnapshot {
                authority: tls.authority_id.map(|id| self.strings.get(id).to_string()),
                tls_version: tls.version_id.map(|id| self.strings.get(id).to_string()),
                cipher: tls.cipher_id.map(|id| self.strings.get(id).to_string()),
                alpn: tls.alpn_id.map(|id| self.strings.get(id).to_string()),
                peer_subjects: tls
                    .subject_ids
                    .iter()
                    .map(|id| self.strings.get(*id).to_string())
                    .collect(),
                peer_fingerprints: tls
                    .fingerprint_ids
                    .iter()
                    .map(|id| self.strings.get(*id).to_string())
                    .collect(),
                ja3: tls.ja3_id.map(|id| self.strings.get(id).to_string()),
                ja3s: tls.ja3s_id.map(|id| self.strings.get(id).to_string()),
                ja3_raw: tls.ja3_raw_id.map(|id| self.strings.get(id).to_string()),
                ja3s_raw: tls.ja3s_raw_id.map(|id| self.strings.get(id).to_string()),
                certificate_chain_pem: tls
                    .certificate_ids
                    .iter()
                    .map(|id| self.strings.get(*id).to_string())
                    .collect(),
            }),
        }
    }
}

impl HttpSegmentView {
    pub fn from_arc(
        data: Arc<Vec<u8>>,
        segment_offset: usize,
        segment_len: usize,
    ) -> Result<Self, DecodeError> {
        if segment_offset + segment_len > data.len() {
            return Err(DecodeError("http segment out of bounds"));
        }
        let bytes = &data[segment_offset..segment_offset + segment_len];
        if bytes.len() < HttpSegmentHeader::SIZE {
            return Err(DecodeError("http segment too small"));
        }
        let header = HttpSegmentHeader::read(bytes)?;

        let mut offset = HttpSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("http directory overflow"))?;
        if segment_offset + dir_end > data.len() {
            return Err(DecodeError("http directory out of bounds"));
        }
        let directory_bytes = &data[segment_offset + offset..segment_offset + dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("http payload overflow"))?;
        if segment_offset + payload_end > data.len() {
            return Err(DecodeError("http payload out of bounds"));
        }
        let payload_offset = segment_offset + offset;
        offset = payload_end;

        let strings_offset = segment_offset + offset;
        let strings_end_abs = strings_offset
            .checked_add(header.strings_len as usize)
            .ok_or(DecodeError("http string table overflow"))?;
        if strings_end_abs > data.len() {
            return Err(DecodeError("http string table out of bounds"));
        }
        let strings_bytes = &data[strings_offset..strings_end_abs];

        let strings = decode_string_table(strings_bytes)?;
        let mut directory = HttpDirEntry::read_all(directory_bytes, header.host_count as usize)?;
        directory.sort_by_key(|entry| entry.host_id);

        Ok(Self {
            strings,
            directory,
            data,
            payload_offset,
            payload_len: header.payload_len as usize,
            version: header.version,
        })
    }

    pub fn records_for_host(&self, host: &str) -> Result<Vec<HttpHeadersRecord>, DecodeError> {
        let Some(host_id) = self.strings.get_id(host) else {
            return Ok(Vec::new());
        };
        let Some(dir) = self.directory.iter().find(|entry| entry.host_id == host_id) else {
            return Ok(Vec::new());
        };

        let payload = &self.data[self.payload_offset..self.payload_offset + self.payload_len];
        decode_http_records(
            payload,
            dir.payload_offset,
            dir.payload_len,
            dir.record_count,
            host_id,
            &self.strings,
            self.version,
        )
    }
}

impl<'a> Iterator for HttpIter<'a> {
    type Item = HttpHeadersRecord;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.segment.entries.len() {
            return None;
        }
        let record = self.segment.to_record(&self.segment.entries[self.pos]);
        self.pos += 1;
        Some(record)
    }
}

fn decode_http_entry(bytes: &[u8], pos: &mut usize, version: u16) -> Result<Entry, DecodeError> {
    let url_id = read_varu32(bytes, pos)?;
    let method_id = read_varu32(bytes, pos)?;
    let scheme_id = read_varu32(bytes, pos)?;
    let version_id = read_varu32(bytes, pos)?;
    let status_code = read_varu32(bytes, pos)? as u16;
    let status_text_id = read_varu32(bytes, pos)?;
    if *pos >= bytes.len() {
        return Err(DecodeError("unexpected eof (http server flag)"));
    }
    let server_id = if bytes[*pos] != 0 {
        *pos += 1;
        Some(read_varu32(bytes, pos)?)
    } else {
        *pos += 1;
        None
    };
    let body_size = read_varu32(bytes, pos)?;
    let timestamp = read_varu32(bytes, pos)?;
    let header_count = read_varu32(bytes, pos)? as usize;
    let mut headers = Vec::with_capacity(header_count);
    for _ in 0..header_count {
        let name_id = read_varu32(bytes, pos)?;
        let value_id = read_varu32(bytes, pos)?;
        headers.push(HeaderEntry { name_id, value_id });
    }
    let tls = if version >= 2 {
        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (http tls flag)"));
        }
        let has_tls = bytes[*pos] != 0;
        *pos += 1;
        if has_tls {
            let authority_id = read_optional_varu32(bytes, pos)?;
            let version_id = read_optional_varu32(bytes, pos)?;
            let cipher_id = read_optional_varu32(bytes, pos)?;
            let alpn_id = read_optional_varu32(bytes, pos)?;
            let subject_count = read_varu32(bytes, pos)? as usize;
            let mut subject_ids = Vec::with_capacity(subject_count);
            for _ in 0..subject_count {
                subject_ids.push(read_varu32(bytes, pos)?);
            }
            let fingerprint_count = read_varu32(bytes, pos)? as usize;
            let mut fingerprint_ids = Vec::with_capacity(fingerprint_count);
            for _ in 0..fingerprint_count {
                fingerprint_ids.push(read_varu32(bytes, pos)?);
            }
            let (ja3_id, ja3s_id, ja3_raw_id, ja3s_raw_id, certificate_ids) = if version >= 3 {
                let ja3_id = read_optional_varu32(bytes, pos)?;
                let ja3s_id = read_optional_varu32(bytes, pos)?;
                let ja3_raw_id = read_optional_varu32(bytes, pos)?;
                let ja3s_raw_id = read_optional_varu32(bytes, pos)?;
                let cert_count = read_varu32(bytes, pos)? as usize;
                let mut certificate_ids = Vec::with_capacity(cert_count);
                for _ in 0..cert_count {
                    certificate_ids.push(read_varu32(bytes, pos)?);
                }
                (ja3_id, ja3s_id, ja3_raw_id, ja3s_raw_id, certificate_ids)
            } else {
                (None, None, None, None, Vec::new())
            };
            Some(TlsEntry {
                authority_id,
                version_id,
                cipher_id,
                alpn_id,
                subject_ids,
                fingerprint_ids,
                ja3_id,
                ja3s_id,
                ja3_raw_id,
                ja3s_raw_id,
                certificate_ids,
            })
        } else {
            None
        }
    } else {
        None
    };
    Ok(Entry {
        host_id: 0,
        url_id,
        method_id,
        scheme_id,
        version_id,
        status_code,
        status_text_id,
        server_id,
        body_size,
        timestamp,
        headers,
        tls,
    })
}

fn decode_http_records(
    payload: &[u8],
    offset: u64,
    length: u64,
    expected_count: u32,
    host_id: u32,
    strings: &StringTable,
    version: u16,
) -> Result<Vec<HttpHeadersRecord>, DecodeError> {
    let mut cursor = offset as usize;
    let end = cursor + length as usize;
    if end > payload.len() {
        return Err(DecodeError("http payload slice out of bounds"));
    }
    let count = read_varu32(payload, &mut cursor)? as usize;
    if count as u32 != expected_count {
        return Err(DecodeError("http record count mismatch"));
    }

    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let entry = decode_http_entry(payload, &mut cursor, version)?;
        let tls = entry.tls.as_ref().map(|tls_entry| HttpTlsSnapshot {
            authority: tls_entry.authority_id.map(|id| strings.get(id).to_string()),
            tls_version: tls_entry.version_id.map(|id| strings.get(id).to_string()),
            cipher: tls_entry.cipher_id.map(|id| strings.get(id).to_string()),
            alpn: tls_entry.alpn_id.map(|id| strings.get(id).to_string()),
            peer_subjects: tls_entry
                .subject_ids
                .iter()
                .map(|id| strings.get(*id).to_string())
                .collect(),
            peer_fingerprints: tls_entry
                .fingerprint_ids
                .iter()
                .map(|id| strings.get(*id).to_string())
                .collect(),
            ja3: tls_entry.ja3_id.map(|id| strings.get(id).to_string()),
            ja3s: tls_entry.ja3s_id.map(|id| strings.get(id).to_string()),
            ja3_raw: tls_entry.ja3_raw_id.map(|id| strings.get(id).to_string()),
            ja3s_raw: tls_entry.ja3s_raw_id.map(|id| strings.get(id).to_string()),
            certificate_chain_pem: tls_entry
                .certificate_ids
                .iter()
                .map(|id| strings.get(*id).to_string())
                .collect(),
        });

        records.push(HttpHeadersRecord {
            host: strings.get(host_id).to_string(),
            url: strings.get(entry.url_id).to_string(),
            method: strings.get(entry.method_id).to_string(),
            scheme: strings.get(entry.scheme_id).to_string(),
            http_version: strings.get(entry.version_id).to_string(),
            status_code: entry.status_code,
            status_text: strings.get(entry.status_text_id).to_string(),
            server: entry.server_id.map(|id| strings.get(id).to_string()),
            body_size: entry.body_size,
            headers: entry
                .headers
                .iter()
                .map(|header| {
                    (
                        strings.get(header.name_id).to_string(),
                        strings.get(header.value_id).to_string(),
                    )
                })
                .collect(),
            timestamp: entry.timestamp,
            tls,
        });
    }

    if cursor != end {
        return Err(DecodeError("http payload length mismatch"));
    }

    Ok(records)
}

fn extract_host(url: &str) -> &str {
    let without_scheme = if let Some(idx) = url.find("://") {
        &url[idx + 3..]
    } else {
        url
    };
    without_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .split('@')
        .last()
        .unwrap_or("")
}

fn extract_scheme(url: &str) -> &str {
    url.split("://").next().unwrap_or("http")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::records::HttpTlsSnapshot;

    #[test]
    fn roundtrip_http_segment() {
        let mut segment = HttpSegment::new();
        segment.insert(HttpHeadersRecord {
            host: "example.com".into(),
            url: "https://example.com/".into(),
            method: "GET".into(),
            scheme: "https".into(),
            http_version: "HTTP/1.1".into(),
            status_code: 200,
            status_text: "OK".into(),
            server: Some("ExampleServer".into()),
            body_size: 1234,
            headers: vec![
                ("content-type".into(), "text/html".into()),
                ("server".into(), "ExampleServer".into()),
            ],
            timestamp: 1_700_000_000,
            tls: None,
        });

        let encoded = segment.serialize();
        let mut decoded = HttpSegment::deserialize(&encoded).expect("decode");
        let records = decoded.all_records();
        assert_eq!(records.len(), 1);
        let rec = &records[0];
        assert_eq!(rec.host, "example.com");
        assert_eq!(rec.scheme, "https");
        assert_eq!(rec.method, "GET");
        assert_eq!(rec.status_text, "OK");
        assert_eq!(rec.body_size, 1234);
    }

    #[test]
    fn roundtrip_http_segment_with_tls() {
        let mut segment = HttpSegment::new();
        segment.insert(HttpHeadersRecord {
            host: "tls.example.com".into(),
            url: "https://tls.example.com/secure".into(),
            method: "GET".into(),
            scheme: "https".into(),
            http_version: "HTTP/2".into(),
            status_code: 200,
            status_text: "".into(),
            server: Some("h2-server".into()),
            body_size: 42,
            headers: vec![("content-type".into(), "application/json".into())],
            timestamp: 1_800_000_000,
            tls: Some(HttpTlsSnapshot {
                authority: Some("tls.example.com".into()),
                tls_version: Some("TLS1.3".into()),
                cipher: Some("TLS_AES_128_GCM_SHA256".into()),
                alpn: Some("h2".into()),
                peer_subjects: vec!["CN=tls.example.com".into()],
                peer_fingerprints: vec!["AA:BB:CC".into()],
                ja3: Some("d41d8cd98f00b204e9800998ecf8427e".into()),
                ja3s: Some("0f343b0931126a20f133d67c2b018a3b".into()),
                ja3_raw: Some("771,4865-4866-4867,0-11,29-23-24,0".into()),
                ja3s_raw: Some("771,4865,0-16,29-23-24,0".into()),
                certificate_chain_pem: vec![
                    "-----BEGIN CERTIFICATE-----\nFAKE\n-----END CERTIFICATE-----".into(),
                ],
            }),
        });

        let encoded = segment.serialize();
        let mut decoded = HttpSegment::deserialize(&encoded).expect("decode");
        let records = decoded.all_records();
        assert_eq!(records.len(), 1);
        let rec = &records[0];
        let tls = rec.tls.as_ref().expect("tls metadata");
        assert_eq!(tls.authority.as_deref(), Some("tls.example.com"));
        assert_eq!(tls.tls_version.as_deref(), Some("TLS1.3"));
        assert_eq!(tls.alpn.as_deref(), Some("h2"));
        assert_eq!(tls.peer_subjects, vec!["CN=tls.example.com".to_string()]);
        assert_eq!(tls.peer_fingerprints, vec!["AA:BB:CC".to_string()]);
        assert_eq!(tls.ja3.as_deref(), Some("d41d8cd98f00b204e9800998ecf8427e"));
        assert_eq!(
            tls.ja3s.as_deref(),
            Some("0f343b0931126a20f133d67c2b018a3b")
        );
        assert_eq!(tls.certificate_chain_pem.len(), 1);
    }

    #[test]
    fn http_segment_view_reads_host() {
        let mut segment = HttpSegment::new();
        segment.insert(HttpHeadersRecord {
            host: "example.com".into(),
            url: "https://example.com/".into(),
            method: "GET".into(),
            scheme: "https".into(),
            http_version: "HTTP/1.1".into(),
            status_code: 200,
            status_text: "OK".into(),
            server: Some("ExampleServer".into()),
            body_size: 100,
            headers: vec![("content-type".into(), "text/html".into())],
            timestamp: 10,
            tls: None,
        });
        segment.insert(HttpHeadersRecord {
            host: "api.example.com".into(),
            url: "https://api.example.com/v1".into(),
            method: "POST".into(),
            scheme: "https".into(),
            http_version: "HTTP/2".into(),
            status_code: 201,
            status_text: "Created".into(),
            server: None,
            body_size: 512,
            headers: vec![],
            timestamp: 20,
            tls: None,
        });

        let encoded = segment.serialize();
        let data = Arc::new(encoded);
        let view =
            HttpSegmentView::from_arc(Arc::clone(&data), 0, data.len()).expect("view loaded");
        let records = view.records_for_host("api.example.com").expect("records");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status_code, 201);
    }

    #[test]
    fn http_segment_view_provides_tls_metadata() {
        let mut segment = HttpSegment::new();
        segment.insert(HttpHeadersRecord {
            host: "secure.example.com".into(),
            url: "https://secure.example.com/api".into(),
            method: "POST".into(),
            scheme: "https".into(),
            http_version: "HTTP/2".into(),
            status_code: 204,
            status_text: "".into(),
            server: Some("secure-h2".into()),
            body_size: 0,
            headers: vec![("content-length".into(), "0".into())],
            timestamp: 25,
            tls: Some(HttpTlsSnapshot {
                authority: Some("secure.example.com".into()),
                tls_version: Some("TLS1.3".into()),
                cipher: Some("TLS_AES_256_GCM_SHA384".into()),
                alpn: Some("h2".into()),
                peer_subjects: vec!["CN=secure.example.com".into()],
                peer_fingerprints: vec!["FF:EE:DD".into()],
                ja3: Some("d41d8cd98f00b204e9800998ecf8427e".into()),
                ja3s: Some("0f343b0931126a20f133d67c2b018a3b".into()),
                ja3_raw: Some("771,4865-4866-4867,0-11,29-23-24,0".into()),
                ja3s_raw: Some("771,4866,0-16,29-23-24,0".into()),
                certificate_chain_pem: vec![
                    "-----BEGIN CERTIFICATE-----\nANOTHER\n-----END CERTIFICATE-----".into(),
                ],
            }),
        });

        let encoded = segment.serialize();
        let data = Arc::new(encoded);
        let view =
            HttpSegmentView::from_arc(Arc::clone(&data), 0, data.len()).expect("view loaded");
        let records = view
            .records_for_host("secure.example.com")
            .expect("records");
        assert_eq!(records.len(), 1);
        let tls = records[0].tls.as_ref().expect("tls metadata");
        assert_eq!(tls.tls_version.as_deref(), Some("TLS1.3"));
        assert_eq!(tls.cipher.as_deref(), Some("TLS_AES_256_GCM_SHA384"));
        assert_eq!(tls.peer_subjects, vec!["CN=secure.example.com".to_string()]);
        assert_eq!(tls.peer_fingerprints, vec!["FF:EE:DD".to_string()]);
        assert_eq!(tls.ja3.as_deref(), Some("d41d8cd98f00b204e9800998ecf8427e"));
        assert_eq!(
            tls.ja3s.as_deref(),
            Some("0f343b0931126a20f133d67c2b018a3b")
        );
        assert_eq!(tls.certificate_chain_pem.len(), 1);
    }
}
