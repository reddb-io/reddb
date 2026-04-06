//! State Machine Segment - Execution state tracking for playbooks and actions.
//!
//! This segment provides state machine capabilities for tracking playbook/action
//! execution state, with valid transitions and stuck detection.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::storage::primitives::encoding::{
    read_string, read_varu32, read_varu64, write_string, write_varu32, write_varu64, DecodeError,
};

// ==================== Execution State ====================

/// Current state of an execution (playbook, action, or session)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecutionState {
    /// Ready to start, not yet executing
    Idle = 0,
    /// Planning or analyzing next action
    Thinking = 1,
    /// Actively running a step/action
    Executing = 2,
    /// Waiting for user input or external event
    WaitingInput = 3,
    /// Successfully finished
    Complete = 4,
    /// Terminated with error
    Error = 5,
    /// Paused by user
    Paused = 6,
    /// Cancelled by user
    Cancelled = 7,
}

impl ExecutionState {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Idle),
            1 => Some(Self::Thinking),
            2 => Some(Self::Executing),
            3 => Some(Self::WaitingInput),
            4 => Some(Self::Complete),
            5 => Some(Self::Error),
            6 => Some(Self::Paused),
            7 => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Thinking => "thinking",
            Self::Executing => "executing",
            Self::WaitingInput => "waiting_input",
            Self::Complete => "complete",
            Self::Error => "error",
            Self::Paused => "paused",
            Self::Cancelled => "cancelled",
        }
    }

    /// Is this a terminal state?
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete | Self::Error | Self::Cancelled)
    }

    /// Is this an active (working) state?
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Thinking | Self::Executing)
    }
}

// ==================== State Transition ====================

/// Record of a state change
#[derive(Debug, Clone)]
pub struct StateTransition {
    /// Previous state
    pub from: ExecutionState,
    /// New state
    pub to: ExecutionState,
    /// Unix timestamp (ms)
    pub timestamp: u64,
    /// Reason for transition
    pub reason: String,
}

impl StateTransition {
    pub fn new(from: ExecutionState, to: ExecutionState, reason: impl Into<String>) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            from,
            to,
            timestamp,
            reason: reason.into(),
        }
    }

    fn encode(&self, buf: &mut Vec<u8>) {
        buf.push(self.from as u8);
        buf.push(self.to as u8);
        write_varu64(buf, self.timestamp);
        write_string(buf, &self.reason);
    }

    fn decode(bytes: &[u8], pos: &mut usize) -> Result<Self, DecodeError> {
        if *pos + 2 > bytes.len() {
            return Err(DecodeError("truncated state transition"));
        }
        let from = ExecutionState::from_u8(bytes[*pos]).ok_or(DecodeError("invalid from state"))?;
        *pos += 1;
        let to = ExecutionState::from_u8(bytes[*pos]).ok_or(DecodeError("invalid to state"))?;
        *pos += 1;
        let timestamp = read_varu64(bytes, pos)?;
        let reason = read_string(bytes, pos)?.to_string();

        Ok(Self {
            from,
            to,
            timestamp,
            reason,
        })
    }
}

// ==================== State Manager ====================

/// Manages execution state with valid transitions and history
#[derive(Debug, Clone)]
pub struct StateManager {
    /// Current state
    pub current: ExecutionState,
    /// Timestamp when current state was entered (ms)
    pub state_entered_at: u64,
    /// Transition history
    pub history: Vec<StateTransition>,
    /// Valid transitions: from_state → allowed_to_states
    valid_transitions: HashMap<ExecutionState, Vec<ExecutionState>>,
    /// Timeout thresholds for stuck detection (ms)
    pub stuck_thresholds: HashMap<ExecutionState, u64>,
}

impl Default for StateManager {
    fn default() -> Self {
        Self::new()
    }
}

