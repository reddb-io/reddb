//! Graph Spill Manager
//!
//! Manages memory limits for large graphs by spilling cold data to disk.
//! Provides transparent access to spilled data by reloading on demand.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │                   SpillManager                          │
//! ├─────────────────────────────────────────────────────────┤
//! │  ┌──────────┐   ┌──────────┐   ┌──────────────────┐    │
//! │  │MemTracker│   │AccessLog │   │  SpillDirectory  │    │
//! │  │(current) │   │(LRU/LFU) │   │  (temp files)    │    │
//! │  └────┬─────┘   └────┬─────┘   └────────┬─────────┘    │
//! │       │              │                   │              │
//! │  ┌────▼──────────────▼───────────────────▼─────────┐   │
//! │  │              Spill Policy Engine                 │   │
//! │  │  - Threshold detection (80% memory limit)        │   │
//! │  │  - Cold segment identification (LRU + freq)      │   │
//! │  │  - Async spill/reload operations                 │   │
//! │  └──────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```ignore
//! use storage::cache::spill::{SpillManager, SpillConfig};
//!
//! let config = SpillConfig::new()
//!     .max_memory(512 * 1024 * 1024)  // 512MB
//!     .spill_threshold(0.8)            // 80%
//!     .spill_dir("/tmp/reddb-spill");
//!
//! let mut manager = SpillManager::new(config);
//!
//! // Register a graph segment
//! manager.register_segment("hosts", 50_000_000);  // 50MB
//!
//! // Track access
//! manager.access("hosts");
//!
//! // Check if spill needed
//! if let Some(segments) = manager.needs_spill() {
//!     for seg in segments {
//!         let data = get_segment_data(&seg);
//!         manager.spill(&seg, &data)?;
//!     }
//! }
//!
//! // Reload spilled data
//! if let Some(data) = manager.reload("hosts")? {
//!     restore_segment("hosts", &data);
//! }
//! ```

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Instant;

fn recover_read_guard<'a, T>(lock: &'a RwLock<T>) -> RwLockReadGuard<'a, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn recover_write_guard<'a, T>(lock: &'a RwLock<T>) -> RwLockWriteGuard<'a, T> {
    match lock.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn spill_lock_error(context: &'static str) -> SpillError {
    SpillError::Io(io::Error::other(format!("{context} lock poisoned")))
}

fn read_guard_or_err<'a, T>(
    lock: &'a RwLock<T>,
    context: &'static str,
) -> Result<RwLockReadGuard<'a, T>, SpillError> {
    lock.read().map_err(|_| spill_lock_error(context))
}

fn write_guard_or_err<'a, T>(
    lock: &'a RwLock<T>,
    context: &'static str,
) -> Result<RwLockWriteGuard<'a, T>, SpillError> {
    lock.write().map_err(|_| spill_lock_error(context))
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the spill manager
#[derive(Debug, Clone)]
pub struct SpillConfig {
    /// Maximum memory limit in bytes
    pub max_memory: usize,
    /// Threshold (0.0-1.0) at which to start spilling
    pub spill_threshold: f64,
    /// Directory for spill files
    pub spill_dir: PathBuf,
    /// Target memory after spill (0.0-1.0)
    pub target_after_spill: f64,
    /// Minimum segment size to consider for spilling (bytes)
    pub min_spill_size: usize,
    /// Access weight decay factor (0.0-1.0)
    pub access_decay: f64,
}

impl SpillConfig {
    /// Create a new config with reasonable defaults
    pub fn new() -> Self {
        Self {
            max_memory: 512 * 1024 * 1024, // 512MB default
            spill_threshold: 0.80,         // Spill at 80%
            spill_dir: std::env::temp_dir().join("reddb-spill"),
            target_after_spill: 0.60,    // Target 60% after spill
            min_spill_size: 1024 * 1024, // 1MB minimum
            access_decay: 0.95,          // 5% decay per check cycle
        }
    }

    /// Set maximum memory
    pub fn max_memory(mut self, bytes: usize) -> Self {
        self.max_memory = bytes;
        self
    }

    /// Set spill threshold
    pub fn spill_threshold(mut self, threshold: f64) -> Self {
        self.spill_threshold = threshold.clamp(0.1, 0.99);
        self
    }

