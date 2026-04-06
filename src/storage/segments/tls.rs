use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::primitives::encoding::{
    read_string, read_varu32, write_string, write_varu32, DecodeError,
};
use crate::storage::records::{
    TlsCertRecord, TlsCipherRecord, TlsCipherStrength, TlsScanRecord, TlsSeverity,
    TlsVersionRecord, TlsVulnerabilityRecord,
};
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
struct VersionEntry {
    version_id: u32,
    supported: bool,
    error_id: Option<u32>,
}

#[derive(Debug, Clone)]
struct CipherEntry {
    name_id: u32,
    code: u16,
    strength: u8,
}

#[derive(Debug, Clone)]
struct VulnerabilityEntry {
    name_id: u32,
    severity: u8,
    description_id: u32,
}

#[derive(Debug, Clone)]
struct CertEntry {
    domain_id: u32,
    issuer_id: u32,
    subject_id: u32,
    serial_id: u32,
    signature_algorithm_id: u32,
    public_key_algorithm_id: u32,
    version: u8,
    not_before: u32,
    not_after: u32,
    timestamp: u32,
    self_signed: bool,
    sans: Vec<u32>,
}

#[derive(Debug, Clone)]
struct ScanEntry {
    host_id: u32,
    port: u16,
    timestamp: u32,
    negotiated_version_id: Option<u32>,
    negotiated_cipher_id: Option<u32>,
    negotiated_cipher_code: Option<u16>,
    negotiated_strength: u8,
    certificate_valid: bool,
    versions: Vec<VersionEntry>,
    ciphers: Vec<CipherEntry>,
    vulnerabilities: Vec<VulnerabilityEntry>,
    certificates: Vec<CertEntry>,
    ja3_id: Option<u32>,
    ja3s_id: Option<u32>,
    ja3_raw_id: Option<u32>,
    ja3s_raw_id: Option<u32>,
    peer_fingerprint_ids: Vec<u32>,
    certificate_pem_ids: Vec<u32>,
}

#[derive(Debug, Clone, Copy)]
struct TlsDirEntry {
    host_id: u32,
    scan_count: u32,
    payload_offset: u64,
    payload_len: u64,
}

impl TlsDirEntry {
    const SIZE: usize = 4 + 4 + 8 + 8;

    fn write_all(entries: &[Self], buf: &mut Vec<u8>) {
        for entry in entries {
            buf.extend_from_slice(&entry.host_id.to_le_bytes());
            buf.extend_from_slice(&entry.scan_count.to_le_bytes());
            buf.extend_from_slice(&entry.payload_offset.to_le_bytes());
            buf.extend_from_slice(&entry.payload_len.to_le_bytes());
        }
    }

    fn read_all(bytes: &[u8], count: usize) -> Result<Vec<Self>, DecodeError> {
        if bytes.len() != count * Self::SIZE {
            return Err(DecodeError("invalid tls directory size"));
        }
        let mut entries = Vec::with_capacity(count);
        let mut offset = 0usize;
        for _ in 0..count {
            let host_id = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            offset += 4;
            let scan_count = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            offset += 4;
            let payload_offset = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let payload_len = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            entries.push(Self {
                host_id,
                scan_count,
                payload_offset,
                payload_len,
            });
        }
        Ok(entries)
    }
}

#[derive(Debug, Clone, Copy)]
struct TlsSegmentHeader {
    version: u16,
    host_count: u32,
    scan_count: u32,
    directory_len: u64,
    payload_len: u64,
    strings_len: u64,
}

