//! Graph Emitter - High-level API for modules to emit graph data
//!
//! This module provides a simple API for security modules to emit
//! nodes and edges to the intelligence graph without needing to
//! understand the underlying storage details.
//!
//! # Example
//!
//! ```ignore
//! use crate::storage::engine::emitter::GraphEmitter;
//!
//! let emitter = GraphEmitter::global();
//!
//! // Emit a host discovered by port scan
//! emitter.emit_host("192.168.1.1", Some("linux"), Some("gateway.local"));
//!
//! // Emit services found on the host
//! emitter.emit_service("192.168.1.1", 22, "ssh", Some("OpenSSH 8.9"));
//! emitter.emit_service("192.168.1.1", 80, "http", Some("nginx/1.18.0"));
//!
//! // Emit a vulnerability affecting the host
//! emitter.emit_vulnerability("CVE-2021-44228", 10.0, Some("Log4Shell"));
//! emitter.emit_host_vuln("192.168.1.1", "CVE-2021-44228");
//! ```

use crate::storage::engine::graph_store::{GraphEdgeType, GraphNodeType, GraphStore};
use std::sync::{Arc, OnceLock, RwLock};

/// Global graph emitter instance
static GLOBAL_EMITTER: OnceLock<Arc<GraphEmitter>> = OnceLock::new();

/// High-level API for emitting graph data from modules
pub struct GraphEmitter {
    graph: RwLock<GraphStore>,
}

impl GraphEmitter {
    /// Create a new emitter with an empty graph
    pub fn new() -> Self {
        Self {
            graph: RwLock::new(GraphStore::new()),
        }
    }

    /// Create an emitter wrapping an existing graph
    pub fn with_graph(graph: GraphStore) -> Self {
        Self {
            graph: RwLock::new(graph),
        }
    }

    /// Get or create the global emitter instance
    pub fn global() -> Arc<GraphEmitter> {
        GLOBAL_EMITTER
            .get_or_init(|| Arc::new(GraphEmitter::new()))
            .clone()
    }

    /// Reset the global emitter (useful for testing)
    pub fn reset_global() {
        if let Some(emitter) = GLOBAL_EMITTER.get() {
            *emitter.graph_mut() = GraphStore::new();
        }
    }

    /// Get read access to the underlying graph
    pub fn graph(&self) -> std::sync::RwLockReadGuard<'_, GraphStore> {
        self.graph
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Get write access to the underlying graph
    pub fn graph_mut(&self) -> std::sync::RwLockWriteGuard<'_, GraphStore> {
        self.graph
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // ==================== Host Emission ====================

    /// Emit a host node
    ///
    /// # Arguments
    /// * `ip` - IP address of the host
    /// * `os` - Optional OS detection result
    /// * `hostname` - Optional hostname
    pub fn emit_host(&self, ip: &str, os: Option<&str>, hostname: Option<&str>) {
        let node_id = format!("host:{}", ip);

        // Build label with all available info
        let label = match (hostname, os) {
            (Some(name), Some(os_name)) => format!("{} ({}) [{}]", ip, name, os_name),
            (Some(name), None) => format!("{} ({})", ip, name),
            (None, Some(os_name)) => format!("{} [{}]", ip, os_name),
            (None, None) => ip.to_string(),
        };

        let graph = self.graph_mut();
        let _ = graph.add_node(&node_id, &label, GraphNodeType::Host);
    }

    /// Emit a host with TTL information (useful for OS fingerprinting)
    pub fn emit_host_with_ttl(&self, ip: &str, ttl: u8, hostname: Option<&str>) {
        let os = Self::guess_os_from_ttl(ttl);
        self.emit_host(ip, os, hostname);
    }

