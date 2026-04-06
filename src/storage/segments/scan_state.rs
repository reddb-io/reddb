//! Scan State Segment - Checkpoint and resume capability for scans.
//!
//! Stores scan progress, configuration, and state for resuming interrupted scans.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::storage::primitives::encoding::{
    read_string, read_varu32, read_varu64, write_string, write_varu32, write_varu64, DecodeError,
};

static SCAN_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

// ==================== Scan Type ====================

/// Type of scan being performed
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanType {
    /// Web directory fuzzing
    DirectoryFuzz = 0,
    /// File fuzzing
    FileFuzz = 1,
    /// Parameter fuzzing
    ParameterFuzz = 2,
    /// Subdomain enumeration
    SubdomainEnum = 3,
    /// Recursive discovery
    Recursive = 4,
    /// Password brute force
    PasswordBrute = 5,
    /// Custom scan type
    Custom = 255,
}

impl ScanType {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::DirectoryFuzz,
            1 => Self::FileFuzz,
            2 => Self::ParameterFuzz,
            3 => Self::SubdomainEnum,
            4 => Self::Recursive,
            5 => Self::PasswordBrute,
            _ => Self::Custom,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DirectoryFuzz => "directory_fuzz",
            Self::FileFuzz => "file_fuzz",
            Self::ParameterFuzz => "parameter_fuzz",
            Self::SubdomainEnum => "subdomain_enum",
            Self::Recursive => "recursive",
            Self::PasswordBrute => "password_brute",
            Self::Custom => "custom",
        }
    }
}

// ==================== Scan Status ====================

/// Current status of a scan
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanStatus {
    /// Scan is running
    Running = 0,
    /// Scan is paused (can resume)
    Paused = 1,
    /// Scan completed successfully
    Completed = 2,
    /// Scan failed with error
    Failed = 3,
    /// Scan was cancelled
    Cancelled = 4,
}

impl ScanStatus {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Running,
            1 => Self::Paused,
            2 => Self::Completed,
            3 => Self::Failed,
            4 => Self::Cancelled,
            _ => Self::Failed,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Can this scan be resumed?
    pub fn is_resumable(&self) -> bool {
        matches!(self, Self::Paused | Self::Running)
    }

