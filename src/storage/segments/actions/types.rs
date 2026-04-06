//! Core Types for Actions
//!
//! Defines the fundamental enums: ActionSource, Target, ActionType, ActionOutcome.

use std::net::IpAddr;

use crate::storage::primitives::encoding::{
    read_ip, read_string, read_varu32, read_varu64, write_ip, write_string, write_varu32,
    write_varu64, DecodeError,
};

// ==================== ActionSource ====================

/// Source of an action - who/what produced it
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionSource {
    /// CLI tool execution
    Tool { name: String } = 0,
    /// Playbook step execution
    Playbook { id: [u8; 16], step: u32 } = 1,
    /// Manual entry
    Manual = 2,
}

impl ActionSource {
    pub fn tool(name: &str) -> Self {
        Self::Tool {
            name: name.to_string(),
        }
    }

    pub fn playbook(id: [u8; 16], step: u32) -> Self {
        Self::Playbook { id, step }
    }

    fn discriminant(&self) -> u8 {
        match self {
            Self::Tool { .. } => 0,
            Self::Playbook { .. } => 1,
            Self::Manual => 2,
        }
    }

    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        buf.push(self.discriminant());
        match self {
            Self::Tool { name } => {
                write_string(buf, name);
            }
            Self::Playbook { id, step } => {
                buf.extend_from_slice(id);
                write_varu32(buf, *step);
            }
            Self::Manual => {}
        }
    }

    pub(crate) fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (ActionSource)"));
        }
        let disc = bytes[*pos];
        *pos += 1;

        match disc {
            0 => {
                let name = read_string(bytes, pos)?.to_string();
                Ok(Self::Tool { name })
            }
            1 => {
                if *pos + 16 > bytes.len() {
                    return Err(DecodeError("unexpected eof (playbook id)"));
                }
                let mut id = [0u8; 16];
                id.copy_from_slice(&bytes[*pos..*pos + 16]);
                *pos += 16;
                let step = read_varu32(bytes, pos)?;
                Ok(Self::Playbook { id, step })
            }
            2 => Ok(Self::Manual),
            _ => Err(DecodeError("invalid ActionSource discriminant")),
        }
    }
}

// ==================== Target ====================

/// Target of an action - what was acted upon
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// Single host IP
    Host(IpAddr) = 0,
    /// Network CIDR (ip, prefix_len)
    Network(IpAddr, u8) = 1,
    /// Domain name
    Domain(String) = 2,
    /// Full URL
    Url(String) = 3,
    /// Specific port on host
    Port(IpAddr, u16) = 4,
    /// Service on host:port
    Service(IpAddr, u16, String) = 5,
}

impl Target {
    fn discriminant(&self) -> u8 {
        match self {
            Self::Host(_) => 0,
            Self::Network(_, _) => 1,
            Self::Domain(_) => 2,
            Self::Url(_) => 3,
            Self::Port(_, _) => 4,
            Self::Service(_, _, _) => 5,
        }
    }

    /// Get the primary IP if target has one
    pub fn ip(&self) -> Option<IpAddr> {
        match self {
            Self::Host(ip) | Self::Network(ip, _) | Self::Port(ip, _) | Self::Service(ip, _, _) => {
                Some(*ip)
            }
            Self::Domain(_) | Self::Url(_) => None,
        }
    }

