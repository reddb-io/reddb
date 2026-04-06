// Scan presets: passive, stealth, aggressive
// Each preset defines behavior, rate limits, and which modules to run

use std::time::Duration;

/// Scan preset configuration
#[derive(Debug, Clone)]
pub struct ScanPreset {
    pub name: &'static str,
    pub description: &'static str,
    pub modules: Vec<Module>,
    pub rate_limit: RateLimit,
    pub parallelism: Parallelism,
    pub save_incremental: bool,
    pub output_format: OutputFormat,
}

/// Modules to execute
#[derive(Debug, Clone, PartialEq)]
pub enum Module {
    // Passive (no direct contact)
    DnsPassive,       // DNS records only
    WhoisLookup,      // WHOIS info
    CertTransparency, // CT logs
    SearchEngines,    // Google/Bing dorking
    ArchiveOrg,       // Wayback machine

    // Stealth (minimal contact, looks normal)
    TlsCert,        // TLS certificate check
    HttpHeaders,    // HTTP headers (1 request)
    DnsEnumeration, // Subdomain bruteforce (slow)
    PortScanCommon, // Only common ports (80, 443, 22, etc)

    // Aggressive (all-out scanning)
    PortScanFull, // All 65535 ports
    DirFuzzing,   // Directory fuzzing
    VulnScanning, // Nikto-style vuln scan
    WebCrawling,  // Full site crawl
}

/// Rate limiting strategy
#[derive(Debug, Clone)]
pub struct RateLimit {
    pub requests_per_second: u32,
    pub delay_between_requests: Duration,
    pub max_concurrent: usize,
    pub jitter: bool, // Add random delay to avoid patterns
}

/// Parallelism configuration
#[derive(Debug, Clone)]
pub struct Parallelism {
    pub threads: usize,
    pub batch_size: usize,
    pub queue_size: usize,
}

/// Output format
#[derive(Debug, Clone)]
pub enum OutputFormat {
    Text,
    Json,
    Both,
}

impl ScanPreset {
    /// Passive preset - 100% OSINT, zero direct contact
    pub fn passive() -> Self {
        Self {
            name: "passive",
            description: "100% passive reconnaissance using OSINT sources only",
            modules: vec![
                Module::DnsPassive,
                Module::WhoisLookup,
                Module::CertTransparency,
                Module::SearchEngines,
                Module::ArchiveOrg,
            ],
            rate_limit: RateLimit {
                requests_per_second: 5,
                delay_between_requests: Duration::from_millis(200),
                max_concurrent: 3,
                jitter: true,
            },
            parallelism: Parallelism {
                threads: 5,
                batch_size: 10,
                queue_size: 100,
            },
            save_incremental: true,
            output_format: OutputFormat::Json,
        }
    }

    /// Stealth preset - Minimal contact, looks like normal traffic
    pub fn stealth() -> Self {
        Self {
            name: "stealth",
            description: "Minimal contact, rate-limited, incremental scanning",
            modules: vec![
                // Start with passive
                Module::DnsPassive,
                Module::WhoisLookup,
                Module::CertTransparency,
                // Then minimal active
                Module::TlsCert,
                Module::HttpHeaders,
                Module::DnsEnumeration,
                Module::PortScanCommon,
            ],
            rate_limit: RateLimit {
                requests_per_second: 10,
                delay_between_requests: Duration::from_millis(100),
                max_concurrent: 5,
                jitter: true, // Random delays to avoid patterns
            },
            parallelism: Parallelism {
                threads: 10,
                batch_size: 20,
                queue_size: 200,
            },
            save_incremental: true, // Save progress continuously
            output_format: OutputFormat::Both,
        }
    }

    /// Aggressive preset - Maximum speed, full coverage
    pub fn aggressive() -> Self {
        Self {
            name: "aggressive",
            description: "Full-speed scanning with all modules enabled",
            modules: vec![
                // All passive modules
                Module::DnsPassive,
                Module::WhoisLookup,
                Module::CertTransparency,
                Module::SearchEngines,
                Module::ArchiveOrg,
                // All active modules
                Module::TlsCert,
                Module::HttpHeaders,
                Module::DnsEnumeration,
                Module::PortScanFull,
                Module::DirFuzzing,
                Module::VulnScanning,
                Module::WebCrawling,
            ],
            rate_limit: RateLimit {
                requests_per_second: 100,
                delay_between_requests: Duration::from_millis(10),
                max_concurrent: 50,
                jitter: false,
            },
            parallelism: Parallelism {
                threads: 100,
                batch_size: 100,
                queue_size: 1000,
            },
            save_incremental: true,
            output_format: OutputFormat::Text,
        }
    }

    /// Get preset by name
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "passive" => Some(Self::passive()),
            "stealth" => Some(Self::stealth()),
            "aggressive" | "aggresive" => Some(Self::aggressive()),
            _ => None,
        }
    }

    /// Check if module is enabled
    pub fn has_module(&self, module: &Module) -> bool {
        self.modules.contains(module)
    }
}

impl Default for ScanPreset {
    fn default() -> Self {
        Self::stealth() // Default to stealth mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_presets() {
        let passive = ScanPreset::passive();
        assert_eq!(passive.name, "passive");
        assert_eq!(passive.modules.len(), 5);
        assert!(passive.has_module(&Module::DnsPassive));
        assert!(!passive.has_module(&Module::PortScanFull));

        let stealth = ScanPreset::stealth();
        assert_eq!(stealth.name, "stealth");
        assert!(stealth.save_incremental);

        let aggressive = ScanPreset::aggressive();
        assert_eq!(aggressive.name, "aggressive");
        assert!(aggressive.modules.len() > stealth.modules.len());
    }

    #[test]
    fn test_from_name() {
        assert!(ScanPreset::from_name("passive").is_some());
        assert!(ScanPreset::from_name("stealth").is_some());
        assert!(ScanPreset::from_name("aggressive").is_some());
        assert!(ScanPreset::from_name("invalid").is_none());
    }
}
