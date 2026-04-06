//! PlaybookDefinitionSegment - Storage for playbook methodology definitions
//!
//! Stores playbook templates (THP3-web, THP3-network, etc.) with phases and techniques.
//! This complements PlaybookSegment which stores execution history.

use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::primitives::encoding::{
    read_string, read_varu32, write_string, write_varu32, DecodeError,
};

/// A playbook definition (methodology template)
#[derive(Debug, Clone)]
pub struct Playbook {
    pub id: String,                 // "thp3-web", "thp3-network", "thp3-recon"
    pub name: String,               // Human-readable name
    pub description: String,        // What this playbook tests
    pub category: PlaybookCategory, // web, network, recon, cloud, etc.
    pub phases: Vec<Phase>,         // Ordered phases
    pub created_at: i64,
    pub updated_at: i64,
    pub author: String,    // Creator/maintainer
    pub version: String,   // Version string
    pub tags: Vec<String>, // Searchable tags
}

/// Playbook category for filtering
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PlaybookCategory {
    Web = 0,
    Network = 1,
    Recon = 2,
    Cloud = 3,
    Mobile = 4,
    Api = 5,
    Infrastructure = 6,
    Wireless = 7,
    Social = 8,
    Physical = 9,
    Custom = 255,
}

impl PlaybookCategory {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Web,
            1 => Self::Network,
            2 => Self::Recon,
            3 => Self::Cloud,
            4 => Self::Mobile,
            5 => Self::Api,
            6 => Self::Infrastructure,
            7 => Self::Wireless,
            8 => Self::Social,
            9 => Self::Physical,
            _ => Self::Custom,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Network => "network",
            Self::Recon => "recon",
            Self::Cloud => "cloud",
            Self::Mobile => "mobile",
            Self::Api => "api",
            Self::Infrastructure => "infrastructure",
            Self::Wireless => "wireless",
            Self::Social => "social",
            Self::Physical => "physical",
            Self::Custom => "custom",
        }
    }
}

/// A phase within a playbook
#[derive(Debug, Clone)]
pub struct Phase {
    pub name: String,               // "Discovery", "Exploitation", "Post-Exploitation"
    pub objective: String,          // What this phase achieves
    pub order: u32,                 // Phase sequence number
    pub techniques: Vec<Technique>, // Techniques/tests in this phase
}

/// A technique/test within a phase
#[derive(Debug, Clone)]
pub struct Technique {
    pub id: String,               // "port-scan", "vuln-scan", "cred-spray"
    pub name: String,             // Human-readable name
    pub description: String,      // What this technique does
    pub command_hint: String,     // Suggested rb command (e.g., "rb network ports scan {target}")
    pub mitre_id: Option<String>, // MITRE ATT&CK ID if applicable
    pub required: bool,           // Is this technique mandatory?
}

impl Playbook {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        write_string(&mut buf, &self.id);
        write_string(&mut buf, &self.name);
        write_string(&mut buf, &self.description);
        buf.push(self.category as u8);

        // Phases
        write_varu32(&mut buf, self.phases.len() as u32);
        for phase in &self.phases {
            phase.write(&mut buf);
        }

        // Timestamps
        buf.extend_from_slice(&self.created_at.to_le_bytes());
        buf.extend_from_slice(&self.updated_at.to_le_bytes());

        // Metadata
        write_string(&mut buf, &self.author);
        write_string(&mut buf, &self.version);

        // Tags
        write_varu32(&mut buf, self.tags.len() as u32);
        for tag in &self.tags {
            write_string(&mut buf, tag);
        }

        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut pos = 0;

        let id = read_string(bytes, &mut pos)?.to_string();
        let name = read_string(bytes, &mut pos)?.to_string();
        let description = read_string(bytes, &mut pos)?.to_string();

        if pos >= bytes.len() {
            return Err(DecodeError("playbook truncated at category"));
        }
        let category = PlaybookCategory::from_u8(bytes[pos]);
        pos += 1;