    fn guess_os_from_ttl(ttl: u8) -> Option<&'static str> {
        match ttl {
            0..=64 => Some("linux"),
            65..=128 => Some("windows"),
            129..=255 => Some("network-device"),
        }
    }

    // ==================== Service Emission ====================

    /// Emit a service node and link it to a host
    ///
    /// # Arguments
    /// * `host_ip` - IP address of the host
    /// * `port` - Port number
    /// * `service_name` - Service name (e.g., "ssh", "http")
    /// * `version` - Optional version string
    pub fn emit_service(
        &self,
        host_ip: &str,
        port: u16,
        service_name: &str,
        version: Option<&str>,
    ) {
        // Ensure host exists
        self.ensure_host(host_ip);

        let node_id = format!("service:{}:{}:{}", host_ip, port, service_name);
        let label = match version {
            Some(v) => format!("{}:{} ({} {})", host_ip, port, service_name, v),
            None => format!("{}:{} ({})", host_ip, port, service_name),
        };

        let host_id = format!("host:{}", host_ip);

        let graph = self.graph_mut();
        let _ = graph.add_node(&node_id, &label, GraphNodeType::Service);
        let _ = graph.add_edge(&host_id, &node_id, GraphEdgeType::HasService, 1.0);
    }

    /// Emit a service with banner information
    pub fn emit_service_with_banner(
        &self,
        host_ip: &str,
        port: u16,
        service_name: &str,
        version: Option<&str>,
        banner: &str,
    ) {
        // Ensure host exists
        self.ensure_host(host_ip);

        let node_id = format!("service:{}:{}:{}", host_ip, port, service_name);
        // Include banner info in label (truncated for display)
        let banner_preview = if banner.len() > 30 {
            format!("{}...", &banner[..30])
        } else {
            banner.to_string()
        };
        let label = match version {
            Some(v) => format!(
                "{}:{} ({} {}) [{}]",
                host_ip, port, service_name, v, banner_preview
            ),
            None => format!(
                "{}:{} ({}) [{}]",
                host_ip, port, service_name, banner_preview
            ),
        };

        let host_id = format!("host:{}", host_ip);

        let graph = self.graph_mut();
        let _ = graph.add_node(&node_id, &label, GraphNodeType::Service);
        let _ = graph.add_edge(&host_id, &node_id, GraphEdgeType::HasService, 1.0);
    }

    // ==================== Vulnerability Emission ====================

    /// Emit a vulnerability node
    ///
    /// # Arguments
    /// * `cve` - CVE identifier (e.g., "CVE-2021-44228")
    /// * `cvss` - CVSS score (0.0 - 10.0)
    /// * `title` - Optional vulnerability title
    pub fn emit_vulnerability(&self, cve: &str, cvss: f64, title: Option<&str>) {
        let node_id = format!("vuln:{}", cve);
        let label = match title {
            Some(t) => format!("{} - {} (CVSS: {:.1})", cve, t, cvss),
            None => format!("{} (CVSS: {:.1})", cve, cvss),
        };

        let graph = self.graph_mut();
        let _ = graph.add_node(&node_id, &label, GraphNodeType::Vulnerability);
    }

    /// Link a vulnerability to a host
    pub fn emit_host_vuln(&self, host_ip: &str, cve: &str) {
        self.ensure_host(host_ip);
        self.ensure_vuln(cve);

        let host_id = format!("host:{}", host_ip);
        let vuln_id = format!("vuln:{}", cve);

        let graph = self.graph_mut();
        let _ = graph.add_edge(&host_id, &vuln_id, GraphEdgeType::AffectedBy, 1.0);
    }

    /// Link a vulnerability to a service
    pub fn emit_service_vuln(&self, host_ip: &str, port: u16, service_name: &str, cve: &str) {
        self.ensure_vuln(cve);

        let service_id = format!("service:{}:{}:{}", host_ip, port, service_name);
        let vuln_id = format!("vuln:{}", cve);

        let graph = self.graph_mut();
        let _ = graph.add_edge(&service_id, &vuln_id, GraphEdgeType::AffectedBy, 1.0);
    }

    // ==================== Technology Emission ====================

    /// Emit a technology node and link it to a host
    ///
    /// # Arguments
    /// * `host_ip` - IP address of the host
    /// * `tech_name` - Technology name (e.g., "nginx", "Apache")
    /// * `version` - Optional version string
    /// * `category` - Optional category (e.g., "WebServer", "Database")
    pub fn emit_technology(
        &self,
        host_ip: &str,
        tech_name: &str,
        version: Option<&str>,
        category: Option<&str>,
    ) {
        self.ensure_host(host_ip);

        let node_id = format!("tech:{}:{}", host_ip, tech_name);
        let label = match (version, category) {
            (Some(v), Some(c)) => format!("{} {} [{}]", tech_name, v, c),
            (Some(v), None) => format!("{} {}", tech_name, v),
            (None, Some(c)) => format!("{} [{}]", tech_name, c),
            (None, None) => tech_name.to_string(),
        };

        let host_id = format!("host:{}", host_ip);

        let graph = self.graph_mut();
        let _ = graph.add_node(&node_id, &label, GraphNodeType::Technology);
        let _ = graph.add_edge(&host_id, &node_id, GraphEdgeType::UsesTech, 1.0);
    }

    // ==================== Credential Emission ====================

    /// Emit a credential node
    ///
    /// # Arguments
    /// * `username` - Username
    /// * `password` - Password (will be stored but should be handled carefully)
    /// * `source_host` - Host where the credential was found
    pub fn emit_credential(
        &self,
        username: &str,
        password: Option<&str>,
        source_host: Option<&str>,
    ) {
        let node_id = format!("cred:{}:{}", username, password.unwrap_or(""));
        let label = format!(
            "{}:{}",
            username,
            password
                .map(|p| "*".repeat(p.len().min(8)))
                .unwrap_or_default()
        );

        {
            let graph = self.graph_mut();
            let _ = graph.add_node(&node_id, &label, GraphNodeType::Credential);
        }

        // Link to source host if specified
        if let Some(host) = source_host {
            self.ensure_host(host);
            let host_id = format!("host:{}", host);
            let graph = self.graph_mut();
            let _ = graph.add_edge(&host_id, &node_id, GraphEdgeType::HasUser, 1.0);
        }
    }

    /// Emit that a credential provides access to a host
    pub fn emit_credential_access(
        &self,
        username: &str,
        password: Option<&str>,
        target_host: &str,
    ) {
        self.ensure_host(target_host);

        let cred_id = format!("cred:{}:{}", username, password.unwrap_or(""));
        let host_id = format!("host:{}", target_host);

        let graph = self.graph_mut();
        let _ = graph.add_edge(&cred_id, &host_id, GraphEdgeType::AuthAccess, 1.0);
    }

    // ==================== User Emission ====================

    /// Emit a user node and link to a host
    pub fn emit_user(&self, username: &str, host_ip: &str, privileges: Option<&str>) {
        self.ensure_host(host_ip);

        let node_id = format!("user:{}@{}", username, host_ip);
        let label = match privileges {
            Some(p) => format!("{}@{} ({})", username, host_ip, p),
            None => format!("{}@{}", username, host_ip),
        };

        let host_id = format!("host:{}", host_ip);

        let graph = self.graph_mut();
        let _ = graph.add_node(&node_id, &label, GraphNodeType::User);
        let _ = graph.add_edge(&host_id, &node_id, GraphEdgeType::HasUser, 1.0);
    }

    // ==================== Domain Emission ====================

    /// Emit a domain node
    pub fn emit_domain(&self, domain: &str, parent: Option<&str>) {
        let node_id = format!("domain:{}", domain);

        {
            let graph = self.graph_mut();
            let _ = graph.add_node(&node_id, domain, GraphNodeType::Domain);
        }

        // Link to parent domain if specified
        if let Some(parent_domain) = parent {
            self.emit_domain(parent_domain, None);
            let parent_id = format!("domain:{}", parent_domain);
            let graph = self.graph_mut();
            let _ = graph.add_edge(&parent_id, &node_id, GraphEdgeType::RelatedTo, 1.0);
        }
    }

    /// Link a domain to a host (DNS resolution)
    pub fn emit_domain_host(&self, domain: &str, host_ip: &str) {
        self.emit_domain(domain, None);
        self.ensure_host(host_ip);

        let domain_id = format!("domain:{}", domain);
        let host_id = format!("host:{}", host_ip);

        let graph = self.graph_mut();
        let _ = graph.add_edge(&domain_id, &host_id, GraphEdgeType::RelatedTo, 1.0);
    }

    // ==================== Endpoint Emission ====================

    /// Emit an endpoint (URL path)
    pub fn emit_endpoint(&self, host_ip: &str, method: &str, path: &str, status: Option<u16>) {
        self.ensure_host(host_ip);

        let node_id = format!("endpoint:{}:{}:{}", host_ip, method, path);
        let label = match status {
            Some(s) => format!("{} {} → {}", method, path, s),
            None => format!("{} {}", method, path),
        };

        let host_id = format!("host:{}", host_ip);

        let graph = self.graph_mut();
        let _ = graph.add_node(&node_id, &label, GraphNodeType::Endpoint);
        let _ = graph.add_edge(&host_id, &node_id, GraphEdgeType::RelatedTo, 1.0);
    }

    // ==================== Certificate Emission ====================

    /// Emit a certificate node
    pub fn emit_certificate(&self, subject: &str, issuer: &str, expiry_days: Option<i64>) {
        let node_id = format!("cert:{}", subject);
        let label = match expiry_days {
            Some(d) if d < 0 => format!("{} (EXPIRED) [issuer: {}]", subject, issuer),
            Some(d) if d < 30 => {
                format!("{} (expires in {} days) [issuer: {}]", subject, d, issuer)
            }
            Some(d) => format!("{} ({} days) [issuer: {}]", subject, d, issuer),
            None => format!("{} [issuer: {}]", subject, issuer),
        };

        let graph = self.graph_mut();
        let _ = graph.add_node(&node_id, &label, GraphNodeType::Certificate);
    }

    /// Link a certificate to a host
    pub fn emit_host_certificate(&self, host_ip: &str, subject: &str) {
        self.ensure_host(host_ip);

        let host_id = format!("host:{}", host_ip);
        let cert_id = format!("cert:{}", subject);

        let graph = self.graph_mut();
        let _ = graph.add_edge(&host_id, &cert_id, GraphEdgeType::HasCert, 1.0);
    }

    // ==================== Network Topology ====================

    /// Emit a connection between two hosts
    pub fn emit_host_connection(&self, from_ip: &str, to_ip: &str, protocol: Option<&str>) {
        self.ensure_host(from_ip);
        self.ensure_host(to_ip);

        let from_id = format!("host:{}", from_ip);
        let to_id = format!("host:{}", to_ip);

        // Protocol info is encoded in the weight for now (1.0 = default, others reserved)
        let weight = match protocol {
            Some("tcp") => 1.0,
            Some("udp") => 2.0,
            Some("icmp") => 3.0,
            _ => 1.0,
        };

        let graph = self.graph_mut();
        let _ = graph.add_edge(&from_id, &to_id, GraphEdgeType::ConnectsTo, weight);
    }

    // ==================== Batch Operations ====================

    /// Emit multiple hosts from a scan result
    pub fn emit_scan_results<'a, I>(&self, results: I)
    where
        I: IntoIterator<Item = &'a ScanResult>,
    {
        for result in results {
            self.emit_host(&result.host_ip, None, result.hostname.as_deref());

            for service in &result.services {
                if let Some(ref banner) = service.banner {
                    self.emit_service_with_banner(
                        &result.host_ip,
                        service.port,
                        &service.name,
                        service.version.as_deref(),
                        banner,
                    );
                } else {
                    self.emit_service(
                        &result.host_ip,
                        service.port,
                        &service.name,
                        service.version.as_deref(),
                    );
                }
            }
        }
    }

    // ==================== Helper Methods ====================

    fn ensure_host(&self, ip: &str) {
        let node_id = format!("host:{}", ip);
        let exists = {
            let graph = self.graph();
            graph.has_node(&node_id)
        };
        if exists {
            return;
        }
        self.emit_host(ip, None, None);
    }

    fn ensure_vuln(&self, cve: &str) {
        let node_id = format!("vuln:{}", cve);
        let exists = {
            let graph = self.graph();
            graph.has_node(&node_id)
        };
        if exists {
            return;
        }
        self.emit_vulnerability(cve, 0.0, None);
    }

    // ==================== Statistics ====================

    /// Get current graph statistics
    pub fn stats(&self) -> EmitterStats {
        let graph = self.graph();
        let mut stats = EmitterStats::default();

        for node in graph.iter_nodes() {
            match node.node_type {
                GraphNodeType::Host => stats.hosts += 1,
                GraphNodeType::Service => stats.services += 1,
                GraphNodeType::Vulnerability => stats.vulnerabilities += 1,
                GraphNodeType::Credential => stats.credentials += 1,
                GraphNodeType::User => stats.users += 1,
                GraphNodeType::Technology => stats.technologies += 1,
                GraphNodeType::Domain => stats.domains += 1,
                GraphNodeType::Endpoint => stats.endpoints += 1,
                GraphNodeType::Certificate => stats.certificates += 1,
            }
        }

        stats.total_nodes = graph.node_count() as usize;
        stats.total_edges = graph.edge_count() as usize;

        stats
    }
}