    /// Get domain/host string representation
    pub fn host_str(&self) -> String {
        match self {
            Self::Host(ip) => ip.to_string(),
            Self::Network(ip, prefix) => format!("{}/{}", ip, prefix),
            Self::Domain(d) => d.clone(),
            Self::Url(u) => u.clone(),
            Self::Port(ip, port) => format!("{}:{}", ip, port),
            Self::Service(ip, port, svc) => format!("{}:{} ({})", ip, port, svc),
        }
    }

    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        buf.push(self.discriminant());
        match self {
            Self::Host(ip) => {
                write_ip(buf, ip);
            }
            Self::Network(ip, prefix) => {
                write_ip(buf, ip);
                buf.push(*prefix);
            }
            Self::Domain(d) => {
                write_string(buf, d);
            }
            Self::Url(u) => {
                write_string(buf, u);
            }
            Self::Port(ip, port) => {
                write_ip(buf, ip);
                buf.extend_from_slice(&port.to_le_bytes());
            }
            Self::Service(ip, port, svc) => {
                write_ip(buf, ip);
                buf.extend_from_slice(&port.to_le_bytes());
                write_string(buf, svc);
            }
        }
    }

    pub(crate) fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (Target)"));
        }
        let disc = bytes[*pos];
        *pos += 1;

        match disc {
            0 => {
                let ip = read_ip(bytes, pos)?;
                Ok(Self::Host(ip))
            }
            1 => {
                let ip = read_ip(bytes, pos)?;
                if *pos >= bytes.len() {
                    return Err(DecodeError("unexpected eof (network prefix)"));
                }
                let prefix = bytes[*pos];
                *pos += 1;
                Ok(Self::Network(ip, prefix))
            }
            2 => {
                let d = read_string(bytes, pos)?.to_string();
                Ok(Self::Domain(d))
            }
            3 => {
                let u = read_string(bytes, pos)?.to_string();
                Ok(Self::Url(u))
            }
            4 => {
                let ip = read_ip(bytes, pos)?;
                if *pos + 2 > bytes.len() {
                    return Err(DecodeError("unexpected eof (port)"));
                }
                let port = u16::from_le_bytes([bytes[*pos], bytes[*pos + 1]]);
                *pos += 2;
                Ok(Self::Port(ip, port))
            }
            5 => {
                let ip = read_ip(bytes, pos)?;
                if *pos + 2 > bytes.len() {
                    return Err(DecodeError("unexpected eof (service port)"));
                }
                let port = u16::from_le_bytes([bytes[*pos], bytes[*pos + 1]]);
                *pos += 2;
                let svc = read_string(bytes, pos)?.to_string();
                Ok(Self::Service(ip, port, svc))
            }
            _ => Err(DecodeError("invalid Target discriminant")),
        }
    }
}

// ==================== ActionType ====================

/// Type of action performed
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionType {
    /// Port/host discovery
    Scan = 0,
    /// Service/version enumeration
    Enumerate = 1,
    /// Service fingerprinting
    Fingerprint = 2,
    /// Security audit
    Audit = 3,
    /// Exploit attempt
    Exploit = 4,
    /// Report generation
    Report = 5,
    /// DNS resolution
    Resolve = 6,
    /// HTTP request
    Request = 7,
    /// TLS inspection
    Inspect = 8,
    /// Whois lookup
    Lookup = 9,
}

impl ActionType {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Scan),
            1 => Some(Self::Enumerate),
            2 => Some(Self::Fingerprint),
            3 => Some(Self::Audit),
            4 => Some(Self::Exploit),
            5 => Some(Self::Report),
            6 => Some(Self::Resolve),
            7 => Some(Self::Request),
            8 => Some(Self::Inspect),
            9 => Some(Self::Lookup),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Enumerate => "enumerate",
            Self::Fingerprint => "fingerprint",
            Self::Audit => "audit",
            Self::Exploit => "exploit",
            Self::Report => "report",
            Self::Resolve => "resolve",
            Self::Request => "request",
            Self::Inspect => "inspect",
            Self::Lookup => "lookup",
        }
    }
}

// ==================== ActionOutcome ====================

/// Outcome of an action
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionOutcome {
    /// Completed successfully
    Success = 0,
    /// Failed with error
    Failed { error: String } = 1,
    /// Timed out
    Timeout { after_ms: u64 } = 2,
    /// Partially completed
    Partial { completed: u32, total: u32 } = 3,
    /// Skipped (e.g., already done)
    Skipped { reason: String } = 4,
}

impl ActionOutcome {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. } | Self::Timeout { .. })
    }

    pub(crate) fn discriminant(&self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Failed { .. } => 1,
            Self::Timeout { .. } => 2,
            Self::Partial { .. } => 3,
            Self::Skipped { .. } => 4,
        }
    }

    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        buf.push(self.discriminant());
        match self {
            Self::Success => {}
            Self::Failed { error } => {
                write_string(buf, error);
            }
            Self::Timeout { after_ms } => {
                write_varu64(buf, *after_ms);
            }
            Self::Partial { completed, total } => {
                write_varu32(buf, *completed);
                write_varu32(buf, *total);
            }
            Self::Skipped { reason } => {
                write_string(buf, reason);
            }
        }
    }

    pub(crate) fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (ActionOutcome)"));
        }
        let disc = bytes[*pos];
        *pos += 1;

        match disc {
            0 => Ok(Self::Success),
            1 => {
                let error = read_string(bytes, pos)?.to_string();
                Ok(Self::Failed { error })
            }
            2 => {
                let after_ms = read_varu64(bytes, pos)?;
                Ok(Self::Timeout { after_ms })
            }
            3 => {
                let completed = read_varu32(bytes, pos)?;
                let total = read_varu32(bytes, pos)?;
                Ok(Self::Partial { completed, total })
            }
            4 => {
                let reason = read_string(bytes, pos)?.to_string();
                Ok(Self::Skipped { reason })
            }
            _ => Err(DecodeError("invalid ActionOutcome discriminant")),
        }
    }
}
