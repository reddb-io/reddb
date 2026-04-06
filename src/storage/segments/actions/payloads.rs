//! Record Payload Types
//!
//! Data structures for different action types: PortScan, Ping, DNS, TLS, HTTP, Whois, Vuln, Fingerprint.

use crate::storage::primitives::encoding::{
    read_string, read_varu32, read_varu64, write_string, write_varu32, write_varu64, DecodeError,
};

// ==================== PortScanData ====================

/// Port scan result data
#[derive(Debug, Clone, Default)]
pub struct PortScanData {
    pub open_ports: Vec<u16>,
    pub closed_ports: Vec<u16>,
    pub filtered_ports: Vec<u16>,
    pub duration_ms: u64,
}

impl PortScanData {
    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        write_varu32(buf, self.open_ports.len() as u32);
        for port in &self.open_ports {
            buf.extend_from_slice(&port.to_le_bytes());
        }
        write_varu32(buf, self.closed_ports.len() as u32);
        for port in &self.closed_ports {
            buf.extend_from_slice(&port.to_le_bytes());
        }
        write_varu32(buf, self.filtered_ports.len() as u32);
        for port in &self.filtered_ports {
            buf.extend_from_slice(&port.to_le_bytes());
        }
        write_varu64(buf, self.duration_ms);
    }

    pub(crate) fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let open_count = read_varu32(bytes, pos)? as usize;
        let mut open_ports = Vec::with_capacity(open_count);
        for _ in 0..open_count {
            if *pos + 2 > bytes.len() {
                return Err(DecodeError("unexpected eof (open port)"));
            }
            open_ports.push(u16::from_le_bytes([bytes[*pos], bytes[*pos + 1]]));
            *pos += 2;
        }

        let closed_count = read_varu32(bytes, pos)? as usize;
        let mut closed_ports = Vec::with_capacity(closed_count);
        for _ in 0..closed_count {
            if *pos + 2 > bytes.len() {
                return Err(DecodeError("unexpected eof (closed port)"));
            }
            closed_ports.push(u16::from_le_bytes([bytes[*pos], bytes[*pos + 1]]));
            *pos += 2;
        }

        let filtered_count = read_varu32(bytes, pos)? as usize;
        let mut filtered_ports = Vec::with_capacity(filtered_count);
        for _ in 0..filtered_count {
            if *pos + 2 > bytes.len() {
                return Err(DecodeError("unexpected eof (filtered port)"));
            }
            filtered_ports.push(u16::from_le_bytes([bytes[*pos], bytes[*pos + 1]]));
            *pos += 2;
        }

        let duration_ms = read_varu64(bytes, pos)?;

        Ok(Self {
            open_ports,
            closed_ports,
            filtered_ports,
            duration_ms,
        })
    }
}

// ==================== PingData ====================

/// Ping result data
#[derive(Debug, Clone, Default)]
pub struct PingData {
    pub reachable: bool,
    pub rtt_ms: Option<u64>,
    pub ttl: Option<u8>,
}

impl PingData {
    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        buf.push(if self.reachable { 1 } else { 0 });
        match self.rtt_ms {
            Some(rtt) => {
                buf.push(1);
                write_varu64(buf, rtt);
            }
            None => buf.push(0),
        }
        match self.ttl {
            Some(ttl) => {
                buf.push(1);
                buf.push(ttl);
            }
            None => buf.push(0),
        }
    }

    pub(crate) fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (PingData)"));
        }
        let reachable = bytes[*pos] != 0;
        *pos += 1;

        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (rtt flag)"));
        }
        let rtt_ms = if bytes[*pos] != 0 {
            *pos += 1;
            Some(read_varu64(bytes, pos)?)
        } else {
            *pos += 1;
            None
        };

        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (ttl flag)"));
        }
        let ttl = if bytes[*pos] != 0 {
            *pos += 1;
            if *pos >= bytes.len() {
                return Err(DecodeError("unexpected eof (ttl)"));
            }
            let val = bytes[*pos];
            *pos += 1;
            Some(val)
        } else {
            *pos += 1;
            None
        };

        Ok(Self {
            reachable,
            rtt_ms,
            ttl,
        })
    }
}

// ==================== DnsData ====================

/// DNS result data
#[derive(Debug, Clone, Default)]
pub struct DnsData {
    pub record_type: String,
    pub records: Vec<String>,
    pub ttl: Option<u32>,
}

