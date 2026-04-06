//! Host intelligence and fingerprint record types

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::storage::primitives::encoding::{read_varu32, write_varu32, DecodeError};

use super::helpers::{read_optional_string, read_string, write_optional_string, write_string};

/// Service-level fingerprint information captured during host analysis.
#[derive(Debug, Clone)]
pub struct ServiceIntelRecord {
    pub port: u16,
    pub service_name: Option<String>,
    pub banner: Option<String>,
    pub os_hints: Vec<String>,
}

impl ServiceIntelRecord {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.port.to_le_bytes());
        write_optional_string(&mut buf, &self.service_name);
        write_optional_string(&mut buf, &self.banner);
        write_varu32(&mut buf, self.os_hints.len() as u32);
        for hint in &self.os_hints {
            write_string(&mut buf, hint);
        }
        buf
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < 2 {
            return Err(DecodeError("service record too small"));
        }
        let port = u16::from_le_bytes([bytes[0], bytes[1]]);
        let mut pos = 2usize;
        let service_name = read_optional_string(bytes, &mut pos)?;
        let banner = read_optional_string(bytes, &mut pos)?;

        let hint_count = read_varu32(bytes, &mut pos)? as usize;
        let mut os_hints = Vec::with_capacity(hint_count);
        for _ in 0..hint_count {
            let value = read_string(bytes, &mut pos)?;
            os_hints.push(value);
        }

        Ok(Self {
            port,
            service_name,
            banner,
            os_hints,
        })
    }
}

/// Aggregated host fingerprint/intelligence record.
#[derive(Debug, Clone)]
pub struct HostIntelRecord {
    pub ip: IpAddr,
    pub os_family: Option<String>,
    pub confidence: f32,
    pub last_seen: u32,
    pub services: Vec<ServiceIntelRecord>,
}

impl HostIntelRecord {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self.ip {
            IpAddr::V4(ip) => {
                buf.push(4);
                buf.extend_from_slice(&ip.octets());
            }
            IpAddr::V6(ip) => {
                buf.push(6);
                buf.extend_from_slice(&ip.octets());
            }
        }

        buf.extend_from_slice(&self.last_seen.to_le_bytes());
        buf.extend_from_slice(&self.confidence.to_bits().to_le_bytes());
        write_optional_string(&mut buf, &self.os_family);

        write_varu32(&mut buf, self.services.len() as u32);
        for service in &self.services {
            let svc_bytes = service.to_bytes();
            write_varu32(&mut buf, svc_bytes.len() as u32);
            buf.extend_from_slice(&svc_bytes);
        }

        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.is_empty() {
            return Err(DecodeError("empty host record"));
        }

        let ip_version = bytes[0];
        let mut pos = 1usize;
        let ip = match ip_version {
            4 => {
                if bytes.len() < pos + 4 {
                    return Err(DecodeError("truncated IPv4 address"));
                }
                let octets = [bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]];
                pos += 4;
                IpAddr::V4(Ipv4Addr::from(octets))
            }
            6 => {
                if bytes.len() < pos + 16 {
                    return Err(DecodeError("truncated IPv6 address"));
                }
                let mut octets = [0u8; 16];
                octets.copy_from_slice(&bytes[pos..pos + 16]);
                pos += 16;
                IpAddr::V6(Ipv6Addr::from(octets))
            }
            _ => return Err(DecodeError("invalid IP version")),
        };

        if bytes.len() < pos + 8 {
            return Err(DecodeError("truncated host record metadata"));
        }

        let last_seen =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        pos += 4;

        let confidence_bits =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        pos += 4;
        let confidence = f32::from_bits(confidence_bits);

        let os_family = read_optional_string(bytes, &mut pos)?;

        let service_count = read_varu32(bytes, &mut pos)? as usize;
        let mut services = Vec::with_capacity(service_count);
        for _ in 0..service_count {
            let svc_len = read_varu32(bytes, &mut pos)? as usize;
            if bytes.len() < pos + svc_len {
                return Err(DecodeError("truncated service entry"));
            }
            let record = ServiceIntelRecord::from_slice(&bytes[pos..pos + svc_len])?;
            pos += svc_len;
            services.push(record);
        }

        Ok(Self {
            ip,
            os_family,
            confidence,
            last_seen,
            services,
        })
    }
}

/// Fingerprint record from service detection
#[derive(Debug, Clone)]
pub struct FingerprintRecord {
    pub host: String,
    pub port: u16,
    pub technology: String,      // e.g., "nginx"
    pub version: Option<String>, // e.g., "1.21.0"
    pub cpe: Option<String>,     // e.g., "cpe:2.3:a:nginx:nginx:1.21.0"
    pub confidence: u8,          // 0-100
    pub source: String,          // banner/header/probe
    pub detected_at: u32,
}

impl FingerprintRecord {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        write_string(&mut buf, &self.host);
        buf.extend_from_slice(&self.port.to_le_bytes());
        write_string(&mut buf, &self.technology);
        write_optional_string(&mut buf, &self.version);
        write_optional_string(&mut buf, &self.cpe);
        buf.push(self.confidence);
        write_string(&mut buf, &self.source);
        buf.extend_from_slice(&self.detected_at.to_le_bytes());
        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.is_empty() {
            return Err(DecodeError("empty fingerprint record"));
        }
        let mut pos = 0;
        let host = read_string(bytes, &mut pos)?;

        if bytes.len() < pos + 2 {
            return Err(DecodeError("truncated port"));
        }
        let port = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]);
        pos += 2;

        let technology = read_string(bytes, &mut pos)?;
        let version = read_optional_string(bytes, &mut pos)?;
        let cpe = read_optional_string(bytes, &mut pos)?;

        if pos >= bytes.len() {
            return Err(DecodeError("truncated confidence"));
        }
        let confidence = bytes[pos];
        pos += 1;

        let source = read_string(bytes, &mut pos)?;

        if bytes.len() < pos + 4 {
            return Err(DecodeError("truncated timestamp"));
        }
        let detected_at =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);

        Ok(Self {
            host,
            port,
            technology,
            version,
            cpe,
            confidence,
            source,
            detected_at,
        })
    }
}
