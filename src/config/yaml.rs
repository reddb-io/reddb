// YAML config parser - ZERO external dependencies!
// Implements minimal YAML parser for .redblue.yaml

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

/// Parsed configuration from .redblue.yaml
#[derive(Debug, Clone, Default)] // Derive Default for easier initialization
pub struct YamlConfig {
    // --- Global/Core Settings ---
    pub verbose: Option<bool>,
    pub no_color: Option<bool>,
    pub output_format: Option<String>,
    pub output_file: Option<String>,
    pub preset: Option<String>,
    pub threads: Option<usize>,
    pub rate_limit: Option<u32>,
    pub auto_persist: Option<bool>,

    // --- Network Configuration ---
    pub network_timeout_ms: Option<u64>,
    pub network_max_retries: Option<usize>,
    pub network_request_delay_ms: Option<u64>,
    pub network_dns_resolver: Option<String>,
    pub network_dns_timeout_ms: Option<u64>,

    // --- Web Configuration ---
    pub web_user_agent: Option<String>,
    pub web_follow_redirects: Option<bool>,
    pub web_max_redirects: Option<usize>,
    pub web_verify_ssl: Option<bool>,
    pub web_headers: HashMap<String, String>, // Already a HashMap
    pub web_timeout_secs: Option<u64>,

    // --- Reconnaissance Configuration ---
    pub recon_subdomain_wordlist: Option<String>,
    pub recon_passive_only: Option<bool>,
    pub recon_dns_timeout_ms: Option<u64>,

    // --- Database Configuration ---
    pub db_dir: Option<String>,
    pub db_auto_name: Option<bool>,
    pub db_auto_persist: Option<bool>,
    pub db_format_version: Option<u32>,

    // --- Wordlists ---
    pub wordlists: HashMap<String, String>,

    // --- Credentials (e.g., api_keys for external services) ---
    pub credentials: HashMap<String, HashMap<String, String>>,

    // --- Command-specific overrides ---
    pub commands: HashMap<String, HashMap<String, String>>,

    // --- Custom/Unknown fields ---
    pub custom: HashMap<String, String>,
}

static CACHE: OnceLock<YamlConfig> = OnceLock::new();

