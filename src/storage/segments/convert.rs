//! IntoActionRecord conversions for existing Result types.
//!
//! This module bridges the gap between existing tool outputs and the unified
//! ActionRecord format, enabling automatic persistence to RedDB.

use std::net::IpAddr;

use super::actions::{
    ActionOutcome, ActionRecord, ActionSource, ActionType, DnsData, FingerprintData, HttpData,
    IntoActionRecord, PingData, PortScanData, RecordPayload, Target, TlsData, VulnData, WhoisData,
};
use super::loot::Confidence;

// ==================== Network Module Conversions ====================

/// Wrapper for port scan results from a full scan
pub struct PortScanResults {
    pub target: IpAddr,
    pub open_ports: Vec<u16>,
    pub closed_ports: Vec<u16>,
    pub filtered_ports: Vec<u16>,
    pub duration_ms: u64,
}

impl IntoActionRecord for PortScanResults {
    fn into_action_record(self, source: ActionSource) -> ActionRecord {
        let outcome = if !self.open_ports.is_empty() {
            ActionOutcome::Success
        } else if !self.filtered_ports.is_empty() {
            ActionOutcome::Partial {
                completed: self.open_ports.len() as u32 + self.closed_ports.len() as u32,
                total: (self.open_ports.len() + self.closed_ports.len() + self.filtered_ports.len())
                    as u32,
            }
        } else {
            ActionOutcome::Success
        };

        ActionRecord::new(
            source,
            Target::Host(self.target),
            ActionType::Scan,
            RecordPayload::PortScan(PortScanData {
                open_ports: self.open_ports,
                closed_ports: self.closed_ports,
                filtered_ports: self.filtered_ports,
                duration_ms: self.duration_ms,
            }),
            outcome,
        )
    }
}

// ==================== Ping Result Conversion ====================

/// Wrapper for ping results
pub struct PingResults {
    pub target: IpAddr,
    pub reachable: bool,
    pub rtt_ms: Option<u64>,
    pub ttl: Option<u8>,
}

impl IntoActionRecord for PingResults {
    fn into_action_record(self, source: ActionSource) -> ActionRecord {
        let outcome = if self.reachable {
            ActionOutcome::Success
        } else {
            ActionOutcome::Failed {
                error: "Host unreachable".into(),
            }
        };

        ActionRecord::new(
            source,
            Target::Host(self.target),
            ActionType::Scan,
            RecordPayload::Ping(PingData {
                reachable: self.reachable,
                rtt_ms: self.rtt_ms,
                ttl: self.ttl,
            }),
            outcome,
        )
    }
}

// ==================== DNS Result Conversion ====================

/// Wrapper for DNS lookup results
pub struct DnsResults {
    pub domain: String,
    pub record_type: String,
    pub records: Vec<String>,
    pub ttl: Option<u32>,
    pub error: Option<String>,
}

impl IntoActionRecord for DnsResults {
    fn into_action_record(self, source: ActionSource) -> ActionRecord {
        let outcome = match &self.error {
            Some(e) => ActionOutcome::Failed { error: e.clone() },
            None if self.records.is_empty() => ActionOutcome::Success, // NXDOMAIN is still a result
            None => ActionOutcome::Success,
        };

        ActionRecord::new(
            source,
            Target::Domain(self.domain),
            ActionType::Resolve,
            RecordPayload::Dns(DnsData {
                record_type: self.record_type,
                records: self.records,
                ttl: self.ttl,
            }),
            outcome,
        )
    }
}

// ==================== TLS Audit Result Conversion ====================

/// Wrapper for TLS audit results
pub struct TlsAuditResults {
    pub host: String,
    pub port: u16,
    pub version: String,
    pub cipher: String,
    pub certificate_subject: Option<String>,
    pub certificate_issuer: Option<String>,
    pub expires_at: Option<u64>,
    pub issues: Vec<String>,
    pub error: Option<String>,
}

impl IntoActionRecord for TlsAuditResults {
    fn into_action_record(self, source: ActionSource) -> ActionRecord {
        let target = if let Ok(ip) = self.host.parse::<IpAddr>() {
            Target::Service(ip, self.port, "tls".into())
        } else {
            Target::Domain(format!("{}:{}", self.host, self.port))
        };

        let outcome = match &self.error {
            Some(e) => ActionOutcome::Failed { error: e.clone() },
            None => ActionOutcome::Success,
        };

        ActionRecord::new(
            source,
            target,
            ActionType::Audit,
            RecordPayload::Tls(TlsData {
                version: self.version,
                cipher: self.cipher,
                certificate_subject: self.certificate_subject,
                certificate_issuer: self.certificate_issuer,
                expires_at: self.expires_at,
                issues: self.issues,
            }),
            outcome,
        )
    }
}

