//! SPARQL Query Executor
//!
//! Implements Jena-inspired SPARQL execution with:
//! - Variable bindings propagated through pattern matching
//! - FILTER evaluation with expression types
//! - OPTIONAL blocks with left-join semantics
//!
//! # Architecture (inspired by Jena)
//!
//! ```text
//! SparqlQuery → BasicPattern → TriplePattern matching → Binding propagation
//!                    ↓                    ↓
//!               FILTER eval          OPTIONAL (left-join)
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::storage::engine::graph_store::{GraphEdgeType, GraphStore};
use crate::storage::query::ast::CompareOp;
use crate::storage::query::modes::sparql::{
    SparqlFilter, SparqlParser, SparqlQuery, SparqlTerm, TriplePattern,
};
use crate::storage::query::unified::{
    ExecutionError, MatchedEdge, MatchedNode, QueryStats, UnifiedRecord, UnifiedResult,
};
use crate::storage::schema::Value;

/// A variable binding represents a mapping from variable names to values
#[derive(Debug, Clone, Default)]
pub struct Binding {
    /// Variable bindings: varname → value
    values: HashMap<String, BoundValue>,
    /// Parent binding for scoped lookups
    parent: Option<Box<Binding>>,
}

/// A bound value in SPARQL
#[derive(Debug, Clone, PartialEq)]
pub enum BoundValue {
    /// Node reference
    Node(String),
    /// Edge reference (from, type, to)
    Edge(String, GraphEdgeType, String),
    /// Literal string
    Literal(String),
    /// Literal integer
    Integer(i64),
    /// Literal float
    Float(f64),
    /// Literal boolean
    Boolean(bool),
}

impl BoundValue {
    /// Get as node ID if this is a node
    pub fn as_node_id(&self) -> Option<&str> {
        match self {
            Self::Node(id) => Some(id),
            _ => None,
        }
    }

    /// Convert to string representation
    pub fn to_string_value(&self) -> String {
        match self {
            Self::Node(id) => id.clone(),
            Self::Edge(from, etype, to) => format!("{}--{:?}-->{}", from, etype, to),
            Self::Literal(s) => s.clone(),
            Self::Integer(i) => i.to_string(),
            Self::Float(f) => f.to_string(),
            Self::Boolean(b) => b.to_string(),
        }
    }
}

impl Binding {
    /// Create empty binding
    pub fn new() -> Self {
        Self::default()
    }

    /// Create binding with parent scope
    pub fn with_parent(parent: Binding) -> Self {
        Self {
            values: HashMap::new(),
            parent: Some(Box::new(parent)),
        }
    }

    /// Bind a variable
    pub fn bind(&mut self, var: &str, value: BoundValue) {
        // Remove leading ? if present
        let var_name = var.strip_prefix('?').unwrap_or(var);
        self.values.insert(var_name.to_string(), value);
    }

    /// Get a binding
    pub fn get(&self, var: &str) -> Option<&BoundValue> {
        let var_name = var.strip_prefix('?').unwrap_or(var);
        self.values
            .get(var_name)
            .or_else(|| self.parent.as_ref().and_then(|p| p.get(var_name)))
    }

    /// Check if variable is bound
    pub fn contains(&self, var: &str) -> bool {
        self.get(var).is_some()
    }

    /// Merge with another binding (for join)
    pub fn merge(&self, other: &Binding) -> Option<Binding> {
        let mut result = self.clone();
        for (var, value) in &other.values {
            if let Some(existing) = result.get(var) {
                // Check compatibility
                if existing != value {
                    return None; // Conflict
                }
            } else {
                result.bind(var, value.clone());
            }
        }
        Some(result)
    }

    /// Get all variable names
    pub fn vars(&self) -> Vec<String> {
        let mut vars: HashSet<_> = self.values.keys().cloned().collect();
        if let Some(ref parent) = self.parent {
            for v in parent.vars() {
                vars.insert(v);
            }
        }
        vars.into_iter().collect()
    }
}

/// SPARQL executor with variable binding semantics
pub struct SparqlExecutor {
    graph: Arc<GraphStore>,
}

impl SparqlExecutor {
    /// Create a new SPARQL executor
    pub fn new(graph: Arc<GraphStore>) -> Self {
        Self { graph }
    }

