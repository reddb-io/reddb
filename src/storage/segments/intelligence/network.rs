//! Network-Centric Intelligence
//!
//! Answers: "What's the topology? Segments? Routes?"

use std::collections::{HashMap, HashSet};

use crate::storage::segments::graph::{EdgeType, GraphSegment, NodeType};

/// Network topology overview
#[derive(Debug, Clone)]
pub struct NetworkTopology {
    pub segments: Vec<NetworkSegment>,
    pub cross_segment_paths: usize,
    pub critical_chokepoints: Vec<String>,
}

impl NetworkTopology {
    pub fn display(&self) -> String {
        let mut s = String::new();
        s.push_str("┌─────────────────────────────────────────────────────────────────┐\n");
        s.push_str("│  NETWORK TOPOLOGY                                               │\n");
        s.push_str("├─────────────────────────────────────────────────────────────────┤\n");
        s.push_str(&format!(
            "│  SEGMENTS DISCOVERED: {:<40} │\n",
            self.segments.len()
        ));

        for segment in &self.segments {
            s.push_str("│                                                                 │\n");
            let seg_str = format!("─ {} ({} hosts)", segment.cidr, segment.host_count);
            s.push_str(&format!("│  ┌{:<60}┐ │\n", seg_str));

            let internet = if segment.internet_facing { "Yes" } else { "No" };
            s.push_str(&format!("│  │  Internet-facing: {:<40}│ │\n", internet));

            if !segment.services.is_empty() {
                let svcs = segment.services.join(", ");
                s.push_str(&format!(
                    "│  │  Services: {:<48}│ │\n",
                    if svcs.len() > 48 {
                        format!("{}...", &svcs[..45])
                    } else {
                        svcs
                    }
                ));
            }
            s.push_str("│  └─────────────────────────────────────────────────────────────┘ │\n");
        }

        s.push_str("│                                                                 │\n");
        s.push_str(&format!(
            "│  CROSS-SEGMENT PATHS: {:<40} │\n",
            self.cross_segment_paths
        ));
        s.push_str(&format!(
            "│  CRITICAL CHOKEPOINTS: {} hosts                               │\n",
            self.critical_chokepoints.len()
        ));

        s.push_str("└─────────────────────────────────────────────────────────────────┘\n");
        s
    }
}

/// A network segment
#[derive(Debug, Clone)]
pub struct NetworkSegment {
    pub cidr: String,
    pub name: Option<String>,
    pub host_count: usize,
    pub gateway_count: usize,
    pub internet_facing: bool,
    pub services: Vec<String>,
    pub connections_to: Vec<String>,
}

/// Network-centric intelligence queries
pub struct NetworkIntelligence<'a> {
    graph: &'a GraphSegment,
}

impl<'a> NetworkIntelligence<'a> {
    pub fn new(graph: &'a GraphSegment) -> Self {
        Self { graph }
    }

    /// Get complete network topology
    pub fn topology(&self) -> NetworkTopology {
        let segments = self.segments();
        let chokepoints = self.chokepoints();

        // Count cross-segment paths
        let cross_paths = self.count_cross_segment_connections(&segments);

        NetworkTopology {
            segments,
            cross_segment_paths: cross_paths,
            critical_chokepoints: chokepoints.into_iter().map(|(h, _)| h).collect(),
        }
    }

    /// List all network segments
    pub fn segments(&self) -> Vec<NetworkSegment> {
        let mut segment_hosts: HashMap<String, Vec<String>> = HashMap::new();

        for host in self.graph.nodes_of_type(NodeType::Host) {
            let ip = host.id.trim_start_matches("host:");

            // Parse IP and determine /24 segment
            if let Some(segment) = self.ip_to_segment(ip) {
                segment_hosts
                    .entry(segment)
                    .or_default()
                    .push(ip.to_string());
            }
        }

        let mut segments: Vec<NetworkSegment> = segment_hosts
            .into_iter()
            .map(|(cidr, hosts)| {
                let services = self.collect_segment_services(&hosts);

                NetworkSegment {
                    cidr,
                    name: None,
                    host_count: hosts.len(),
                    gateway_count: 0,
                    internet_facing: self.is_internet_facing(&hosts),
                    services,
                    connections_to: vec![],
                }
            })
            .collect();

        segments.sort_by(|a, b| b.host_count.cmp(&a.host_count));
        segments
    }

    /// Convert IP to /24 segment
    fn ip_to_segment(&self, ip: &str) -> Option<String> {
        let parts: Vec<&str> = ip.split('.').collect();
        if parts.len() == 4 {
            Some(format!("{}.{}.{}.0/24", parts[0], parts[1], parts[2]))
        } else {
            None
        }
    }

