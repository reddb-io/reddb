//! Certificate-Centric Intelligence
//!
//! Answers: "What certificates are in use? Expiring? Self-signed?"

use crate::storage::segments::graph::{EdgeType, GraphSegment};

/// Profile of a TLS certificate
#[derive(Debug, Clone)]
pub struct CertProfile {
    pub fingerprint: String,
    pub subject: CertSubject,
    pub issuer: String,
    pub not_before: Option<String>,
    pub not_after: Option<String>,
    pub days_until_expiry: Option<i64>,
    pub sans: Vec<String>,
    pub key_type: String,
    pub key_size: u32,
    pub signature_algorithm: String,
    pub is_self_signed: bool,
    pub is_expired: bool,
    pub is_expiring_soon: bool,
    pub hosts_using: Vec<String>,
    pub issues: Vec<CertIssue>,
}

impl CertProfile {
    pub fn display(&self) -> String {
        let mut s = String::new();
        s.push_str("┌─────────────────────────────────────────────────────────────────┐\n");

        let subj_str = self
            .subject
            .common_name
            .as_deref()
            .unwrap_or(&self.fingerprint);
        s.push_str(&format!(
            "│  CERTIFICATE: {:<48} │\n",
            if subj_str.len() > 48 {
                format!("{}...", &subj_str[..45])
            } else {
                subj_str.to_string()
            }
        ));
        s.push_str("├─────────────────────────────────────────────────────────────────┤\n");

        // Status indicators
        let status = if self.is_expired {
            "⛔ EXPIRED"
        } else if self.is_expiring_soon {
            "⚠️  EXPIRING SOON"
        } else if self.is_self_signed {
            "⚠️  SELF-SIGNED"
        } else {
            "✓ VALID"
        };
        s.push_str(&format!("│  STATUS: {:<53} │\n", status));

        s.push_str(&format!(
            "│  ISSUER: {:<53} │\n",
            if self.issuer.len() > 53 {
                format!("{}...", &self.issuer[..50])
            } else {
                self.issuer.clone()
            }
        ));

        if let Some(ref expiry) = self.not_after {
            let days_str = self
                .days_until_expiry
                .map(|d| format!(" ({} days)", d))
                .unwrap_or_default();
            s.push_str(&format!("│  EXPIRES: {}{:<42} │\n", expiry, days_str));
        }

        s.push_str(&format!(
            "│  KEY: {} {} bits                                     │\n",
            self.key_type, self.key_size
        ));
        s.push_str(&format!(
            "│  SIGNATURE: {:<50} │\n",
            self.signature_algorithm
        ));

        if !self.sans.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str(&format!(
                "│  SUBJECT ALTERNATIVE NAMES ({:<2}):                            │\n",
                self.sans.len()
            ));
            for san in self.sans.iter().take(5) {
                s.push_str(&format!(
                    "│    • {:<55} │\n",
                    if san.len() > 55 {
                        format!("{}...", &san[..52])
                    } else {
                        san.clone()
                    }
                ));
            }
            if self.sans.len() > 5 {
                s.push_str(&format!(
                    "│    ... and {} more                                          │\n",
                    self.sans.len() - 5
                ));
            }
        }

        if !self.hosts_using.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str(&format!(
                "│  HOSTS USING THIS CERT ({:<2}):                                │\n",
                self.hosts_using.len()
            ));
            for host in self.hosts_using.iter().take(4) {
                s.push_str(&format!("│    • {:<55} │\n", host));
            }
        }

        if !self.issues.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str("│  SECURITY ISSUES:                                               │\n");
            for issue in &self.issues {
                let severity = match issue.severity {
                    IssueSeverity::Critical => "CRIT",
                    IssueSeverity::High => "HIGH",
                    IssueSeverity::Medium => "MED",
                    IssueSeverity::Low => "LOW",
                    IssueSeverity::Info => "INFO",
                };
                let issue_str = format!("[{}] {}", severity, issue.title);
                s.push_str(&format!(
                    "│    • {:<55} │\n",
                    if issue_str.len() > 55 {
                        format!("{}...", &issue_str[..52])
                    } else {
                        issue_str
                    }
                ));
            }
        }

        s.push_str("└─────────────────────────────────────────────────────────────────┘\n");
        s
    }
}

/// Certificate subject information
#[derive(Debug, Clone, Default)]
pub struct CertSubject {
    pub common_name: Option<String>,
    pub organization: Option<String>,
    pub organizational_unit: Option<String>,
    pub country: Option<String>,
    pub state: Option<String>,
    pub locality: Option<String>,
}

