//! Host-Centric Intelligence
//!
//! Answers: "What do I know about this host?"

use std::collections::HashMap;

use super::types::*;
use crate::modules::common::Severity;
use crate::storage::segments::graph::{EdgeType, GraphSegment, NodeType};

/// Complete profile of a discovered host
#[derive(Debug, Clone)]
pub struct HostProfile {
    pub ip: String,
    pub hostname: Option<String>,
    pub os: OsInfo,
    pub technologies: Vec<Technology>,
    pub ports: Vec<PortInfo>,
    pub users: Vec<DetectedUser>,
    pub vulnerabilities: Vec<VulnInfo>,
    pub attack_paths_in: usize,
    pub attack_paths_out: usize,
    pub pagerank: f64,
    pub community_id: Option<u32>,
}

impl HostProfile {
    /// Format as a display string
    pub fn display(&self) -> String {
        let mut s = String::new();
        s.push_str("┌─────────────────────────────────────────────────────────────────┐\n");
        s.push_str(&format!("│  HOST: {:<55} │\n", self.ip));
        s.push_str("├─────────────────────────────────────────────────────────────────┤\n");

        // OS info
        let os_str = if let Some(ref dist) = self.os.distribution {
            if let Some(ref ver) = self.os.version {
                format!("{} ({} {})", self.os.family.as_str(), dist, ver)
            } else {
                format!("{} ({})", self.os.family.as_str(), dist)
            }
        } else {
            self.os.family.as_str().to_string()
        };
        s.push_str(&format!("│  OS: {:<57} │\n", os_str));

        // Technologies
        if !self.technologies.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str("│  TECHNOLOGIES:                                                  │\n");
            for tech in self.technologies.iter().take(5) {
                let ver = tech.version.as_deref().unwrap_or("");
                let name = if ver.is_empty() {
                    tech.name.clone()
                } else {
                    format!("{} {}", tech.name, ver)
                };
                s.push_str(&format!("│    • {:<55} │\n", name));
            }
            if self.technologies.len() > 5 {
                s.push_str(&format!(
                    "│    ... and {} more                                          │\n",
                    self.technologies.len() - 5
                ));
            }
        }

        // Open ports
        if !self.ports.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str("│  OPEN PORTS:                                                    │\n");
            for port in self.ports.iter().take(5) {
                let ver = port.version.as_deref().unwrap_or("");
                let port_str = format!("{}/{} {} {}", port.port, port.protocol, port.service, ver);
                s.push_str(&format!("│    {:<57} │\n", port_str));
            }
            if self.ports.len() > 5 {
                s.push_str(&format!(
                    "│    ... and {} more                                          │\n",
                    self.ports.len() - 5
                ));
            }
        }

        // Users
        if !self.users.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str("│  DETECTED USERS:                                                │\n");
            for user in self.users.iter().take(4) {
                let cred = if user.has_credential {
                    "(credentials known)"
                } else {
                    ""
                };
                let user_str = format!("{} {} - {}", user.username, cred, user.services.join(", "));
                s.push_str(&format!("│    • {:<55} │\n", user_str));
            }
        }

        // Vulnerabilities
        if !self.vulnerabilities.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str("│  VULNERABILITIES:                                               │\n");
            for vuln in self.vulnerabilities.iter().take(4) {
                let cve = vuln.cve.as_deref().unwrap_or("N/A");
                let vuln_str = format!("{} - {}", cve, vuln.severity.as_str().to_uppercase());
                s.push_str(&format!("│    • {:<55} │\n", vuln_str));
            }
        }

        // Attack paths
        s.push_str("│                                                                 │\n");
        s.push_str(&format!(
            "│  ATTACK PATHS TO THIS HOST: {:<34} │\n",
            self.attack_paths_in
        ));
        s.push_str(&format!(
            "│  REACHABLE FROM THIS HOST: {:<35} │\n",
            self.attack_paths_out
        ));

        s.push_str("└─────────────────────────────────────────────────────────────────┘\n");
        s
    }
}

