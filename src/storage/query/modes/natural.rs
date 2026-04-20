//! Natural Language Query Parser
//!
//! Translates natural language queries to graph patterns:
//! - "find all hosts with ssh open"
//! - "show me credentials for user admin"
//! - "what vulnerabilities affect host 10.0.0.1?"
//! - "list users with weak passwords"
//!
//! # Approach
//!
//! 1. Intent classification (find, show, list, count, path)
//! 2. Entity extraction (hosts, users, credentials, vulnerabilities)
//! 3. Property extraction (ip, name, port, cve)
//! 4. Relationship inference (connects, has, affects)
//! 5. Generate equivalent graph query

use crate::storage::engine::graph_store::{GraphEdgeType, GraphNodeType};
use crate::storage::query::ast::{
    CompareOp, EdgeDirection, EdgePattern, FieldRef, Filter, GraphPattern, GraphQuery, NodePattern,
    Projection, PropertyFilter as AstPropertyFilter, QueryExpr,
};
use crate::storage::schema::Value;

/// Natural language parse error
#[derive(Debug, Clone)]
pub struct NaturalError {
    pub message: String,
}

impl std::fmt::Display for NaturalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Natural language error: {}", self.message)
    }
}

impl std::error::Error for NaturalError {}

/// A parsed natural language query
#[derive(Debug, Clone)]
pub struct NaturalQuery {
    /// The detected intent
    pub intent: QueryIntent,
    /// Primary entity type
    pub primary_entity: Option<EntityType>,
    /// Secondary entity (for relationships)
    pub secondary_entity: Option<EntityType>,
    /// Extracted entities with values
    pub entities: Vec<ExtractedEntity>,
    /// Property filters
    pub filters: Vec<PropertyFilter>,
    /// Relationship type (if any)
    pub relationship: Option<RelationshipType>,
    /// Limit on results
    pub limit: Option<u64>,
}

/// Query intent
#[derive(Debug, Clone, PartialEq)]
pub enum QueryIntent {
    /// Find/list entities
    Find,
    /// Show details
    Show,
    /// Count entities
    Count,
    /// Find path between entities
    Path,
    /// Check if relationship exists
    Check,
}

/// Entity types in the security domain
#[derive(Debug, Clone, PartialEq)]
pub enum EntityType {
    Host,
    Service,
    Port,
    User,
    Credential,
    Vulnerability,
    Technology,
    Domain,
    Certificate,
    Network,
}

/// An extracted entity mention
#[derive(Debug, Clone)]
pub struct ExtractedEntity {
    pub entity_type: EntityType,
    pub value: Option<String>,
    pub alias: String,
}

/// Property filter from natural language
#[derive(Debug, Clone)]
pub struct PropertyFilter {
    pub property: String,
    pub op: CompareOp,
    pub value: String,
}

/// Relationship types
#[derive(Debug, Clone, PartialEq)]
pub enum RelationshipType {
    HasService,
    HasPort,
    HasVuln,
    HasCredential,
    HasUser,
    ConnectsTo,
    Affects,
    AuthAccess,
    Uses,
    RunsOn,
    Exposes,
}

/// Natural language parser
pub struct NaturalParser;

impl NaturalParser {
    /// Parse a natural language query
    pub fn parse(input: &str) -> Result<NaturalQuery, NaturalError> {
        let text = Self::normalize(input);
        let tokens: Vec<&str> = text.split_whitespace().collect();

        if tokens.is_empty() {
            return Err(NaturalError {
                message: "Empty query".to_string(),
            });
        }

        // Detect intent
        let intent = Self::detect_intent(&tokens);

        // Extract entities
        let entities = Self::extract_entities(&text);

        // Determine primary and secondary entity types
        let (primary, secondary) = Self::determine_entity_types(&entities, &text);

        // Extract property filters
        let filters = Self::extract_filters(&text);

        // Detect relationship
        let relationship = Self::detect_relationship(&text, &primary, &secondary);

        // Extract limit
        let limit = Self::extract_limit(&text);

        Ok(NaturalQuery {
            intent,
            primary_entity: primary,
            secondary_entity: secondary,
            entities,
            filters,
            relationship,
            limit,
        })
    }

