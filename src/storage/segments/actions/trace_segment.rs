//! TraceSegment and Tracer - Execution trace storage and helper
//!
//! Stores and queries detailed execution traces for debugging and optimization.

use std::collections::HashMap;

use crate::storage::primitives::encoding::{read_varu32, write_varu32, DecodeError};

use super::record::current_timestamp;
use super::trace::{ActionTrace, Attempt, AttemptOutcome, TimingInfo};

// ==================== TraceSegment ====================

/// Segment for storing execution traces
#[derive(Debug)]
pub struct TraceSegment {
    traces: Vec<ActionTrace>,
    by_action: HashMap<[u8; 16], usize>,
}

impl TraceSegment {
    pub fn new() -> Self {
        Self {
            traces: Vec::new(),
            by_action: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.traces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.traces.is_empty()
    }

    /// Add a trace
    pub fn add(&mut self, trace: ActionTrace) {
        let idx = self.traces.len();
        self.by_action.insert(trace.action_id, idx);
        self.traces.push(trace);
    }

    /// Get trace for an action
    pub fn for_action(&self, action_id: &[u8; 16]) -> Option<&ActionTrace> {
        self.by_action.get(action_id).map(|&i| &self.traces[i])
    }

    /// Get all failed attempts across all traces
    pub fn all_failed_attempts(&self) -> Vec<&Attempt> {
        self.traces
            .iter()
            .flat_map(|t| t.failed_attempts())
            .collect()
    }

    /// Get all timeout attempts across all traces
    pub fn all_timeouts(&self) -> Vec<&Attempt> {
        self.traces
            .iter()
            .flat_map(|t| t.timeout_attempts())
            .collect()
    }

    /// Get all traces
    pub fn all(&self) -> &[ActionTrace] {
        &self.traces
    }

    /// Serialize the segment
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        write_varu32(&mut buf, self.traces.len() as u32);

        for trace in &self.traces {
            let encoded = trace.encode();
            write_varu32(&mut buf, encoded.len() as u32);
            buf.extend_from_slice(&encoded);
        }

        buf
    }

    /// Deserialize the segment
    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut pos = 0;
        let count = read_varu32(bytes, &mut pos)? as usize;

        let mut segment = Self::new();

        for _ in 0..count {
            let len = read_varu32(bytes, &mut pos)? as usize;
            if pos + len > bytes.len() {
                return Err(DecodeError("trace out of bounds"));
            }
            let trace = ActionTrace::decode(&bytes[pos..pos + len])?;
            pos += len;
            segment.add(trace);
        }

        Ok(segment)
    }
}

impl Default for TraceSegment {
    fn default() -> Self {
        Self::new()
    }
}

// ==================== Tracer Helper ====================

/// Helper for tracing execution with --trace flag
pub struct Tracer {
    action_id: [u8; 16],
    attempts: Vec<Attempt>,
    parameters: Vec<(String, String)>,
    timing: TimingInfo,
}

impl Tracer {
    pub fn new(action_id: [u8; 16]) -> Self {
        Self {
            action_id,
            attempts: Vec::new(),
            parameters: Vec::new(),
            timing: TimingInfo::new(),
        }
    }

    /// Execute a closure and record the attempt
    pub fn attempt<T, E, F>(&mut self, what: &str, f: F) -> Result<T, E>
    where
        F: FnOnce() -> Result<T, E>,
        E: std::fmt::Display,
    {
        let start = current_timestamp();
        let result = f();
        let duration = current_timestamp().saturating_sub(start);

        let outcome = match &result {
            Ok(_) => AttemptOutcome::Success,
            Err(e) => AttemptOutcome::Failed(e.to_string()),
        };

        self.attempts.push(Attempt {
            timestamp: start,
            what: what.to_string(),
            outcome,
            duration_ms: duration,
        });

        // Track as network time by default
        self.timing.add_network(duration);

        result
    }

    /// Record a timeout attempt
    pub fn timeout(&mut self, what: &str, duration_ms: u64) {
        self.attempts.push(Attempt {
            timestamp: current_timestamp(),
            what: what.to_string(),
            outcome: AttemptOutcome::Timeout,
            duration_ms,
        });
    }

    /// Add a parameter
    pub fn param(&mut self, key: &str, value: &str) {
        self.parameters.push((key.to_string(), value.to_string()));
    }

    /// Finalize and return the trace
    pub fn finish(mut self) -> ActionTrace {
        self.timing.complete();
        ActionTrace {
            action_id: self.action_id,
            attempts: self.attempts,
            parameters: self.parameters,
            timing: self.timing,
        }
    }
}
