// Unified storage client for writers/readers.
// Provides the same API previously exposed via crate::persistence.

pub mod query;

pub use super::keyring::PasswordSource;
pub use query::QueryManager;

use crate::config;
use crate::storage::keyring::resolve_password;
use crate::storage::records::{
    DnsRecordType, HostIntelRecord, HttpHeadersRecord, ProxyConnectionRecord,
    ProxyHttpRequestRecord, ProxyHttpResponseRecord, ProxyWebSocketRecord, SubdomainSource,
    TlsScanRecord, VulnerabilityRecord,
};
use crate::storage::service::StorageService;
use crate::storage::RedDB; // Use Modern RedDB
use std::fs;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Persistence configuration options
#[derive(Debug, Clone, Default)]
pub struct PersistenceConfig {
    /// Database path (if None, auto-generated from target)
    pub db_path: Option<PathBuf>,
    /// Password for encryption (from flag, env, or keyring)
    pub password: Option<String>,
    /// Force persistence even if auto_persist is disabled
    pub force_save: bool,
}

impl PersistenceConfig {
    /// Create a new config with --save flag
    pub fn with_save() -> Self {
        Self {
            force_save: true,
            ..Default::default()
        }
    }

    /// Set database path
    pub fn with_db_path(mut self, path: PathBuf) -> Self {
        self.db_path = Some(path);
        self
    }

    /// Set password explicitly
    pub fn with_password(mut self, password: String) -> Self {
        self.password = Some(password);
        self
    }
}

/// Action recording configuration options
/// Controls the unified intelligence layer behavior
#[derive(Debug, Clone, Default)]
pub struct ActionConfig {
    /// Enable detailed tracing (timing, request/response capture)
    pub enable_tracing: bool,
    /// Disable persisting actions to the database
    pub disable_storage: bool,
}

impl ActionConfig {
    /// Create config with tracing enabled
    pub fn with_tracing() -> Self {
        Self {
            enable_tracing: true,
            ..Default::default()
        }
    }

    /// Create config with storage disabled
    pub fn without_storage() -> Self {
        Self {
            disable_storage: true,
            ..Default::default()
        }
    }

    /// Check if actions should be stored
    pub fn should_store(&self) -> bool {
        !self.disable_storage
    }

    /// Check if detailed tracing is enabled
    pub fn should_trace(&self) -> bool {
        self.enable_tracing
    }
}

// Import action types for ActionRecorder
use crate::storage::schema::Value;
use crate::storage::segments::actions::{
    ActionRecord, ActionSource, ActionTrace, IntoActionRecord,
};

/// High-level recorder for the unified intelligence layer.
pub struct ActionRecorder {
    source: ActionSource,
    config: ActionConfig,
    db: Option<RedDB>,
    records: Vec<ActionRecord>,
    traces: Vec<ActionTrace>,
}

impl ActionRecorder {
    /// Create a new action recorder with the given source name and config
    pub fn new(source_name: &str, config: ActionConfig) -> Result<Self, String> {
        let db = if config.should_store() {
            let global_config = crate::config::get();
            if global_config.database.auto_persist {
                let path = StorageService::db_path("_actions");
                Some(RedDB::open(&path).map_err(|e| format!("Failed to open database: {}", e))?)
            } else {
                None
            }
        } else {
            None
        };

        Ok(Self {
            source: ActionSource::tool(source_name),
            config,
            db,
            records: Vec::new(),
            traces: Vec::new(),
        })
    }

    /// Create with explicit database path
    pub fn with_db_path(
        source_name: &str,
        config: ActionConfig,
        path: PathBuf,
    ) -> Result<Self, String> {
        let db = if config.should_store() {
            Some(RedDB::open(&path).map_err(|e| format!("Failed to open database: {}", e))?)
        } else {
            None
        };

        Ok(Self {
            source: ActionSource::tool(source_name),
            config,
            db,
            records: Vec::new(),
            traces: Vec::new(),
        })
    }

    /// Record an action from any type that implements IntoActionRecord
    pub fn record<T: IntoActionRecord>(&mut self, result: T) -> Result<(), String> {
        let record = result.into_action_record(self.source.clone());

        if self.config.should_trace() {
            let trace = ActionTrace::new(record.id);
            self.traces.push(trace);
        }

        self.records.push(record);
        Ok(())
    }

    /// Record a raw ActionRecord directly
    pub fn record_raw(&mut self, record: ActionRecord) -> Result<(), String> {
        if self.config.should_trace() {
            let trace = ActionTrace::new(record.id);
            self.traces.push(trace);
        }
        self.records.push(record);
        Ok(())
    }