/// Certificate security issue
#[derive(Debug, Clone)]
pub struct CertIssue {
    pub title: String,
    pub severity: IssueSeverity,
    pub description: String,
}

/// Issue severity level
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IssueSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

/// Certificate-centric intelligence queries
pub struct CertIntelligence<'a> {
    graph: &'a GraphSegment,
}

impl<'a> CertIntelligence<'a> {
    pub fn new(graph: &'a GraphSegment) -> Self {
        Self { graph }
    }

    /// Get certificate profile by fingerprint or subject
    pub fn profile(&self, identifier: &str) -> Option<CertProfile> {
        // Look for certificate nodes
        let cert_id = if identifier.starts_with("cert:") {
            identifier.to_string()
        } else {
            format!("cert:{}", identifier)
        };

        // Try exact match first
        if let Some(node) = self.graph.get_node(&cert_id) {
            return Some(self.build_profile(&cert_id, &node.label));
        }

        // Try to find by subject match
        for node in self.graph.all_nodes() {
            if node.id.starts_with("cert:")
                && (node
                    .label
                    .to_lowercase()
                    .contains(&identifier.to_lowercase())
                    || node.id.to_lowercase().contains(&identifier.to_lowercase()))
            {
                return Some(self.build_profile(&node.id, &node.label));
            }
        }

        None
    }

    /// Build a certificate profile from node data
    fn build_profile(&self, cert_id: &str, label: &str) -> CertProfile {
        let fingerprint = cert_id.trim_start_matches("cert:").to_string();
        let hosts = self.hosts_using_cert(cert_id);
        let subject = parse_subject(label);
        let issuer = parse_issuer(label);
        let (not_before, not_after, days_until_expiry) = parse_validity(label);
        let is_self_signed = is_self_signed(label, &subject, &issuer);
        let is_expired = days_until_expiry.is_some_and(|d| d < 0);
        let is_expiring_soon = days_until_expiry.is_some_and(|d| (0..=30).contains(&d));
        let sans = parse_sans(label);
        let (key_type, key_size) = parse_key_info(label);
        let signature_algorithm = parse_signature_algorithm(label);
        let issues = self.analyze_issues(
            is_self_signed,
            is_expired,
            is_expiring_soon,
            &key_type,
            key_size,
            &signature_algorithm,
        );

        CertProfile {
            fingerprint,
            subject,
            issuer,
            not_before,
            not_after,
            days_until_expiry,
            sans,
            key_type,
            key_size,
            signature_algorithm,
            is_self_signed,
            is_expired,
            is_expiring_soon,
            hosts_using: hosts,
            issues,
        }
    }

    /// Find hosts using a specific certificate
    fn hosts_using_cert(&self, cert_id: &str) -> Vec<String> {
        let mut hosts = Vec::new();

        if let Some(node) = self.graph.get_node(cert_id) {
            for edge in &node.in_edges {
                if edge.target_id.starts_with("host:") {
                    hosts.push(edge.target_id.trim_start_matches("host:").to_string());
                }
                // Also check services
                if edge.target_id.starts_with("service:") {
                    // Extract host from service ID (service:ip:port:name)
                    let parts: Vec<&str> = edge.target_id.split(':').collect();
                    if parts.len() >= 2 {
                        hosts.push(parts[1].to_string());
                    }
                }
            }
        }

        hosts.sort();
        hosts.dedup();
        hosts
    }

    /// Analyze certificate for security issues
    fn analyze_issues(
        &self,
        is_self_signed: bool,
        is_expired: bool,
        is_expiring_soon: bool,
        key_type: &str,
        key_size: u32,
        sig_alg: &str,
    ) -> Vec<CertIssue> {
        let mut issues = Vec::new();

        if is_expired {
            issues.push(CertIssue {
                title: "Certificate has expired".to_string(),
                severity: IssueSeverity::Critical,
                description:
                    "The certificate is no longer valid and browsers will show security warnings"
                        .to_string(),
            });
        }

        if is_expiring_soon {
            issues.push(CertIssue {
                title: "Certificate expiring within 30 days".to_string(),
                severity: IssueSeverity::High,
                description: "Certificate should be renewed soon to avoid service disruption"
                    .to_string(),
            });
        }

        if is_self_signed {
            issues.push(CertIssue {
                title: "Self-signed certificate".to_string(),
                severity: IssueSeverity::Medium,
                description: "Self-signed certificates are not trusted by browsers and clients"
                    .to_string(),
            });
        }

        // Check key strength
        let weak_key = match key_type.to_uppercase().as_str() {
            "RSA" => key_size < 2048,
            "EC" | "ECDSA" => key_size < 256,
            _ => false,
        };

        if weak_key {
            issues.push(CertIssue {
                title: format!("Weak key: {} {} bits", key_type, key_size),
                severity: IssueSeverity::High,
                description: "Key size is below recommended minimum for security".to_string(),
            });
        }

        // Check signature algorithm
        let sig_lower = sig_alg.to_lowercase();
        if sig_lower.contains("md5") {
            issues.push(CertIssue {
                title: "MD5 signature algorithm".to_string(),
                severity: IssueSeverity::Critical,
                description: "MD5 is cryptographically broken and should not be used".to_string(),
            });
        } else if sig_lower.contains("sha1") && !sig_lower.contains("sha1with") {
            issues.push(CertIssue {
                title: "SHA-1 signature algorithm".to_string(),
                severity: IssueSeverity::High,
                description: "SHA-1 is deprecated and should be replaced with SHA-256 or better"
                    .to_string(),
            });
        }

        // Sort by severity
        issues.sort_by(|a, b| b.severity.cmp(&a.severity));
        issues
    }

