//! Loot Segment - Persistent storage for pentest findings, credentials, and artifacts.
//!
//! This segment implements the pentest intelligence storage inspired by PentestAgent,
//! but using RedDB's segment pattern for persistence and zero-copy reads.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use super::actions::{ActionOutcome, ActionRecord, ActionType, RecordPayload};
use crate::storage::primitives::encoding::{
    read_ip, read_string, read_varu32, write_ip, write_string, write_varu32, DecodeError,
};

// ==================== Loot Entry Core Types ====================

/// Category of loot entry
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LootCategory {
    /// Authentication credentials (username/password, API keys, tokens)
    Credential = 0,
    /// Security vulnerabilities (CVE, misconfigurations)
    Vulnerability = 1,
    /// General security findings
    Finding = 2,
    /// Collected artifacts (files, screenshots, logs)
    Artifact = 3,
    /// Discovered services (HTTP, SSH, SMB, etc.)
    Service = 4,
    /// Web endpoints (URLs, API routes)
    Endpoint = 5,
    /// Technology stack (frameworks, versions)
    Technology = 6,
    /// Pentest tasks and objectives
    Task = 7,
    /// Informational notes
    Info = 8,
}

impl LootCategory {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Credential),
            1 => Some(Self::Vulnerability),
            2 => Some(Self::Finding),
            3 => Some(Self::Artifact),
            4 => Some(Self::Service),
            5 => Some(Self::Endpoint),
            6 => Some(Self::Technology),
            7 => Some(Self::Task),
            8 => Some(Self::Info),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Credential => "credential",
            Self::Vulnerability => "vulnerability",
            Self::Finding => "finding",
            Self::Artifact => "artifact",
            Self::Service => "service",
            Self::Endpoint => "endpoint",
            Self::Technology => "technology",
            Self::Task => "task",
            Self::Info => "info",
        }
    }
}

/// Confidence level for findings
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Low = 0,
    Medium = 1,
    High = 2,
}

impl Confidence {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Low),
            1 => Some(Self::Medium),
            2 => Some(Self::High),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Status of loot entry
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LootStatus {
    /// Initial discovery, needs verification
    Open = 0,
    /// Manually closed/resolved
    Closed = 1,
    /// Verified as valid
    Confirmed = 2,
    /// Potential finding, needs more evidence
    Potential = 3,
    /// False positive, filtered out
    Filtered = 4,
}

impl LootStatus {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Open),
            1 => Some(Self::Closed),
            2 => Some(Self::Confirmed),
            3 => Some(Self::Potential),
            4 => Some(Self::Filtered),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Confirmed => "confirmed",
            Self::Potential => "potential",
            Self::Filtered => "filtered",
        }
    }
}

// ==================== Loot Metadata ====================

/// Service information embedded in loot
#[derive(Debug, Clone, Default)]
pub struct ServiceInfo {
    pub name: String,
    pub port: u16,
    pub protocol: String,
    pub version: Option<String>,
    pub banner: Option<String>,
}

impl ServiceInfo {
    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        write_string(&mut buf, &self.name);
        buf.extend_from_slice(&self.port.to_le_bytes());
        write_string(&mut buf, &self.protocol);
        write_optional_string(&mut buf, &self.version);
        write_optional_string(&mut buf, &self.banner);
        buf
    }

    fn from_bytes(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let name = read_string(bytes, pos)?.to_string();
        if *pos + 2 > bytes.len() {
            return Err(DecodeError("truncated service port"));
        }
        let port = u16::from_le_bytes([bytes[*pos], bytes[*pos + 1]]);
        *pos += 2;
        let protocol = read_string(bytes, pos)?.to_string();
        let version = read_optional_string(bytes, pos)?;
        let banner = read_optional_string(bytes, pos)?;
        Ok(Self {
            name,
            port,
            protocol,
            version,
            banner,
        })
    }
}

