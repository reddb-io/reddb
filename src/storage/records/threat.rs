//! Threat intelligence record types

use crate::storage::primitives::encoding::{read_varu32, write_varu32, DecodeError};

use super::helpers::{read_optional_string, read_string, write_optional_string, write_string};

#[derive(Debug, Clone)]
pub struct MitreAttackRecord {
    pub technique_id: String,   // e.g., "T1059.001"
    pub technique_name: String, // e.g., "PowerShell"
    pub tactic: String,         // e.g., "Execution"
    pub target: String,         // e.g., "example.com"
    pub source_finding: String, // e.g., "port_scan:5985"
    pub cve_id: Option<String>, // e.g., "CVE-2021-44228"
    pub confidence: u8,         // 0-100
    pub score: u8,              // 0-100 (for Navigator)
    pub detected_at: u32,       // Unix timestamp
    pub evidence: String,       // Detail string
}

impl MitreAttackRecord {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        write_string(&mut buf, &self.technique_id);
        write_string(&mut buf, &self.technique_name);
        write_string(&mut buf, &self.tactic);
        write_string(&mut buf, &self.target);
        write_string(&mut buf, &self.source_finding);
        write_optional_string(&mut buf, &self.cve_id);
        buf.push(self.confidence);
        buf.push(self.score);
        buf.extend_from_slice(&self.detected_at.to_le_bytes());
        write_string(&mut buf, &self.evidence);
        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut pos = 0;
        let technique_id = read_string(bytes, &mut pos)?;
        let technique_name = read_string(bytes, &mut pos)?;
        let tactic = read_string(bytes, &mut pos)?;
        let target = read_string(bytes, &mut pos)?;
        let source_finding = read_string(bytes, &mut pos)?;
        let cve_id = read_optional_string(bytes, &mut pos)?;

        if bytes.len() < pos + 2 {
            return Err(DecodeError("truncated confidence/score"));
        }
        let confidence = bytes[pos];
        pos += 1;
        let score = bytes[pos];
        pos += 1;

        if bytes.len() < pos + 4 {
            return Err(DecodeError("truncated timestamp"));
        }
        let detected_at =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        pos += 4;

        let evidence = read_string(bytes, &mut pos)?;

        Ok(Self {
            technique_id,
            technique_name,
            tactic,
            target,
            source_finding,
            cve_id,
            confidence,
            score,
            detected_at,
            evidence,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IocType {
    IPv4 = 0,
    IPv6 = 1,
    Domain = 2,
    URL = 3,
    Email = 4,
    HashMD5 = 5,
    HashSHA1 = 6,
    HashSHA256 = 7,
    Certificate = 8,
    JA3 = 9,
}

#[derive(Debug, Clone)]
pub struct IocRecord {
    pub ioc_type: IocType,
    pub value: String,                 // e.g., "192.168.1.1"
    pub target: String,                // e.g., "example.com"
    pub confidence: u8,                // 0-100
    pub source: String,                // e.g., "port_scan", "dns_lookup"
    pub mitre_techniques: Vec<String>, // List of T-codes
    pub tags: Vec<String>,             // e.g., ["phishing", "apt29"]
    pub first_seen: u32,
    pub last_seen: u32,
    pub stix_id: Option<String>,
}

impl IocRecord {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(self.ioc_type as u8);
        write_string(&mut buf, &self.value);
        write_string(&mut buf, &self.target);
        buf.push(self.confidence);
        write_string(&mut buf, &self.source);

        write_varu32(&mut buf, self.mitre_techniques.len() as u32);
        for tech in &self.mitre_techniques {
            write_string(&mut buf, tech);
        }

        write_varu32(&mut buf, self.tags.len() as u32);
        for tag in &self.tags {
            write_string(&mut buf, tag);
        }

        buf.extend_from_slice(&self.first_seen.to_le_bytes());
        buf.extend_from_slice(&self.last_seen.to_le_bytes());
        write_optional_string(&mut buf, &self.stix_id);
        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.is_empty() {
            return Err(DecodeError("empty ioc record"));
        }
        let mut pos = 0;
        let ioc_type = match bytes[pos] {
            0 => IocType::IPv4,
            1 => IocType::IPv6,
            2 => IocType::Domain,
            3 => IocType::URL,
            4 => IocType::Email,
            5 => IocType::HashMD5,
            6 => IocType::HashSHA1,
            7 => IocType::HashSHA256,
            8 => IocType::Certificate,
            9 => IocType::JA3,
            _ => IocType::Domain,
        };
        pos += 1;

        let value = read_string(bytes, &mut pos)?;
        let target = read_string(bytes, &mut pos)?;

        if pos >= bytes.len() {
            return Err(DecodeError("truncated confidence"));
        }
        let confidence = bytes[pos];
        pos += 1;

        let source = read_string(bytes, &mut pos)?;

        let tech_count = read_varu32(bytes, &mut pos)? as usize;
        let mut mitre_techniques = Vec::with_capacity(tech_count);
        for _ in 0..tech_count {
            mitre_techniques.push(read_string(bytes, &mut pos)?);
        }

        let tag_count = read_varu32(bytes, &mut pos)? as usize;
        let mut tags = Vec::with_capacity(tag_count);
        for _ in 0..tag_count {
            tags.push(read_string(bytes, &mut pos)?);
        }

        if bytes.len() < pos + 8 {
            return Err(DecodeError("truncated timestamps"));
        }
        let first_seen =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        pos += 4;
        let last_seen =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        pos += 4;

        let stix_id = read_optional_string(bytes, &mut pos)?;

        Ok(Self {
            ioc_type,
            value,
            target,
            confidence,
            source,
            mitre_techniques,
            tags,
            first_seen,
            last_seen,
            stix_id,
        })
    }
}
