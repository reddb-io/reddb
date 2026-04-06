//! Security Intelligence Queries
//!
//! Pre-built cross-modal query templates for penetration testing and
//! security analysis. These queries combine Table, Graph, and Vector
//! storage for maximum intelligence extraction.
//!
//! # Query Categories
//!
//! - **Attack Paths**: Find routes between hosts via exploitable relationships
//! - **Vulnerability Analysis**: CVE impact assessment and similarity search
//! - **Lateral Movement**: Credential-based access paths
//! - **Privilege Escalation**: Paths to elevated access
//! - **Asset Impact**: Blast radius and dependency analysis
//!
//! # Example
//!
//! ```ignore
//! use storage::query::security::{SecurityQueries, AttackPathQuery};
//!
//! let queries = SecurityQueries::new(&graph, &vector_store, &unified_index);
//!
//! // Find attack paths from external to database
//! let paths = queries.attack_paths(AttackPathQuery {
//!     from: "external_host",
//!     to: "db_server",
//!     max_hops: 5,
//!     via_edge_types: vec!["EXPLOITS", "CONNECTS_TO", "AUTH_ACCESS"],
//! })?;
//!
//! // Find similar CVEs to prioritize patching
//! let similar = queries.similar_cves("CVE-2021-44228", 10)?;
//!
//! // Assess lateral movement risk
//! let lateral = queries.lateral_movement_paths("compromised_host", 3)?;
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::storage::engine::algorithms::{BetweennessCentrality, PageRank};
use crate::storage::engine::graph_store::{GraphEdgeType, GraphNodeType, GraphStore};
use crate::storage::engine::graph_table_index::GraphTableIndex;
use crate::storage::engine::unified_index::UnifiedIndex;
use crate::storage::engine::vector_store::VectorStore;
use crate::storage::query::unified::ExecutionError;

// ============================================================================
// Query Types
// ============================================================================

/// Attack path query parameters
#[derive(Debug, Clone)]
pub struct AttackPathQuery {
    /// Source node (attacker position)
    pub from: String,
    /// Target node (objective)
    pub to: String,
    /// Maximum hops allowed
    pub max_hops: usize,
    /// Edge types to traverse (empty = all)
    pub via_edge_types: Vec<GraphEdgeType>,
    /// Minimum required privilege level (filter low-value paths)
    pub min_severity: Option<f32>,
    /// Include only paths with these node types
    pub through_node_types: Vec<GraphNodeType>,
}

impl AttackPathQuery {
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            max_hops: 10,
            via_edge_types: Vec::new(),
            min_severity: None,
            through_node_types: Vec::new(),
        }
    }

    pub fn max_hops(mut self, hops: usize) -> Self {
        self.max_hops = hops;
        self
    }

    pub fn via(mut self, edge_types: Vec<GraphEdgeType>) -> Self {
        self.via_edge_types = edge_types;
        self
    }

    pub fn min_severity(mut self, severity: f32) -> Self {
        self.min_severity = Some(severity);
        self
    }
}

/// Lateral movement query parameters
#[derive(Debug, Clone)]
pub struct LateralMovementQuery {
    /// Starting compromised host
    pub from: String,
    /// Maximum depth to explore
    pub max_depth: usize,
    /// Credential types to consider
    pub credential_types: Vec<String>,
    /// Include only reachable via admin access
    pub admin_only: bool,
}

impl LateralMovementQuery {
    pub fn new(from: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            max_depth: 5,
            credential_types: Vec::new(),
            admin_only: false,
        }
    }

    pub fn max_depth(mut self, depth: usize) -> Self {
        self.max_depth = depth;
        self
    }

    pub fn admin_only(mut self) -> Self {
        self.admin_only = true;
        self
    }
}

/// Privilege escalation query parameters
#[derive(Debug, Clone)]
pub struct PrivEscQuery {
    /// Starting node (current access)
    pub from: String,
    /// Target privilege level (e.g., "root", "SYSTEM", "Domain Admin")
    pub target_privilege: Option<String>,
    /// Maximum path length
    pub max_hops: usize,
}

impl PrivEscQuery {
    pub fn new(from: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            target_privilege: None,
            max_hops: 5,
        }
    }

    pub fn target(mut self, privilege: impl Into<String>) -> Self {
        self.target_privilege = Some(privilege.into());
        self
    }
}