        let phase_count = read_varu32(bytes, &mut pos)? as usize;
        let mut phases = Vec::with_capacity(phase_count);
        for _ in 0..phase_count {
            phases.push(Phase::read(bytes, &mut pos)?);
        }

        if pos + 16 > bytes.len() {
            return Err(DecodeError("playbook truncated at timestamps"));
        }
        let created_at = i64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let updated_at = i64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap());
        pos += 8;

        let author = read_string(bytes, &mut pos)?.to_string();
        let version = read_string(bytes, &mut pos)?.to_string();

        let tag_count = read_varu32(bytes, &mut pos)? as usize;
        let mut tags = Vec::with_capacity(tag_count);
        for _ in 0..tag_count {
            tags.push(read_string(bytes, &mut pos)?.to_string());
        }

        Ok(Self {
            id,
            name,
            description,
            category,
            phases,
            created_at,
            updated_at,
            author,
            version,
            tags,
        })
    }
}

impl Phase {
    fn write(&self, buf: &mut Vec<u8>) {
        write_string(buf, &self.name);
        write_string(buf, &self.objective);
        buf.extend_from_slice(&self.order.to_le_bytes());

        write_varu32(buf, self.techniques.len() as u32);
        for tech in &self.techniques {
            tech.write(buf);
        }
    }

    fn read(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let name = read_string(bytes, pos)?.to_string();
        let objective = read_string(bytes, pos)?.to_string();

        if *pos + 4 > bytes.len() {
            return Err(DecodeError("phase truncated at order"));
        }
        let order = u32::from_le_bytes(bytes[*pos..*pos + 4].try_into().unwrap());
        *pos += 4;

        let tech_count = read_varu32(bytes, pos)? as usize;
        let mut techniques = Vec::with_capacity(tech_count);
        for _ in 0..tech_count {
            techniques.push(Technique::read(bytes, pos)?);
        }

        Ok(Self {
            name,
            objective,
            order,
            techniques,
        })
    }
}

impl Technique {
    fn write(&self, buf: &mut Vec<u8>) {
        write_string(buf, &self.id);
        write_string(buf, &self.name);
        write_string(buf, &self.description);
        write_string(buf, &self.command_hint);

        // Optional MITRE ID
        match &self.mitre_id {
            Some(id) => {
                buf.push(1);
                write_string(buf, id);
            }
            None => buf.push(0),
        }

        buf.push(if self.required { 1 } else { 0 });
    }

    fn read(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let id = read_string(bytes, pos)?.to_string();
        let name = read_string(bytes, pos)?.to_string();
        let description = read_string(bytes, pos)?.to_string();
        let command_hint = read_string(bytes, pos)?.to_string();

        if *pos >= bytes.len() {
            return Err(DecodeError("technique truncated at mitre flag"));
        }
        let has_mitre = bytes[*pos] != 0;
        *pos += 1;

        let mitre_id = if has_mitre {
            Some(read_string(bytes, pos)?.to_string())
        } else {
            None
        };

        if *pos >= bytes.len() {
            return Err(DecodeError("technique truncated at required flag"));
        }
        let required = bytes[*pos] != 0;
        *pos += 1;

        Ok(Self {
            id,
            name,
            description,
            command_hint,
            mitre_id,
            required,
        })
    }
}

// ============================================================================
// Segment Header
// ============================================================================

#[derive(Debug, Clone, Copy)]
struct PlaybookDefHeader {
    playbook_count: u32,
    directory_len: u64,
    payload_len: u64,
}

impl PlaybookDefHeader {
    const MAGIC: [u8; 4] = *b"PD01";
    const VERSION: u16 = 1;
    const SIZE: usize = 4 + 2 + 2 + 4 + 8 + 8; // 28 bytes

    fn write(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&Self::MAGIC);
        buf.extend_from_slice(&Self::VERSION.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // reserved
        buf.extend_from_slice(&self.playbook_count.to_le_bytes());
        buf.extend_from_slice(&self.directory_len.to_le_bytes());
        buf.extend_from_slice(&self.payload_len.to_le_bytes());
    }

