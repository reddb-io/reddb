//! In-memory store for probabilistic data structures (HLL, Sketch, Filter)

use std::collections::HashMap;

use parking_lot::RwLock;

use crate::storage::primitives::count_min_sketch::CountMinSketch;
use crate::storage::primitives::cuckoo_filter::CuckooFilter;
use crate::storage::primitives::hyperloglog::HyperLogLog;

/// Central store for all probabilistic data structures.
/// Lives inside `RuntimeInner` and persists for the lifetime of the runtime.
pub struct ProbabilisticStore {
    pub hlls: RwLock<HashMap<String, HyperLogLog>>,
    pub sketches: RwLock<HashMap<String, CountMinSketch>>,
    pub filters: RwLock<HashMap<String, CuckooFilter>>,
}

impl ProbabilisticStore {
    pub fn new() -> Self {
        Self {
            hlls: RwLock::new(HashMap::new()),
            sketches: RwLock::new(HashMap::new()),
            filters: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for ProbabilisticStore {
    fn default() -> Self {
        Self::new()
    }
}
