//! Graph Segment - Persistent storage for attack path analysis graph.
//!
//! This segment implements the ShadowGraph storage layer, storing nodes and edges
//! that represent the relationships between hosts, services, credentials, and vulnerabilities.
//! Inspired by the genetics-ai.js graph implementation and PentestAgent's ShadowGraph.

use std::collections::HashMap;
use std::sync::Arc;

use super::actions::{ActionOutcome, ActionRecord, RecordPayload, Target};
use crate::storage::primitives::encoding::{
    read_bytes, read_string, read_varu32, write_bytes, write_string, write_varu32, DecodeError,
};

// ==================== Graph Node Types ====================

/// Type of graph node
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeType {
    /// Target host/IP
    Host = 0,
    /// Network service (SSH, HTTP, SMB, etc.)
    Service = 1,
    /// Authentication credential
    Credential = 2,
    /// Security vulnerability
    Vulnerability = 3,
    /// Web endpoint/URL
    Endpoint = 4,
    /// Technology/framework
    Technology = 5,
    /// Network (CIDR range)
    Network = 6,
    /// Domain name
    Domain = 7,
    /// Attack chain (playbook execution)
    AttackChain = 8,
}

impl NodeType {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Host),
            1 => Some(Self::Service),
            2 => Some(Self::Credential),
            3 => Some(Self::Vulnerability),
            4 => Some(Self::Endpoint),
            5 => Some(Self::Technology),
            6 => Some(Self::Network),
            7 => Some(Self::Domain),
            8 => Some(Self::AttackChain),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Service => "service",
            Self::Credential => "credential",
            Self::Vulnerability => "vulnerability",
            Self::Endpoint => "endpoint",
            Self::Technology => "technology",
            Self::Network => "network",
            Self::Domain => "domain",
            Self::AttackChain => "attack_chain",
        }
    }
}

/// Type of graph edge relationship
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeType {
    /// Host has a service (host → service)
    HasService = 0,
    /// Host has an endpoint (host → endpoint)
    HasEndpoint = 1,
    /// Host uses technology (host → technology)
    UsesTech = 2,
    /// Credential provides access (credential → host)
    AuthAccess = 3,
    /// Host affected by vulnerability (host → vulnerability)
    AffectedBy = 4,
    /// Host contains credential (host → credential)
    Contains = 5,
    /// Host connects to host (host → host)
    ConnectsTo = 6,
    /// Generic relationship
    RelatedTo = 7,
    /// Attack path (chain → target)
    AttackPath = 8,
}

impl EdgeType {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::HasService),
            1 => Some(Self::HasEndpoint),
            2 => Some(Self::UsesTech),
            3 => Some(Self::AuthAccess),
            4 => Some(Self::AffectedBy),
            5 => Some(Self::Contains),
            6 => Some(Self::ConnectsTo),
            7 => Some(Self::RelatedTo),
            8 => Some(Self::AttackPath),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HasService => "has_service",
            Self::HasEndpoint => "has_endpoint",
            Self::UsesTech => "uses_tech",
            Self::AuthAccess => "auth_access",
            Self::AffectedBy => "affected_by",
            Self::Contains => "contains",
            Self::ConnectsTo => "connects_to",
            Self::RelatedTo => "related_to",
            Self::AttackPath => "attack_path",
        }
    }

    /// Default weight for edge type (lower = more preferred path)
    pub fn default_weight(&self) -> f32 {
        match self {
            Self::AuthAccess => 1.0,  // Direct auth is highly preferred
            Self::Contains => 1.5,    // Finding creds on a host
            Self::HasService => 2.0,  // Service enumeration
            Self::HasEndpoint => 2.0, // Endpoint enumeration
            Self::AffectedBy => 2.5,  // Vulnerability exploitation
            Self::UsesTech => 3.0,    // Technology stack
            Self::ConnectsTo => 3.0,  // Network pivot
            Self::RelatedTo => 5.0,   // Generic (least preferred)
            Self::AttackPath => 1.5,  // Attack chain connections (preferred for path analysis)
        }
    }
}

// ==================== Strategic Insights ====================

/// Types of strategic insights derived from graph analysis
#[derive(Debug, Clone)]
pub enum StrategicInsight {
    /// Credential exists but not used to access any host
    UnusedCredential {
        credential_id: String,
        username: String,
    },

    /// Host with multiple vulnerabilities or exposed services
    HighValueTarget {
        host_id: String,
        host: String,
        vuln_count: usize,
        service_count: usize,
        score: f32,
    },

    /// Path from credential/vuln to target host
    AttackPath {
        from: String,
        to: String,
        hops: Vec<String>,
        total_weight: f32,
    },

    /// Credential can reach multiple hosts (pivot opportunity)
    LateralMovement {
        credential_id: String,
        reachable_hosts: Vec<String>,
    },

    /// Hosts with no scan coverage
    CoverageGap {
        host_id: String,
        host: String,
        missing: Vec<String>, // What's missing: "port_scan", "vuln_check", "tech_fingerprint"
    },
}

impl StrategicInsight {
    /// Get a short description of the insight
    pub fn summary(&self) -> String {
        match self {
            Self::UnusedCredential { username, .. } => {
                format!("Unused credential: {}", username)
            }
            Self::HighValueTarget {
                host,
                vuln_count,
                service_count,
                score,
                ..
            } => {
                format!(
                    "High-value target: {} ({} vulns, {} services, score: {:.1})",
                    host, vuln_count, service_count, score
                )
            }
            Self::AttackPath { from, to, hops, .. } => {
                format!("Attack path: {} → {} ({} hops)", from, to, hops.len())
            }
            Self::LateralMovement {
                credential_id,
                reachable_hosts,
            } => {
                format!(
                    "Lateral movement via {}: {} hosts reachable",
                    credential_id,
                    reachable_hosts.len()
                )
            }
            Self::CoverageGap { host, missing, .. } => {
                format!("Coverage gap: {} missing {}", host, missing.join(", "))
            }
        }
    }

