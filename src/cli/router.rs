/// Command router: resolves domain/resource/verb from positional tokens.
///
/// Builds a RouteTree from registry data and resolves a token stream into
/// one of several outcomes: fully resolved command, global command,
/// partial match, or unknown with suggestions.
use std::collections::{HashMap, HashSet};

use super::error::suggest;
use super::token::Token;
use super::types::CommandPath;

/// Global commands that bypass domain/resource/verb routing.
const GLOBAL_COMMANDS: &[&str] = &["help", "version", "commands"];

/// Result of route resolution.
#[derive(Debug)]
pub enum RouteResolution {
    /// Fully resolved: domain + resource + verb.
    Resolved {
        path: CommandPath,
        remaining_tokens: Vec<Token>,
    },
    /// Global command: help, version, commands.
    GlobalCommand {
        name: String,
        remaining_tokens: Vec<Token>,
    },
    /// Partial: only domain found (no resource given).
    PartialDomain {
        domain: String,
        remaining_tokens: Vec<Token>,
    },
    /// Partial: domain + resource found but no verb.
    PartialResource {
        domain: String,
        resource: String,
        remaining_tokens: Vec<Token>,
    },
    /// Nothing matched.
    Unknown {
        tokens: Vec<String>,
        suggestions: Vec<String>,
    },
}

// ------------------------------------------------------------------
// Internal tree structures
// ------------------------------------------------------------------

struct ResourceEntry {
    verbs: HashSet<String>,
    verb_aliases: HashMap<String, String>,
}

struct DomainEntry {
    resources: HashMap<String, ResourceEntry>,
    aliases: HashMap<String, String>,
}

/// Domain/resource/verb tree built from Command trait implementations.
pub struct RouteTree {
    domains: HashMap<String, DomainEntry>,
    aliases: HashMap<String, String>,
}

impl RouteTree {
    /// Build from command registry data.
    ///
    /// * `domains`   - `(domain_name, domain_aliases)`
    /// * `resources` - `(domain, resource_name, resource_aliases)`
    /// * `verbs`     - `(domain, resource, verb_name, verb_aliases)`
    pub fn build(
        domains: &[(String, Vec<String>)],
        resources: &[(String, String, Vec<String>)],
        verbs: &[(String, String, String, Vec<String>)],
    ) -> Self {
        let mut tree_domains: HashMap<String, DomainEntry> = HashMap::new();
        let mut tree_aliases: HashMap<String, String> = HashMap::new();

        // Register domains and their aliases.
        for (name, aliases) in domains {
            for alias in aliases {
                tree_aliases.insert(alias.clone(), name.clone());
            }
            tree_domains
                .entry(name.clone())
                .or_insert_with(|| DomainEntry {
                    resources: HashMap::new(),
                    aliases: HashMap::new(),
                });
        }

        // Register resources and their aliases within their domain.
        for (domain, resource, aliases) in resources {
            let domain_entry = tree_domains
                .entry(domain.clone())
                .or_insert_with(|| DomainEntry {
                    resources: HashMap::new(),
                    aliases: HashMap::new(),
                });
            for alias in aliases {
                domain_entry.aliases.insert(alias.clone(), resource.clone());
            }
            domain_entry
                .resources
                .entry(resource.clone())
                .or_insert_with(|| ResourceEntry {
                    verbs: HashSet::new(),
                    verb_aliases: HashMap::new(),
                });
        }

        // Register verbs and their aliases within their domain/resource.
        for (domain, resource, verb, aliases) in verbs {
            let domain_entry = tree_domains
                .entry(domain.clone())
                .or_insert_with(|| DomainEntry {
                    resources: HashMap::new(),
                    aliases: HashMap::new(),
                });
            let resource_entry = domain_entry
                .resources
                .entry(resource.clone())
                .or_insert_with(|| ResourceEntry {
                    verbs: HashSet::new(),
                    verb_aliases: HashMap::new(),
                });
            resource_entry.verbs.insert(verb.clone());
            for alias in aliases {
                resource_entry
                    .verb_aliases
                    .insert(alias.clone(), verb.clone());
            }
        }

        Self {
            domains: tree_domains,
            aliases: tree_aliases,
        }
    }

