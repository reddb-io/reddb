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

use crate::ast::{
    CompareOp, EdgeDirection, EdgePattern, FieldRef, Filter, GraphPattern, GraphQuery, NodePattern,
    Projection, PropertyFilter as AstPropertyFilter, QueryExpr,
};
use reddb_types::types::Value;

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

            nodes.push(NodePattern {
                alias: entity.alias.clone(),
                node_label: node_type.clone(),
                properties,
            });
        }

        // Add edges based on relationships. Map the natural-language
        // relationship enum to the canonical edge label string used by
        // the legacy reserved range; users can introduce new relationship
        // types by extending this match.
        if let Some(ref relationship) = self.relationship {
            if nodes.len() >= 2 {
                let edge_label = Some(
                    match relationship {
                        RelationshipType::HasService => "has_service",
                        RelationshipType::HasPort => "has_endpoint",
                        RelationshipType::HasVuln => "affected_by",
                        RelationshipType::HasCredential => "auth_access",
                        RelationshipType::HasUser => "has_user",
                        RelationshipType::ConnectsTo => "connects_to",
                        RelationshipType::Affects => "affected_by",
                        RelationshipType::AuthAccess => "auth_access",
                        RelationshipType::Uses => "uses_tech",
                        RelationshipType::RunsOn => "contains",
                        RelationshipType::Exposes => "has_endpoint",
                    }
                    .to_string(),
                );

                edges.push(EdgePattern {
                    alias: None,
                    from: nodes[0].alias.clone(),
                    to: nodes[1].alias.clone(),
                    edge_label,
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
                let node_label = match entity_type {
                    EntityType::Host => Some("host".to_string()),
                    EntityType::Service => Some("service".to_string()),
                    EntityType::User => Some("user".to_string()),
                    EntityType::Credential => Some("credential".to_string()),
                    EntityType::Vulnerability => Some("vulnerability".to_string()),
                    _ => None,
                };

                nodes.push(NodePattern {
                    alias: "n0".to_string(),
                    node_label,
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
            limit: self.limit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(expr: QueryExpr) -> GraphQuery {
        match expr {
            QueryExpr::Graph(graph) => graph,
            other => panic!("expected graph query, got {other:?}"),
        }
    }

    fn entity(entity_type: EntityType, alias: &str) -> ExtractedEntity {
        ExtractedEntity {
            entity_type,
            value: None,
            alias: alias.to_string(),
        }
    }

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

    #[test]
    fn test_parse_rejects_empty_after_normalization() {
        let err = NaturalParser::parse(" ?! ").unwrap_err();
        assert_eq!(err.message, "Empty query");
    }

    #[test]
    fn test_parse_intent_variants() {
        let cases = [
            ("search hosts", QueryIntent::Find),
            ("display users", QueryIntent::Show),
            ("count users", QueryIntent::Count),
            ("how are hosts", QueryIntent::Find),
            ("route between hosts", QueryIntent::Path),
            ("does user admin access host 10.0.0.1", QueryIntent::Check),
            ("which services are public", QueryIntent::Find),
            ("unexpected words", QueryIntent::Find),
        ];

        for (input, expected) in cases {
            let q = NaturalParser::parse(input).unwrap();
            assert_eq!(q.intent, expected, "{input}");
        }
    }

    #[test]
    fn test_parse_entities_from_values_and_identifiers() {
        let user = NaturalParser::parse("show user the admin").unwrap();
        assert!(user
            .entities
            .iter()
            .any(|e| e.entity_type == EntityType::User && e.value == Some("admin".to_string())));

        let host = NaturalParser::parse("find 192.168.1.10").unwrap();
        assert!(host.entities.iter().any(|e| {
            e.entity_type == EntityType::Host && e.value == Some("192.168.1.10".to_string())
        }));

        let cve = NaturalParser::parse("show cve:2024-1234").unwrap();
        assert!(cve.entities.iter().any(|e| {
            e.entity_type == EntityType::Vulnerability
                && e.value == Some("CVE-2024-1234".to_string())
        }));
    }

    #[test]
    fn test_parse_filter_variants() {
        let high = NaturalParser::parse("find high vulnerabilities").unwrap();
        assert!(high
            .filters
            .iter()
            .any(|f| f.property == "severity" && f.op == CompareOp::Ge && f.value == "7.0"));

        let medium = NaturalParser::parse("find medium vulnerabilities").unwrap();
        assert!(medium
            .filters
            .iter()
            .any(|f| f.property == "severity" && f.op == CompareOp::Ge && f.value == "4.0"));

        let public_rdp = NaturalParser::parse("find public rdp services").unwrap();
        assert!(public_rdp
            .filters
            .iter()
            .any(|f| f.property == "service" && f.value == "rdp"));
        assert!(public_rdp
            .filters
            .iter()
            .any(|f| f.property == "status" && f.value == "open"));

        let zero_port = NaturalParser::parse("find hosts with port 0").unwrap();
        assert!(!zero_port.filters.iter().any(|f| f.property == "port"));
    }

    #[test]
    fn test_parse_limit_variants() {
        let cases = [
            ("top 3 hosts", Some(3)),
            ("first 4 hosts", Some(4)),
            ("limit 5 hosts", Some(5)),
            ("show 6 hosts", Some(6)),
            ("show hosts", None),
            ("top hosts", None),
        ];

        for (input, expected) in cases {
            let q = NaturalParser::parse(input).unwrap();
            assert_eq!(q.limit, expected, "{input}");
        }
    }

    #[test]
    fn test_parse_explicit_relationship_phrases() {
        let cases = [
            (
                "find hosts running on technology linux",
                RelationshipType::RunsOn,
            ),
            (
                "find services using certificate tls",
                RelationshipType::Uses,
            ),
            (
                "show host 10.0.0.1 exposes port 443",
                RelationshipType::Exposes,
            ),
            (
                "find hosts affected by cve-2024-1234",
                RelationshipType::Affects,
            ),
            (
                "check users authenticate to host 10.0.0.1",
                RelationshipType::AuthAccess,
            ),
        ];

        for (input, expected) in cases {
            let q = NaturalParser::parse(input).unwrap();
            assert_eq!(q.relationship, Some(expected), "{input}");
        }
    }

    #[test]
    fn test_parse_inferred_relationships_from_entity_pairs() {
        let cases = [
            ("find hosts services", RelationshipType::HasService),
            ("find hosts port 443", RelationshipType::HasPort),
            ("find hosts vulnerabilities", RelationshipType::HasVuln),
            ("find users credentials", RelationshipType::HasCredential),
            (
                "find credentials for 10.0.0.1",
                RelationshipType::AuthAccess,
            ),
            ("find cves for 10.0.0.1", RelationshipType::Affects),
        ];

        for (input, expected) in cases {
            let q = NaturalParser::parse(input).unwrap();
            assert_eq!(q.relationship, Some(expected), "{input}");
        }

        let unrelated = NaturalParser::parse("find domains certificates").unwrap();
        assert_eq!(unrelated.relationship, None);
    }

    #[test]
    fn test_unknown_text_falls_back_to_find_without_entities() {
        let q = NaturalParser::parse("unmapped gibberish").unwrap();
        assert_eq!(q.intent, QueryIntent::Find);
        assert_eq!(q.primary_entity, None);
        assert_eq!(q.secondary_entity, None);
        assert!(q.entities.is_empty());
        assert!(q.filters.is_empty());
        assert_eq!(q.relationship, None);

        let graph = graph(q.to_query_expr());
        assert!(graph.pattern.nodes.is_empty());
        assert!(graph.pattern.edges.is_empty());
        assert_eq!(graph.limit, None);
    }

    #[test]
    fn test_to_query_expr_builds_count_projection_limit_and_nested_filters() {
        let q = NaturalParser::parse("count top 2 hosts with ssh open").unwrap();
        let graph = graph(q.to_query_expr());

        assert_eq!(graph.limit, Some(2));
        assert!(matches!(graph.filter, Some(Filter::And(_, _))));
        match graph.return_.as_slice() {
            [Projection::Field(FieldRef::NodeId { alias }, Some(name))] => {
                assert_eq!(alias, "e0");
                assert_eq!(name, "count");
            }
            other => panic!("unexpected projection: {other:?}"),
        }
    }

    #[test]
    fn test_to_query_expr_maps_all_relationship_edge_labels() {
        let cases = [
            (RelationshipType::HasService, "has_service"),
            (RelationshipType::HasPort, "has_endpoint"),
            (RelationshipType::HasVuln, "affected_by"),
            (RelationshipType::HasCredential, "auth_access"),
            (RelationshipType::HasUser, "has_user"),
            (RelationshipType::ConnectsTo, "connects_to"),
            (RelationshipType::Affects, "affected_by"),
            (RelationshipType::AuthAccess, "auth_access"),
            (RelationshipType::Uses, "uses_tech"),
            (RelationshipType::RunsOn, "contains"),
            (RelationshipType::Exposes, "has_endpoint"),
        ];

        for (relationship, expected_label) in cases {
            let debug_name = format!("{relationship:?}");
            let q = NaturalQuery {
                intent: QueryIntent::Find,
                primary_entity: Some(EntityType::Host),
                secondary_entity: Some(EntityType::Service),
                entities: vec![
                    entity(EntityType::Host, "source"),
                    entity(EntityType::Service, "target"),
                ],
                filters: Vec::new(),
                relationship: Some(relationship),
                limit: None,
            };
            let graph = graph(q.to_query_expr());

            assert_eq!(graph.pattern.edges.len(), 1, "{debug_name}");
            let edge = &graph.pattern.edges[0];
            assert_eq!(edge.from, "source", "{debug_name}");
            assert_eq!(edge.to, "target", "{debug_name}");
            assert_eq!(
                edge.edge_label.as_deref(),
                Some(expected_label),
                "{debug_name}"
            );
        }
    }

    #[test]
    fn test_to_query_expr_skips_edge_without_two_nodes() {
        let q = NaturalQuery {
            intent: QueryIntent::Find,
            primary_entity: Some(EntityType::Host),
            secondary_entity: None,
            entities: vec![entity(EntityType::Host, "source")],
            filters: Vec::new(),
            relationship: Some(RelationshipType::ConnectsTo),
            limit: None,
        };

        let graph = graph(q.to_query_expr());
        assert_eq!(graph.pattern.nodes.len(), 1);
        assert!(graph.pattern.edges.is_empty());
    }

    #[test]
    fn test_to_query_expr_maps_entity_labels_and_id_properties() {
        let cases = [
            (EntityType::Host, Some("host")),
            (EntityType::Service, Some("service")),
            (EntityType::Port, None),
            (EntityType::User, Some("user")),
            (EntityType::Credential, Some("credential")),
            (EntityType::Vulnerability, Some("vulnerability")),
            (EntityType::Technology, Some("technology")),
            (EntityType::Domain, Some("domain")),
            (EntityType::Certificate, Some("certificate")),
            (EntityType::Network, None),
        ];
        let entities: Vec<_> = cases
            .iter()
            .enumerate()
            .map(|(i, (entity_type, _))| ExtractedEntity {
                entity_type: entity_type.clone(),
                value: Some(format!("value{i}")),
                alias: format!("e{i}"),
            })
            .collect();
        let q = NaturalQuery {
            intent: QueryIntent::Find,
            primary_entity: Some(EntityType::Host),
            secondary_entity: None,
            entities,
            filters: Vec::new(),
            relationship: None,
            limit: None,
        };
        let graph = graph(q.to_query_expr());

        for (node, (_, expected_label)) in graph.pattern.nodes.iter().zip(cases.iter()) {
            assert_eq!(node.node_label.as_deref(), *expected_label);
            assert_eq!(node.properties.len(), 1);
            assert_eq!(node.properties[0].name, "id");
        }
    }

    #[test]
    fn test_to_query_expr_creates_default_node_from_primary_entity() {
        let cases = [
            (EntityType::Host, Some("host")),
            (EntityType::Service, Some("service")),
            (EntityType::User, Some("user")),
            (EntityType::Credential, Some("credential")),
            (EntityType::Vulnerability, Some("vulnerability")),
            (EntityType::Network, None),
        ];

        for (entity_type, expected_label) in cases {
            let q = NaturalQuery {
                intent: QueryIntent::Find,
                primary_entity: Some(entity_type),
                secondary_entity: None,
                entities: Vec::new(),
                filters: Vec::new(),
                relationship: None,
                limit: None,
            };
            let graph = graph(q.to_query_expr());

            assert_eq!(graph.pattern.nodes.len(), 1);
            assert_eq!(graph.pattern.nodes[0].alias, "n0");
            assert_eq!(graph.pattern.nodes[0].node_label.as_deref(), expected_label);
        }
    }
}