// ==================== HTTP Result Conversion ====================

/// Wrapper for HTTP response results
pub struct HttpResults {
    pub url: String,
    pub method: String,
    pub status_code: u16,
    pub headers: Vec<(String, String)>,
    pub body_size: u64,
    pub response_time_ms: u64,
    pub error: Option<String>,
}

impl IntoActionRecord for HttpResults {
    fn into_action_record(self, source: ActionSource) -> ActionRecord {
        let outcome = match &self.error {
            Some(e) => ActionOutcome::Failed { error: e.clone() },
            None if self.status_code >= 400 => ActionOutcome::Partial {
                completed: 1,
                total: 1,
            },
            None => ActionOutcome::Success,
        };

        ActionRecord::new(
            source,
            Target::Url(self.url),
            ActionType::Request,
            RecordPayload::Http(HttpData {
                method: self.method,
                status_code: self.status_code,
                headers: self.headers,
                body_size: self.body_size,
                response_time_ms: self.response_time_ms,
            }),
            outcome,
        )
    }
}

// ==================== Whois Result Conversion ====================

/// Wrapper for WHOIS lookup results
pub struct WhoisResults {
    pub domain: String,
    pub registrar: Option<String>,
    pub created_date: Option<String>,
    pub expiry_date: Option<String>,
    pub name_servers: Vec<String>,
    pub raw: String,
    pub error: Option<String>,
}

impl IntoActionRecord for WhoisResults {
    fn into_action_record(self, source: ActionSource) -> ActionRecord {
        let outcome = match &self.error {
            Some(e) => ActionOutcome::Failed { error: e.clone() },
            None => ActionOutcome::Success,
        };

        ActionRecord::new(
            source,
            Target::Domain(self.domain),
            ActionType::Lookup,
            RecordPayload::Whois(WhoisData {
                registrar: self.registrar,
                created_date: self.created_date,
                expiry_date: self.expiry_date,
                name_servers: self.name_servers,
                raw: self.raw,
            }),
            outcome,
        )
    }
}

// ==================== Fingerprint Result Conversion ====================

/// Wrapper for service fingerprint results
pub struct FingerprintResults {
    pub target: IpAddr,
    pub port: u16,
    pub service: String,
    pub version: Option<String>,
    pub os: Option<String>,
    pub cpe: Vec<String>,
    pub banner: Option<String>,
    pub error: Option<String>,
}

impl IntoActionRecord for FingerprintResults {
    fn into_action_record(self, source: ActionSource) -> ActionRecord {
        let outcome = match &self.error {
            Some(e) => ActionOutcome::Failed { error: e.clone() },
            None => ActionOutcome::Success,
        };

        ActionRecord::new(
            source,
            Target::Service(self.target, self.port, self.service.clone()),
            ActionType::Fingerprint,
            RecordPayload::Fingerprint(FingerprintData {
                service: self.service,
                version: self.version,
                os: self.os,
                cpe: self.cpe,
                banner: self.banner,
            }),
            outcome,
        )
    }
}

// ==================== Vulnerability Result Conversion ====================

/// Wrapper for vulnerability findings
pub struct VulnResults {
    pub target: Target,
    pub cve: Option<String>,
    pub title: String,
    pub severity: u8,
    pub description: String,
    pub evidence: Option<String>,
}

impl IntoActionRecord for VulnResults {
    fn into_action_record(self, source: ActionSource) -> ActionRecord {
        let mut record = ActionRecord::new(
            source,
            self.target,
            ActionType::Audit,
            RecordPayload::Vuln(VulnData {
                cve: self.cve,
                title: self.title,
                severity: self.severity,
                description: self.description,
                evidence: self.evidence,
            }),
            ActionOutcome::Success,
        );

        // High severity = high confidence
        record.confidence = if self.severity >= 3 {
            Confidence::High
        } else if self.severity >= 2 {
            Confidence::Medium
        } else {
            Confidence::Low
        };

        record
    }
}

// ==================== Builder Helpers ====================

/// Helper to create ActionRecord from common scan patterns
pub struct ActionBuilder {
    source: ActionSource,
}

impl ActionBuilder {
    pub fn new(tool_name: &str) -> Self {
        Self {
            source: ActionSource::tool(tool_name),
        }
    }

    pub fn playbook(id: [u8; 16], step: u32) -> Self {
        Self {
            source: ActionSource::playbook(id, step),
        }
    }

