//! Core record types and enums

use super::dns::DnsRecordData;
use super::http::HttpHeadersRecord;
use super::intel::HostIntelRecord;
use super::network::{PortScanRecord, SubdomainRecord};
use super::pentest::{ExploitAttemptRecord, PlaybookRunRecord, SessionRecord, VulnerabilityRecord};
use super::threat::{IocRecord, MitreAttackRecord};
use super::tls::TlsScanRecord;

/// Data types supported by RedDB
#[derive(Debug, Clone)]
pub enum RecordType {
    /// Port scan result: IP + port + status + timestamp
    PortScan(PortScanRecord),
    /// Subdomain: domain + IPs + source + timestamp
    Subdomain(SubdomainRecord),
    /// WHOIS: domain + registrar + dates + NS
    WhoisInfo(WhoisRecord),
    /// TLS scan result with full metadata
    TlsScan(TlsScanRecord),
    /// HTTP headers: URL + headers map
    HttpHeaders(HttpHeadersRecord),
    /// DNS record: domain + type + value
    DnsRecord(DnsRecordData),
    /// Generic key-value for flexibility
    KeyValue(Vec<u8>, Vec<u8>),
    /// Host fingerprint/intel data
    HostIntel(HostIntelRecord),
    /// Service fingerprint with CPE
    Fingerprint(super::intel::FingerprintRecord),
    /// Vulnerability with risk score
    Vulnerability(VulnerabilityRecord),
    /// Exploit execution attempt
    ExploitAttempt(ExploitAttemptRecord),
    /// Interactive session state
    Session(SessionRecord),
    /// Playbook execution history
    PlaybookRun(PlaybookRunRecord),
    /// MITRE ATT&CK Technique detection
    MitreAttack(MitreAttackRecord),
    /// Indicator of Compromise
    Ioc(IocRecord),
}

/// WHOIS record - compact
#[derive(Debug, Clone)]
pub struct WhoisRecord {
    pub domain: String,
    pub registrar: String,
    pub created_date: u32, // Unix timestamp
    pub expires_date: u32,
    pub nameservers: Vec<String>,
    pub timestamp: u32, // When we fetched this
}
