//! ActionSegment - Indexed storage for action records
//!
//! Provides efficient querying by target, type, and outcome.

use std::collections::HashMap;

use crate::storage::primitives::encoding::{read_varu32, write_varu32, DecodeError};

use super::record::ActionRecord;
use super::types::{ActionType, Target};

// ==================== TargetKey ====================

/// Key for target-based indexing
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TargetKey(String);

impl From<&Target> for TargetKey {
    fn from(target: &Target) -> Self {
        TargetKey(target.host_str())
    }
}

// ==================== ActionSegment ====================

/// Segment for storing all action records with indices
#[derive(Debug)]
pub struct ActionSegment {
    records: Vec<ActionRecord>,
    by_target: HashMap<TargetKey, Vec<usize>>,
    by_type: HashMap<ActionType, Vec<usize>>,
    by_outcome: HashMap<u8, Vec<usize>>, // 0=success, 1=failed, 2=timeout, 3=partial, 4=skipped
}

impl ActionSegment {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            by_target: HashMap::new(),
            by_type: HashMap::new(),
            by_outcome: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Add an action record
    pub fn add(&mut self, record: ActionRecord) {
        let idx = self.records.len();

        // Index by target
        let target_key = TargetKey::from(&record.target);
        self.by_target.entry(target_key).or_default().push(idx);

        // Index by type
        self.by_type
            .entry(record.action_type)
            .or_default()
            .push(idx);

        // Index by outcome
        let outcome_key = record.outcome.discriminant();
        self.by_outcome.entry(outcome_key).or_default().push(idx);

        self.records.push(record);
    }

    /// Get all records for a target
    pub fn by_target(&self, target: &Target) -> Vec<&ActionRecord> {
        let key = TargetKey::from(target);
        self.by_target
            .get(&key)
            .map(|indices| indices.iter().map(|&i| &self.records[i]).collect())
            .unwrap_or_default()
    }

    /// Get all records of a specific type
    pub fn by_type(&self, action_type: ActionType) -> Vec<&ActionRecord> {
        self.by_type
            .get(&action_type)
            .map(|indices| indices.iter().map(|&i| &self.records[i]).collect())
            .unwrap_or_default()
    }

    /// Get all successful records
    pub fn successes(&self) -> Vec<&ActionRecord> {
        self.by_outcome
            .get(&0)
            .map(|indices| indices.iter().map(|&i| &self.records[i]).collect())
            .unwrap_or_default()
    }

    /// Get all failed records
    pub fn failures(&self) -> Vec<&ActionRecord> {
        self.by_outcome
            .get(&1)
            .map(|indices| indices.iter().map(|&i| &self.records[i]).collect())
            .unwrap_or_default()
    }

    /// Get all timeout records
    pub fn timeouts(&self) -> Vec<&ActionRecord> {
        self.by_outcome
            .get(&2)
            .map(|indices| indices.iter().map(|&i| &self.records[i]).collect())
            .unwrap_or_default()
    }

    /// Get all records
    pub fn all(&self) -> &[ActionRecord] {
        &self.records
    }

    /// Get a record by ID
    pub fn by_id(&self, id: &[u8; 16]) -> Option<&ActionRecord> {
        self.records.iter().find(|r| &r.id == id)
    }

    /// Get records within a time range
    pub fn by_time_range(&self, start: u64, end: u64) -> Vec<&ActionRecord> {
        self.records
            .iter()
            .filter(|r| r.timestamp >= start && r.timestamp <= end)
            .collect()
    }

    /// Get most recent N records
    pub fn recent(&self, n: usize) -> Vec<&ActionRecord> {
        let start = self.records.len().saturating_sub(n);
        self.records[start..].iter().collect()
    }

    /// Serialize the segment
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Write record count
        write_varu32(&mut buf, self.records.len() as u32);

        // Write each record with length prefix
        for record in &self.records {
            let encoded = record.encode();
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
                return Err(DecodeError("action record out of bounds"));
            }
            let record = ActionRecord::decode(&bytes[pos..pos + len])?;
            pos += len;
            segment.add(record);
        }

        Ok(segment)
    }
}

impl Default for ActionSegment {
    fn default() -> Self {
        Self::new()
    }
}
