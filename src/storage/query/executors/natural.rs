//! Natural Language Query Executor
//!
//! Translates natural language queries to RQL and executes them,
//! providing explanations of the translation process.
//!
//! # Features
//!
//! - Intent classification: list, find, path, count, show
//! - Entity extraction: hosts, users, credentials, services, vulnerabilities
//! - Query generation with confidence scoring
//! - Execution explanation for user understanding

use std::sync::Arc;

use crate::storage::engine::graph_store::{GraphNodeType, GraphStore};
use crate::storage::query::modes::natural::{
    EntityType, ExtractedEntity, NaturalParser, NaturalQuery, QueryIntent,
};
use crate::storage::query::unified::{
    ExecutionError, MatchedNode, QueryStats, UnifiedRecord, UnifiedResult,
};

/// Natural language executor with translation explanation
pub struct NaturalExecutor {
    graph: Arc<GraphStore>,
}

impl NaturalExecutor {
    /// Create a new natural language executor
    pub fn new(graph: Arc<GraphStore>) -> Self {
        Self { graph }
    }

    /// Execute a natural language query and return explanation
    pub fn execute_with_explanation(
        &self,
        query: &str,
    ) -> Result<(UnifiedResult, String), ExecutionError> {
        // Parse natural language
        let parsed = NaturalParser::parse(query).map_err(|e| ExecutionError::new(e.to_string()))?;

        // Generate explanation
        let explanation = self.generate_explanation(&parsed, query);

        // Execute
        let result = self.execute_natural(&parsed)?;

        Ok((result, explanation))
    }

    /// Execute a natural language query
    pub fn execute(&self, query: &str) -> Result<UnifiedResult, ExecutionError> {
        let parsed = NaturalParser::parse(query).map_err(|e| ExecutionError::new(e.to_string()))?;
        self.execute_natural(&parsed)
    }

    /// Execute a parsed natural language query
    fn execute_natural(&self, query: &NaturalQuery) -> Result<UnifiedResult, ExecutionError> {
        let mut stats = QueryStats::default();
        let mut result = UnifiedResult::empty();

        match query.intent {
            QueryIntent::Find => {
                // Find handles both "find" and "list" semantics
                self.execute_find(query, &mut result, &mut stats)?;
            }
            QueryIntent::Path => {
                self.execute_path(query, &mut result, &mut stats)?;
            }
            QueryIntent::Count => {
                self.execute_count(query, &mut result, &mut stats)?;
            }
            QueryIntent::Show => {
                self.execute_show(query, &mut result, &mut stats)?;
            }
            QueryIntent::Check => {
                self.execute_check(query, &mut result, &mut stats)?;
            }
        }

        result.stats = stats;
        Ok(result)
    }

    /// Execute FIND intent (also handles LIST semantics)
    fn execute_find(
        &self,
        query: &NaturalQuery,
        result: &mut UnifiedResult,
        stats: &mut QueryStats,
    ) -> Result<(), ExecutionError> {
        let node_type = self.primary_entity_type(query);

        for node in self.graph.iter_nodes() {
            stats.nodes_scanned += 1;

            // Check type match
            let type_matches = match &node_type {
                Some(t) => node.node_type == *t,
                None => true,
            };

            if !type_matches {
                continue;
            }

            // Check entity filters
            if !self.node_matches_filters(&node, &query.entities) {
                continue;
            }

            // Check relationship constraints
            let mut rel_match = true;
            for entity in &query.entities {
                if let Some(ref value) = entity.value {
                    // Check if node has relationship to this entity
                    if !self.has_relationship_to(&node.id, value, stats) {
                        rel_match = false;
                        break;
                    }
                }
            }

            if rel_match {
                let mut record = UnifiedRecord::new();
                record.set_node("_", MatchedNode::from_stored(&node));
                result.push(record);
            }
        }

        // Apply limit if specified
        if let Some(limit) = query.limit {
            if result.len() > limit as usize {
                result.records.truncate(limit as usize);
            }
        }

        Ok(())
    }