    /// Execute a SPARQL query string
    pub fn execute(&self, query: &str) -> Result<UnifiedResult, ExecutionError> {
        let parsed = SparqlParser::parse(query).map_err(|e| ExecutionError::new(e.to_string()))?;
        self.execute_query(&parsed)
    }

    /// Execute a parsed SPARQL query
    pub fn execute_query(&self, query: &SparqlQuery) -> Result<UnifiedResult, ExecutionError> {
        let mut stats = QueryStats::default();

        // Start with empty binding
        let initial = vec![Binding::new()];

        // Execute WHERE clause patterns
        let mut bindings = self.execute_patterns(&query.where_patterns, initial, &mut stats)?;

        // Apply FILTER clauses
        for filter in &query.filters {
            bindings = self.apply_filter(bindings, filter)?;
        }

        // Execute OPTIONAL blocks
        for optional in &query.optionals {
            bindings = self.execute_optional(bindings, optional, &mut stats)?;
        }

        // Apply LIMIT if present
        if let Some(limit) = query.limit {
            bindings.truncate(limit as usize);
        }

        // Project to selected variables
        self.project_results(&query.select, bindings, stats)
    }

    /// Execute triple patterns
    fn execute_patterns(
        &self,
        patterns: &[TriplePattern],
        bindings: Vec<Binding>,
        stats: &mut QueryStats,
    ) -> Result<Vec<Binding>, ExecutionError> {
        let mut current = bindings;

        for pattern in patterns {
            current = self.match_pattern(pattern, current, stats)?;
            if current.is_empty() {
                break;
            }
        }

        Ok(current)
    }

    /// Match a single triple pattern
    fn match_pattern(
        &self,
        pattern: &TriplePattern,
        bindings: Vec<Binding>,
        stats: &mut QueryStats,
    ) -> Result<Vec<Binding>, ExecutionError> {
        let mut results = Vec::new();

        for binding in bindings {
            // Resolve subject
            let subjects = self.resolve_term(&pattern.subject, &binding, stats);

            for subject in subjects {
                // Check if subject is a node
                let subject_id = match &subject {
                    BoundValue::Node(id) => id.clone(),
                    BoundValue::Literal(s) => s.clone(),
                    _ => continue,
                };

                // Get edges from subject
                for (edge_type, target, _weight) in self.graph.outgoing_edges(&subject_id) {
                    stats.edges_scanned += 1;

                    // Check predicate match
                    if !self.predicate_matches(&pattern.predicate, edge_type, &binding) {
                        continue;
                    }

                    // Check object match
                    let object_value = self.resolve_object(&pattern.object, &binding, &target);
                    if object_value.is_none() {
                        continue;
                    }

                    // Create new binding with matched values
                    let mut new_binding = binding.clone();

                    // Bind subject if variable
                    if let SparqlTerm::Variable(var) = &pattern.subject {
                        new_binding.bind(var, subject.clone());
                    }

                    // Bind predicate if variable
                    if let SparqlTerm::Variable(var) = &pattern.predicate {
                        new_binding.bind(var, BoundValue::Literal(format!("{:?}", edge_type)));
                    }

                    // Bind object if variable
                    if let SparqlTerm::Variable(var) = &pattern.object {
                        if let Some(obj) = object_value {
                            new_binding.bind(var, obj);
                        }
                    }

                    results.push(new_binding);
                }

                // Also check for node type patterns (rdf:type / 'a')
                if self.is_type_predicate(&pattern.predicate) {
                    if let Some(node) = self.graph.get_node(&subject_id) {
                        stats.nodes_scanned += 1;
                        let node_type_str = format!("{:?}", node.node_type);

                        if self.object_matches_type(&pattern.object, &node_type_str, &binding) {
                            let mut new_binding = binding.clone();

                            if let SparqlTerm::Variable(var) = &pattern.subject {
                                new_binding.bind(var, BoundValue::Node(subject_id.clone()));
                            }
                            if let SparqlTerm::Variable(var) = &pattern.object {
                                new_binding.bind(var, BoundValue::Literal(node_type_str));
                            }

                            results.push(new_binding);
                        }
                    }
                }
            }
        }

        Ok(results)
    }