    /// Is this a terminal state?
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

// ==================== Scan State ====================

/// Complete state of a scan for checkpointing
#[derive(Debug, Clone)]
pub struct ScanState {
    /// Unique scan identifier
    pub scan_id: String,
    /// Type of scan
    pub scan_type: ScanType,
    /// Target URL/domain
    pub target: String,
    /// Wordlist path used
    pub wordlist_path: String,
    /// Current position in wordlist
    pub wordlist_position: usize,
    /// Total words in wordlist
    pub wordlist_total: usize,
    /// Current status
    pub status: ScanStatus,
    /// Error message (if failed)
    pub error_message: Option<String>,
    /// Discovered URLs/paths
    pub discovered: Vec<String>,
    /// URLs pending in recursion queue
    pub pending_urls: Vec<String>,
    /// Current recursion depth
    pub current_depth: u32,
    /// Maximum recursion depth
    pub max_depth: u32,
    /// Request count
    pub request_count: u64,
    /// Error count
    pub error_count: u32,
    /// Filtered count (soft-404, duplicates, etc.)
    pub filtered_count: u64,
    /// Started at (unix ms)
    pub started_at: u64,
    /// Last checkpoint at (unix ms)
    pub checkpoint_at: u64,
    /// Completed at (unix ms, 0 if not completed)
    pub completed_at: u64,
    /// Simhash baselines for similarity filter (hash -> url)
    pub simhash_baselines: HashMap<u64, String>,
    /// Learned words during this scan
    pub learned_word_count: u32,
    /// Additional metadata (JSON-serializable config)
    pub metadata: String,
}

impl ScanState {
    /// Create a new scan state
    pub fn new(scan_id: String, scan_type: ScanType, target: String) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            scan_id,
            scan_type,
            target,
            wordlist_path: String::new(),
            wordlist_position: 0,
            wordlist_total: 0,
            status: ScanStatus::Running,
            error_message: None,
            discovered: Vec::new(),
            pending_urls: Vec::new(),
            current_depth: 0,
            max_depth: 0,
            request_count: 0,
            error_count: 0,
            filtered_count: 0,
            started_at: now,
            checkpoint_at: now,
            completed_at: 0,
            simhash_baselines: HashMap::new(),
            learned_word_count: 0,
            metadata: String::new(),
        }
    }

    /// Generate unique scan ID
    pub fn generate_id() -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let counter = SCAN_ID_COUNTER.fetch_add(1, Ordering::Relaxed) & 0xFFFFFF;
        format!("{:x}{:06x}", timestamp, counter)
    }

    /// Update checkpoint timestamp
    pub fn checkpoint(&mut self) {
        self.checkpoint_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
    }

    /// Mark scan as completed
    pub fn complete(&mut self) {
        self.status = ScanStatus::Completed;
        self.completed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
    }

    /// Mark scan as failed
    pub fn fail(&mut self, error: impl Into<String>) {
        self.status = ScanStatus::Failed;
        self.error_message = Some(error.into());
        self.completed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
    }

    /// Mark scan as paused
    pub fn pause(&mut self) {
        self.status = ScanStatus::Paused;
        self.checkpoint();
    }

    /// Mark scan as cancelled
    pub fn cancel(&mut self) {
        self.status = ScanStatus::Cancelled;
        self.completed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
    }

    /// Resume paused scan
    pub fn resume(&mut self) {
        if self.status == ScanStatus::Paused {
            self.status = ScanStatus::Running;
        }
    }

    /// Get progress percentage
    pub fn progress_percent(&self) -> f64 {
        if self.wordlist_total == 0 {
            return 0.0;
        }
        (self.wordlist_position as f64 / self.wordlist_total as f64) * 100.0
    }

    /// Get elapsed time in seconds
    pub fn elapsed_secs(&self) -> u64 {
        let end = if self.completed_at > 0 {
            self.completed_at
        } else {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64
        };
        (end.saturating_sub(self.started_at)) / 1000
    }

    /// Get requests per second
    pub fn requests_per_second(&self) -> f64 {
        let elapsed = self.elapsed_secs();
        if elapsed == 0 {
            return 0.0;
        }
        self.request_count as f64 / elapsed as f64
    }

    fn encode(&self, buf: &mut Vec<u8>) {
        write_string(buf, &self.scan_id);
        buf.push(self.scan_type as u8);
        write_string(buf, &self.target);
        write_string(buf, &self.wordlist_path);
        write_varu64(buf, self.wordlist_position as u64);
        write_varu64(buf, self.wordlist_total as u64);
        buf.push(self.status as u8);
        write_string(buf, self.error_message.as_deref().unwrap_or(""));

        // Discovered URLs
        write_varu32(buf, self.discovered.len() as u32);
        for url in &self.discovered {
            write_string(buf, url);
        }

        // Pending URLs
        write_varu32(buf, self.pending_urls.len() as u32);
        for url in &self.pending_urls {
            write_string(buf, url);
        }

        write_varu32(buf, self.current_depth);
        write_varu32(buf, self.max_depth);
        write_varu64(buf, self.request_count);
        write_varu32(buf, self.error_count);
        write_varu64(buf, self.filtered_count);
        write_varu64(buf, self.started_at);
        write_varu64(buf, self.checkpoint_at);
        write_varu64(buf, self.completed_at);

        // Simhash baselines
        write_varu32(buf, self.simhash_baselines.len() as u32);
        for (hash, url) in &self.simhash_baselines {
            write_varu64(buf, *hash);
            write_string(buf, url);
        }

        write_varu32(buf, self.learned_word_count);
        write_string(buf, &self.metadata);
    }

    fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let scan_id = read_string(bytes, pos)?.to_string();

        if *pos >= bytes.len() {
            return Err(DecodeError("truncated scan state"));
        }
        let scan_type = ScanType::from_u8(bytes[*pos]);
        *pos += 1;

        let target = read_string(bytes, pos)?.to_string();
        let wordlist_path = read_string(bytes, pos)?.to_string();
        let wordlist_position = read_varu64(bytes, pos)? as usize;
        let wordlist_total = read_varu64(bytes, pos)? as usize;

        if *pos >= bytes.len() {
            return Err(DecodeError("truncated scan state status"));
        }
        let status = ScanStatus::from_u8(bytes[*pos]);
        *pos += 1;

        let error_str = read_string(bytes, pos)?;
        let error_message = if error_str.is_empty() {
            None
        } else {
            Some(error_str.to_string())
        };

        // Discovered URLs
        let discovered_count = read_varu32(bytes, pos)? as usize;
        let mut discovered = Vec::with_capacity(discovered_count);
        for _ in 0..discovered_count {
            discovered.push(read_string(bytes, pos)?.to_string());
        }

        // Pending URLs
        let pending_count = read_varu32(bytes, pos)? as usize;
        let mut pending_urls = Vec::with_capacity(pending_count);
        for _ in 0..pending_count {
            pending_urls.push(read_string(bytes, pos)?.to_string());
        }

        let current_depth = read_varu32(bytes, pos)?;
        let max_depth = read_varu32(bytes, pos)?;
        let request_count = read_varu64(bytes, pos)?;
        let error_count = read_varu32(bytes, pos)?;
        let filtered_count = read_varu64(bytes, pos)?;
        let started_at = read_varu64(bytes, pos)?;
        let checkpoint_at = read_varu64(bytes, pos)?;
        let completed_at = read_varu64(bytes, pos)?;

        // Simhash baselines
        let baseline_count = read_varu32(bytes, pos)? as usize;
        let mut simhash_baselines = HashMap::with_capacity(baseline_count);
        for _ in 0..baseline_count {
            let hash = read_varu64(bytes, pos)?;
            let url = read_string(bytes, pos)?.to_string();
            simhash_baselines.insert(hash, url);
        }

        let learned_word_count = read_varu32(bytes, pos)?;
        let metadata = read_string(bytes, pos)?.to_string();

        Ok(Self {
            scan_id,
            scan_type,
            target,
            wordlist_path,
            wordlist_position,
            wordlist_total,
            status,
            error_message,
            discovered,
            pending_urls,
            current_depth,
            max_depth,
            request_count,
            error_count,
            filtered_count,
            started_at,
            checkpoint_at,
            completed_at,
            simhash_baselines,
            learned_word_count,
            metadata,
        })
    }
}

