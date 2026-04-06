// Session management - Creates and manages .rb-session files
// Each scan creates a {target}.rb-session file in the current directory

use crate::storage::service::{PartitionMetadata, StorageService};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Session file for tracking scan progress
pub struct SessionFile {
    path: PathBuf,
    identifier: String,
    created_at: u64,
}

impl SessionFile {
    pub const EXTENSION: &'static str = ".rb-session";

    /// Compute stable identifier for a given target (matches filename prefix)
    pub fn identifier_for(target: &str) -> String {
        Self::sanitize_identifier(target)
    }

    /// Create or open a session file for the target
    pub fn create(target: &str, command_args: &[String]) -> Result<Self, String> {
        let identifier = Self::sanitize_identifier(target);
        let filename = format!("{}{}", identifier, Self::EXTENSION);
        let path = PathBuf::from(&filename);

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let session = Self {
            path,
            identifier,
            created_at,
        };

        // Write initial session metadata
        session.write_header(target, command_args)?;

        let metadata = PartitionMetadata::new(
            StorageService::key_for_path(&session.path),
            format!("session:{}", target),
            session.path.clone(),
            vec![],
        )
        .with_attribute("category", "session")
        .with_attribute("target", target)
        .with_attribute("command", command_args.join(" "))
        .with_attribute("run_ts", session.created_at.to_string());
        StorageService::global().register_partition(metadata);

        Ok(session)
    }

    /// Write session header with metadata
    fn write_header(&self, target: &str, command_args: &[String]) -> Result<(), String> {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)
            .map_err(|e| format!("Failed to create session file: {}", e))?;

        let header = format!(
            "# redblue session\n\
             created_at = {}\n\
             target = {}\n\
             identifier = {}\n\
             command = rb {}\n\
             \n\
             # Scan results will be appended below\n\
             # Format: timestamp | phase | module | status | data\n\
             \n",
            self.created_at,
            target,
            self.identifier,
            command_args.join(" ")
        );

        file.write_all(header.as_bytes())
            .map_err(|e| format!("Failed to write header: {}", e))?;

        Ok(())
    }

    /// Append a scan result to the session
    pub fn append_result(
        &self,
        phase: &str,
        module: &str,
        status: &str,
        data: &str,
    ) -> Result<(), String> {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("Failed to open session file: {}", e))?;

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let entry = format!(
            "{} | {} | {} | {} | {}\n",
            timestamp, phase, module, status, data
        );

        file.write_all(entry.as_bytes())
            .map_err(|e| format!("Failed to append result: {}", e))?;

        Ok(())
    }

    /// Append section header
    pub fn append_section(&self, section: &str) -> Result<(), String> {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("Failed to open session file: {}", e))?;

        let entry = format!("\n## {}\n", section);

        file.write_all(entry.as_bytes())
            .map_err(|e| format!("Failed to append section: {}", e))?;

        Ok(())
    }

    /// Append raw data
    pub fn append_data(&self, data: &str) -> Result<(), String> {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("Failed to open session file: {}", e))?;

        file.write_all(data.as_bytes())
            .map_err(|e| format!("Failed to append data: {}", e))?;

        Ok(())
    }

    /// Mark session as completed
    pub fn mark_complete(&self, duration_secs: f64) -> Result<(), String> {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("Failed to open session file: {}", e))?;

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let footer = format!(
            "\n# Scan completed\n\
             completed_at = {}\n\
             duration_secs = {:.2}\n",
            timestamp, duration_secs
        );

        file.write_all(footer.as_bytes())
            .map_err(|e| format!("Failed to write footer: {}", e))?;

        Ok(())
    }

    /// Get session file path
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Get identifier
    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    /// Sanitize target to create valid filename
    fn sanitize_identifier(target: &str) -> String {
        let trimmed = target.trim();

        // Remove protocol
        let without_protocol = if let Some(idx) = trimmed.find("://") {
            &trimmed[idx + 3..]
        } else {
            trimmed
        };

        // Remove user info (user@host)
        let without_user = without_protocol
            .split('@')
            .last()
            .unwrap_or(without_protocol);

        // Get just the host part (before /, ?, #)
        let base = without_user
            .trim_start_matches('/')
            .split(|c| matches!(c, '/' | '?' | '#'))
            .next()
            .unwrap_or(without_user);

        // Remove port
        let host = base.split(':').next().unwrap_or(base);

        // Remove IPv6 brackets
        let host = host.trim_matches(|c| matches!(c, '[' | ']'));

        // Sanitize to valid filename
        let mut sanitized = String::with_capacity(host.len());
        for ch in host.chars() {
            let mapped = match ch {
                'a'..='z' | '0'..='9' | '.' | '-' | '_' => ch,
                'A'..='Z' => ch.to_ascii_lowercase(),
                _ => '_',
            };
            sanitized.push(mapped);
        }

        if sanitized.is_empty() {
            "unknown".to_string()
        } else {
            sanitized
        }
    }

    /// Check if session file already exists
    pub fn exists(target: &str) -> bool {
        let identifier = Self::sanitize_identifier(target);
        let filename = format!("{}{}", identifier, Self::EXTENSION);
        PathBuf::from(&filename).exists()
    }

    /// Load existing session metadata
    pub fn load_metadata(target: &str) -> Result<SessionMetadata, String> {
        let identifier = Self::sanitize_identifier(target);
        let filename = format!("{}{}", identifier, Self::EXTENSION);
        let path = PathBuf::from(&filename);

        if !path.exists() {
            return Err("Session file does not exist".to_string());
        }

        let content =
            fs::read_to_string(&path).map_err(|e| format!("Failed to read session file: {}", e))?;

        Self::parse_metadata(&content)
    }

    /// Parse session metadata from file content
    pub fn parse_metadata(content: &str) -> Result<SessionMetadata, String> {
        let mut metadata = SessionMetadata {
            created_at: 0,
            completed_at: None,
            target: String::new(),
            identifier: String::new(),
            command: String::new(),
            duration_secs: None,
        };

        for line in content.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }

            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim();

                match key {
                    "created_at" => {
                        metadata.created_at = value.parse().unwrap_or(0);
                    }
                    "completed_at" => {
                        metadata.completed_at = value.parse().ok();
                    }
                    "target" => {
                        metadata.target = value.to_string();
                    }
                    "identifier" => {
                        metadata.identifier = value.to_string();
                    }
                    "command" => {
                        metadata.command = value.to_string();
                    }
                    "duration_secs" => {
                        metadata.duration_secs = value.parse().ok();
                    }
                    _ => {}
                }
            }
        }

        Ok(metadata)
    }
}