    /// Resolve a SPARQL term to possible values
    fn resolve_term(
        &self,
        term: &SparqlTerm,
        binding: &Binding,
        stats: &mut QueryStats,
    ) -> Vec<BoundValue> {
        match term {
            SparqlTerm::Variable(var) => {
                // Check if bound
                if let Some(value) = binding.get(var) {
                    return vec![value.clone()];
                }
                // Unbound variable - return all nodes
                self.graph
                    .iter_nodes()
                    .map(|n| {
                        stats.nodes_scanned += 1;
                        BoundValue::Node(n.id.clone())
                    })
                    .collect()
            }
            SparqlTerm::PrefixedName(prefix, local) => {
                let id = if prefix.is_empty() {
                    local.clone()
                } else {
                    format!("{}:{}", prefix, local)
                };
                vec![BoundValue::Node(id)]
            }
            SparqlTerm::Iri(iri) => {
                // Extract local name from IRI
                let id = iri
                    .rsplit('/')
                    .next()
                    .or_else(|| iri.rsplit('#').next())
                    .unwrap_or(iri);
                vec![BoundValue::Node(id.to_string())]
            }
            SparqlTerm::Literal(lit) => {
                vec![BoundValue::Literal(lit.clone())]
            }
            SparqlTerm::TypedLiteral(lit, _datatype) => {
                vec![BoundValue::Literal(lit.clone())]
            }
            SparqlTerm::Number(n) => {
                vec![BoundValue::Float(*n)]
            }
            SparqlTerm::Boolean(b) => {
                vec![BoundValue::Boolean(*b)]
            }
            SparqlTerm::A => {
                vec![BoundValue::Literal("rdf:type".to_string())]
            }
        }
    }

    /// Check if predicate matches edge type
    fn predicate_matches(
        &self,
        predicate: &SparqlTerm,
        edge_type: GraphEdgeType,
        binding: &Binding,
    ) -> bool {
        match predicate {
            SparqlTerm::Variable(var) => {
                if let Some(bound) = binding.get(var) {
                    let bound_str = bound.to_string_value().to_lowercase();
                    let edge_str = format!("{:?}", edge_type).to_lowercase();
                    return bound_str == edge_str || edge_str.contains(&bound_str);
                }
                true // Unbound variable matches all
            }
            SparqlTerm::PrefixedName(_, local) => {
                let pred_clean = local.to_lowercase();
                let edge_str = format!("{:?}", edge_type).to_lowercase();
                edge_str == pred_clean
                    || edge_str.contains(&pred_clean)
                    || self.predicate_alias_matches(&pred_clean, edge_type)
            }
            SparqlTerm::Iri(iri) => {
                let local = iri
                    .rsplit('/')
                    .next()
                    .or_else(|| iri.rsplit('#').next())
                    .unwrap_or(iri);
                let pred_clean = local.to_lowercase();
                let edge_str = format!("{:?}", edge_type).to_lowercase();
                edge_str == pred_clean
                    || edge_str.contains(&pred_clean)
                    || self.predicate_alias_matches(&pred_clean, edge_type)
            }
            SparqlTerm::A => false, // 'a' is for type, not edges
            _ => false,
        }
    }

    /// Check predicate aliases
    /// Maps SPARQL predicate names to available GraphEdgeType variants
    fn predicate_alias_matches(&self, predicate: &str, edge_type: GraphEdgeType) -> bool {
        match (predicate, edge_type) {
            // Direct mappings
            ("hasservice" | "has_service" | "service", GraphEdgeType::HasService) => true,
            ("connectsto" | "connects_to" | "connects", GraphEdgeType::ConnectsTo) => true,
            ("hasuser" | "has_user", GraphEdgeType::HasUser) => true,
            ("usestech" | "uses_tech" | "uses", GraphEdgeType::UsesTech) => true,
            ("authaccess" | "auth_access", GraphEdgeType::AuthAccess) => true,
            ("hasendpoint" | "has_endpoint", GraphEdgeType::HasEndpoint) => true,
            (
                "hascert" | "has_cert" | "hascertificate" | "has_certificate",
                GraphEdgeType::HasCert,
            ) => true,
            ("contains" | "has_subdomain" | "hassubdomain", GraphEdgeType::Contains) => true,
            (
                "affectedby" | "affected_by" | "hasvulnerability" | "has_vuln" | "vulnerable_to",
                GraphEdgeType::AffectedBy,
            ) => true,
            ("relatedto" | "related_to" | "memberof" | "member_of", GraphEdgeType::RelatedTo) => {
                true
            }
            _ => false,
        }
    }