    /// Set spill directory
    pub fn spill_dir<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.spill_dir = path.as_ref().to_path_buf();
        self
    }

    /// Set target memory after spill
    pub fn target_after_spill(mut self, target: f64) -> Self {
        self.target_after_spill = target.clamp(0.1, 0.9);
        self
    }

    /// Set minimum spill size
    pub fn min_spill_size(mut self, size: usize) -> Self {
        self.min_spill_size = size;
        self
    }
}

impl Default for SpillConfig {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Segment Tracking
// ============================================================================

/// Information about a tracked memory segment
#[derive(Debug)]
struct SegmentInfo {
    /// Segment name/identifier
    name: String,
    /// Current size in bytes
    size: AtomicUsize,
    /// Access count (weighted by recency)
    access_score: AtomicU64,
    /// Raw access count
    access_count: AtomicU64,
    /// Last access time
    last_access: RwLock<Instant>,
    /// Whether segment is currently spilled
    is_spilled: RwLock<bool>,
    /// Spill file path (if spilled)
    spill_path: RwLock<Option<PathBuf>>,
}

impl SegmentInfo {
    fn new(name: String, size: usize) -> Self {
        Self {
            name,
            size: AtomicUsize::new(size),
            access_score: AtomicU64::new(100), // Initial score
            access_count: AtomicU64::new(0),
            last_access: RwLock::new(Instant::now()),
            is_spilled: RwLock::new(false),
            spill_path: RwLock::new(None),
        }
    }

    fn touch(&self) {
        self.access_count.fetch_add(1, Ordering::Relaxed);
        // Boost score on access
        self.access_score.fetch_add(10, Ordering::Relaxed);
        *recover_write_guard(&self.last_access) = Instant::now();
    }

    fn decay_score(&self, factor: f64) {
        let current = self.access_score.load(Ordering::Relaxed);
        let new = (current as f64 * factor) as u64;
        self.access_score.store(new.max(1), Ordering::Relaxed);
    }

    fn coldness_score(&self) -> u64 {
        // Lower score = colder (more likely to spill)
        // Invert access score and factor in size
        let access = self.access_score.load(Ordering::Relaxed).max(1);
        let size = self.size.load(Ordering::Relaxed) as u64;

        // Larger segments with low access are coldest
        size / access
    }
}

// ============================================================================
// Spill Statistics
// ============================================================================

/// Statistics about spill operations
#[derive(Debug, Clone, Default)]
pub struct SpillStats {
    /// Current memory usage in bytes
    pub current_memory: usize,
    /// Maximum memory limit
    pub max_memory: usize,
    /// Number of segments tracked
    pub segment_count: usize,
    /// Number of segments currently spilled
    pub spilled_count: usize,
    /// Total bytes spilled to disk
    pub bytes_spilled: u64,
    /// Total bytes reloaded from disk
    pub bytes_reloaded: u64,
    /// Number of spill operations
    pub spill_operations: u64,
    /// Number of reload operations
    pub reload_operations: u64,
    /// Total spill file size on disk
    pub disk_usage: u64,
}

impl SpillStats {
    /// Calculate memory utilization (0.0-1.0)
    pub fn utilization(&self) -> f64 {
        if self.max_memory == 0 {
            0.0
        } else {
            self.current_memory as f64 / self.max_memory as f64
        }
    }

    /// Check if at spill threshold
    pub fn at_threshold(&self, threshold: f64) -> bool {
        self.utilization() >= threshold
    }
}

// ============================================================================
// Spill Manager
// ============================================================================

/// Error types for spill operations
#[derive(Debug)]
pub enum SpillError {
    /// IO error during spill/reload
    Io(io::Error),
    /// Segment not found
    SegmentNotFound(String),
    /// Segment not spilled (for reload)
    NotSpilled(String),
    /// Segment already spilled
    AlreadySpilled(String),
    /// Directory creation failed
    DirectoryCreation(io::Error),
    /// Invalid checksum on reload
    ChecksumMismatch,
}

impl std::fmt::Display for SpillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {}", e),
            Self::SegmentNotFound(s) => write!(f, "Segment not found: {}", s),
            Self::NotSpilled(s) => write!(f, "Segment not spilled: {}", s),
            Self::AlreadySpilled(s) => write!(f, "Segment already spilled: {}", s),
            Self::DirectoryCreation(e) => write!(f, "Failed to create spill dir: {}", e),
            Self::ChecksumMismatch => write!(f, "Checksum mismatch on reload"),
        }
    }
}

impl std::error::Error for SpillError {}