/// A user detected on a host
#[derive(Debug, Clone)]
pub struct DetectedUser {
    pub username: String,
    pub has_credential: bool,
    pub services: Vec<String>,
    pub privilege_level: PrivilegeLevel,
}

/// Host-centric intelligence queries
pub struct HostIntelligence<'a> {
    graph: &'a GraphSegment,
}

impl<'a> HostIntelligence<'a> {
    pub fn new(graph: &'a GraphSegment) -> Self {
        Self { graph }
    }

    /// Get complete host profile
    pub fn profile(&self, host: &str) -> Option<HostProfile> {
        // Normalize host ID
        let host_id = if host.starts_with("host:") {
            host.to_string()
        } else {
            format!("host:{}", host)
        };

        let node = self.graph.get_node(&host_id)?;
        let pagerank_scores = self.compute_pagerank_scores();
        let communities = self.community_assignments();

        Some(HostProfile {
            ip: host.trim_start_matches("host:").to_string(),
            hostname: self.resolve_hostname(node),
            os: self.detect_os(&host_id),
            technologies: self.technologies(&host_id),
            ports: self.ports(&host_id),
            users: self.users(&host_id),
            vulnerabilities: self.vulns(&host_id),
            attack_paths_in: self.count_incoming_paths(&host_id),
            attack_paths_out: self.count_outgoing_paths(&host_id),
            pagerank: pagerank_scores.get(&host_id).copied().unwrap_or(0.0),
            community_id: communities.get(&host_id).copied(),
        })
    }

    fn node_ids(&self) -> Vec<String> {
        self.graph
            .all_node_ids()
            .iter()
            .map(|id| (*id).clone())
            .collect()
    }

    fn compute_pagerank_scores(&self) -> HashMap<String, f64> {
        let nodes = self.node_ids();
        if nodes.is_empty() {
            return HashMap::new();
        }

        let n = nodes.len() as f64;
        let alpha = 0.85_f64;
        let epsilon = 1e-6_f64;
        let max_iterations = 50_usize;

        let mut outgoing: HashMap<String, Vec<String>> = HashMap::new();
        for node_id in &nodes {
            if let Some(node) = self.graph.get_node(node_id) {
                outgoing.insert(
                    node_id.clone(),
                    node.out_edges
                        .iter()
                        .map(|edge| edge.target_id.clone())
                        .collect(),
                );
            } else {
                outgoing.insert(node_id.clone(), Vec::new());
            }
        }

        let mut scores: HashMap<String, f64> = nodes
            .iter()
            .map(|node_id| (node_id.clone(), 1.0 / n))
            .collect();

        for _ in 0..max_iterations {
            let mut next = HashMap::new();
            for node_id in &nodes {
                let mut score = (1.0 - alpha) / n;

                for (source_id, targets) in &outgoing {
                    if targets.contains(node_id) {
                        let source_score = scores.get(source_id).copied().unwrap_or(0.0);
                        if !targets.is_empty() {
                            score += alpha * source_score / targets.len() as f64;
                        }
                    }
                }

                next.insert(node_id.clone(), score);
            }

            let dangling_sum = nodes
                .iter()
                .filter(|id| {
                    outgoing
                        .get(*id)
                        .map(|targets| targets.is_empty())
                        .unwrap_or(true)
                })
                .filter_map(|id| scores.get(id))
                .sum::<f64>();
            let dangling = alpha * dangling_sum / n;
            for score in next.values_mut() {
                *score += dangling;
            }

            let diff: f64 = nodes
                .iter()
                .map(|id| {
                    let old = scores.get(id).copied().unwrap_or(0.0);
                    let updated = next.get(id).copied().unwrap_or(0.0);
                    (old - updated).abs()
                })
                .sum();

            scores = next;
            if diff < epsilon {
                break;
            }
        }

        scores
    }