    /// Normalize input text
    fn normalize(input: &str) -> String {
        // Remove quotes if present
        let trimmed = input.trim();
        let unquoted = if (trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
        {
            &trimmed[1..trimmed.len() - 1]
        } else {
            trimmed
        };

        // Convert to lowercase and remove punctuation (except relevant chars)
        unquoted
            .to_lowercase()
            .chars()
            .map(|c| {
                if c.is_alphanumeric()
                    || c.is_whitespace()
                    || c == '.'
                    || c == ':'
                    || c == '-'
                    || c == '_'
                {
                    c
                } else {
                    ' '
                }
            })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Detect query intent from tokens
    fn detect_intent(tokens: &[&str]) -> QueryIntent {
        let first = tokens.first().copied().unwrap_or("");

        match first {
            "find" | "search" | "list" | "get" | "fetch" | "retrieve" => QueryIntent::Find,
            "show" | "display" | "view" | "describe" | "detail" | "details" => QueryIntent::Show,
            "count" | "how" => {
                if tokens.contains(&"many") || tokens.contains(&"count") {
                    QueryIntent::Count
                } else {
                    QueryIntent::Find
                }
            }
            "path" | "paths" | "route" | "reach" | "reachable" => QueryIntent::Path,
            "is" | "are" | "does" | "can" | "check" => QueryIntent::Check,
            "what" | "which" | "where" | "who" => {
                // Question words usually mean find
                QueryIntent::Find
            }
            _ => QueryIntent::Find,
        }
    }

    /// Extract entities from text
    fn extract_entities(text: &str) -> Vec<ExtractedEntity> {
        let mut entities = Vec::new();
        let mut alias_counter = 0;

        // Entity patterns with regex-like matching
        let entity_patterns: Vec<(EntityType, &[&str], Option<&str>)> = vec![
            (
                EntityType::Host,
                &[
                    "host", "hosts", "server", "servers", "machine", "machines", "ip", "ips",
                ],
                None,
            ),
            (EntityType::Service, &["service", "services"], None),
            (EntityType::Port, &["port", "ports"], None),
            (
                EntityType::User,
                &[
                    "user",
                    "users",
                    "account",
                    "accounts",
                    "username",
                    "usernames",
                ],
                None,
            ),
            (
                EntityType::Credential,
                &[
                    "credential",
                    "credentials",
                    "password",
                    "passwords",
                    "cred",
                    "creds",
                ],
                None,
            ),
            (
                EntityType::Vulnerability,
                &[
                    "vulnerability",
                    "vulnerabilities",
                    "vuln",
                    "vulns",
                    "cve",
                    "cves",
                ],
                None,
            ),
            (
                EntityType::Technology,
                &[
                    "technology",
                    "technologies",
                    "tech",
                    "software",
                    "application",
                    "applications",
                ],
                None,
            ),
            (
                EntityType::Domain,
                &["domain", "domains", "subdomain", "subdomains"],
                None,
            ),
            (
                EntityType::Certificate,
                &["certificate", "certificates", "cert", "certs", "ssl", "tls"],
                None,
            ),
            (
                EntityType::Network,
                &[
                    "network", "networks", "subnet", "subnets", "segment", "segments",
                ],
                None,
            ),
        ];

        for (entity_type, keywords, _) in entity_patterns {
            for keyword in keywords.iter() {
                if text.contains(keyword) {
                    // Try to extract associated value
                    let value = Self::extract_entity_value(text, keyword);

                    entities.push(ExtractedEntity {
                        entity_type: entity_type.clone(),
                        value,
                        alias: format!("e{}", alias_counter),
                    });
                    alias_counter += 1;
                    break; // Only add once per entity type
                }
            }
        }

        // Extract IP addresses
        for word in text.split_whitespace() {
            if Self::is_ip_address(word) {
                let already_has_host = entities
                    .iter()
                    .any(|e| e.entity_type == EntityType::Host && e.value.as_deref() == Some(word));
                if !already_has_host {
                    entities.push(ExtractedEntity {
                        entity_type: EntityType::Host,
                        value: Some(word.to_string()),
                        alias: format!("e{}", alias_counter),
                    });
                    alias_counter += 1;
                }
            }
        }

        // Extract CVE IDs
        for word in text.split_whitespace() {
            if word.starts_with("cve-") || word.starts_with("cve:") {
                let cve = word
                    .replace("cve:", "CVE-")
                    .replace("cve-", "CVE-")
                    .to_uppercase();
                entities.push(ExtractedEntity {
                    entity_type: EntityType::Vulnerability,
                    value: Some(cve),
                    alias: format!("e{}", alias_counter),
                });
                alias_counter += 1;
            }
        }

        entities
    }

    /// Extract value associated with an entity keyword
    fn extract_entity_value(text: &str, keyword: &str) -> Option<String> {
        // Look for patterns like "host 10.0.0.1" or "user admin"
        let parts: Vec<&str> = text.split_whitespace().collect();

        for (i, part) in parts.iter().enumerate() {
            if *part == keyword {
                // Check next word
                if let Some(next) = parts.get(i + 1) {
                    // Skip common words
                    if ![
                        "with", "that", "has", "have", "is", "are", "the", "a", "an", "for", "on",
                        "in",
                    ]
                    .contains(next)
                    {
                        return Some(next.to_string());
                    }
                    // Check word after that
                    if let Some(next2) = parts.get(i + 2) {
                        if ![
                            "with", "that", "has", "have", "is", "are", "the", "a", "an", "for",
                            "on", "in",
                        ]
                        .contains(next2)
                        {
                            return Some(next2.to_string());
                        }
                    }
                }
            }
        }

        None
    }

    /// Check if a string looks like an IP address
    fn is_ip_address(s: &str) -> bool {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 4 {
            return false;
        }
        parts.iter().all(|p| p.parse::<u8>().is_ok())
    }

    /// Determine primary and secondary entity types
    fn determine_entity_types(
        entities: &[ExtractedEntity],
        text: &str,
    ) -> (Option<EntityType>, Option<EntityType>) {
        if entities.is_empty() {
            // Infer from text
            if text.contains("host") || text.contains("server") || text.contains("ip") {
                return (Some(EntityType::Host), None);
            }
            if text.contains("vuln") || text.contains("cve") {
                return (Some(EntityType::Vulnerability), None);
            }
            if text.contains("user") || text.contains("account") {
                return (Some(EntityType::User), None);
            }
            if text.contains("cred") || text.contains("password") {
                return (Some(EntityType::Credential), None);
            }
            if text.contains("service") {
                return (Some(EntityType::Service), None);
            }
            return (None, None);
        }

        let primary = entities.first().map(|e| e.entity_type.clone());
        let secondary = entities.get(1).map(|e| e.entity_type.clone());

        (primary, secondary)
    }

    /// Extract property filters from text
    fn extract_filters(text: &str) -> Vec<PropertyFilter> {
        let mut filters = Vec::new();

        // Port number patterns
        if text.contains("port") {
            for word in text.split_whitespace() {
                if let Ok(port) = word.parse::<u16>() {
                    if port > 0 {
                        // u16 already constrains to 0-65535
                        filters.push(PropertyFilter {
                            property: "port".to_string(),
                            op: CompareOp::Eq,
                            value: port.to_string(),
                        });
                    }
                }
            }
        }

        // Common service names
        let services = [
            "ssh", "http", "https", "ftp", "smtp", "mysql", "postgres", "redis", "mongodb", "rdp",
            "vnc",
        ];
        for svc in services {
            if text.contains(svc) {
                filters.push(PropertyFilter {
                    property: "service".to_string(),
                    op: CompareOp::Eq,
                    value: svc.to_string(),
                });
            }
        }

        // Critical/high/medium/low severity
        if text.contains("critical") {
            filters.push(PropertyFilter {
                property: "severity".to_string(),
                op: CompareOp::Eq,
                value: "critical".to_string(),
            });
        } else if text.contains("high") {
            filters.push(PropertyFilter {
                property: "severity".to_string(),
                op: CompareOp::Ge,
                value: "7.0".to_string(),
            });
        } else if text.contains("medium") {
            filters.push(PropertyFilter {
                property: "severity".to_string(),
                op: CompareOp::Ge,
                value: "4.0".to_string(),
            });
        }

        // Weak passwords
        if text.contains("weak") && (text.contains("password") || text.contains("credential")) {
            filters.push(PropertyFilter {
                property: "strength".to_string(),
                op: CompareOp::Eq,
                value: "weak".to_string(),
            });
        }

        // Open/exposed
        if text.contains("open") || text.contains("exposed") || text.contains("public") {
            filters.push(PropertyFilter {
                property: "status".to_string(),
                op: CompareOp::Eq,
                value: "open".to_string(),
            });
        }

        filters
    }

    /// Detect relationship type from text
    fn detect_relationship(
        text: &str,
        primary: &Option<EntityType>,
        secondary: &Option<EntityType>,
    ) -> Option<RelationshipType> {
        // Explicit relationship keywords
        if text.contains("connects to") || text.contains("connected to") || text.contains("reach") {
            return Some(RelationshipType::ConnectsTo);
        }
        if text.contains("affects") || text.contains("affected by") || text.contains("vulnerable") {
            return Some(RelationshipType::Affects);
        }
        if text.contains("has access")
            || text.contains("can access")
            || text.contains("authenticate")
        {
            return Some(RelationshipType::AuthAccess);
        }
        if text.contains("runs on") || text.contains("running on") {
            return Some(RelationshipType::RunsOn);
        }
        if text.contains("uses") || text.contains("using") {
            return Some(RelationshipType::Uses);
        }
        if text.contains("exposes") || text.contains("exposing") {
            return Some(RelationshipType::Exposes);
        }

        // Infer from entity types
        match (primary, secondary) {
            (Some(EntityType::Host), Some(EntityType::Service)) => {
                Some(RelationshipType::HasService)
            }
            (Some(EntityType::Host), Some(EntityType::Port)) => Some(RelationshipType::HasPort),
            (Some(EntityType::Host), Some(EntityType::Vulnerability)) => {
                Some(RelationshipType::HasVuln)
            }
            (Some(EntityType::User), Some(EntityType::Credential)) => {
                Some(RelationshipType::HasCredential)
            }
            (Some(EntityType::Credential), Some(EntityType::Host)) => {
                Some(RelationshipType::AuthAccess)
            }
            (Some(EntityType::Vulnerability), Some(EntityType::Host)) => {
                Some(RelationshipType::Affects)
            }
            _ => None,
        }
    }

    /// Extract limit from text
    fn extract_limit(text: &str) -> Option<u64> {
        let patterns = [("top ", 4), ("first ", 6), ("limit ", 6), ("show ", 5)];

        for (pattern, skip) in patterns {
            if let Some(pos) = text.find(pattern) {
                let after = &text[pos + skip..];
                let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = num_str.parse::<u64>() {
                    return Some(n);
                }
            }
        }

        None
    }
}

impl NaturalQuery {
    /// Convert to QueryExpr
    pub fn to_query_expr(&self) -> QueryExpr {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut filters = Vec::new();

        // Create nodes from extracted entities
        for entity in &self.entities {
            let node_type = match entity.entity_type {
                EntityType::Host => Some("host".to_string()),
                EntityType::Service => Some("service".to_string()),
                EntityType::User => Some("user".to_string()),
                EntityType::Credential => Some("credential".to_string()),
                EntityType::Vulnerability => Some("vulnerability".to_string()),
                EntityType::Technology => Some("technology".to_string()),
                EntityType::Domain => Some("domain".to_string()),
                EntityType::Certificate => Some("certificate".to_string()),
                _ => None,
            };

            let mut properties: Vec<AstPropertyFilter> = Vec::new();
            if let Some(ref value) = entity.value {
                properties.push(AstPropertyFilter {
                    name: "id".to_string(),
                    op: CompareOp::Eq,
                    value: Value::text(value.clone()),
                });
            }

            // Map string node_type to GraphNodeType
            let graph_node_type = node_type.as_ref().and_then(|t| match t.as_str() {
                "host" => Some(GraphNodeType::Host),
                "service" => Some(GraphNodeType::Service),
                "user" => Some(GraphNodeType::User),
                "credential" => Some(GraphNodeType::Credential),
                "vulnerability" => Some(GraphNodeType::Vulnerability),
                "technology" => Some(GraphNodeType::Technology),
                "domain" => Some(GraphNodeType::Domain),
                "certificate" => Some(GraphNodeType::Certificate),
                "endpoint" => Some(GraphNodeType::Endpoint),
                _ => None,
            });

            nodes.push(NodePattern {
                alias: entity.alias.clone(),
                node_type: graph_node_type,
                properties,
            });
        }

        // Add edges based on relationships
        if let Some(ref relationship) = self.relationship {
            if nodes.len() >= 2 {
                let edge_type = match relationship {
                    RelationshipType::HasService => Some(GraphEdgeType::HasService),
                    RelationshipType::HasPort => Some(GraphEdgeType::HasEndpoint),
                    RelationshipType::HasVuln => Some(GraphEdgeType::AffectedBy),
                    RelationshipType::HasCredential => Some(GraphEdgeType::AuthAccess),
                    RelationshipType::HasUser => Some(GraphEdgeType::HasUser),
                    RelationshipType::ConnectsTo => Some(GraphEdgeType::ConnectsTo),
                    RelationshipType::Affects => Some(GraphEdgeType::AffectedBy),
                    RelationshipType::AuthAccess => Some(GraphEdgeType::AuthAccess),
                    RelationshipType::Uses => Some(GraphEdgeType::UsesTech),
                    RelationshipType::RunsOn => Some(GraphEdgeType::Contains),
                    RelationshipType::Exposes => Some(GraphEdgeType::HasEndpoint),
                };

                edges.push(EdgePattern {
                    alias: None,
                    from: nodes[0].alias.clone(),
                    to: nodes[1].alias.clone(),
                    edge_type,
                    direction: EdgeDirection::Outgoing,
                    min_hops: 1,
                    max_hops: 1,
                });
            }
        }

        // Convert property filters
        let current_alias = nodes
            .first()
            .map(|n| n.alias.clone())
            .unwrap_or_else(|| "n0".to_string());
        for filter in &self.filters {
            filters.push(Filter::Compare {
                field: FieldRef::NodeProperty {
                    alias: current_alias.clone(),
                    property: filter.property.clone(),
                },
                op: filter.op,
                value: Value::text(filter.value.clone()),
            });
        }

        // Build projections based on intent
        let projections = match self.intent {
            QueryIntent::Count => vec![Projection::Field(
                FieldRef::NodeId {
                    alias: current_alias.clone(),
                },
                Some("count".to_string()),
            )],
            _ => vec![Projection::from_field(FieldRef::NodeId {
                alias: current_alias.clone(),
            })],
        };

        // If no nodes were created, create a default based on primary entity
        if nodes.is_empty() {
            if let Some(ref entity_type) = self.primary_entity {
                let node_type = match entity_type {
                    EntityType::Host => Some(GraphNodeType::Host),
                    EntityType::Service => Some(GraphNodeType::Service),
                    EntityType::User => Some(GraphNodeType::User),
                    EntityType::Credential => Some(GraphNodeType::Credential),
                    EntityType::Vulnerability => Some(GraphNodeType::Vulnerability),
                    _ => None,
                };

                nodes.push(NodePattern {
                    alias: "n0".to_string(),
                    node_type,
                    properties: Vec::new(),
                });
            }
        }

        // Fold multiple filters into nested And
        let combined_filter = if filters.is_empty() {
            None
        } else {
            let mut iter = filters.into_iter();
            let first = iter.next().unwrap();
            Some(iter.fold(first, |acc, f| Filter::And(Box::new(acc), Box::new(f))))
        };

        QueryExpr::Graph(GraphQuery {
            alias: None,
            pattern: GraphPattern { nodes, edges },
            filter: combined_filter,
            return_: projections,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_find_hosts() {
        let q = NaturalParser::parse("find all hosts with ssh open").unwrap();
        assert_eq!(q.intent, QueryIntent::Find);
        assert!(q.entities.iter().any(|e| e.entity_type == EntityType::Host));
        assert!(q
            .filters
            .iter()
            .any(|f| f.property == "service" && f.value == "ssh"));
    }

    #[test]
    fn test_parse_show_credentials() {
        let q = NaturalParser::parse("show me credentials for user admin").unwrap();
        assert_eq!(q.intent, QueryIntent::Show);
        assert!(q
            .entities
            .iter()
            .any(|e| e.entity_type == EntityType::Credential));
        assert!(q.entities.iter().any(|e| e.entity_type == EntityType::User));
    }

    #[test]
    fn test_parse_with_ip() {
        let q = NaturalParser::parse("what vulnerabilities affect host 10.0.0.1").unwrap();
        assert!(q
            .entities
            .iter()
            .any(|e| e.entity_type == EntityType::Host && e.value == Some("10.0.0.1".to_string())));
        assert!(q
            .entities
            .iter()
            .any(|e| e.entity_type == EntityType::Vulnerability));
    }

    #[test]
    fn test_parse_count() {
        let q = NaturalParser::parse("how many hosts have port 22 open").unwrap();
        assert_eq!(q.intent, QueryIntent::Count);
    }

    #[test]
    fn test_parse_weak_passwords() {
        let q = NaturalParser::parse("list users with weak passwords").unwrap();
        assert!(q
            .filters
            .iter()
            .any(|f| f.property == "strength" && f.value == "weak"));
    }

    #[test]
    fn test_parse_critical_vulns() {
        let q = NaturalParser::parse("show critical vulnerabilities").unwrap();
        assert!(q
            .filters
            .iter()
            .any(|f| f.property == "severity" && f.value == "critical"));
    }

    #[test]
    fn test_parse_quoted() {
        let q = NaturalParser::parse("\"find hosts connected to 10.0.0.1\"").unwrap();
        assert_eq!(q.intent, QueryIntent::Find);
        assert!(q.relationship == Some(RelationshipType::ConnectsTo));
    }

    #[test]
    fn test_parse_with_limit() {
        let q = NaturalParser::parse("show top 10 vulnerable hosts").unwrap();
        assert_eq!(q.limit, Some(10));
    }

    #[test]
    fn test_to_query_expr() {
        let q = NaturalParser::parse("find all hosts with ssh").unwrap();
        let expr = q.to_query_expr();
        assert!(matches!(expr, QueryExpr::Graph(_)));
    }

    #[test]
    fn test_detect_relationship() {
        let q = NaturalParser::parse("credentials that can access host 10.0.0.1").unwrap();
        assert_eq!(q.relationship, Some(RelationshipType::AuthAccess));
    }
}