impl DnsData {
    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        write_string(buf, &self.record_type);
        write_varu32(buf, self.records.len() as u32);
        for record in &self.records {
            write_string(buf, record);
        }
        match self.ttl {
            Some(ttl) => {
                buf.push(1);
                write_varu32(buf, ttl);
            }
            None => buf.push(0),
        }
    }

    pub(crate) fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let record_type = read_string(bytes, pos)?.to_string();
        let count = read_varu32(bytes, pos)? as usize;
        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            records.push(read_string(bytes, pos)?.to_string());
        }

        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (ttl flag)"));
        }
        let ttl = if bytes[*pos] != 0 {
            *pos += 1;
            Some(read_varu32(bytes, pos)?)
        } else {
            *pos += 1;
            None
        };

        Ok(Self {
            record_type,
            records,
            ttl,
        })
    }
}

// ==================== TlsData ====================

/// TLS audit result data
#[derive(Debug, Clone, Default)]
pub struct TlsData {
    pub version: String,
    pub cipher: String,
    pub certificate_subject: Option<String>,
    pub certificate_issuer: Option<String>,
    pub expires_at: Option<u64>,
    pub issues: Vec<String>,
}

impl TlsData {
    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        write_string(buf, &self.version);
        write_string(buf, &self.cipher);

        match &self.certificate_subject {
            Some(s) => {
                buf.push(1);
                write_string(buf, s);
            }
            None => buf.push(0),
        }

        match &self.certificate_issuer {
            Some(s) => {
                buf.push(1);
                write_string(buf, s);
            }
            None => buf.push(0),
        }

        match self.expires_at {
            Some(ts) => {
                buf.push(1);
                write_varu64(buf, ts);
            }
            None => buf.push(0),
        }

        write_varu32(buf, self.issues.len() as u32);
        for issue in &self.issues {
            write_string(buf, issue);
        }
    }

    pub(crate) fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let version = read_string(bytes, pos)?.to_string();
        let cipher = read_string(bytes, pos)?.to_string();

        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (cert subject flag)"));
        }
        let certificate_subject = if bytes[*pos] != 0 {
            *pos += 1;
            Some(read_string(bytes, pos)?.to_string())
        } else {
            *pos += 1;
            None
        };

        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (cert issuer flag)"));
        }
        let certificate_issuer = if bytes[*pos] != 0 {
            *pos += 1;
            Some(read_string(bytes, pos)?.to_string())
        } else {
            *pos += 1;
            None
        };

        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (expires flag)"));
        }
        let expires_at = if bytes[*pos] != 0 {
            *pos += 1;
            Some(read_varu64(bytes, pos)?)
        } else {
            *pos += 1;
            None
        };

        let count = read_varu32(bytes, pos)? as usize;
        let mut issues = Vec::with_capacity(count);
        for _ in 0..count {
            issues.push(read_string(bytes, pos)?.to_string());
        }

        Ok(Self {
            version,
            cipher,
            certificate_subject,
            certificate_issuer,
            expires_at,
            issues,
        })
    }
}

// ==================== HttpData ====================

/// HTTP result data
#[derive(Debug, Clone, Default)]
pub struct HttpData {
    pub method: String,
    pub status_code: u16,
    pub headers: Vec<(String, String)>,
    pub body_size: u64,
    pub response_time_ms: u64,
}

impl HttpData {
    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        write_string(buf, &self.method);
        buf.extend_from_slice(&self.status_code.to_le_bytes());
        write_varu32(buf, self.headers.len() as u32);
        for (k, v) in &self.headers {
            write_string(buf, k);
            write_string(buf, v);
        }
        write_varu64(buf, self.body_size);
        write_varu64(buf, self.response_time_ms);
    }

    pub(crate) fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let method = read_string(bytes, pos)?.to_string();

        if *pos + 2 > bytes.len() {
            return Err(DecodeError("unexpected eof (status code)"));
        }
        let status_code = u16::from_le_bytes([bytes[*pos], bytes[*pos + 1]]);
        *pos += 2;

        let count = read_varu32(bytes, pos)? as usize;
        let mut headers = Vec::with_capacity(count);
        for _ in 0..count {
            let k = read_string(bytes, pos)?.to_string();
            let v = read_string(bytes, pos)?.to_string();
            headers.push((k, v));
        }

        let body_size = read_varu64(bytes, pos)?;
        let response_time_ms = read_varu64(bytes, pos)?;

        Ok(Self {
            method,
            status_code,
            headers,
            body_size,
            response_time_ms,
        })
    }
}