impl From<io::Error> for SpillError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Manages memory limits by spilling cold data to disk
pub struct SpillManager {
    /// Configuration
    config: SpillConfig,
    /// Tracked segments
    segments: RwLock<HashMap<String, SegmentInfo>>,
    /// Current total memory usage
    current_memory: AtomicUsize,
    /// Statistics
    stats: RwLock<SpillStats>,
    /// Access history for LRU ordering
    access_history: RwLock<VecDeque<String>>,
    /// Spilled segment names
    spilled_segments: RwLock<HashSet<String>>,
}

impl SpillManager {
    /// Create a new spill manager
    pub fn new(config: SpillConfig) -> Self {
        let max_memory = config.max_memory;

        Self {
            config,
            segments: RwLock::new(HashMap::new()),
            current_memory: AtomicUsize::new(0),
            stats: RwLock::new(SpillStats {
                max_memory,
                ..Default::default()
            }),
            access_history: RwLock::new(VecDeque::with_capacity(1000)),
            spilled_segments: RwLock::new(HashSet::new()),
        }
    }

    /// Ensure spill directory exists
    fn ensure_spill_dir(&self) -> Result<(), SpillError> {
        if !self.config.spill_dir.exists() {
            fs::create_dir_all(&self.config.spill_dir).map_err(SpillError::DirectoryCreation)?;
        }
        Ok(())
    }

    /// Register a memory segment for tracking
    pub fn register_segment(&self, name: &str, size: usize) {
        let info = SegmentInfo::new(name.to_string(), size);

        {
            let mut segments = recover_write_guard(&self.segments);
            // If replacing, subtract old size
            if let Some(old) = segments.get(name) {
                let old_size = old.size.load(Ordering::Relaxed);
                self.current_memory.fetch_sub(old_size, Ordering::Relaxed);
            }

            segments.insert(name.to_string(), info);
            self.current_memory.fetch_add(size, Ordering::Relaxed);
        }

        self.update_stats();
    }

    /// Unregister a segment
    pub fn unregister_segment(&self, name: &str) {
        {
            let mut segments = recover_write_guard(&self.segments);
            if let Some(info) = segments.remove(name) {
                let size = info.size.load(Ordering::Relaxed);
                self.current_memory.fetch_sub(size, Ordering::Relaxed);

                // Clean up spill file if exists
                let path = recover_read_guard(&info.spill_path);
                if let Some(p) = path.as_ref() {
                    let _ = fs::remove_file(p);
                }
            }
        }

        recover_write_guard(&self.spilled_segments).remove(name);

        self.update_stats();
    }

    /// Update segment size
    pub fn update_size(&self, name: &str, new_size: usize) {
        {
            let segments = recover_read_guard(&self.segments);
            if let Some(info) = segments.get(name) {
                let old_size = info.size.swap(new_size, Ordering::Relaxed);
                if new_size > old_size {
                    self.current_memory
                        .fetch_add(new_size - old_size, Ordering::Relaxed);
                } else {
                    self.current_memory
                        .fetch_sub(old_size - new_size, Ordering::Relaxed);
                }
            }
        }
        self.update_stats();
    }

    /// Record access to a segment
    pub fn access(&self, name: &str) {
        let segments = recover_read_guard(&self.segments);
        if let Some(info) = segments.get(name) {
            info.touch();
        }

        // Update access history
        let mut history = recover_write_guard(&self.access_history);
        history.push_back(name.to_string());
        // Keep limited history
        while history.len() > 10000 {
            history.pop_front();
        }
    }

    /// Check if spilling is needed, return segments to spill
    pub fn needs_spill(&self) -> Option<Vec<String>> {
        let current = self.current_memory.load(Ordering::Relaxed);
        let threshold = (self.config.max_memory as f64 * self.config.spill_threshold) as usize;

        if current < threshold {
            return None;
        }

        // Apply decay to all segments
        self.decay_all_scores();

        // Calculate how much we need to free
        let target = (self.config.max_memory as f64 * self.config.target_after_spill) as usize;
        let to_free = current.saturating_sub(target);

        if to_free == 0 {
            return None;
        }

        // Find coldest segments to spill
        let mut candidates: Vec<(String, u64, usize)> = Vec::new();

        let segments = recover_read_guard(&self.segments);
        for (name, info) in segments.iter() {
            // Skip already spilled segments
            if *recover_read_guard(&info.is_spilled) {
                continue;
            }

            let size = info.size.load(Ordering::Relaxed);
            if size < self.config.min_spill_size {
                continue;
            }

            let coldness = info.coldness_score();
            candidates.push((name.clone(), coldness, size));
        }

        // Sort by coldness (descending - higher = colder)
        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        // Select segments until we've freed enough
        let mut freed = 0usize;
        let mut to_spill = Vec::new();

        for (name, _, size) in candidates {
            if freed >= to_free {
                break;
            }
            to_spill.push(name);
            freed += size;
        }

        if to_spill.is_empty() {
            None
        } else {
            Some(to_spill)
        }
    }

