//! Domain-Centric Intelligence
//!
//! Answers: "What subdomains exist? What DNS records? What services?"

use std::collections::HashSet;

use crate::storage::segments::graph::{EdgeType, GraphSegment};

/// Profile of a domain and its infrastructure
#[derive(Debug, Clone)]
pub struct DomainProfile {
    pub domain: String,
    pub subdomains: Vec<SubdomainInfo>,
    pub dns_records: Vec<DnsRecordInfo>,
    pub hosts: Vec<String>,
    pub services: Vec<(String, String)>, // (host:port, service_name)
    pub technologies: Vec<String>,
    pub findings: Vec<DomainFinding>,
}

impl DomainProfile {
    pub fn display(&self) -> String {
        let mut s = String::new();
        s.push_str("┌─────────────────────────────────────────────────────────────────┐\n");
        s.push_str(&format!("│  DOMAIN: {:<53} │\n", self.domain));
        s.push_str("├─────────────────────────────────────────────────────────────────┤\n");
        s.push_str(&format!("│  SUBDOMAINS: {:<49} │\n", self.subdomains.len()));
        s.push_str(&format!("│  HOSTS: {:<54} │\n", self.hosts.len()));

        if !self.subdomains.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str("│  SUBDOMAIN TREE:                                                │\n");
            for sub in self.subdomains.iter().take(8) {
                let status = if sub.is_active { "●" } else { "○" };
                let ip_str = sub.ip.as_deref().unwrap_or("-");
                let sub_str = format!("{} {} → {}", status, sub.name, ip_str);
                s.push_str(&format!("│    {:<57} │\n", sub_str));
            }
            if self.subdomains.len() > 8 {
                s.push_str(&format!(
                    "│    ... and {} more                                          │\n",
                    self.subdomains.len() - 8
                ));
            }
        }

        if !self.dns_records.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str("│  DNS RECORDS:                                                   │\n");
            for rec in self.dns_records.iter().take(5) {
                let rec_str = format!("{}: {}", rec.record_type, rec.value);
                s.push_str(&format!(
                    "│    • {:<55} │\n",
                    if rec_str.len() > 55 {
                        format!("{}...", &rec_str[..52])
                    } else {
                        rec_str
                    }
                ));
            }
        }

        if !self.findings.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str("│  FINDINGS:                                                      │\n");
            for finding in &self.findings {
                let severity = match finding.severity {
                    FindingSeverity::Critical => "CRIT",
                    FindingSeverity::High => "HIGH",
                    FindingSeverity::Medium => "MED",
                    FindingSeverity::Low => "LOW",
                    FindingSeverity::Info => "INFO",
                };
                let finding_str = format!("[{}] {}", severity, finding.title);
                s.push_str(&format!(
                    "│    • {:<55} │\n",
                    if finding_str.len() > 55 {
                        format!("{}...", &finding_str[..52])
                    } else {
                        finding_str
                    }
                ));
            }
        }

        s.push_str("└─────────────────────────────────────────────────────────────────┘\n");
        s
    }
}

/// Information about a subdomain
#[derive(Debug, Clone)]
pub struct SubdomainInfo {
    pub name: String,
    pub ip: Option<String>,
    pub is_active: bool,
    pub services: Vec<String>,
}

/// DNS record information
#[derive(Debug, Clone)]
pub struct DnsRecordInfo {
    pub record_type: String, // A, AAAA, MX, NS, TXT, CNAME, etc.
    pub value: String,
    pub ttl: Option<u32>,
}

/// Security finding for a domain
#[derive(Debug, Clone)]
pub struct DomainFinding {
    pub title: String,
    pub severity: FindingSeverity,
    pub description: String,
}

/// Finding severity level
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FindingSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

/// Domain-centric intelligence queries
pub struct DomainIntelligence<'a> {
    graph: &'a GraphSegment,
}

impl<'a> DomainIntelligence<'a> {
    pub fn new(graph: &'a GraphSegment) -> Self {
        Self { graph }
    }

    /// Get complete domain profile
    pub fn profile(&self, domain: &str) -> Option<DomainProfile> {
        let subdomains = self.subdomains(domain);
        let hosts = self.hosts(domain);

        if subdomains.is_empty() && hosts.is_empty() {
            // Try to find any related data
            let domain_lower = domain.to_lowercase();
            let has_data = self.graph.all_nodes().iter().any(|n| {
                n.label.to_lowercase().contains(&domain_lower)
                    || n.id.to_lowercase().contains(&domain_lower)
            });

            if !has_data {
                return None;
            }
        }

        let dns_records = self.dns_records(domain);
        let services = self.services(domain);
        let technologies = self.technologies(domain);
        let findings = self.analyze_findings(domain);

        Some(DomainProfile {
            domain: domain.to_string(),
            subdomains,
            dns_records,
            hosts,
            services,
            technologies,
            findings,
        })
    }