    /// Check if predicate is rdf:type or 'a'
    fn is_type_predicate(&self, predicate: &SparqlTerm) -> bool {
        match predicate {
            SparqlTerm::A => true,
            SparqlTerm::PrefixedName(_prefix, local) => {
                local == "type" // Matches rdf:type, foo:type, etc.
            }
            SparqlTerm::Iri(iri) => iri.ends_with("type") || iri.ends_with("#type"),
            _ => false,
        }
    }

    /// Check if object matches node type
    fn object_matches_type(&self, object: &SparqlTerm, node_type: &str, binding: &Binding) -> bool {
        match object {
            SparqlTerm::Variable(var) => {
                if let Some(bound) = binding.get(var) {
                    bound.to_string_value().to_lowercase() == node_type.to_lowercase()
                } else {
                    true // Unbound - will match any
                }
            }
            SparqlTerm::PrefixedName(_, local) => {
                node_type.to_lowercase() == local.to_lowercase()
                    || node_type.to_lowercase().contains(&local.to_lowercase())
            }
            SparqlTerm::Iri(iri) => {
                let local = iri
                    .rsplit('/')
                    .next()
                    .or_else(|| iri.rsplit('#').next())
                    .unwrap_or(iri);
                node_type.to_lowercase() == local.to_lowercase()
                    || node_type.to_lowercase().contains(&local.to_lowercase())
            }
            SparqlTerm::Literal(lit) => {
                node_type.to_lowercase() == lit.to_lowercase()
                    || node_type.to_lowercase().contains(&lit.to_lowercase())
            }
            _ => false,
        }
    }

    /// Resolve object value
    fn resolve_object(
        &self,
        object: &SparqlTerm,
        binding: &Binding,
        target: &str,
    ) -> Option<BoundValue> {
        match object {
            SparqlTerm::Variable(var) => {
                if let Some(bound) = binding.get(var) {
                    // Must match target
                    if bound.as_node_id() == Some(target) {
                        return Some(bound.clone());
                    }
                    return None;
                }
                // Unbound - return target
                Some(BoundValue::Node(target.to_string()))
            }
            SparqlTerm::PrefixedName(_, local) => {
                if target == local || target.ends_with(local) || target.contains(local) {
                    Some(BoundValue::Node(target.to_string()))
                } else {
                    None
                }
            }
            SparqlTerm::Iri(iri) => {
                let id = iri
                    .rsplit('/')
                    .next()
                    .or_else(|| iri.rsplit('#').next())
                    .unwrap_or(iri);
                if target == id || target.ends_with(id) || target.contains(id) {
                    Some(BoundValue::Node(target.to_string()))
                } else {
                    None
                }
            }
            SparqlTerm::Literal(_) | SparqlTerm::TypedLiteral(_, _) => {
                // Literal can't match edge target
                None
            }
            _ => None,
        }
    }

    /// Apply FILTER expression
    fn apply_filter(
        &self,
        bindings: Vec<Binding>,
        filter: &SparqlFilter,
    ) -> Result<Vec<Binding>, ExecutionError> {
        Ok(bindings
            .into_iter()
            .filter(|b| self.evaluate_filter(filter, b))
            .collect())
    }