/// Endpoint information embedded in loot
#[derive(Debug, Clone, Default)]
pub struct EndpointInfo {
    pub path: String,
    pub method: String,
    pub status_code: Option<u16>,
    pub content_type: Option<String>,
}

impl EndpointInfo {
    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        write_string(&mut buf, &self.path);
        write_string(&mut buf, &self.method);
        match self.status_code {
            Some(code) => {
                buf.push(1);
                buf.extend_from_slice(&code.to_le_bytes());
            }
            None => buf.push(0),
        }
        write_optional_string(&mut buf, &self.content_type);
        buf
    }

    fn from_bytes(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let path = read_string(bytes, pos)?.to_string();
        let method = read_string(bytes, pos)?.to_string();
        if *pos >= bytes.len() {
            return Err(DecodeError("truncated endpoint status"));
        }
        let has_status = bytes[*pos];
        *pos += 1;
        let status_code = if has_status == 1 {
            if *pos + 2 > bytes.len() {
                return Err(DecodeError("truncated status code"));
            }
            let code = u16::from_le_bytes([bytes[*pos], bytes[*pos + 1]]);
            *pos += 2;
            Some(code)
        } else {
            None
        };
        let content_type = read_optional_string(bytes, pos)?;
        Ok(Self {
            path,
            method,
            status_code,
            content_type,
        })
    }
}

/// Technology stack information
#[derive(Debug, Clone, Default)]
pub struct TechInfo {
    pub name: String,
    pub version: Option<String>,
    pub category: String,
}

impl TechInfo {
    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        write_string(&mut buf, &self.name);
        write_optional_string(&mut buf, &self.version);
        write_string(&mut buf, &self.category);
        buf
    }

    fn from_bytes(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let name = read_string(bytes, pos)?.to_string();
        let version = read_optional_string(bytes, pos)?;
        let category = read_string(bytes, pos)?.to_string();
        Ok(Self {
            name,
            version,
            category,
        })
    }
}

/// Weakness information (CWE-based)
#[derive(Debug, Clone, Default)]
pub struct Weakness {
    pub cwe_id: Option<u32>,
    pub name: String,
    pub description: String,
}

impl Weakness {
    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self.cwe_id {
            Some(id) => {
                buf.push(1);
                buf.extend_from_slice(&id.to_le_bytes());
            }
            None => buf.push(0),
        }
        write_string(&mut buf, &self.name);
        write_string(&mut buf, &self.description);
        buf
    }

    fn from_bytes(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        if *pos >= bytes.len() {
            return Err(DecodeError("truncated weakness"));
        }
        let has_cwe = bytes[*pos];
        *pos += 1;
        let cwe_id = if has_cwe == 1 {
            if *pos + 4 > bytes.len() {
                return Err(DecodeError("truncated cwe id"));
            }
            let id = u32::from_le_bytes([
                bytes[*pos],
                bytes[*pos + 1],
                bytes[*pos + 2],
                bytes[*pos + 3],
            ]);
            *pos += 4;
            Some(id)
        } else {
            None
        };
        let name = read_string(bytes, pos)?.to_string();
        let description = read_string(bytes, pos)?.to_string();
        Ok(Self {
            cwe_id,
            name,
            description,
        })
    }
}

/// Category-specific metadata for loot entries
#[derive(Debug, Clone, Default)]
pub struct LootMetadata {
    // Credential fields
    pub username: Option<String>,
    pub password: Option<String>,
    pub protocol: Option<String>,
    pub hash_type: Option<String>,

    // Vulnerability fields
    pub cve: Option<String>,
    pub cvss: Option<f32>,
    pub weaknesses: Vec<Weakness>,

    // Service fields
    pub services: Vec<ServiceInfo>,
    pub endpoints: Vec<EndpointInfo>,
    pub technologies: Vec<TechInfo>,

    // Evidence
    pub url: Option<String>,
    pub evidence_path: Option<String>,

