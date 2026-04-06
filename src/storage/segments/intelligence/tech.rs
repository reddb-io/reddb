//! Technology-Centric Intelligence
//!
//! Answers: "Who uses this technology? What versions?"

use std::collections::HashMap;

use super::types::*;
use crate::storage::segments::graph::{EdgeType, GraphSegment, NodeType};

/// Profile of a technology across the network
#[derive(Debug, Clone)]
pub struct TechProfile {
    pub name: String,
    pub category: TechCategory,
    pub total_hosts: usize,
    pub version_distribution: Vec<VersionCount>,
    pub known_vulnerabilities: Vec<(String, Vec<VulnInfo>)>,
    pub common_configs: Vec<(String, usize)>,
    pub related_technologies: Vec<(String, usize)>,
}

impl TechProfile {
    pub fn display(&self) -> String {
        let mut s = String::new();
        s.push_str("┌─────────────────────────────────────────────────────────────────┐\n");
        s.push_str(&format!("│  TECHNOLOGY: {:<49} │\n", self.name));
        s.push_str("├─────────────────────────────────────────────────────────────────┤\n");
        s.push_str(&format!("│  CATEGORY: {:<51} │\n", self.category.as_str()));
        s.push_str(&format!("│  TOTAL HOSTS: {:<48} │\n", self.total_hosts));

        if !self.version_distribution.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str("│  VERSION DISTRIBUTION:                                          │\n");
            for vc in self.version_distribution.iter().take(5) {
                let status_str = match vc.status {
                    VersionStatus::Eol => " - EOL!",
                    VersionStatus::Outdated => " - OUTDATED",
                    VersionStatus::Old => " - OLD",
                    _ => "",
                };
                let ver_str = format!("{}: {} hosts{}", vc.version, vc.count, status_str);
                s.push_str(&format!("│    • {:<55} │\n", ver_str));
            }
        }

        if !self.related_technologies.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str("│  RELATED TECHNOLOGIES:                                          │\n");
            for (tech, count) in self.related_technologies.iter().take(4) {
                let rel_str = format!("{}: {} hosts", tech, count);
                s.push_str(&format!("│    • {:<55} │\n", rel_str));
            }
        }

        s.push_str("└─────────────────────────────────────────────────────────────────┘\n");
        s
    }
}

/// Version count
#[derive(Debug, Clone)]
pub struct VersionCount {
    pub version: String,
    pub count: usize,
    pub status: VersionStatus,
}

/// Technology-centric intelligence queries
pub struct TechIntelligence<'a> {
    graph: &'a GraphSegment,
}

impl<'a> TechIntelligence<'a> {
    pub fn new(graph: &'a GraphSegment) -> Self {
        Self { graph }
    }

    /// Get complete technology profile
    pub fn profile(&self, tech: &str) -> Option<TechProfile> {
        let hosts = self.hosts(tech);
        if hosts.is_empty() {
            return None;
        }

        let versions = self.versions(tech);
        let related = self.related(tech);

        Some(TechProfile {
            name: tech.to_string(),
            category: TechCategory::from_name(tech),
            total_hosts: hosts.len(),
            version_distribution: versions,
            known_vulnerabilities: vec![],
            common_configs: vec![],
            related_technologies: related,
        })
    }

    /// Find all hosts using this technology
    pub fn hosts(&self, tech: &str) -> Vec<String> {
        let tech_lower = tech.to_lowercase();
        let mut hosts = Vec::new();

        for tech_node in self.graph.nodes_of_type(NodeType::Technology) {
            if tech_node.label.to_lowercase().contains(&tech_lower) {
                // Find hosts that use this technology
                for edge in &tech_node.in_edges {
                    if edge.edge_type == EdgeType::UsesTech {
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
    pub fn versions(&self, tech: &str) -> Vec<VersionCount> {
        let tech_lower = tech.to_lowercase();
        let mut version_counts: HashMap<String, usize> = HashMap::new();

        for tech_node in self.graph.nodes_of_type(NodeType::Technology) {
            if tech_node.label.to_lowercase().contains(&tech_lower) {
                if let Some(version) = extract_version(&tech_node.label) {
                    *version_counts.entry(version).or_insert(0) += 1;
                } else {
                    *version_counts.entry("unknown".to_string()).or_insert(0) += 1;
                }
            }
        }

        let mut versions: Vec<VersionCount> = version_counts
            .into_iter()
            .map(|(v, c)| VersionCount {
                version: v,
                count: c,
                status: VersionStatus::Current,
            })
            .collect();

        versions.sort_by(|a, b| b.count.cmp(&a.count));
        versions
    }

    /// Find all outdated technologies
    pub fn outdated(&self) -> Vec<(String, String, Vec<String>)> {
        // Would need a version database to determine what's outdated
        vec![]
    }

    /// Find end-of-life technologies
    pub fn eol(&self) -> Vec<(String, String, Vec<String>)> {
        // Would need a version database
        vec![]
    }

    /// Find technologies commonly used together
    pub fn related(&self, tech: &str) -> Vec<(String, usize)> {
        let hosts = self.hosts(tech);
        let mut related: HashMap<String, usize> = HashMap::new();

        for host in &hosts {
            if let Some(host_node) = self.graph.get_node(&format!("host:{}", host)) {
                for edge in &host_node.out_edges {
                    if edge.edge_type == EdgeType::UsesTech {
                        if let Some(tech_node) = self.graph.get_node(&edge.target_id) {
                            let name = tech_node.label.clone();
                            if !name.to_lowercase().contains(&tech.to_lowercase()) {
                                *related.entry(name).or_insert(0) += 1;
                            }
                        }
                    }
                }
            }
        }

        let mut sorted: Vec<_> = related.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        sorted.truncate(10);
        sorted
    }

    /// Get all technologies
    pub fn all(&self) -> Vec<String> {
        let mut techs: Vec<String> = self
            .graph
            .nodes_of_type(NodeType::Technology)
            .iter()
            .map(|n| n.label.clone())
            .collect();

        techs.sort();
        techs.dedup();
        techs
    }
}

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
            } else {
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