    /// Evaluate a filter expression
    fn evaluate_filter(&self, filter: &SparqlFilter, binding: &Binding) -> bool {
        match filter {
            SparqlFilter::Compare(var, op, term) => {
                if let Some(bound) = binding.get(var) {
                    let bound_str = bound.to_string_value();
                    let term_str = self.term_to_string(term);

                    match op {
                        CompareOp::Eq => bound_str.to_lowercase() == term_str.to_lowercase(),
                        CompareOp::Ne => bound_str.to_lowercase() != term_str.to_lowercase(),
                        CompareOp::Lt => self.compare_numeric(&bound_str, &term_str, |a, b| a < b),
                        CompareOp::Le => self.compare_numeric(&bound_str, &term_str, |a, b| a <= b),
                        CompareOp::Gt => self.compare_numeric(&bound_str, &term_str, |a, b| a > b),
                        CompareOp::Ge => self.compare_numeric(&bound_str, &term_str, |a, b| a >= b),
                    }
                } else {
                    false
                }
            }
            SparqlFilter::Regex(var, pattern, _flags) => {
                if let Some(value) = binding.get(var) {
                    let s = value.to_string_value();
                    s.contains(pattern) // Simplified regex
                } else {
                    false
                }
            }
            SparqlFilter::Bound(var) => binding.contains(var),
            SparqlFilter::NotBound(var) => !binding.contains(var),
            SparqlFilter::IsIri(var) => binding
                .get(var)
                .map(|v| matches!(v, BoundValue::Node(_)))
                .unwrap_or(false),
            SparqlFilter::IsLiteral(var) => binding
                .get(var)
                .map(|v| !matches!(v, BoundValue::Node(_)))
                .unwrap_or(false),
            SparqlFilter::Contains(var, substring) => {
                if let Some(value) = binding.get(var) {
                    value.to_string_value().contains(substring)
                } else {
                    false
                }
            }
            SparqlFilter::StrStarts(var, prefix) => {
                if let Some(value) = binding.get(var) {
                    value.to_string_value().starts_with(prefix)
                } else {
                    false
                }
            }
            SparqlFilter::StrEnds(var, suffix) => {
                if let Some(value) = binding.get(var) {
                    value.to_string_value().ends_with(suffix)
                } else {
                    false
                }
            }
            SparqlFilter::And(left, right) => {
                self.evaluate_filter(left, binding) && self.evaluate_filter(right, binding)
            }
            SparqlFilter::Or(left, right) => {
                self.evaluate_filter(left, binding) || self.evaluate_filter(right, binding)
            }
            SparqlFilter::Not(inner) => !self.evaluate_filter(inner, binding),
        }
    }

    /// Convert a SparqlTerm to string
    fn term_to_string(&self, term: &SparqlTerm) -> String {
        match term {
            SparqlTerm::Variable(v) => format!("?{}", v),
            SparqlTerm::PrefixedName(p, l) => {
                if p.is_empty() {
                    l.clone()
                } else {
                    format!("{}:{}", p, l)
                }
            }
            SparqlTerm::Iri(iri) => iri.clone(),
            SparqlTerm::Literal(lit) => lit.clone(),
            SparqlTerm::TypedLiteral(lit, _) => lit.clone(),
            SparqlTerm::Number(n) => n.to_string(),
            SparqlTerm::Boolean(b) => b.to_string(),
            SparqlTerm::A => "rdf:type".to_string(),
        }
    }

    /// Compare numeric values
    fn compare_numeric<F>(&self, a: &str, b: &str, f: F) -> bool
    where
        F: Fn(f64, f64) -> bool,
    {
        let a_num: f64 = a.parse().unwrap_or(0.0);
        let b_num: f64 = b.parse().unwrap_or(0.0);
        f(a_num, b_num)
    }

    /// Execute OPTIONAL block (left-join semantics)
    fn execute_optional(
        &self,
        bindings: Vec<Binding>,
        optional_patterns: &[TriplePattern],
        stats: &mut QueryStats,
    ) -> Result<Vec<Binding>, ExecutionError> {
        let mut results = Vec::new();

        for binding in bindings {
            // Try to match optional patterns
            let optional_matches =
                self.execute_patterns(optional_patterns, vec![binding.clone()], stats)?;

            if optional_matches.is_empty() {
                // No match - keep original binding (left-join semantics)
                results.push(binding);
            } else {
                // Matches found - add extended bindings
                results.extend(optional_matches);
            }
        }

        Ok(results)
    }

