//! In-memory store for probabilistic data structures (HLL, Sketch, Filter)

use std::collections::{BTreeSet, HashMap};

use parking_lot::{Mutex, RwLock};

use crate::storage::primitives::count_min_sketch::CountMinSketch;
use crate::storage::primitives::cuckoo_filter::CuckooFilter;
use crate::storage::primitives::hyperloglog::HyperLogLog;

/// Central store for all probabilistic data structures.
/// Lives inside `RuntimeInner` and persists for the lifetime of the runtime.
pub struct ProbabilisticStore {
    pub hlls: RwLock<HashMap<String, HyperLogLog>>,
    pub sketches: RwLock<HashMap<String, CountMinSketch>>,
    pub filters: RwLock<HashMap<String, CuckooFilter>>,
    pub(crate) replayed_names: Mutex<BTreeSet<(u8, String)>>,
}

impl ProbabilisticStore {
    pub fn new() -> Self {
        Self {
            hlls: RwLock::new(HashMap::new()),
            sketches: RwLock::new(HashMap::new()),
            filters: RwLock::new(HashMap::new()),
            replayed_names: Mutex::new(BTreeSet::new()),
        }
    }
}

impl Default for ProbabilisticStore {
    fn default() -> Self {
        Self::new()
    }
}