    /// Collect services in a segment
    fn collect_segment_services(&self, hosts: &[String]) -> Vec<String> {
        let mut services: HashSet<String> = HashSet::new();

        for host in hosts {
            if let Some(node) = self.graph.get_node(&format!("host:{}", host)) {
                for edge in &node.out_edges {
                    if edge.edge_type == EdgeType::HasService {
                        // Extract service name
                        let parts: Vec<&str> = edge.target_id.split(':').collect();
                        if parts.len() >= 4 {
                            services.insert(parts[3].to_uppercase());
                        }
                    }
                }
            }
        }

        let mut result: Vec<String> = services.into_iter().collect();
        result.sort();
        result
    }

    /// Check if any host in segment appears internet-facing
    fn is_internet_facing(&self, hosts: &[String]) -> bool {
        for host in hosts {
            if let Some(node) = self.graph.get_node(&format!("host:{}", host)) {
                // Check for typical internet-facing services
                for edge in &node.out_edges {
                    if edge.edge_type == EdgeType::HasService
                        && (edge.target_id.contains(":80:")
                            || edge.target_id.contains(":443:")
                            || edge.target_id.contains(":25:"))
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Count connections between segments
    fn count_cross_segment_connections(&self, segments: &[NetworkSegment]) -> usize {
        let mut count = 0;

        for host in self.graph.nodes_of_type(NodeType::Host) {
            let source_seg = self.ip_to_segment(host.id.trim_start_matches("host:"));

            for edge in &host.out_edges {
                if edge.edge_type == EdgeType::ConnectsTo {
                    let target_seg = self.ip_to_segment(edge.target_id.trim_start_matches("host:"));
                    if source_seg != target_seg {
                        count += 1;
                    }
                }
            }
        }

        count
    }

    /// Get details of specific segment
    pub fn segment(&self, cidr: &str) -> Option<NetworkSegment> {
        self.segments().into_iter().find(|s| s.cidr == cidr)
    }

    /// Find all gateway/router hosts
    pub fn gateways(&self) -> Vec<String> {
        let mut gateways = Vec::new();

        for host in self.graph.nodes_of_type(NodeType::Host) {
            // A gateway typically connects to multiple segments
            let ip = host.id.trim_start_matches("host:");
            let source_seg = self.ip_to_segment(ip);

            let mut connected_segs: HashSet<String> = HashSet::new();
            for edge in &host.out_edges {
                if edge.edge_type == EdgeType::ConnectsTo {
                    if let Some(seg) =
                        self.ip_to_segment(edge.target_id.trim_start_matches("host:"))
                    {
                        connected_segs.insert(seg);
                    }
                }
            }

            if connected_segs.len() >= 2
                || (source_seg.is_some()
                    && connected_segs
                        .iter()
                        .any(|s| Some(s.as_str()) != source_seg.as_deref()))
            {
                gateways.push(ip.to_string());
            }
        }

        gateways
    }

    /// Find critical path bottlenecks (betweenness centrality approximation)
    pub fn chokepoints(&self) -> Vec<(String, usize)> {
        let mut path_counts: HashMap<String, usize> = HashMap::new();

        // Simple approximation: count how many paths go through each node
        for host in self.graph.nodes_of_type(NodeType::Host) {
            let in_count = host
                .in_edges
                .iter()
                .filter(|e| {
                    e.edge_type == EdgeType::ConnectsTo || e.edge_type == EdgeType::AuthAccess
                })
                .count();
            let out_count = host
                .out_edges
                .iter()
                .filter(|e| {
                    e.edge_type == EdgeType::ConnectsTo || e.edge_type == EdgeType::AuthAccess
                })
                .count();

            if in_count > 0 && out_count > 0 {
                path_counts.insert(
                    host.id.trim_start_matches("host:").to_string(),
                    in_count * out_count,
                );
            }
        }

        let mut sorted: Vec<_> = path_counts.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        sorted.truncate(10);
        sorted
    }

    /// Find isolated segments with no routes
    pub fn isolated(&self) -> Vec<NetworkSegment> {
        self.segments()
            .into_iter()
            .filter(|s| s.connections_to.is_empty() && s.gateway_count == 0)
            .collect()
    }

    /// Find paths between segments
    pub fn paths(&self, from_cidr: &str, to_cidr: &str) -> Vec<Vec<String>> {
        // Would need proper pathfinding implementation
        vec![]
    }
}