    /// Find all expired certificates
    pub fn expired(&self) -> Vec<CertProfile> {
        self.all_profiles()
            .into_iter()
            .filter(|c| c.is_expired)
            .collect()
    }

    /// Find certificates expiring within N days
    pub fn expiring(&self, days: i64) -> Vec<CertProfile> {
        self.all_profiles()
            .into_iter()
            .filter(|c| c.days_until_expiry.is_some_and(|d| d >= 0 && d <= days))
            .collect()
    }

    /// Find all self-signed certificates
    pub fn self_signed(&self) -> Vec<CertProfile> {
        self.all_profiles()
            .into_iter()
            .filter(|c| c.is_self_signed)
            .collect()
    }

    /// Find certificates with weak keys
    pub fn weak(&self) -> Vec<CertProfile> {
        self.all_profiles()
            .into_iter()
            .filter(|c| match c.key_type.to_uppercase().as_str() {
                "RSA" => c.key_size < 2048,
                "EC" | "ECDSA" => c.key_size < 256,
                _ => false,
            })
            .collect()
    }

    /// Find certificates by issuer
    pub fn by_issuer(&self, issuer: &str) -> Vec<CertProfile> {
        let issuer_lower = issuer.to_lowercase();
        self.all_profiles()
            .into_iter()
            .filter(|c| c.issuer.to_lowercase().contains(&issuer_lower))
            .collect()
    }

