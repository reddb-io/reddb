// Query interface for RedDB - RESTful operations
// Provides list, get, describe, delete operations on stored data

use crate::modules::common::Severity as RecordSeverity;
use crate::storage::primitives::encoding::DecodeError;
use crate::storage::records::{
    DnsRecordData, DnsRecordType, HostIntelRecord, HttpHeadersRecord, PortStatus,
    ProxyConnectionRecord, ProxyHttpRequestRecord, ProxyHttpResponseRecord, SubdomainRecord,
    SubdomainSource, TlsScanRecord, WhoisRecord,
};
use crate::storage::schema::Value;
use crate::storage::segments::actions::ActionRecord;
use crate::storage::RedDB;
use std::io;
use std::net::IpAddr;
use std::path::Path;

/// Query interface for reading stored scan data
pub struct QueryManager {
    db: RedDB,
}

impl QueryManager {
    /// Open database for querying
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path_ref = path.as_ref();

        // Open the Modern RedDB store
        let db = RedDB::open(path_ref)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        Ok(Self { db })
    }

    /// List all open ports for a specific IP
    pub fn list_ports(&mut self, ip: IpAddr) -> io::Result<Vec<u16>> {
        let results = self
            .db
            .query()
            .collection("ports")
            .where_prop("ip", ip.to_string())
            .execute();

        match results {
            Ok(res) => {
                let mut ports: Vec<u16> = Vec::new();
                for item in res.items {
                    if let Some(node) = item.entity.data.as_node() {
                        if let Some(state) = node.get("state").and_then(|v| v.as_text()) {
                            if state == "open" {
                                if let Some(p) = node.get("port").and_then(|v| v.as_integer()) {
                                    ports.push(p as u16);
                                }
                            }
                        }
                    }
                }
                ports.sort_unstable();
                ports.dedup();
                Ok(ports)
            }
            Err(_) => Ok(Vec::new()),
        }
    }

    /// List all subdomains for a domain
    pub fn list_subdomains(&mut self, domain: &str) -> io::Result<Vec<String>> {
        let results = self
            .db
            .query()
            .collection("domains")
            .where_prop("parent", domain)
            .execute();

        match results {
            Ok(res) => {
                let mut values: Vec<String> = Vec::new();
                for item in res.items {
                    if let Some(node) = item.entity.data.as_node() {
                        if let Some(name) = node.get("name").and_then(|v| v.as_text()) {
                            values.push(name.to_string());
                        }
                    }
                }
                values.sort();
                values.dedup();
                Ok(values)
            }
            Err(_) => Ok(Vec::new()),
        }
    }

    /// List all DNS records for a domain
    pub fn list_dns_records(&mut self, domain: &str) -> io::Result<Vec<DnsRecordData>> {
        let results = self
            .db
            .query()
            .collection("dns")
            .where_prop("domain", domain)
            .execute();

        Ok(match results {
            Ok(res) => res
                .items
                .into_iter()
                .filter_map(|item| {
                    let node = item.entity.data.as_node()?;
                    let value = node.get("value").and_then(|v| v.as_text())?.to_string();
                    let ttl = node.get("ttl").and_then(|v| v.as_integer()).unwrap_or(0) as u32;
                    let record_type = node
                        .get("type")
                        .and_then(|v| v.as_text())
                        .and_then(parse_dns_record_type)
                        .unwrap_or(DnsRecordType::A);
                    let timestamp = node
                        .get("timestamp")
                        .and_then(|v| v.as_integer())
                        .unwrap_or(0) as u32;

                    Some(DnsRecordData {
                        domain: domain.to_string(),
                        record_type,
                        value,
                        ttl,
                        timestamp,
                    })
                })
                .collect(),
            Err(_) => Vec::new(),
        })
    }

    /// List all HTTP records for a host
    pub fn list_http_records(&mut self, host: &str) -> io::Result<Vec<HttpHeadersRecord>> {
        let mut records = self.query_http_records(host, false)?;
        if records.is_empty() {
            records = self.query_http_records(host, true)?;
        }
        Ok(records)
    }

    /// Get WHOIS data for a specific domain
    pub fn get_whois(&mut self, domain: &str) -> io::Result<Option<WhoisRecord>> {
        let results = self
            .db
            .query()
            .collection("whois")
            .where_prop("domain", domain)
            .execute();

        match results {
            Ok(res) => {
                if let Some(item) = res.items.first() {
                    if let Some(node) = item.entity.data.as_node() {
                        let registrar = node
                            .get("registrar")
                            .and_then(|v| v.as_text())
                            .unwrap_or("")
                            .to_string();
                        return Ok(Some(WhoisRecord {
                            domain: domain.to_string(),
                            registrar,
                            created_date: 0,
                            expires_date: 0,
                            nameservers: Vec::new(),
                            timestamp: 0,
                        }));
                    }
                }
                Ok(None)
            }
            Err(_) => Ok(None),
        }
    }

    /// Get specific port status
    pub fn get_port_status(&mut self, ip: IpAddr, port: u16) -> io::Result<Option<PortStatus>> {
        let results = self
            .db
            .query()
            .collection("ports")
            .where_prop("ip", ip.to_string())
            .where_prop("port", port as i64)
            .execute();

        match results {
            Ok(res) => {
                if let Some(item) = res.items.first() {
                    if let Some(node) = item.entity.data.as_node() {
                        if let Some(state) = node.get("state").and_then(|v| v.as_text()) {
                            return Ok(Some(match state {
                                "open" => PortStatus::Open,
                                "closed" => PortStatus::Closed,
                                "filtered" => PortStatus::Filtered,
                                _ => PortStatus::Open,
                            }));
                        }
                    }
                }
                Ok(None)
            }
            Err(_) => Ok(None),
        }
    }

    /// Get stored host fingerprint
    pub fn get_host_fingerprint(&mut self, ip: IpAddr) -> io::Result<Option<HostIntelRecord>> {
        let results = self
            .db
            .query()
            .collection("hosts")
            .where_prop("ip", ip.to_string())
            .execute();

        match results {
            Ok(res) => {
                if let Some(item) = res.items.first() {
                    if let Some(node) = item.entity.data.as_node() {
                        let os = node
                            .get("os")
                            .and_then(|v| v.as_text())
                            .map(|s| s.to_string());
                        return Ok(Some(HostIntelRecord {
                            ip,
                            os_family: os,
                            confidence: 0.0,
                            last_seen: 0,
                            services: Vec::new(),
                        }));
                    }
                }
                Ok(None)
            }
            Err(_) => Ok(None),
        }
    }

    /// List all stored host fingerprints
    pub fn list_hosts(&mut self) -> io::Result<Vec<HostIntelRecord>> {
        let results = self.db.query().collection("hosts").execute();

        match results {
            Ok(res) => {
                let mut hosts = Vec::new();
                for item in res.items {
                    if let Some(node) = item.entity.data.as_node() {
                        if let Some(ip_str) = node.get("ip").and_then(|v| v.as_text()) {
                            if let Ok(ip) = ip_str.parse() {
                                hosts.push(HostIntelRecord {
                                    ip,
                                    os_family: node
                                        .get("os")
                                        .and_then(|v| v.as_text())
                                        .map(|s| s.to_string()),
                                    confidence: 0.0,
                                    last_seen: 0,
                                    services: Vec::new(),
                                });
                            }
                        }
                    }
                }
                Ok(hosts)
            }
            Err(_) => Ok(Vec::new()),
        }
    }

    /// List TLS scans for a given host
    pub fn list_tls_scans(&mut self, host: &str) -> io::Result<Vec<TlsScanRecord>> {
        let results = self
            .db
            .query()
            .collection("tls")
            .where_prop("host", host)
            .execute();

        Ok(match results {
            Ok(res) => res
                .items
                .into_iter()
                .filter_map(|item| {
                    let node = item.entity.data.as_node()?;
                    let port = node.get("port").and_then(|v| v.as_integer()).unwrap_or(0) as u16;
                    let version = node
                        .get("version")
                        .and_then(|v| v.as_text())
                        .map(|s| s.to_string());
                    let cipher = node
                        .get("cipher")
                        .and_then(|v| v.as_text())
                        .map(|s| s.to_string());
                    let certificate_valid = node
                        .get("certificate_valid")
                        .and_then(|v| v.as_boolean())
                        .unwrap_or(false);
                    let timestamp = node
                        .get("timestamp")
                        .and_then(|v| v.as_integer())
                        .unwrap_or(0) as u32;

                    Some(TlsScanRecord {
                        host: host.to_string(),
                        port,
                        timestamp,
                        negotiated_version: version,
                        negotiated_cipher: cipher,
                        negotiated_cipher_code: None,
                        negotiated_cipher_strength:
                            crate::storage::records::TlsCipherStrength::Strong,
                        certificate_valid,
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
                    })
                })
                .collect(),
            Err(_) => Vec::new(),
        })
    }

    /// Get the most recent TLS scan for a host
    pub fn latest_tls_scan(&mut self, host: &str) -> io::Result<Option<TlsScanRecord>> {
        let mut scans = self.list_tls_scans(host)?;
        scans.sort_by_key(|scan| scan.timestamp);
        Ok(scans.pop())
    }

    /// List all port scan records
    pub fn list_port_scans(&mut self) -> io::Result<Vec<crate::storage::records::PortScanRecord>> {
        let results = self.db.query().collection("ports").execute();

        Ok(match results {
            Ok(res) => res
                .items
                .into_iter()
                .filter_map(|item| {
                    let node = item.entity.data.as_node()?;
                    let ip_str = node.get("ip").and_then(|v| v.as_text())?;
                    let ip = ip_str.parse().ok()?;
                    let port = node.get("port").and_then(|v| v.as_integer())? as u16;
                    let status = node
                        .get("state")
                        .and_then(|v| v.as_text())
                        .and_then(parse_port_status)
                        .unwrap_or(PortStatus::Open);
                    let service_id = node
                        .get("service_id")
                        .and_then(|v| v.as_integer())
                        .unwrap_or(0) as u8;
                    let timestamp = node
                        .get("timestamp")
                        .and_then(|v| v.as_integer())
                        .unwrap_or(0) as u32;

                    Some(crate::storage::records::PortScanRecord {
                        ip,
                        port,
                        status,
                        service_id,
                        timestamp,
                    })
                })
                .collect(),
            Err(_) => Vec::new(),
        })
    }

    /// List all DNS records
    pub fn list_dns_records_all(&mut self) -> io::Result<Vec<DnsRecordData>> {
        let results = self.db.query().collection("dns").execute();

        Ok(match results {
            Ok(res) => res
                .items
                .into_iter()
                .filter_map(|item| {
                    let node = item.entity.data.as_node()?;
                    let domain = node.get("domain").and_then(|v| v.as_text())?.to_string();
                    let value = node.get("value").and_then(|v| v.as_text())?.to_string();
                    let ttl = node.get("ttl").and_then(|v| v.as_integer()).unwrap_or(0) as u32;
                    let record_type = node
                        .get("type")
                        .and_then(|v| v.as_text())
                        .and_then(parse_dns_record_type)
                        .unwrap_or(DnsRecordType::A);
                    let timestamp = node
                        .get("timestamp")
                        .and_then(|v| v.as_integer())
                        .unwrap_or(0) as u32;

                    Some(DnsRecordData {
                        domain,
                        record_type,
                        value,
                        ttl,
                        timestamp,
                    })
                })
                .collect(),
            Err(_) => Vec::new(),
        })
    }

    /// List subdomains for a domain
    pub fn list_subdomain_records(&mut self, domain: &str) -> io::Result<Vec<SubdomainRecord>> {
        let results = self
            .db
            .query()
            .collection("domains")
            .where_prop("parent", domain)
            .execute();

        Ok(match results {
            Ok(res) => res
                .items
                .into_iter()
                .filter_map(|item| parse_subdomain_record(item, domain))
                .collect(),
            Err(_) => Vec::new(),
        })
    }

    /// List all subdomain records
    pub fn list_subdomains_all(&mut self) -> io::Result<Vec<SubdomainRecord>> {
        let results = self.db.query().collection("domains").execute();
        Ok(match results {
            Ok(res) => res
                .items
                .into_iter()
                .filter_map(|item| parse_subdomain_record(item, ""))
                .collect(),
            Err(_) => Vec::new(),
        })
    }

    /// List vulnerability records
    pub fn list_vulnerabilities(
        &mut self,
    ) -> io::Result<Vec<crate::storage::records::VulnerabilityRecord>> {
        let results = self.db.query().collection("vulns").execute();

        Ok(match results {
            Ok(res) => res
                .items
                .into_iter()
                .filter_map(|item| {
                    let node = item.entity.data.as_node()?;
                    let cve_id = node.get("cve_id").and_then(|v| v.as_text())?.to_string();
                    let technology = node
                        .get("technology")
                        .and_then(|v| v.as_text())?
                        .to_string();
                    let description = node
                        .get("description")
                        .and_then(|v| v.as_text())
                        .unwrap_or("")
                        .to_string();
                    let source = node
                        .get("source")
                        .and_then(|v| v.as_text())
                        .unwrap_or("unknown")
                        .to_string();
                    let version = node
                        .get("version")
                        .and_then(|v| v.as_text())
                        .map(|s| s.to_string());
                    let cvss = node.get("cvss").and_then(|v| v.as_float()).unwrap_or(0.0) as f32;
                    let risk_score = node
                        .get("risk_score")
                        .and_then(|v| v.as_integer())
                        .unwrap_or(0) as u8;
                    let severity = node
                        .get("severity")
                        .and_then(|v| v.as_text())
                        .and_then(parse_severity)
                        .unwrap_or(RecordSeverity::Info);
                    let exploit_available = node
                        .get("exploit_available")
                        .and_then(|v| v.as_boolean())
                        .unwrap_or(false);
                    let in_kev = node
                        .get("in_kev")
                        .and_then(|v| v.as_boolean())
                        .unwrap_or(false);
                    let discovered_at = node
                        .get("discovered_at")
                        .and_then(|v| v.as_integer())
                        .unwrap_or(0) as u32;
                    let references = node
                        .get("references")
                        .and_then(|v| v.as_text())
                        .map(parse_references)
                        .unwrap_or_default();

                    Some(crate::storage::records::VulnerabilityRecord {
                        cve_id,
                        technology,
                        version,
                        cvss,
                        risk_score,
                        severity,
                        description,
                        references,
                        exploit_available,
                        in_kev,
                        discovered_at,
                        source,
                    })
                })
                .collect(),
            Err(_) => Vec::new(),
        })
    }

    /// List all stored actions
    pub fn list_actions(&mut self) -> io::Result<Vec<ActionRecord>> {
        let results = self.db.query().collection("actions").execute();

        Ok(match results {
            Ok(res) => res
                .items
                .into_iter()
                .filter_map(|item| {
                    let node = item.entity.data.as_node()?;
                    parse_action_record(node)
                })
                .collect(),
            Err(_) => Vec::new(),
        })
    }

    /// List stored actions for a target (matching host_str)
    pub fn list_actions_by_target(&mut self, target: &str) -> io::Result<Vec<ActionRecord>> {
        let actions = self.list_actions()?;
        Ok(actions
            .into_iter()
            .filter(|action| action.target.host_str() == target)
            .collect())
    }

    // ========================================================================
    // Proxy Data Query Methods
    // ========================================================================

    pub fn list_proxy_connections(&self) -> io::Result<Vec<ProxyConnectionRecord>> {
        // Placeholder
        Ok(Vec::new())
    }

    pub fn list_proxy_requests(&self) -> io::Result<Vec<ProxyHttpRequestRecord>> {
        Ok(Vec::new())
    }

    pub fn list_proxy_responses(&self) -> io::Result<Vec<ProxyHttpResponseRecord>> {
        Ok(Vec::new())
    }

    pub fn get_proxy_connection(
        &self,
        _connection_id: u64,
    ) -> io::Result<Option<ProxyConnectionRecord>> {
        Ok(None)
    }

    pub fn proxy_stats(&self) -> io::Result<ProxyStats> {
        Ok(ProxyStats::default())
    }

    /// Count records in a collection
    pub fn count_collection(&mut self, name: &str) -> io::Result<usize> {
        let results = self.db.query().collection(name).execute();
        Ok(results.map(|res| res.items.len()).unwrap_or(0))
    }
}

