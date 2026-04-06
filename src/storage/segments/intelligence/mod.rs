//! Multi-Perspective Intelligence Layer
//!
//! This module provides 10 different "lenses" to analyze graph data:
//! - Host: What do I know about this host?
//! - Credential: What can this credential access?
//! - User: Where does this user exist?
//! - Service: Who runs this service?
//! - Vulnerability: Who is affected?
//! - Technology: Who uses this tech?
//! - Network: What's the topology?
//! - Path: How do I get from A to B?
//! - Domain: What's the DNS structure?
//! - Certificate: What certs exist?

pub mod backend;
pub mod cert;
pub mod credential;
pub mod domain;
pub mod host;
pub mod network;
pub mod path;
pub mod service;
pub mod tech;
mod types;
pub mod user;
pub mod vuln;

pub use backend::{shared_graph, DiskBackedGraph, GraphBackendStats, SharedGraph};
pub use cert::CertIntelligence;
pub use credential::CredentialIntelligence;
pub use domain::DomainIntelligence;
pub use host::HostIntelligence;
pub use network::NetworkIntelligence;
pub use path::PathIntelligence;
pub use service::ServiceIntelligence;
pub use tech::TechIntelligence;
pub use types::*;
pub use user::UserIntelligence;
pub use vuln::VulnIntelligence;

use super::graph::GraphSegment;

/// Unified intelligence interface providing all 10 perspectives
pub struct Intelligence<'a> {
    graph: &'a GraphSegment,
}

impl<'a> Intelligence<'a> {
    pub fn new(graph: &'a GraphSegment) -> Self {
        Self { graph }
    }

    /// Host-centric intelligence
    pub fn host(&self) -> HostIntelligence<'a> {
        HostIntelligence::new(self.graph)
    }

    /// Credential-centric intelligence
    pub fn credential(&self) -> CredentialIntelligence<'a> {
        CredentialIntelligence::new(self.graph)
    }

    /// User-centric intelligence
    pub fn user(&self) -> UserIntelligence<'a> {
        UserIntelligence::new(self.graph)
    }

    /// Service-centric intelligence
    pub fn service(&self) -> ServiceIntelligence<'a> {
        ServiceIntelligence::new(self.graph)
    }

    /// Vulnerability-centric intelligence
    pub fn vuln(&self) -> VulnIntelligence<'a> {
        VulnIntelligence::new(self.graph)
    }

    /// Technology-centric intelligence
    pub fn tech(&self) -> TechIntelligence<'a> {
        TechIntelligence::new(self.graph)
    }

    /// Network topology intelligence
    pub fn network(&self) -> NetworkIntelligence<'a> {
        NetworkIntelligence::new(self.graph)
    }

    /// Path-finding intelligence
    pub fn path(&self) -> PathIntelligence<'a> {
        PathIntelligence::new(self.graph)
    }

    /// Domain/DNS intelligence
    pub fn domain(&self) -> DomainIntelligence<'a> {
        DomainIntelligence::new(self.graph)
    }

    /// Certificate intelligence
    pub fn cert(&self) -> CertIntelligence<'a> {
        CertIntelligence::new(self.graph)
    }

    /// Get graph statistics summary
    pub fn summary(&self) -> IntelligenceSummary {
        let nodes = self.graph.node_count();
        let edges = self.graph.edge_count();
        let edge_counts = self.graph.count_edges_by_type();

        IntelligenceSummary {
            total_nodes: nodes,
            total_edges: edges,
            hosts: self.graph.nodes_of_type(super::graph::NodeType::Host).len(),
            services: self
                .graph
                .nodes_of_type(super::graph::NodeType::Service)
                .len(),
            credentials: self
                .graph
                .nodes_of_type(super::graph::NodeType::Credential)
                .len(),
            vulnerabilities: self
                .graph
                .nodes_of_type(super::graph::NodeType::Vulnerability)
                .len(),
            technologies: self
                .graph
                .nodes_of_type(super::graph::NodeType::Technology)
                .len(),
            endpoints: self
                .graph
                .nodes_of_type(super::graph::NodeType::Endpoint)
                .len(),
            edge_counts,
        }
    }
}

/// Summary statistics for the intelligence graph
#[derive(Debug, Clone)]
pub struct IntelligenceSummary {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub hosts: usize,
    pub services: usize,
    pub credentials: usize,
    pub vulnerabilities: usize,
    pub technologies: usize,
    pub endpoints: usize,
    pub edge_counts: std::collections::HashMap<super::graph::EdgeType, usize>,
}

impl IntelligenceSummary {
    /// Format as a display string
    pub fn display(&self) -> String {
        let mut s = String::new();
        s.push_str("┌─────────────────────────────────────────────────────────────────┐\n");
        s.push_str("│  INTELLIGENCE SUMMARY                                           │\n");
        s.push_str("├─────────────────────────────────────────────────────────────────┤\n");
        s.push_str(&format!(
            "│  Total Nodes: {:<8}  Total Edges: {:<8}                 │\n",
            self.total_nodes, self.total_edges
        ));
        s.push_str("│                                                                 │\n");
        s.push_str("│  NODE TYPES:                                                    │\n");
        s.push_str(&format!(
            "│    • Hosts: {:<10}  • Services: {:<10}              │\n",
            self.hosts, self.services
        ));
        s.push_str(&format!(
            "│    • Credentials: {:<4}  • Vulnerabilities: {:<4}          │\n",
            self.credentials, self.vulnerabilities
        ));
        s.push_str(&format!(
            "│    • Technologies: {:<3}  • Endpoints: {:<4}                │\n",
            self.technologies, self.endpoints
        ));
        s.push_str("└─────────────────────────────────────────────────────────────────┘\n");
        s
    }
}
