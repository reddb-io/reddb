//! Network scanning record types

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Port scan result - 20 bytes for IPv4 payloads.
#[derive(Debug, Clone)]
pub struct PortScanRecord {
    pub ip: IpAddr,         // 4 or 16 bytes
    pub port: u16,          // 2 bytes
    pub status: PortStatus, // 1 byte
    pub service_id: u8,     // 1 byte (service classification enum)
    pub timestamp: u32,     // 4 bytes (Unix time)
}

impl PortScanRecord {
    pub fn new(ip: u32, port: u16, state: u8, service_id: u8) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        let status = match state {
            0 => PortStatus::Open,
            1 => PortStatus::Closed,
            2 => PortStatus::Filtered,
            _ => PortStatus::OpenFiltered,
        };

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;

        Self {
            ip: IpAddr::V4(std::net::Ipv4Addr::from(ip)),
            port,
            status,
            service_id,
            timestamp,
        }
    }

    /// Serialize to bytes (19 bytes for IPv4)
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(20);

        // IP address
        match self.ip {
            IpAddr::V4(ip) => {
                buf.push(4); // IPv4 marker
                buf.extend_from_slice(&ip.octets());
            }
            IpAddr::V6(ip) => {
                buf.push(6); // IPv6 marker
                buf.extend_from_slice(&ip.octets());
            }
        }

        // Port
        buf.extend_from_slice(&self.port.to_le_bytes());

        // Status + service
        buf.push(self.status as u8);
        buf.push(self.service_id);

        // Timestamp
        buf.extend_from_slice(&self.timestamp.to_le_bytes());

        buf
    }

    /// Deserialize from bytes
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.is_empty() {
            return None;
        }

        let ip_version = buf[0];

        let (ip, offset) = if ip_version == 4 {
            if buf.len() < 1 + 4 {
                return None;
            }
            let octets = [buf[1], buf[2], buf[3], buf[4]];
            (IpAddr::V4(Ipv4Addr::from(octets)), 5)
        } else if ip_version == 6 {
            if buf.len() < 1 + 16 {
                return None;
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&buf[1..17]);
            (IpAddr::V6(Ipv6Addr::from(octets)), 17)
        } else {
            return None;
        };

        if buf.len() < offset + 8 {
            return None;
        }

        let port = u16::from_le_bytes([buf[offset], buf[offset + 1]]);
        let status = match buf[offset + 2] {
            0 => PortStatus::Open,
            1 => PortStatus::Closed,
            2 => PortStatus::Filtered,
            3 => PortStatus::OpenFiltered,
            _ => return None,
        };
        let service_id = buf.get(offset + 3).copied()?;
        let timestamp = u32::from_le_bytes([
            buf[offset + 4],
            buf[offset + 5],
            buf[offset + 6],
            buf[offset + 7],
        ]);

        Some(Self {
            ip,
            port,
            status,
            service_id,
            timestamp,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortStatus {
    Open = 0,
    Closed = 1,
    Filtered = 2,
    OpenFiltered = 3,
}

/// Subdomain record - variable size, compressed
#[derive(Debug, Clone)]
pub struct SubdomainRecord {
    pub subdomain: String,
    pub ips: Vec<IpAddr>,
    pub source: SubdomainSource,
    pub timestamp: u32,
}

impl SubdomainRecord {
    /// Serialize with compression
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Subdomain length + data
        let subdomain_bytes = self.subdomain.as_bytes();
        buf.push(subdomain_bytes.len() as u8);
        buf.extend_from_slice(subdomain_bytes);

        // Number of IPs
        buf.push(self.ips.len() as u8);
        for ip in &self.ips {
            match ip {
                IpAddr::V4(ip) => {
                    buf.push(4);
                    buf.extend_from_slice(&ip.octets());
                }
                IpAddr::V6(ip) => {
                    buf.push(6);
                    buf.extend_from_slice(&ip.octets());
                }
            }
        }

        // Source
        buf.push(self.source as u8);

        // Timestamp
        buf.extend_from_slice(&self.timestamp.to_le_bytes());

        buf
    }

    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.is_empty() {
            return None;
        }

        let mut offset = 0;

        // Read subdomain
        let subdomain_len = buf[offset] as usize;
        offset += 1;
        if buf.len() < offset + subdomain_len {
            return None;
        }
        let subdomain = String::from_utf8(buf[offset..offset + subdomain_len].to_vec()).ok()?;
        offset += subdomain_len;

        // Read IPs
        if buf.len() < offset + 1 {
            return None;
        }
        let ip_count = buf[offset] as usize;
        offset += 1;

        let mut ips = Vec::new();
        for _ in 0..ip_count {
            if buf.len() < offset + 1 {
                return None;
            }
            let ip_version = buf[offset];
            offset += 1;

            if ip_version == 4 {
                if buf.len() < offset + 4 {
                    return None;
                }
                let octets = [
                    buf[offset],
                    buf[offset + 1],
                    buf[offset + 2],
                    buf[offset + 3],
                ];
                ips.push(IpAddr::V4(Ipv4Addr::from(octets)));
                offset += 4;
            } else if ip_version == 6 {
                if buf.len() < offset + 16 {
                    return None;
                }
                let mut octets = [0u8; 16];
                octets.copy_from_slice(&buf[offset..offset + 16]);
                ips.push(IpAddr::V6(Ipv6Addr::from(octets)));
                offset += 16;
            }
        }

        // Source
        if buf.len() < offset + 1 {
            return None;
        }
        let source = match buf[offset] {
            0 => SubdomainSource::DnsBruteforce,
            1 => SubdomainSource::CertTransparency,
            2 => SubdomainSource::SearchEngine,
            3 => SubdomainSource::WebCrawl,
            _ => return None,
        };
        offset += 1;

        // Timestamp
        if buf.len() < offset + 4 {
            return None;
        }
        let timestamp = u32::from_le_bytes([
            buf[offset],
            buf[offset + 1],
            buf[offset + 2],
            buf[offset + 3],
        ]);

        Some(Self {
            subdomain,
            ips,
            source,
            timestamp,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum SubdomainSource {
    DnsBruteforce = 0,
    CertTransparency = 1,
    SearchEngine = 2,
    WebCrawl = 3,
}