/// Blast radius query parameters
#[derive(Debug, Clone)]
pub struct BlastRadiusQuery {
    /// Compromised asset
    pub compromised: String,
    /// Analysis depth
    pub depth: usize,
    /// Include indirect impacts
    pub transitive: bool,
}

impl BlastRadiusQuery {
    pub fn new(compromised: impl Into<String>) -> Self {
        Self {
            compromised: compromised.into(),
            depth: 3,
            transitive: true,
        }
    }
}

// ============================================================================
// Query Results
// ============================================================================

/// Attack path result
#[derive(Debug, Clone)]
pub struct AttackPath {
    /// Nodes in the path
    pub nodes: Vec<String>,
    /// Edge types traversed
    pub edges: Vec<GraphEdgeType>,
    /// Total path weight (lower = easier attack)
    pub difficulty: f32,
    /// Techniques required at each hop
    pub techniques: Vec<String>,
    /// Risk score (higher = more dangerous)
    pub risk_score: f32,
}

impl AttackPath {
    /// Number of hops in the path
    pub fn hop_count(&self) -> usize {
        self.edges.len()
    }

    /// Check if path uses a specific technique
    pub fn uses_technique(&self, technique: &str) -> bool {
        self.techniques.iter().any(|t| t.contains(technique))
    }
}

/// Lateral movement result
#[derive(Debug, Clone)]
pub struct LateralMovementResult {
    /// Reachable hosts from starting position
    pub reachable_hosts: Vec<ReachableHost>,
    /// Total unique hosts reachable
    pub total_reachable: usize,
    /// Credential chains used
    pub credential_chains: Vec<CredentialChain>,
}

/// A host reachable via lateral movement
#[derive(Debug, Clone)]
pub struct ReachableHost {
    pub host_id: String,
    pub hostname: Option<String>,
    pub hops: usize,
    pub access_level: String,
    pub path: Vec<String>,
}

/// A chain of credentials enabling lateral movement
#[derive(Debug, Clone)]
pub struct CredentialChain {
    pub credentials: Vec<String>,
    pub enables_access_to: Vec<String>,
}

/// Privilege escalation result
#[derive(Debug, Clone)]
pub struct PrivEscPath {
    /// Path from current to elevated privilege
    pub path: Vec<String>,
    /// Vulnerabilities or misconfigs exploited
    pub exploits: Vec<String>,
    /// Final privilege achieved
    pub achieved_privilege: String,
    /// Difficulty score (lower = easier)
    pub difficulty: f32,
}

/// Blast radius result
#[derive(Debug, Clone)]
pub struct BlastRadiusResult {
    /// Directly impacted assets
    pub direct_impact: Vec<String>,
    /// Indirectly impacted (transitive)
    pub indirect_impact: Vec<String>,
    /// Critical assets affected
    pub critical_assets: Vec<String>,
    /// Impact score by category
    pub impact_by_category: HashMap<String, usize>,
    /// Total impact score
    pub total_impact_score: f32,
}

/// Similar CVE result
#[derive(Debug, Clone)]
pub struct SimilarCVE {
    pub cve_id: String,
    pub similarity_score: f32,
    pub shared_cwe: Vec<String>,
    pub affected_products: Vec<String>,
}

// ============================================================================
// Security Query Engine
// ============================================================================

/// Security intelligence query engine
pub struct SecurityQueries {
    graph: Arc<GraphStore>,
    index: Arc<GraphTableIndex>,
    vector_store: Option<Arc<VectorStore>>,
    unified_index: Option<Arc<UnifiedIndex>>,
}

impl SecurityQueries {
    /// Create a new security query engine
    pub fn new(graph: Arc<GraphStore>, index: Arc<GraphTableIndex>) -> Self {
        Self {
            graph,
            index,
            vector_store: None,
            unified_index: None,
        }
    }

    /// Add vector store for similarity queries
    pub fn with_vector_store(mut self, store: Arc<VectorStore>) -> Self {
        self.vector_store = Some(store);
        self
    }

    /// Add unified index for cross-modal queries
    pub fn with_unified_index(mut self, index: Arc<UnifiedIndex>) -> Self {
        self.unified_index = Some(index);
        self
    }

    // ========================================================================
    // Attack Path Queries
    // ========================================================================

