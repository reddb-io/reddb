//! TLS certificate and scan record types

/// TLS certificate - compact
#[derive(Debug, Clone)]
pub struct TlsCertRecord {
    pub domain: String,
    pub issuer: String,
    pub subject: String,
    pub serial_number: String,
    pub signature_algorithm: String,
    pub public_key_algorithm: String,
    pub version: u8,
    pub not_before: u32,
    pub not_after: u32,
    pub sans: Vec<String>, // Subject Alternative Names
    pub self_signed: bool,
    pub timestamp: u32,
}

/// TLS scan result persisted from the auditor.
#[derive(Debug, Clone)]
pub struct TlsScanRecord {
    pub host: String,
    pub port: u16,
    pub timestamp: u32,
    pub negotiated_version: Option<String>,
    pub negotiated_cipher: Option<String>,
    pub negotiated_cipher_code: Option<u16>,
    pub negotiated_cipher_strength: TlsCipherStrength,
    pub certificate_valid: bool,
    pub versions: Vec<TlsVersionRecord>,
    pub ciphers: Vec<TlsCipherRecord>,
    pub vulnerabilities: Vec<TlsVulnerabilityRecord>,
    pub certificate_chain: Vec<TlsCertRecord>,
    pub ja3: Option<String>,
    pub ja3s: Option<String>,
    pub ja3_raw: Option<String>,
    pub ja3s_raw: Option<String>,
    pub peer_fingerprints: Vec<String>,
    pub certificate_chain_pem: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TlsVersionRecord {
    pub version: String,
    pub supported: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TlsCipherRecord {
    pub name: String,
    pub code: u16,
    pub strength: TlsCipherStrength,
}

#[derive(Debug, Clone, Copy)]
pub enum TlsCipherStrength {
    Weak = 0,
    Medium = 1,
    Strong = 2,
}

#[derive(Debug, Clone)]
pub struct TlsVulnerabilityRecord {
    pub name: String,
    pub severity: TlsSeverity,
    pub description: String,
}

#[derive(Debug, Clone, Copy)]
pub enum TlsSeverity {
    Low = 0,
    Medium = 1,
    High = 2,
    Critical = 3,
}