    /// Get severity/priority (higher = more important)
    pub fn priority(&self) -> u8 {
        match self {
            Self::AttackPath { hops, .. } if hops.len() <= 2 => 100,
            Self::LateralMovement {
                reachable_hosts, ..
            } if reachable_hosts.len() >= 3 => 90,
            Self::HighValueTarget { score, .. } if *score >= 8.0 => 85,
            Self::AttackPath { .. } => 70,
            Self::LateralMovement { .. } => 60,
            Self::HighValueTarget { .. } => 50,
            Self::UnusedCredential { .. } => 40,
            Self::CoverageGap { .. } => 30,
        }
    }
}

// ==================== Graph Node ====================

/// Reference to an edge (stored in adjacency lists)
#[derive(Debug, Clone)]
pub struct EdgeRef {
    /// Target node ID
    pub target_id: String,
    /// Type of relationship
    pub edge_type: EdgeType,
    /// Weight for path finding (lower = preferred)
    pub weight: f32,
}

impl EdgeRef {
    fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        write_string(&mut buf, &self.target_id);
        buf.push(self.edge_type as u8);
        buf.extend_from_slice(&self.weight.to_bits().to_le_bytes());
        buf
    }

    fn from_bytes(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let target_id = read_string(bytes, pos)?.to_string();
        if *pos >= bytes.len() {
            return Err(DecodeError("truncated edge type"));
        }
        let edge_type = EdgeType::from_u8(bytes[*pos]).ok_or(DecodeError("invalid edge type"))?;
        *pos += 1;
        if *pos + 4 > bytes.len() {
            return Err(DecodeError("truncated edge weight"));
        }
        let weight_bits = u32::from_le_bytes([
            bytes[*pos],
            bytes[*pos + 1],
            bytes[*pos + 2],
            bytes[*pos + 3],
        ]);
        *pos += 4;
        let weight = f32::from_bits(weight_bits);
        Ok(Self {
            target_id,
            edge_type,
            weight,
        })
    }
}

/// A node in the attack path graph
#[derive(Debug, Clone)]
pub struct GraphNode {
    /// Unique node ID (format: "{type}:{identifier}", e.g., "host:192.168.1.1")
    pub id: String,
    /// Type of node
    pub node_type: NodeType,
    /// Human-readable label
    pub label: String,
    /// Serialized metadata (JSON-like)
    pub metadata: Vec<u8>,
    /// Incoming edges (nodes pointing to this node)
    pub in_edges: Vec<EdgeRef>,
    /// Outgoing edges (this node points to)
    pub out_edges: Vec<EdgeRef>,
    /// Cache generation for traversal optimization (from genetics-ai)
    pub cache_generation: u64,
    /// Cached computed value for this generation
    pub cache_value: f64,
    /// Depth in the graph (for topological ordering)
    pub depth: usize,
}

impl GraphNode {
    /// Create a new graph node
    pub fn new(id: impl Into<String>, node_type: NodeType, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            node_type,
            label: label.into(),
            metadata: Vec::new(),
            in_edges: Vec::new(),
            out_edges: Vec::new(),
            cache_generation: 0,
            cache_value: 0.0,
            depth: 0,
        }
    }

    /// Create a host node
    pub fn host(ip: &str) -> Self {
        Self::new(format!("host:{}", ip), NodeType::Host, ip.to_string())
    }

    /// Create a service node
    pub fn service(host: &str, port: u16, name: &str) -> Self {
        Self::new(
            format!("service:{}:{}:{}", host, port, name),
            NodeType::Service,
            format!("{}:{} ({})", host, port, name),
        )
    }

    /// Create a credential node
    pub fn credential(key: &str, username: &str) -> Self {
        Self::new(
            format!("cred:{}", key),
            NodeType::Credential,
            format!("Creds: {}", username),
        )
    }

    /// Create a vulnerability node
    pub fn vulnerability(cve: &str) -> Self {
        Self::new(
            format!("vuln:{}", cve),
            NodeType::Vulnerability,
            cve.to_string(),
        )
    }

    /// Check if this node has an outgoing edge to target
    pub fn has_edge_to(&self, target_id: &str) -> bool {
        self.out_edges.iter().any(|e| e.target_id == target_id)
    }

    /// Check if this node has an incoming edge from source
    pub fn has_edge_from(&self, source_id: &str) -> bool {
        self.in_edges.iter().any(|e| e.target_id == source_id)
    }

    /// Get cached or calculate value (genetics-ai pattern)
    pub fn get_cached_or_compute<F>(&mut self, generation: u64, compute: F) -> f64
    where
        F: FnOnce(&Self) -> f64,
    {
        if self.cache_generation == generation {
            return self.cache_value;
        }
        self.cache_value = compute(self);
        self.cache_generation = generation;
        self.cache_value
    }

    /// Serialize to binary format
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        write_string(&mut buf, &self.id);
        buf.push(self.node_type as u8);
        write_string(&mut buf, &self.label);
        write_bytes(&mut buf, &self.metadata);

        // In edges
        write_varu32(&mut buf, self.in_edges.len() as u32);
        for edge in &self.in_edges {
            let edge_bytes = edge.to_bytes();
            write_varu32(&mut buf, edge_bytes.len() as u32);
            buf.extend_from_slice(&edge_bytes);
        }

        // Out edges
        write_varu32(&mut buf, self.out_edges.len() as u32);
        for edge in &self.out_edges {
            let edge_bytes = edge.to_bytes();
            write_varu32(&mut buf, edge_bytes.len() as u32);
            buf.extend_from_slice(&edge_bytes);
        }

        // Depth (for topological order)
        write_varu32(&mut buf, self.depth as u32);

        buf
    }

    /// Deserialize from binary format
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut pos = 0usize;

        let id = read_string(bytes, &mut pos)?.to_string();

        if pos >= bytes.len() {
            return Err(DecodeError("truncated node type"));
        }
        let node_type = NodeType::from_u8(bytes[pos]).ok_or(DecodeError("invalid node type"))?;
        pos += 1;

        let label = read_string(bytes, &mut pos)?.to_string();
        let metadata = read_bytes(bytes, &mut pos)?.to_vec();

        // In edges
        let in_edge_count = read_varu32(bytes, &mut pos)? as usize;
        let mut in_edges = Vec::with_capacity(in_edge_count);
        for _ in 0..in_edge_count {
            let edge_len = read_varu32(bytes, &mut pos)? as usize;
            if pos + edge_len > bytes.len() {
                return Err(DecodeError("truncated in edge"));
            }
            let edge = EdgeRef::from_bytes(bytes, &mut pos)?;
            in_edges.push(edge);
        }

        // Out edges
        let out_edge_count = read_varu32(bytes, &mut pos)? as usize;
        let mut out_edges = Vec::with_capacity(out_edge_count);
        for _ in 0..out_edge_count {
            let edge_len = read_varu32(bytes, &mut pos)? as usize;
            if pos + edge_len > bytes.len() {
                return Err(DecodeError("truncated out edge"));
            }
            let edge = EdgeRef::from_bytes(bytes, &mut pos)?;
            out_edges.push(edge);
        }

        let depth = read_varu32(bytes, &mut pos)? as usize;

        Ok(Self {
            id,
            node_type,
            label,
            metadata,
            in_edges,
            out_edges,
            cache_generation: 0,
            cache_value: 0.0,
            depth,
        })
    }
}