    /// Find all subdomains
    pub fn subdomains(&self, domain: &str) -> Vec<SubdomainInfo> {
        let domain_lower = domain.to_lowercase();
        let mut subdomains = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        // Look through all nodes for domain references
        for node in self.graph.all_nodes() {
            let label_lower = node.label.to_lowercase();
            let id_lower = node.id.to_lowercase();

            // Check if this looks like a subdomain
            if label_lower.ends_with(&domain_lower) || id_lower.contains(&domain_lower) {
                let subdomain_name = if label_lower.ends_with(&domain_lower) {
                    node.label.clone()
                } else {
                    // Extract domain from ID
                    extract_domain_from_id(&node.id).unwrap_or_default()
                };

                if !subdomain_name.is_empty() && !seen.contains(&subdomain_name.to_lowercase()) {
                    seen.insert(subdomain_name.to_lowercase());

                    // Try to find associated IP
                    let ip = self.find_ip_for_subdomain(&node.id);

                    // Check if active (has services or connections)
                    let is_active = !node.out_edges.is_empty() || !node.in_edges.is_empty();

                    subdomains.push(SubdomainInfo {
                        name: subdomain_name,
                        ip,
                        is_active,
                        services: vec![],
                    });
                }
            }
        }

        subdomains.sort_by(|a, b| a.name.cmp(&b.name));
        subdomains
    }

    /// Find IP associated with a subdomain node
    fn find_ip_for_subdomain(&self, node_id: &str) -> Option<String> {
        if let Some(node) = self.graph.get_node(node_id) {
            for edge in &node.out_edges {
                if edge.target_id.starts_with("host:") {
                    return Some(edge.target_id.trim_start_matches("host:").to_string());
                }
            }
            for edge in &node.in_edges {
                if edge.target_id.starts_with("host:") {
                    return Some(edge.target_id.trim_start_matches("host:").to_string());
                }
            }
        }
        None
    }

    /// Get DNS records for domain
    pub fn dns_records(&self, domain: &str) -> Vec<DnsRecordInfo> {
        let domain_lower = domain.to_lowercase();
        let mut records = Vec::new();

        // Look for DNS record nodes
        for node in self.graph.all_nodes() {
            if node.id.starts_with("dns:") || node.label.to_lowercase().contains(&domain_lower) {
                // Parse DNS record from label or metadata
                if let Some(record) = parse_dns_record(&node.label) {
                    records.push(record);
                }
            }
        }

        records
    }

    /// Find all hosts associated with domain
    pub fn hosts(&self, domain: &str) -> Vec<String> {
        let domain_lower = domain.to_lowercase();
        let mut hosts: HashSet<String> = HashSet::new();

        for node in self.graph.all_nodes() {
            // Check if node references the domain
            if node.label.to_lowercase().contains(&domain_lower)
                || node.id.to_lowercase().contains(&domain_lower)
            {
                // Find connected hosts
                for edge in &node.out_edges {
                    if edge.target_id.starts_with("host:") {
                        hosts.insert(edge.target_id.trim_start_matches("host:").to_string());
                    }
                }
                for edge in &node.in_edges {
                    if edge.target_id.starts_with("host:") {
                        hosts.insert(edge.target_id.trim_start_matches("host:").to_string());
                    }
                }
            }

            // Also check if it's a host node with domain in label
            if node.id.starts_with("host:") && node.label.to_lowercase().contains(&domain_lower) {
                hosts.insert(node.id.trim_start_matches("host:").to_string());
            }
        }

        let mut result: Vec<String> = hosts.into_iter().collect();
        result.sort();
        result
    }

    /// Get services running on domain infrastructure
    pub fn services(&self, domain: &str) -> Vec<(String, String)> {
        let hosts = self.hosts(domain);
        let mut services = Vec::new();

        for host in hosts {
            if let Some(node) = self.graph.get_node(&format!("host:{}", host)) {
                for edge in &node.out_edges {
                    if edge.edge_type == EdgeType::HasService {
                        if let Some(svc_node) = self.graph.get_node(&edge.target_id) {
                            // Extract port from service ID (service:ip:port:name)
                            let parts: Vec<&str> = edge.target_id.split(':').collect();
                            let port = if parts.len() >= 3 { parts[2] } else { "?" };
                            services.push((format!("{}:{}", host, port), svc_node.label.clone()));
                        }
                    }
                }
            }
        }

        services
    }

