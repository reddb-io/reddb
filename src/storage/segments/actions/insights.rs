//! Trace Insights - Analysis types for execution traces
//!
//! Provides failure pattern detection, performance metrics, and optimization suggestions.

use std::collections::HashMap;

use super::trace::{Attempt, AttemptOutcome};
use super::trace_segment::TraceSegment;

// ==================== FailurePattern ====================

/// Detected failure pattern in traces
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FailurePattern {
    /// Connection refused (port closed/blocked)
    ConnectionRefused,
    /// Rate limiting detected (429 or similar)
    RateLimited,
    /// Firewall drop (timeouts without response)
    FirewallDrop,
    /// Unstable target (intermittent failures)
    Unstable,
    /// Authentication failures
    AuthFailure,
    /// Network unreachable
    NetworkUnreachable,
}

impl FailurePattern {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ConnectionRefused => "connection_refused",
            Self::RateLimited => "rate_limited",
            Self::FirewallDrop => "firewall_drop",
            Self::Unstable => "unstable",
            Self::AuthFailure => "auth_failure",
            Self::NetworkUnreachable => "network_unreachable",
        }
    }

    /// Suggested action for this pattern
    pub fn suggestion(&self) -> &'static str {
        match self {
            Self::ConnectionRefused => "Port is closed or service down. Skip similar scans.",
            Self::RateLimited => "Reduce scan rate or add delays between requests.",
            Self::FirewallDrop => "Target may be behind firewall. Try different ports/protocols.",
            Self::Unstable => "Target connectivity is intermittent. Retry with longer timeouts.",
            Self::AuthFailure => "Credentials invalid or locked. Try different credentials.",
            Self::NetworkUnreachable => "Check network path. Target may be on different subnet.",
        }
    }
}

// ==================== PerformanceInsight ====================

/// Performance metrics from trace analysis
#[derive(Debug, Clone)]
pub struct PerformanceInsight {
    /// Average response time (ms)
    pub avg_response_ms: f64,
    /// 95th percentile response time (ms)
    pub p95_response_ms: u64,
    /// Success rate (0.0 - 1.0)
    pub success_rate: f64,
    /// Timeout rate (0.0 - 1.0)
    pub timeout_rate: f64,
    /// Total attempts analyzed
    pub total_attempts: usize,
    /// Network overhead ratio (network_time / total_time)
    pub network_ratio: f64,
}

impl PerformanceInsight {
    /// Get a summary string
    pub fn summary(&self) -> String {
        format!(
            "Avg: {:.0}ms, P95: {}ms, Success: {:.1}%, Timeouts: {:.1}% ({} attempts)",
            self.avg_response_ms,
            self.p95_response_ms,
            self.success_rate * 100.0,
            self.timeout_rate * 100.0,
            self.total_attempts
        )
    }

    /// Is performance concerning?
    pub fn is_concerning(&self) -> bool {
        self.success_rate < 0.5 || self.timeout_rate > 0.3 || self.avg_response_ms > 5000.0
    }
}

// ==================== PlaybookInsight ====================

/// Playbook execution optimization insight
#[derive(Debug, Clone)]
pub struct PlaybookInsight {
    /// Steps that took the longest
    pub bottlenecks: Vec<(String, u64)>,
    /// Steps that could run in parallel
    pub parallelizable: Vec<Vec<String>>,
    /// Steps that always succeed (could skip retries)
    pub always_succeeds: Vec<String>,
    /// Steps that always fail (should skip)
    pub always_fails: Vec<String>,
    /// Estimated time savings if optimized (ms)
    pub potential_savings_ms: u64,
}