impl TlsSegmentHeader {
    const MAGIC: [u8; 4] = *b"TL01";
    const VERSION: u16 = 2;
    const SIZE: usize = 4 + 2 + 2 + 4 + 4 + 8 + 8 + 8;

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&Self::MAGIC);
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // reserved
        buf.extend_from_slice(&self.host_count.to_le_bytes());
        buf.extend_from_slice(&self.scan_count.to_le_bytes());
        buf.extend_from_slice(&self.directory_len.to_le_bytes());
        buf.extend_from_slice(&self.payload_len.to_le_bytes());
        buf.extend_from_slice(&self.strings_len.to_le_bytes());
    }

    fn read(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < Self::SIZE {
            return Err(DecodeError("tls header too small"));
        }
        if bytes[0..4] != Self::MAGIC {
            return Err(DecodeError("invalid tls segment magic"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != 1 && version != Self::VERSION {
            return Err(DecodeError("unsupported tls segment version"));
        }
        let host_count = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let scan_count = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let directory_len = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let payload_len = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        let strings_len = u64::from_le_bytes(bytes[32..40].try_into().unwrap());

        Ok(Self {
            version,
            host_count,
            scan_count,
            directory_len,
            payload_len,
            strings_len,
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct TlsSegment {
    strings: StringTable,
    scans: Vec<ScanEntry>,
    host_map: HashMap<u32, Vec<usize>>,
}

pub struct TlsSegmentView {
    strings: StringTable,
    directory: Vec<TlsDirEntry>,
    data: Arc<Vec<u8>>,
    payload_offset: usize,
    payload_len: usize,
    version: u16,
}

impl TlsSegment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.scans.len()
    }

    pub fn insert(&mut self, record: TlsScanRecord) {
        let TlsScanRecord {
            host,
            port,
            timestamp,
            negotiated_version,
            negotiated_cipher,
            negotiated_cipher_code,
            negotiated_cipher_strength,
            certificate_valid,
            versions,
            ciphers,
            vulnerabilities,
            certificate_chain,
            ja3,
            ja3s,
            ja3_raw,
            ja3s_raw,
            peer_fingerprints,
            certificate_chain_pem,
        } = record;

        let host_id = self.strings.intern(&host);
        let negotiated_version_id = negotiated_version
            .as_ref()
            .map(|value| self.strings.intern(value));
        let negotiated_cipher_id = negotiated_cipher
            .as_ref()
            .map(|value| self.strings.intern(value));
        let negotiated_strength = strength_to_u8(negotiated_cipher_strength);

        let versions = versions
            .into_iter()
            .map(|version| VersionEntry {
                version_id: self.strings.intern(version.version),
                supported: version.supported,
                error_id: version.error.map(|err| self.strings.intern(err)),
            })
            .collect();

        let ciphers = ciphers
            .into_iter()
            .map(|cipher| CipherEntry {
                name_id: self.strings.intern(cipher.name),
                code: cipher.code,
                strength: strength_to_u8(cipher.strength),
            })
            .collect();

        let vulnerabilities = vulnerabilities
            .into_iter()
            .map(|vuln| VulnerabilityEntry {
                name_id: self.strings.intern(vuln.name),
                severity: severity_to_u8(vuln.severity),
                description_id: self.strings.intern(vuln.description),
            })
            .collect();

        let certificates = certificate_chain
            .into_iter()
            .map(|cert| CertEntry {
                domain_id: self.strings.intern(cert.domain),
                issuer_id: self.strings.intern(cert.issuer),
                subject_id: self.strings.intern(cert.subject),
                serial_id: self.strings.intern(cert.serial_number),
                signature_algorithm_id: self.strings.intern(cert.signature_algorithm),
                public_key_algorithm_id: self.strings.intern(cert.public_key_algorithm),
                version: cert.version,
                not_before: cert.not_before,
                not_after: cert.not_after,
                timestamp: cert.timestamp,
                self_signed: cert.self_signed,
                sans: cert
                    .sans
                    .into_iter()
                    .map(|san| self.strings.intern(san))
                    .collect(),
            })
            .collect();

        let ja3_id = ja3.as_ref().map(|value| self.strings.intern(value));
        let ja3s_id = ja3s.as_ref().map(|value| self.strings.intern(value));
        let ja3_raw_id = ja3_raw.as_ref().map(|value| self.strings.intern(value));
        let ja3s_raw_id = ja3s_raw.as_ref().map(|value| self.strings.intern(value));

        let peer_fingerprint_ids = peer_fingerprints
            .into_iter()
            .map(|fp| self.strings.intern(fp))
            .collect();

        let certificate_pem_ids = certificate_chain_pem
            .into_iter()
            .map(|pem| self.strings.intern(pem))
            .collect();

        let index = self.scans.len();
        self.scans.push(ScanEntry {
            host_id,
            port,
            timestamp,
            negotiated_version_id,
            negotiated_cipher_id,
            negotiated_cipher_code,
            negotiated_strength,
            certificate_valid,
            versions,
            ciphers,
            vulnerabilities,
            certificates,
            ja3_id,
            ja3s_id,
            ja3_raw_id,
            ja3s_raw_id,
            peer_fingerprint_ids,
            certificate_pem_ids,
        });

        self.host_map.entry(host_id).or_default().push(index);
    }

    pub fn scans_for_host(&self, host: &str) -> Vec<TlsScanRecord> {
        let Some(host_id) = self.strings.get_id(host) else {
            return Vec::new();
        };
        self.host_map
            .get(&host_id)
            .into_iter()
            .flatten()
            .map(|&idx| self.to_record(&self.scans[idx]))
            .collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = TlsScanRecord> + '_ {
        self.scans.iter().map(|entry| self.to_record(entry))
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.serialize_into(&mut buf);
        buf
    }

    pub fn serialize_into(&self, out: &mut Vec<u8>) {
        let mut hosts: Vec<(u32, Vec<usize>)> = self
            .host_map
            .iter()
            .map(|(host_id, indices)| {
                let mut sorted = indices.clone();
                sorted.sort_unstable();
                (*host_id, sorted)
            })
            .collect();
        hosts.sort_by_key(|(host_id, _)| *host_id);

        let mut directory = Vec::with_capacity(hosts.len());
        let mut payload = Vec::new();

        for (host_id, indices) in hosts {
            let scan_count = indices.len() as u32;
            let start_offset = payload.len() as u64;
            let mut block = Vec::new();
            write_varu32(&mut block, scan_count);
            for idx in indices {
                let scan = &self.scans[idx];
                write_varu32(&mut block, scan.port as u32);
                write_varu32(&mut block, scan.timestamp);
                write_optional_id(&mut block, scan.negotiated_version_id);
                write_optional_id(&mut block, scan.negotiated_cipher_id);
                write_optional_u16(&mut block, scan.negotiated_cipher_code);
                block.push(scan.negotiated_strength);
                block.push(scan.certificate_valid as u8);

                encode_versions(&mut block, &scan.versions);
                encode_ciphers(&mut block, &scan.ciphers);
                encode_vulnerabilities(&mut block, &scan.vulnerabilities);
                encode_certificates(&mut block, &scan.certificates);
                write_optional_id(&mut block, scan.ja3_id);
                write_optional_id(&mut block, scan.ja3s_id);
                write_optional_id(&mut block, scan.ja3_raw_id);
                write_optional_id(&mut block, scan.ja3s_raw_id);
                write_varu32(&mut block, scan.peer_fingerprint_ids.len() as u32);
                for id in &scan.peer_fingerprint_ids {
                    write_varu32(&mut block, *id);
                }
                write_varu32(&mut block, scan.certificate_pem_ids.len() as u32);
                for id in &scan.certificate_pem_ids {
                    write_varu32(&mut block, *id);
                }
            }
            let block_len = block.len() as u64;
            payload.extend_from_slice(&block);
            directory.push(TlsDirEntry {
                host_id,
                scan_count,
                payload_offset: start_offset,
                payload_len: block_len,
            });
        }

        let string_section = encode_string_table(&self.strings);
        let directory_len = (directory.len() * TlsDirEntry::SIZE) as u64;
        let payload_len = payload.len() as u64;
        let strings_len = string_section.len() as u64;

        let header = TlsSegmentHeader {
            version: TlsSegmentHeader::VERSION,
            host_count: directory.len() as u32,
            scan_count: self.scans.len() as u32,
            directory_len,
            payload_len,
            strings_len,
        };

        out.clear();
        out.reserve(
            TlsSegmentHeader::SIZE
                + directory.len() * TlsDirEntry::SIZE
                + payload.len()
                + string_section.len(),
        );
        header.write(out);
        TlsDirEntry::write_all(&directory, out);
        out.extend_from_slice(&payload);
        out.extend_from_slice(&string_section);
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < TlsSegmentHeader::SIZE {
            return Err(DecodeError("tls segment too small"));
        }
        let header = TlsSegmentHeader::read(bytes)?;

        let mut offset = TlsSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("tls directory overflow"))?;
        if dir_end > bytes.len() {
            return Err(DecodeError("tls directory out of bounds"));
        }
        let directory_bytes = &bytes[offset..dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("tls payload overflow"))?;
        if payload_end > bytes.len() {
            return Err(DecodeError("tls payload out of bounds"));
        }
        let payload_bytes = &bytes[offset..payload_end];
        offset = payload_end;

        let strings_end = offset
            .checked_add(header.strings_len as usize)
            .ok_or(DecodeError("tls string table overflow"))?;
        if strings_end > bytes.len() {
            return Err(DecodeError("tls string table out of bounds"));
        }
        let strings_bytes = &bytes[offset..strings_end];

        let directory = TlsDirEntry::read_all(directory_bytes, header.host_count as usize)?;
        let strings = decode_string_table(strings_bytes)?;

        let mut scans = Vec::with_capacity(header.scan_count as usize);
        let mut host_map: HashMap<u32, Vec<usize>> = HashMap::with_capacity(directory.len());

        for entry in &directory {
            let mut cursor = entry.payload_offset as usize;
            let end = cursor + entry.payload_len as usize;
            if end > payload_bytes.len() {
                return Err(DecodeError("tls payload slice out of bounds"));
            }
            let scan_count = read_varu32(payload_bytes, &mut cursor)? as usize;
            if scan_count as u32 != entry.scan_count {
                return Err(DecodeError("tls scan count mismatch"));
            }

            for _ in 0..scan_count {
                let scan = decode_scan_entry(payload_bytes, &mut cursor, header.version)?;
                let idx = scans.len();
                scans.push(ScanEntry {
                    host_id: entry.host_id,
                    ..scan
                });
                host_map.entry(entry.host_id).or_default().push(idx);
            }

            if cursor != end {
                return Err(DecodeError("tls payload length mismatch"));
            }
        }

        Ok(Self {
            strings,
            scans,
            host_map,
        })
    }

    fn to_record(&self, entry: &ScanEntry) -> TlsScanRecord {
        let versions = entry
            .versions
            .iter()
            .map(|version| TlsVersionRecord {
                version: self.strings.get(version.version_id).to_string(),
                supported: version.supported,
                error: version.error_id.map(|id| self.strings.get(id).to_string()),
            })
            .collect();

        let ciphers = entry
            .ciphers
            .iter()
            .map(|cipher| TlsCipherRecord {
                name: self.strings.get(cipher.name_id).to_string(),
                code: cipher.code,
                strength: u8_to_strength(cipher.strength),
            })
            .collect();

        let vulnerabilities = entry
            .vulnerabilities
            .iter()
            .map(|vuln| TlsVulnerabilityRecord {
                name: self.strings.get(vuln.name_id).to_string(),
                severity: u8_to_severity(vuln.severity),
                description: self.strings.get(vuln.description_id).to_string(),
            })
            .collect();

        let certificates = entry
            .certificates
            .iter()
            .map(|cert| TlsCertRecord {
                domain: self.strings.get(cert.domain_id).to_string(),
                issuer: self.strings.get(cert.issuer_id).to_string(),
                subject: self.strings.get(cert.subject_id).to_string(),
                serial_number: self.strings.get(cert.serial_id).to_string(),
                signature_algorithm: self.strings.get(cert.signature_algorithm_id).to_string(),
                public_key_algorithm: self.strings.get(cert.public_key_algorithm_id).to_string(),
                version: cert.version,
                not_before: cert.not_before,
                not_after: cert.not_after,
                sans: cert
                    .sans
                    .iter()
                    .map(|id| self.strings.get(*id).to_string())
                    .collect(),
                self_signed: cert.self_signed,
                timestamp: cert.timestamp,
            })
            .collect();

        TlsScanRecord {
            host: self.strings.get(entry.host_id).to_string(),
            port: entry.port,
            timestamp: entry.timestamp,
            negotiated_version: entry
                .negotiated_version_id
                .map(|id| self.strings.get(id).to_string()),
            negotiated_cipher: entry
                .negotiated_cipher_id
                .map(|id| self.strings.get(id).to_string()),
            negotiated_cipher_code: entry.negotiated_cipher_code,
            negotiated_cipher_strength: u8_to_strength(entry.negotiated_strength),
            certificate_valid: entry.certificate_valid,
            versions,
            ciphers,
            vulnerabilities,
            certificate_chain: certificates,
            ja3: entry.ja3_id.map(|id| self.strings.get(id).to_string()),
            ja3s: entry.ja3s_id.map(|id| self.strings.get(id).to_string()),
            ja3_raw: entry.ja3_raw_id.map(|id| self.strings.get(id).to_string()),
            ja3s_raw: entry.ja3s_raw_id.map(|id| self.strings.get(id).to_string()),
            peer_fingerprints: entry
                .peer_fingerprint_ids
                .iter()
                .map(|id| self.strings.get(*id).to_string())
                .collect(),
            certificate_chain_pem: entry
                .certificate_pem_ids
                .iter()
                .map(|id| self.strings.get(*id).to_string())
                .collect(),
        }
    }
}

impl TlsSegmentView {
    pub fn from_arc(
        data: Arc<Vec<u8>>,
        segment_offset: usize,
        segment_len: usize,
    ) -> Result<Self, DecodeError> {
        if segment_offset + segment_len > data.len() {
            return Err(DecodeError("tls segment out of bounds"));
        }
        let bytes = &data[segment_offset..segment_offset + segment_len];
        if bytes.len() < TlsSegmentHeader::SIZE {
            return Err(DecodeError("tls segment too small"));
        }
        let header = TlsSegmentHeader::read(bytes)?;

        let mut offset = TlsSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("tls directory overflow"))?;
        if segment_offset + dir_end > data.len() {
            return Err(DecodeError("tls directory out of bounds"));
        }
        let directory_bytes = &data[segment_offset + offset..segment_offset + dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("tls payload overflow"))?;
        if segment_offset + payload_end > data.len() {
            return Err(DecodeError("tls payload out of bounds"));
        }
        let payload_offset = segment_offset + offset;
        offset = payload_end;

        let strings_offset = segment_offset + offset;
        let strings_end_abs = strings_offset
            .checked_add(header.strings_len as usize)
            .ok_or(DecodeError("tls string table overflow"))?;
        if strings_end_abs > data.len() {
            return Err(DecodeError("tls string table out of bounds"));
        }
        let strings_bytes = &data[strings_offset..strings_end_abs];

        let mut directory = TlsDirEntry::read_all(directory_bytes, header.host_count as usize)?;
        directory.sort_by_key(|entry| entry.host_id);
        let strings = decode_string_table(strings_bytes)?;

        Ok(Self {
            strings,
            directory,
            data,
            payload_offset,
            payload_len: header.payload_len as usize,
            version: header.version,
        })
    }

    pub fn records_for_host(&self, host: &str) -> Result<Vec<TlsScanRecord>, DecodeError> {
        let Some(host_id) = self.strings.get_id(host) else {
            return Ok(Vec::new());
        };
        let entry = self.directory.iter().find(|dir| dir.host_id == host_id);
        let Some(dir) = entry else {
            return Ok(Vec::new());
        };

        let payload = &self.data[self.payload_offset..self.payload_offset + self.payload_len];
        decode_scan_records(
            payload,
            dir.payload_offset,
            dir.payload_len,
            dir.scan_count,
            host,
            &self.strings,
            self.version,
        )
    }
}

fn encode_versions(buf: &mut Vec<u8>, versions: &[VersionEntry]) {
    write_varu32(buf, versions.len() as u32);
    for version in versions {
        write_varu32(buf, version.version_id);
        buf.push(version.supported as u8);
        write_optional_id(buf, version.error_id);
    }
}

fn decode_versions(bytes: &[u8], pos: &mut usize) -> Result<Vec<VersionEntry>, DecodeError> {
    let count = read_varu32(bytes, pos)? as usize;
    let mut versions = Vec::with_capacity(count);
    for _ in 0..count {
        let version_id = read_varu32(bytes, pos)?;
        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (tls version supported)"));
        }
        let supported = bytes[*pos] != 0;
        *pos += 1;
        let error_id = read_optional_id(bytes, pos)?;
        versions.push(VersionEntry {
            version_id,
            supported,
            error_id,
        });
    }
    Ok(versions)
}

fn encode_ciphers(buf: &mut Vec<u8>, ciphers: &[CipherEntry]) {
    write_varu32(buf, ciphers.len() as u32);
    for cipher in ciphers {
        write_varu32(buf, cipher.name_id);
        buf.extend_from_slice(&cipher.code.to_le_bytes());
        buf.push(cipher.strength);
    }
}

fn decode_ciphers(bytes: &[u8], pos: &mut usize) -> Result<Vec<CipherEntry>, DecodeError> {
    let count = read_varu32(bytes, pos)? as usize;
    let mut ciphers = Vec::with_capacity(count);
    for _ in 0..count {
        let name_id = read_varu32(bytes, pos)?;
        if *pos + 2 > bytes.len() {
            return Err(DecodeError("unexpected eof (tls cipher code)"));
        }
        let code = u16::from_le_bytes(bytes[*pos..*pos + 2].try_into().unwrap());
        *pos += 2;
        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (tls cipher strength)"));
        }
        let strength = bytes[*pos];
        *pos += 1;
        ciphers.push(CipherEntry {
            name_id,
            code,
            strength,
        });
    }
    Ok(ciphers)
}