    /// Get the number of recorded actions
    pub fn count(&self) -> usize {
        self.records.len()
    }

    /// Get all recorded actions
    pub fn actions(&self) -> &[ActionRecord] {
        &self.records
    }

    /// Check if tracing is enabled
    pub fn is_tracing(&self) -> bool {
        self.config.should_trace()
    }

    /// Check if storage is enabled
    pub fn is_storing(&self) -> bool {
        self.db.is_some()
    }

    /// Commit all recorded actions to storage
    pub fn commit(mut self) -> Result<usize, String> {
        let count = self.records.len();

        if let Some(ref mut db) = self.db {
            // Save all actions as nodes
            for record in self.records.drain(..) {
                db.node("actions", "Action")
                    .property("id", Value::Uuid(record.id))
                    .property("timestamp", record.timestamp as i64)
                    .property("source", format!("{:?}", record.source))
                    .property("target", record.target.host_str())
                    .property("type", format!("{:?}", record.action_type))
                    .property("status", format!("{:?}", record.outcome))
                    .property("record", Value::Blob(record.encode()))
                    .save()
                    .map_err(|e| format!("Failed to save action: {}", e))?;
            }

            // Flush to disk
            db.flush()
                .map_err(|e| format!("Failed to flush database: {}", e))?;
        }

        Ok(count)
    }
}

/// Handles persistence for scan results (writer-facing API).
pub struct PersistenceManager {
    db: Option<RedDB>,
    db_path: Option<PathBuf>,
    target: String,
    password_source: PasswordSource,
}

impl PersistenceManager {
    /// Create new persistence manager with an optional persistence override
    pub fn new(target: &str, persist: Option<bool>) -> Result<Self, String> {
        let config = PersistenceConfig {
            force_save: persist.unwrap_or(false),
            ..Default::default()
        };
        Self::with_config(target, config)
    }

    /// Create persistence manager with explicit configuration
    pub fn with_config(target: &str, config: PersistenceConfig) -> Result<Self, String> {
        let global_config = config::get();

        // Determine if we should persist
        let should_persist = config.force_save || global_config.database.auto_persist;

        if !should_persist {
            return Ok(Self {
                db: None,
                db_path: None,
                target: target.to_string(),
                password_source: PasswordSource::None,
            });
        }

        // Determine database path
        let path = match config.db_path {
            Some(p) => p,
            None => Self::get_db_path(target)?,
        };

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create database directory: {}", e))?;
        }

        let password_source = resolve_password(config.password.as_deref());

        // Open database (store handles encryption internally if implemented, or rely on file permissions for now)
        // Note: the store currently uses simple JSON serialization. Encryption layer needs to be ported.
        let db = RedDB::open(&path).map_err(|e| format!("Failed to open database: {}", e))?;

        StorageService::global().ensure_target_partition(target, path.clone(), None, None);