    /// Find certificate for a specific host
    pub fn for_host(&self, host: &str) -> Vec<CertProfile> {
        let host_id = if host.starts_with("host:") {
            host.to_string()
        } else {
            format!("host:{}", host)
        };

        let mut certs = Vec::new();

        if let Some(node) = self.graph.get_node(&host_id) {
            for edge in &node.out_edges {
                if edge.target_id.starts_with("cert:") {
                    if let Some(cert) = self.profile(&edge.target_id) {
                        certs.push(cert);
                    }
                }
            }

            // Also check services on this host
            for edge in &node.out_edges {
                if edge.edge_type == EdgeType::HasService {
                    if let Some(svc_node) = self.graph.get_node(&edge.target_id) {
                        for svc_edge in &svc_node.out_edges {
                            if svc_edge.target_id.starts_with("cert:") {
                                if let Some(cert) = self.profile(&svc_edge.target_id) {
                                    if !certs.iter().any(|c| c.fingerprint == cert.fingerprint) {
                                        certs.push(cert);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        certs
    }

    /// Get all certificate profiles
    fn all_profiles(&self) -> Vec<CertProfile> {
        self.graph
            .all_nodes()
            .iter()
            .filter(|n| n.id.starts_with("cert:"))
            .filter_map(|n| self.profile(&n.id))
            .collect()
    }

    /// Get all certificate fingerprints
    pub fn all(&self) -> Vec<String> {
        self.graph
            .all_nodes()
            .iter()
            .filter(|n| n.id.starts_with("cert:"))
            .map(|n| n.id.trim_start_matches("cert:").to_string())
            .collect()
    }

    /// Get certificate statistics
    pub fn stats(&self) -> CertStats {
        let all = self.all_profiles();

        CertStats {
            total: all.len(),
            expired: all.iter().filter(|c| c.is_expired).count(),
            expiring_30_days: all.iter().filter(|c| c.is_expiring_soon).count(),
            self_signed: all.iter().filter(|c| c.is_self_signed).count(),
            weak_keys: all
                .iter()
                .filter(|c| match c.key_type.to_uppercase().as_str() {
                    "RSA" => c.key_size < 2048,
                    "EC" | "ECDSA" => c.key_size < 256,
                    _ => false,
                })
                .count(),
        }
    }
}

/// Certificate statistics
#[derive(Debug, Clone, Default)]
pub struct CertStats {
    pub total: usize,
    pub expired: usize,
    pub expiring_30_days: usize,
    pub self_signed: usize,
    pub weak_keys: usize,
}

/// Parse certificate subject from label
fn parse_subject(label: &str) -> CertSubject {
    let mut subject = CertSubject::default();

    // Look for CN= pattern
    if let Some(cn_start) = label.find("CN=") {
        let cn_part = &label[cn_start + 3..];
        let cn_end = cn_part.find([',', '/', ' ']).unwrap_or(cn_part.len());
        subject.common_name = Some(cn_part[..cn_end].to_string());
    }

    // Look for O= pattern
    if let Some(o_start) = label.find("O=") {
        let o_part = &label[o_start + 2..];
        let o_end = o_part.find([',', '/', ' ']).unwrap_or(o_part.len());
        subject.organization = Some(o_part[..o_end].to_string());
    }

    subject
}

/// Parse issuer from label
fn parse_issuer(label: &str) -> String {
    // Look for Issuer: or issuer= pattern
    if let Some(idx) = label.to_lowercase().find("issuer") {
        let part = &label[idx..];
        let start = part.find([':', '=']).map(|i| i + 1).unwrap_or(7);
        let end = part[start..]
            .find(['\n', ';'])
            .unwrap_or(part.len() - start);
        return part[start..start + end].trim().to_string();
    }

    "Unknown".to_string()
}

/// Parse validity dates from label
fn parse_validity(label: &str) -> (Option<String>, Option<String>, Option<i64>) {
    // This is a placeholder - real implementation would parse date strings
    (None, None, None)
}

/// Check if certificate is self-signed
fn is_self_signed(label: &str, subject: &CertSubject, issuer: &str) -> bool {
    let label_lower = label.to_lowercase();
    if label_lower.contains("self-signed") || label_lower.contains("selfsigned") {
        return true;
    }

    // Check if subject CN matches issuer
    if let Some(ref cn) = subject.common_name {
        if issuer.contains(cn) {
            return true;
        }
    }

    false
}

/// Parse Subject Alternative Names from label
fn parse_sans(label: &str) -> Vec<String> {
    let mut sans = Vec::new();

    // Look for SAN or DNS: patterns
    let label_lower = label.to_lowercase();
    if let Some(idx) = label_lower.find("san") {
        let part = &label[idx..];
        // Extract domain names after SAN marker
        for word in part.split_whitespace() {
            let clean = word
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '-' && c != '*');
            if clean.contains('.') && clean.len() > 3 {
                sans.push(clean.to_string());
            }
        }
    }

    // Also look for DNS: entries
    for (idx, _) in label.match_indices("DNS:") {
        let part = &label[idx + 4..];
        let end = part.find([',', ' ', '\n']).unwrap_or(part.len());
        let san = part[..end].trim();
        if !san.is_empty() && !sans.contains(&san.to_string()) {
            sans.push(san.to_string());
        }
    }

    sans
}

/// Parse key information from label
fn parse_key_info(label: &str) -> (String, u32) {
    let label_lower = label.to_lowercase();

    // Check for RSA
    if label_lower.contains("rsa") {
        // Look for key size
        for size in [4096, 2048, 1024, 512] {
            if label.contains(&size.to_string()) {
                return ("RSA".to_string(), size);
            }
        }
        return ("RSA".to_string(), 2048); // Default assumption
    }

    // Check for EC/ECDSA
    if label_lower.contains("ec") || label_lower.contains("ecdsa") {
        for size in [521, 384, 256, 224] {
            if label.contains(&size.to_string()) {
                return ("EC".to_string(), size);
            }
        }
        return ("EC".to_string(), 256); // Default assumption
    }

    ("Unknown".to_string(), 0)
}

/// Parse signature algorithm from label
fn parse_signature_algorithm(label: &str) -> String {
    let label_lower = label.to_lowercase();

    if label_lower.contains("sha256") {
        return "SHA256withRSA".to_string();
    }
    if label_lower.contains("sha384") {
        return "SHA384withRSA".to_string();
    }
    if label_lower.contains("sha512") {
        return "SHA512withRSA".to_string();
    }
    if label_lower.contains("sha1") {
        return "SHA1withRSA".to_string();
    }
    if label_lower.contains("md5") {
        return "MD5withRSA".to_string();
    }

    "Unknown".to_string()
}