// ==================== WhoisData ====================

/// Whois result data
#[derive(Debug, Clone, Default)]
pub struct WhoisData {
    pub registrar: Option<String>,
    pub created_date: Option<String>,
    pub expiry_date: Option<String>,
    pub name_servers: Vec<String>,
    pub raw: String,
}

impl WhoisData {
    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        match &self.registrar {
            Some(s) => {
                buf.push(1);
                write_string(buf, s);
            }
            None => buf.push(0),
        }
        match &self.created_date {
            Some(s) => {
                buf.push(1);
                write_string(buf, s);
            }
            None => buf.push(0),
        }
        match &self.expiry_date {
            Some(s) => {
                buf.push(1);
                write_string(buf, s);
            }
            None => buf.push(0),
        }
        write_varu32(buf, self.name_servers.len() as u32);
        for ns in &self.name_servers {
            write_string(buf, ns);
        }
        write_string(buf, &self.raw);
    }

    pub(crate) fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (registrar flag)"));
        }
        let registrar = if bytes[*pos] != 0 {
            *pos += 1;
            Some(read_string(bytes, pos)?.to_string())
        } else {
            *pos += 1;
            None
        };

        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (created flag)"));
        }
        let created_date = if bytes[*pos] != 0 {
            *pos += 1;
            Some(read_string(bytes, pos)?.to_string())
        } else {
            *pos += 1;
            None
        };

        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (expiry flag)"));
        }
        let expiry_date = if bytes[*pos] != 0 {
            *pos += 1;
            Some(read_string(bytes, pos)?.to_string())
        } else {
            *pos += 1;
            None
        };

        let count = read_varu32(bytes, pos)? as usize;
        let mut name_servers = Vec::with_capacity(count);
        for _ in 0..count {
            name_servers.push(read_string(bytes, pos)?.to_string());
        }

        let raw = read_string(bytes, pos)?.to_string();

        Ok(Self {
            registrar,
            created_date,
            expiry_date,
            name_servers,
            raw,
        })
    }
}

// ==================== VulnData ====================

/// Vulnerability finding data
#[derive(Debug, Clone, Default)]
pub struct VulnData {
    pub cve: Option<String>,
    pub title: String,
    pub severity: u8, // 0=Info, 1=Low, 2=Medium, 3=High, 4=Critical
    pub description: String,
    pub evidence: Option<String>,
}

impl VulnData {
    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        match &self.cve {
            Some(s) => {
                buf.push(1);
                write_string(buf, s);
            }
            None => buf.push(0),
        }
        write_string(buf, &self.title);
        buf.push(self.severity);
        write_string(buf, &self.description);
        match &self.evidence {
            Some(s) => {
                buf.push(1);
                write_string(buf, s);
            }
            None => buf.push(0),
        }
    }

    pub(crate) fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (cve flag)"));
        }
        let cve = if bytes[*pos] != 0 {
            *pos += 1;
            Some(read_string(bytes, pos)?.to_string())
        } else {
            *pos += 1;
            None
        };

        let title = read_string(bytes, pos)?.to_string();

        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (severity)"));
        }
        let severity = bytes[*pos];
        *pos += 1;

        let description = read_string(bytes, pos)?.to_string();

        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (evidence flag)"));
        }
        let evidence = if bytes[*pos] != 0 {
            *pos += 1;
            Some(read_string(bytes, pos)?.to_string())
        } else {
            *pos += 1;
            None
        };

        Ok(Self {
            cve,
            title,
            severity,
            description,
            evidence,
        })
    }
}

// ==================== FingerprintData ====================

/// Fingerprint result data
#[derive(Debug, Clone, Default)]
pub struct FingerprintData {
    pub service: String,
    pub version: Option<String>,
    pub os: Option<String>,
    pub cpe: Vec<String>,
    pub banner: Option<String>,
}