impl Default for GraphEmitter {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics from the emitter
#[derive(Debug, Default, Clone)]
pub struct EmitterStats {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub hosts: usize,
    pub services: usize,
    pub vulnerabilities: usize,
    pub credentials: usize,
    pub users: usize,
    pub technologies: usize,
    pub domains: usize,
    pub endpoints: usize,
    pub certificates: usize,
}

/// Scan result structure for batch emission
#[derive(Debug, Clone)]
pub struct ScanResult {
    pub host_ip: String,
    pub hostname: Option<String>,
    pub services: Vec<ServiceResult>,
}

/// Service result for batch emission
#[derive(Debug, Clone)]
pub struct ServiceResult {
    pub port: u16,
    pub name: String,
    pub version: Option<String>,
    pub banner: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emit_host() {
        let emitter = GraphEmitter::new();
        emitter.emit_host("192.168.1.1", Some("linux"), Some("server1"));

        let stats = emitter.stats();
        assert_eq!(stats.hosts, 1);
    }

    #[test]
    fn test_emit_service() {
        let emitter = GraphEmitter::new();
        emitter.emit_service("192.168.1.1", 22, "ssh", Some("OpenSSH 8.9"));

        let stats = emitter.stats();
        assert_eq!(stats.hosts, 1); // Auto-created
        assert_eq!(stats.services, 1);
        assert_eq!(stats.total_edges, 1);
    }

