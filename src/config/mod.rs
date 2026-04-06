/// Configuration management for redblue
pub mod presets;
pub mod yaml;

pub use presets::{Module, OutputFormat, Parallelism, RateLimit, ScanPreset};
pub use yaml::YamlConfig;

use std::collections::HashMap;
use std::sync::{Once, OnceLock};

#[derive(Debug, Clone)]
pub struct RedBlueConfig {
    // Global settings
    pub verbose: bool,
    pub no_color: bool,
    pub output_format: String,
    pub output_file: Option<String>,
    pub preset: Option<String>,
    pub threads: usize,
    pub rate_limit: u32,
    pub auto_persist: bool,

    // Nested configurations
    pub network: NetworkConfig,
    pub web: WebConfig,
    pub recon: ReconConfig,
    pub database: DatabaseConfig,

    // Dynamic maps
    pub wordlists: HashMap<String, String>,
    pub credentials: HashMap<String, HashMap<String, String>>,
    pub commands: HashMap<String, HashMap<String, String>>,
}

#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub threads: usize, // Fallback if global threads not used? Usually global threads overrides.
    pub timeout_ms: u64,
    pub max_retries: usize,
    pub request_delay_ms: u64,
    pub dns_resolver: String,
    pub dns_timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct WebConfig {
    pub user_agent: String,
    pub follow_redirects: bool,
    pub max_redirects: usize,
    pub verify_ssl: bool,
    pub headers: HashMap<String, String>,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone)]
pub struct ReconConfig {
    pub subdomain_wordlist: Option<String>,
    pub passive_only: bool,
    pub dns_timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    pub db_dir: Option<String>,
    pub auto_name: bool,
    pub auto_persist: bool,
    pub format_version: u32,
}

static INIT: Once = Once::new();
static GLOBAL_CONFIG: OnceLock<RedBlueConfig> = OnceLock::new();

pub fn init() -> &'static RedBlueConfig {
    GLOBAL_CONFIG.get_or_init(RedBlueConfig::load)
}

/// Access the global configuration, loading defaults if necessary.
pub fn get() -> &'static RedBlueConfig {
    init()
}

impl Default for RedBlueConfig {
    fn default() -> Self {
        Self::from_yaml_config(YamlConfig::default())
    }
}

impl RedBlueConfig {
    fn from_yaml_config(cfg: YamlConfig) -> Self {
        Self {
            verbose: cfg.verbose.unwrap_or(false),
            no_color: cfg.no_color.unwrap_or(false),
            output_format: cfg.output_format.unwrap_or_else(|| "human".to_string()),
            output_file: cfg.output_file,
            preset: cfg.preset,
            threads: cfg.threads.unwrap_or(10),
            rate_limit: cfg.rate_limit.unwrap_or(0),
            auto_persist: cfg.auto_persist.unwrap_or(false),

            network: NetworkConfig {
                threads: cfg.threads.unwrap_or(10),
                timeout_ms: cfg.network_timeout_ms.unwrap_or(5000),
                max_retries: cfg.network_max_retries.unwrap_or(2),
                request_delay_ms: cfg.network_request_delay_ms.unwrap_or(0),
                dns_resolver: cfg
                    .network_dns_resolver
                    .unwrap_or_else(|| "8.8.8.8".to_string()),
                dns_timeout_ms: cfg.network_dns_timeout_ms.unwrap_or(3000),
            },

            web: WebConfig {
                user_agent: cfg
                    .web_user_agent
                    .unwrap_or_else(|| "RedBlue/1.0".to_string()),
                follow_redirects: cfg.web_follow_redirects.unwrap_or(true),
                max_redirects: cfg.web_max_redirects.unwrap_or(5),
                verify_ssl: cfg.web_verify_ssl.unwrap_or(true),
                headers: cfg.web_headers,
                timeout_secs: cfg.web_timeout_secs.unwrap_or(10),
            },

            recon: ReconConfig {
                subdomain_wordlist: cfg.recon_subdomain_wordlist,
                passive_only: cfg.recon_passive_only.unwrap_or(false),
                dns_timeout_ms: cfg.recon_dns_timeout_ms.unwrap_or(3000),
            },

            database: DatabaseConfig {
                db_dir: cfg.db_dir,
                auto_name: cfg.db_auto_name.unwrap_or(true),
                auto_persist: cfg.db_auto_persist.unwrap_or(true),
                format_version: cfg.db_format_version.unwrap_or(1),
            },

            wordlists: cfg.wordlists,
            credentials: cfg.credentials,
            commands: cfg.commands,
        }
    }

    pub fn load() -> Self {
        let yaml_config = YamlConfig::load_from_cwd_cached();
        Self::from_yaml_config(yaml_config.clone())
    }

    pub fn create_default_file() -> Result<(), String> {
        // ... (Same as before, implementation of default file creation)
        use std::fs;
        use std::path::Path;

        let path = ".redblue.yaml";

        if Path::new(path).exists() {
            return Err("Config file already exists.".to_string());
        }

        let content = r#"# RedBlue Configuration File
verbose: false
no_color: false
output_format: human
threads: 10
auto_persist: false

network:
  timeout_ms: 5000
  max_retries: 2
  dns_resolver: "8.8.8.8"
  dns_timeout_ms: 3000

web:
  user_agent: "RedBlue/1.0"
  follow_redirects: true
  verify_ssl: true
"#;
        fs::write(path, content).map_err(|e| e.to_string())
    }
}
