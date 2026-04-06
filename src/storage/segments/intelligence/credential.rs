//! Credential-Centric Intelligence
//!
//! Answers: "What can this credential access?"

use super::types::*;
use crate::storage::segments::graph::{EdgeType, GraphSegment, NodeType};

/// Complete profile of a discovered credential
#[derive(Debug, Clone)]
pub struct CredentialProfile {
    pub username: String,
    pub password: Option<String>,
    pub cred_type: CredentialType,
    pub discovered_on: Vec<DiscoveryInfo>,
    pub accessible_hosts: Vec<AccessibleHost>,
    pub privilege_level: PrivilegeLevel,
    pub reuse_count: usize,
    pub password_strength: PasswordStrength,
    pub attack_paths_enabled: usize,
}

impl CredentialProfile {
    /// Format as a display string
    pub fn display(&self) -> String {
        let mut s = String::new();
        s.push_str("┌─────────────────────────────────────────────────────────────────┐\n");

        let cred_display = if let Some(ref pass) = self.password {
            format!("{}:{}", self.username, mask_password(pass))
        } else {
            self.username.clone()
        };
        s.push_str(&format!("│  CREDENTIAL: {:<49} │\n", cred_display));
        s.push_str("├─────────────────────────────────────────────────────────────────┤\n");
        s.push_str(&format!("│  TYPE: {:<55} │\n", self.cred_type.as_str()));
        s.push_str(&format!(
            "│  STRENGTH: {:<51} │\n",
            self.password_strength.as_str()
        ));

        // Discovery sources
        if !self.discovered_on.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str("│  DISCOVERED ON:                                                 │\n");
            for disc in self.discovered_on.iter().take(4) {
                let disc_str = format!("{} via {}", disc.host, disc.method);
                s.push_str(&format!("│    • {:<55} │\n", disc_str));
            }
        }

        // Accessible hosts
        if !self.accessible_hosts.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str("│  CAN ACCESS:                                                    │\n");
            for host in self.accessible_hosts.iter().take(5) {
                let verified = if host.verified { "✓" } else { "?" };
                let host_str = format!(
                    "{}:{} ({}) - {}",
                    host.host, host.port, host.service, verified
                );
                s.push_str(&format!("│    • {:<55} │\n", host_str));
            }
            if self.accessible_hosts.len() > 5 {
                s.push_str(&format!(
                    "│    ... and {} more                                          │\n",
                    self.accessible_hosts.len() - 5
                ));
            }
        }

        s.push_str("│                                                                 │\n");
        s.push_str(&format!(
            "│  PRIVILEGE LEVEL: {:<44} │\n",
            self.privilege_level.as_str()
        ));
        s.push_str(&format!(
            "│  REUSE DETECTED: {} hosts share this credential               │\n",
            self.reuse_count
        ));
        s.push_str("│                                                                 │\n");
        s.push_str(&format!(
            "│  ATTACK PATHS ENABLED: {:<39} │\n",
            self.attack_paths_enabled
        ));

        s.push_str("└─────────────────────────────────────────────────────────────────┘\n");
        s
    }
}

/// Where/how a credential was discovered
#[derive(Debug, Clone)]
pub struct DiscoveryInfo {
    pub host: String,
    pub service: String,
    pub method: String,
    pub timestamp: u64,
}

/// A host accessible with a credential
#[derive(Debug, Clone)]
pub struct AccessibleHost {
    pub host: String,
    pub port: u16,
    pub service: String,
    pub verified: bool,
}

/// Credential-centric intelligence queries
pub struct CredentialIntelligence<'a> {
    graph: &'a GraphSegment,
}

impl<'a> CredentialIntelligence<'a> {
    pub fn new(graph: &'a GraphSegment) -> Self {
        Self { graph }
    }

    /// Get complete credential profile
    pub fn profile(&self, cred: &str) -> Option<CredentialProfile> {
        // Find the credential node
        let cred_id = if cred.starts_with("cred:") {
            cred.to_string()
        } else {
            format!("cred:{}", cred)
        };

        let node = self.graph.get_node(&cred_id)?;

        // Extract username and password from label
        let label = node.label.trim_start_matches("Creds: ");
        let (username, password) = if let Some(idx) = label.find(':') {
            (label[..idx].to_string(), Some(label[idx + 1..].to_string()))
        } else {
            (label.to_string(), None)
        };

        let strength = password
            .as_ref()
            .map(|p| PasswordStrength::analyze(p))
            .unwrap_or(PasswordStrength::Unknown);

        let accessible = self.reach(&cred_id);
        let discovered = self.origin(&cred_id);

        Some(CredentialProfile {
            username: username.clone(),
            password,
            cred_type: CredentialType::Password,
            discovered_on: discovered,
            accessible_hosts: accessible.clone(),
            privilege_level: PrivilegeLevel::from_username(&username),
            reuse_count: accessible.len(),
            password_strength: strength,
            attack_paths_enabled: accessible.len(),
        })
    }