impl StateManager {
    /// Create a new state manager with default valid transitions
    pub fn new() -> Self {
        let mut valid_transitions = HashMap::new();

        // Define valid state transitions
        valid_transitions.insert(
            ExecutionState::Idle,
            vec![
                ExecutionState::Thinking,
                ExecutionState::Executing,
                ExecutionState::Cancelled,
            ],
        );
        valid_transitions.insert(
            ExecutionState::Thinking,
            vec![
                ExecutionState::Executing,
                ExecutionState::WaitingInput,
                ExecutionState::Error,
                ExecutionState::Paused,
                ExecutionState::Cancelled,
            ],
        );
        valid_transitions.insert(
            ExecutionState::Executing,
            vec![
                ExecutionState::Thinking,
                ExecutionState::WaitingInput,
                ExecutionState::Complete,
                ExecutionState::Error,
                ExecutionState::Paused,
                ExecutionState::Cancelled,
            ],
        );
        valid_transitions.insert(
            ExecutionState::WaitingInput,
            vec![
                ExecutionState::Thinking,
                ExecutionState::Executing,
                ExecutionState::Error,
                ExecutionState::Cancelled,
            ],
        );
        valid_transitions.insert(
            ExecutionState::Paused,
            vec![
                ExecutionState::Thinking,
                ExecutionState::Executing,
                ExecutionState::Cancelled,
            ],
        );
        // Terminal states have no transitions
        valid_transitions.insert(ExecutionState::Complete, vec![]);
        valid_transitions.insert(ExecutionState::Error, vec![]);
        valid_transitions.insert(ExecutionState::Cancelled, vec![]);

        // Default stuck thresholds
        let mut stuck_thresholds = HashMap::new();
        stuck_thresholds.insert(ExecutionState::Thinking, 30_000); // 30s
        stuck_thresholds.insert(ExecutionState::Executing, 300_000); // 5min
        stuck_thresholds.insert(ExecutionState::WaitingInput, 600_000); // 10min

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            current: ExecutionState::Idle,
            state_entered_at: now,
            history: Vec::new(),
            valid_transitions,
            stuck_thresholds,
        }
    }

    /// Check if a transition is valid
    pub fn can_transition(&self, to: ExecutionState) -> bool {
        self.valid_transitions
            .get(&self.current)
            .map(|allowed| allowed.contains(&to))
            .unwrap_or(false)
    }

    /// Attempt to transition to a new state
    pub fn transition(
        &mut self,
        to: ExecutionState,
        reason: impl Into<String>,
    ) -> Result<(), String> {
        if !self.can_transition(to) {
            return Err(format!(
                "Invalid transition: {} → {}",
                self.current.as_str(),
                to.as_str()
            ));
        }

        let transition = StateTransition::new(self.current, to, reason);
        self.history.push(transition);
        self.current = to;
        self.state_entered_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Ok(())
    }

    /// Force transition (bypass validation) - use sparingly
    pub fn force_transition(&mut self, to: ExecutionState, reason: impl Into<String>) {
        let transition = StateTransition::new(self.current, to, reason);
        self.history.push(transition);
        self.current = to;
        self.state_entered_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
    }

    /// Get time spent in current state
    pub fn time_in_current_state(&self) -> Duration {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Duration::from_millis(now.saturating_sub(self.state_entered_at))
    }

    /// Check if execution appears stuck
    pub fn is_stuck(&self) -> bool {
        if let Some(&threshold) = self.stuck_thresholds.get(&self.current) {
            let elapsed = self.time_in_current_state().as_millis() as u64;
            elapsed > threshold
        } else {
            false
        }
    }

    /// Get stuck state details if stuck
    pub fn stuck_info(&self) -> Option<StuckInfo> {
        if !self.is_stuck() {
            return None;
        }

        let threshold = *self.stuck_thresholds.get(&self.current)?;
        let elapsed = self.time_in_current_state().as_millis() as u64;

        Some(StuckInfo {
            state: self.current,
            elapsed_ms: elapsed,
            threshold_ms: threshold,
            last_transition_reason: self.history.last().map(|t| t.reason.clone()),
        })
    }

    /// Total execution time (from first non-Idle to now or terminal)
    pub fn total_execution_time(&self) -> Duration {
        if self.history.is_empty() {
            return Duration::ZERO;
        }

        let start = self.history.first().map(|t| t.timestamp).unwrap_or(0);
        let end = if self.current.is_terminal() {
            self.state_entered_at
        } else {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64
        };

        Duration::from_millis(end.saturating_sub(start))
    }

    /// Get transition count
    pub fn transition_count(&self) -> usize {
        self.history.len()
    }

    /// Reset to Idle state (clears history)
    pub fn reset(&mut self) {
        self.current = ExecutionState::Idle;
        self.state_entered_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.history.clear();
    }

    /// Serialize state manager
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(self.current as u8);
        write_varu64(&mut buf, self.state_entered_at);
        write_varu32(&mut buf, self.history.len() as u32);
        for t in &self.history {
            t.encode(&mut buf);
        }
        buf
    }

    /// Deserialize state manager
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut pos = 0usize;

        if pos >= bytes.len() {
            return Err(DecodeError("empty state manager"));
        }
        let current =
            ExecutionState::from_u8(bytes[pos]).ok_or(DecodeError("invalid current state"))?;
        pos += 1;

        let state_entered_at = read_varu64(bytes, &mut pos)?;
        let history_len = read_varu32(bytes, &mut pos)? as usize;

        let mut history = Vec::with_capacity(history_len);
        for _ in 0..history_len {
            history.push(StateTransition::decode(bytes, &mut pos)?);
        }

        let mut manager = Self::new();
        manager.current = current;
        manager.state_entered_at = state_entered_at;
        manager.history = history;

        Ok(manager)
    }
}

// ==================== Stuck Detection Info ====================

/// Information about a stuck execution
#[derive(Debug, Clone)]
pub struct StuckInfo {
    pub state: ExecutionState,
    pub elapsed_ms: u64,
    pub threshold_ms: u64,
    pub last_transition_reason: Option<String>,
}