    #[test]
    fn test_emit_vulnerability() {
        let emitter = GraphEmitter::new();
        emitter.emit_host("192.168.1.1", None, None);
        emitter.emit_vulnerability("CVE-2021-44228", 10.0, Some("Log4Shell"));
        emitter.emit_host_vuln("192.168.1.1", "CVE-2021-44228");

        let stats = emitter.stats();
        assert_eq!(stats.hosts, 1);
        assert_eq!(stats.vulnerabilities, 1);
        assert_eq!(stats.total_edges, 1);
    }

    #[test]
    fn test_batch_emission() {
        let emitter = GraphEmitter::new();

        let results = vec![ScanResult {
            host_ip: "192.168.1.1".to_string(),
            hostname: Some("server1".to_string()),
            services: vec![
                ServiceResult {
                    port: 22,
                    name: "ssh".to_string(),
                    version: Some("OpenSSH 8.9".to_string()),
                    banner: Some("SSH-2.0-OpenSSH_8.9".to_string()),
                },
                ServiceResult {
                    port: 80,
                    name: "http".to_string(),
                    version: Some("nginx/1.18.0".to_string()),
                    banner: None,
                },
            ],
        }];

        emitter.emit_scan_results(&results);

        let stats = emitter.stats();
        assert_eq!(stats.hosts, 1);
        assert_eq!(stats.services, 2);
    }

