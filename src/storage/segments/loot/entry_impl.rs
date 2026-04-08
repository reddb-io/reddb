use super::*;

impl LootEntry {
    /// Create a new loot entry with current timestamp
    pub fn new(key: impl Into<String>, category: LootCategory, content: impl Into<String>) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        Self {
            key: key.into(),
            category,
            content: content.into(),
            confidence: Confidence::Medium,
            status: LootStatus::Open,
            target: None,
            source: None,
            created_at: now,
            updated_at: now,
            metadata: LootMetadata::default(),
        }
    }

    /// Serialize to binary format
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        write_string(&mut buf, &self.key);
        buf.push(self.category as u8);
        write_string(&mut buf, &self.content);
        buf.push(self.confidence as u8);
        buf.push(self.status as u8);

        // Optional target IP
        match &self.target {
            Some(ip) => {
                buf.push(1);
                write_ip(&mut buf, ip);
            }
            None => buf.push(0),
        }

        // Optional source IP
        match &self.source {
            Some(ip) => {
                buf.push(1);
                write_ip(&mut buf, ip);
            }
            None => buf.push(0),
        }

        // Timestamps (i64 as little-endian)
        buf.extend_from_slice(&self.created_at.to_le_bytes());
        buf.extend_from_slice(&self.updated_at.to_le_bytes());

        // Metadata
        let meta_bytes = self.metadata.to_bytes();
        write_varu32(&mut buf, meta_bytes.len() as u32);
        buf.extend_from_slice(&meta_bytes);

        buf
    }

    /// Deserialize from binary format
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut pos = 0usize;

        let key = read_string(bytes, &mut pos)?.to_string();

        if pos >= bytes.len() {
            return Err(DecodeError("truncated loot category"));
        }
        let category =
            LootCategory::from_u8(bytes[pos]).ok_or(DecodeError("invalid loot category"))?;
        pos += 1;

        let content = read_string(bytes, &mut pos)?.to_string();

        if pos >= bytes.len() {
            return Err(DecodeError("truncated confidence"));
        }
        let confidence =
            Confidence::from_u8(bytes[pos]).ok_or(DecodeError("invalid confidence"))?;
        pos += 1;

        if pos >= bytes.len() {
            return Err(DecodeError("truncated status"));
        }
        let status = LootStatus::from_u8(bytes[pos]).ok_or(DecodeError("invalid status"))?;
        pos += 1;

        // Optional target IP
        if pos >= bytes.len() {
            return Err(DecodeError("truncated target flag"));
        }
        let has_target = bytes[pos];
        pos += 1;
        let target = if has_target == 1 {
            Some(read_ip(bytes, &mut pos)?)
        } else {
            None
        };

        // Optional source IP
        if pos >= bytes.len() {
            return Err(DecodeError("truncated source flag"));
        }
        let has_source = bytes[pos];
        pos += 1;
        let source = if has_source == 1 {
            Some(read_ip(bytes, &mut pos)?)
        } else {
            None
        };

        // Timestamps
        if pos + 16 > bytes.len() {
            return Err(DecodeError("truncated timestamps"));
        }
        let created_at = i64::from_le_bytes([
            bytes[pos],
            bytes[pos + 1],
            bytes[pos + 2],
            bytes[pos + 3],
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]);
        pos += 8;
        let updated_at = i64::from_le_bytes([
            bytes[pos],
            bytes[pos + 1],
            bytes[pos + 2],
            bytes[pos + 3],
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]);
        pos += 8;

        // Metadata
        let meta_len = read_varu32(bytes, &mut pos)? as usize;
        if pos + meta_len > bytes.len() {
            return Err(DecodeError("truncated metadata"));
        }
        let metadata = LootMetadata::from_bytes(bytes, &mut pos)?;

        Ok(Self {
            key,
            category,
            content,
            confidence,
            status,
            target,
            source,
            created_at,
            updated_at,
            metadata,
        })
    }

    /// Convert an ActionRecord to a LootEntry if it contains promotable data
    ///
    /// Returns Some(LootEntry) for actions that contain:
    /// - Successful vulnerability scans
    /// - Service discoveries
    /// - Successful exploits
    ///
    /// Returns None for actions that don't warrant loot promotion
    pub fn from_action(action: &ActionRecord) -> Option<Self> {
        // Only promote successful or partial actions
        if !matches!(
            action.outcome,
            ActionOutcome::Success | ActionOutcome::Partial { .. }
        ) {
            return None;
        }

        let target_ip = action.target.ip();
        let target_str = action.target.host_str();

        match (&action.action_type, &action.payload) {
            // Port scans with open ports → Service loot
            (ActionType::Scan, RecordPayload::PortScan(data)) => {
                if data.open_ports.is_empty() {
                    return None;
                }
                let ports: Vec<String> = data.open_ports.iter().map(|p| p.to_string()).collect();
                let key = format!("ports_{}", target_str.replace([':', '/'], "_"));
                let content = format!("Open ports: {}", ports.join(", "));

                let mut entry = Self::new(key, LootCategory::Service, content);
                entry.target = target_ip;
                entry.confidence = Confidence::High;
                entry.metadata.protocol = Some("tcp".to_string());
                Some(entry)
            }

            // Vulnerability scans → Vulnerability loot
            (ActionType::Scan | ActionType::Enumerate, RecordPayload::Vuln(data)) => {
                let key = format!("vuln_{}", target_str.replace([':', '/'], "_"));
                let content = format!("{}: {}", data.title, data.description);

                let mut entry = Self::new(key, LootCategory::Vulnerability, content);
                entry.target = target_ip;

                // Map severity
                entry.confidence = match data.severity {
                    4 => Confidence::High,   // Critical
                    3 => Confidence::High,   // High
                    2 => Confidence::Medium, // Medium
                    _ => Confidence::Low,    // Low/Info
                };

                // Set CVSS based on severity
                entry.metadata.cvss = Some(match data.severity {
                    4 => 9.0, // Critical
                    3 => 7.5, // High
                    2 => 5.0, // Medium
                    1 => 3.0, // Low
                    _ => 0.0, // Info
                });

                if let Some(ref cve) = data.cve {
                    entry.metadata.cve = Some(cve.clone());
                }

                Some(entry)
            }

            // TLS audits → Technology loot
            (ActionType::Scan, RecordPayload::Tls(data)) => {
                let key = format!("tls_{}", target_str.replace([':', '/'], "_"));
                let content = format!("TLS: {} ({})", data.version, data.cipher);

                let mut entry = Self::new(key, LootCategory::Technology, content);
                entry.target = target_ip;
                entry.confidence = Confidence::High;
                entry.metadata.technologies.push(TechInfo {
                    name: "TLS".to_string(),
                    version: Some(data.version.clone()),
                    category: "security".to_string(),
                });
                Some(entry)
            }

            // HTTP responses → Technology/Endpoint loot
            (ActionType::Scan | ActionType::Enumerate, RecordPayload::Http(data)) => {
                // Extract server technology from headers
                let server = data
                    .headers
                    .iter()
                    .find(|(k, _)| k.to_lowercase() == "server")
                    .map(|(_, v)| v.clone());

                let key = format!("http_{}", target_str.replace([':', '/'], "_"));
                let content = if let Some(ref srv) = server {
                    format!("HTTP {} - Server: {}", data.status_code, srv)
                } else {
                    format!("HTTP {}", data.status_code)
                };

                let mut entry = Self::new(key, LootCategory::Endpoint, content);
                entry.target = target_ip;
                entry.confidence = Confidence::High;
                entry.metadata.url = Some(target_str);

                // Store HTTP info in endpoints
                entry.metadata.endpoints.push(EndpointInfo {
                    path: "/".to_string(),
                    method: "GET".to_string(),
                    status_code: Some(data.status_code),
                    content_type: data
                        .headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                        .map(|(_, v)| v.clone()),
                });

                if let Some(srv) = server {
                    entry.metadata.technologies.push(TechInfo {
                        name: srv,
                        version: None,
                        category: "web-server".to_string(),
                    });
                }

                Some(entry)
            }

            // DNS results → Finding loot
            (ActionType::Enumerate, RecordPayload::Dns(data)) => {
                if data.records.is_empty() {
                    return None;
                }

                let key = format!("dns_{}", target_str.replace([':', '/'], "_"));
                let content = format!(
                    "DNS {} records: {}",
                    data.record_type,
                    data.records.join(", ")
                );

                let mut entry = Self::new(key, LootCategory::Finding, content);
                entry.confidence = Confidence::High;
                Some(entry)
            }

            // Fingerprint results → Technology loot
            (ActionType::Scan, RecordPayload::Fingerprint(data)) => {
                let key = format!("fingerprint_{}", target_str.replace([':', '/'], "_"));
                let mut parts = Vec::new();

                parts.push(format!("Service: {}", data.service));
                if let Some(ref ver) = data.version {
                    parts.push(format!("Version: {}", ver));
                }
                if let Some(ref os) = data.os {
                    parts.push(format!("OS: {}", os));
                }
                let content = parts.join(", ");

                let mut entry = Self::new(key, LootCategory::Technology, content);
                entry.target = target_ip;
                entry.confidence = Confidence::High;

                entry.metadata.technologies.push(TechInfo {
                    name: data.service.clone(),
                    version: data.version.clone(),
                    category: "service".to_string(),
                });

                Some(entry)
            }

            // Exploit success → Critical finding
            (ActionType::Exploit, _) => {
                let key = format!("exploit_{}", target_str.replace([':', '/'], "_"));
                let content = format!("Successful exploitation of {}", target_str);

                let mut entry = Self::new(key, LootCategory::Vulnerability, content);
                entry.target = target_ip;
                entry.confidence = Confidence::High;
                entry.status = LootStatus::Confirmed;
                entry.metadata.cvss = Some(9.0); // Exploited = critical
                Some(entry)
            }

            // Other actions don't auto-promote
            _ => None,
        }
    }
}