    /// Get technologies used across domain
    pub fn technologies(&self, domain: &str) -> Vec<String> {
        let hosts = self.hosts(domain);
        let mut techs: HashSet<String> = HashSet::new();

        for host in hosts {
            if let Some(node) = self.graph.get_node(&format!("host:{}", host)) {
                for edge in &node.out_edges {
                    if edge.edge_type == EdgeType::UsesTech {
                        if let Some(tech_node) = self.graph.get_node(&edge.target_id) {
                            techs.insert(tech_node.label.clone());
                        }
                    }
                }
            }
        }

        let mut result: Vec<String> = techs.into_iter().collect();
        result.sort();
        result
    }

    /// Analyze domain for security findings
    pub fn analyze_findings(&self, domain: &str) -> Vec<DomainFinding> {
        let mut findings = Vec::new();
        let subdomains = self.subdomains(domain);
        let hosts = self.hosts(domain);

        // Check for subdomain takeover candidates (inactive subdomains)
        let inactive: Vec<_> = subdomains
            .iter()
            .filter(|s| !s.is_active && s.ip.is_none())
            .collect();

        if !inactive.is_empty() {
            findings.push(DomainFinding {
                title: format!("{} potential subdomain takeover candidates", inactive.len()),
                severity: FindingSeverity::Medium,
                description:
                    "Subdomains pointing to non-existent resources may be vulnerable to takeover"
                        .to_string(),
            });
        }

        // Check for exposed services
        let services = self.services(domain);
        let sensitive_ports = ["22", "3389", "5432", "3306", "27017", "6379", "11211"];
        let exposed: Vec<_> = services
            .iter()
            .filter(|(addr, _)| {
                let port = addr.split(':').next_back().unwrap_or("");
                sensitive_ports.contains(&port)
            })
            .collect();

        if !exposed.is_empty() {
            findings.push(DomainFinding {
                title: format!("{} sensitive services exposed", exposed.len()),
                severity: FindingSeverity::High,
                description:
                    "Database and administrative services should not be publicly accessible"
                        .to_string(),
            });
        }

        // Sort by severity
        findings.sort_by(|a, b| b.severity.cmp(&a.severity));
        findings
    }

    /// Find related domains (same infrastructure)
    pub fn related(&self, domain: &str) -> Vec<String> {
        let hosts = self.hosts(domain);
        let mut related: HashSet<String> = HashSet::new();

        for host in &hosts {
            if let Some(node) = self.graph.get_node(&format!("host:{}", host)) {
                // Find other domains pointing to same hosts
                for edge in &node.in_edges {
                    if let Some(src_node) = self.graph.get_node(&edge.target_id) {
                        if let Some(other_domain) = extract_domain_from_label(&src_node.label) {
                            if other_domain.to_lowercase() != domain.to_lowercase() {
                                related.insert(other_domain);
                            }
                        }
                    }
                }
            }
        }

        let mut result: Vec<String> = related.into_iter().collect();
        result.sort();
        result
    }

    /// Get all known domains
    pub fn all(&self) -> Vec<String> {
        let mut domains: HashSet<String> = HashSet::new();

        for node in self.graph.all_nodes() {
            if let Some(domain) = extract_domain_from_label(&node.label) {
                // Only add if it looks like a domain (has at least one dot)
                if domain.contains('.') && domain.parse::<std::net::Ipv4Addr>().is_err() {
                    domains.insert(domain);
                }
            }
        }

        let mut result: Vec<String> = domains.into_iter().collect();
        result.sort();
        result
    }
}

/// Extract domain from node ID
fn extract_domain_from_id(id: &str) -> Option<String> {
    let parts: Vec<&str> = id.split(':').collect();
    if parts.len() >= 2 {
        let potential = parts[1];
        if potential.contains('.') && potential.parse::<std::net::Ipv4Addr>().is_err() {
            return Some(potential.to_string());
        }
    }
    None
}

/// Extract domain from label
fn extract_domain_from_label(label: &str) -> Option<String> {
    // Simple heuristic: look for domain-like patterns
    let words: Vec<&str> = label.split_whitespace().collect();
    for word in words {
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '-');
        if clean.contains('.')
            && clean.parse::<std::net::Ipv4Addr>().is_err()
            && clean.chars().filter(|&c| c == '.').count() >= 1
        {
            // Check if it looks like a domain
            let parts: Vec<&str> = clean.split('.').collect();
            if parts.len() >= 2 && parts.last().is_some_and(|p| p.len() >= 2) {
                return Some(clean.to_string());
            }
        }
    }
    None
}

/// Parse DNS record from label
fn parse_dns_record(label: &str) -> Option<DnsRecordInfo> {
    let record_types = ["A", "AAAA", "MX", "NS", "TXT", "CNAME", "PTR", "SOA", "SRV"];

    for rt in record_types {
        if label.contains(rt) {
            return Some(DnsRecordInfo {
                record_type: rt.to_string(),
                value: label.replace(rt, "").trim().to_string(),
                ttl: None,
            });
        }
    }

    None
}