    fn read(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < Self::SIZE {
            return Err(DecodeError("playbook def header too small"));
        }
        if bytes[0..4] != Self::MAGIC {
            return Err(DecodeError("invalid playbook def segment magic"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != Self::VERSION {
            return Err(DecodeError("unsupported playbook def segment version"));
        }

        let playbook_count = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let directory_len = u64::from_le_bytes(bytes[12..20].try_into().unwrap());
        let payload_len = u64::from_le_bytes(bytes[20..28].try_into().unwrap());

        Ok(Self {
            playbook_count,
            directory_len,
            payload_len,
        })
    }
}

// ============================================================================
// PlaybookDefinitionSegment - Mutable API
// ============================================================================

/// Segment for storing playbook methodology definitions
#[derive(Debug, Default, Clone)]
pub struct PlaybookDefinitionSegment {
    playbooks: Vec<Playbook>,
    /// Index by ID for fast lookup
    id_index: HashMap<String, usize>,
    /// Index by category for filtering
    category_index: HashMap<PlaybookCategory, Vec<usize>>,
}

impl PlaybookDefinitionSegment {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create segment with built-in THP3 playbooks
    pub fn with_builtins() -> Self {
        let mut segment = Self::new();
        segment.add_builtin_playbooks();
        segment
    }

    /// Add a playbook definition
    pub fn push(&mut self, playbook: Playbook) {
        // Remove existing if updating
        if let Some(&idx) = self.id_index.get(&playbook.id) {
            let old = &self.playbooks[idx];
            if let Some(indices) = self.category_index.get_mut(&old.category) {
                indices.retain(|&i| i != idx);
            }
            self.playbooks[idx] = playbook.clone();
            self.category_index
                .entry(playbook.category)
                .or_default()
                .push(idx);
        } else {
            let idx = self.playbooks.len();
            self.id_index.insert(playbook.id.clone(), idx);
            self.category_index
                .entry(playbook.category)
                .or_default()
                .push(idx);
            self.playbooks.push(playbook);
        }
    }

    pub fn len(&self) -> usize {
        self.playbooks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.playbooks.is_empty()
    }

    /// Get playbook by ID
    pub fn get(&self, id: &str) -> Option<&Playbook> {
        self.id_index.get(id).map(|&idx| &self.playbooks[idx])
    }

    /// Get all playbooks of a category
    pub fn get_by_category(&self, category: PlaybookCategory) -> Vec<&Playbook> {
        self.category_index
            .get(&category)
            .map(|indices| indices.iter().map(|&i| &self.playbooks[i]).collect())
            .unwrap_or_default()
    }

    /// List all playbook IDs
    pub fn list_ids(&self) -> Vec<&str> {
        self.playbooks.iter().map(|p| p.id.as_str()).collect()
    }

    /// Get all playbooks
    pub fn all(&self) -> &[Playbook] {
        &self.playbooks
    }

    /// Remove a playbook by ID
    pub fn remove(&mut self, id: &str) -> Option<Playbook> {
        if let Some(&idx) = self.id_index.get(id) {
            let playbook = self.playbooks.remove(idx);
            self.rebuild_indices();
            Some(playbook)
        } else {
            None
        }
    }

    fn rebuild_indices(&mut self) {
        self.id_index.clear();
        self.category_index.clear();
        for (idx, playbook) in self.playbooks.iter().enumerate() {
            self.id_index.insert(playbook.id.clone(), idx);
            self.category_index
                .entry(playbook.category)
                .or_default()
                .push(idx);
        }
    }

    // ========================================================================
    // Built-in Playbooks (THP3 methodologies)
    // ========================================================================

    fn add_builtin_playbooks(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        // THP3-Web: Web Application Exploitation
        self.push(Playbook {
            id: "thp3-web".to_string(),
            name: "THP3 Web Application Testing".to_string(),
            description: "Comprehensive web application penetration testing methodology covering reconnaissance, vulnerability assessment, and exploitation.".to_string(),
            category: PlaybookCategory::Web,
            phases: vec![
                Phase {
                    name: "Reconnaissance".to_string(),
                    objective: "Gather information about the target web application".to_string(),
                    order: 1,
                    techniques: vec![
                        Technique {
                            id: "subdomain-enum".to_string(),
                            name: "Subdomain Enumeration".to_string(),
                            description: "Discover subdomains using passive and active methods".to_string(),
                            command_hint: "rb recon domain subdomains {target}".to_string(),
                            mitre_id: Some("T1596.001".to_string()),
                            required: true,
                        },
                        Technique {
                            id: "tech-fingerprint".to_string(),
                            name: "Technology Fingerprinting".to_string(),
                            description: "Identify web technologies, frameworks, and versions".to_string(),
                            command_hint: "rb web asset security {target}".to_string(),
                            mitre_id: Some("T1592".to_string()),
                            required: true,
                        },
                        Technique {
                            id: "dir-brute".to_string(),
                            name: "Directory Bruteforce".to_string(),
                            description: "Discover hidden directories and files".to_string(),
                            command_hint: "rb web security fuzz {target} --wordlist dirs".to_string(),
                            mitre_id: Some("T1083".to_string()),
                            required: false,
                        },
                    ],
                },
                Phase {
                    name: "Vulnerability Assessment".to_string(),
                    objective: "Identify vulnerabilities in the web application".to_string(),
                    order: 2,
                    techniques: vec![
                        Technique {
                            id: "xss-scan".to_string(),
                            name: "XSS Detection".to_string(),
                            description: "Test for Cross-Site Scripting vulnerabilities".to_string(),
                            command_hint: "rb web security xss {target}".to_string(),
                            mitre_id: Some("T1189".to_string()),
                            required: true,
                        },
                        Technique {
                            id: "sqli-scan".to_string(),
                            name: "SQL Injection Detection".to_string(),
                            description: "Test for SQL Injection vulnerabilities".to_string(),
                            command_hint: "rb web security sqli {target}".to_string(),
                            mitre_id: Some("T1190".to_string()),
                            required: true,
                        },
                        Technique {
                            id: "header-check".to_string(),
                            name: "Security Headers Analysis".to_string(),
                            description: "Check for missing or misconfigured security headers".to_string(),
                            command_hint: "rb web asset headers {target}".to_string(),
                            mitre_id: None,
                            required: true,
                        },
                    ],
                },
                Phase {
                    name: "Exploitation".to_string(),
                    objective: "Exploit identified vulnerabilities".to_string(),
                    order: 3,
                    techniques: vec![
                        Technique {
                            id: "auth-bypass".to_string(),
                            name: "Authentication Bypass".to_string(),
                            description: "Attempt to bypass authentication mechanisms".to_string(),
                            command_hint: "rb auth bypass {target}".to_string(),
                            mitre_id: Some("T1078".to_string()),
                            required: false,
                        },
                        Technique {
                            id: "file-upload".to_string(),
                            name: "File Upload Exploitation".to_string(),
                            description: "Test for unrestricted file upload vulnerabilities".to_string(),
                            command_hint: "rb web security upload {target}".to_string(),
                            mitre_id: Some("T1105".to_string()),
                            required: false,
                        },
                    ],
                },
            ],
            created_at: now,
            updated_at: now,
            author: "redblue".to_string(),
            version: "1.0.0".to_string(),
            tags: vec!["web".to_string(), "owasp".to_string(), "pentest".to_string()],
        });

        // THP3-Network: Network Infrastructure Testing
        self.push(Playbook {
            id: "thp3-network".to_string(),
            name: "THP3 Network Infrastructure Testing".to_string(),
            description: "Network infrastructure penetration testing including port scanning, service enumeration, and vulnerability assessment.".to_string(),
            category: PlaybookCategory::Network,
            phases: vec![
                Phase {
                    name: "Host Discovery".to_string(),
                    objective: "Identify live hosts on the network".to_string(),
                    order: 1,
                    techniques: vec![
                        Technique {
                            id: "ping-sweep".to_string(),
                            name: "Ping Sweep".to_string(),
                            description: "Discover live hosts using ICMP".to_string(),
                            command_hint: "rb network host ping {target}".to_string(),
                            mitre_id: Some("T1018".to_string()),
                            required: true,
                        },
                        Technique {
                            id: "arp-scan".to_string(),
                            name: "ARP Scan".to_string(),
                            description: "Discover hosts on local network using ARP".to_string(),
                            command_hint: "rb network host discover {target}".to_string(),
                            mitre_id: Some("T1018".to_string()),
                            required: false,
                        },
                    ],
                },
                Phase {
                    name: "Port Scanning".to_string(),
                    objective: "Identify open ports and services".to_string(),
                    order: 2,
                    techniques: vec![
                        Technique {
                            id: "port-scan".to_string(),
                            name: "Port Scan".to_string(),
                            description: "Scan for open TCP/UDP ports".to_string(),
                            command_hint: "rb network ports scan {target} --preset full".to_string(),
                            mitre_id: Some("T1046".to_string()),
                            required: true,
                        },
                        Technique {
                            id: "service-enum".to_string(),
                            name: "Service Enumeration".to_string(),
                            description: "Identify services running on open ports".to_string(),
                            command_hint: "rb network ports scan {target} --banner".to_string(),
                            mitre_id: Some("T1046".to_string()),
                            required: true,
                        },
                    ],
                },
                Phase {
                    name: "Vulnerability Scanning".to_string(),
                    objective: "Identify vulnerabilities in discovered services".to_string(),
                    order: 3,
                    techniques: vec![
                        Technique {
                            id: "vuln-scan".to_string(),
                            name: "Vulnerability Scan".to_string(),
                            description: "Scan services for known vulnerabilities".to_string(),
                            command_hint: "rb intel vuln search {service} {version}".to_string(),
                            mitre_id: Some("T1595.002".to_string()),
                            required: true,
                        },
                        Technique {
                            id: "ssl-audit".to_string(),
                            name: "SSL/TLS Audit".to_string(),
                            description: "Check SSL/TLS configuration and certificates".to_string(),
                            command_hint: "rb tls audit {target}".to_string(),
                            mitre_id: None,
                            required: true,
                        },
                    ],
                },
                Phase {
                    name: "Exploitation".to_string(),
                    objective: "Exploit identified vulnerabilities".to_string(),
                    order: 4,
                    techniques: vec![
                        Technique {
                            id: "cred-spray".to_string(),
                            name: "Credential Spraying".to_string(),
                            description: "Test common credentials against services".to_string(),
                            command_hint: "rb auth spray {target} --userlist users.txt --passlist pass.txt".to_string(),
                            mitre_id: Some("T1110.003".to_string()),
                            required: false,
                        },
                        Technique {
                            id: "exploit-exec".to_string(),
                            name: "Exploit Execution".to_string(),
                            description: "Execute exploits against vulnerable services".to_string(),
                            command_hint: "rb exploit run {exploit-id} {target}".to_string(),
                            mitre_id: Some("T1203".to_string()),
                            required: false,
                        },
                    ],
                },
            ],
            created_at: now,
            updated_at: now,
            author: "redblue".to_string(),
            version: "1.0.0".to_string(),
            tags: vec!["network".to_string(), "infrastructure".to_string(), "pentest".to_string()],
        });

        // THP3-Recon: Reconnaissance Phase
        self.push(Playbook {
            id: "thp3-recon".to_string(),
            name: "THP3 Reconnaissance".to_string(),
            description: "Comprehensive reconnaissance playbook for initial target assessment and information gathering.".to_string(),
            category: PlaybookCategory::Recon,
            phases: vec![
                Phase {
                    name: "Passive Reconnaissance".to_string(),
                    objective: "Gather information without direct interaction".to_string(),
                    order: 1,
                    techniques: vec![
                        Technique {
                            id: "whois".to_string(),
                            name: "WHOIS Lookup".to_string(),
                            description: "Query domain registration information".to_string(),
                            command_hint: "rb recon domain whois {target}".to_string(),
                            mitre_id: Some("T1596.002".to_string()),
                            required: true,
                        },
                        Technique {
                            id: "dns-recon".to_string(),
                            name: "DNS Reconnaissance".to_string(),
                            description: "Enumerate DNS records".to_string(),
                            command_hint: "rb dns record lookup {target} --type ANY".to_string(),
                            mitre_id: Some("T1596.001".to_string()),
                            required: true,
                        },
                        Technique {
                            id: "cert-enum".to_string(),
                            name: "Certificate Enumeration".to_string(),
                            description: "Query Certificate Transparency logs".to_string(),
                            command_hint: "rb tls ct-logs {target}".to_string(),
                            mitre_id: Some("T1596.003".to_string()),
                            required: true,
                        },
                        Technique {
                            id: "asn-lookup".to_string(),
                            name: "ASN Lookup".to_string(),
                            description: "Identify AS numbers and IP ranges".to_string(),
                            command_hint: "rb recon asn lookup {target}".to_string(),
                            mitre_id: Some("T1590".to_string()),
                            required: false,
                        },
                    ],
                },
                Phase {
                    name: "Active Reconnaissance".to_string(),
                    objective: "Gather information through direct interaction".to_string(),
                    order: 2,
                    techniques: vec![
                        Technique {
                            id: "subdomain-brute".to_string(),
                            name: "Subdomain Bruteforce".to_string(),
                            description: "Actively brute force subdomains".to_string(),
                            command_hint: "rb recon subdomain bruteforce {target}".to_string(),
                            mitre_id: Some("T1595.003".to_string()),
                            required: false,
                        },
                        Technique {
                            id: "vhost-enum".to_string(),
                            name: "Virtual Host Enumeration".to_string(),
                            description: "Discover virtual hosts on target".to_string(),
                            command_hint: "rb web security vhost {target}".to_string(),
                            mitre_id: Some("T1595".to_string()),
                            required: false,
                        },
                    ],
                },
                Phase {
                    name: "OSINT".to_string(),
                    objective: "Open source intelligence gathering".to_string(),
                    order: 3,
                    techniques: vec![
                        Technique {
                            id: "email-harvest".to_string(),
                            name: "Email Harvesting".to_string(),
                            description: "Gather email addresses from public sources".to_string(),
                            command_hint: "rb recon email harvest {target}".to_string(),
                            mitre_id: Some("T1589.002".to_string()),
                            required: false,
                        },
                        Technique {
                            id: "breach-check".to_string(),
                            name: "Breach Data Check".to_string(),
                            description: "Check for credentials in breach databases".to_string(),
                            command_hint: "rb recon breach check {email}".to_string(),
                            mitre_id: Some("T1589.001".to_string()),
                            required: false,
                        },
                        Technique {
                            id: "social-profile".to_string(),
                            name: "Social Media Profiling".to_string(),
                            description: "Discover social media profiles".to_string(),
                            command_hint: "rb recon social profile {username}".to_string(),
                            mitre_id: Some("T1593".to_string()),
                            required: false,
                        },
                    ],
                },
            ],
            created_at: now,
            updated_at: now,
            author: "redblue".to_string(),
            version: "1.0.0".to_string(),
            tags: vec!["recon".to_string(), "osint".to_string(), "passive".to_string()],
        });
    }

    // ========================================================================
    // Serialization
    // ========================================================================

    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Build directory and payload
        let mut directory = Vec::new();
        let mut payload = Vec::new();

        for playbook in &self.playbooks {
            let offset = payload.len() as u64;
            let pb_bytes = playbook.to_bytes();
            let len = pb_bytes.len() as u64;

            // Directory entry: id, offset, len
            write_string(&mut directory, &playbook.id);
            directory.extend_from_slice(&offset.to_le_bytes());
            directory.extend_from_slice(&len.to_le_bytes());

            payload.extend_from_slice(&pb_bytes);
        }

        let header = PlaybookDefHeader {
            playbook_count: self.playbooks.len() as u32,
            directory_len: directory.len() as u64,
            payload_len: payload.len() as u64,
        };

        header.write(&mut buf);
        buf.extend_from_slice(&directory);
        buf.extend_from_slice(&payload);

        buf
    }

    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < PlaybookDefHeader::SIZE {
            return Err(DecodeError("segment too small"));
        }

        let header = PlaybookDefHeader::read(bytes)?;

        let dir_start = PlaybookDefHeader::SIZE;
        let dir_end = dir_start + header.directory_len as usize;
        let payload_start = dir_end;
        let payload_end = payload_start + header.payload_len as usize;

        if bytes.len() < payload_end {
            return Err(DecodeError("segment truncated"));
        }

        let dir_bytes = &bytes[dir_start..dir_end];
        let payload_bytes = &bytes[payload_start..payload_end];

        let mut segment = Self::new();
        let mut dir_pos = 0;

        for _ in 0..header.playbook_count {
            let _id = read_string(dir_bytes, &mut dir_pos)?;

            if dir_pos + 16 > dir_bytes.len() {
                return Err(DecodeError("directory truncated"));
            }
            let offset = u64::from_le_bytes(dir_bytes[dir_pos..dir_pos + 8].try_into().unwrap());
            dir_pos += 8;
            let len = u64::from_le_bytes(dir_bytes[dir_pos..dir_pos + 8].try_into().unwrap());
            dir_pos += 8;

            let pb_start = offset as usize;
            let pb_end = pb_start + len as usize;

            if pb_end > payload_bytes.len() {
                return Err(DecodeError("playbook out of bounds"));
            }

            let playbook = Playbook::from_bytes(&payload_bytes[pb_start..pb_end])?;
            segment.push(playbook);
        }

        Ok(segment)
    }
}

// ============================================================================
// PlaybookDefinitionView - Zero-copy read-only access
// ============================================================================

/// Read-only view for memory-mapped access
pub struct PlaybookDefinitionView {
    data: Arc<Vec<u8>>,
    directory: HashMap<String, (u64, u64)>, // id -> (offset, len)
    payload_start: usize,
}

impl PlaybookDefinitionView {
    pub fn from_arc(data: Arc<Vec<u8>>, offset: usize, len: usize) -> Result<Self, DecodeError> {
        let slice = &data[offset..offset + len];
        if slice.len() < PlaybookDefHeader::SIZE {
            return Err(DecodeError("view too small"));
        }

        let header = PlaybookDefHeader::read(slice)?;

        let dir_start = offset + PlaybookDefHeader::SIZE;
        let dir_end = dir_start + header.directory_len as usize;
        let payload_start = dir_end;

        if dir_end > offset + len {
            return Err(DecodeError("directory out of bounds"));
        }

        let dir_bytes = &data[dir_start..dir_end];
        let mut directory = HashMap::with_capacity(header.playbook_count as usize);
        let mut pos = 0;

        for _ in 0..header.playbook_count {
            let id = read_string(dir_bytes, &mut pos)?.to_string();
            let p_offset = u64::from_le_bytes(dir_bytes[pos..pos + 8].try_into().unwrap());
            pos += 8;
            let p_len = u64::from_le_bytes(dir_bytes[pos..pos + 8].try_into().unwrap());
            pos += 8;

            directory.insert(id, (p_offset, p_len));
        }

        Ok(Self {
            data,
            directory,
            payload_start,
        })
    }