// ==================== Graph Edge ====================

/// A standalone edge record (for edge-centric queries)
#[derive(Debug, Clone)]
pub struct GraphEdge {
    /// Source node ID
    pub source: String,
    /// Target node ID
    pub target: String,
    /// Type of relationship
    pub edge_type: EdgeType,
    /// Weight for path finding
    pub weight: f32,
    /// Additional metadata
    pub metadata: Vec<u8>,
}

impl GraphEdge {
    pub fn new(source: impl Into<String>, target: impl Into<String>, edge_type: EdgeType) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            edge_type,
            weight: edge_type.default_weight(),
            metadata: Vec::new(),
        }
    }

    pub fn with_weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        write_string(&mut buf, &self.source);
        write_string(&mut buf, &self.target);
        buf.push(self.edge_type as u8);
        buf.extend_from_slice(&self.weight.to_bits().to_le_bytes());
        write_bytes(&mut buf, &self.metadata);
        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut pos = 0usize;
        let source = read_string(bytes, &mut pos)?.to_string();
        let target = read_string(bytes, &mut pos)?.to_string();

        if pos >= bytes.len() {
            return Err(DecodeError("truncated edge type"));
        }
        let edge_type = EdgeType::from_u8(bytes[pos]).ok_or(DecodeError("invalid edge type"))?;
        pos += 1;

        if pos + 4 > bytes.len() {
            return Err(DecodeError("truncated edge weight"));
        }
        let weight_bits =
            u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        pos += 4;
        let weight = f32::from_bits(weight_bits);

        let metadata = read_bytes(bytes, &mut pos)?.to_vec();

        Ok(Self {
            source,
            target,
            edge_type,
            weight,
            metadata,
        })
    }
}

// ==================== Graph Segment Directory ====================

#[derive(Debug, Clone)]
struct GraphNodeDirEntry {
    id_hash: u64,
    payload_offset: u64,
    payload_len: u64,
}

impl GraphNodeDirEntry {
    const SIZE: usize = 8 + 8 + 8;

    fn write_all(entries: &[Self], buf: &mut Vec<u8>) {
        for entry in entries {
            buf.extend_from_slice(&entry.id_hash.to_le_bytes());
            buf.extend_from_slice(&entry.payload_offset.to_le_bytes());
            buf.extend_from_slice(&entry.payload_len.to_le_bytes());
        }
    }

    fn read_all(bytes: &[u8], count: usize) -> Result<Vec<Self>, DecodeError> {
        if bytes.len() != count * Self::SIZE {
            return Err(DecodeError("invalid graph node directory size"));
        }
        let mut entries = Vec::with_capacity(count);
        let mut offset = 0usize;
        for _ in 0..count {
            let id_hash = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let payload_offset = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let payload_len = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;
            entries.push(Self {
                id_hash,
                payload_offset,
                payload_len,
            });
        }
        Ok(entries)
    }
}

// ==================== Graph Segment Header ====================

#[derive(Debug, Clone, Copy)]
struct GraphSegmentHeader {
    node_count: u32,
    directory_len: u64,
    payload_len: u64,
}

