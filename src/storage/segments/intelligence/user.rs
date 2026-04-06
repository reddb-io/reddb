//! User-Centric Intelligence
//!
//! Answers: "Where does this user exist? What privileges?"

use std::collections::{HashMap, HashSet};

use super::types::*;
use crate::storage::segments::graph::{EdgeType, GraphSegment, NodeType};

/// Profile of a username across the network
#[derive(Debug, Clone)]
pub struct UserProfile {
    pub username: String,
    pub host_occurrences: Vec<UserOccurrence>,
    pub known_credentials: Vec<String>,
    pub privilege_summary: PrivilegeSummary,
    pub password_patterns: Vec<String>,
    pub services_accessed: Vec<String>,
}

impl UserProfile {
    pub fn display(&self) -> String {
        let mut s = String::new();
        s.push_str("┌─────────────────────────────────────────────────────────────────┐\n");
        s.push_str(&format!("│  USER: {:<55} │\n", self.username));
        s.push_str("├─────────────────────────────────────────────────────────────────┤\n");

        if !self.host_occurrences.is_empty() {
            s.push_str("│  FOUND ON HOSTS:                                                │\n");
            for occ in self.host_occurrences.iter().take(5) {
                let cred = if occ.has_credential {
                    "(credentials known)"
                } else {
                    ""
                };
                let occ_str = format!("{} {} - {}", occ.host, cred, occ.services.join(", "));
                s.push_str(&format!("│    • {:<55} │\n", occ_str));
            }
            if self.host_occurrences.len() > 5 {
                s.push_str(&format!(
                    "│    ... and {} more                                          │\n",
                    self.host_occurrences.len() - 5
                ));
            }
        }

        s.push_str("│                                                                 │\n");
        s.push_str("│  PRIVILEGE LEVELS:                                              │\n");
        s.push_str(&format!(
            "│    • Root/Admin: {} hosts                                       │\n",
            self.privilege_summary.root_count + self.privilege_summary.admin_count
        ));
        s.push_str(&format!(
            "│    • User: {} hosts                                              │\n",
            self.privilege_summary.user_count
        ));

        s.push_str("│                                                                 │\n");
        s.push_str(&format!(
            "│  CREDENTIALS KNOWN: {} different passwords                     │\n",
            self.known_credentials.len()
        ));

        if !self.services_accessed.is_empty() {
            s.push_str(&format!(
                "│  SERVICES ACCESSED: {:<41} │\n",
                self.services_accessed.join(", ")
            ));
        }

        s.push_str("└─────────────────────────────────────────────────────────────────┘\n");
        s
    }
}

/// A user's presence on a specific host
#[derive(Debug, Clone)]
pub struct UserOccurrence {
    pub host: String,
    pub services: Vec<String>,
    pub has_credential: bool,
    pub privilege: PrivilegeLevel,
}

/// Summary of privilege levels
#[derive(Debug, Clone, Default)]
pub struct PrivilegeSummary {
    pub root_count: usize,
    pub admin_count: usize,
    pub user_count: usize,
}

/// User-centric intelligence queries
pub struct UserIntelligence<'a> {
    graph: &'a GraphSegment,
}

impl<'a> UserIntelligence<'a> {
    pub fn new(graph: &'a GraphSegment) -> Self {
        Self { graph }
    }

    /// Get complete user profile across all hosts
    pub fn profile(&self, username: &str) -> Option<UserProfile> {
        let hosts = self.hosts(username);
        if hosts.is_empty() {
            return None;
        }

        let credentials = self.credentials(username);
        let privileges = self.privileges(username);

        let mut priv_summary = PrivilegeSummary::default();
        for (_, level) in &privileges {
            match level {
                PrivilegeLevel::Root => priv_summary.root_count += 1,
                PrivilegeLevel::Admin => priv_summary.admin_count += 1,
                _ => priv_summary.user_count += 1,
            }
        }

        let mut services: HashSet<String> = HashSet::new();
        let mut occurrences = Vec::new();

        for host in &hosts {
            let host_services = self.services_for_host(host);

            for service in &host_services {
                services.insert(service.clone());
            }

            let has_cred = credentials.iter().any(|c| {
                if let Some(node) = self.graph.get_node(&format!("cred:{}", c)) {
                    node.out_edges.iter().any(|e| {
                        e.edge_type == EdgeType::AuthAccess
                            && e.target_id == format!("host:{}", host)
                    })
                } else {
                    false
                }
            });

            let priv_level = privileges
                .iter()
                .find(|(h, _)| h == host)
                .map(|(_, l)| *l)
                .unwrap_or(PrivilegeLevel::Unknown);

            occurrences.push(UserOccurrence {
                host: host.clone(),
                services: host_services,
                has_credential: has_cred,
                privilege: priv_level,
            });
        }

        let mut services_accessed: Vec<String> = services.into_iter().collect();
        services_accessed.sort();

        let password_patterns = self.password_patterns(&credentials, username);

        Some(UserProfile {
            username: username.to_string(),
            host_occurrences: occurrences,
            known_credentials: credentials,
            privilege_summary: priv_summary,
            password_patterns,
            services_accessed,
        })
    }