    /// Spill a segment to disk
    pub fn spill(&self, name: &str, data: &[u8]) -> Result<PathBuf, SpillError> {
        self.ensure_spill_dir()?;

        let segments = read_guard_or_err(&self.segments, "spill manager segments")?;

        let info = segments
            .get(name)
            .ok_or_else(|| SpillError::SegmentNotFound(name.to_string()))?;

        // Check if already spilled
        if *read_guard_or_err(&info.is_spilled, "spill manager segment flag")? {
            return Err(SpillError::AlreadySpilled(name.to_string()));
        }

        // Generate spill file path
        let filename = format!("{}-{}.spill", name, std::process::id());
        let path = self.config.spill_dir.join(&filename);

        // Write data with checksum
        let file = File::create(&path)?;
        let mut writer = BufWriter::new(file);

        // Header: magic(4) + version(1) + checksum(4) + size(8)
        writer.write_all(b"SPIL")?; // Magic
        writer.write_all(&[1u8])?; // Version

        // Calculate simple checksum
        let checksum = data.iter().fold(0u32, |acc, &b| acc.wrapping_add(b as u32));
        writer.write_all(&checksum.to_le_bytes())?;
        writer.write_all(&(data.len() as u64).to_le_bytes())?;

        // Write data
        writer.write_all(data)?;
        writer.flush()?;

        // Update segment state
        drop(segments);

        let segments = read_guard_or_err(&self.segments, "spill manager segments")?;
        if let Some(info) = segments.get(name) {
            *write_guard_or_err(&info.is_spilled, "spill manager segment flag")? = true;
            *write_guard_or_err(&info.spill_path, "spill manager segment spill path")? =
                Some(path.clone());
        }

        // Update memory tracking
        self.current_memory.fetch_sub(data.len(), Ordering::Relaxed);

        // Track in spilled set
        write_guard_or_err(&self.spilled_segments, "spill manager spilled set")?
            .insert(name.to_string());

        // Update stats
        let mut stats = write_guard_or_err(&self.stats, "spill manager stats")?;
        stats.spill_operations += 1;
        stats.bytes_spilled += data.len() as u64;
        stats.spilled_count += 1;
        stats.disk_usage += data.len() as u64;
        drop(stats);

        self.update_stats();

        Ok(path)
    }

    /// Reload a spilled segment from disk
    pub fn reload(&self, name: &str) -> Result<Option<Vec<u8>>, SpillError> {
        let segments = read_guard_or_err(&self.segments, "spill manager segments")?;

        let info = segments
            .get(name)
            .ok_or_else(|| SpillError::SegmentNotFound(name.to_string()))?;

        // Check if actually spilled
        if !*read_guard_or_err(&info.is_spilled, "spill manager segment flag")? {
            return Ok(None);
        }

        let path = info
            .spill_path
            .read()
            .map_err(|_| spill_lock_error("spill manager segment spill path"))?
            .clone()
            .ok_or_else(|| SpillError::NotSpilled(name.to_string()))?;

        // Read and validate
        let file = File::open(&path)?;
        let mut reader = BufReader::new(file);

        // Read header
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        if &magic != b"SPIL" {
            return Err(SpillError::ChecksumMismatch);
        }

        let mut version = [0u8; 1];
        reader.read_exact(&mut version)?;

        let mut checksum_bytes = [0u8; 4];
        reader.read_exact(&mut checksum_bytes)?;
        let expected_checksum = u32::from_le_bytes(checksum_bytes);

        let mut size_bytes = [0u8; 8];
        reader.read_exact(&mut size_bytes)?;
        let size = u64::from_le_bytes(size_bytes) as usize;

        // Read data
        let mut data = vec![0u8; size];
        reader.read_exact(&mut data)?;

        // Validate checksum
        let actual_checksum = data.iter().fold(0u32, |acc, &b| acc.wrapping_add(b as u32));
        if actual_checksum != expected_checksum {
            return Err(SpillError::ChecksumMismatch);
        }

        // Update segment state
        drop(segments);

        let segments = read_guard_or_err(&self.segments, "spill manager segments")?;
        if let Some(info) = segments.get(name) {
            *write_guard_or_err(&info.is_spilled, "spill manager segment flag")? = false;
            *write_guard_or_err(&info.spill_path, "spill manager segment spill path")? = None;
        }

        // Update memory tracking
        self.current_memory.fetch_add(data.len(), Ordering::Relaxed);

        // Remove from spilled set
        write_guard_or_err(&self.spilled_segments, "spill manager spilled set")?.remove(name);

        // Delete spill file
        let _ = fs::remove_file(&path);

        // Update stats
        let mut stats = write_guard_or_err(&self.stats, "spill manager stats")?;
        stats.reload_operations += 1;
        stats.bytes_reloaded += data.len() as u64;
        stats.spilled_count = stats.spilled_count.saturating_sub(1);
        stats.disk_usage = stats.disk_usage.saturating_sub(data.len() as u64);
        drop(stats);

        self.update_stats();

        Ok(Some(data))
    }

