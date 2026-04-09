/// RedDB CLI argument parser.
///
/// Schema-driven CLI with tokenizer, router, help generation, and shell
/// completion. Self-contained -- no external dependencies on config or
/// storage layers.
pub mod commands;
pub mod complete;
pub mod error;
pub mod help;
pub mod router;
pub mod schema;
pub mod token;
pub mod types;

use std::collections::HashMap;

/// Output format for CLI results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// Human-readable colorized output (default)
    #[default]
    Human,
    /// JSON output for automation/scripting
    Json,
    /// YAML output for configuration
    Yaml,
}

impl OutputFormat {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "human" | "h" | "text" => Some(OutputFormat::Human),
            "json" | "j" => Some(OutputFormat::Json),
            "yaml" | "yml" | "y" => Some(OutputFormat::Yaml),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            OutputFormat::Human => "human",
            OutputFormat::Json => "json",
            OutputFormat::Yaml => "yaml",
        }
    }
}

/// CLI execution context after parsing.
///
/// Holds the parsed command components and provides ergonomic helpers
/// for flag lookup, output format detection, etc.
#[derive(Debug, Clone, Default)]
pub struct CliContext {
    /// Full argument vector after `red`
    pub raw: Vec<String>,
    /// Primary command (e.g. "server", "query", "health")
    pub domain: Option<String>,
    /// Resource within the domain (e.g. collection name)
    pub resource: Option<String>,
    /// Verb or action to perform
    pub verb: Option<String>,
    /// Optional target (id, query string, etc.)
    pub target: Option<String>,
    /// Additional positional arguments beyond the target
    pub args: Vec<String>,
    /// Parsed flags (`--flag=value`, `-f value`, etc.)
    pub flags: HashMap<String, String>,
}