    /// Execute PATH intent
    fn execute_path(
        &self,
        query: &NaturalQuery,
        result: &mut UnifiedResult,
        stats: &mut QueryStats,
    ) -> Result<(), ExecutionError> {
        // Extract source and target from entities
        let (source, target) = self.extract_path_endpoints(query)?;

        // BFS to find path
        use crate::storage::query::unified::{GraphPath, MatchedEdge};
        use std::collections::{HashSet, VecDeque};

        let mut queue: VecDeque<(String, GraphPath)> = VecDeque::new();
        let mut visited: HashSet<String> = HashSet::new();

        queue.push_back((source.clone(), GraphPath::start(&source)));
        visited.insert(source.clone());

        let max_hops = query.limit.unwrap_or(10) as usize;

        while let Some((current, path)) = queue.pop_front() {
            if path.len() > max_hops {
                continue;
            }

            if current == target {
                let mut record = UnifiedRecord::new();
                record.paths.push(path);
                result.push(record);
                break; // Found shortest path
            }

            for (edge_type, neighbor, weight) in self.graph.outgoing_edges(&current) {
                stats.edges_scanned += 1;

                if !visited.contains(&neighbor) {
                    visited.insert(neighbor.clone());
                    let edge = MatchedEdge::from_tuple(&current, edge_type, &neighbor, weight);
                    let new_path = path.extend(edge, &neighbor);
                    queue.push_back((neighbor, new_path));
                }
            }
        }

        if result.is_empty() {
            return Err(ExecutionError::new(format!(
                "No path found from {} to {}",
                source, target
            )));
        }

        Ok(())
    }

    /// Execute COUNT intent
    fn execute_count(
        &self,
        query: &NaturalQuery,
        result: &mut UnifiedResult,
        stats: &mut QueryStats,
    ) -> Result<(), ExecutionError> {
        let node_type = self.primary_entity_type(query);
        let mut count = 0u64;

        for node in self.graph.iter_nodes() {
            stats.nodes_scanned += 1;

            let type_matches = match &node_type {
                Some(t) => node.node_type == *t,
                None => true,
            };

            if type_matches && self.node_matches_filters(&node, &query.entities) {
                count += 1;
            }
        }

        let mut record = UnifiedRecord::new();
        record.set(
            "count",
            crate::storage::schema::Value::Integer(count as i64),
        );
        result.push(record);
        result.columns.push("count".to_string());

        Ok(())
    }

    /// Execute SHOW intent
    fn execute_show(
        &self,
        query: &NaturalQuery,
        result: &mut UnifiedResult,
        stats: &mut QueryStats,
    ) -> Result<(), ExecutionError> {
        // SHOW is like FIND but includes more details
        self.execute_find(query, result, stats)?;

        // Add neighbors for context
        if result.len() == 1 {
            if let Some(node) = result.records.first().and_then(|r| r.nodes.get("_")) {
                // Add outgoing connections
                for (edge_type, target, _) in self.graph.outgoing_edges(&node.id) {
                    stats.edges_scanned += 1;
                    if let Some(target_node) = self.graph.get_node(&target) {
                        let mut record = UnifiedRecord::new();
                        record.set_node("related", MatchedNode::from_stored(&target_node));
                        record.set(
                            "relationship",
                            crate::storage::schema::Value::Text(format!("{:?}", edge_type)),
                        );
                        result.push(record);
                    }
                }
            }
        }

        Ok(())
    }

    /// Execute CHECK intent - verify if a relationship exists
    fn execute_check(
        &self,
        query: &NaturalQuery,
        result: &mut UnifiedResult,
        stats: &mut QueryStats,
    ) -> Result<(), ExecutionError> {
        // Check requires two entities with a relationship
        let (source, target) = self.extract_path_endpoints(query)?;

        // Check if direct connection exists
        let mut found = false;
        for (edge_type, neighbor, weight) in self.graph.outgoing_edges(&source) {
            stats.edges_scanned += 1;
            if neighbor == target || neighbor.contains(&target) {
                found = true;
                // Add the relationship to result
                let mut record = UnifiedRecord::new();
                if let Some(src_node) = self.graph.get_node(&source) {
                    record.set_node("source", MatchedNode::from_stored(&src_node));
                }
                if let Some(tgt_node) = self.graph.get_node(&neighbor) {
                    record.set_node("target", MatchedNode::from_stored(&tgt_node));
                }
                record.set(
                    "relationship",
                    crate::storage::schema::Value::Text(format!("{:?}", edge_type)),
                );
                record.set("exists", crate::storage::schema::Value::Boolean(true));
                record.set(
                    "weight",
                    crate::storage::schema::Value::Float(weight as f64),
                );
                result.push(record);
                break;
            }
        }

        if !found {
            // Report that no relationship was found
            let mut record = UnifiedRecord::new();
            record.set("exists", crate::storage::schema::Value::Boolean(false));
            record.set("source", crate::storage::schema::Value::Text(source));
            record.set("target", crate::storage::schema::Value::Text(target));
            result.push(record);
        }

        result.columns = vec![
            "source".into(),
            "target".into(),
            "relationship".into(),
            "exists".into(),
        ];
        Ok(())
    }