fn encode_vulnerabilities(buf: &mut Vec<u8>, vulns: &[VulnerabilityEntry]) {
    write_varu32(buf, vulns.len() as u32);
    for vuln in vulns {
        write_varu32(buf, vuln.name_id);
        buf.push(vuln.severity);
        write_varu32(buf, vuln.description_id);
    }
}

fn decode_vulnerabilities(
    bytes: &[u8],
    pos: &mut usize,
) -> Result<Vec<VulnerabilityEntry>, DecodeError> {
    let count = read_varu32(bytes, pos)? as usize;
    let mut vulns = Vec::with_capacity(count);
    for _ in 0..count {
        let name_id = read_varu32(bytes, pos)?;
        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (tls vuln severity)"));
        }
        let severity = bytes[*pos];
        *pos += 1;
        let description_id = read_varu32(bytes, pos)?;
        vulns.push(VulnerabilityEntry {
            name_id,
            severity,
            description_id,
        });
    }
    Ok(vulns)
}

fn encode_certificates(buf: &mut Vec<u8>, certs: &[CertEntry]) {
    write_varu32(buf, certs.len() as u32);
    for cert in certs {
        write_varu32(buf, cert.domain_id);
        write_varu32(buf, cert.issuer_id);
        write_varu32(buf, cert.subject_id);
        write_varu32(buf, cert.serial_id);
        write_varu32(buf, cert.signature_algorithm_id);
        write_varu32(buf, cert.public_key_algorithm_id);
        buf.push(cert.version);
        write_varu32(buf, cert.not_before);
        write_varu32(buf, cert.not_after);
        buf.push(cert.self_signed as u8);
        write_varu32(buf, cert.timestamp);
        write_varu32(buf, cert.sans.len() as u32);
        for san in &cert.sans {
            write_varu32(buf, *san);
        }
    }
}

