//! Triple Store Strategies
//!
//! Pluggable indexing strategies for graph/triple storage optimization.
//!
//! # Strategies
//!
//! - **Eager**: Index on add (high write cost, fast reads)
//! - **Lazy**: Index on first query (lazy materialization)
//! - **Minimal**: Minimal indexes for memory-constrained environments
//!
//! # Pattern Classification
//!
//! Queries are classified by which components are bound:
//! - `SUB_PRE_OBJ`: All bound (exact lookup)
//! - `SUB_ANY_ANY`: Subject bound only
//! - `ANY_PRE_OBJ`: Predicate and object bound
//! - etc.
//!
//! # References
//!
//! - Jena TDB `PatternType` classification
//! - Jena `StoreStrategy` for index selection

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Pattern type for triple queries
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PatternType {
    /// Subject, Predicate, Object all bound (exact match)
    SubPreObj,
    /// Subject, Predicate bound
    SubPre,
    /// Subject, Object bound
    SubObj,
    /// Predicate, Object bound
    PreObj,
    /// Only Subject bound
    Sub,
    /// Only Predicate bound
    Pre,
    /// Only Object bound
    Obj,
    /// Nothing bound (full scan)
    Any,
}

impl PatternType {
    /// Create pattern type from bound flags
    pub fn from_bounds(sub: bool, pre: bool, obj: bool) -> Self {
        match (sub, pre, obj) {
            (true, true, true) => PatternType::SubPreObj,
            (true, true, false) => PatternType::SubPre,
            (true, false, true) => PatternType::SubObj,
            (false, true, true) => PatternType::PreObj,
            (true, false, false) => PatternType::Sub,
            (false, true, false) => PatternType::Pre,
            (false, false, true) => PatternType::Obj,
            (false, false, false) => PatternType::Any,
        }
    }

    /// Get selectivity estimate (lower = more selective)
    pub fn selectivity(&self) -> f64 {
        match self {
            PatternType::SubPreObj => 0.001, // Most selective
            PatternType::SubPre => 0.01,
            PatternType::SubObj => 0.02,
            PatternType::PreObj => 0.03,
            PatternType::Sub => 0.1,
            PatternType::Pre => 0.3,
            PatternType::Obj => 0.2,
            PatternType::Any => 1.0, // Full scan
        }
    }

    /// Get recommended index for this pattern
    pub fn recommended_index(&self) -> IndexType {
        match self {
            PatternType::SubPreObj => IndexType::SPO,
            PatternType::SubPre => IndexType::SPO,
            PatternType::SubObj => IndexType::SOP,
            PatternType::PreObj => IndexType::POS,
            PatternType::Sub => IndexType::SPO,
            PatternType::Pre => IndexType::POS,
            PatternType::Obj => IndexType::OPS,
            PatternType::Any => IndexType::SPO,
        }
    }
}

/// Index type (triple ordering)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexType {
    /// Subject-Predicate-Object (primary)
    SPO,
    /// Subject-Object-Predicate
    SOP,
    /// Predicate-Object-Subject
    POS,
    /// Predicate-Subject-Object
    PSO,
    /// Object-Predicate-Subject
    OPS,
    /// Object-Subject-Predicate
    OSP,
}

impl IndexType {
    /// All possible index orderings
    pub fn all() -> &'static [IndexType] {
        &[
            IndexType::SPO,
            IndexType::SOP,
            IndexType::POS,
            IndexType::PSO,
            IndexType::OPS,
            IndexType::OSP,
        ]
    }

    /// Get key ordering for this index
    pub fn key_order(&self) -> (usize, usize, usize) {
        match self {
            IndexType::SPO => (0, 1, 2),
            IndexType::SOP => (0, 2, 1),
            IndexType::POS => (1, 2, 0),
            IndexType::PSO => (1, 0, 2),
            IndexType::OPS => (2, 1, 0),
            IndexType::OSP => (2, 0, 1),
        }
    }
}

/// Store strategy trait
pub trait StoreStrategy: Send + Sync {
    /// Get strategy name
    fn name(&self) -> &str;

    /// Get indexes to maintain
    fn indexes(&self) -> &[IndexType];

    /// Should index on add?
    fn index_on_add(&self) -> bool;

    /// Select best index for pattern
    fn select_index(&self, pattern: PatternType) -> IndexType;

    /// Get estimated cost for pattern with this strategy
    fn estimate_cost(&self, pattern: PatternType) -> f64;
}

/// Eager indexing strategy - index on every add
#[derive(Debug, Clone)]
pub struct EagerStoreStrategy {
    /// Indexes to maintain
    indexes: Vec<IndexType>,
}