/// Session metadata
#[derive(Debug, Clone)]
pub struct SessionMetadata {
    pub created_at: u64,
    pub completed_at: Option<u64>,
    pub target: String,
    pub identifier: String,
    pub command: String,
    pub duration_secs: Option<f64>,
}

impl SessionMetadata {
    /// Check if session is complete
    pub fn is_complete(&self) -> bool {
        self.completed_at.is_some()
    }

    /// Get age in seconds
    pub fn age_secs(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        now - self.created_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_identifier() {
        assert_eq!(
            SessionFile::sanitize_identifier("https://example.com"),
            "example.com"
        );
        assert_eq!(
            SessionFile::sanitize_identifier("http://www.example.com:8080/path"),
            "www.example.com"
        );
        assert_eq!(
            SessionFile::sanitize_identifier("192.168.1.1"),
            "192.168.1.1"
        );
        assert_eq!(
            SessionFile::sanitize_identifier("user@host.com"),
            "host.com"
        );
        assert_eq!(
            SessionFile::sanitize_identifier("Example.COM"),
            "example.com"
        );
    }

    #[test]
    fn test_session_creation() {
        let session = SessionFile::create("example.com", &["example.com".to_string()]).unwrap();

        assert_eq!(session.identifier(), "example.com");
        assert!(session.path().exists());

        // Cleanup
        let _ = fs::remove_file(session.path());
    }

    #[test]
    fn test_append_result() {
        let session = SessionFile::create("test.com", &["test.com".to_string()]).unwrap();

        session
            .append_result("passive", "dns", "success", "Found 5 records")
            .unwrap();

        let content = fs::read_to_string(session.path()).unwrap();
        assert!(content.contains("passive"));
        assert!(content.contains("dns"));
        assert!(content.contains("Found 5 records"));

        // Cleanup
        let _ = fs::remove_file(session.path());
    }

    #[test]
    fn test_mark_complete() {
        let session = SessionFile::create("test2.com", &["test2.com".to_string()]).unwrap();

        session.mark_complete(12.34).unwrap();

        let content = fs::read_to_string(session.path()).unwrap();
        assert!(content.contains("completed_at"));
        assert!(content.contains("12.34"));

        // Cleanup
        let _ = fs::remove_file(session.path());
    }

    #[test]
    fn test_load_metadata() {
        let session =
            SessionFile::create("metadata-test.com", &["metadata-test.com".to_string()]).unwrap();

        let metadata = SessionFile::load_metadata("metadata-test.com").unwrap();
        assert_eq!(metadata.target, "metadata-test.com");
        assert!(!metadata.is_complete());

        session.mark_complete(5.0).unwrap();

        let metadata = SessionFile::load_metadata("metadata-test.com").unwrap();
        assert!(metadata.is_complete());
        assert_eq!(metadata.duration_secs, Some(5.0));

        // Cleanup
        let _ = fs::remove_file(session.path());
    }
}