fn decode_certificates(bytes: &[u8], pos: &mut usize) -> Result<Vec<CertEntry>, DecodeError> {
    let count = read_varu32(bytes, pos)? as usize;
    let mut certs = Vec::with_capacity(count);
    for _ in 0..count {
        let domain_id = read_varu32(bytes, pos)?;
        let issuer_id = read_varu32(bytes, pos)?;
        let subject_id = read_varu32(bytes, pos)?;
        let serial_id = read_varu32(bytes, pos)?;
        let signature_algorithm_id = read_varu32(bytes, pos)?;
        let public_key_algorithm_id = read_varu32(bytes, pos)?;
        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (tls cert version)"));
        }
        let version = bytes[*pos];
        *pos += 1;
        let not_before = read_varu32(bytes, pos)?;
        let not_after = read_varu32(bytes, pos)?;
        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (tls cert self signed)"));
        }
        let self_signed = bytes[*pos] != 0;
        *pos += 1;
        let timestamp = read_varu32(bytes, pos)?;
        let san_count = read_varu32(bytes, pos)? as usize;
        let mut sans = Vec::with_capacity(san_count);
        for _ in 0..san_count {
            sans.push(read_varu32(bytes, pos)?);
        }
        certs.push(CertEntry {
            domain_id,
            issuer_id,
            subject_id,
            serial_id,
            signature_algorithm_id,
            public_key_algorithm_id,
            version,
            not_before,
            not_after,
            timestamp,
            self_signed,
            sans,
        });
    }
    Ok(certs)
}