impl PlaybookInsight {
    /// Get optimization suggestions
    pub fn suggestions(&self) -> Vec<String> {
        let mut suggestions = Vec::new();

        if !self.bottlenecks.is_empty() {
            let (step, time) = &self.bottlenecks[0];
            suggestions.push(format!(
                "Bottleneck: '{}' takes {}ms - consider async execution",
                step, time
            ));
        }

        if !self.parallelizable.is_empty() {
            suggestions.push(format!(
                "{} step groups can run in parallel for faster execution",
                self.parallelizable.len()
            ));
        }

        if !self.always_fails.is_empty() {
            suggestions.push(format!(
                "Steps always fail: {}. Consider removing or fixing.",
                self.always_fails.join(", ")
            ));
        }

        if self.potential_savings_ms > 1000 {
            suggestions.push(format!(
                "Potential savings: {:.1}s with optimizations",
                self.potential_savings_ms as f64 / 1000.0
            ));
        }

        suggestions
    }
}

// ==================== TargetProfile ====================

/// Profile of what techniques work on a target
#[derive(Debug, Clone)]
pub struct TargetProfile {
    /// Target identifier
    pub target: String,
    /// Effective techniques (high success rate)
    pub effective: Vec<(String, f64)>, // (technique, success_rate)
    /// Ineffective techniques (low success rate)
    pub ineffective: Vec<(String, f64)>,
    /// Average response time for this target
    pub avg_response_ms: f64,
    /// Detected failure patterns
    pub patterns: Vec<FailurePattern>,
}

impl TargetProfile {
    /// Get a recommended approach for this target
    pub fn recommended_approach(&self) -> Vec<String> {
        let mut recs = Vec::new();

        // Recommend effective techniques
        for (tech, rate) in &self.effective {
            if *rate > 0.8 {
                recs.push(format!("Use '{}' ({:.0}% success)", tech, rate * 100.0));
            }
        }

        // Warn about ineffective techniques
        for (tech, rate) in &self.ineffective {
            if *rate < 0.2 {
                recs.push(format!("Avoid '{}' ({:.0}% success)", tech, rate * 100.0));
            }
        }

        // Add pattern-based suggestions
        for pattern in &self.patterns {
            recs.push(pattern.suggestion().to_string());
        }

        recs
    }
}

// ==================== TraceSegment Insight Methods ====================

impl TraceSegment {
    /// Calculate performance metrics across all traces
    pub fn performance_insight(&self) -> PerformanceInsight {
        let all_attempts: Vec<&Attempt> =
            self.all().iter().flat_map(|t| t.attempts.iter()).collect();

        if all_attempts.is_empty() {
            return PerformanceInsight {
                avg_response_ms: 0.0,
                p95_response_ms: 0,
                success_rate: 0.0,
                timeout_rate: 0.0,
                total_attempts: 0,
                network_ratio: 0.0,
            };
        }

        let total = all_attempts.len();
        let successes = all_attempts
            .iter()
            .filter(|a| a.outcome == AttemptOutcome::Success)
            .count();
        let timeouts = all_attempts
            .iter()
            .filter(|a| a.outcome == AttemptOutcome::Timeout)
            .count();

        // Calculate average response time
        let sum_ms: u64 = all_attempts.iter().map(|a| a.duration_ms).sum();
        let avg_response_ms = sum_ms as f64 / total as f64;

        // Calculate P95
        let mut durations: Vec<u64> = all_attempts.iter().map(|a| a.duration_ms).collect();
        durations.sort_unstable();
        let p95_idx = (durations.len() as f64 * 0.95) as usize;
        let p95_response_ms = durations
            .get(p95_idx.min(durations.len() - 1))
            .copied()
            .unwrap_or(0);

        // Calculate network ratio from timing info
        let total_time: u64 = self.all().iter().map(|t| t.timing.total_ms).sum();
        let network_time: u64 = self.all().iter().map(|t| t.timing.network_ms).sum();
        let network_ratio = if total_time > 0 {
            network_time as f64 / total_time as f64
        } else {
            0.0
        };

        PerformanceInsight {
            avg_response_ms,
            p95_response_ms,
            success_rate: successes as f64 / total as f64,
            timeout_rate: timeouts as f64 / total as f64,
            total_attempts: total,
            network_ratio,
        }
    }

