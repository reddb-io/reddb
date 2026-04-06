//! Shared types for the intelligence layer

use crate::modules::common::Severity;

/// OS family detection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OsFamily {
    Windows,
    Linux,
    MacOS,
    BSD,
    Solaris,
    AIX,
    Unknown,
}

impl OsFamily {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::Linux => "Linux",
            Self::MacOS => "macOS",
            Self::BSD => "BSD",
            Self::Solaris => "Solaris",
            Self::AIX => "AIX",
            Self::Unknown => "Unknown",
        }
    }
}

/// OS information with confidence score
#[derive(Debug, Clone)]
pub struct OsInfo {
    pub family: OsFamily,
    pub distribution: Option<String>,
    pub version: Option<String>,
    pub kernel: Option<String>,
    pub confidence: f32,
}

impl Default for OsInfo {
    fn default() -> Self {
        Self {
            family: OsFamily::Unknown,
            distribution: None,
            version: None,
            kernel: None,
            confidence: 0.0,
        }
    }
}

/// Technology category
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TechCategory {
    WebServer,
    Database,
    Cache,
    MessageQueue,
    Runtime,
    Framework,
    Os,
    Network,
    Security,
    Other,
}

impl TechCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WebServer => "Web Server",
            Self::Database => "Database",
            Self::Cache => "Cache",
            Self::MessageQueue => "Message Queue",
            Self::Runtime => "Runtime",
            Self::Framework => "Framework",
            Self::Os => "OS",
            Self::Network => "Network",
            Self::Security => "Security",
            Self::Other => "Other",
        }
    }

    /// Infer category from technology name
    pub fn from_name(name: &str) -> Self {
        let lower = name.to_lowercase();
        if lower.contains("nginx")
            || lower.contains("apache")
            || lower.contains("iis")
            || lower.contains("httpd")
            || lower.contains("lighttpd")
            || lower.contains("caddy")
        {
            Self::WebServer
        } else if lower.contains("mysql")
            || lower.contains("postgres")
            || lower.contains("mongo")
            || lower.contains("redis")
            || lower.contains("sqlite")
            || lower.contains("oracle")
            || lower.contains("mssql")
            || lower.contains("mariadb")
        {
            Self::Database
        } else if lower.contains("memcached") || lower.contains("varnish") {
            Self::Cache
        } else if lower.contains("rabbitmq")
            || lower.contains("kafka")
            || lower.contains("activemq")
        {
            Self::MessageQueue
        } else if lower.contains("java")
            || lower.contains("python")
            || lower.contains("node")
            || lower.contains("php")
            || lower.contains("ruby")
            || lower.contains("dotnet")
        {
            Self::Runtime
        } else if lower.contains("react")
            || lower.contains("angular")
            || lower.contains("vue")
            || lower.contains("django")
            || lower.contains("rails")
            || lower.contains("spring")
        {
            Self::Framework
        } else if lower.contains("ubuntu")
            || lower.contains("debian")
            || lower.contains("centos")
            || lower.contains("windows")
            || lower.contains("linux")
            || lower.contains("bsd")
        {
            Self::Os
        } else if lower.contains("ssh")
            || lower.contains("ftp")
            || lower.contains("smtp")
            || lower.contains("dns")
            || lower.contains("vpn")
        {
            Self::Network
        } else if lower.contains("firewall")
            || lower.contains("ids")
            || lower.contains("waf")
            || lower.contains("ssl")
            || lower.contains("tls")
        {
            Self::Security
        } else {
            Self::Other
        }
    }
}

/// Technology with version
#[derive(Debug, Clone)]
pub struct Technology {
    pub name: String,
    pub version: Option<String>,
    pub category: TechCategory,
    pub source: DetectionSource,
}

/// How a technology was detected
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionSource {
    Banner,
    Header,
    Fingerprint,
    Port,
    Manual,
}

impl DetectionSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Banner => "banner",
            Self::Header => "header",
            Self::Fingerprint => "fingerprint",
            Self::Port => "port",
            Self::Manual => "manual",
        }
    }
}

/// Privilege level for a user or credential
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PrivilegeLevel {
    Unknown = 0,
    Service = 1,
    User = 2,
    Admin = 3,
    Root = 4,
}