    // Tags for flexible categorization
    pub tags: Vec<String>,
}

impl LootMetadata {
    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Credential fields
        write_optional_string(&mut buf, &self.username);
        write_optional_string(&mut buf, &self.password);
        write_optional_string(&mut buf, &self.protocol);
        write_optional_string(&mut buf, &self.hash_type);

        // Vulnerability fields
        write_optional_string(&mut buf, &self.cve);
        match self.cvss {
            Some(score) => {
                buf.push(1);
                buf.extend_from_slice(&score.to_bits().to_le_bytes());
            }
            None => buf.push(0),
        }

        // Weaknesses
        write_varu32(&mut buf, self.weaknesses.len() as u32);
        for w in &self.weaknesses {
            let w_bytes = w.to_bytes();
            write_varu32(&mut buf, w_bytes.len() as u32);
            buf.extend_from_slice(&w_bytes);
        }

        // Services
        write_varu32(&mut buf, self.services.len() as u32);
        for s in &self.services {
            let s_bytes = s.to_bytes();
            write_varu32(&mut buf, s_bytes.len() as u32);
            buf.extend_from_slice(&s_bytes);
        }

        // Endpoints
        write_varu32(&mut buf, self.endpoints.len() as u32);
        for e in &self.endpoints {
            let e_bytes = e.to_bytes();
            write_varu32(&mut buf, e_bytes.len() as u32);
            buf.extend_from_slice(&e_bytes);
        }

        // Technologies
        write_varu32(&mut buf, self.technologies.len() as u32);
        for t in &self.technologies {
            let t_bytes = t.to_bytes();
            write_varu32(&mut buf, t_bytes.len() as u32);
            buf.extend_from_slice(&t_bytes);
        }

        // Evidence
        write_optional_string(&mut buf, &self.url);
        write_optional_string(&mut buf, &self.evidence_path);

        // Tags
        write_varu32(&mut buf, self.tags.len() as u32);
        for tag in &self.tags {
            write_string(&mut buf, tag);
        }

        buf
    }

    fn from_bytes(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        // Credential fields
        let username = read_optional_string(bytes, pos)?;
        let password = read_optional_string(bytes, pos)?;
        let protocol = read_optional_string(bytes, pos)?;
        let hash_type = read_optional_string(bytes, pos)?;

        // Vulnerability fields
        let cve = read_optional_string(bytes, pos)?;
        if *pos >= bytes.len() {
            return Err(DecodeError("truncated cvss flag"));
        }
        let has_cvss = bytes[*pos];
        *pos += 1;
        let cvss = if has_cvss == 1 {
            if *pos + 4 > bytes.len() {
                return Err(DecodeError("truncated cvss value"));
            }
            let bits = u32::from_le_bytes([
                bytes[*pos],
                bytes[*pos + 1],
                bytes[*pos + 2],
                bytes[*pos + 3],
            ]);
            *pos += 4;
            Some(f32::from_bits(bits))
        } else {
            None
        };

        // Weaknesses
        let weakness_count = read_varu32(bytes, pos)? as usize;
        let mut weaknesses = Vec::with_capacity(weakness_count);
        for _ in 0..weakness_count {
            let w_len = read_varu32(bytes, pos)? as usize;
            let end = *pos + w_len;
            if end > bytes.len() {
                return Err(DecodeError("truncated weakness"));
            }
            let w = Weakness::from_bytes(bytes, pos)?;
            weaknesses.push(w);
        }

        // Services
        let service_count = read_varu32(bytes, pos)? as usize;
        let mut services = Vec::with_capacity(service_count);
        for _ in 0..service_count {
            let s_len = read_varu32(bytes, pos)? as usize;
            let end = *pos + s_len;
            if end > bytes.len() {
                return Err(DecodeError("truncated service"));
            }
            let s = ServiceInfo::from_bytes(bytes, pos)?;
            services.push(s);
        }

        // Endpoints
        let endpoint_count = read_varu32(bytes, pos)? as usize;
        let mut endpoints = Vec::with_capacity(endpoint_count);
        for _ in 0..endpoint_count {
            let e_len = read_varu32(bytes, pos)? as usize;
            let end = *pos + e_len;
            if end > bytes.len() {
                return Err(DecodeError("truncated endpoint"));
            }
            let e = EndpointInfo::from_bytes(bytes, pos)?;
            endpoints.push(e);
        }

        // Technologies
        let tech_count = read_varu32(bytes, pos)? as usize;
        let mut technologies = Vec::with_capacity(tech_count);
        for _ in 0..tech_count {
            let t_len = read_varu32(bytes, pos)? as usize;
            let end = *pos + t_len;
            if end > bytes.len() {
                return Err(DecodeError("truncated technology"));
            }
            let t = TechInfo::from_bytes(bytes, pos)?;
            technologies.push(t);
        }

        // Evidence
        let url = read_optional_string(bytes, pos)?;
        let evidence_path = read_optional_string(bytes, pos)?;

        // Tags
        let tag_count = read_varu32(bytes, pos)? as usize;
        let mut tags = Vec::with_capacity(tag_count);
        for _ in 0..tag_count {
            tags.push(read_string(bytes, pos)?.to_string());
        }

        Ok(Self {
            username,
            password,
            protocol,
            hash_type,
            cve,
            cvss,
            weaknesses,
            services,
            endpoints,
            technologies,
            url,
            evidence_path,
            tags,
        })
    }
}