impl YamlConfig {
    /// Load from current directory once and cache the result.
    pub fn load_from_cwd_cached() -> &'static YamlConfig {
        CACHE.get_or_init(|| {
            if let Some(cfg) = YamlConfig::load_from_cwd() {
                cfg
            } else {
                YamlConfig::default() // Load default if no file found
            }
        })
    }

    /// Load config from file
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let content =
            fs::read_to_string(path).map_err(|e| format!("Failed to read config: {}", e))?;

        Self::parse(&content)
    }

    /// Try to load from current directory
    pub fn load_from_cwd() -> Option<Self> {
        // Try .redblue.yaml first
        if let Ok(config) = Self::load(".redblue.yaml") {
            return Some(config);
        }

        // Try .redblue.yml
        if let Ok(config) = Self::load(".redblue.yml") {
            return Some(config);
        }

        None
    }

    /// Parse YAML content (minimal parser)
    fn parse(content: &str) -> Result<Self, String> {
        let mut config = YamlConfig::default(); // Use default for easier base

        let mut current_section: Option<String> = None;
        let mut current_subsection: Option<String> = None; // For credentials/commands sub-sections
        let mut _current_map_key: Option<String> = None; // For headers or other maps

        for line in content.lines() {
            let trimmed = line.trim();

            // Skip comments and empty lines
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            // Detect indentation for nested sections
            let indent_level = line.len() - line.trim_start().len();

            // Check for section header (ends with :)
            if trimmed.ends_with(':') && !trimmed.contains(": ") {
                // Avoid key: value: like in headers: X-Custom: value
                let section_name = trimmed.trim_end_matches(':').to_string();

                if indent_level == 0 {
                    // Top-level section
                    current_section = Some(section_name.clone());
                    current_subsection = None; // Reset subsection on new top-level
                    _current_map_key = None; // Reset map key
                } else if indent_level == 2 && current_section.is_some() {
                    // Nested section (for commands or credentials sub-sections)
                    current_subsection = Some(section_name);
                    _current_map_key = None;
                } else if indent_level == 4 && current_subsection.is_some() {
                    // Deeply nested, usually key-value pairs inside a map
                    _current_map_key = Some(section_name);
                }
                continue;
            }

            // Handle array values (e.g., url_sources)
            if trimmed.starts_with('-') {
                // This is an item in a list, like `- item`
                let item = trimmed
                    .trim_start_matches('-')
                    .trim()
                    .trim_matches('"')
                    .to_string();
                if current_section.as_deref() == Some("recon")
                    && current_subsection.as_deref() == Some("url_sources")
                {
                    // How to add to a Vec in Config? This needs a Vec in Config.
                    // For now, this parser doesn't collect lists, but a proper YamlConfig would have Vec<String> fields.
                    // We'll add this to custom for now as a workaround for simple config.
                    config.custom.insert(
                        format!(
                            "{}.{}.{}",
                            current_section.as_deref().unwrap_or(""),
                            current_subsection.as_deref().unwrap_or(""),
                            item
                        ),
                        "true".to_string(),
                    );
                }
                continue;
            }

            // Parse key-value pairs
            if let Some((key, value)) = Self::parse_key_value(trimmed) {
                match (current_section.as_deref(), current_subsection.as_deref()) {
                    // Global settings
                    (None, None) => match key {
                        "verbose" => config.verbose = Self::parse_bool(value),
                        "no_color" | "no-color" => config.no_color = Self::parse_bool(value),
                        "output_format" => config.output_format = Some(value.to_string()),
                        "output_file" => config.output_file = Some(value.to_string()),
                        "preset" => config.preset = Some(value.to_string()),
                        "threads" => config.threads = value.parse().ok(),
                        "rate_limit" => config.rate_limit = value.parse().ok(),
                        "auto_persist" | "persist" => config.auto_persist = Self::parse_bool(value),
                        _ => {
                            config.custom.insert(key.to_string(), value.to_string());
                        }
                    },
                    // Specific sections
                    (Some("network"), None) => match key {
                        "timeout_ms" => config.network_timeout_ms = value.parse().ok(),
                        "max_retries" => config.network_max_retries = value.parse().ok(),
                        "request_delay_ms" => config.network_request_delay_ms = value.parse().ok(),
                        "dns_resolver" => config.network_dns_resolver = Some(value.to_string()),
                        "dns_timeout_ms" => config.network_dns_timeout_ms = value.parse().ok(),
                        _ => {
                            config
                                .custom
                                .insert(format!("network.{}", key), value.to_string());
                        }
                    },
                    (Some("web"), Some("headers")) => {
                        // Specific handling for web.headers map
                        config
                            .web_headers
                            .insert(key.to_string(), value.to_string());
                    }
                    (Some("web"), None) => match key {
                        "user_agent" => config.web_user_agent = Some(value.to_string()),
                        "follow_redirects" => config.web_follow_redirects = Self::parse_bool(value),
                        "max_redirects" => config.web_max_redirects = value.parse().ok(),
                        "verify_ssl" => config.web_verify_ssl = Self::parse_bool(value),
                        "timeout_secs" => config.web_timeout_secs = value.parse().ok(),
                        _ => {
                            config
                                .custom
                                .insert(format!("web.{}", key), value.to_string());
                        }
                    },
                    (Some("recon"), None) => match key {
                        "subdomain_wordlist" => {
                            config.recon_subdomain_wordlist = Some(value.to_string())
                        }
                        "passive_only" => config.recon_passive_only = Self::parse_bool(value),
                        "dns_timeout_ms" => config.recon_dns_timeout_ms = value.parse().ok(),
                        _ => {
                            config
                                .custom
                                .insert(format!("recon.{}", key), value.to_string());
                        }
                    },
                    (Some("database"), None) => match key {
                        "auto_name" => config.db_auto_name = Self::parse_bool(value),
                        "auto_persist" => config.db_auto_persist = Self::parse_bool(value),
                        "db_dir" => config.db_dir = Some(value.to_string()),
                        "format_version" => config.db_format_version = value.parse().ok(),
                        _ => {
                            config
                                .custom
                                .insert(format!("database.{}", key), value.to_string());
                        }
                    },
                    (Some("wordlists"), None) => {
                        config.wordlists.insert(key.to_string(), value.to_string());
                    }
                    (Some("credentials"), Some(service_name)) => {
                        config
                            .credentials
                            .entry(service_name.to_string())
                            .or_insert_with(HashMap::new)
                            .insert(key.to_string(), value.to_string());
                    }
                    (Some("commands"), Some(cmd)) => {
                        config
                            .commands
                            .entry(cmd.to_string())
                            .or_insert_with(HashMap::new)
                            .insert(key.to_string(), value.to_string());
                    }
                    // Catch-all for unknown sections or malformed entries
                    _ => {
                        config.custom.insert(key.to_string(), value.to_string());
                    }
                }
            }
        }

        Ok(config)
    }

    /// Parse "key: value" line
    fn parse_key_value(line: &str) -> Option<(&str, &str)> {
        let mut parts = line.splitn(2, ':');
        let key = parts.next()?.trim();
        let value = parts.next()?.trim();

        // Remove quotes if present
        let value = value.trim_matches(|c| c == '"' || c == '\'');

        Some((key, value))
    }

    fn parse_bool(value: &str) -> Option<bool> {
        match value.to_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        }
    }

    /// Get command-specific flag value
    /// Tries: domain.resource.verb -> domain.resource -> domain
    pub fn get_command_flag(
        &self,
        domain: &str,
        resource: &str,
        verb: &str,
        flag: &str,
    ) -> Option<String> {
        // Try full path: network.nc.listen
        let full_path = format!("{}.{}.{}", domain, resource, verb);
        if let Some(flags) = self.commands.get(&full_path) {
            if let Some(value) = flags.get(flag) {
                return Some(value.clone());
            }
        }

        // Try resource level: network.nc
        let resource_path = format!("{}.{}", domain, resource);
        if let Some(flags) = self.commands.get(&resource_path) {
            if let Some(value) = flags.get(flag) {
                return Some(value.clone());
            }
        }

        // Try domain level: network
        if let Some(flags) = self.commands.get(domain) {
            if let Some(value) = flags.get(flag) {
                return Some(value.clone());
            }
        }

        None
    }

    /// Collect all command-level flags (domain/resource/verb) with specificity overrides.
    pub fn command_flags(
        &self,
        domain: &str,
        resource: &str,
        verb: &str,
    ) -> HashMap<String, String> {
        let mut merged = HashMap::new();

        if domain.is_empty() {
            return merged;
        }

        if let Some(flags) = self.commands.get(domain) {
            merged.extend(flags.clone());
        }

        if !resource.is_empty() {
            let resource_path = format!("{}.{}", domain, resource);
            if let Some(flags) = self.commands.get(&resource_path) {
                merged.extend(flags.clone());
            }
        }

        if !resource.is_empty() && !verb.is_empty() {
            let full_path = format!("{}.{}.{}", domain, resource, verb);
            if let Some(flags) = self.commands.get(&full_path) {
                merged.extend(flags.clone());
            }
        }

        merged
    }

    /// Check if command flag is set to true
    pub fn has_command_flag(&self, domain: &str, resource: &str, verb: &str, flag: &str) -> bool {
        if let Some(value) = self.get_command_flag(domain, resource, verb, flag) {
            Self::parse_bool(&value).unwrap_or(false)
        } else {
            false
        }
    }

    /// Get a credential value for a given service and key
    pub fn get_credential(&self, service: &str, key: &str) -> Option<String> {
        self.credentials
            .get(service)
            .and_then(|service_creds| service_creds.get(key).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple() {
        let yaml = r###"#
# RedBlue config
verbose: true
output_format: json
threads: 20
rate_limit: 10
#"###;

        let config = YamlConfig::parse(yaml).unwrap();
        assert_eq!(config.verbose, Some(true));
        assert_eq!(config.output_format, Some("json".to_string()));
        assert_eq!(config.threads, Some(20));
        assert_eq!(config.rate_limit, Some(10));
    }

    #[test]
    fn test_parse_network_config() {
        let yaml = r###"#
network:
  timeout_ms: 10000
  dns_resolver: "1.1.1.1"
#"###;
        let config = YamlConfig::parse(yaml).unwrap();
        assert_eq!(config.network_timeout_ms, Some(10000));
        assert_eq!(config.network_dns_resolver, Some("1.1.1.1".to_string()));
    }

    #[test]
    fn test_parse_web_config() {
        let yaml = r###"#
web:
  user_agent: "MyCustomUA"
  follow_redirects: false
  headers:
    X-API-Key: "abc"
    Accept: "application/json"
#"###;
        let config = YamlConfig::parse(yaml).unwrap();
        assert_eq!(config.web_user_agent, Some("MyCustomUA".to_string()));
        assert_eq!(config.web_follow_redirects, Some(false));
        assert_eq!(
            config.web_headers.get("X-API-Key"),
            Some(&"abc".to_string())
        );
    }

    #[test]
    fn test_parse_wordlists() {
        let yaml = r###"#
wordlists:
  subdomains: /usr/share/wordlists/subdomains.txt
  directories: /usr/share/wordlists/dirs.txt
#"###;

        let config = YamlConfig::parse(yaml).unwrap();
        assert_eq!(config.wordlists.len(), 2);
        assert!(config.wordlists.contains_key("subdomains"));
    }

    #[test]
    fn test_parse_credentials() {
        let yaml = r###"#
credentials:
  hibp:
    api_key: "my_hibp_key"
  shodan:
    api_key: "my_shodan_key"
    username: "user"
#"###;
        let config = YamlConfig::parse(yaml).unwrap();
        assert!(config.credentials.contains_key("hibp"));
        assert_eq!(
            config.credentials.get("hibp").unwrap().get("api_key"),
            Some(&"my_hibp_key".to_string())
        );
        assert_eq!(
            config.get_credential("shodan", "api_key"),
            Some("my_shodan_key".to_string())
        );
    }

    #[test]
    fn test_parse_bool_values() {
        let yaml = r###"#
verbose: yes
no_color: 0
auto_persist: "true"
#"###;
        let config = YamlConfig::parse(yaml).unwrap();
        assert_eq!(config.verbose, Some(true));
        assert_eq!(config.no_color, Some(false));
        assert_eq!(config.auto_persist, Some(true));
    }

    #[test]
    fn test_parse_command_specific_flags() {
        let yaml = r###"#
commands:
  recon.domain.subdomains:
    threads: 50
    passive_only: true
  web.fuzz:
    rate_limit: 10
#"###;
        let config = YamlConfig::parse(yaml).unwrap();
        assert_eq!(
            config.get_command_flag("recon", "domain", "subdomains", "threads"),
            Some("50".to_string())
        );
        assert_eq!(
            config.has_command_flag("recon", "domain", "subdomains", "passive_only"),
            true
        );
        assert_eq!(
            config.get_command_flag("web", "fuzz", "run", "rate_limit"),
            Some("10".to_string())
        );
    }
}
