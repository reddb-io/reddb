//! ActionRecord - Universal envelope for all actions
//!
//! The core data structure that wraps all security tool outputs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::storage::primitives::encoding::{
    read_string, read_varu32, read_varu64, write_string, write_varu32, write_varu64, DecodeError,
};

use super::payloads::RecordPayload;
use super::types::{ActionOutcome, ActionSource, ActionType, Target};

// Re-export Confidence from loot to maintain single source of truth
pub use super::super::loot::Confidence;

// ==================== ActionRecord ====================

/// Universal envelope for all actions
#[derive(Debug, Clone)]
pub struct ActionRecord {
    /// Unique identifier
    pub id: [u8; 16],
    /// Timestamp (unix millis)
    pub timestamp: u64,
    /// Who produced this action
    pub source: ActionSource,
    /// What was acted upon
    pub target: Target,
    /// Type of action
    pub action_type: ActionType,
    /// Action-specific payload
    pub payload: RecordPayload,
    /// Outcome of the action
    pub outcome: ActionOutcome,
    /// Confidence level
    pub confidence: Confidence,
    /// User-defined tags
    pub tags: Vec<String>,
}

impl ActionRecord {
    /// Create a new action record with generated ID and current timestamp
    pub fn new(
        source: ActionSource,
        target: Target,
        action_type: ActionType,
        payload: RecordPayload,
        outcome: ActionOutcome,
    ) -> Self {
        Self {
            id: generate_id(),
            timestamp: current_timestamp(),
            source,
            target,
            action_type,
            payload,
            outcome,
            confidence: Confidence::High,
            tags: Vec::new(),
        }
    }

    /// Check if action was successful
    pub fn is_success(&self) -> bool {
        self.outcome.is_success()
    }

    /// Check if this action represents a vulnerability
    pub fn is_vulnerability(&self) -> bool {
        matches!(self.payload, RecordPayload::Vuln(_))
    }

    /// Encode to binary format
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(256);

        // ID (16 bytes)
        buf.extend_from_slice(&self.id);

        // Timestamp
        write_varu64(&mut buf, self.timestamp);

        // Source
        self.source.encode(&mut buf);

        // Target
        self.target.encode(&mut buf);

        // Action type
        buf.push(self.action_type as u8);

        // Payload
        self.payload.encode(&mut buf);

        // Outcome
        self.outcome.encode(&mut buf);

        // Confidence
        buf.push(self.confidence as u8);

        // Tags
        write_varu32(&mut buf, self.tags.len() as u32);
        for tag in &self.tags {
            write_string(&mut buf, tag);
        }

        buf
    }

    /// Decode from binary format
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        // ID
        if bytes.len() < 16 {
            return Err(DecodeError("unexpected eof (id)"));
        }
        let mut id = [0u8; 16];
        id.copy_from_slice(&bytes[..16]);
        let mut pos = 16;

        // Timestamp
        let timestamp = read_varu64(bytes, &mut pos)?;

        // Source
        let source = ActionSource::decode(bytes, &mut pos)?;

        // Target
        let target = Target::decode(bytes, &mut pos)?;

        // Action type
        if pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (action type)"));
        }
        let action_type =
            ActionType::from_u8(bytes[pos]).ok_or(DecodeError("invalid action type"))?;
        pos += 1;

        // Payload
        let payload = RecordPayload::decode(bytes, &mut pos)?;

        // Outcome
        let outcome = ActionOutcome::decode(bytes, &mut pos)?;

        // Confidence
        if pos >= bytes.len() {
            return Err(DecodeError("unexpected eof (confidence)"));
        }
        let confidence =
            Confidence::from_u8(bytes[pos]).ok_or(DecodeError("invalid confidence"))?;
        pos += 1;

        // Tags
        let tag_count = read_varu32(bytes, &mut pos)? as usize;
        let mut tags = Vec::with_capacity(tag_count);
        for _ in 0..tag_count {
            tags.push(read_string(bytes, &mut pos)?.to_string());
        }

        Ok(Self {
            id,
            timestamp,
            source,
            target,
            action_type,
            payload,
            outcome,
            confidence,
            tags,
        })
    }
}

// ==================== Utility Functions ====================

/// Generate a random 16-byte ID
pub fn generate_id() -> [u8; 16] {
    let mut id = [0u8; 16];

    // First 8 bytes: timestamp for ordering
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    id[..8].copy_from_slice(&ts.to_be_bytes());

    // Last 8 bytes: random-ish (using pointer + counter)
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ptr = &id as *const _ as u64;
    let random_part = counter.wrapping_mul(0x517cc1b727220a95) ^ ptr;
    id[8..].copy_from_slice(&random_part.to_le_bytes());

    id
}

/// Get current timestamp in milliseconds
pub fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ==================== IntoActionRecord Trait ====================

/// Trait for types that can be converted to ActionRecord
pub trait IntoActionRecord {
    fn into_action_record(self, source: ActionSource) -> ActionRecord;
}