    /// Check if a segment is spilled
    pub fn is_spilled(&self, name: &str) -> bool {
        recover_read_guard(&self.spilled_segments).contains(name)
    }

    /// Get current statistics
    pub fn stats(&self) -> SpillStats {
        recover_read_guard(&self.stats).clone()
    }

    /// Get current memory usage
    pub fn memory_usage(&self) -> usize {
        self.current_memory.load(Ordering::Relaxed)
    }

    /// Get memory utilization (0.0-1.0)
    pub fn utilization(&self) -> f64 {
        let current = self.current_memory.load(Ordering::Relaxed);
        if self.config.max_memory == 0 {
            0.0
        } else {
            current as f64 / self.config.max_memory as f64
        }
    }

    /// List all tracked segments
    pub fn list_segments(&self) -> Vec<(String, usize, bool)> {
        let segments = recover_read_guard(&self.segments);
        segments
            .iter()
            .map(|(name, info)| {
                (
                    name.clone(),
                    info.size.load(Ordering::Relaxed),
                    *recover_read_guard(&info.is_spilled),
                )
            })
            .collect()
    }

    /// Clean up all spill files
    pub fn cleanup(&self) -> io::Result<()> {
        if self.config.spill_dir.exists() {
            for entry in fs::read_dir(&self.config.spill_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().map(|e| e == "spill").unwrap_or(false) {
                    let _ = fs::remove_file(path);
                }
            }
        }

        // Clear spilled state
        let segments = recover_read_guard(&self.segments);
        for info in segments.values() {
            *recover_write_guard(&info.is_spilled) = false;
            *recover_write_guard(&info.spill_path) = None;
        }

        recover_write_guard(&self.spilled_segments).clear();

        Ok(())
    }

    /// Decay all segment scores (called periodically)
    fn decay_all_scores(&self) {
        let segments = recover_read_guard(&self.segments);
        for info in segments.values() {
            info.decay_score(self.config.access_decay);
        }
    }

    /// Update stats from current state
    fn update_stats(&self) {
        let mut stats = recover_write_guard(&self.stats);
        stats.current_memory = self.current_memory.load(Ordering::Relaxed);

        let segments = recover_read_guard(&self.segments);
        stats.segment_count = segments.len();
        drop(segments);

        let spilled = recover_read_guard(&self.spilled_segments);
        stats.spilled_count = spilled.len();
    }
}

impl Default for SpillManager {
    fn default() -> Self {
        Self::new(SpillConfig::default())
    }
}

impl Drop for SpillManager {
    fn drop(&mut self) {
        // Clean up spill files on drop
        let _ = self.cleanup();
    }
}

// ============================================================================
// Memory-Limited Graph Wrapper
// ============================================================================

/// A graph wrapper that automatically spills to disk when memory limit is reached
pub struct SpillableGraph<G> {
    /// The underlying graph
    pub graph: G,
    /// Spill manager
    pub spill_manager: SpillManager,
    /// Segment name for this graph
    segment_name: String,
}

impl<G> SpillableGraph<G> {
    /// Create a new spillable graph wrapper
    pub fn new(graph: G, segment_name: &str, config: SpillConfig) -> Self {
        Self {
            graph,
            spill_manager: SpillManager::new(config),
            segment_name: segment_name.to_string(),
        }
    }

    /// Get the segment name
    pub fn segment_name(&self) -> &str {
        &self.segment_name
    }