    fn community_assignments(&self) -> HashMap<String, u32> {
        let nodes = self.node_ids();
        if nodes.is_empty() {
            return HashMap::new();
        }

        let mut labels: HashMap<String, String> = nodes
            .iter()
            .map(|node_id| (node_id.clone(), node_id.clone()))
            .collect();

        for _ in 0..30 {
            let mut changed = false;
            for node_id in &nodes {
                let mut counts: HashMap<String, usize> = HashMap::new();

                if let Some(node) = self.graph.get_node(node_id) {
                    for edge in node.out_edges.iter().chain(node.in_edges.iter()) {
                        if let Some(label) = labels.get(&edge.target_id) {
                            *counts.entry(label.clone()).or_insert(0) += 1;
                        }
                    }
                }

                if counts.is_empty() {
                    continue;
                }

                let best_label = counts
                    .into_iter()
                    .max_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
                    .map(|(label, _)| label);

                if let Some(next_label) = best_label {
                    if labels.get(node_id) != Some(&next_label) {
                        labels.insert(node_id.clone(), next_label);
                        changed = true;
                    }
                }
            }

            if !changed {
                break;
            }
        }

        let mut groups: HashMap<String, Vec<String>> = HashMap::new();
        for (node_id, label) in labels {
            groups.entry(label).or_default().push(node_id);
        }

        let mut ordered: Vec<(String, Vec<String>)> = groups.into_iter().collect();
        ordered.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));

        let mut assignments = HashMap::new();
        for (index, (_label, members)) in ordered.into_iter().enumerate() {
            let community_id = index as u32;
            for member in members {
                assignments.insert(member, community_id);
            }
        }

        assignments
    }

    /// Resolve hostname from graph metadata
    fn resolve_hostname(
        &self,
        node: &crate::storage::segments::graph::GraphNode,
    ) -> Option<String> {
        // Check if label contains a hostname
        if node.label.contains('.') && !node.label.chars().all(|c| c.is_ascii_digit() || c == '.') {
            Some(node.label.clone())
        } else {
            None
        }
    }

    /// Detect OS from graph signals
    pub fn detect_os(&self, host: &str) -> OsInfo {
        let host_id = if host.starts_with("host:") {
            host.to_string()
        } else {
            format!("host:{}", host)
        };

        let mut signals: Vec<(OsFamily, f32)> = Vec::new();
        let mut distribution: Option<String> = None;
        let mut version: Option<String> = None;

        if let Some(node) = self.graph.get_node(&host_id) {
            // Check technology nodes for OS indicators
            for edge in &node.out_edges {
                if edge.edge_type == EdgeType::UsesTech {
                    if let Some(tech_node) = self.graph.get_node(&edge.target_id) {
                        let label = tech_node.label.to_lowercase();

                        // Check for specific OS indicators
                        if label.contains("ubuntu") {
                            signals.push((OsFamily::Linux, 0.95));
                            distribution = Some("Ubuntu".to_string());
                            // Try to extract version
                            if let Some(ver) = extract_version(&tech_node.label) {
                                version = Some(ver);
                            }
                        } else if label.contains("debian") {
                            signals.push((OsFamily::Linux, 0.95));
                            distribution = Some("Debian".to_string());
                        } else if label.contains("centos") || label.contains("rhel") {
                            signals.push((OsFamily::Linux, 0.95));
                            distribution = Some("CentOS/RHEL".to_string());
                        } else if label.contains("windows") || label.contains("microsoft-iis") {
                            signals.push((OsFamily::Windows, 0.95));
                            if label.contains("iis") {
                                // IIS version mapping
                                if label.contains("10") {
                                    distribution = Some("Windows Server 2016+".to_string());
                                } else if label.contains("8.5") {
                                    distribution = Some("Windows Server 2012 R2".to_string());
                                }
                            }
                        } else if label.contains("openssh") {
                            // OpenSSH is more common on Linux, but not definitive
                            signals.push((OsFamily::Linux, 0.6));
                        }
                    }
                }
            }

            // Check services for OS hints
            for edge in &node.out_edges {
                if edge.edge_type == EdgeType::HasService {
                    if let Some(svc_node) = self.graph.get_node(&edge.target_id) {
                        let label = svc_node.label.to_lowercase();

                        // Windows-specific ports
                        if label.contains(":3389") || label.contains("rdp") {
                            signals.push((OsFamily::Windows, 0.85));
                        }
                        if label.contains(":135") || label.contains(":445") {
                            signals.push((OsFamily::Windows, 0.8));
                        }

                        // Linux-specific patterns
                        if label.contains("openssh") {
                            signals.push((OsFamily::Linux, 0.6));
                        }
                    }
                }
            }
        }

        // Aggregate signals with weighted voting
        if signals.is_empty() {
            return OsInfo::default();
        }

        let mut votes: HashMap<OsFamily, f32> = HashMap::new();
        for (family, weight) in &signals {
            *votes.entry(*family).or_insert(0.0) += weight;
        }

        let (best_family, total_weight) = votes
            .iter()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(k, v)| (*k, *v))
            .unwrap_or((OsFamily::Unknown, 0.0));

        let confidence = (total_weight / signals.len() as f32).min(1.0);

        OsInfo {
            family: best_family,
            distribution,
            version,
            kernel: None,
            confidence,
        }
    }

    /// Get all technologies detected on a host
    pub fn technologies(&self, host: &str) -> Vec<Technology> {
        let host_id = if host.starts_with("host:") {
            host.to_string()
        } else {
            format!("host:{}", host)
        };

        let mut techs = Vec::new();

        if let Some(node) = self.graph.get_node(&host_id) {
            for edge in &node.out_edges {
                if edge.edge_type == EdgeType::UsesTech {
                    if let Some(tech_node) = self.graph.get_node(&edge.target_id) {
                        let name = tech_node.label.clone();
                        let version = extract_version(&name);
                        let base_name = if let Some(ref v) = version {
                            name.replace(v, "").trim().to_string()
                        } else {
                            name.clone()
                        };

                        techs.push(Technology {
                            name: base_name,
                            version,
                            category: TechCategory::from_name(&name),
                            source: DetectionSource::Fingerprint,
                        });
                    }
                }
            }
        }

        techs
    }

    /// Get all open ports on a host
    pub fn ports(&self, host: &str) -> Vec<PortInfo> {
        let host_id = if host.starts_with("host:") {
            host.to_string()
        } else {
            format!("host:{}", host)
        };

        let mut ports = Vec::new();

        if let Some(node) = self.graph.get_node(&host_id) {
            for edge in &node.out_edges {
                if edge.edge_type == EdgeType::HasService {
                    if let Some(svc_node) = self.graph.get_node(&edge.target_id) {
                        // Parse service ID: "service:ip:port:name"
                        let parts: Vec<&str> = svc_node.id.split(':').collect();
                        if parts.len() >= 4 {
                            if let Ok(port) = parts[2].parse::<u16>() {
                                let service = parts[3..].join(":");
                                let version = extract_version(&svc_node.label);

                                ports.push(PortInfo {
                                    port,
                                    protocol: "tcp".to_string(),
                                    service,
                                    version,
                                    banner: None,
                                });
                            }
                        }
                    }
                }
            }
        }

        // Sort by port number
        ports.sort_by_key(|p| p.port);
        ports
    }

    /// Get all detected users on a host
    pub fn users(&self, host: &str) -> Vec<DetectedUser> {
        let host_id = if host.starts_with("host:") {
            host.to_string()
        } else {
            format!("host:{}", host)
        };

        let mut users: HashMap<String, DetectedUser> = HashMap::new();

        if let Some(node) = self.graph.get_node(&host_id) {
            // Check credentials contained by this host
            for edge in &node.out_edges {
                if edge.edge_type == EdgeType::Contains {
                    if let Some(cred_node) = self.graph.get_node(&edge.target_id) {
                        // Extract username from credential label
                        let username = cred_node
                            .label
                            .trim_start_matches("Creds: ")
                            .split(':')
                            .next()
                            .unwrap_or(&cred_node.label)
                            .to_string();

                        let user = users.entry(username.clone()).or_insert(DetectedUser {
                            username: username.clone(),
                            has_credential: true,
                            services: Vec::new(),
                            privilege_level: PrivilegeLevel::from_username(&username),
                        });
                        user.has_credential = true;
                    }
                }
            }

            // Check incoming auth_access edges (credentials that access this host)
            for edge in &node.in_edges {
                if edge.edge_type == EdgeType::AuthAccess {
                    if let Some(cred_node) = self.graph.get_node(&edge.target_id) {
                        let username = cred_node
                            .label
                            .trim_start_matches("Creds: ")
                            .split(':')
                            .next()
                            .unwrap_or(&cred_node.label)
                            .to_string();

                        let user = users.entry(username.clone()).or_insert(DetectedUser {
                            username: username.clone(),
                            has_credential: true,
                            services: Vec::new(),
                            privilege_level: PrivilegeLevel::from_username(&username),
                        });
                        user.has_credential = true;
                    }
                }
            }
        }

        users.into_values().collect()
    }

    /// Get vulnerabilities affecting a host
    pub fn vulns(&self, host: &str) -> Vec<VulnInfo> {
        let host_id = if host.starts_with("host:") {
            host.to_string()
        } else {
            format!("host:{}", host)
        };

        let mut vulns = Vec::new();

        if let Some(node) = self.graph.get_node(&host_id) {
            for edge in &node.out_edges {
                if edge.edge_type == EdgeType::AffectedBy {
                    if let Some(vuln_node) = self.graph.get_node(&edge.target_id) {
                        // Parse vulnerability info from node
                        let cve = if vuln_node.id.starts_with("vuln:CVE-") {
                            Some(vuln_node.id.trim_start_matches("vuln:").to_string())
                        } else {
                            None
                        };

                        // Try to extract severity from label
                        let severity = if vuln_node.label.contains("Critical") {
                            Severity::Critical
                        } else if vuln_node.label.contains("High") {
                            Severity::High
                        } else if vuln_node.label.contains("Medium") {
                            Severity::Medium
                        } else if vuln_node.label.contains("Low") {
                            Severity::Low
                        } else {
                            Severity::Info
                        };

                        vulns.push(VulnInfo {
                            cve,
                            title: vuln_node.label.clone(),
                            cvss: match severity {
                                Severity::Critical => 9.5,
                                Severity::High => 7.5,
                                Severity::Medium => 5.0,
                                Severity::Low => 2.5,
                                Severity::Info => 0.0,
                            },
                            severity,
                            vuln_type: VulnType::Other,
                            exploitable: false,
                            description: None,
                        });
                    }
                }
            }
        }

        // Sort by severity (highest first)
        vulns.sort_by(|a, b| b.severity.cmp(&a.severity));
        vulns
    }

    /// Count incoming attack paths
    fn count_incoming_paths(&self, host_id: &str) -> usize {
        // Count credentials and vulnerabilities that can reach this host
        if let Some(node) = self.graph.get_node(host_id) {
            node.in_edges
                .iter()
                .filter(|e| {
                    e.edge_type == EdgeType::AuthAccess || e.edge_type == EdgeType::ConnectsTo
                })
                .count()
        } else {
            0
        }
    }

    /// Count outgoing attack paths
    fn count_outgoing_paths(&self, host_id: &str) -> usize {
        if let Some(node) = self.graph.get_node(host_id) {
            node.out_edges
                .iter()
                .filter(|e| {
                    e.edge_type == EdgeType::AuthAccess || e.edge_type == EdgeType::ConnectsTo
                })
                .count()
        } else {
            0
        }
    }

    /// Get all hosts in the graph
    pub fn all(&self) -> Vec<String> {
        self.graph
            .nodes_of_type(NodeType::Host)
            .iter()
            .map(|n| n.id.trim_start_matches("host:").to_string())
            .collect()
    }

    /// Find hosts with specific technology
    pub fn with_technology(&self, tech: &str) -> Vec<String> {
        let tech_lower = tech.to_lowercase();
        let mut hosts = Vec::new();

        for host in self.graph.nodes_of_type(NodeType::Host) {
            for edge in &host.out_edges {
                if edge.edge_type == EdgeType::UsesTech {
                    if let Some(tech_node) = self.graph.get_node(&edge.target_id) {
                        if tech_node.label.to_lowercase().contains(&tech_lower) {
                            hosts.push(host.id.trim_start_matches("host:").to_string());
                            break;
                        }
                    }
                }
            }
        }

        hosts
    }

    /// Find hosts with specific open port
    pub fn with_port(&self, port: u16) -> Vec<String> {
        let port_str = format!(":{}", port);
        let mut hosts = Vec::new();

        for host in self.graph.nodes_of_type(NodeType::Host) {
            for edge in &host.out_edges {
                if edge.edge_type == EdgeType::HasService && edge.target_id.contains(&port_str) {
                    hosts.push(host.id.trim_start_matches("host:").to_string());
                    break;
                }
            }
        }

        hosts
    }

    /// Find hosts affected by vulnerabilities
    pub fn vulnerable(&self) -> Vec<String> {
        let mut hosts = Vec::new();

        for host in self.graph.nodes_of_type(NodeType::Host) {
            let has_vuln = host
                .out_edges
                .iter()
                .any(|e| e.edge_type == EdgeType::AffectedBy);
            if has_vuln {
                hosts.push(host.id.trim_start_matches("host:").to_string());
            }
        }

        hosts
    }
}