    #[test]
    fn test_emit_credential() {
        let emitter = GraphEmitter::new();
        emitter.emit_credential("admin", Some("password123"), Some("192.168.1.1"));
        emitter.emit_credential_access("admin", Some("password123"), "192.168.1.2");

        let stats = emitter.stats();
        assert_eq!(stats.credentials, 1);
        assert_eq!(stats.hosts, 2); // Source and target hosts
    }

    #[test]
    fn test_emit_domain() {
        let emitter = GraphEmitter::new();
        emitter.emit_domain("www.example.com", Some("example.com"));
        emitter.emit_domain_host("example.com", "93.184.216.34");

        let stats = emitter.stats();
        assert_eq!(stats.domains, 2);
        assert_eq!(stats.hosts, 1);
    }

    #[test]
    fn test_graph_access_recovers_after_lock_poisoning() {
        let emitter = std::sync::Arc::new(GraphEmitter::new());
        let poison_target = std::sync::Arc::clone(&emitter);
        let _ = std::thread::spawn(move || {
            let _guard = poison_target
                .graph
                .write()
                .expect("graph lock should be acquired");
            panic!("poison graph lock");
        })
        .join();

        {
            let graph = emitter.graph();
            assert_eq!(graph.node_count(), 0);
        }

        {
            emitter.emit_host("127.0.0.1", None, None);
        }

        assert_eq!(emitter.stats().hosts, 1);
    }
}
