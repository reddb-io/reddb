use super::*;

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
        insights.sort_by_key(|b| std::cmp::Reverse(b.priority()));

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
                    let da = distances.get::<str>(a).unwrap_or(&f32::INFINITY);
                    let db = distances.get::<str>(b).unwrap_or(&f32::INFINITY);
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