    /// Find attack paths between two nodes
    pub fn attack_paths(&self, query: AttackPathQuery) -> Result<Vec<AttackPath>, ExecutionError> {
        // Get all paths up to max_hops
        let paths = self.find_paths_bfs(
            &query.from,
            &query.to,
            query.max_hops,
            &query.via_edge_types,
        )?;

        // Convert to attack paths with scoring
        let attack_paths: Vec<AttackPath> = paths
            .into_iter()
            .filter_map(|path| {
                let difficulty = self.calculate_path_difficulty(&path);

                // Filter by minimum severity if specified
                if let Some(min_sev) = query.min_severity {
                    if difficulty > min_sev {
                        return None;
                    }
                }

                Some(AttackPath {
                    nodes: path.nodes.clone(),
                    edges: path.edges.clone(),
                    difficulty,
                    techniques: self.extract_techniques(&path),
                    risk_score: self.calculate_risk_score(&path),
                })
            })
            .collect();

        Ok(attack_paths)
    }

    /// Find shortest attack path (minimum difficulty)
    pub fn shortest_attack_path(
        &self,
        from: &str,
        to: &str,
    ) -> Result<Option<AttackPath>, ExecutionError> {
        let query = AttackPathQuery::new(from, to);
        let mut paths = self.attack_paths(query)?;

        // Sort by difficulty (ascending)
        paths.sort_by(|a, b| a.difficulty.partial_cmp(&b.difficulty).unwrap());

        Ok(paths.into_iter().next())
    }

    /// Find all attack paths to critical assets
    /// Note: Requires a list of critical node IDs to be provided
    pub fn attack_paths_to_critical(
        &self,
        from: &str,
        critical_node_ids: &[String],
    ) -> Result<Vec<AttackPath>, ExecutionError> {
        let mut all_paths = Vec::new();

        for target in critical_node_ids {
            if target != from {
                let query = AttackPathQuery::new(from, target).max_hops(5);
                if let Ok(paths) = self.attack_paths(query) {
                    all_paths.extend(paths);
                }
            }
        }

        // Sort by risk score (descending)
        all_paths.sort_by(|a, b| b.risk_score.partial_cmp(&a.risk_score).unwrap());

        Ok(all_paths)
    }

    // ========================================================================
    // Lateral Movement Queries
    // ========================================================================

    /// Find lateral movement possibilities from a compromised host
    pub fn lateral_movement(
        &self,
        query: LateralMovementQuery,
    ) -> Result<LateralMovementResult, ExecutionError> {
        let mut reachable = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = vec![(query.from.clone(), 0, vec![query.from.clone()])];

        while let Some((current, depth, path)) = queue.pop() {
            if depth > query.max_depth {
                continue;
            }

            if visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());

            // Get outgoing edges representing access
            let edges = self.graph.outgoing_edges(&current);

            for (edge_type, target, weight) in edges {
                // Check if edge represents credential-based access
                let is_credential_access = matches!(
                    edge_type,
                    GraphEdgeType::AuthAccess | GraphEdgeType::HasUser | GraphEdgeType::ConnectsTo
                );

                if !is_credential_access {
                    continue;
                }

                // Check admin_only filter - AuthAccess implies elevated access
                if query.admin_only && !matches!(edge_type, GraphEdgeType::AuthAccess) {
                    continue;
                }

                let mut new_path = path.clone();
                new_path.push(target.clone());

                reachable.push(ReachableHost {
                    host_id: target.clone(),
                    hostname: None, // Would lookup from table
                    hops: depth + 1,
                    access_level: format!("{:?}", edge_type),
                    path: new_path.clone(),
                });

                queue.push((target, depth + 1, new_path));
            }
        }