impl CliContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a flag value by name.
    pub fn get_flag(&self, key: &str) -> Option<String> {
        self.flags.get(key).cloned()
    }

    /// Check if a flag is set.
    pub fn has_flag(&self, key: &str) -> bool {
        self.flags.contains_key(key)
    }

    /// Get a flag value or return a default.
    pub fn get_flag_or(&self, key: &str, default: &str) -> String {
        self.get_flag(key).unwrap_or_else(|| default.to_string())
    }

    /// Get the domain only (first positional).
    pub fn domain_only(&self) -> Option<&str> {
        self.domain.as_deref()
    }

    /// Check if JSON output was explicitly requested.
    pub fn wants_json(&self) -> bool {
        self.has_flag("json") || self.has_flag("j")
    }

    /// Check if machine-readable output was requested (JSON or YAML).
    pub fn wants_machine_output(&self) -> bool {
        self.get_output_format() != OutputFormat::Human
    }

    /// Get the output format from flags or default to human.
    pub fn get_output_format(&self) -> OutputFormat {
        if self.wants_json() {
            return OutputFormat::Json;
        }

        // Check both --output/-o and --format
        let format_str = self
            .get_flag("output")
            .or_else(|| self.get_flag("o"))
            .or_else(|| self.get_flag("format"));

        if let Some(format_str) = format_str {
            OutputFormat::from_str(&format_str).unwrap_or_default()
        } else {
            OutputFormat::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_context_default() {
        let ctx = CliContext::default();
        assert!(ctx.raw.is_empty());
        assert!(ctx.domain.is_none());
        assert!(ctx.resource.is_none());
        assert!(ctx.verb.is_none());
        assert!(ctx.target.is_none());
        assert!(ctx.args.is_empty());
        assert!(ctx.flags.is_empty());
    }

    #[test]
    fn test_cli_context_new() {
        let ctx = CliContext::new();
        assert!(ctx.raw.is_empty());
        assert!(ctx.domain.is_none());
    }

    #[test]
    fn test_get_flag_from_cli() {
        let mut ctx = CliContext::new();
        ctx.flags.insert("path".to_string(), "/data".to_string());
        ctx.flags
            .insert("bind".to_string(), "0.0.0.0:6380".to_string());

        assert_eq!(ctx.get_flag("path"), Some("/data".to_string()));
        assert_eq!(ctx.get_flag("bind"), Some("0.0.0.0:6380".to_string()));
        assert_eq!(ctx.get_flag("nonexistent"), None);
    }

    #[test]
    fn test_has_flag() {
        let mut ctx = CliContext::new();
        ctx.flags.insert("verbose".to_string(), "true".to_string());
        ctx.flags.insert("quiet".to_string(), "".to_string());

        assert!(ctx.has_flag("verbose"));
        assert!(ctx.has_flag("quiet"));
        assert!(!ctx.has_flag("nonexistent"));
    }

    #[test]
    fn test_get_flag_or() {
        let mut ctx = CliContext::new();
        ctx.flags.insert("path".to_string(), "/data".to_string());

        assert_eq!(ctx.get_flag_or("path", "/default"), "/data");
        assert_eq!(ctx.get_flag_or("bind", "0.0.0.0:6380"), "0.0.0.0:6380");
    }

    #[test]
    fn test_domain_only() {
        let mut ctx = CliContext::new();
        assert_eq!(ctx.domain_only(), None);

        ctx.domain = Some("server".to_string());
        assert_eq!(ctx.domain_only(), Some("server"));
    }

    #[test]
    fn test_get_output_format_default() {
        let ctx = CliContext::new();
        let format = ctx.get_output_format();
        assert_eq!(format, OutputFormat::default());
    }

    #[test]
    fn test_get_output_format_from_output_flag() {
        let mut ctx = CliContext::new();
        ctx.flags.insert("output".to_string(), "json".to_string());
        let format = ctx.get_output_format();
        assert_eq!(format, OutputFormat::Json);
    }

    #[test]
    fn test_get_output_format_from_o_flag() {
        let mut ctx = CliContext::new();
        ctx.flags.insert("o".to_string(), "json".to_string());
        let format = ctx.get_output_format();
        assert_eq!(format, OutputFormat::Json);
    }

    #[test]
    fn test_get_output_format_from_format_flag() {
        let mut ctx = CliContext::new();
        ctx.flags.insert("format".to_string(), "json".to_string());
        let format = ctx.get_output_format();
        assert_eq!(format, OutputFormat::Json);
    }

    #[test]
    fn test_get_output_format_from_json_flag() {
        let mut ctx = CliContext::new();
        ctx.flags.insert("json".to_string(), "true".to_string());
        let format = ctx.get_output_format();
        assert_eq!(format, OutputFormat::Json);
    }

    #[test]
    fn test_wants_json_from_short_flag() {
        let mut ctx = CliContext::new();
        ctx.flags.insert("j".to_string(), "true".to_string());
        assert!(ctx.wants_json());
        assert_eq!(ctx.get_output_format(), OutputFormat::Json);
    }

    #[test]
    fn test_wants_machine_output_from_yaml_flag() {
        let mut ctx = CliContext::new();
        ctx.flags.insert("output".to_string(), "yaml".to_string());
        assert!(ctx.wants_machine_output());
    }

    #[test]
    fn test_get_output_format_priority() {
        let mut ctx = CliContext::new();
        // --output has priority over -o and --format
        ctx.flags.insert("output".to_string(), "json".to_string());
        ctx.flags.insert("o".to_string(), "text".to_string());
        ctx.flags.insert("format".to_string(), "csv".to_string());
        let format = ctx.get_output_format();
        assert_eq!(format, OutputFormat::Json);
    }

    #[test]
    fn test_cli_context_with_full_command() {
        let mut ctx = CliContext::new();
        ctx.raw = vec![
            "server".to_string(),
            "--path".to_string(),
            "/data".to_string(),
            "--bind".to_string(),
            "0.0.0.0:6380".to_string(),
        ];
        ctx.domain = Some("server".to_string());
        ctx.flags.insert("path".to_string(), "/data".to_string());
        ctx.flags
            .insert("bind".to_string(), "0.0.0.0:6380".to_string());

        assert_eq!(ctx.domain_only(), Some("server"));
        assert_eq!(ctx.get_flag("path"), Some("/data".to_string()));
        assert_eq!(ctx.get_flag_or("bind", "localhost:6380"), "0.0.0.0:6380");
        assert!(ctx.has_flag("path"));
        assert!(!ctx.has_flag("verbose"));
    }

    #[test]
    fn test_cli_context_with_args() {
        let mut ctx = CliContext::new();
        ctx.args = vec!["arg1".to_string(), "arg2".to_string(), "arg3".to_string()];

        assert_eq!(ctx.args.len(), 3);
        assert_eq!(ctx.args[0], "arg1");
        assert_eq!(ctx.args[1], "arg2");
        assert_eq!(ctx.args[2], "arg3");
    }

    #[test]
    fn test_cli_context_clone() {
        let mut ctx = CliContext::new();
        ctx.domain = Some("server".to_string());
        ctx.flags
            .insert("bind".to_string(), "0.0.0.0:6380".to_string());

        let ctx2 = ctx.clone();
        assert_eq!(ctx2.domain, ctx.domain);
        assert_eq!(ctx2.get_flag("bind"), ctx.get_flag("bind"));
    }

    #[test]
    fn test_cli_context_debug() {
        let ctx = CliContext::new();
        let debug_str = format!("{:?}", ctx);
        assert!(debug_str.contains("CliContext"));
        assert!(debug_str.contains("raw"));
        assert!(debug_str.contains("domain"));
    }

    #[test]
    fn test_output_format_from_str() {
        assert_eq!(OutputFormat::from_str("json"), Some(OutputFormat::Json));
        assert_eq!(OutputFormat::from_str("JSON"), Some(OutputFormat::Json));
        assert_eq!(OutputFormat::from_str("yaml"), Some(OutputFormat::Yaml));
        assert_eq!(OutputFormat::from_str("yml"), Some(OutputFormat::Yaml));
        assert_eq!(OutputFormat::from_str("human"), Some(OutputFormat::Human));
        assert_eq!(OutputFormat::from_str("text"), Some(OutputFormat::Human));
        assert_eq!(OutputFormat::from_str("xml"), None);
    }

    #[test]
    fn test_output_format_as_str() {
        assert_eq!(OutputFormat::Human.as_str(), "human");
        assert_eq!(OutputFormat::Json.as_str(), "json");
        assert_eq!(OutputFormat::Yaml.as_str(), "yaml");
    }
}