    /// Get playbook by ID
    pub fn get(&self, id: &str) -> Result<Option<Playbook>, DecodeError> {
        if let Some(&(offset, len)) = self.directory.get(id) {
            let abs_start = self.payload_start + offset as usize;
            let abs_end = abs_start + len as usize;

            if abs_end > self.data.len() {
                return Err(DecodeError("playbook read out of bounds"));
            }

            let playbook = Playbook::from_bytes(&self.data[abs_start..abs_end])?;
            Ok(Some(playbook))
        } else {
            Ok(None)
        }
    }

    /// List all playbook IDs
    pub fn list_ids(&self) -> Vec<&str> {
        self.directory.keys().map(|s| s.as_str()).collect()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_playbook_roundtrip() {
        let playbook = Playbook {
            id: "test-playbook".to_string(),
            name: "Test Playbook".to_string(),
            description: "A test playbook for unit tests".to_string(),
            category: PlaybookCategory::Web,
            phases: vec![Phase {
                name: "Phase 1".to_string(),
                objective: "Test objective".to_string(),
                order: 1,
                techniques: vec![
                    Technique {
                        id: "tech-1".to_string(),
                        name: "Technique 1".to_string(),
                        description: "First technique".to_string(),
                        command_hint: "rb test {target}".to_string(),
                        mitre_id: Some("T1234".to_string()),
                        required: true,
                    },
                    Technique {
                        id: "tech-2".to_string(),
                        name: "Technique 2".to_string(),
                        description: "Second technique".to_string(),
                        command_hint: "rb test2 {target}".to_string(),
                        mitre_id: None,
                        required: false,
                    },
                ],
            }],
            created_at: 1700000000,
            updated_at: 1700000000,
            author: "test".to_string(),
            version: "1.0.0".to_string(),
            tags: vec!["test".to_string(), "unit".to_string()],
        };

        let bytes = playbook.to_bytes();
        let restored = Playbook::from_bytes(&bytes).unwrap();

        assert_eq!(restored.id, "test-playbook");
        assert_eq!(restored.name, "Test Playbook");
        assert_eq!(restored.category, PlaybookCategory::Web);
        assert_eq!(restored.phases.len(), 1);
        assert_eq!(restored.phases[0].techniques.len(), 2);
        assert_eq!(
            restored.phases[0].techniques[0].mitre_id,
            Some("T1234".to_string())
        );
        assert_eq!(restored.phases[0].techniques[1].mitre_id, None);
        assert_eq!(restored.tags.len(), 2);
    }

    #[test]
    fn test_segment_with_builtins() {
        let segment = PlaybookDefinitionSegment::with_builtins();

        assert_eq!(segment.len(), 3);
        assert!(segment.get("thp3-web").is_some());
        assert!(segment.get("thp3-network").is_some());
        assert!(segment.get("thp3-recon").is_some());

        let web = segment.get("thp3-web").unwrap();
        assert_eq!(web.category, PlaybookCategory::Web);
        assert!(web.phases.len() >= 3);

        let network = segment.get("thp3-network").unwrap();
        assert_eq!(network.category, PlaybookCategory::Network);

        let recon = segment.get("thp3-recon").unwrap();
        assert_eq!(recon.category, PlaybookCategory::Recon);
    }

    #[test]
    fn test_segment_roundtrip() {
        let segment = PlaybookDefinitionSegment::with_builtins();

        let bytes = segment.serialize();
        let restored = PlaybookDefinitionSegment::deserialize(&bytes).unwrap();

        assert_eq!(restored.len(), segment.len());
        assert_eq!(restored.list_ids().len(), segment.list_ids().len());

        for id in segment.list_ids() {
            let orig = segment.get(id).unwrap();
            let rest = restored.get(id).unwrap();
            assert_eq!(orig.id, rest.id);
            assert_eq!(orig.name, rest.name);
            assert_eq!(orig.phases.len(), rest.phases.len());
        }
    }

    #[test]
    fn test_category_filtering() {
        let segment = PlaybookDefinitionSegment::with_builtins();

        let web_pbs = segment.get_by_category(PlaybookCategory::Web);
        assert_eq!(web_pbs.len(), 1);
        assert_eq!(web_pbs[0].id, "thp3-web");

        let network_pbs = segment.get_by_category(PlaybookCategory::Network);
        assert_eq!(network_pbs.len(), 1);

        let cloud_pbs = segment.get_by_category(PlaybookCategory::Cloud);
        assert!(cloud_pbs.is_empty());
    }

    #[test]
    fn test_view_access() {
        let segment = PlaybookDefinitionSegment::with_builtins();
        let bytes = segment.serialize();

        let data = Arc::new(bytes);
        let view = PlaybookDefinitionView::from_arc(data.clone(), 0, data.len()).unwrap();

        assert_eq!(view.list_ids().len(), 3);

        let web = view.get("thp3-web").unwrap().unwrap();
        assert_eq!(web.name, "THP3 Web Application Testing");
    }
}