        Ok(LateralMovementResult {
            total_reachable: reachable.len(),
            reachable_hosts: reachable,
            credential_chains: Vec::new(), // Would extract from paths
        })
    }

    // ========================================================================
    // Privilege Escalation Queries
    // ========================================================================

    /// Find privilege escalation paths
    pub fn privilege_escalation(
        &self,
        query: PrivEscQuery,
    ) -> Result<Vec<PrivEscPath>, ExecutionError> {
        let mut results = Vec::new();

        // Get nodes representing elevated privileges
        let priv_edge_types = vec![GraphEdgeType::AuthAccess, GraphEdgeType::AffectedBy];

        let paths = self.find_paths_bfs(
            &query.from,
            &query.target_privilege.clone().unwrap_or_default(),
            query.max_hops,
            &priv_edge_types,
        )?;

        for path in paths {
            results.push(PrivEscPath {
                path: path.nodes.clone(),
                exploits: self.extract_techniques(&path),
                achieved_privilege: path.nodes.last().cloned().unwrap_or_default(),
                difficulty: self.calculate_path_difficulty(&path),
            });
        }

        Ok(results)
    }

    // ========================================================================
    // Blast Radius / Impact Analysis
    // ========================================================================

    /// Calculate blast radius from a compromised asset
    pub fn blast_radius(
        &self,
        query: BlastRadiusQuery,
    ) -> Result<BlastRadiusResult, ExecutionError> {
        let mut direct = Vec::new();
        let mut indirect = Vec::new();
        let mut impact_by_category: HashMap<String, usize> = HashMap::new();

        // BFS to find all impacted nodes
        let mut visited = HashSet::new();
        let mut queue = vec![(query.compromised.clone(), 0)];

        while let Some((current, depth)) = queue.pop() {
            if depth > query.depth {
                continue;
            }

            if visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());

            // Categorize impact
            if let Some(node) = self.graph.get_node(&current) {
                let category = format!("{:?}", node.node_type);
                *impact_by_category.entry(category).or_insert(0) += 1;

                if depth == 1 {
                    direct.push(current.clone());
                } else if depth > 1 && query.transitive {
                    indirect.push(current.clone());
                }
            }

            // Get dependencies
            let edges = self.graph.outgoing_edges(&current);
            for (_, target, _) in edges {
                queue.push((target, depth + 1));
            }
        }

        // Calculate total impact score
        let total_score = direct.len() as f32 * 1.0 + indirect.len() as f32 * 0.5;

        Ok(BlastRadiusResult {
            direct_impact: direct,
            indirect_impact: indirect,
            critical_assets: Vec::new(), // Would filter by criticality
            impact_by_category,
            total_impact_score: total_score,
        })
    }

    // ========================================================================
    // CVE Similarity Queries (requires vector store)
    // ========================================================================

    /// Find CVEs similar to a given CVE using vector embeddings
    /// Returns empty if vector store not configured
    pub fn similar_cves(
        &self,
        cve_id: &str,
        _cve_embedding: &[f32],
        k: usize,
    ) -> Result<Vec<SimilarCVE>, ExecutionError> {
        // This requires the caller to provide the CVE embedding
        // In practice, you'd lookup the embedding from unified index

        // For now, return placeholder showing the API shape
        // Real implementation would use:
        // 1. unified_index.get_vector_ref(cve_id) to get vector reference
        // 2. vector_store.search() with the embedding
        // 3. Map results back through unified_index

        Ok(Vec::new())
    }

    /// Find hosts affected by a CVE via graph traversal
    /// Pattern: (cve)-[:AffectedBy|HasService]->(host)
    pub fn hosts_affected_by_cve(&self, cve_id: &str) -> Result<Vec<String>, ExecutionError> {
        let mut affected_hosts = Vec::new();

        // Get outgoing edges from CVE node
        let cve_edges = self.graph.outgoing_edges(cve_id);

        for (edge_type, target_id, _) in cve_edges {
            // AffectedBy edge directly connects to affected entity
            if matches!(edge_type, GraphEdgeType::AffectedBy) {
                affected_hosts.push(target_id.clone());
            }
            // Or follow HasService to find the host
            else if matches!(edge_type, GraphEdgeType::HasService) {
                // The target is a service, find its host
                let service_edges = self.graph.incoming_edges(&target_id);
                for (_, host_id, _) in service_edges {
                    affected_hosts.push(host_id);
                }
            }
        }

        affected_hosts.sort();
        affected_hosts.dedup();

        Ok(affected_hosts)
    }

    // ========================================================================
    // Critical Asset Analysis
    // ========================================================================

    /// Find most critical nodes using PageRank
    pub fn critical_assets(&self, top_k: usize) -> Result<Vec<(String, f64)>, ExecutionError> {
        let pagerank = PageRank::default();
        let result = pagerank.run(&*self.graph);

        // Use PageRankResult's top() method for efficiency
        Ok(result.top(top_k))
    }

    /// Find choke points using betweenness centrality
    pub fn choke_points(&self, top_k: usize) -> Result<Vec<(String, f64)>, ExecutionError> {
        // BetweennessCentrality uses static compute() method
        let result = BetweennessCentrality::compute(&*self.graph, true);

        let mut scores: Vec<_> = result.scores.into_iter().collect();
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(scores.into_iter().take(top_k).collect())
    }

    // ========================================================================
    // Helper Methods
    // ========================================================================

    fn find_paths_bfs(
        &self,
        from: &str,
        to: &str,
        max_hops: usize,
        edge_types: &[GraphEdgeType],
    ) -> Result<Vec<PathResult>, ExecutionError> {
        let mut results = Vec::new();
        let mut queue = vec![(from.to_string(), vec![from.to_string()], Vec::new(), 0.0)];
        let mut visited_at_depth: HashMap<String, usize> = HashMap::new();

        while let Some((current, nodes, edges, weight)) = queue.pop() {
            if nodes.len() > max_hops + 1 {
                continue;
            }

            // Check if we've visited this node at a shorter path
            if let Some(&prev_depth) = visited_at_depth.get(&current) {
                if prev_depth <= nodes.len() {
                    continue;
                }
            }
            visited_at_depth.insert(current.clone(), nodes.len());

            if current == to {
                results.push(PathResult {
                    nodes: nodes.clone(),
                    edges: edges.clone(),
                    total_weight: weight,
                });
                continue;
            }

            // Get outgoing edges
            let out_edges = self.graph.outgoing_edges(&current);

            for (edge_type, target, edge_weight) in out_edges {
                // Filter by edge types if specified
                if !edge_types.is_empty() && !edge_types.contains(&edge_type) {
                    continue;
                }

                if nodes.contains(&target) {
                    continue; // Avoid cycles
                }

                let mut new_nodes = nodes.clone();
                new_nodes.push(target.clone());

                let mut new_edges = edges.clone();
                new_edges.push(edge_type);

                queue.push((target, new_nodes, new_edges, weight + edge_weight));
            }
        }

        Ok(results)
    }

    fn calculate_path_difficulty(&self, path: &PathResult) -> f32 {
        // Lower weight = easier attack
        // Base on edge weights + number of hops
        let hop_penalty = path.nodes.len() as f32 * 0.1;
        path.total_weight + hop_penalty
    }

    fn calculate_risk_score(&self, path: &PathResult) -> f32 {
        // Higher = more dangerous
        // Inverse of difficulty, plus bonuses for certain edge types
        let base = 10.0 / (path.total_weight + 1.0);

        let bonus: f32 = path
            .edges
            .iter()
            .map(|e| match e {
                GraphEdgeType::AffectedBy => 2.0, // Exploitable vulnerability
                GraphEdgeType::AuthAccess => 1.8, // Credential-based access
                GraphEdgeType::HasUser => 1.5,    // User account access
                GraphEdgeType::ConnectsTo => 0.5, // Network path
                _ => 0.0,
            })
            .sum();

        base + bonus
    }

    fn extract_techniques(&self, path: &PathResult) -> Vec<String> {
        path.edges.iter().map(|e| format!("{:?}", e)).collect()
    }
}