/// Statistics for proxy data
#[derive(Debug, Clone, Default)]
pub struct ProxyStats {
    pub connection_count: usize,
    pub request_count: usize,
    pub response_count: usize,
    pub total_bytes_sent: u64,
    pub total_bytes_received: u64,
    pub tls_intercepted_count: usize,
}

fn decode_err_to_io(err: DecodeError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err.0)
}

fn parse_port_status(status: &str) -> Option<PortStatus> {
    match status {
        "open" => Some(PortStatus::Open),
        "closed" => Some(PortStatus::Closed),
        "filtered" => Some(PortStatus::Filtered),
        "open|filtered" => Some(PortStatus::OpenFiltered),
        _ => None,
    }
}

fn parse_dns_record_type(record_type: &str) -> Option<DnsRecordType> {
    match record_type {
        "A" => Some(DnsRecordType::A),
        "AAAA" => Some(DnsRecordType::AAAA),
        "MX" => Some(DnsRecordType::MX),
        "NS" => Some(DnsRecordType::NS),
        "TXT" => Some(DnsRecordType::TXT),
        "CNAME" => Some(DnsRecordType::CNAME),
        _ => None,
    }
}

fn parse_subdomain_source(value: &str) -> Option<SubdomainSource> {
    match value {
        "DnsBruteforce" => Some(SubdomainSource::DnsBruteforce),
        "CertTransparency" => Some(SubdomainSource::CertTransparency),
        "SearchEngine" => Some(SubdomainSource::SearchEngine),
        "WebCrawl" => Some(SubdomainSource::WebCrawl),
        _ => None,
    }
}