// ==================== Scan State Summary ====================

/// Summary of a scan state (for listing)
#[derive(Debug, Clone)]
pub struct ScanStateSummary {
    pub scan_id: String,
    pub scan_type: ScanType,
    pub target: String,
    pub status: ScanStatus,
    pub progress_percent: f64,
    pub discovered_count: usize,
    pub started_at: u64,
    pub checkpoint_at: u64,
}

impl From<&ScanState> for ScanStateSummary {
    fn from(state: &ScanState) -> Self {
        Self {
            scan_id: state.scan_id.clone(),
            scan_type: state.scan_type,
            target: state.target.clone(),
            status: state.status,
            progress_percent: state.progress_percent(),
            discovered_count: state.discovered.len(),
            started_at: state.started_at,
            checkpoint_at: state.checkpoint_at,
        }
    }
}

// ==================== Scan State Segment ====================

/// Storage segment for scan states
#[derive(Debug, Clone, Default)]
pub struct ScanStateSegment {
    /// All scan states by ID
    states: HashMap<String, ScanState>,
}

impl ScanStateSegment {
    pub fn new() -> Self {
        Self {
            states: HashMap::new(),
        }
    }

    /// Save/update scan state
    pub fn save(&mut self, state: ScanState) {
        self.states.insert(state.scan_id.clone(), state);
    }

    /// Get scan state by ID
    pub fn get(&self, scan_id: &str) -> Option<&ScanState> {
        self.states.get(scan_id)
    }

    /// Get mutable scan state
    pub fn get_mut(&mut self, scan_id: &str) -> Option<&mut ScanState> {
        self.states.get_mut(scan_id)
    }

    /// Remove scan state
    pub fn remove(&mut self, scan_id: &str) -> Option<ScanState> {
        self.states.remove(scan_id)
    }

    /// List all scan IDs
    pub fn list_ids(&self) -> Vec<&str> {
        self.states.keys().map(|s| s.as_str()).collect()
    }

    /// List resumable scans
    pub fn list_resumable(&self) -> Vec<ScanStateSummary> {
        self.states
            .values()
            .filter(|s| s.status.is_resumable())
            .map(ScanStateSummary::from)
            .collect()
    }

    /// List recent scans
    pub fn list_recent(&self, limit: usize) -> Vec<ScanStateSummary> {
        let mut summaries: Vec<_> = self.states.values().map(ScanStateSummary::from).collect();
        summaries.sort_by(|a, b| b.checkpoint_at.cmp(&a.checkpoint_at));
        summaries.truncate(limit);
        summaries
    }

    /// Clean up old completed scans
    pub fn cleanup_old(&mut self, max_age_ms: u64) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let cutoff = now.saturating_sub(max_age_ms);