    /// Detect failure patterns across traces
    pub fn detect_patterns(&self) -> Vec<(FailurePattern, usize)> {
        let mut pattern_counts: HashMap<FailurePattern, usize> = HashMap::new();

        for trace in self.all() {
            for attempt in &trace.attempts {
                if let AttemptOutcome::Failed(ref msg) = attempt.outcome {
                    let msg_lower = msg.to_lowercase();

                    if msg_lower.contains("refused") || msg_lower.contains("reset") {
                        *pattern_counts
                            .entry(FailurePattern::ConnectionRefused)
                            .or_insert(0) += 1;
                    } else if msg_lower.contains("429")
                        || msg_lower.contains("rate")
                        || msg_lower.contains("limit")
                    {
                        *pattern_counts
                            .entry(FailurePattern::RateLimited)
                            .or_insert(0) += 1;
                    } else if msg_lower.contains("auth")
                        || msg_lower.contains("401")
                        || msg_lower.contains("403")
                    {
                        *pattern_counts
                            .entry(FailurePattern::AuthFailure)
                            .or_insert(0) += 1;
                    } else if msg_lower.contains("unreachable") || msg_lower.contains("network") {
                        *pattern_counts
                            .entry(FailurePattern::NetworkUnreachable)
                            .or_insert(0) += 1;
                    }
                } else if attempt.outcome == AttemptOutcome::Timeout {
                    *pattern_counts
                        .entry(FailurePattern::FirewallDrop)
                        .or_insert(0) += 1;
                }
            }
        }

        // Check for instability (mix of success and failures on same operation)
        let mut ops_success: HashMap<String, usize> = HashMap::new();
        let mut ops_fail: HashMap<String, usize> = HashMap::new();

        for trace in self.all() {
            for attempt in &trace.attempts {
                if attempt.outcome == AttemptOutcome::Success {
                    *ops_success.entry(attempt.what.clone()).or_insert(0) += 1;
                } else {
                    *ops_fail.entry(attempt.what.clone()).or_insert(0) += 1;
                }
            }
        }

        for (op, success_count) in &ops_success {
            if let Some(&fail_count) = ops_fail.get(op) {
                let total = success_count + fail_count;
                let fail_ratio = fail_count as f64 / total as f64;
                // If between 30-70% failure rate, it's unstable
                if fail_ratio > 0.3 && fail_ratio < 0.7 && total >= 3 {
                    *pattern_counts.entry(FailurePattern::Unstable).or_insert(0) += 1;
                }
            }
        }

        let mut patterns: Vec<_> = pattern_counts.into_iter().collect();
        patterns.sort_by(|a, b| b.1.cmp(&a.1));
        patterns
    }

    /// Analyze playbook execution for optimization opportunities
    pub fn playbook_insight(&self) -> PlaybookInsight {
        // Find bottlenecks (top 3 slowest operations)
        let mut op_times: HashMap<String, Vec<u64>> = HashMap::new();
        let mut op_success: HashMap<String, usize> = HashMap::new();
        let mut op_total: HashMap<String, usize> = HashMap::new();

        for trace in self.all() {
            for attempt in &trace.attempts {
                op_times
                    .entry(attempt.what.clone())
                    .or_default()
                    .push(attempt.duration_ms);
                *op_total.entry(attempt.what.clone()).or_insert(0) += 1;
                if attempt.outcome == AttemptOutcome::Success {
                    *op_success.entry(attempt.what.clone()).or_insert(0) += 1;
                }
            }
        }

        // Calculate average times and find bottlenecks
        let mut avg_times: Vec<(String, u64)> = op_times
            .iter()
            .map(|(op, times)| {
                let avg = times.iter().sum::<u64>() / times.len() as u64;
                (op.clone(), avg)
            })
            .collect();
        avg_times.sort_by(|a, b| b.1.cmp(&a.1));
        let bottlenecks: Vec<_> = avg_times.into_iter().take(3).collect();

        // Find always succeeds / always fails
        let mut always_succeeds = Vec::new();
        let mut always_fails = Vec::new();

        for (op, total) in &op_total {
            let successes = *op_success.get(op).unwrap_or(&0);
            if *total >= 3 {
                if successes == *total {
                    always_succeeds.push(op.clone());
                } else if successes == 0 {
                    always_fails.push(op.clone());
                }
            }
        }

        // Estimate potential savings (skip always-fails, reduce retries on always-succeeds)
        let fail_time: u64 = always_fails
            .iter()
            .filter_map(|op| op_times.get(op))
            .flat_map(|times| times.iter())
            .sum();

        PlaybookInsight {
            bottlenecks,
            parallelizable: Vec::new(), // Would need step dependency info
            always_succeeds,
            always_fails,
            potential_savings_ms: fail_time,
        }
    }

