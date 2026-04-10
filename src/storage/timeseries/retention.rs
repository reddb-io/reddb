//! Retention policy for time-series data

/// Retention policy configuration
#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    /// Maximum age in nanoseconds. Data older than this is eligible for deletion.
    pub max_age_ns: u64,
    /// Optional: only apply to a specific resolution tier
    pub resolution_tier: Option<String>,
}

impl RetentionPolicy {
    /// Create a retention policy with a duration in seconds
    pub fn from_secs(secs: u64) -> Self {
        Self {
            max_age_ns: secs * 1_000_000_000,
            resolution_tier: None,
        }
    }

    /// Create a retention policy with a duration in days
    pub fn from_days(days: u64) -> Self {
        Self::from_secs(days * 86400)
    }

    /// Check if a timestamp is expired given the current time
    pub fn is_expired(&self, timestamp_ns: u64, now_ns: u64) -> bool {
        now_ns.saturating_sub(timestamp_ns) > self.max_age_ns
    }

    /// Get the cutoff timestamp (anything older should be deleted)
    pub fn cutoff_ns(&self, now_ns: u64) -> u64 {
        now_ns.saturating_sub(self.max_age_ns)
    }
}

/// Downsample policy definition
#[derive(Debug, Clone)]
pub struct DownsamplePolicy {
    /// Source resolution label (e.g., "raw", "1m", "5m")
    pub source: String,
    /// Target resolution label (e.g., "5m", "1h")
    pub target: String,
    /// Aggregation function to use (e.g., "avg", "max")
    pub aggregation: String,
    /// Target bucket size in nanoseconds
    pub bucket_ns: u64,
}

impl DownsamplePolicy {
    /// Parse a policy string like "1h:5m:avg"
    /// Format: target_resolution:source_resolution:aggregation
    pub fn parse(spec: &str) -> Option<Self> {
        let parts: Vec<&str> = spec.split(':').collect();
        if parts.len() < 2 {
            return None;
        }
        let target = parts[0].to_string();
        let source = parts[1].to_string();
        let aggregation = if parts.len() > 2 {
            parts[2].to_string()
        } else {
            "avg".to_string()
        };
        let bucket_ns = parse_duration_ns(&target)?;

        Some(Self {
            source,
            target,
            aggregation,
            bucket_ns,
        })
    }
}

/// Parse a duration string (e.g., "5m", "1h", "30s") into nanoseconds
pub fn parse_duration_ns(s: &str) -> Option<u64> {
    let s = s.trim();
    if s == "raw" {
        return Some(0);
    }
    let (num_str, unit) = if s.ends_with("ms") {
        (&s[..s.len() - 2], "ms")
    } else if s.ends_with('s') {
        (&s[..s.len() - 1], "s")
    } else if s.ends_with('m') {
        (&s[..s.len() - 1], "m")
    } else if s.ends_with('h') {
        (&s[..s.len() - 1], "h")
    } else if s.ends_with('d') {
        (&s[..s.len() - 1], "d")
    } else {
        return None;
    };

    let num: u64 = num_str.parse().ok()?;
    let multiplier = match unit {
        "ms" => 1_000_000,
        "s" => 1_000_000_000,
        "m" => 60_000_000_000,
        "h" => 3_600_000_000_000,
        "d" => 86_400_000_000_000,
        _ => return None,
    };

    Some(num * multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retention_policy() {
        let policy = RetentionPolicy::from_days(30);
        let now = 100_000_000_000_000u64; // some time in ns
        let old = now - 31 * 86_400_000_000_000; // 31 days ago
        let recent = now - 1_000_000_000; // 1 second ago

        assert!(policy.is_expired(old, now));
        assert!(!policy.is_expired(recent, now));
    }

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration_ns("5m"), Some(300_000_000_000));
        assert_eq!(parse_duration_ns("1h"), Some(3_600_000_000_000));
        assert_eq!(parse_duration_ns("30s"), Some(30_000_000_000));
        assert_eq!(parse_duration_ns("1d"), Some(86_400_000_000_000));
        assert_eq!(parse_duration_ns("100ms"), Some(100_000_000));
        assert_eq!(parse_duration_ns("raw"), Some(0));
        assert_eq!(parse_duration_ns("invalid"), None);
    }

    #[test]
    fn test_downsample_policy_parse() {
        let policy = DownsamplePolicy::parse("1h:5m:avg").unwrap();
        assert_eq!(policy.target, "1h");
        assert_eq!(policy.source, "5m");
        assert_eq!(policy.aggregation, "avg");
        assert_eq!(policy.bucket_ns, 3_600_000_000_000);
    }
}