    fn services_for_host(&self, host: &str) -> Vec<String> {
        let mut services: HashSet<String> = HashSet::new();
        let host_id = format!("host:{}", host);

        if let Some(host_node) = self.graph.get_node(&host_id) {
            for edge in &host_node.out_edges {
                if edge.edge_type != EdgeType::HasService {
                    continue;
                }

                if let Some(svc_node) = self.graph.get_node(&edge.target_id) {
                    if let Some(service) = svc_node.label.split(':').next_back() {
                        services.insert(service.to_string());
                    } else {
                        services.insert(svc_node.label.clone());
                    }
                    continue;
                }

                let parts: Vec<&str> = edge.target_id.split(':').collect();
                if parts.len() >= 4 {
                    services.insert(parts[3..].join(":"));
                } else {
                    services.insert(edge.target_id.clone());
                }
            }
        }

        let mut services: Vec<String> = services.into_iter().collect();
        services.sort();
        services
    }

    fn password_patterns(&self, credentials: &[String], username: &str) -> Vec<String> {
        let mut patterns: HashSet<String> = HashSet::new();
        let username_lower = username.to_lowercase();

        for credential in credentials {
            let password = credential
                .trim_start_matches("cred:")
                .splitn(2, ':')
                .nth(1)
                .unwrap_or("");

            if password.is_empty() {
                patterns.insert("empty-password".to_string());
                continue;
            }

            if password == username {
                patterns.insert("username-as-password".to_string());
            }
            if password.eq_ignore_ascii_case(&username_lower) {
                patterns.insert("username-based-pattern".to_string());
            }
            if password.len() < 8 {
                patterns.insert("short-password".to_string());
            }

            if password.chars().all(|c| c.is_ascii_digit()) {
                patterns.insert("numeric-only".to_string());
            }
            if password.chars().all(|c| c.is_ascii_alphabetic()) {
                patterns.insert("alpha-only".to_string());
            }
            if password
                .chars()
                .any(|c| c == '!' || c == '@' || c == '#' || c == '$' || c == '%')
            {
                patterns.insert("symbol-prefixed".to_string());
            }
            if password.chars().any(|c| !c.is_ascii_alphanumeric()) {
                patterns.insert("contains-symbol".to_string());
            }
            if password.contains("123") {
                patterns.insert("sequential-numeric-pattern".to_string());
            }

            match PasswordStrength::analyze(password) {
                PasswordStrength::Weak => {
                    patterns.insert("weak-password-pattern".to_string());
                }
                PasswordStrength::Medium => {
                    patterns.insert("medium-password-pattern".to_string());
                }
                PasswordStrength::Strong => {
                    patterns.insert("strong-password-pattern".to_string());
                }
                PasswordStrength::Unknown => {}
            }

            if let Some(first) = password.chars().next() {
                if password.chars().all(|c| c == first) {
                    patterns.insert("repeated-char".to_string());
                }
            }
        }

        let mut patterns: Vec<String> = patterns.into_iter().collect();
        patterns.sort();
        patterns
    }

    /// Find all hosts where this user exists
    pub fn hosts(&self, username: &str) -> Vec<String> {
        let user_lower = username.to_lowercase();
        let mut hosts = Vec::new();

        for cred in self.graph.nodes_of_type(NodeType::Credential) {
            let label = cred.label.trim_start_matches("Creds: ").to_lowercase();
            let cred_user = label.split(':').next().unwrap_or(&label);

            if cred_user == user_lower {
                // Find hosts this credential accesses
                for edge in &cred.out_edges {
                    if edge.edge_type == EdgeType::AuthAccess {
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

    /// Find all known credentials for this user
    pub fn credentials(&self, username: &str) -> Vec<String> {
        let user_lower = username.to_lowercase();
        let mut credentials = Vec::new();

        for cred in self.graph.nodes_of_type(NodeType::Credential) {
            let label = cred.label.trim_start_matches("Creds: ");
            let cred_user = label.split(':').next().unwrap_or(label).to_lowercase();

            if cred_user == user_lower {
                credentials.push(cred.id.trim_start_matches("cred:").to_string());
            }
        }

        credentials
    }

    /// Get privilege levels per host
    pub fn privileges(&self, username: &str) -> Vec<(String, PrivilegeLevel)> {
        let level = PrivilegeLevel::from_username(username);
        self.hosts(username)
            .into_iter()
            .map(|h| (h, level))
            .collect()
    }

    /// Find most common usernames in the graph
    pub fn common(&self, limit: usize) -> Vec<(String, usize)> {
        let mut counts: HashMap<String, usize> = HashMap::new();

        for cred in self.graph.nodes_of_type(NodeType::Credential) {
            let label = cred.label.trim_start_matches("Creds: ");
            let username = label.split(':').next().unwrap_or(label).to_string();
            *counts.entry(username).or_insert(0) += 1;
        }

        let mut sorted: Vec<_> = counts.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        sorted.truncate(limit);
        sorted
    }
}