fn decode_scan_entry(
    bytes: &[u8],
    pos: &mut usize,
    version: u16,
) -> Result<ScanEntry, DecodeError> {
    let port = read_varu32(bytes, pos)? as u16;
    let timestamp = read_varu32(bytes, pos)?;
    let negotiated_version_id = read_optional_id(bytes, pos)?;
    let negotiated_cipher_id = read_optional_id(bytes, pos)?;
    let negotiated_cipher_code = read_optional_u16(bytes, pos)?;
    if *pos >= bytes.len() {
        return Err(DecodeError("unexpected eof (tls negotiated strength)"));
    }
    let negotiated_strength = bytes[*pos];
    *pos += 1;
    if *pos >= bytes.len() {
        return Err(DecodeError("unexpected eof (tls certificate valid)"));
    }
    let certificate_valid = bytes[*pos] != 0;
    *pos += 1;

    let versions = decode_versions(bytes, pos)?;
    let ciphers = decode_ciphers(bytes, pos)?;
    let vulnerabilities = decode_vulnerabilities(bytes, pos)?;
    let certificates = decode_certificates(bytes, pos)?;

    let (ja3_id, ja3s_id, ja3_raw_id, ja3s_raw_id, peer_fingerprint_ids, certificate_pem_ids) =
        if version >= 2 {
            let ja3_id = read_optional_id(bytes, pos)?;
            let ja3s_id = read_optional_id(bytes, pos)?;
            let ja3_raw_id = read_optional_id(bytes, pos)?;
            let ja3s_raw_id = read_optional_id(bytes, pos)?;

            let fingerprint_count = read_varu32(bytes, pos)? as usize;
            let mut peer_fingerprint_ids = Vec::with_capacity(fingerprint_count);
            for _ in 0..fingerprint_count {
                peer_fingerprint_ids.push(read_varu32(bytes, pos)?);
            }

            let pem_count = read_varu32(bytes, pos)? as usize;
            let mut certificate_pem_ids = Vec::with_capacity(pem_count);
            for _ in 0..pem_count {
                certificate_pem_ids.push(read_varu32(bytes, pos)?);
            }

            (
                ja3_id,
                ja3s_id,
                ja3_raw_id,
                ja3s_raw_id,
                peer_fingerprint_ids,
                certificate_pem_ids,
            )
        } else {
            (None, None, None, None, Vec::new(), Vec::new())
        };

    Ok(ScanEntry {
        host_id: 0, // placeholder, overwritten by caller
        port,
        timestamp,
        negotiated_version_id,
        negotiated_cipher_id,
        negotiated_cipher_code,
        negotiated_strength,
        certificate_valid,
        versions,
        ciphers,
        vulnerabilities,
        certificates,
        ja3_id,
        ja3s_id,
        ja3_raw_id,
        ja3s_raw_id,
        peer_fingerprint_ids,
        certificate_pem_ids,
    })
}