/// Extract version from a string (e.g., "nginx 1.18.0" -> Some("1.18.0"))
fn extract_version(s: &str) -> Option<String> {
    // Look for version patterns like X.Y.Z, X.Y, X.Y.Z-suffix
    let mut chars = s.chars().peekable();
    let mut in_version = false;
    let mut version = String::new();

    while let Some(c) = chars.next() {
        if c.is_ascii_digit() && !in_version {
            // Check if this looks like a version (followed by .)
            if chars.peek() == Some(&'.') {
                in_version = true;
                version.push(c);
            }
        } else if in_version {
            if c.is_ascii_digit() || c == '.' || c == '-' || c == '_' || c == 'p' {
                version.push(c);
            } else if c.is_whitespace() || c == ')' || c == '(' {
                break;
            }
        }
    }

    if version.len() >= 3 && version.contains('.') {
        Some(
            version
                .trim_end_matches(|c: char| !c.is_ascii_digit())
                .to_string(),
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_version() {
        assert_eq!(extract_version("nginx 1.18.0"), Some("1.18.0".to_string()));
        // OpenSSH uses 'p' suffix for portable version (e.g., 8.9p1)
        assert_eq!(extract_version("OpenSSH 8.9p1"), Some("8.9p1".to_string()));
        assert_eq!(extract_version("MySQL 8.0.33"), Some("8.0.33".to_string()));
        assert_eq!(extract_version("no version"), None);
    }

    #[test]
    fn test_password_strength() {
        assert_eq!(PasswordStrength::analyze("admin"), PasswordStrength::Weak);
        assert_eq!(
            PasswordStrength::analyze("password123"),
            PasswordStrength::Weak
        );
        // Strong requires: len >= 12 AND complexity >= 3
        assert_eq!(
            PasswordStrength::analyze("MyP@ssw0rd!!"),
            PasswordStrength::Strong
        ); // 12 chars, 4 complexity
           // Medium: len >= 8 AND complexity >= 2 but not meeting Strong criteria
        assert_eq!(
            PasswordStrength::analyze("MyP@ssw0rd!"),
            PasswordStrength::Medium
        ); // 11 chars, 4 complexity
        assert_eq!(
            PasswordStrength::analyze("simple12"),
            PasswordStrength::Medium
        );
    }

    #[test]
    fn test_privilege_from_username() {
        assert_eq!(PrivilegeLevel::from_username("root"), PrivilegeLevel::Root);
        assert_eq!(
            PrivilegeLevel::from_username("Administrator"),
            PrivilegeLevel::Root
        );
        assert_eq!(
            PrivilegeLevel::from_username("admin"),
            PrivilegeLevel::Admin
        );
        assert_eq!(
            PrivilegeLevel::from_username("www-data"),
            PrivilegeLevel::Service
        );
        assert_eq!(PrivilegeLevel::from_username("john"), PrivilegeLevel::User);
    }
}