impl StuckInfo {
    pub fn summary(&self) -> String {
        format!(
            "Stuck in {} for {:.1}s (threshold: {:.1}s)",
            self.state.as_str(),
            self.elapsed_ms as f64 / 1000.0,
            self.threshold_ms as f64 / 1000.0
        )
    }
}

// ==================== State Segment ====================

/// Segment for persisting state histories
#[derive(Debug, Default, Clone)]
pub struct StateSegment {
    /// Map of execution_id → StateManager
    states: HashMap<String, StateManager>,
}

impl StateSegment {
    pub fn new() -> Self {
        Self {
            states: HashMap::new(),
        }
    }

    /// Get or create a state manager for an execution
    pub fn get_or_create(&mut self, execution_id: &str) -> &mut StateManager {
        self.states
            .entry(execution_id.to_string())
            .or_insert_with(StateManager::new)
    }

    /// Get state manager for an execution
    pub fn get(&self, execution_id: &str) -> Option<&StateManager> {
        self.states.get(execution_id)
    }

    /// Get mutable state manager
    pub fn get_mut(&mut self, execution_id: &str) -> Option<&mut StateManager> {
        self.states.get_mut(execution_id)
    }

    /// Remove a completed execution's state
    pub fn remove(&mut self, execution_id: &str) -> Option<StateManager> {
        self.states.remove(execution_id)
    }

    /// List all execution IDs
    pub fn execution_ids(&self) -> Vec<&String> {
        self.states.keys().collect()
    }

    /// Find all stuck executions
    pub fn find_stuck(&self) -> Vec<(&str, StuckInfo)> {
        self.states
            .iter()
            .filter_map(|(id, manager)| manager.stuck_info().map(|info| (id.as_str(), info)))
            .collect()
    }

    /// Count by state
    pub fn count_by_state(&self) -> HashMap<ExecutionState, usize> {
        let mut counts = HashMap::new();
        for manager in self.states.values() {
            *counts.entry(manager.current).or_insert(0) += 1;
        }
        counts
    }

    /// Serialize segment
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        write_varu32(&mut buf, self.states.len() as u32);
        for (id, manager) in &self.states {
            write_string(&mut buf, id);
            let manager_bytes = manager.to_bytes();
            write_varu32(&mut buf, manager_bytes.len() as u32);
            buf.extend_from_slice(&manager_bytes);
        }
        buf
    }

    /// Deserialize segment
    pub fn deserialize(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut pos = 0usize;
        let count = read_varu32(bytes, &mut pos)? as usize;

        let mut states = HashMap::with_capacity(count);
        for _ in 0..count {
            let id = read_string(bytes, &mut pos)?.to_string();
            let len = read_varu32(bytes, &mut pos)? as usize;
            if pos + len > bytes.len() {
                return Err(DecodeError("state segment truncated"));
            }
            let manager = StateManager::from_bytes(&bytes[pos..pos + len])?;
            pos += len;
            states.insert(id, manager);
        }

        Ok(Self { states })
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_transitions() {
        let mut manager = StateManager::new();

        assert_eq!(manager.current, ExecutionState::Idle);
        assert!(manager.can_transition(ExecutionState::Thinking));
        assert!(!manager.can_transition(ExecutionState::Complete));

        manager
            .transition(ExecutionState::Thinking, "planning")
            .unwrap();
        assert_eq!(manager.current, ExecutionState::Thinking);
        assert_eq!(manager.transition_count(), 1);

        manager
            .transition(ExecutionState::Executing, "running step 1")
            .unwrap();
        assert_eq!(manager.current, ExecutionState::Executing);

        manager
            .transition(ExecutionState::Complete, "finished")
            .unwrap();
        assert!(manager.current.is_terminal());
    }

    #[test]
    fn test_invalid_transition() {
        let mut manager = StateManager::new();
        let result = manager.transition(ExecutionState::Complete, "skip");
        assert!(result.is_err());
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mut manager = StateManager::new();
        manager
            .transition(ExecutionState::Thinking, "test")
            .unwrap();
        manager
            .transition(ExecutionState::Executing, "run")
            .unwrap();

        let bytes = manager.to_bytes();
        let restored = StateManager::from_bytes(&bytes).unwrap();

        assert_eq!(restored.current, ExecutionState::Executing);
        assert_eq!(restored.history.len(), 2);
    }

    #[test]
    fn test_state_segment() {
        let mut segment = StateSegment::new();

        let manager = segment.get_or_create("playbook-1");
        manager
            .transition(ExecutionState::Thinking, "start")
            .unwrap();

        let manager2 = segment.get_or_create("playbook-2");
        manager2
            .transition(ExecutionState::Executing, "run")
            .unwrap();

        let bytes = segment.serialize();
        let restored = StateSegment::deserialize(&bytes).unwrap();

        assert_eq!(restored.states.len(), 2);
        assert!(restored.get("playbook-1").is_some());
    }
}