    /// Resolve tokens into a route.
    pub fn resolve(&self, tokens: &[Token]) -> RouteResolution {
        // 1. Collect leading positionals (stop at first flag token).
        let mut positionals: Vec<&str> = Vec::new();
        let mut first_non_pos_idx: Option<usize> = None;

        for (i, token) in tokens.iter().enumerate() {
            match token {
                Token::Positional(val) => positionals.push(val.as_str()),
                _ => {
                    first_non_pos_idx = Some(i);
                    break;
                }
            }
        }

        // Remaining = everything after positionals consumed for routing.
        let build_remaining = |consumed: usize| -> Vec<Token> {
            let start = if consumed < positionals.len() {
                // Unconsumed positionals + all remaining tokens.
                consumed
            } else {
                positionals.len()
            };
            let mut remaining = Vec::new();
            // Re-emit unconsumed positionals.
            for &p in &positionals[start..] {
                remaining.push(Token::Positional(p.to_string()));
            }
            // Append everything from the first non-positional onward.
            let tail_start = first_non_pos_idx.unwrap_or(tokens.len());
            remaining.extend_from_slice(&tokens[tail_start..]);
            remaining
        };

        // 2. If empty: nothing to route.
        if positionals.is_empty() {
            return RouteResolution::Unknown {
                tokens: vec![],
                suggestions: self.domain_names(),
            };
        }

        let first = positionals[0];

        // 3. Check global commands.
        if GLOBAL_COMMANDS.contains(&first) {
            return RouteResolution::GlobalCommand {
                name: first.to_string(),
                remaining_tokens: build_remaining(1),
            };
        }

        // 4. Resolve domain (canonical or alias).
        let domain = if self.domains.contains_key(first) {
            first.to_string()
        } else if let Some(canonical) = self.aliases.get(first) {
            canonical.clone()
        } else {
            // Unknown domain -- generate suggestions.
            let candidates = self.domain_names();
            let candidate_refs: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
            let suggestions = suggest(first, &candidate_refs, 3);
            return RouteResolution::Unknown {
                tokens: positionals.iter().map(|s| s.to_string()).collect(),
                suggestions,
            };
        };

        let domain_entry = self.domains.get(&domain).expect("domain just resolved");

        // 5. Only domain given.
        if positionals.len() == 1 {
            return RouteResolution::PartialDomain {
                domain,
                remaining_tokens: build_remaining(1),
            };
        }

        let second = positionals[1];

        // 6. Resolve resource (canonical or alias within domain).
        let resource = if domain_entry.resources.contains_key(second) {
            second.to_string()
        } else if let Some(canonical) = domain_entry.aliases.get(second) {
            canonical.clone()
        } else {
            // Before giving up, check compat translation (legacy: domain verb resource).
            if positionals.len() >= 3 {
                if let Some(resolution) = self.try_compat_swap(
                    &domain,
                    domain_entry,
                    positionals[1],
                    positionals[2],
                    &build_remaining(3),
                ) {
                    return resolution;
                }
            }
            // Resource not found.
            return RouteResolution::PartialDomain {
                domain,
                remaining_tokens: build_remaining(1),
            };
        };

        // 7. Only domain + resource given.
        if positionals.len() == 2 {
            return RouteResolution::PartialResource {
                domain,
                resource,
                remaining_tokens: build_remaining(2),
            };
        }

        let third = positionals[2];
        let resource_entry = domain_entry
            .resources
            .get(&resource)
            .expect("resource just resolved");

        // 8. Resolve verb (canonical or alias within resource).
        if resource_entry.verbs.contains(third) {
            return RouteResolution::Resolved {
                path: CommandPath {
                    domain,
                    resource: Some(resource),
                    verb: Some(third.to_string()),
                },
                remaining_tokens: build_remaining(3),
            };
        }

        if let Some(canonical_verb) = resource_entry.verb_aliases.get(third) {
            return RouteResolution::Resolved {
                path: CommandPath {
                    domain,
                    resource: Some(resource),
                    verb: Some(canonical_verb.clone()),
                },
                remaining_tokens: build_remaining(3),
            };
        }

        // 9. Compat translation: try swapping positional[1] and positional[2].
        if let Some(resolution) = self.try_compat_swap(
            &domain,
            domain_entry,
            positionals[1],
            positionals[2],
            &build_remaining(3),
        ) {
            return resolution;
        }

        // Verb not found but domain+resource valid.
        RouteResolution::PartialResource {
            domain,
            resource,
            remaining_tokens: build_remaining(2),
        }
    }

    /// Get all canonical domain names.
    pub fn domains(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.domains.keys().map(|s| s.as_str()).collect();
        names.sort();
        names
    }

    /// Get canonical resource names for a domain.
    pub fn resources(&self, domain: &str) -> Vec<&str> {
        self.domains
            .get(domain)
            .map(|entry| {
                let mut names: Vec<&str> = entry.resources.keys().map(|s| s.as_str()).collect();
                names.sort();
                names
            })
            .unwrap_or_default()
    }

    /// Get canonical verb names for a domain/resource pair.
    pub fn verbs(&self, domain: &str, resource: &str) -> Vec<&str> {
        self.domains
            .get(domain)
            .and_then(|d| d.resources.get(resource))
            .map(|r| {
                let mut names: Vec<&str> = r.verbs.iter().map(|s| s.as_str()).collect();
                names.sort();
                names
            })
            .unwrap_or_default()
    }

