//! Trace Types for Action Execution
//!
//! Types for capturing detailed execution traces: AttemptOutcome, Attempt, TimingInfo, ActionTrace.

use crate::storage::primitives::encoding::{
    read_string, read_varu32, read_varu64, write_string, write_varu32, write_varu64, DecodeError,
};

use super::record::current_timestamp;

// ==================== AttemptOutcome ====================

/// Outcome of a single attempt
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptOutcome {
    Success = 0,
    Failed(String) = 1,
    Timeout = 2,
    Retry = 3,
}

impl AttemptOutcome {
    fn discriminant(&self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Failed(_) => 1,
            Self::Timeout => 2,
            Self::Retry => 3,
        }
    }

    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        buf.push(self.discriminant());
        if let Self::Failed(msg) = self {
            write_string(buf, msg);
        }
    }

    pub(crate) fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        if *pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (AttemptOutcome)"));
        }
        let disc = bytes[*pos];
        *pos += 1;

        match disc {
            0 => Ok(Self::Success),
            1 => Ok(Self::Failed(read_string(bytes, pos)?.to_string())),
            2 => Ok(Self::Timeout),
            3 => Ok(Self::Retry),
            _ => Err(DecodeError("invalid AttemptOutcome discriminant")),
        }
    }
}

// ==================== Attempt ====================

/// A single attempt within an action trace
#[derive(Debug, Clone)]
pub struct Attempt {
    /// When the attempt started (unix millis)
    pub timestamp: u64,
    /// What was attempted (e.g., "connect", "send_request", "parse_response")
    pub what: String,
    /// Outcome of this attempt
    pub outcome: AttemptOutcome,
    /// Duration in milliseconds
    pub duration_ms: u64,
}

impl Attempt {
    pub fn new(what: &str, outcome: AttemptOutcome, duration_ms: u64) -> Self {
        Self {
            timestamp: current_timestamp(),
            what: what.to_string(),
            outcome,
            duration_ms,
        }
    }

    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        write_varu64(buf, self.timestamp);
        write_string(buf, &self.what);
        self.outcome.encode(buf);
        write_varu64(buf, self.duration_ms);
    }

    pub(crate) fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        let timestamp = read_varu64(bytes, pos)?;
        let what = read_string(bytes, pos)?.to_string();
        let outcome = AttemptOutcome::decode(bytes, pos)?;
        let duration_ms = read_varu64(bytes, pos)?;

        Ok(Self {
            timestamp,
            what,
            outcome,
            duration_ms,
        })
    }
}

// ==================== TimingInfo ====================

/// Timing breakdown for an action
#[derive(Debug, Clone, Default)]
pub struct TimingInfo {
    /// When the action started (unix millis)
    pub started_at: u64,
    /// When the action ended (unix millis)
    pub ended_at: u64,
    /// Total duration in milliseconds
    pub total_ms: u64,
    /// Time spent waiting for network (ms)
    pub network_ms: u64,
    /// Time spent processing data (ms)
    pub processing_ms: u64,
}

impl TimingInfo {
    pub fn new() -> Self {
        Self {
            started_at: current_timestamp(),
            ..Default::default()
        }
    }

    /// Mark the action as complete and calculate totals
    pub fn complete(&mut self) {
        self.ended_at = current_timestamp();
        self.total_ms = self.ended_at.saturating_sub(self.started_at);
    }

    /// Add network time
    pub fn add_network(&mut self, ms: u64) {
        self.network_ms += ms;
    }

    /// Add processing time
    pub fn add_processing(&mut self, ms: u64) {
        self.processing_ms += ms;
    }

    pub(crate) fn encode(&self, buf: &mut Vec<u8>) {
        write_varu64(buf, self.started_at);
        write_varu64(buf, self.ended_at);
        write_varu64(buf, self.total_ms);
        write_varu64(buf, self.network_ms);
        write_varu64(buf, self.processing_ms);
    }

    pub(crate) fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        Ok(Self {
            started_at: read_varu64(bytes, pos)?,
            ended_at: read_varu64(bytes, pos)?,
            total_ms: read_varu64(bytes, pos)?,
            network_ms: read_varu64(bytes, pos)?,
            processing_ms: read_varu64(bytes, pos)?,
        })
    }
}

// ==================== ActionTrace ====================

/// Full execution trace for an action (captured with --trace flag)
#[derive(Debug, Clone)]
pub struct ActionTrace {
    /// Links to the ActionRecord
    pub action_id: [u8; 16],
    /// All attempts made during execution
    pub attempts: Vec<Attempt>,
    /// Parameters used (key-value pairs)
    pub parameters: Vec<(String, String)>,
    /// Timing breakdown
    pub timing: TimingInfo,
}

impl ActionTrace {
    pub fn new(action_id: [u8; 16]) -> Self {
        Self {
            action_id,
            attempts: Vec::new(),
            parameters: Vec::new(),
            timing: TimingInfo::new(),
        }
    }

    /// Add an attempt to the trace
    pub fn add_attempt(&mut self, what: &str, outcome: AttemptOutcome, duration_ms: u64) {
        self.attempts.push(Attempt::new(what, outcome, duration_ms));
    }

    /// Add a parameter
    pub fn add_param(&mut self, key: &str, value: &str) {
        self.parameters.push((key.to_string(), value.to_string()));
    }

    /// Get all failed attempts
    pub fn failed_attempts(&self) -> Vec<&Attempt> {
        self.attempts
            .iter()
            .filter(|a| matches!(a.outcome, AttemptOutcome::Failed(_)))
            .collect()
    }

    /// Get all timeout attempts
    pub fn timeout_attempts(&self) -> Vec<&Attempt> {
        self.attempts
            .iter()
            .filter(|a| matches!(a.outcome, AttemptOutcome::Timeout))
            .collect()
    }

    /// Encode to binary format
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(256);

        // Action ID
        buf.extend_from_slice(&self.action_id);

        // Attempts
        write_varu32(&mut buf, self.attempts.len() as u32);
        for attempt in &self.attempts {
            attempt.encode(&mut buf);
        }

        // Parameters
        write_varu32(&mut buf, self.parameters.len() as u32);
        for (k, v) in &self.parameters {
            write_string(&mut buf, k);
            write_string(&mut buf, v);
        }

        // Timing
        self.timing.encode(&mut buf);

        buf
    }

    /// Decode from binary format
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        // Action ID
        if bytes.len() < 16 {
            return Err(DecodeError("unexpected eof (action_id)"));
        }
        let mut action_id = [0u8; 16];
        action_id.copy_from_slice(&bytes[..16]);
        let mut pos = 16;

        // Attempts
        let attempt_count = read_varu32(bytes, &mut pos)? as usize;
        let mut attempts = Vec::with_capacity(attempt_count);
        for _ in 0..attempt_count {
            attempts.push(Attempt::decode(bytes, &mut pos)?);
        }

        // Parameters
        let param_count = read_varu32(bytes, &mut pos)? as usize;
        let mut parameters = Vec::with_capacity(param_count);
        for _ in 0..param_count {
            let k = read_string(bytes, &mut pos)?.to_string();
            let v = read_string(bytes, &mut pos)?.to_string();
            parameters.push((k, v));
        }

        // Timing
        let timing = TimingInfo::decode(bytes, &mut pos)?;

        Ok(Self {
            action_id,
            attempts,
            parameters,
            timing,
        })
    }
}