        Ok(Self {
            db: Some(db),
            db_path: Some(path),
            target: target.to_string(),
            password_source,
        })
    }

    /// Get database file path for target
    fn get_db_path(target: &str) -> Result<PathBuf, String> {
        let config = config::get();

        // Base directory
        let base_dir = if let Some(dir) = &config.database.db_dir {
            PathBuf::from(dir)
        } else {
            std::env::current_dir()
                .map_err(|e| format!("Failed to get current directory: {}", e))?
        };

        // File name (using .json for now as it is JSON based)
        let filename = if config.database.auto_name {
            format!("{}.json", sanitize_filename(target))
        } else {
            "scan.json".to_string()
        };

        Ok(base_dir.join(filename))
    }

    /// Check if persistence is enabled
    pub fn is_enabled(&self) -> bool {
        self.db.is_some()
    }

    /// Get database path
    pub fn db_path(&self) -> Option<&PathBuf> {
        self.db_path.as_ref()
    }

    /// Get the password source used
    pub fn password_source(&self) -> &PasswordSource {
        &self.password_source
    }

    /// Check if database is encrypted
    pub fn is_encrypted(&self) -> bool {
        self.password_source.is_encrypted()
    }

    /// Add port scan result
    pub fn add_port_scan(
        &mut self,
        ip: IpAddr,
        port: u16,
        state: u8,
        _service_id: u8,
    ) -> Result<(), String> {
        if let Some(db) = &mut self.db {
            let status = match state {
                0 => "open",
                1 => "closed",
                2 => "filtered",
                3 => "open|filtered",
                _ => "open",
            };
            let timestamp = current_timestamp();
            db.node("ports", "Port")
                .property("ip", ip.to_string())
                .property("port", port as i64)
                .property("state", status)
                .property("service_id", _service_id as i64)
                .property("timestamp", timestamp as i64)
                .save()
                .map_err(|e| format!("Database error: {}", e))?;
        }
        Ok(())
    }

    /// Add DNS record
    pub fn add_dns_record(
        &mut self,
        domain: &str,
        record_type: u16,
        ttl: u32,
        value: &str,
    ) -> Result<(), String> {
        if let Some(db) = &mut self.db {
            if let Some(rt) = map_dns_record_type(record_type) {
                let timestamp = current_timestamp();
                db.node("dns", "Record")
                    .property("domain", domain)
                    .property("type", format!("{:?}", rt))
                    .property("value", value)
                    .property("ttl", ttl as i64)
                    .property("timestamp", timestamp as i64)
                    .save()
                    .map_err(|e| format!("Database error: {}", e))?;
            }
        }
        Ok(())
    }

    /// Add subdomain
    pub fn add_subdomain(
        &mut self,
        parent: &str,
        subdomain: &str,
        status: u8,
        ips: &[IpAddr],
    ) -> Result<(), String> {
        if let Some(db) = &mut self.db {
            let source = map_subdomain_source_id(status);
            let ip_list: Vec<String> = ips.iter().map(|ip| ip.to_string()).collect();
            let timestamp = current_timestamp();

            let mut node = db
                .node("domains", "Domain")
                .property("name", subdomain)
                .property("parent", parent)
                .property("source", format!("{:?}", source))
                .property("timestamp", timestamp as i64);

            if !ip_list.is_empty() {
                node = node.property("ips", ip_list.join(","));
            }

            node.save().map_err(|e| format!("Database error: {}", e))?;
        }
        Ok(())
    }

    /// Add WHOIS data
    pub fn add_whois(
        &mut self,
        domain: &str,
        registrar: &str,
        created: u32,
        expires: u32,
        nameservers: &[String],
    ) -> Result<(), String> {
        if let Some(db) = &mut self.db {
            let timestamp = current_timestamp();
            let nameservers_str = nameservers.join(",");
            db.node("whois", "Whois")
                .property("domain", domain)
                .property("registrar", registrar)
                .property("created", created as i64)
                .property("expires", expires as i64)
                .property("nameservers", nameservers_str)
                .property("timestamp", timestamp as i64)
                .save()
                .map_err(|e| format!("Database error: {}", e))?;
        }
        Ok(())
    }

    /// Add TLS scan result
    pub fn add_tls_scan(&mut self, mut record: TlsScanRecord) -> Result<(), String> {
        if let Some(db) = &mut self.db {
            if record.timestamp == 0 {
                record.timestamp = current_timestamp();
            }
            let mut node = db
                .node("tls", "Certificate")
                .property("host", record.host)
                .property("port", record.port as i64)
                .property("timestamp", record.timestamp as i64)
                .property("certificate_valid", record.certificate_valid);

            if let Some(version) = record.negotiated_version {
                node = node.property("version", version);
            }
            if let Some(cipher) = record.negotiated_cipher {
                node = node.property("cipher", cipher);
            }

            node.save().map_err(|e| format!("Database error: {}", e))?;
        }
        Ok(())
    }

    /// Add HTTP capture
    pub fn add_http_capture(&mut self, mut record: HttpHeadersRecord) -> Result<(), String> {
        if let Some(db) = &mut self.db {
            if record.timestamp == 0 {
                record.timestamp = current_timestamp();
            }

            let headers = if record.headers.is_empty() {
                None
            } else {
                Some(
                    record
                        .headers
                        .iter()
                        .map(|(k, v)| format!("{}: {}", k, v))
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            };

            let mut node = db
                .node("http", "Response")
                .property("host", record.host)
                .property("url", record.url)
                .property("method", record.method)
                .property("scheme", record.scheme)
                .property("http_version", record.http_version)
                .property("status", record.status_code as i64)
                .property("status_text", record.status_text)
                .property("body_size", record.body_size as i64)
                .property("timestamp", record.timestamp as i64);

            if let Some(server) = record.server {
                node = node.property("server", server);
            }
            if let Some(headers) = headers {
                node = node.property("headers", headers);
            }

            node.save().map_err(|e| format!("Database error: {}", e))?;
        }
        Ok(())
    }

    /// Add host fingerprint/intel record
    pub fn add_host_intel(&mut self, record: HostIntelRecord) -> Result<(), String> {
        if let Some(db) = &mut self.db {
            let mut node = db
                .node("hosts", "Host")
                .property("ip", record.ip.to_string())
                .property("confidence", record.confidence as f64)
                .property("last_seen", record.last_seen as i64);

            if let Some(os) = record.os_family {
                node = node.property("os", os);
            }

            node.save().map_err(|e| format!("Database error: {}", e))?;
        }
        Ok(())
    }

    /// Add proxy connection record
    pub fn add_proxy_connection(&mut self, record: ProxyConnectionRecord) -> Result<(), String> {
        if let Some(db) = &mut self.db {
            // Simplified proxy storage for now
            db.node("proxy", "Connection")
                .property("connection_id", record.connection_id as i64)
                .property("client", record.src_ip.to_string())
                .property("client_port", record.src_port as i64)
                .property("target", record.dst_host.clone())
                .property("target_port", record.dst_port as i64)
                .save()
                .map_err(|e| format!("Database error: {}", e))?;
        }
        Ok(())
    }

    /// Add proxy HTTP request record
    pub fn add_proxy_http_request(&mut self, record: ProxyHttpRequestRecord) -> Result<(), String> {
        if let Some(db) = &mut self.db {
            db.node("proxy", "Request")
                .property("connection_id", record.connection_id as i64)
                .property("method", record.method)
                .property("path", record.path)
                .property("host", record.host)
                .save()
                .map_err(|e| format!("Database error: {}", e))?;
        }
        Ok(())
    }

    /// Add proxy HTTP response record
    pub fn add_proxy_http_response(
        &mut self,
        record: ProxyHttpResponseRecord,
    ) -> Result<(), String> {
        if let Some(db) = &mut self.db {
            db.node("proxy", "Response")
                .property("connection_id", record.connection_id as i64)
                .property("request_seq", record.request_seq as i64)
                .property("status", record.status_code as i64)
                .save()
                .map_err(|e| format!("Database error: {}", e))?;
        }
        Ok(())
    }

    /// Add proxy WebSocket frame record
    pub fn add_proxy_websocket(&mut self, record: ProxyWebSocketRecord) -> Result<(), String> {
        if let Some(db) = &mut self.db {
            db.node("proxy", "WebSocket")
                .property("connection_id", record.connection_id as i64)
                .property("direction", format!("{:?}", record.direction))
                .save()
                .map_err(|e| format!("Database error: {}", e))?;
        }
        Ok(())
    }

    /// Add vulnerability record
    pub fn add_vulnerability(&mut self, record: VulnerabilityRecord) -> Result<(), String> {
        if let Some(db) = &mut self.db {
            let mut node = db
                .node("vulns", "Vulnerability")
                .property("cve_id", record.cve_id)
                .property("technology", record.technology)
                .property("cvss", record.cvss as f64)
                .property("risk_score", record.risk_score as i64)
                .property("severity", format!("{:?}", record.severity))
                .property("description", record.description)
                .property("exploit_available", record.exploit_available)
                .property("in_kev", record.in_kev)
                .property("discovered_at", record.discovered_at as i64)
                .property("source", record.source);

            if let Some(version) = record.version {
                node = node.property("version", version);
            }
            if !record.references.is_empty() {
                node = node.property("references", record.references.join("\n"));
            }

            node.save().map_err(|e| format!("Database error: {}", e))?;
        }
        Ok(())
    }

    /// Commit and finalize database
    pub fn commit(self) -> Result<Option<PathBuf>, String> {
        if let Some(db) = self.db {
            db.flush().map_err(|e| format!("Database error: {}", e))?;
            if let Some(path) = &self.db_path {
                let service = StorageService::global();
                let _ = service.refresh_target_partition(&self.target, path);
            }
            Ok(self.db_path)
        } else {
            Ok(None)
        }
    }
}

/// Sanitize filename (remove invalid characters)
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

fn current_timestamp() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| std::time::Duration::from_secs(0))
        .as_secs() as u32
}

fn map_dns_record_type(code: u16) -> Option<DnsRecordType> {
    match code {
        1 => Some(DnsRecordType::A),
        2 => Some(DnsRecordType::NS),
        5 => Some(DnsRecordType::CNAME),
        15 => Some(DnsRecordType::MX),
        16 => Some(DnsRecordType::TXT),
        28 => Some(DnsRecordType::AAAA),
        _ => None,
    }
}

fn map_subdomain_source_id(code: u8) -> SubdomainSource {
    match code {
        0 => SubdomainSource::DnsBruteforce,
        1 => SubdomainSource::CertTransparency,
        2 => SubdomainSource::SearchEngine,
        3 => SubdomainSource::WebCrawl,
        _ => SubdomainSource::SearchEngine,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Mutex to serialize tests that modify global state
    static CLIENT_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_dummy() {
        assert!(true);
    }
}