impl EagerStoreStrategy {
    /// Create with default indexes (SPO, POS, OSP for coverage)
    pub fn new() -> Self {
        Self {
            indexes: vec![IndexType::SPO, IndexType::POS, IndexType::OSP],
        }
    }

    /// Create with full indexes (all 6 orderings)
    pub fn full() -> Self {
        Self {
            indexes: IndexType::all().to_vec(),
        }
    }

    /// Create with custom indexes
    pub fn with_indexes(indexes: Vec<IndexType>) -> Self {
        Self { indexes }
    }
}

impl Default for EagerStoreStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreStrategy for EagerStoreStrategy {
    fn name(&self) -> &str {
        "Eager"
    }

    fn indexes(&self) -> &[IndexType] {
        &self.indexes
    }

    fn index_on_add(&self) -> bool {
        true
    }

    fn select_index(&self, pattern: PatternType) -> IndexType {
        let preferred = pattern.recommended_index();
        if self.indexes.contains(&preferred) {
            preferred
        } else {
            // Fall back to first available
            self.indexes.first().copied().unwrap_or(IndexType::SPO)
        }
    }

    fn estimate_cost(&self, pattern: PatternType) -> f64 {
        // Eager has optimal read cost
        pattern.selectivity()
    }
}

/// Lazy indexing strategy - index on first query
#[derive(Debug)]
pub struct LazyStoreStrategy {
    /// Primary index (always available)
    primary: IndexType,
    /// Secondary indexes (built lazily)
    secondary: RwLock<Vec<IndexType>>,
    /// Indexes already materialized
    materialized: RwLock<Vec<IndexType>>,
}

impl LazyStoreStrategy {
    /// Create with SPO as primary
    pub fn new() -> Self {
        Self {
            primary: IndexType::SPO,
            secondary: RwLock::new(vec![
                IndexType::POS,
                IndexType::OSP,
                IndexType::SOP,
                IndexType::PSO,
                IndexType::OPS,
            ]),
            materialized: RwLock::new(vec![IndexType::SPO]),
        }
    }

    /// Check if index is materialized
    pub fn is_materialized(&self, index: IndexType) -> bool {
        self.materialized.read().unwrap().contains(&index)
    }

    /// Mark index as materialized
    pub fn mark_materialized(&self, index: IndexType) {
        let mut mat = self.materialized.write().unwrap();
        if !mat.contains(&index) {
            mat.push(index);
        }
    }
}

impl Default for LazyStoreStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreStrategy for LazyStoreStrategy {
    fn name(&self) -> &str {
        "Lazy"
    }

    fn indexes(&self) -> &[IndexType] {
        // Only return currently materialized indexes
        // Note: This returns a static slice, so we return primary only
        std::slice::from_ref(&self.primary)
    }

    fn index_on_add(&self) -> bool {
        false // Only index primary on add
    }

    fn select_index(&self, pattern: PatternType) -> IndexType {
        let preferred = pattern.recommended_index();
        if self.is_materialized(preferred) {
            preferred
        } else {
            // Use primary, but note we should materialize preferred later
            self.primary
        }
    }

    fn estimate_cost(&self, pattern: PatternType) -> f64 {
        let preferred = pattern.recommended_index();
        if self.is_materialized(preferred) {
            pattern.selectivity()
        } else {
            // Higher cost when using non-optimal index
            pattern.selectivity() * 10.0
        }
    }
}

/// Minimal indexing strategy - memory-constrained
#[derive(Debug, Clone)]
pub struct MinimalStoreStrategy {
    /// Single index to maintain
    index: IndexType,
}

impl MinimalStoreStrategy {
    /// Create with SPO index
    pub fn new() -> Self {
        Self {
            index: IndexType::SPO,
        }
    }

    /// Create with custom index
    pub fn with_index(index: IndexType) -> Self {
        Self { index }
    }
}

impl Default for MinimalStoreStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreStrategy for MinimalStoreStrategy {
    fn name(&self) -> &str {
        "Minimal"
    }

    fn indexes(&self) -> &[IndexType] {
        std::slice::from_ref(&self.index)
    }

    fn index_on_add(&self) -> bool {
        true
    }

    fn select_index(&self, _pattern: PatternType) -> IndexType {
        self.index // Always use the single index
    }

    fn estimate_cost(&self, pattern: PatternType) -> f64 {
        // Higher cost for patterns that don't match our single index
        let optimal = pattern.recommended_index();
        if optimal == self.index {
            pattern.selectivity()
        } else {
            // Scan penalty
            pattern.selectivity() * 5.0
        }
    }
}