    /// Project results to selected variables
    fn project_results(
        &self,
        select: &[String],
        bindings: Vec<Binding>,
        stats: QueryStats,
    ) -> Result<UnifiedResult, ExecutionError> {
        let mut result = UnifiedResult::empty();
        result.stats = stats;

        // Determine columns
        let columns: Vec<String> = if select.is_empty() || select.iter().any(|s| s == "*") {
            // SELECT * - get all variables from first binding
            if let Some(first) = bindings.first() {
                first.vars()
            } else {
                Vec::new()
            }
        } else {
            select
                .iter()
                .map(|s| s.strip_prefix('?').unwrap_or(s).to_string())
                .collect()
        };
        result.columns = columns.clone();

        // Convert bindings to records
        for binding in bindings {
            let mut record = UnifiedRecord::new();

            for col in &columns {
                if let Some(value) = binding.get(col) {
                    match value {
                        BoundValue::Node(id) => {
                            // Try to get node info
                            if let Some(node) = self.graph.get_node(id) {
                                record.set_node(col, MatchedNode::from_stored(&node));
                            }
                            record.set(col, Value::Text(id.clone()));
                        }
                        BoundValue::Edge(from, etype, to) => {
                            record.set_edge(col, MatchedEdge::from_tuple(from, *etype, to, 1.0));
                            record.set(
                                col,
                                Value::Text(format!(
                                    "{}->{}({})",
                                    from,
                                    to,
                                    format!("{:?}", etype)
                                )),
                            );
                        }
                        BoundValue::Literal(s) => {
                            record.set(col, Value::Text(s.clone()));
                        }
                        BoundValue::Integer(i) => {
                            record.set(col, Value::Integer(*i));
                        }
                        BoundValue::Float(f) => {
                            record.set(col, Value::Float(*f));
                        }
                        BoundValue::Boolean(b) => {
                            record.set(col, Value::Boolean(*b));
                        }
                    }
                }
            }

            result.push(record);
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::engine::graph_store::GraphNodeType;

    fn create_test_graph() -> Arc<GraphStore> {
        let graph = GraphStore::new();

        // Add test nodes
        graph.add_node("host:10.0.0.1", "webserver", GraphNodeType::Host);
        graph.add_node("host:10.0.0.2", "database", GraphNodeType::Host);
        graph.add_node("svc:ssh", "SSH", GraphNodeType::Service);
        graph.add_node("svc:http", "HTTP", GraphNodeType::Service);
        graph.add_node("user:admin", "admin", GraphNodeType::User);

        // Add edges
        graph.add_edge("host:10.0.0.1", "svc:ssh", GraphEdgeType::HasService, 1.0);
        graph.add_edge("host:10.0.0.1", "svc:http", GraphEdgeType::HasService, 1.0);
        graph.add_edge(
            "host:10.0.0.1",
            "host:10.0.0.2",
            GraphEdgeType::ConnectsTo,
            1.0,
        );
        graph.add_edge("host:10.0.0.2", "svc:ssh", GraphEdgeType::HasService, 1.0);
        graph.add_edge("host:10.0.0.1", "user:admin", GraphEdgeType::HasUser, 1.0);

        Arc::new(graph)
    }

    #[test]
    fn test_simple_pattern() {
        let graph = create_test_graph();
        let executor = SparqlExecutor::new(graph);

        let result = executor
            .execute("SELECT ?s WHERE { ?s :hasService ?o }")
            .unwrap();
        assert!(!result.is_empty());
    }

    #[test]
    fn test_type_pattern() {
        let graph = create_test_graph();
        let executor = SparqlExecutor::new(graph);

        let result = executor.execute("SELECT ?h WHERE { ?h a :Host }").unwrap();
        assert_eq!(result.records.len(), 2); // 2 hosts
    }

    #[test]
    fn test_binding() {
        let mut binding = Binding::new();
        binding.bind("?x", BoundValue::Node("test".to_string()));

        assert!(binding.contains("?x"));
        assert!(binding.contains("x")); // Should work without ?
        assert_eq!(binding.get("x").unwrap().as_node_id(), Some("test"));
    }

    #[test]
    fn test_optional() {
        let graph = create_test_graph();
        let executor = SparqlExecutor::new(graph);

        let result = executor
            .execute("SELECT ?h ?u WHERE { ?h a :Host } OPTIONAL { ?h :hasUser ?u }")
            .unwrap();
        // Should have 2 hosts, one with user bound
        assert_eq!(result.records.len(), 2);
    }
}