impl FingerprintData {
    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        write_string(buf, &self.service);
        match &self.version {
            Some(s) => {
                buf.push(1);
                write_string(buf, s);
            }
            None => buf.push(0),
        }
        match &self.os {
            Some(s) => {
                buf.push(1);
                write_string(buf, s);
            }
            None => buf.push(0),
        }
        write_varu32(buf, self.cpe.len() as u32);
        for c in &self.cpe {
            write_string(buf, c);
        }
        match &self.banner {
            Some(s) => {
                buf.push(1);
                write_string(buf, s);
            }
            None => buf.push(0),
        }
    }

    pub(crate) fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let service = read_string(bytes, pos)?.to_string();

        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (version flag)"));
        }
        let version = if bytes[*pos] != 0 {
            *pos += 1;
            Some(read_string(bytes, pos)?.to_string())
        } else {
            *pos += 1;
            None
        };

        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (os flag)"));
        }
        let os = if bytes[*pos] != 0 {
            *pos += 1;
            Some(read_string(bytes, pos)?.to_string())
        } else {
            *pos += 1;
            None
        };

        let count = read_varu32(bytes, pos)? as usize;
        let mut cpe = Vec::with_capacity(count);
        for _ in 0..count {
            cpe.push(read_string(bytes, pos)?.to_string());
        }

        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (banner flag)"));
        }
        let banner = if bytes[*pos] != 0 {
            *pos += 1;
            Some(read_string(bytes, pos)?.to_string())
        } else {
            *pos += 1;
            None
        };

        Ok(Self {
            service,
            version,
            os,
            cpe,
            banner,
        })
    }
}

// ==================== RecordPayload ====================

/// Payload variant for different action types
#[repr(u8)]
#[derive(Debug, Clone)]
pub enum RecordPayload {
    PortScan(PortScanData) = 0,
    Ping(PingData) = 1,
    Dns(DnsData) = 2,
    Tls(TlsData) = 3,
    Http(HttpData) = 4,
    Whois(WhoisData) = 5,
    Vuln(VulnData) = 6,
    Fingerprint(FingerprintData) = 7,
    /// Generic payload for custom data
    Custom(Vec<u8>) = 8,
}

impl RecordPayload {
    fn discriminant(&self) -> u8 {
        match self {
            Self::PortScan(_) => 0,
            Self::Ping(_) => 1,
            Self::Dns(_) => 2,
            Self::Tls(_) => 3,
            Self::Http(_) => 4,
            Self::Whois(_) => 5,
            Self::Vuln(_) => 6,
            Self::Fingerprint(_) => 7,
            Self::Custom(_) => 8,
        }
    }

    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        buf.push(self.discriminant());
        match self {
            Self::PortScan(d) => d.encode(buf),
            Self::Ping(d) => d.encode(buf),
            Self::Dns(d) => d.encode(buf),
            Self::Tls(d) => d.encode(buf),
            Self::Http(d) => d.encode(buf),
            Self::Whois(d) => d.encode(buf),
            Self::Vuln(d) => d.encode(buf),
            Self::Fingerprint(d) => d.encode(buf),
            Self::Custom(data) => {
                write_varu32(buf, data.len() as u32);
                buf.extend_from_slice(data);
            }
        }
    }

    pub(crate) fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (RecordPayload)"));
        }
        let disc = bytes[*pos];
        *pos += 1;

        match disc {
            0 => Ok(Self::PortScan(PortScanData::decode(bytes, pos)?)),
            1 => Ok(Self::Ping(PingData::decode(bytes, pos)?)),
            2 => Ok(Self::Dns(DnsData::decode(bytes, pos)?)),
            3 => Ok(Self::Tls(TlsData::decode(bytes, pos)?)),
            4 => Ok(Self::Http(HttpData::decode(bytes, pos)?)),
            5 => Ok(Self::Whois(WhoisData::decode(bytes, pos)?)),
            6 => Ok(Self::Vuln(VulnData::decode(bytes, pos)?)),
            7 => Ok(Self::Fingerprint(FingerprintData::decode(bytes, pos)?)),
            8 => {
                let len = read_varu32(bytes, pos)? as usize;
                if *pos + len > bytes.len() {
                    return Err(DecodeError("unexpected eof (custom data)"));
                }
                let data = bytes[*pos..*pos + len].to_vec();
                *pos += len;
                Ok(Self::Custom(data))
            }
            _ => Err(DecodeError("invalid RecordPayload discriminant")),
        }
    }
}