    /// Find all hosts accessible with this credential
    pub fn reach(&self, cred: &str) -> Vec<AccessibleHost> {
        let cred_id = if cred.starts_with("cred:") {
            cred.to_string()
        } else {
            format!("cred:{}", cred)
        };

        let mut hosts = Vec::new();

        if let Some(node) = self.graph.get_node(&cred_id) {
            for edge in &node.out_edges {
                if edge.edge_type == EdgeType::AuthAccess {
                    if let Some(host_node) = self.graph.get_node(&edge.target_id) {
                        let ip = host_node.id.trim_start_matches("host:").to_string();

                        // Try to find what service this credential accesses
                        let port = self.infer_port_for_credential(&host_node.id);

                        hosts.push(AccessibleHost {
                            host: ip,
                            port: port.0,
                            service: port.1,
                            verified: true,
                        });
                    }
                }
            }
        }

        hosts
    }

    /// Infer the port/service for a credential-host relationship
    fn infer_port_for_credential(&self, host_id: &str) -> (u16, String) {
        // Check common credential-based services on this host
        if let Some(host) = self.graph.get_node(host_id) {
            for edge in &host.out_edges {
                if edge.edge_type == EdgeType::HasService {
                    // Check for SSH, RDP, MySQL, etc.
                    if edge.target_id.contains(":22:") || edge.target_id.contains(":ssh") {
                        return (22, "SSH".to_string());
                    }
                    if edge.target_id.contains(":3389:") || edge.target_id.contains(":rdp") {
                        return (3389, "RDP".to_string());
                    }
                    if edge.target_id.contains(":3306:") || edge.target_id.contains(":mysql") {
                        return (3306, "MySQL".to_string());
                    }
                    if edge.target_id.contains(":5432:") || edge.target_id.contains(":postgres") {
                        return (5432, "PostgreSQL".to_string());
                    }
                }
            }
        }
        (0, "Unknown".to_string())
    }

    /// Find where/how credential was discovered
    pub fn origin(&self, cred: &str) -> Vec<DiscoveryInfo> {
        let cred_id = if cred.starts_with("cred:") {
            cred.to_string()
        } else {
            format!("cred:{}", cred)
        };

        let mut origins = Vec::new();

        if let Some(node) = self.graph.get_node(&cred_id) {
            // Check incoming edges (who contains this credential)
            for edge in &node.in_edges {
                if edge.edge_type == EdgeType::Contains {
                    if let Some(host_node) = self.graph.get_node(&edge.target_id) {
                        origins.push(DiscoveryInfo {
                            host: host_node.id.trim_start_matches("host:").to_string(),
                            service: "unknown".to_string(),
                            method: "discovered".to_string(),
                            timestamp: 0,
                        });
                    }
                }
            }
        }

        origins
    }

    /// Find credentials reused across multiple hosts
    pub fn reuse(&self) -> Vec<(String, Vec<String>)> {
        let mut results = Vec::new();

        for cred in self.graph.nodes_of_type(NodeType::Credential) {
            let hosts: Vec<String> = cred
                .out_edges
                .iter()
                .filter(|e| e.edge_type == EdgeType::AuthAccess)
                .filter_map(|e| self.graph.get_node(&e.target_id))
                .map(|n| n.id.trim_start_matches("host:").to_string())
                .collect();

            if hosts.len() >= 2 {
                results.push((cred.label.clone(), hosts));
            }
        }

        // Sort by reuse count (highest first)
        results.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
        results
    }

    /// Find weak/default credentials
    pub fn weak(&self) -> Vec<CredentialProfile> {
        let mut weak = Vec::new();

        for cred in self.graph.nodes_of_type(NodeType::Credential) {
            let label = cred.label.trim_start_matches("Creds: ");
            if let Some(idx) = label.find(':') {
                let password = &label[idx + 1..];
                if PasswordStrength::analyze(password) == PasswordStrength::Weak {
                    if let Some(profile) = self.profile(&cred.id) {
                        weak.push(profile);
                    }
                }
            }
        }

        weak
    }

    /// Find credentials with admin/root access
    pub fn privileged(&self) -> Vec<CredentialProfile> {
        let mut privileged = Vec::new();

        for cred in self.graph.nodes_of_type(NodeType::Credential) {
            let label = cred.label.trim_start_matches("Creds: ");
            let username = label.split(':').next().unwrap_or(label);
            let level = PrivilegeLevel::from_username(username);

            if level >= PrivilegeLevel::Admin {
                if let Some(profile) = self.profile(&cred.id) {
                    privileged.push(profile);
                }
            }
        }

        privileged
    }

    /// Get all credentials
    pub fn all(&self) -> Vec<String> {
        self.graph
            .nodes_of_type(NodeType::Credential)
            .iter()
            .map(|n| n.id.trim_start_matches("cred:").to_string())
            .collect()
    }
}

/// Mask a password for display (show first and last char)
fn mask_password(password: &str) -> String {
    if password.len() <= 2 {
        "*".repeat(password.len())
    } else {
        let first = password.chars().next().unwrap();
        let last = password.chars().last().unwrap();
        format!("{}{}{}", first, "*".repeat(password.len() - 2), last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_password() {
        assert_eq!(mask_password("admin123"), "a******3");
        assert_eq!(mask_password("ab"), "**");
        assert_eq!(mask_password("a"), "*");
    }
}