    /// Build a profile of what works on a specific target
    pub fn target_profile(&self, target: &str) -> TargetProfile {
        // Filter traces for this target (would need target info in trace)
        // For now, analyze all traces as if for a single target

        let mut technique_success: HashMap<String, usize> = HashMap::new();
        let mut technique_total: HashMap<String, usize> = HashMap::new();
        let mut total_time: u64 = 0;
        let mut count: usize = 0;

        for trace in self.all() {
            for attempt in &trace.attempts {
                *technique_total.entry(attempt.what.clone()).or_insert(0) += 1;
                if attempt.outcome == AttemptOutcome::Success {
                    *technique_success.entry(attempt.what.clone()).or_insert(0) += 1;
                }
                total_time += attempt.duration_ms;
                count += 1;
            }
        }

        let avg_response_ms = if count > 0 {
            total_time as f64 / count as f64
        } else {
            0.0
        };

        // Calculate success rates per technique
        let mut effective = Vec::new();
        let mut ineffective = Vec::new();

        for (tech, total) in &technique_total {
            if *total >= 2 {
                let successes = *technique_success.get(tech).unwrap_or(&0);
                let rate = successes as f64 / *total as f64;
                if rate >= 0.7 {
                    effective.push((tech.clone(), rate));
                } else if rate <= 0.3 {
                    ineffective.push((tech.clone(), rate));
                }
            }
        }

        // Sort by rate
        effective.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ineffective.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        // Get detected patterns
        let patterns: Vec<FailurePattern> =
            self.detect_patterns().into_iter().map(|(p, _)| p).collect();

        TargetProfile {
            target: target.to_string(),
            effective,
            ineffective,
            avg_response_ms,
            patterns,
        }
    }

    /// Get proactive warnings before scanning a target
    pub fn pre_scan_warnings(&self, target: &str) -> Vec<String> {
        let profile = self.target_profile(target);
        let mut warnings = Vec::new();

        // Check for concerning patterns
        for pattern in &profile.patterns {
            match pattern {
                FailurePattern::RateLimited => {
                    warnings.push("Previously rate-limited. Consider adding delays.".to_string());
                }
                FailurePattern::FirewallDrop => {
                    warnings.push("Firewall detected. Scans may timeout.".to_string());
                }
                FailurePattern::AuthFailure => {
                    warnings.push("Auth failures detected. Verify credentials.".to_string());
                }
                _ => {}
            }
        }

        // Check response times
        if profile.avg_response_ms > 3000.0 {
            warnings.push(format!(
                "Slow target (avg: {:.0}ms). Increase timeouts.",
                profile.avg_response_ms
            ));
        }

        // Check ineffective techniques
        if !profile.ineffective.is_empty() {
            let techs: Vec<_> = profile
                .ineffective
                .iter()
                .take(2)
                .map(|(t, _)| t.as_str())
                .collect();
            warnings.push(format!("Previously ineffective: {}", techs.join(", ")));
        }

        warnings
    }
}