    /// Check memory and spill if needed
    pub fn check_memory(&mut self, current_size: usize) -> bool {
        self.spill_manager
            .update_size(&self.segment_name, current_size);
        self.spill_manager.needs_spill().is_some()
    }

    /// Get spill manager stats
    pub fn stats(&self) -> SpillStats {
        self.spill_manager.stats()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn test_config() -> SpillConfig {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        SpillConfig::new()
            .max_memory(1024 * 1024) // 1MB for testing
            .spill_threshold(0.5)
            .target_after_spill(0.3)
            .min_spill_size(100)
            .spill_dir(env::temp_dir().join(format!(
                "reddb-spill-test-{}-{}",
                std::process::id(),
                id
            )))
    }

    #[test]
    fn test_register_segment() {
        let manager = SpillManager::new(test_config());

        manager.register_segment("seg1", 100_000);
        manager.register_segment("seg2", 200_000);

        assert_eq!(manager.memory_usage(), 300_000);

        let stats = manager.stats();
        assert_eq!(stats.segment_count, 2);
    }

    #[test]
    fn test_update_size() {
        let manager = SpillManager::new(test_config());

        manager.register_segment("seg1", 100_000);
        assert_eq!(manager.memory_usage(), 100_000);

        manager.update_size("seg1", 150_000);
        assert_eq!(manager.memory_usage(), 150_000);

        manager.update_size("seg1", 50_000);
        assert_eq!(manager.memory_usage(), 50_000);
    }

    #[test]
    fn test_needs_spill() {
        let manager = SpillManager::new(test_config());

        // Below threshold
        manager.register_segment("seg1", 400_000); // 40%
        assert!(manager.needs_spill().is_none());

        // Above threshold (50%)
        manager.register_segment("seg2", 200_000); // Now 60%

        // Access seg1 more to make seg2 colder
        for _ in 0..100 {
            manager.access("seg1");
        }

        let to_spill = manager.needs_spill();
        assert!(to_spill.is_some());
        let segments = to_spill.unwrap();
        assert!(segments.contains(&"seg2".to_string()));
    }

    #[test]
    fn test_spill_and_reload() {
        let manager = SpillManager::new(test_config());

        manager.register_segment("test_seg", 1000);

        let data = vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let path = manager.spill("test_seg", &data).unwrap();

        assert!(path.exists());
        assert!(manager.is_spilled("test_seg"));

        let reloaded = manager.reload("test_seg").unwrap();
        assert!(reloaded.is_some());
        assert_eq!(reloaded.unwrap(), data);
        assert!(!manager.is_spilled("test_seg"));
    }

    #[test]
    fn test_checksum_validation() {
        let manager = SpillManager::new(test_config());

        // Use unique segment name to avoid conflicts
        manager.register_segment("checksum_test_seg", 100);

        let data = b"test data for checksum validation";
        manager.spill("checksum_test_seg", data).unwrap();

        // Should reload successfully
        let reloaded = manager.reload("checksum_test_seg").unwrap();
        assert!(reloaded.is_some());
        assert_eq!(&reloaded.unwrap()[..], data);
    }

    #[test]
    fn test_list_segments() {
        let manager = SpillManager::new(test_config());

        manager.register_segment("alpha", 1000);
        manager.register_segment("beta", 2000);
        manager.register_segment("gamma", 3000);

        let segments = manager.list_segments();
        assert_eq!(segments.len(), 3);

        let names: Vec<_> = segments.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
        assert!(names.contains(&"gamma"));
    }

    #[test]
    fn test_unregister_segment() {
        let manager = SpillManager::new(test_config());

        manager.register_segment("seg1", 100_000);
        manager.register_segment("seg2", 200_000);

        assert_eq!(manager.memory_usage(), 300_000);

        manager.unregister_segment("seg1");

        assert_eq!(manager.memory_usage(), 200_000);
        assert_eq!(manager.stats().segment_count, 1);
    }

    #[test]
    fn test_cleanup() {
        let manager = SpillManager::new(test_config());

        manager.register_segment("seg1", 100);
        manager.spill("seg1", b"test data").unwrap();

        assert!(manager.is_spilled("seg1"));

        manager.cleanup().unwrap();

        assert!(!manager.is_spilled("seg1"));
    }

    #[test]
    fn test_utilization() {
        let config = SpillConfig::new().max_memory(1000);
        let manager = SpillManager::new(config);

        manager.register_segment("seg", 500);

        let util = manager.utilization();
        assert!((util - 0.5).abs() < 0.001);
    }
}