/// Internal path result for processing
#[derive(Debug, Clone)]
struct PathResult {
    nodes: Vec<String>,
    edges: Vec<GraphEdgeType>,
    total_weight: f32,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_attack_path_query_builder() {
        let query = AttackPathQuery::new("attacker", "target")
            .max_hops(3)
            .min_severity(0.5);

        assert_eq!(query.from, "attacker");
        assert_eq!(query.to, "target");
        assert_eq!(query.max_hops, 3);
        assert_eq!(query.min_severity, Some(0.5));
    }

    #[test]
    fn test_lateral_movement_query_builder() {
        let query = LateralMovementQuery::new("compromised_host")
            .max_depth(4)
            .admin_only();

        assert_eq!(query.from, "compromised_host");
        assert_eq!(query.max_depth, 4);
        assert!(query.admin_only);
    }

    #[test]
    fn test_attack_path_hop_count() {
        let path = AttackPath {
            nodes: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            edges: vec![GraphEdgeType::ConnectsTo, GraphEdgeType::AffectedBy],
            difficulty: 1.5,
            techniques: vec!["T1021".to_string()],
            risk_score: 8.5,
        };

        assert_eq!(path.hop_count(), 2);
        assert!(path.uses_technique("T1021"));
        assert!(!path.uses_technique("T1059"));
    }
}
