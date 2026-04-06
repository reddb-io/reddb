//! Service-Centric Intelligence
//!
//! Answers: "Who runs this service? What versions exist?"

use std::collections::HashMap;

use super::types::*;
use crate::storage::segments::graph::{EdgeType, GraphSegment, NodeType};

/// Profile of a service across the network
#[derive(Debug, Clone)]
pub struct ServiceProfile {
    pub name: String,
    pub total_hosts: usize,
    pub version_distribution: Vec<VersionCount>,
    pub vulnerabilities_by_version: Vec<(String, Vec<VulnInfo>)>,
    pub authentication_methods: Vec<(String, usize)>,
    pub credentials_found: usize,
}

impl ServiceProfile {
    pub fn display(&self) -> String {
        let mut s = String::new();
        s.push_str("┌─────────────────────────────────────────────────────────────────┐\n");
        s.push_str(&format!("│  SERVICE: {:<52} │\n", self.name));
        s.push_str("├─────────────────────────────────────────────────────────────────┤\n");
        s.push_str(&format!("│  TOTAL HOSTS: {:<48} │\n", self.total_hosts));

        if !self.version_distribution.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str("│  VERSION DISTRIBUTION:                                          │\n");
            for vc in self.version_distribution.iter().take(5) {
                let pct = if self.total_hosts > 0 {
                    (vc.count as f32 / self.total_hosts as f32 * 100.0) as u32
                } else {
                    0
                };
                let ver_str = format!(
                    "{}: {} hosts ({}%) - {}",
                    vc.version,
                    vc.count,
                    pct,
                    vc.status.as_str()
                );
                s.push_str(&format!("│    • {:<55} │\n", ver_str));
            }
        }

        s.push_str("│                                                                 │\n");
        s.push_str(&format!(
            "│  CREDENTIALS FOUND: {} valid pairs                            │\n",
            self.credentials_found
        ));

        s.push_str("└─────────────────────────────────────────────────────────────────┘\n");
        s
    }
}

/// Version count with status
#[derive(Debug, Clone)]
pub struct VersionCount {
    pub version: String,
    pub count: usize,
    pub status: VersionStatus,
}

/// Service-centric intelligence queries
pub struct ServiceIntelligence<'a> {
    graph: &'a GraphSegment,
}

impl<'a> ServiceIntelligence<'a> {
    pub fn new(graph: &'a GraphSegment) -> Self {
        Self { graph }
    }

    /// Get complete service profile
    pub fn profile(&self, service: &str) -> Option<ServiceProfile> {
        let hosts = self.hosts(service);
        if hosts.is_empty() {
            return None;
        }

        let versions = self.versions(service);

        Some(ServiceProfile {
            name: service.to_string(),
            total_hosts: hosts.len(),
            version_distribution: versions,
            vulnerabilities_by_version: vec![],
            authentication_methods: vec![],
            credentials_found: 0,
        })
    }

    /// Find all hosts running this service
    pub fn hosts(&self, service: &str) -> Vec<String> {
        let svc_lower = service.to_lowercase();
        let mut hosts = Vec::new();

        for svc_node in self.graph.nodes_of_type(NodeType::Service) {
            if svc_node.label.to_lowercase().contains(&svc_lower)
                || svc_node.id.to_lowercase().contains(&svc_lower)
            {
                // Find the host that has this service
                for edge in &svc_node.in_edges {
                    if edge.edge_type == EdgeType::HasService {
                        let host = edge.target_id.trim_start_matches("host:").to_string();
                        if !hosts.contains(&host) {
                            hosts.push(host);
                        }
                    }
                }
            }
        }

        hosts
    }

    /// Get version distribution
    pub fn versions(&self, service: &str) -> Vec<VersionCount> {
        let svc_lower = service.to_lowercase();
        let mut version_counts: HashMap<String, usize> = HashMap::new();

        for svc_node in self.graph.nodes_of_type(NodeType::Service) {
            if svc_node.label.to_lowercase().contains(&svc_lower) {
                // Try to extract version from label
                if let Some(version) = extract_version(&svc_node.label) {
                    *version_counts.entry(version).or_insert(0) += 1;
                } else {
                    *version_counts.entry("unknown".to_string()).or_insert(0) += 1;
                }
            }
        }

        let mut versions: Vec<VersionCount> = version_counts
            .into_iter()
            .map(|(v, c)| VersionCount {
                version: v.clone(),
                count: c,
                status: infer_version_status(&v),
            })
            .collect();

        versions.sort_by(|a, b| b.count.cmp(&a.count));
        versions
    }

    /// Find hosts with outdated versions
    pub fn outdated(&self, service: &str) -> Vec<(String, String)> {
        let versions = self.versions(service);
        let hosts = self.hosts(service);
        let svc_lower = service.to_lowercase();

        let mut outdated = Vec::new();

        for host in hosts {
            if let Some(host_node) = self.graph.get_node(&format!("host:{}", host)) {
                for edge in &host_node.out_edges {
                    if edge.edge_type == EdgeType::HasService {
                        if let Some(svc_node) = self.graph.get_node(&edge.target_id) {
                            if svc_node.label.to_lowercase().contains(&svc_lower) {
                                if let Some(ver) = extract_version(&svc_node.label) {
                                    let status = infer_version_status(&ver);
                                    if matches!(
                                        status,
                                        VersionStatus::Outdated
                                            | VersionStatus::Old
                                            | VersionStatus::Eol
                                    ) {
                                        outdated.push((host.clone(), ver));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        outdated
    }

    /// Get all service names
    pub fn all(&self) -> Vec<String> {
        let mut services: Vec<String> = self
            .graph
            .nodes_of_type(NodeType::Service)
            .iter()
            .filter_map(|n| {
                // Extract service name from ID (service:ip:port:name)
                let parts: Vec<&str> = n.id.split(':').collect();
                if parts.len() >= 4 {
                    Some(parts[3..].join(":"))
                } else {
                    None
                }
            })
            .collect();

        services.sort();
        services.dedup();
        services
    }
}

/// Extract version from a string
fn extract_version(s: &str) -> Option<String> {
    let mut chars = s.chars().peekable();
    let mut in_version = false;
    let mut version = String::new();

    while let Some(c) = chars.next() {
        if c.is_ascii_digit() && !in_version {
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

/// Infer version status (simplified - would need version database in practice)
fn infer_version_status(_version: &str) -> VersionStatus {
    // In a real implementation, this would check against a version database
    VersionStatus::Current
}