        self.states.retain(|_, state| {
            // Keep non-terminal states
            if !state.status.is_terminal() {
                return true;
            }
            // Keep recent terminal states
            state.completed_at > cutoff
        });
    }

    /// Serialize segment
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Version
        buf.push(1);

        // State count
        write_varu32(&mut buf, self.states.len() as u32);

        for state in self.states.values() {
            state.encode(&mut buf);
        }

        buf
    }

    /// Deserialize segment
    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.is_empty() {
            return Err(DecodeError("empty scan state segment"));
        }

        let mut pos = 0usize;

        // Version
        let version = bytes[pos];
        pos += 1;
        if version != 1 {
            return Err(DecodeError("unsupported scan state version"));
        }

        // State count
        let count = read_varu32(bytes, &mut pos)? as usize;
        let mut states = HashMap::with_capacity(count);

        for _ in 0..count {
            let state = ScanState::decode(bytes, &mut pos)?;
            states.insert(state.scan_id.clone(), state);
        }

        Ok(Self { states })
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scan_state_new() {
        let state = ScanState::new(
            "test123".to_string(),
            ScanType::DirectoryFuzz,
            "http://example.com".to_string(),
        );

        assert_eq!(state.scan_id, "test123");
        assert_eq!(state.status, ScanStatus::Running);
        assert!(state.started_at > 0);
    }

    #[test]
    fn test_scan_state_lifecycle() {
        let mut state = ScanState::new(
            "test".to_string(),
            ScanType::DirectoryFuzz,
            "http://example.com".to_string(),
        );

        assert_eq!(state.status, ScanStatus::Running);

        state.pause();
        assert_eq!(state.status, ScanStatus::Paused);
        assert!(state.status.is_resumable());

        state.resume();
        assert_eq!(state.status, ScanStatus::Running);

        state.complete();
        assert_eq!(state.status, ScanStatus::Completed);
        assert!(state.status.is_terminal());
    }

    #[test]
    fn test_scan_progress() {
        let mut state = ScanState::new(
            "test".to_string(),
            ScanType::DirectoryFuzz,
            "http://example.com".to_string(),
        );

        state.wordlist_total = 1000;
        state.wordlist_position = 250;

        assert_eq!(state.progress_percent(), 25.0);
    }

    #[test]
    fn test_generate_id() {
        let id1 = ScanState::generate_id();
        let id2 = ScanState::generate_id();

        assert!(!id1.is_empty());
        assert_ne!(id1, id2); // Should be unique
    }

    #[test]
    fn test_scan_state_segment() {
        let mut segment = ScanStateSegment::new();

        let mut state = ScanState::new(
            "scan1".to_string(),
            ScanType::DirectoryFuzz,
            "http://example.com".to_string(),
        );
        state.wordlist_total = 1000;
        state.wordlist_position = 500;
        state.discovered.push("/admin".to_string());

        segment.save(state);

        let retrieved = segment.get("scan1").unwrap();
        assert_eq!(retrieved.wordlist_position, 500);
        assert_eq!(retrieved.discovered.len(), 1);
    }

    #[test]
    fn test_list_resumable() {
        let mut segment = ScanStateSegment::new();

        let mut running = ScanState::new(
            "running".to_string(),
            ScanType::DirectoryFuzz,
            "http://a.com".to_string(),
        );
        let mut paused = ScanState::new(
            "paused".to_string(),
            ScanType::DirectoryFuzz,
            "http://b.com".to_string(),
        );
        paused.pause();

        let mut completed = ScanState::new(
            "completed".to_string(),
            ScanType::DirectoryFuzz,
            "http://c.com".to_string(),
        );
        completed.complete();

        segment.save(running);
        segment.save(paused);
        segment.save(completed);

        let resumable = segment.list_resumable();
        assert_eq!(resumable.len(), 2); // running + paused
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mut segment = ScanStateSegment::new();

        let mut state = ScanState::new(
            "test123".to_string(),
            ScanType::Recursive,
            "http://example.com".to_string(),
        );
        state.wordlist_path = "/path/to/wordlist.txt".to_string();
        state.wordlist_total = 5000;
        state.wordlist_position = 2500;
        state.discovered.push("/admin".to_string());
        state.discovered.push("/backup".to_string());
        state.pending_urls.push("/admin/users".to_string());
        state
            .simhash_baselines
            .insert(123456789, "/404".to_string());
        state.request_count = 2500;
        state.error_count = 5;

        segment.save(state);

        let bytes = segment.serialize();
        let restored = ScanStateSegment::deserialize(&bytes).unwrap();

        let restored_state = restored.get("test123").unwrap();
        assert_eq!(restored_state.wordlist_position, 2500);
        assert_eq!(restored_state.discovered.len(), 2);
        assert_eq!(restored_state.pending_urls.len(), 1);
        assert_eq!(restored_state.simhash_baselines.len(), 1);
        assert_eq!(restored_state.request_count, 2500);
    }
}