    /// Get the primary entity type from query - maps EntityType to GraphNodeType
    fn primary_entity_type(&self, query: &NaturalQuery) -> Option<GraphNodeType> {
        for entity in &query.entities {
            match entity.entity_type {
                EntityType::Host => return Some(GraphNodeType::Host),
                EntityType::User => return Some(GraphNodeType::User),
                EntityType::Credential => return Some(GraphNodeType::Credential),
                EntityType::Service | EntityType::Port => return Some(GraphNodeType::Service),
                EntityType::Vulnerability => return Some(GraphNodeType::Vulnerability),
                EntityType::Technology => return Some(GraphNodeType::Technology),
                EntityType::Domain => return Some(GraphNodeType::Domain),
                EntityType::Certificate => return Some(GraphNodeType::Certificate),
                // Network doesn't have a GraphNodeType equivalent, skip
                EntityType::Network => continue,
            }
        }
        None
    }

    /// Check if node matches entity filters
    fn node_matches_filters(
        &self,
        node: &crate::storage::engine::graph_store::StoredNode,
        entities: &[ExtractedEntity],
    ) -> bool {
        for entity in entities {
            if let Some(ref value) = entity.value {
                // Check if value matches node ID or label
                let matches = node.id.contains(value)
                    || node.label.to_lowercase().contains(&value.to_lowercase())
                    || value.to_lowercase().contains(&node.label.to_lowercase());
                if matches {
                    return true;
                }
            }
        }
        // If no values to match, accept all
        entities.iter().all(|e| e.value.is_none())
    }

    /// Check if node has relationship to target
    fn has_relationship_to(&self, node_id: &str, target: &str, stats: &mut QueryStats) -> bool {
        for (_, neighbor, _) in self.graph.outgoing_edges(node_id) {
            stats.edges_scanned += 1;
            if neighbor.contains(target) {
                return true;
            }
            // Check neighbor's label
            if let Some(neighbor_node) = self.graph.get_node(&neighbor) {
                if neighbor_node
                    .label
                    .to_lowercase()
                    .contains(&target.to_lowercase())
                {
                    return true;
                }
            }
        }
        false
    }

    /// Extract path endpoints from query
    fn extract_path_endpoints(
        &self,
        query: &NaturalQuery,
    ) -> Result<(String, String), ExecutionError> {
        // Look for "from X to Y" pattern in entities
        let mut source = None;
        let mut target = None;

        for entity in &query.entities {
            if let Some(ref value) = entity.value {
                // Find nodes matching this value
                for node in self.graph.iter_nodes() {
                    if node.id.contains(value)
                        || node.label.to_lowercase().contains(&value.to_lowercase())
                    {
                        if source.is_none() {
                            source = Some(node.id.clone());
                        } else if target.is_none() && Some(&node.id) != source.as_ref() {
                            target = Some(node.id.clone());
                        }
                    }
                }
            }
        }

        match (source, target) {
            (Some(s), Some(t)) => Ok((s, t)),
            (Some(s), None) => Err(ExecutionError::new(format!(
                "Path query needs a target. Found source: {}",
                s
            ))),
            _ => Err(ExecutionError::new(
                "Path query needs source and target. Try: 'path from host X to host Y'",
            )),
        }
    }

    /// Generate explanation of query translation
    fn generate_explanation(&self, query: &NaturalQuery, original: &str) -> String {
        let mut explanation = Vec::new();

        explanation.push(format!("Query: \"{}\"", original));
        explanation.push(format!("Intent: {:?}", query.intent));

        if !query.entities.is_empty() {
            let entities: Vec<String> = query
                .entities
                .iter()
                .map(|e| {
                    if let Some(ref val) = e.value {
                        format!("{:?}({})", e.entity_type, val)
                    } else {
                        format!("{:?}", e.entity_type)
                    }
                })
                .collect();
            explanation.push(format!("Entities: {}", entities.join(", ")));
        }

        // Generate equivalent RQL
        let rql = self.to_rql(query);
        explanation.push(format!("Equivalent RQL: {}", rql));

        explanation.join("\n")
    }

