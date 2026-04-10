//! In-memory store for probabilistic data structures (HLL, Sketch, Filter)

use std::collections::HashMap;
use std::sync::RwLock;

use crate::storage::primitives::hyperloglog::HyperLogLog;

/// Central store for all probabilistic data structures.
/// Lives inside `RuntimeInner` and persists for the lifetime of the runtime.
pub struct ProbabilisticStore {
    pub hlls: RwLock<HashMap<String, HyperLogLog>>,
    // Sketch and Filter stores will be added in Phases 7 and 8
}

impl ProbabilisticStore {
    pub fn new() -> Self {
        Self {
            hlls: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for ProbabilisticStore {
    fn default() -> Self {
        Self::new()
    }
}