fn parse_severity(value: &str) -> Option<RecordSeverity> {
    match value {
        "Critical" => Some(RecordSeverity::Critical),
        "High" => Some(RecordSeverity::High),
        "Medium" => Some(RecordSeverity::Medium),
        "Low" => Some(RecordSeverity::Low),
        "Info" => Some(RecordSeverity::Info),
        _ => None,
    }
}

fn parse_references(value: &str) -> Vec<String> {
    value
        .split('\n')
        .filter(|item| !item.is_empty())
        .map(|item| item.to_string())
        .collect()
}

fn parse_headers(headers: &str) -> Vec<(String, String)> {
    headers
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, ':');
            let key = parts.next()?.trim();
            let value = parts.next()?.trim();
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

fn parse_subdomain_record(
    item: crate::storage::QueryResultItem,
    domain_fallback: &str,
) -> Option<SubdomainRecord> {
    let node = item.entity.data.as_node()?;
    let subdomain = node.get("name").and_then(|v| v.as_text())?.to_string();
    let ips = node
        .get("ips")
        .and_then(|v| v.as_text())
        .map(|ips| {
            ips.split(',')
                .filter_map(|ip| ip.trim().parse().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let source = node
        .get("source")
        .and_then(|v| v.as_text())
        .and_then(parse_subdomain_source)
        .unwrap_or(SubdomainSource::SearchEngine);
    let timestamp = node
        .get("timestamp")
        .and_then(|v| v.as_integer())
        .unwrap_or(0) as u32;
    let _ = node
        .get("parent")
        .and_then(|v| v.as_text())
        .unwrap_or(domain_fallback);

    Some(SubdomainRecord {
        subdomain,
        ips,
        source,
        timestamp,
    })
}

fn parse_action_record(node: &crate::storage::NodeData) -> Option<ActionRecord> {
    let bytes = match node.get("record")? {
        Value::Blob(data) => data,
        _ => return None,
    };

    ActionRecord::decode(bytes).ok()
}

impl QueryManager {
    fn query_http_records(
        &mut self,
        host: &str,
        fallback: bool,
    ) -> io::Result<Vec<HttpHeadersRecord>> {
        let mut builder = self.db.query().collection("http");
        if fallback {
            builder = builder.where_prop_contains("url", host);
        } else {
            builder = builder.where_prop("host", host);
        }

        let results = builder.execute();
        Ok(match results {
            Ok(res) => res
                .items
                .into_iter()
                .filter_map(|item| {
                    let node = item.entity.data.as_node()?;
                    let url = node.get("url").and_then(|v| v.as_text())?.to_string();
                    let method = node
                        .get("method")
                        .and_then(|v| v.as_text())
                        .unwrap_or("GET")
                        .to_string();
                    let scheme = node
                        .get("scheme")
                        .and_then(|v| v.as_text())
                        .unwrap_or("http")
                        .to_string();
                    let http_version = node
                        .get("http_version")
                        .and_then(|v| v.as_text())
                        .unwrap_or("HTTP/1.1")
                        .to_string();
                    let status_code =
                        node.get("status").and_then(|v| v.as_integer()).unwrap_or(0) as u16;
                    let status_text = node
                        .get("status_text")
                        .and_then(|v| v.as_text())
                        .unwrap_or("")
                        .to_string();
                    let server = node
                        .get("server")
                        .and_then(|v| v.as_text())
                        .map(|s| s.to_string());
                    let body_size = node
                        .get("body_size")
                        .and_then(|v| v.as_integer())
                        .unwrap_or(0) as u32;
                    let headers = node
                        .get("headers")
                        .and_then(|v| v.as_text())
                        .map(parse_headers)
                        .unwrap_or_default();
                    let timestamp = node
                        .get("timestamp")
                        .and_then(|v| v.as_integer())
                        .unwrap_or(0) as u32;
                    let host_val = node
                        .get("host")
                        .and_then(|v| v.as_text())
                        .unwrap_or(host)
                        .to_string();

                    Some(HttpHeadersRecord {
                        host: host_val,
                        url,
                        method,
                        scheme,
                        http_version,
                        status_code,
                        status_text,
                        server,
                        body_size,
                        headers,
                        timestamp,
                        tls: None,
                    })
                })
                .collect(),
            Err(_) => Vec::new(),
        })
    }
}

/// Format helpers for displaying query results
pub mod format {
    use super::*;

    pub fn format_ports(ports: &[u16]) -> String {
        if ports.is_empty() {
            return "No open ports found".to_string();
        }

        let mut result = format!("Open Ports ({})\n", ports.len());
        result.push_str("━━━━━━━━━━━━━━━\n");

        for port in ports {
            result.push_str(&format!("  {}  \n", port));
        }

        result
    }

    pub fn format_subdomains(subdomains: &[String]) -> String {
        if subdomains.is_empty() {
            return "No subdomains found".to_string();
        }

        let mut result = format!("Subdomains ({})\n", subdomains.len());
        result.push_str("━━━━━━━━━━━━━━━\n");

        for subdomain in subdomains {
            result.push_str(&format!("  • {}\n", subdomain));
        }

        result
    }

    pub fn format_whois(record: &WhoisRecord) -> String {
        format!(
            "WHOIS Record\n\u{200b}             ━━━━━━━━━━━━\n\u{200b}             Registrar: {}\n\u{200b}             Created:   {}\n\u{200b}             Expires:   {}\n\u{200b}             Nameservers:\n{}",
            record.registrar,
            record.created_date,
            record.expires_date,
            record
                .nameservers
                .iter()
                .map(|ns| format!("  • {}", ns))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }

    pub fn format_dns_records(records: &[DnsRecordData]) -> String {
        if records.is_empty() {
            return "No DNS records found".to_string();
        }

        let mut result = format!("DNS Records ({})\n", records.len());
        result.push_str("━━━━━━━━━━━━━━━\n");

        for record in records {
            result.push_str(&format!(
                "  {} {:?} (TTL: {})\n",
                record.domain, record.record_type, record.ttl
            ));
        }

        result
    }

    pub fn format_host(record: &HostIntelRecord) -> String {
        let mut result = String::new();
        result.push_str("Host Fingerprint\n");
        result.push_str("━━━━━━━━━━━━━━━━\n");
        result.push_str(&format!("IP Address: {}\n", record.ip));
        if let Some(os) = &record.os_family {
            result.push_str(&format!(
                "OS Guess:   {} ({:.0}% confidence)\n",
                os,
                (record.confidence * 100.0).round()
            ));
        } else {
            result.push_str("OS Guess:   unknown\n");
        }
        result.push_str(&format!("Last Seen:  {}\n", record.last_seen));
        result.push_str(&format!("Services ({})\n", record.services.len()));
        result.push_str("━━━━━━━━━━━━━━━━━━━━\n");
        for service in &record.services {
            result.push_str(&format!("Port {:<5}", service.port));
            if let Some(name) = &service.service_name {
                result.push(' ');
                result.push_str(name);
            }
            result.push('\n');
            if let Some(banner) = &service.banner {
                result.push_str(&format!("  Banner: {}\n", banner));
            }
            for hint in &service.os_hints {
                result.push_str(&format!("  Hint:   {}\n", hint));
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    #[ignore = "requires RUST_MIN_STACK=8388608"]
    fn query_manager_compiles() {
        // Placeholder to ensure module compiles; integration tests live elsewhere.
        let _ = QueryManager::open("/tmp/non-existent.json");
    }

    #[test]
    fn format_ports_output() {
        let ports = vec![22, 80, 443];
        let formatted = format::format_ports(&ports);
        assert!(formatted.contains("Open Ports (3)"));
        assert!(formatted.contains("22"));
        assert!(formatted.contains("80"));
        assert!(formatted.contains("443"));
    }

    #[test]
    fn format_host_output() {
        let record = HostIntelRecord {
            ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
            os_family: Some("linux".into()),
            confidence: 0.85,
            last_seen: 1_700_000_000,
            services: Vec::new(),
        };
        let formatted = format::format_host(&record);
        assert!(formatted.contains("linux"));
    }
}