/// Index statistics
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    /// Number of entries
    pub entries: u64,
    /// Number of lookups
    pub lookups: u64,
    /// Number of scans
    pub scans: u64,
    /// Number of inserts
    pub inserts: u64,
    /// Cache hits
    pub cache_hits: u64,
    /// Cache misses
    pub cache_misses: u64,
}

impl IndexStats {
    /// Calculate hit rate
    pub fn hit_rate(&self) -> f64 {
        let total = self.cache_hits + self.cache_misses;
        if total == 0 {
            0.0
        } else {
            self.cache_hits as f64 / total as f64
        }
    }
}

/// Triple index (generic for any ordering)
pub struct TripleIndex {
    /// Index type
    index_type: IndexType,
    /// Storage: key -> values
    /// Key is composite of first two elements, value is third
    data: RwLock<HashMap<(String, String), Vec<String>>>,
    /// Statistics
    stats: RwLock<IndexStats>,
}

impl TripleIndex {
    /// Create new index
    pub fn new(index_type: IndexType) -> Self {
        Self {
            index_type,
            data: RwLock::new(HashMap::new()),
            stats: RwLock::new(IndexStats::default()),
        }
    }

    /// Insert triple
    pub fn insert(&self, subject: &str, predicate: &str, object: &str) {
        let (k1, k2, v) = self.order_triple(subject, predicate, object);
        let key = (k1.to_string(), k2.to_string());

        let mut data = self.data.write().unwrap();
        data.entry(key).or_insert_with(Vec::new).push(v.to_string());

        let mut stats = self.stats.write().unwrap();
        stats.inserts += 1;
        stats.entries += 1;
    }

    /// Lookup with prefix
    pub fn lookup(&self, first: &str, second: Option<&str>) -> Vec<(String, String, String)> {
        let data = self.data.read().unwrap();
        let mut results = Vec::new();

        let mut stats = self.stats.write().unwrap();
        stats.lookups += 1;

        for ((k1, k2), values) in data.iter() {
            if k1 == first {
                if let Some(s) = second {
                    if k2 == s {
                        for v in values {
                            results.push(self.restore_triple(k1, k2, v));
                        }
                    }
                } else {
                    for v in values {
                        results.push(self.restore_triple(k1, k2, v));
                    }
                }
            }
        }

        results
    }

    /// Scan all entries
    pub fn scan(&self) -> Vec<(String, String, String)> {
        let data = self.data.read().unwrap();
        let mut results = Vec::new();

        let mut stats = self.stats.write().unwrap();
        stats.scans += 1;

        for ((k1, k2), values) in data.iter() {
            for v in values {
                results.push(self.restore_triple(k1, k2, v));
            }
        }

        results
    }

    /// Get statistics
    pub fn stats(&self) -> IndexStats {
        self.stats.read().unwrap().clone()
    }

    /// Order triple according to index type
    fn order_triple<'a>(&self, s: &'a str, p: &'a str, o: &'a str) -> (&'a str, &'a str, &'a str) {
        let parts = [s, p, o];
        let (i1, i2, i3) = self.index_type.key_order();
        (parts[i1], parts[i2], parts[i3])
    }

    /// Restore original triple order from index order
    fn restore_triple(&self, k1: &str, k2: &str, v: &str) -> (String, String, String) {
        let (i1, i2, i3) = self.index_type.key_order();
        let mut result = ["", "", ""];
        result[i1] = k1;
        result[i2] = k2;
        result[i3] = v;
        (
            result[0].to_string(),
            result[1].to_string(),
            result[2].to_string(),
        )
    }
}

/// Triple store with pluggable strategy
pub struct TripleStore {
    /// Indexing strategy
    strategy: Arc<dyn StoreStrategy>,
    /// Indexes
    indexes: RwLock<HashMap<IndexType, Arc<TripleIndex>>>,
}

impl TripleStore {
    /// Create with strategy
    pub fn new(strategy: Arc<dyn StoreStrategy>) -> Self {
        let mut indexes = HashMap::new();

        // Create indexes according to strategy
        for &idx_type in strategy.indexes() {
            indexes.insert(idx_type, Arc::new(TripleIndex::new(idx_type)));
        }

        Self {
            strategy,
            indexes: RwLock::new(indexes),
        }
    }

    /// Create with eager strategy
    pub fn eager() -> Self {
        Self::new(Arc::new(EagerStoreStrategy::new()))
    }

    /// Create with lazy strategy
    pub fn lazy() -> Self {
        Self::new(Arc::new(LazyStoreStrategy::new()))
    }

    /// Create with minimal strategy
    pub fn minimal() -> Self {
        Self::new(Arc::new(MinimalStoreStrategy::new()))
    }