impl GraphSegmentHeader {
    const MAGIC: [u8; 4] = *b"GR01"; // Graph segment magic
    const VERSION: u16 = 1;
    const SIZE: usize = 4 + 2 + 2 + 4 + 8 + 8;

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&Self::MAGIC);
        buf.extend_from_slice(&Self::VERSION.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // reserved
        buf.extend_from_slice(&self.node_count.to_le_bytes());
        buf.extend_from_slice(&self.directory_len.to_le_bytes());
        buf.extend_from_slice(&self.payload_len.to_le_bytes());
    }

    fn read(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < Self::SIZE {
            return Err(DecodeError("graph header too small"));
        }
        if bytes[0..4] != Self::MAGIC {
            return Err(DecodeError("invalid graph segment magic"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != Self::VERSION {
            return Err(DecodeError("unsupported graph segment version"));
        }
        let node_count = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let directory_len = u64::from_le_bytes(bytes[12..20].try_into().unwrap());
        let payload_len = u64::from_le_bytes(bytes[20..28].try_into().unwrap());
        Ok(Self {
            node_count,
            directory_len,
            payload_len,
        })
    }
}

// ==================== Hash Function ====================

/// FNV-1a hash for string IDs
fn hash_id(id: &str) -> u64 {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;

    let mut hash = FNV_OFFSET;
    for byte in id.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

// ==================== Graph Segment ====================

/// Mutable graph segment for building/modifying the attack path graph
#[derive(Debug, Default, Clone)]
pub struct GraphSegment {
    nodes: HashMap<String, GraphNode>,
    sorted_ids: Vec<String>,
    sorted: bool,
}

impl GraphSegment {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            sorted_ids: Vec::new(),
            sorted: true,
        }
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Add or update a node
    pub fn add_node(&mut self, node: GraphNode) {
        let id = node.id.clone();
        self.nodes.insert(id.clone(), node);
        if !self.sorted_ids.contains(&id) {
            self.sorted_ids.push(id);
            self.sorted = false;
        }
    }

    /// Get a node by ID
    pub fn get_node(&self, id: &str) -> Option<&GraphNode> {
        self.nodes.get(id)
    }

    /// Get a mutable node by ID
    pub fn get_node_mut(&mut self, id: &str) -> Option<&mut GraphNode> {
        self.nodes.get_mut(id)
    }

    /// Add an edge between two nodes (creates nodes if they don't exist)
    pub fn add_edge(&mut self, edge: GraphEdge) {
        // Ensure source node exists
        if !self.nodes.contains_key(&edge.source) {
            // Create a placeholder node
            let node_type = Self::infer_node_type(&edge.source);
            self.add_node(GraphNode::new(&edge.source, node_type, &edge.source));
        }

        // Ensure target node exists
        if !self.nodes.contains_key(&edge.target) {
            let node_type = Self::infer_node_type(&edge.target);
            self.add_node(GraphNode::new(&edge.target, node_type, &edge.target));
        }

        // Add outgoing edge to source
        if let Some(source_node) = self.nodes.get_mut(&edge.source) {
            if !source_node.has_edge_to(&edge.target) {
                source_node.out_edges.push(EdgeRef {
                    target_id: edge.target.clone(),
                    edge_type: edge.edge_type,
                    weight: edge.weight,
                });
            }
        }

        // Add incoming edge to target
        if let Some(target_node) = self.nodes.get_mut(&edge.target) {
            if !target_node.has_edge_from(&edge.source) {
                target_node.in_edges.push(EdgeRef {
                    target_id: edge.source.clone(),
                    edge_type: edge.edge_type,
                    weight: edge.weight,
                });
            }
        }
    }

    /// Infer node type from ID prefix
    fn infer_node_type(id: &str) -> NodeType {
        if id.starts_with("host:") {
            NodeType::Host
        } else if id.starts_with("service:") {
            NodeType::Service
        } else if id.starts_with("cred:") {
            NodeType::Credential
        } else if id.starts_with("vuln:") {
            NodeType::Vulnerability
        } else if id.starts_with("endpoint:") {
            NodeType::Endpoint
        } else if id.starts_with("tech:") {
            NodeType::Technology
        } else {
            NodeType::Host // Default
        }
    }

    /// Get all nodes of a specific type
    pub fn nodes_of_type(&self, node_type: NodeType) -> Vec<&GraphNode> {
        self.nodes
            .values()
            .filter(|n| n.node_type == node_type)
            .collect()
    }

    /// Get all nodes
    pub fn all_nodes(&self) -> Vec<&GraphNode> {
        self.nodes.values().collect()
    }

    /// Get all node IDs
    pub fn all_node_ids(&self) -> Vec<&String> {
        self.nodes.keys().collect()
    }

    /// Remove a node and its edges
    pub fn remove_node(&mut self, id: &str) -> Option<GraphNode> {
        if let Some(node) = self.nodes.remove(id) {
            // Remove edges pointing to this node
            for other in self.nodes.values_mut() {
                other.out_edges.retain(|e| e.target_id != id);
                other.in_edges.retain(|e| e.target_id != id);
            }
            self.sorted_ids.retain(|s| s != id);
            Some(node)
        } else {
            None
        }
    }

    /// Count edges by type
    pub fn count_edges_by_type(&self) -> HashMap<EdgeType, usize> {
        let mut counts = HashMap::new();
        for node in self.nodes.values() {
            for edge in &node.out_edges {
                *counts.entry(edge.edge_type).or_insert(0) += 1;
            }
        }
        counts
    }

    /// Total edge count
    pub fn edge_count(&self) -> usize {
        self.nodes.values().map(|n| n.out_edges.len()).sum()
    }

    // ==================== Action Integration ====================

    /// Update graph from an ActionRecord
    ///
    /// Creates nodes for hosts, services, vulnerabilities, technologies
    /// and edges representing their relationships.
    pub fn update_from_action(&mut self, action: &ActionRecord) {
        // Only process successful or partial outcomes
        if !matches!(
            action.outcome,
            ActionOutcome::Success | ActionOutcome::Partial { .. }
        ) {
            return;
        }

        // Get or create host node from target
        let host_id = self.ensure_host_from_target(&action.target);

        // Process payload-specific graph updates
        match &action.payload {
            RecordPayload::PortScan(data) => {
                // Create service nodes for open ports
                for port in &data.open_ports {
                    // Map common ports to service names
                    let service_name = match port {
                        21 => "ftp",
                        22 => "ssh",
                        23 => "telnet",
                        25 => "smtp",
                        53 => "dns",
                        80 => "http",
                        110 => "pop3",
                        143 => "imap",
                        443 => "https",
                        445 => "smb",
                        993 => "imaps",
                        995 => "pop3s",
                        1433 => "mssql",
                        3306 => "mysql",
                        3389 => "rdp",
                        5432 => "postgresql",
                        6379 => "redis",
                        8080 | 8443 => "http-alt",
                        27017 => "mongodb",
                        _ => "unknown",
                    };

                    let service_id = if let Some(ip) = action.target.ip() {
                        format!("service:{}:{}:{}", ip, port, service_name)
                    } else {
                        format!(
                            "service:{}:{}:{}",
                            action.target.host_str(),
                            port,
                            service_name
                        )
                    };

                    let label = format!("{}:{} ({})", action.target.host_str(), port, service_name);
                    self.add_node(GraphNode::new(&service_id, NodeType::Service, label));
                    self.add_edge(GraphEdge::new(&host_id, &service_id, EdgeType::HasService));
                }
            }

            RecordPayload::Vuln(data) => {
                // Create vulnerability node
                let vuln_id = if let Some(ref cve) = data.cve {
                    format!("vuln:{}", cve)
                } else {
                    format!("vuln:{}", data.title.replace(' ', "_").to_lowercase())
                };

                let severity_str = match data.severity {
                    0 => "Info",
                    1 => "Low",
                    2 => "Medium",
                    3 => "High",
                    4 => "Critical",
                    _ => "Unknown",
                };
                let label = format!("{} ({})", data.title, severity_str);
                self.add_node(GraphNode::new(&vuln_id, NodeType::Vulnerability, label));

                // Edge: host affected by vuln
                self.add_edge(GraphEdge::new(&host_id, &vuln_id, EdgeType::AffectedBy));
            }

            RecordPayload::Tls(data) => {
                // Create technology nodes for TLS info
                let tech_id = format!("tech:tls:{}", data.version.replace(' ', "_").to_lowercase());
                let label = format!("TLS {}", data.version);
                self.add_node(GraphNode::new(&tech_id, NodeType::Technology, label));
                self.add_edge(GraphEdge::new(&host_id, &tech_id, EdgeType::UsesTech));

                // Add cipher as tech node if meaningful
                if !data.cipher.is_empty() {
                    let cipher_id = format!(
                        "tech:cipher:{}",
                        data.cipher.replace(' ', "_").to_lowercase()
                    );
                    self.add_node(GraphNode::new(
                        &cipher_id,
                        NodeType::Technology,
                        &data.cipher,
                    ));
                    self.add_edge(GraphEdge::new(&host_id, &cipher_id, EdgeType::UsesTech));
                }
            }

            RecordPayload::Http(data) => {
                // Create endpoint node for the URL
                let endpoint_id = format!(
                    "endpoint:{}",
                    action.target.host_str().replace([':', '/'], "_")
                );
                let label = format!("HTTP {} {}", data.status_code, action.target.host_str());
                self.add_node(GraphNode::new(&endpoint_id, NodeType::Endpoint, label));
                self.add_edge(GraphEdge::new(
                    &host_id,
                    &endpoint_id,
                    EdgeType::HasEndpoint,
                ));

                // Check for server header → technology
                if let Some((_, server)) = data
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("server"))
                {
                    let tech_id = format!(
                        "tech:server:{}",
                        server.replace(['/', ' '], "_").to_lowercase()
                    );
                    self.add_node(GraphNode::new(&tech_id, NodeType::Technology, server));
                    self.add_edge(GraphEdge::new(&host_id, &tech_id, EdgeType::UsesTech));
                }
            }

            RecordPayload::Fingerprint(data) => {
                // Create technology node from fingerprint
                let version_suffix = data
                    .version
                    .as_ref()
                    .map(|v| format!(":{}", v.replace(['/', ' ', '.'], "_")))
                    .unwrap_or_default();
                let tech_id = format!(
                    "tech:{}{}",
                    data.service.replace(['/', ' ', '.'], "_").to_lowercase(),
                    version_suffix.to_lowercase()
                );
                let label = if let Some(ref v) = data.version {
                    format!("{} {}", data.service, v)
                } else {
                    data.service.clone()
                };
                self.add_node(GraphNode::new(&tech_id, NodeType::Technology, label));
                self.add_edge(GraphEdge::new(&host_id, &tech_id, EdgeType::UsesTech));

                // Add OS as technology if present
                if let Some(ref os) = data.os {
                    let os_id = format!(
                        "tech:os:{}",
                        os.replace(['/', ' ', '.'], "_").to_lowercase()
                    );
                    self.add_node(GraphNode::new(&os_id, NodeType::Technology, os));
                    self.add_edge(GraphEdge::new(&host_id, &os_id, EdgeType::UsesTech));
                }
            }

            RecordPayload::Dns(data) => {
                // DNS creates relationships between domains and IPs
                // For now, just note the relationship in metadata
                for record in &data.records {
                    // If record looks like an IP, create ConnectsTo edge
                    if record.parse::<std::net::IpAddr>().is_ok() {
                        let resolved_host_id = format!("host:{}", record);
                        self.add_node(GraphNode::host(record));
                        self.add_edge(GraphEdge::new(
                            &host_id,
                            &resolved_host_id,
                            EdgeType::ConnectsTo,
                        ));
                    }
                }
            }

            RecordPayload::Whois(_) | RecordPayload::Ping(_) | RecordPayload::Custom(_) => {
                // These don't create additional graph relationships
            }
        }
    }

    /// Ensure a host node exists for the given target, return its ID
    fn ensure_host_from_target(&mut self, target: &Target) -> String {
        let host_id = match target {
            Target::Host(ip) => format!("host:{}", ip),
            Target::Network(ip, prefix) => format!("host:{}/{}", ip, prefix),
            Target::Domain(d) => format!("host:{}", d),
            Target::Url(u) => {
                // Extract host from URL
                let host = u
                    .split("://")
                    .nth(1)
                    .unwrap_or(u)
                    .split('/')
                    .next()
                    .unwrap_or(u);
                format!("host:{}", host)
            }
            Target::Port(ip, _) => format!("host:{}", ip),
            Target::Service(ip, _, _) => format!("host:{}", ip),
        };

        if !self.nodes.contains_key(&host_id) {
            let label = match target {
                Target::Host(ip) => ip.to_string(),
                Target::Network(ip, prefix) => format!("{}/{}", ip, prefix),
                Target::Domain(d) => d.clone(),
                Target::Url(u) => u
                    .split("://")
                    .nth(1)
                    .unwrap_or(u)
                    .split('/')
                    .next()
                    .unwrap_or(u)
                    .to_string(),
                Target::Port(ip, _) => ip.to_string(),
                Target::Service(ip, _, _) => ip.to_string(),
            };
            self.add_node(GraphNode::new(&host_id, NodeType::Host, label));
        }

        host_id
    }

    /// Batch update graph from multiple actions
    pub fn update_from_actions(&mut self, actions: &[ActionRecord]) {
        for action in actions {
            self.update_from_action(action);
        }
    }

    // ==================== Strategic Insights ====================

    /// Get all strategic insights from the graph
    pub fn get_strategic_insights(&self) -> Vec<StrategicInsight> {
        let mut insights = Vec::new();

        // 1. Unused credentials
        insights.extend(self.find_unused_credentials());

        // 2. High-value targets
        insights.extend(self.find_high_value_targets());

        // 3. Lateral movement opportunities
        insights.extend(self.find_lateral_movement());

        // 4. Coverage gaps
        insights.extend(self.find_coverage_gaps());

        // Sort by priority (highest first)
        insights.sort_by(|a, b| b.priority().cmp(&a.priority()));

        insights
    }

    /// Find credentials that exist but aren't used to access any host
    fn find_unused_credentials(&self) -> Vec<StrategicInsight> {
        self.nodes_of_type(NodeType::Credential)
            .iter()
            .filter(|cred| {
                // Credential is unused if it has no AuthAccess edges
                !cred
                    .out_edges
                    .iter()
                    .any(|e| e.edge_type == EdgeType::AuthAccess)
            })
            .map(|cred| {
                let username = cred.label.replace("Creds: ", "");
                StrategicInsight::UnusedCredential {
                    credential_id: cred.id.clone(),
                    username,
                }
            })
            .collect()
    }

    /// Find hosts with multiple vulnerabilities or exposed services
    fn find_high_value_targets(&self) -> Vec<StrategicInsight> {
        self.nodes_of_type(NodeType::Host)
            .iter()
            .filter_map(|host| {
                let vuln_count = host
                    .out_edges
                    .iter()
                    .filter(|e| e.edge_type == EdgeType::AffectedBy)
                    .count();

                let service_count = host
                    .out_edges
                    .iter()
                    .filter(|e| e.edge_type == EdgeType::HasService)
                    .count();

                // Score: vulns are weighted higher
                let score = (vuln_count as f32 * 2.0) + (service_count as f32 * 0.5);

                // Only report if there's something interesting
                if vuln_count > 0 || service_count >= 3 {
                    Some(StrategicInsight::HighValueTarget {
                        host_id: host.id.clone(),
                        host: host.label.clone(),
                        vuln_count,
                        service_count,
                        score,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Find credentials that can reach multiple hosts
    fn find_lateral_movement(&self) -> Vec<StrategicInsight> {
        self.nodes_of_type(NodeType::Credential)
            .iter()
            .filter_map(|cred| {
                let reachable: Vec<String> = cred
                    .out_edges
                    .iter()
                    .filter(|e| e.edge_type == EdgeType::AuthAccess)
                    .map(|e| e.target_id.clone())
                    .collect();

                if reachable.len() >= 2 {
                    Some(StrategicInsight::LateralMovement {
                        credential_id: cred.id.clone(),
                        reachable_hosts: reachable,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Find hosts with incomplete scan coverage
    fn find_coverage_gaps(&self) -> Vec<StrategicInsight> {
        self.nodes_of_type(NodeType::Host)
            .iter()
            .filter_map(|host| {
                let mut missing = Vec::new();

                // Check for port scan coverage (has services)
                let has_services = host
                    .out_edges
                    .iter()
                    .any(|e| e.edge_type == EdgeType::HasService);
                if !has_services {
                    missing.push("port_scan".to_string());
                }

                // Check for vuln assessment
                let has_vuln_check = host
                    .out_edges
                    .iter()
                    .any(|e| e.edge_type == EdgeType::AffectedBy);
                if !has_vuln_check {
                    missing.push("vuln_check".to_string());
                }

                // Check for tech fingerprinting
                let has_tech = host
                    .out_edges
                    .iter()
                    .any(|e| e.edge_type == EdgeType::UsesTech);
                if !has_tech {
                    missing.push("tech_fingerprint".to_string());
                }

                if !missing.is_empty() && missing.len() < 3 {
                    // Only report partial gaps (some coverage, some missing)
                    Some(StrategicInsight::CoverageGap {
                        host_id: host.id.clone(),
                        host: host.label.clone(),
                        missing,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Find shortest path between two nodes using Dijkstra's algorithm
    pub fn find_attack_path(&self, from: &str, to: &str) -> Option<StrategicInsight> {
        // Dijkstra's algorithm
        let mut distances: HashMap<String, f32> = HashMap::new();
        let mut previous: HashMap<String, String> = HashMap::new();
        let mut unvisited: Vec<&str> = self.nodes.keys().map(|s| s.as_str()).collect();

        // Initialize distances
        for id in &unvisited {
            distances.insert(id.to_string(), f32::INFINITY);
        }
        distances.insert(from.to_string(), 0.0);

        while !unvisited.is_empty() {
            // Find node with minimum distance
            let current = unvisited
                .iter()
                .min_by(|a, b| {
                    let da = distances.get(&a.to_string()).unwrap_or(&f32::INFINITY);
                    let db = distances.get(&b.to_string()).unwrap_or(&f32::INFINITY);
                    da.partial_cmp(db).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|s| s.to_string())?;

            // Remove from unvisited
            unvisited.retain(|s| *s != current);

            // If we've reached the target, build the path
            if current == to {
                let mut path = Vec::new();
                let mut node = to.to_string();
                while let Some(prev) = previous.get(&node) {
                    path.push(node.clone());
                    node = prev.clone();
                }
                path.push(from.to_string());
                path.reverse();

                let total_weight = *distances.get(to).unwrap_or(&0.0);

                return Some(StrategicInsight::AttackPath {
                    from: from.to_string(),
                    to: to.to_string(),
                    hops: path,
                    total_weight,
                });
            }

            // Check neighbors
            if let Some(node) = self.nodes.get(&current) {
                for edge in &node.out_edges {
                    if unvisited.contains(&edge.target_id.as_str()) {
                        let current_dist = *distances.get(&current).unwrap_or(&f32::INFINITY);
                        let new_dist = current_dist + edge.weight;
                        let old_dist = *distances.get(&edge.target_id).unwrap_or(&f32::INFINITY);

                        if new_dist < old_dist {
                            distances.insert(edge.target_id.clone(), new_dist);
                            previous.insert(edge.target_id.clone(), current.clone());
                        }
                    }
                }
            }
        }

        None // No path found
    }

    /// Find all attack paths from any credential/vuln to a target host
    pub fn find_all_attack_paths(&self, target_host: &str) -> Vec<StrategicInsight> {
        let mut paths = Vec::new();

        // From credentials
        for cred in self.nodes_of_type(NodeType::Credential) {
            if let Some(path) = self.find_attack_path(&cred.id, target_host) {
                paths.push(path);
            }
        }

        // From vulnerabilities
        for vuln in self.nodes_of_type(NodeType::Vulnerability) {
            if let Some(path) = self.find_attack_path(&vuln.id, target_host) {
                paths.push(path);
            }
        }

        paths
    }

    fn ensure_sorted(&mut self) {
        if self.sorted {
            return;
        }
        self.sorted_ids = self.nodes.keys().cloned().collect();
        self.sorted_ids.sort();
        self.sorted = true;
    }

    pub fn serialize(&mut self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.serialize_into(&mut buf);
        buf
    }

    pub fn serialize_into(&mut self, out: &mut Vec<u8>) {
        self.ensure_sorted();
        let mut directory = Vec::with_capacity(self.nodes.len());
        let mut payload = Vec::new();

        for id in &self.sorted_ids {
            if let Some(node) = self.nodes.get(id) {
                let id_hash = hash_id(id);
                let start_offset = payload.len() as u64;
                let bytes = node.to_bytes();
                write_varu32(&mut payload, bytes.len() as u32);
                payload.extend_from_slice(&bytes);
                let block_len = payload.len() as u64 - start_offset;
                directory.push(GraphNodeDirEntry {
                    id_hash,
                    payload_offset: start_offset,
                    payload_len: block_len,
                });
            }
        }

        let directory_len = (directory.len() * GraphNodeDirEntry::SIZE) as u64;
        let payload_len = payload.len() as u64;
        let header = GraphSegmentHeader {
            node_count: self.nodes.len() as u32,
            directory_len,
            payload_len,
        };

        out.clear();
        out.reserve(
            GraphSegmentHeader::SIZE + directory.len() * GraphNodeDirEntry::SIZE + payload.len(),
        );
        header.write(out);
        GraphNodeDirEntry::write_all(&directory, out);
        out.extend_from_slice(&payload);
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < GraphSegmentHeader::SIZE {
            return Err(DecodeError("graph segment too small"));
        }
        let header = GraphSegmentHeader::read(bytes)?;

        let mut offset = GraphSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("graph directory overflow"))?;
        if dir_end > bytes.len() {
            return Err(DecodeError("graph directory out of bounds"));
        }
        let directory_bytes = &bytes[offset..dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("graph payload overflow"))?;
        if payload_end > bytes.len() {
            return Err(DecodeError("graph payload out of bounds"));
        }
        let payload_bytes = &bytes[offset..payload_end];

        let directory = GraphNodeDirEntry::read_all(directory_bytes, header.node_count as usize)?;

        let mut nodes = HashMap::with_capacity(header.node_count as usize);
        let mut sorted_ids = Vec::with_capacity(header.node_count as usize);

        for entry in directory {
            let mut cursor = entry.payload_offset as usize;
            let end = cursor + entry.payload_len as usize;
            if end > payload_bytes.len() {
                return Err(DecodeError("graph payload slice out of bounds"));
            }
            let len = read_varu32(payload_bytes, &mut cursor)? as usize;
            if cursor + len > end {
                return Err(DecodeError("graph node length mismatch"));
            }
            let node = GraphNode::from_bytes(&payload_bytes[cursor..cursor + len])?;
            cursor += len;
            if cursor != end {
                return Err(DecodeError("graph payload length mismatch"));
            }
            sorted_ids.push(node.id.clone());
            nodes.insert(node.id.clone(), node);
        }

        Ok(Self {
            nodes,
            sorted_ids,
            sorted: true,
        })
    }
}

// ==================== Graph Segment View (Zero-Copy) ====================

/// Immutable view for zero-copy reads
pub struct GraphSegmentView {
    directory: Vec<GraphNodeDirEntry>,
    node_ids: Vec<String>,
    data: Arc<Vec<u8>>,
    payload_offset: usize,
    payload_len: usize,
}

impl GraphSegmentView {
    pub fn from_arc(
        data: Arc<Vec<u8>>,
        segment_offset: usize,
        segment_len: usize,
    ) -> Result<Self, DecodeError> {
        if segment_offset + segment_len > data.len() {
            return Err(DecodeError("graph segment out of bounds"));
        }
        let bytes = &data[segment_offset..segment_offset + segment_len];
        if bytes.len() < GraphSegmentHeader::SIZE {
            return Err(DecodeError("graph segment too small"));
        }
        let header = GraphSegmentHeader::read(bytes)?;

        let mut offset = GraphSegmentHeader::SIZE;
        let dir_end = offset
            .checked_add(header.directory_len as usize)
            .ok_or(DecodeError("graph directory overflow"))?;
        if segment_offset + dir_end > data.len() {
            return Err(DecodeError("graph directory out of bounds"));
        }
        let directory_bytes = &data[segment_offset + offset..segment_offset + dir_end];
        offset = dir_end;

        let payload_end = offset
            .checked_add(header.payload_len as usize)
            .ok_or(DecodeError("graph payload overflow"))?;
        if segment_offset + payload_end > data.len() {
            return Err(DecodeError("graph payload out of bounds"));
        }
        let payload_offset = segment_offset + offset;

        let directory = GraphNodeDirEntry::read_all(directory_bytes, header.node_count as usize)?;

        // Pre-cache node IDs
        let payload = &data[payload_offset..payload_offset + header.payload_len as usize];
        let mut node_ids = Vec::with_capacity(header.node_count as usize);
        for entry in &directory {
            let mut cursor = entry.payload_offset as usize;
            let end = cursor + entry.payload_len as usize;
            if end > payload.len() {
                return Err(DecodeError("graph payload slice out of bounds"));
            }
            let len = read_varu32(payload, &mut cursor)? as usize;
            if cursor + len > end {
                return Err(DecodeError("graph node length mismatch"));
            }
            // Just read the ID (first field)
            let id = read_string(&payload[cursor..cursor + len], &mut 0)?.to_string();
            node_ids.push(id);
        }

        Ok(Self {
            directory,
            node_ids,
            data,
            payload_offset,
            payload_len: header.payload_len as usize,
        })
    }

    pub fn get_node(&self, id: &str) -> Result<Option<GraphNode>, DecodeError> {
        let target_hash = hash_id(id);
        for (idx, entry) in self.directory.iter().enumerate() {
            if entry.id_hash == target_hash && self.node_ids[idx] == id {
                let payload =
                    &self.data[self.payload_offset..self.payload_offset + self.payload_len];
                let mut cursor = entry.payload_offset as usize;
                let end = cursor + entry.payload_len as usize;
                if end > payload.len() {
                    return Err(DecodeError("graph payload slice out of bounds"));
                }
                let len = read_varu32(payload, &mut cursor)? as usize;
                if cursor + len > end {
                    return Err(DecodeError("graph node length mismatch"));
                }
                let node = GraphNode::from_bytes(&payload[cursor..cursor + len])?;
                return Ok(Some(node));
            }
        }
        Ok(None)
    }

    pub fn all_nodes(&self) -> Result<Vec<GraphNode>, DecodeError> {
        let payload = &self.data[self.payload_offset..self.payload_offset + self.payload_len];
        let mut nodes = Vec::with_capacity(self.directory.len());
        for entry in &self.directory {
            let mut cursor = entry.payload_offset as usize;
            let end = cursor + entry.payload_len as usize;
            if end > payload.len() {
                return Err(DecodeError("graph payload slice out of bounds"));
            }
            let len = read_varu32(payload, &mut cursor)? as usize;
            if cursor + len > end {
                return Err(DecodeError("graph node length mismatch"));
            }
            let node = GraphNode::from_bytes(&payload[cursor..cursor + len])?;
            nodes.push(node);
        }
        Ok(nodes)
    }

    pub fn node_count(&self) -> usize {
        self.directory.len()
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_graph_node_roundtrip() {
        let mut node = GraphNode::host("192.168.1.1");
        node.out_edges.push(EdgeRef {
            target_id: "service:192.168.1.1:22:ssh".to_string(),
            edge_type: EdgeType::HasService,
            weight: 2.0,
        });
        node.in_edges.push(EdgeRef {
            target_id: "cred:ssh_admin".to_string(),
            edge_type: EdgeType::AuthAccess,
            weight: 1.0,
        });

        let bytes = node.to_bytes();
        let decoded = GraphNode::from_bytes(&bytes).expect("decode");

        assert_eq!(decoded.id, "host:192.168.1.1");
        assert_eq!(decoded.node_type, NodeType::Host);
        assert_eq!(decoded.out_edges.len(), 1);
        assert_eq!(decoded.in_edges.len(), 1);
        assert_eq!(decoded.out_edges[0].edge_type, EdgeType::HasService);
    }

    #[test]
    fn test_graph_edge_roundtrip() {
        let edge = GraphEdge::new(
            "host:192.168.1.1",
            "vuln:CVE-2021-44228",
            EdgeType::AffectedBy,
        )
        .with_weight(2.5);

        let bytes = edge.to_bytes();
        let decoded = GraphEdge::from_bytes(&bytes).expect("decode");

        assert_eq!(decoded.source, "host:192.168.1.1");
        assert_eq!(decoded.target, "vuln:CVE-2021-44228");
        assert_eq!(decoded.edge_type, EdgeType::AffectedBy);
        assert!((decoded.weight - 2.5).abs() < 0.001);
    }

    #[test]
    fn test_graph_segment_roundtrip() {
        let mut segment = GraphSegment::new();

        segment.add_node(GraphNode::host("192.168.1.1"));
        segment.add_node(GraphNode::host("192.168.1.2"));
        segment.add_node(GraphNode::credential("ssh_admin", "admin"));

        segment.add_edge(GraphEdge::new(
            "cred:ssh_admin",
            "host:192.168.1.1",
            EdgeType::AuthAccess,
        ));
        segment.add_edge(GraphEdge::new(
            "host:192.168.1.1",
            "host:192.168.1.2",
            EdgeType::ConnectsTo,
        ));

        let bytes = segment.serialize();
        let decoded = GraphSegment::deserialize(&bytes).expect("decode");

        assert_eq!(decoded.node_count(), 3);
        let host1 = decoded.get_node("host:192.168.1.1").expect("host1");
        assert_eq!(host1.out_edges.len(), 1);
        assert_eq!(host1.in_edges.len(), 1);
    }

    #[test]
    fn test_graph_segment_view() {
        let mut segment = GraphSegment::new();
        segment.add_node(GraphNode::host("10.0.0.1"));

        let bytes = segment.serialize();
        let data = Arc::new(bytes);
        let view = GraphSegmentView::from_arc(Arc::clone(&data), 0, data.len()).expect("view");

        let node = view
            .get_node("host:10.0.0.1")
            .expect("result")
            .expect("node");
        assert_eq!(node.node_type, NodeType::Host);
    }

    #[test]
    fn test_nodes_of_type() {
        let mut segment = GraphSegment::new();
        segment.add_node(GraphNode::host("192.168.1.1"));
        segment.add_node(GraphNode::host("192.168.1.2"));
        segment.add_node(GraphNode::credential("admin", "admin"));
        segment.add_node(GraphNode::vulnerability("CVE-2021-44228"));

        let hosts = segment.nodes_of_type(NodeType::Host);
        assert_eq!(hosts.len(), 2);

        let creds = segment.nodes_of_type(NodeType::Credential);
        assert_eq!(creds.len(), 1);
    }

    #[test]
    fn test_edge_default_weights() {
        assert_eq!(EdgeType::AuthAccess.default_weight(), 1.0);
        assert_eq!(EdgeType::HasService.default_weight(), 2.0);
        assert_eq!(EdgeType::RelatedTo.default_weight(), 5.0);
    }

    #[test]
    fn test_remove_node() {
        let mut segment = GraphSegment::new();
        segment.add_node(GraphNode::host("192.168.1.1"));
        segment.add_node(GraphNode::host("192.168.1.2"));
        segment.add_edge(GraphEdge::new(
            "host:192.168.1.1",
            "host:192.168.1.2",
            EdgeType::ConnectsTo,
        ));

        assert_eq!(segment.node_count(), 2);
        segment.remove_node("host:192.168.1.1");
        assert_eq!(segment.node_count(), 1);

        // Verify edge was cleaned up
        let host2 = segment.get_node("host:192.168.1.2").expect("host2");
        assert!(host2.in_edges.is_empty());
    }
}