// ==================== Loot Entry ====================

/// A single loot entry representing a finding, credential, vulnerability, or artifact.
#[derive(Debug, Clone)]
pub struct LootEntry {
    /// Human-readable key (e.g., "ssh_creds_192.168.1.1")
    pub key: String,
    /// Category of loot
    pub category: LootCategory,
    /// Human-readable description/content
    pub content: String,
    /// Confidence level
    pub confidence: Confidence,
    /// Current status
    pub status: LootStatus,
    /// Target host/IP (if applicable)
    pub target: Option<IpAddr>,
    /// Source host/IP where finding originated
    pub source: Option<IpAddr>,
    /// Unix timestamp of creation
    pub created_at: i64,
    /// Unix timestamp of last update
    pub updated_at: i64,
    /// Category-specific metadata
    pub metadata: LootMetadata,
}


mod entry_impl;
// ==================== Helper Functions ====================

fn write_optional_string(buf: &mut Vec<u8>, value: &Option<String>) {
    match value {
        Some(s) => {
            buf.push(1);
            write_string(buf, s);
        }
        None => buf.push(0),
    }
}

fn read_optional_string(bytes: &[u8], pos: &mut usize) -> Result<Option<String>, DecodeError> {
    if *pos >= bytes.len() {
        return Err(DecodeError("truncated optional string flag"));
    }
    let has_value = bytes[*pos];
    *pos += 1;
    if has_value == 1 {
        Ok(Some(read_string(bytes, pos)?.to_string()))
    } else {
        Ok(None)
    }
}

// ==================== Loot Segment Directory ====================

#[derive(Debug, Clone)]
struct LootDirEntry {
    key_hash: u64,
    payload_offset: u64,
    payload_len: u64,
}

impl LootDirEntry {
    const SIZE: usize = 8 + 8 + 8; // key_hash + offset + len

    fn write_all(entries: &[Self], buf: &mut Vec<u8>) {
        for entry in entries {
            buf.extend_from_slice(&entry.key_hash.to_le_bytes());
            buf.extend_from_slice(&entry.payload_offset.to_le_bytes());
            buf.extend_from_slice(&entry.payload_len.to_le_bytes());
        }
    }

