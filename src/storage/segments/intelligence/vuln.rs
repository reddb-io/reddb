//! Vulnerability-Centric Intelligence
//!
//! Answers: "Who is affected? What's the impact?"

use super::types::*;
use crate::modules::common::Severity;
use crate::storage::segments::graph::{EdgeType, GraphSegment, NodeType};

/// Profile of a vulnerability across the network
#[derive(Debug, Clone)]
pub struct VulnProfile {
    pub cve: Option<String>,
    pub title: String,
    pub cvss: f32,
    pub severity: Severity,
    pub vuln_type: VulnType,
    pub exploitable: bool,
    pub affected_hosts: Vec<AffectedHost>,
    pub affected_services: Vec<(String, usize)>,
    pub network_exposure: NetworkExposure,
    pub attack_paths_count: usize,
}

impl VulnProfile {
    pub fn display(&self) -> String {
        let mut s = String::new();
        s.push_str("┌─────────────────────────────────────────────────────────────────┐\n");

        let title = self.cve.as_ref().unwrap_or(&self.title);
        s.push_str(&format!("│  VULNERABILITY: {:<46} │\n", title));
        s.push_str("├─────────────────────────────────────────────────────────────────┤\n");
        s.push_str(&format!(
            "│  CVSS: {:<10} ({})                                    │\n",
            self.cvss,
            self.severity.as_str().to_uppercase()
        ));
        s.push_str(&format!("│  TYPE: {:<55} │\n", self.vuln_type.as_str()));

        let exploit_str = if self.exploitable {
            "Yes (public exploit available)"
        } else {
            "No"
        };
        s.push_str(&format!("│  EXPLOITABLE: {:<48} │\n", exploit_str));

        if !self.affected_hosts.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str(&format!(
                "│  AFFECTED HOSTS: {:<45} │\n",
                self.affected_hosts.len()
            ));
            for host in self.affected_hosts.iter().take(5) {
                let host_str = format!("{} - {}", host.ip, host.service);
                s.push_str(&format!("│    • {:<55} │\n", host_str));
            }
            if self.affected_hosts.len() > 5 {
                s.push_str(&format!(
                    "│    ... and {} more                                          │\n",
                    self.affected_hosts.len() - 5
                ));
            }
        }

        s.push_str("│                                                                 │\n");
        s.push_str("│  NETWORK EXPOSURE:                                              │\n");
        s.push_str(&format!(
            "│    • Internet-facing: {} hosts                                 │\n",
            self.network_exposure.internet_facing
        ));
        s.push_str(&format!(
            "│    • Internal only: {} hosts                                   │\n",
            self.network_exposure.internal_only
        ));

        s.push_str("│                                                                 │\n");
        s.push_str(&format!(
            "│  ATTACK PATHS VIA THIS VULN: {:<33} │\n",
            self.attack_paths_count
        ));

        s.push_str("└─────────────────────────────────────────────────────────────────┘\n");
        s
    }
}

/// A host affected by a vulnerability
#[derive(Debug, Clone)]
pub struct AffectedHost {
    pub ip: String,
    pub service: String,
    pub version: String,
}

/// Network exposure information
#[derive(Debug, Clone, Default)]
pub struct NetworkExposure {
    pub internet_facing: usize,
    pub internal_only: usize,
}

/// Vulnerability-centric intelligence queries
pub struct VulnIntelligence<'a> {
    graph: &'a GraphSegment,
}

impl<'a> VulnIntelligence<'a> {
    pub fn new(graph: &'a GraphSegment) -> Self {
        Self { graph }
    }

    /// Get complete vulnerability profile
    pub fn profile(&self, cve: &str) -> Option<VulnProfile> {
        let vuln_id = if cve.starts_with("vuln:") {
            cve.to_string()
        } else {
            format!("vuln:{}", cve)
        };

        let node = self.graph.get_node(&vuln_id)?;
        let affected = self.affected(&vuln_id);

        // Parse severity from label
        let severity = if node.label.contains("Critical") {
            Severity::Critical
        } else if node.label.contains("High") {
            Severity::High
        } else if node.label.contains("Medium") {
            Severity::Medium
        } else if node.label.contains("Low") {
            Severity::Low
        } else {
            Severity::Info
        };

        let cvss = match severity {
            Severity::Critical => 9.5,
            Severity::High => 7.5,
            Severity::Medium => 5.0,
            Severity::Low => 2.5,
            Severity::Info => 0.0,
        };

        Some(VulnProfile {
            cve: if cve.starts_with("CVE-") {
                Some(cve.to_string())
            } else {
                None
            },
            title: node.label.clone(),
            cvss,
            severity,
            vuln_type: VulnType::Other,
            exploitable: false,
            affected_hosts: affected.clone(),
            affected_services: vec![],
            network_exposure: NetworkExposure {
                internet_facing: 0,
                internal_only: affected.len(),
            },
            attack_paths_count: affected.len(),
        })
    }

    /// Find all affected hosts
    pub fn affected(&self, cve: &str) -> Vec<AffectedHost> {
        let vuln_id = if cve.starts_with("vuln:") {
            cve.to_string()
        } else {
            format!("vuln:{}", cve)
        };

        let mut affected = Vec::new();

        if let Some(node) = self.graph.get_node(&vuln_id) {
            // Check incoming edges (hosts affected by this vuln)
            for edge in &node.in_edges {
                if edge.edge_type == EdgeType::AffectedBy {
                    if let Some(host_node) = self.graph.get_node(&edge.target_id) {
                        affected.push(AffectedHost {
                            ip: host_node.id.trim_start_matches("host:").to_string(),
                            service: "unknown".to_string(),
                            version: "unknown".to_string(),
                        });
                    }
                }
            }
        }

        affected
    }

    /// Find all critical vulnerabilities (CVSS >= 9.0)
    pub fn critical(&self) -> Vec<VulnProfile> {
        self.graph
            .nodes_of_type(NodeType::Vulnerability)
            .iter()
            .filter(|n| n.label.contains("Critical"))
            .filter_map(|n| self.profile(&n.id))
            .collect()
    }

    /// Find vulnerabilities with public exploits
    pub fn exploitable(&self) -> Vec<VulnProfile> {
        // In a real implementation, this would check against an exploit database
        vec![]
    }

    /// Find RCE vulnerabilities
    pub fn rce(&self) -> Vec<VulnProfile> {
        self.graph
            .nodes_of_type(NodeType::Vulnerability)
            .iter()
            .filter(|n| {
                let label = n.label.to_lowercase();
                label.contains("rce")
                    || label.contains("remote code")
                    || label.contains("command injection")
                    || label.contains("code execution")
            })
            .filter_map(|n| self.profile(&n.id))
            .collect()
    }

    /// Get vulnerabilities for a specific host
    pub fn by_host(&self, host: &str) -> Vec<VulnInfo> {
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
                        let cve = if vuln_node.id.starts_with("vuln:CVE-") {
                            Some(vuln_node.id.trim_start_matches("vuln:").to_string())
                        } else {
                            None
                        };

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

        vulns.sort_by(|a, b| b.severity.cmp(&a.severity));
        vulns
    }

    /// Get all vulnerabilities
    pub fn all(&self) -> Vec<String> {
        self.graph
            .nodes_of_type(NodeType::Vulnerability)
            .iter()
            .map(|n| n.id.trim_start_matches("vuln:").to_string())
            .collect()
    }
}