    /// Add triple
    pub fn add(&self, subject: &str, predicate: &str, object: &str) {
        if self.strategy.index_on_add() {
            let indexes = self.indexes.read().unwrap();
            for index in indexes.values() {
                index.insert(subject, predicate, object);
            }
        } else {
            // Lazy: only update primary index
            let indexes = self.indexes.read().unwrap();
            if let Some(primary) = indexes.values().next() {
                primary.insert(subject, predicate, object);
            }
        }
    }

    /// Query with pattern
    pub fn query(
        &self,
        subject: Option<&str>,
        predicate: Option<&str>,
        object: Option<&str>,
    ) -> Vec<(String, String, String)> {
        let pattern =
            PatternType::from_bounds(subject.is_some(), predicate.is_some(), object.is_some());

        let index_type = self.strategy.select_index(pattern);
        let indexes = self.indexes.read().unwrap();

        if let Some(index) = indexes.get(&index_type) {
            // Use appropriate lookup based on pattern
            match pattern {
                PatternType::SubPreObj => {
                    // Exact match
                    index
                        .lookup(subject.unwrap(), Some(predicate.unwrap()))
                        .into_iter()
                        .filter(|(_, _, o)| o == object.unwrap())
                        .collect()
                }
                PatternType::SubPre => index.lookup(subject.unwrap(), Some(predicate.unwrap())),
                PatternType::Sub => index.lookup(subject.unwrap(), None),
                _ => {
                    // Fall back to scan with filter
                    index
                        .scan()
                        .into_iter()
                        .filter(|(s, p, o)| {
                            subject.map_or(true, |sub| s == sub)
                                && predicate.map_or(true, |pre| p == pre)
                                && object.map_or(true, |obj| o == obj)
                        })
                        .collect()
                }
            }
        } else {
            Vec::new()
        }
    }

    /// Get strategy name
    pub fn strategy_name(&self) -> &str {
        self.strategy.name()
    }

    /// Get index statistics
    pub fn index_stats(&self) -> HashMap<IndexType, IndexStats> {
        let indexes = self.indexes.read().unwrap();
        indexes.iter().map(|(&k, v)| (k, v.stats())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pattern_type() {
        assert_eq!(
            PatternType::from_bounds(true, true, true),
            PatternType::SubPreObj
        );
        assert_eq!(
            PatternType::from_bounds(true, false, false),
            PatternType::Sub
        );
        assert_eq!(
            PatternType::from_bounds(false, false, false),
            PatternType::Any
        );
    }

    #[test]
    fn test_eager_strategy() {
        let strategy = EagerStoreStrategy::new();
        assert_eq!(strategy.name(), "Eager");
        assert!(strategy.index_on_add());
        assert_eq!(strategy.indexes().len(), 3);
    }

    #[test]
    fn test_minimal_strategy() {
        let strategy = MinimalStoreStrategy::new();
        assert_eq!(strategy.name(), "Minimal");
        assert_eq!(strategy.indexes().len(), 1);
    }

    #[test]
    fn test_triple_index() {
        let index = TripleIndex::new(IndexType::SPO);

        index.insert("alice", "knows", "bob");
        index.insert("alice", "knows", "charlie");
        index.insert("bob", "knows", "alice");

        let results = index.lookup("alice", None);
        assert_eq!(results.len(), 2);

        let results = index.lookup("alice", Some("knows"));
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_triple_store_eager() {
        let store = TripleStore::eager();

        store.add("alice", "knows", "bob");
        store.add("alice", "likes", "coffee");
        store.add("bob", "knows", "charlie");

        let results = store.query(Some("alice"), None, None);
        assert_eq!(results.len(), 2);

        let results = store.query(Some("alice"), Some("knows"), None);
        assert_eq!(results.len(), 1);

        let results = store.query(None, Some("knows"), None);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_triple_store_minimal() {
        let store = TripleStore::minimal();

        store.add("alice", "knows", "bob");
        store.add("bob", "knows", "charlie");

        // Minimal still works but may be slower for some patterns
        let results = store.query(Some("alice"), None, None);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_index_type_ordering() {
        let spo = IndexType::SPO;
        assert_eq!(spo.key_order(), (0, 1, 2));

        let pos = IndexType::POS;
        assert_eq!(pos.key_order(), (1, 2, 0));
    }

    #[test]
    fn test_pattern_selectivity() {
        assert!(PatternType::SubPreObj.selectivity() < PatternType::Sub.selectivity());
        assert!(PatternType::Sub.selectivity() < PatternType::Any.selectivity());
    }
}