    // ------------------------------------------------------------------
    // Private helpers
    // ------------------------------------------------------------------

    fn domain_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.domains.keys().cloned().collect();
        names.sort();
        names
    }

    /// Try the legacy order swap: `domain verb resource` -> `domain resource verb`.
    /// Returns `Some(Resolved)` if the swap produces a valid route.
    fn try_compat_swap(
        &self,
        domain: &str,
        domain_entry: &DomainEntry,
        first_token: &str,
        second_token: &str,
        remaining: &[Token],
    ) -> Option<RouteResolution> {
        // Interpret first_token as verb, second_token as resource.
        let swapped_resource = if domain_entry.resources.contains_key(second_token) {
            second_token.to_string()
        } else {
            domain_entry.aliases.get(second_token)?.clone()
        };

        let resource_entry = domain_entry.resources.get(&swapped_resource)?;

        let swapped_verb = if resource_entry.verbs.contains(first_token) {
            first_token.to_string()
        } else {
            resource_entry.verb_aliases.get(first_token)?.clone()
        };

        Some(RouteResolution::Resolved {
            path: CommandPath {
                domain: domain.to_string(),
                resource: Some(swapped_resource),
                verb: Some(swapped_verb),
            },
            remaining_tokens: remaining.to_vec(),
        })
    }
}