    /// Convert natural query to RQL string
    fn to_rql(&self, query: &NaturalQuery) -> String {
        match query.intent {
            QueryIntent::Find => {
                let node_type = self
                    .primary_entity_type(query)
                    .map(|t| format!("{:?}", t))
                    .unwrap_or_else(|| "*".to_string());

                let filters: Vec<String> = query
                    .entities
                    .iter()
                    .filter_map(|e| {
                        e.value
                            .as_ref()
                            .map(|v| format!("n.label CONTAINS '{}'", v))
                    })
                    .collect();

                if filters.is_empty() {
                    format!("MATCH (n:{}) RETURN n", node_type)
                } else {
                    format!(
                        "MATCH (n:{}) WHERE {} RETURN n",
                        node_type,
                        filters.join(" AND ")
                    )
                }
            }
            QueryIntent::Path => {
                let endpoints: Vec<&str> = query
                    .entities
                    .iter()
                    .filter_map(|e| e.value.as_deref())
                    .collect();
                if endpoints.len() >= 2 {
                    format!("PATH FROM '{}' TO '{}'", endpoints[0], endpoints[1])
                } else {
                    "PATH FROM ? TO ?".to_string()
                }
            }
            QueryIntent::Count => {
                let node_type = self
                    .primary_entity_type(query)
                    .map(|t| format!("{:?}", t))
                    .unwrap_or_else(|| "*".to_string());
                format!("MATCH (n:{}) RETURN COUNT(n)", node_type)
            }
            QueryIntent::Show => {
                let filters: Vec<String> = query
                    .entities
                    .iter()
                    .filter_map(|e| e.value.as_ref().map(|v| format!("n.id = '{}'", v)))
                    .collect();
                if filters.is_empty() {
                    "MATCH (n) RETURN n".to_string()
                } else {
                    format!(
                        "MATCH (n) WHERE {} RETURN n, n.neighbors",
                        filters.first().unwrap()
                    )
                }
            }
            QueryIntent::Check => {
                let endpoints: Vec<&str> = query
                    .entities
                    .iter()
                    .filter_map(|e| e.value.as_deref())
                    .collect();
                if endpoints.len() >= 2 {
                    format!(
                        "MATCH (a)-[r]->(b) WHERE a.id = '{}' AND b.id = '{}' RETURN EXISTS(r)",
                        endpoints[0], endpoints[1]
                    )
                } else {
                    "MATCH (a)-[r]->(b) RETURN EXISTS(r)".to_string()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::ast::EdgeDirection;
    use crate::storage::query::test_support::service_graph_with_user;

    fn create_test_graph() -> Arc<GraphStore> {
        service_graph_with_user()
    }

    #[test]
    fn test_list_hosts() {
        let graph = create_test_graph();
        let executor = NaturalExecutor::new(graph);

        let (result, explanation) = executor.execute_with_explanation("list all hosts").unwrap();
        assert_eq!(result.records.len(), 2);
        // "list" maps to Find intent in NaturalParser
        assert!(explanation.contains("Intent: Find"));
    }

    #[test]
    fn test_find_services() {
        let graph = create_test_graph();
        let executor = NaturalExecutor::new(graph);

        let (result, explanation) = executor.execute_with_explanation("find services").unwrap();
        assert_eq!(result.records.len(), 2);
        assert!(explanation.contains("Service"));
    }

    #[test]
    fn test_count_hosts() {
        let graph = create_test_graph();
        let executor = NaturalExecutor::new(graph);

        let (result, _) = executor.execute_with_explanation("how many hosts").unwrap();
        assert_eq!(result.records.len(), 1);
        let count = result.records[0].values.get("count");
        assert!(count.is_some());
    }

    #[test]
    fn test_explanation_includes_rql() {
        let graph = create_test_graph();
        let executor = NaturalExecutor::new(graph);

        let (_, explanation) = executor
            .execute_with_explanation("find hosts with SSH")
            .unwrap();
        assert!(explanation.contains("Equivalent RQL:"));
        assert!(explanation.contains("MATCH"));
    }

    #[test]
    fn test_path_query() {
        let graph = create_test_graph();
        let executor = NaturalExecutor::new(graph);

        let (result, explanation) = executor
            .execute_with_explanation("path from host 10.0.0.1 to host 10.0.0.2")
            .unwrap();
        assert!(!result.is_empty());
        assert!(explanation.contains("Path"));
    }
}