fn decode_scan_records(
    payload: &[u8],
    offset: u64,
    length: u64,
    expected_count: u32,
    host: &str,
    strings: &StringTable,
    version: u16,
) -> Result<Vec<TlsScanRecord>, DecodeError> {
    let mut cursor = offset as usize;
    let end = cursor + length as usize;
    if end > payload.len() {
        return Err(DecodeError("tls payload slice out of bounds"));
    }
    let count = read_varu32(payload, &mut cursor)? as usize;
    if count as u32 != expected_count {
        return Err(DecodeError("tls scan count mismatch"));
    }

    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let mut scan = decode_scan_entry(payload, &mut cursor, version)?;
        scan.host_id = strings
            .get_id(host)
            .ok_or(DecodeError("host not found in string table"))?;
        let segment = TlsSegment {
            strings: strings.clone(),
            scans: vec![scan],
            host_map: HashMap::new(),
        };
        records.push(segment.to_record(&segment.scans[0]));
    }

    if cursor != end {
        return Err(DecodeError("tls payload length mismatch"));
    }

    Ok(records)
}

fn write_optional_id(buf: &mut Vec<u8>, value: Option<u32>) {
    match value {
        Some(id) => {
            buf.push(1);
            write_varu32(buf, id);
        }
        None => buf.push(0),
    }
}