    fn read_all(bytes: &[u8], count: usize) -> Result<Vec<Self>, DecodeError> {
        if bytes.len() != count * Self::SIZE {
            return Err(DecodeError("invalid loot directory size"));
        }
        let mut entries = Vec::with_capacity(count);
        let mut offset = 0usize;
        for _ in 0..count {
            let key_hash = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let payload_offset = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let payload_len = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            entries.push(Self {
                key_hash,
                payload_offset,
                payload_len,
            });
        }
        Ok(entries)
    }
}

// ==================== Loot Segment Header ====================

#[derive(Debug, Clone, Copy)]
struct LootSegmentHeader {
    record_count: u32,
    directory_len: u64,
    payload_len: u64,
}

impl LootSegmentHeader {
    const MAGIC: [u8; 4] = *b"LT01"; // Loot segment magic
    const VERSION: u16 = 1;
    const SIZE: usize = 4 + 2 + 2 + 4 + 8 + 8; // magic + version + reserved + count + dir_len + payload_len

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&Self::MAGIC);
        buf.extend_from_slice(&Self::VERSION.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // reserved
        buf.extend_from_slice(&self.record_count.to_le_bytes());
        buf.extend_from_slice(&self.directory_len.to_le_bytes());
        buf.extend_from_slice(&self.payload_len.to_le_bytes());
    }

    fn read(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < Self::SIZE {
            return Err(DecodeError("loot header too small"));
        }
        if bytes[0..4] != Self::MAGIC {
            return Err(DecodeError("invalid loot segment magic"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != Self::VERSION {
            return Err(DecodeError("unsupported loot segment version"));
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

// ==================== Loot Segment ====================

/// Simple hash function for string keys (FNV-1a)
fn hash_key(key: &str) -> u64 {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;

    let mut hash = FNV_OFFSET;
    for byte in key.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Mutable loot segment for building/modifying loot entries
#[derive(Debug, Default, Clone)]
pub struct LootSegment {
    records: Vec<LootEntry>,
    index: HashMap<String, usize>,
    sorted: bool,
}


mod segment_impl;
// ==================== Loot Segment View (Zero-Copy) ====================

/// Immutable view for zero-copy reads from serialized data
pub struct LootSegmentView {
    directory: Vec<LootDirEntry>,
    keys: Vec<String>, // Cached keys for lookup
    data: Arc<Vec<u8>>,
    payload_offset: usize,
    payload_len: usize,
}

impl LootSegmentView {
    pub fn from_arc(
        data: Arc<Vec<u8>>,
        segment_offset: usize,
        segment_len: usize,
    ) -> Result<Self, DecodeError> {
        if segment_offset + segment_len > data.len() {
            return Err(DecodeError("loot segment out of bounds"));
        }
        let bytes = &data[segment_offset..segment_offset + segment_len];
        if bytes.len() < LootSegmentHeader::SIZE {
            return Err(DecodeError("loot segment too small"));
        }
        let header = LootSegmentHeader::read(bytes)?;

        let mut offset = LootSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("loot directory overflow"))?;
        if segment_offset + dir_end > data.len() {
            return Err(DecodeError("loot directory out of bounds"));
        }
        let directory_bytes = &data[segment_offset + offset..segment_offset + dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("loot payload overflow"))?;
        if segment_offset + payload_end > data.len() {
            return Err(DecodeError("loot payload out of bounds"));
        }
        let payload_offset = segment_offset + offset;

        let directory = LootDirEntry::read_all(directory_bytes, header.record_count as usize)?;

        // Pre-cache keys for lookup (we need to read each record to get the key)
        let payload = &data[payload_offset..payload_offset + header.payload_len as usize];
        let mut keys = Vec::with_capacity(header.record_count as usize);
        for entry in &directory {
            let mut cursor = entry.payload_offset as usize;
            let end = cursor + entry.payload_len as usize;
            if end > payload.len() {
                return Err(DecodeError("loot payload slice out of bounds"));
            }
            let len = read_varu32(payload, &mut cursor)? as usize;
            if cursor + len > end {
                return Err(DecodeError("loot record length mismatch"));
            }
            // Just read the key (first field)
            let key = read_string(&payload[cursor..cursor + len], &mut 0)?.to_string();
            keys.push(key);
        }

        Ok(Self {
            directory,
            keys,
            data,
            payload_offset,
            payload_len: header.payload_len as usize,
        })
    }

    /// Get a loot entry by key
    pub fn get(&self, key: &str) -> Result<Option<LootEntry>, DecodeError> {
        let target_hash = hash_key(key);
        for (idx, entry) in self.directory.iter().enumerate() {
            if entry.key_hash == target_hash && self.keys[idx] == key {
                let payload =
                    &self.data[self.payload_offset..self.payload_offset + self.payload_len];
                let mut cursor = entry.payload_offset as usize;
                let end = cursor + entry.payload_len as usize;
                if end > payload.len() {
                    return Err(DecodeError("loot payload slice out of bounds"));
                }
                let len = read_varu32(payload, &mut cursor)? as usize;
                if cursor + len > end {
                    return Err(DecodeError("loot record length mismatch"));
                }
                let record = LootEntry::from_bytes(&payload[cursor..cursor + len])?;
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    /// Get all loot entries
    pub fn all(&self) -> Result<Vec<LootEntry>, DecodeError> {
        let payload = &self.data[self.payload_offset..self.payload_offset + self.payload_len];
        let mut records = Vec::with_capacity(self.directory.len());
        for entry in &self.directory {
            let mut cursor = entry.payload_offset as usize;
            let end = cursor + entry.payload_len as usize;
            if end > payload.len() {
                return Err(DecodeError("loot payload slice out of bounds"));
            }
            let len = read_varu32(payload, &mut cursor)? as usize;
            if cursor + len > end {
                return Err(DecodeError("loot record length mismatch"));
            }
            let record = LootEntry::from_bytes(&payload[cursor..cursor + len])?;
            records.push(record);
        }
        Ok(records)
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn test_loot_entry_roundtrip() {
        let mut entry = LootEntry::new(
            "test_cred_192.168.1.1",
            LootCategory::Credential,
            "SSH credentials found",
        );
        entry.target = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)));
        entry.confidence = Confidence::High;
        entry.status = LootStatus::Confirmed;
        entry.metadata.username = Some("admin".to_string());
        entry.metadata.password = Some("secret123".to_string());
        entry.metadata.protocol = Some("ssh".to_string());

        let bytes = entry.to_bytes();
        let decoded = LootEntry::from_bytes(&bytes).expect("decode");

        assert_eq!(decoded.key, "test_cred_192.168.1.1");
        assert_eq!(decoded.category, LootCategory::Credential);
        assert_eq!(decoded.content, "SSH credentials found");
        assert_eq!(decoded.confidence, Confidence::High);
        assert_eq!(decoded.status, LootStatus::Confirmed);
        assert_eq!(
            decoded.target,
            Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)))
        );
        assert_eq!(decoded.metadata.username, Some("admin".to_string()));
        assert_eq!(decoded.metadata.password, Some("secret123".to_string()));
    }

    #[test]
    fn test_loot_segment_roundtrip() {
        let mut segment = LootSegment::new();

        let mut cred = LootEntry::new("ssh_root", LootCategory::Credential, "Root SSH access");
        cred.target = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
        cred.metadata.username = Some("root".to_string());
        segment.insert(cred);

        let mut vuln = LootEntry::new("cve_2021_44228", LootCategory::Vulnerability, "Log4Shell");
        vuln.target = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)));
        vuln.metadata.cve = Some("CVE-2021-44228".to_string());
        vuln.metadata.cvss = Some(10.0);
        segment.insert(vuln);

        let bytes = segment.serialize();
        let mut decoded = LootSegment::deserialize(&bytes).expect("decode");

        assert_eq!(decoded.len(), 2);
        let cred_decoded = decoded.get("ssh_root").expect("cred");
        assert_eq!(cred_decoded.metadata.username, Some("root".to_string()));

        let vuln_decoded = decoded.get("cve_2021_44228").expect("vuln");
        assert_eq!(
            vuln_decoded.metadata.cve,
            Some("CVE-2021-44228".to_string())
        );
        assert_eq!(vuln_decoded.metadata.cvss, Some(10.0));
    }

    #[test]
    fn test_loot_segment_view() {
        let mut segment = LootSegment::new();
        segment.insert(LootEntry::new(
            "finding_1",
            LootCategory::Finding,
            "Test finding",
        ));

        let bytes = segment.serialize();
        let data = Arc::new(bytes);
        let view = LootSegmentView::from_arc(Arc::clone(&data), 0, data.len()).expect("view");

        let entry = view.get("finding_1").expect("result").expect("entry");
        assert_eq!(entry.key, "finding_1");
        assert_eq!(entry.category, LootCategory::Finding);
    }

    #[test]
    fn test_loot_by_category() {
        let mut segment = LootSegment::new();
        segment.insert(LootEntry::new("cred1", LootCategory::Credential, "cred1"));
        segment.insert(LootEntry::new("cred2", LootCategory::Credential, "cred2"));
        segment.insert(LootEntry::new(
            "vuln1",
            LootCategory::Vulnerability,
            "vuln1",
        ));

        let creds = segment.by_category(LootCategory::Credential);
        assert_eq!(creds.len(), 2);

        let vulns = segment.by_category(LootCategory::Vulnerability);
        assert_eq!(vulns.len(), 1);
    }

    #[test]
    fn test_loot_delete() {
        let mut segment = LootSegment::new();
        segment.insert(LootEntry::new("key1", LootCategory::Info, "info1"));
        segment.insert(LootEntry::new("key2", LootCategory::Info, "info2"));

        assert_eq!(segment.len(), 2);
        let deleted = segment.delete("key1");
        assert!(deleted.is_some());
        assert_eq!(segment.len(), 1);
        assert!(segment.get("key1").is_none());
        assert!(segment.get("key2").is_some());
    }

    #[test]
    fn test_service_info_roundtrip() {
        let service = ServiceInfo {
            name: "ssh".to_string(),
            port: 22,
            protocol: "tcp".to_string(),
            version: Some("OpenSSH_8.2p1".to_string()),
            banner: Some("SSH-2.0-OpenSSH_8.2p1".to_string()),
        };

        let bytes = service.to_bytes();
        let mut pos = 0;
        let decoded = ServiceInfo::from_bytes(&bytes, &mut pos).expect("decode");

        assert_eq!(decoded.name, "ssh");
        assert_eq!(decoded.port, 22);
        assert_eq!(decoded.protocol, "tcp");
        assert_eq!(decoded.version, Some("OpenSSH_8.2p1".to_string()));
        assert_eq!(decoded.banner, Some("SSH-2.0-OpenSSH_8.2p1".to_string()));
    }

    #[test]
    fn test_metadata_with_services() {
        let mut entry = LootEntry::new("host_services", LootCategory::Service, "Host services");
        entry.metadata.services.push(ServiceInfo {
            name: "http".to_string(),
            port: 80,
            protocol: "tcp".to_string(),
            version: Some("nginx/1.18.0".to_string()),
            banner: None,
        });
        entry.metadata.services.push(ServiceInfo {
            name: "ssh".to_string(),
            port: 22,
            protocol: "tcp".to_string(),
            version: None,
            banner: None,
        });

        let bytes = entry.to_bytes();
        let decoded = LootEntry::from_bytes(&bytes).expect("decode");

        assert_eq!(decoded.metadata.services.len(), 2);
        assert_eq!(decoded.metadata.services[0].name, "http");
        assert_eq!(decoded.metadata.services[1].port, 22);
    }
}