    pub fn port_scan(
        self,
        target: IpAddr,
        open: Vec<u16>,
        closed: Vec<u16>,
        filtered: Vec<u16>,
        duration_ms: u64,
    ) -> ActionRecord {
        PortScanResults {
            target,
            open_ports: open,
            closed_ports: closed,
            filtered_ports: filtered,
            duration_ms,
        }
        .into_action_record(self.source)
    }

    pub fn ping(self, target: IpAddr, reachable: bool, rtt_ms: Option<u64>) -> ActionRecord {
        PingResults {
            target,
            reachable,
            rtt_ms,
            ttl: None,
        }
        .into_action_record(self.source)
    }

    pub fn dns(
        self,
        domain: String,
        record_type: &str,
        records: Vec<String>,
        ttl: Option<u32>,
    ) -> ActionRecord {
        DnsResults {
            domain,
            record_type: record_type.to_string(),
            records,
            ttl,
            error: None,
        }
        .into_action_record(self.source)
    }

    pub fn dns_error(self, domain: String, record_type: &str, error: String) -> ActionRecord {
        DnsResults {
            domain,
            record_type: record_type.to_string(),
            records: vec![],
            ttl: None,
            error: Some(error),
        }
        .into_action_record(self.source)
    }

    pub fn http(
        self,
        url: String,
        method: &str,
        status: u16,
        headers: Vec<(String, String)>,
        body_size: u64,
        time_ms: u64,
    ) -> ActionRecord {
        HttpResults {
            url,
            method: method.to_string(),
            status_code: status,
            headers,
            body_size,
            response_time_ms: time_ms,
            error: None,
        }
        .into_action_record(self.source)
    }

    pub fn http_error(self, url: String, method: &str, error: String) -> ActionRecord {
        HttpResults {
            url,
            method: method.to_string(),
            status_code: 0,
            headers: vec![],
            body_size: 0,
            response_time_ms: 0,
            error: Some(error),
        }
        .into_action_record(self.source)
    }

    pub fn timeout(self, target: Target, action_type: ActionType, after_ms: u64) -> ActionRecord {
        ActionRecord::new(
            self.source,
            target,
            action_type,
            RecordPayload::Custom(vec![]),
            ActionOutcome::Timeout { after_ms },
        )
    }

    pub fn skipped(self, target: Target, action_type: ActionType, reason: &str) -> ActionRecord {
        ActionRecord::new(
            self.source,
            target,
            action_type,
            RecordPayload::Custom(vec![]),
            ActionOutcome::Skipped {
                reason: reason.to_string(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn test_port_scan_conversion() {
        let results = PortScanResults {
            target: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            open_ports: vec![22, 80, 443],
            closed_ports: vec![21],
            filtered_ports: vec![],
            duration_ms: 5000,
        };

        let record = results.into_action_record(ActionSource::tool("network-ports"));

        assert!(record.is_success());
        assert_eq!(record.action_type, ActionType::Scan);

        if let RecordPayload::PortScan(data) = record.payload {
            assert_eq!(data.open_ports, vec![22, 80, 443]);
        } else {
            panic!("wrong payload type");
        }
    }

    #[test]
    fn test_action_builder() {
        let builder = ActionBuilder::new("network-ports");
        let record = builder.port_scan(
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            vec![80],
            vec![],
            vec![],
            1000,
        );

        assert!(record.is_success());
        if let ActionSource::Tool { name } = &record.source {
            assert_eq!(name, "network-ports");
        } else {
            panic!("wrong source type");
        }
    }

    #[test]
    fn test_dns_conversion() {
        let results = DnsResults {
            domain: "example.com".into(),
            record_type: "A".into(),
            records: vec!["93.184.216.34".into()],
            ttl: Some(3600),
            error: None,
        };

        let record = results.into_action_record(ActionSource::tool("dns-lookup"));

        assert!(record.is_success());
        assert_eq!(record.action_type, ActionType::Resolve);
    }

    #[test]
    fn test_vuln_conversion() {
        let results = VulnResults {
            target: Target::Host(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
            cve: Some("CVE-2021-44228".into()),
            title: "Log4Shell".into(),
            severity: 4, // Critical
            description: "Remote code execution via JNDI".into(),
            evidence: Some("Found vulnerable endpoint".into()),
        };

        let record = results.into_action_record(ActionSource::tool("vuln-scanner"));

        assert_eq!(record.confidence, Confidence::High);
        if let RecordPayload::Vuln(data) = record.payload {
            assert_eq!(data.cve, Some("CVE-2021-44228".into()));
            assert_eq!(data.severity, 4);
        } else {
            panic!("wrong payload type");
        }
    }
}