fn read_optional_id(bytes: &[u8], pos: &mut usize) -> Result<Option<u32>, DecodeError> {
    if *pos >= bytes.len() {
        return Err(DecodeError("unexpected eof (optional flag)"));
    }
    let flag = bytes[*pos];
    *pos += 1;
    if flag == 0 {
        Ok(None)
    } else {
        Ok(Some(read_varu32(bytes, pos)?))
    }
}

fn write_optional_u16(buf: &mut Vec<u8>, value: Option<u16>) {
    match value {
        Some(v) => {
            buf.push(1);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        None => buf.push(0),
    }
}

fn read_optional_u16(bytes: &[u8], pos: &mut usize) -> Result<Option<u16>, DecodeError> {
    if *pos >= bytes.len() {
        return Err(DecodeError("unexpected eof (optional flag)"));
    }
    let flag = bytes[*pos];
    *pos += 1;
    if flag == 0 {
        Ok(None)
    } else {
        if *pos + 2 > bytes.len() {
            return Err(DecodeError("unexpected eof (optional u16)"));
        }
        let value = u16::from_le_bytes(bytes[*pos..*pos + 2].try_into().unwrap());
        *pos += 2;
        Ok(Some(value))
    }
}

fn strength_to_u8(strength: TlsCipherStrength) -> u8 {
    match strength {
        TlsCipherStrength::Weak => 0,
        TlsCipherStrength::Medium => 1,
        TlsCipherStrength::Strong => 2,
    }
}

fn u8_to_strength(value: u8) -> TlsCipherStrength {
    match value {
        0 => TlsCipherStrength::Weak,
        1 => TlsCipherStrength::Medium,
        2 => TlsCipherStrength::Strong,
        _ => TlsCipherStrength::Medium,
    }
}

fn severity_to_u8(severity: TlsSeverity) -> u8 {
    match severity {
        TlsSeverity::Low => 0,
        TlsSeverity::Medium => 1,
        TlsSeverity::High => 2,
        TlsSeverity::Critical => 3,
    }
}

fn u8_to_severity(value: u8) -> TlsSeverity {
    match value {
        0 => TlsSeverity::Low,
        1 => TlsSeverity::Medium,
        2 => TlsSeverity::High,
        3 => TlsSeverity::Critical,
        _ => TlsSeverity::Medium,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_segment_roundtrip() {
        let mut segment = TlsSegment::new();
        segment.insert(TlsScanRecord {
            host: "example.com".into(),
            port: 443,
            timestamp: 1_700_000_000,
            negotiated_version: Some("TLS 1.2".into()),
            negotiated_cipher: Some("TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256".into()),
            negotiated_cipher_code: Some(0xC02F),
            negotiated_cipher_strength: TlsCipherStrength::Strong,
            certificate_valid: true,
            versions: vec![
                TlsVersionRecord {
                    version: "TLS 1.0".into(),
                    supported: false,
                    error: Some("disabled".into()),
                },
                TlsVersionRecord {
                    version: "TLS 1.2".into(),
                    supported: true,
                    error: None,
                },
            ],
            ciphers: vec![TlsCipherRecord {
                name: "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256".into(),
                code: 0xC02F,
                strength: TlsCipherStrength::Strong,
            }],
            vulnerabilities: vec![TlsVulnerabilityRecord {
                name: "Test Finding".into(),
                severity: TlsSeverity::Low,
                description: "Example vulnerability".into(),
            }],
            certificate_chain: vec![TlsCertRecord {
                domain: "example.com".into(),
                issuer: "Example CA".into(),
                subject: "CN=example.com".into(),
                serial_number: "01".into(),
                signature_algorithm: "sha256WithRSAEncryption".into(),
                public_key_algorithm: "rsaEncryption".into(),
                version: 3,
                not_before: 1_600_000_000,
                not_after: 1_900_000_000,
                sans: vec!["example.com".into(), "www.example.com".into()],
                self_signed: false,
                timestamp: 1_700_000_000,
            }],
            ja3: Some("d41d8cd98f00b204e9800998ecf8427e".into()),
            ja3s: Some("0f343b0931126a20f133d67c2b018a3b".into()),
            ja3_raw: Some("771,49195,0-10-11,23-24,0".into()),
            ja3s_raw: Some("771,49195,0-11,23-24,0".into()),
            peer_fingerprints: vec!["AA:BB:CC:DD".into()],
            certificate_chain_pem: vec![
                "-----BEGIN CERTIFICATE-----\nFAKE\n-----END CERTIFICATE-----".into(),
            ],
        });

        let bytes = segment.serialize();
        let decoded = TlsSegment::deserialize(&bytes).expect("decode");
        let mut records = decoded.scans_for_host("example.com");
        assert_eq!(records.len(), 1);
        let record = records.remove(0);
        assert_eq!(record.port, 443);
        assert_eq!(
            record.negotiated_cipher.unwrap(),
            "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256"
        );
        assert_eq!(record.certificate_chain.len(), 1);
    }

    #[test]
    fn tls_segment_view_reads_host() {
        let mut segment = TlsSegment::new();
        segment.insert(TlsScanRecord {
            host: "example.com".into(),
            port: 443,
            timestamp: 1,
            negotiated_version: None,
            negotiated_cipher: None,
            negotiated_cipher_code: None,
            negotiated_cipher_strength: TlsCipherStrength::Medium,
            certificate_valid: false,
            versions: Vec::new(),
            ciphers: Vec::new(),
            vulnerabilities: Vec::new(),
            certificate_chain: Vec::new(),
            ja3: None,
            ja3s: None,
            ja3_raw: None,
            ja3s_raw: None,
            peer_fingerprints: Vec::new(),
            certificate_chain_pem: Vec::new(),
        });
        segment.insert(TlsScanRecord {
            host: "api.example.com".into(),
            port: 8443,
            timestamp: 2,
            negotiated_version: None,
            negotiated_cipher: None,
            negotiated_cipher_code: None,
            negotiated_cipher_strength: TlsCipherStrength::Weak,
            certificate_valid: true,
            versions: Vec::new(),
            ciphers: Vec::new(),
            vulnerabilities: Vec::new(),
            certificate_chain: Vec::new(),
            ja3: None,
            ja3s: None,
            ja3_raw: None,
            ja3s_raw: None,
            peer_fingerprints: Vec::new(),
            certificate_chain_pem: Vec::new(),
        });

        let bytes = segment.serialize();
        let data = Arc::new(bytes);
        let view = TlsSegmentView::from_arc(Arc::clone(&data), 0, data.len()).expect("view loaded");

        let records = view.records_for_host("example.com").expect("records");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].port, 443);
    }
}