// ==================================================================
// Tests
// ==================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::token::Token;

    /// Build a small tree for testing.
    fn test_tree() -> RouteTree {
        let domains = vec![
            ("data".into(), vec![]),
            ("server".into(), vec!["s".into(), "srv".into()]),
            ("index".into(), vec![]),
            ("graph".into(), vec!["g".into()]),
        ];
        let resources = vec![
            ("data".into(), "collection".into(), vec!["col".into()]),
            ("server".into(), "grpc".into(), vec!["rpc".into()]),
            ("server".into(), "http".into(), vec![]),
            ("index".into(), "vector".into(), vec![]),
            ("graph".into(), "node".into(), vec!["n".into()]),
        ];
        let verbs = vec![
            ("data".into(), "collection".into(), "list".into(), vec![]),
            ("data".into(), "collection".into(), "create".into(), vec![]),
            (
                "server".into(),
                "grpc".into(),
                "start".into(),
                vec!["s".into()],
            ),
            ("server".into(), "http".into(), "start".into(), vec![]),
            ("index".into(), "vector".into(), "build".into(), vec![]),
            (
                "graph".into(),
                "node".into(),
                "query".into(),
                vec!["q".into()],
            ),
            ("graph".into(), "node".into(), "traverse".into(), vec![]),
        ];
        RouteTree::build(&domains, &resources, &verbs)
    }

    fn pos(s: &str) -> Token {
        Token::Positional(s.to_string())
    }

    fn long_flag(name: &str) -> Token {
        Token::LongFlag {
            name: name.to_string(),
            value: None,
        }
    }

    // ----------------------------------------------------------------
    // 1. Full command resolution
    // ----------------------------------------------------------------
    #[test]
    fn test_resolve_full_command() {
        let tree = test_tree();
        let tokens = vec![pos("data"), pos("collection"), pos("list")];
        match tree.resolve(&tokens) {
            RouteResolution::Resolved {
                path,
                remaining_tokens,
            } => {
                assert_eq!(path.domain, "data");
                assert_eq!(path.resource.as_deref(), Some("collection"));
                assert_eq!(path.verb.as_deref(), Some("list"));
                assert!(remaining_tokens.is_empty());
            }
            other => panic!("expected Resolved, got {:?}", other),
        }
    }

    // ----------------------------------------------------------------
    // 2. Alias resolution
    // ----------------------------------------------------------------
    #[test]
    fn test_resolve_with_aliases() {
        let tree = test_tree();
        let tokens = vec![pos("s"), pos("grpc"), pos("s")];
        match tree.resolve(&tokens) {
            RouteResolution::Resolved {
                path,
                remaining_tokens,
            } => {
                assert_eq!(path.domain, "server");
                assert_eq!(path.resource.as_deref(), Some("grpc"));
                assert_eq!(path.verb.as_deref(), Some("start"));
                assert!(remaining_tokens.is_empty());
            }
            other => panic!("expected Resolved, got {:?}", other),
        }
    }

    // ----------------------------------------------------------------
    // 3. Global help
    // ----------------------------------------------------------------
    #[test]
    fn test_resolve_global_help() {
        let tree = test_tree();
        let tokens = vec![pos("help")];
        match tree.resolve(&tokens) {
            RouteResolution::GlobalCommand {
                name,
                remaining_tokens,
            } => {
                assert_eq!(name, "help");
                assert!(remaining_tokens.is_empty());
            }
            other => panic!("expected GlobalCommand, got {:?}", other),
        }
    }

    // ----------------------------------------------------------------
    // 4. Global version
    // ----------------------------------------------------------------
    #[test]
    fn test_resolve_global_version() {
        let tree = test_tree();
        let tokens = vec![pos("version")];
        match tree.resolve(&tokens) {
            RouteResolution::GlobalCommand {
                name,
                remaining_tokens,
            } => {
                assert_eq!(name, "version");
                assert!(remaining_tokens.is_empty());
            }
            other => panic!("expected GlobalCommand, got {:?}", other),
        }
    }

    // ----------------------------------------------------------------
    // 5. Partial domain
    // ----------------------------------------------------------------
    #[test]
    fn test_resolve_partial_domain() {
        let tree = test_tree();
        let tokens = vec![pos("data")];
        match tree.resolve(&tokens) {
            RouteResolution::PartialDomain {
                domain,
                remaining_tokens,
            } => {
                assert_eq!(domain, "data");
                assert!(remaining_tokens.is_empty());
            }
            other => panic!("expected PartialDomain, got {:?}", other),
        }
    }

    // ----------------------------------------------------------------
    // 6. Partial resource
    // ----------------------------------------------------------------
    #[test]
    fn test_resolve_partial_resource() {
        let tree = test_tree();
        let tokens = vec![pos("data"), pos("collection")];
        match tree.resolve(&tokens) {
            RouteResolution::PartialResource {
                domain,
                resource,
                remaining_tokens,
            } => {
                assert_eq!(domain, "data");
                assert_eq!(resource, "collection");
                assert!(remaining_tokens.is_empty());
            }
            other => panic!("expected PartialResource, got {:?}", other),
        }
    }

    // ----------------------------------------------------------------
    // 7. Unknown domain
    // ----------------------------------------------------------------
    #[test]
    fn test_resolve_unknown_domain() {
        let tree = test_tree();
        let tokens = vec![pos("daat"), pos("collection"), pos("list")];
        match tree.resolve(&tokens) {
            RouteResolution::Unknown {
                tokens: toks,
                suggestions,
            } => {
                assert_eq!(toks, vec!["daat", "collection", "list"]);
                assert!(suggestions.contains(&"data".to_string()));
            }
            other => panic!("expected Unknown, got {:?}", other),
        }
    }

    // ----------------------------------------------------------------
    // 8. Compat translation (legacy order: domain verb resource)
    // ----------------------------------------------------------------
    #[test]
    fn test_resolve_compat_translation() {
        let tree = test_tree();
        // Legacy: red data list collection -> canonical: red data collection list
        let tokens = vec![pos("data"), pos("list"), pos("collection")];
        match tree.resolve(&tokens) {
            RouteResolution::Resolved { path, .. } => {
                assert_eq!(path.domain, "data");
                assert_eq!(path.resource.as_deref(), Some("collection"));
                assert_eq!(path.verb.as_deref(), Some("list"));
            }
            other => panic!("expected Resolved via compat swap, got {:?}", other),
        }
    }

    // ----------------------------------------------------------------
    // 9. Remaining tokens pass-through
    // ----------------------------------------------------------------
    #[test]
    fn test_resolve_with_remaining_tokens() {
        let tree = test_tree();
        let tokens = vec![
            pos("server"),
            pos("grpc"),
            pos("start"),
            pos("--path"),
            long_flag("bind"),
            pos("0.0.0.0:6380"),
        ];
        match tree.resolve(&tokens) {
            RouteResolution::Resolved {
                path,
                remaining_tokens,
            } => {
                assert_eq!(path.domain, "server");
                assert_eq!(path.resource.as_deref(), Some("grpc"));
                assert_eq!(path.verb.as_deref(), Some("start"));
                // remaining: "--path" positional, --bind flag, "0.0.0.0:6380" positional
                assert_eq!(remaining_tokens.len(), 3);
                assert_eq!(remaining_tokens[0], pos("--path"));
            }
            other => panic!("expected Resolved, got {:?}", other),
        }
    }

    // ----------------------------------------------------------------
    // 10. domains() and resources() listing
    // ----------------------------------------------------------------
    #[test]
    fn test_domains_and_resources_listing() {
        let tree = test_tree();
        let domains = tree.domains();
        assert!(domains.contains(&"data"));
        assert!(domains.contains(&"server"));
        assert!(domains.contains(&"index"));
        assert!(domains.contains(&"graph"));

        let resources = tree.resources("server");
        assert!(resources.contains(&"grpc"));
        assert!(resources.contains(&"http"));

        let verbs = tree.verbs("data", "collection");
        assert!(verbs.contains(&"list"));
        assert!(verbs.contains(&"create"));

        // Non-existent domain returns empty.
        assert!(tree.resources("fake").is_empty());
        assert!(tree.verbs("fake", "foo").is_empty());
    }
}