impl PrivilegeLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Admin => "admin",
            Self::User => "user",
            Self::Service => "service",
            Self::Unknown => "unknown",
        }
    }

    /// Infer privilege from username
    pub fn from_username(username: &str) -> Self {
        let lower = username.to_lowercase();
        if lower == "root" || lower == "administrator" || lower == "system" {
            Self::Root
        } else if lower.contains("admin") || lower == "sa" || lower == "dba" {
            Self::Admin
        } else if lower.ends_with("svc")
            || lower.contains("service")
            || lower.starts_with("www")
            || lower == "daemon"
            || lower == "nobody"
        {
            Self::Service
        } else {
            Self::User
        }
    }
}

/// Credential type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialType {
    Password,
    Hash,
    SshKey,
    Token,
    Certificate,
    ApiKey,
}

impl CredentialType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Hash => "hash",
            Self::SshKey => "ssh_key",
            Self::Token => "token",
            Self::Certificate => "certificate",
            Self::ApiKey => "api_key",
        }
    }
}

/// Password strength assessment
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PasswordStrength {
    Weak = 0,
    Medium = 1,
    Strong = 2,
    Unknown = 3,
}

impl PasswordStrength {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Weak => "weak",
            Self::Medium => "medium",
            Self::Strong => "strong",
            Self::Unknown => "unknown",
        }
    }

    /// Analyze password strength
    pub fn analyze(password: &str) -> Self {
        // Common/default passwords
        const COMMON: &[&str] = &[
            "admin",
            "password",
            "123456",
            "root",
            "admin123",
            "password123",
            "12345678",
            "qwerty",
            "letmein",
            "welcome",
            "monkey",
            "dragon",
            "master",
            "login",
            "abc123",
            "iloveyou",
            "trustno1",
            "sunshine",
            "passw0rd",
            "p@ssw0rd",
            "admin@123",
            "test",
            "guest",
            "default",
        ];

        let lower = password.to_lowercase();
        if COMMON.iter().any(|c| lower == *c) {
            return Self::Weak;
        }

        let len = password.len();
        let has_upper = password.chars().any(|c| c.is_uppercase());
        let has_lower = password.chars().any(|c| c.is_lowercase());
        let has_digit = password.chars().any(|c| c.is_ascii_digit());
        let has_special = password.chars().any(|c| !c.is_alphanumeric());

        let complexity = [has_upper, has_lower, has_digit, has_special]
            .iter()
            .filter(|&&x| x)
            .count();

        if len < 8 || complexity < 2 {
            Self::Weak
        } else if len >= 12 && complexity >= 3 {
            Self::Strong
        } else {
            Self::Medium
        }
    }
}

// Severity is imported from crate::modules::common

/// Version status for software
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionStatus {
    Current,
    Supported,
    Outdated,
    Old,
    Critical,
    Eol,
}

impl VersionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Supported => "supported",
            Self::Outdated => "outdated",
            Self::Old => "old",
            Self::Critical => "critical",
            Self::Eol => "eol",
        }
    }
}

/// Vulnerability type classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VulnType {
    Rce,
    SqlInjection,
    Xss,
    Lfi,
    Rfi,
    AuthBypass,
    PrivEsc,
    DoS,
    InfoDisclosure,
    Other,
}

impl VulnType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rce => "RCE",
            Self::SqlInjection => "SQL Injection",
            Self::Xss => "XSS",
            Self::Lfi => "LFI",
            Self::Rfi => "RFI",
            Self::AuthBypass => "Auth Bypass",
            Self::PrivEsc => "Privilege Escalation",
            Self::DoS => "DoS",
            Self::InfoDisclosure => "Info Disclosure",
            Self::Other => "Other",
        }
    }
}

/// Port information
#[derive(Debug, Clone)]
pub struct PortInfo {
    pub port: u16,
    pub protocol: String,
    pub service: String,
    pub version: Option<String>,
    pub banner: Option<String>,
}

/// Vulnerability information
#[derive(Debug, Clone)]
pub struct VulnInfo {
    pub cve: Option<String>,
    pub title: String,
    pub cvss: f32,
    pub severity: Severity,
    pub vuln_type: VulnType,
    pub exploitable: bool,
    pub description: Option<String>,
}
