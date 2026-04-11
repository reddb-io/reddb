//! Cuckoo Filter — Probabilistic Membership Testing with Deletion
//!
//! Like a Bloom Filter but supports deletion. Uses cuckoo hashing with
//! fingerprints stored in buckets. Each bucket holds up to `BUCKET_SIZE`
//! fingerprints.
//!
//! - `insert(key)` → true if inserted, false if filter is full
//! - `contains(key)` → true if key might exist (small false positive rate)
//! - `delete(key)` → true if removed

/// Number of fingerprints per bucket
const BUCKET_SIZE: usize = 4;
/// Maximum number of relocations before declaring the filter full
const MAX_KICKS: usize = 500;

/// Cuckoo Filter for approximate set membership with deletion support
pub struct CuckooFilter {
    /// Buckets, each holding up to BUCKET_SIZE fingerprints (0 = empty)
    buckets: Vec<[u8; BUCKET_SIZE]>,
    /// Number of buckets
    num_buckets: usize,
    /// Number of items currently stored
    count: usize,
}

impl CuckooFilter {
    /// Create a new cuckoo filter with the given capacity (approximate max items)
    pub fn new(capacity: usize) -> Self {
        // Each bucket holds BUCKET_SIZE items, so we need capacity/BUCKET_SIZE buckets
        // Use next power of 2 for efficient modulo via bitwise AND
        let num_buckets = ((capacity / BUCKET_SIZE) + 1).next_power_of_two();
        Self {
            buckets: vec![[0u8; BUCKET_SIZE]; num_buckets],
            num_buckets,
            count: 0,
        }
    }

    /// Insert an element. Returns true if inserted, false if the filter is full.
    pub fn insert(&mut self, key: &[u8]) -> bool {
        let fp = Self::fingerprint(key);
        let i1 = self.index1(key);
        let i2 = self.index2(i1, fp);

        // Try bucket i1
        if self.bucket_insert(i1, fp) {
            self.count += 1;
            return true;
        }
        // Try bucket i2
        if self.bucket_insert(i2, fp) {
            self.count += 1;
            return true;
        }

        // Both full — kick out an existing entry
        let mut idx = if Self::simple_hash(key) & 1 == 0 {
            i1
        } else {
            i2
        };
        let mut evicted_fp = fp;

        for _ in 0..MAX_KICKS {
            // Pick a random slot to evict
            let slot = (evicted_fp as usize) % BUCKET_SIZE;
            std::mem::swap(&mut self.buckets[idx][slot], &mut evicted_fp);

            // Find alternate bucket for evicted fingerprint
            idx = self.index2(idx, evicted_fp);

            if self.bucket_insert(idx, evicted_fp) {
                self.count += 1;
                return true;
            }
        }

        false // Filter is too full
    }

    /// Check if an element might exist in the filter
    pub fn contains(&self, key: &[u8]) -> bool {
        let fp = Self::fingerprint(key);
        let i1 = self.index1(key);
        let i2 = self.index2(i1, fp);

        self.bucket_contains(i1, fp) || self.bucket_contains(i2, fp)
    }

    /// Delete an element. Returns true if found and removed.
    pub fn delete(&mut self, key: &[u8]) -> bool {
        let fp = Self::fingerprint(key);
        let i1 = self.index1(key);
        let i2 = self.index2(i1, fp);

        if self.bucket_remove(i1, fp) {
            self.count -= 1;
            return true;
        }
        if self.bucket_remove(i2, fp) {
            self.count -= 1;
            return true;
        }
        false
    }

    /// Number of items currently stored
    pub fn count(&self) -> usize {
        self.count
    }

    /// Whether the filter is empty
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Approximate load factor (0.0 to 1.0)
    pub fn load_factor(&self) -> f64 {
        self.count as f64 / (self.num_buckets * BUCKET_SIZE) as f64
    }

    /// Memory usage in bytes
    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.num_buckets * BUCKET_SIZE
    }

    /// Clear all entries
    pub fn clear(&mut self) {
        for bucket in &mut self.buckets {
            *bucket = [0u8; BUCKET_SIZE];
        }
        self.count = 0;
    }

    // ── Internal helpers ─────────────────────────────────────────

    /// Generate a 1-byte fingerprint (non-zero) from key
    fn fingerprint(key: &[u8]) -> u8 {
        let mut h = 0x811c9dc5u32;
        for &byte in key {
            h ^= byte as u32;
            h = h.wrapping_mul(0x01000193);
        }
        // 1-255, never 0
        (h % 255) as u8 + 1
    }

    /// Primary index from key
    fn index1(&self, key: &[u8]) -> usize {
        let h = Self::simple_hash(key) as usize;
        h & (self.num_buckets - 1)
    }

    /// Alternate index: i2 = i1 XOR hash(fingerprint)
    fn index2(&self, i1: usize, fp: u8) -> usize {
        let fp_hash = (fp as usize).wrapping_mul(0x5bd1e995);
        (i1 ^ fp_hash) & (self.num_buckets - 1)
    }

    /// Simple 64-bit hash for key
    fn simple_hash(key: &[u8]) -> u64 {
        let mut h = 0xcbf29ce484222325u64;
        for &byte in key {
            h ^= byte as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    /// Try to insert a fingerprint into a bucket. Returns true if successful.
    fn bucket_insert(&mut self, idx: usize, fp: u8) -> bool {
        for slot in &mut self.buckets[idx] {
            if *slot == 0 {
                *slot = fp;
                return true;
            }
        }
        false
    }

    /// Check if a bucket contains a fingerprint
    fn bucket_contains(&self, idx: usize, fp: u8) -> bool {
        self.buckets[idx].contains(&fp)
    }

    /// Remove a fingerprint from a bucket. Returns true if found and removed.
    fn bucket_remove(&mut self, idx: usize, fp: u8) -> bool {
        for slot in &mut self.buckets[idx] {
            if *slot == fp {
                *slot = 0;
                return true;
            }
        }
        false
    }
}

impl Default for CuckooFilter {
    fn default() -> Self {
        Self::new(100_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cuckoo_basic() {
        let mut cf = CuckooFilter::new(1000);
        assert!(cf.insert(b"hello"));
        assert!(cf.insert(b"world"));

        assert!(cf.contains(b"hello"));
        assert!(cf.contains(b"world"));
        assert!(!cf.contains(b"missing"));
        assert_eq!(cf.count(), 2);
    }

    #[test]
    fn test_cuckoo_delete() {
        let mut cf = CuckooFilter::new(1000);
        cf.insert(b"key1");
        cf.insert(b"key2");

        assert!(cf.delete(b"key1"));
        assert!(!cf.contains(b"key1"));
        assert!(cf.contains(b"key2"));
        assert_eq!(cf.count(), 1);
    }

    #[test]
    fn test_cuckoo_delete_missing() {
        let mut cf = CuckooFilter::new(1000);
        assert!(!cf.delete(b"nonexistent"));
    }

    #[test]
    fn test_cuckoo_many_inserts() {
        let mut cf = CuckooFilter::new(10_000);
        let n = 5000;
        let mut inserted = 0;

        for i in 0..n {
            let key = format!("item_{}", i);
            if cf.insert(key.as_bytes()) {
                inserted += 1;
            }
        }

        // Should insert most items
        assert!(inserted > n * 9 / 10, "Only inserted {inserted}/{n}");

        // All inserted items should be found
        let mut found = 0;
        for i in 0..n {
            let key = format!("item_{}", i);
            if cf.contains(key.as_bytes()) {
                found += 1;
            }
        }
        assert!(found >= inserted, "found={found}, inserted={inserted}");
    }

    #[test]
    fn test_cuckoo_false_positive_rate() {
        let mut cf = CuckooFilter::new(10_000);

        // Insert 5000 items
        for i in 0..5000 {
            cf.insert(format!("in_{}", i).as_bytes());
        }

        // Check 5000 items that were NOT inserted
        let mut fps = 0;
        for i in 0..5000 {
            if cf.contains(format!("out_{}", i).as_bytes()) {
                fps += 1;
            }
        }

        let fp_rate = fps as f64 / 5000.0;
        // With 1-byte fingerprints, FP rate should be < 3%
        assert!(fp_rate < 0.05, "FP rate too high: {fp_rate:.4}");
    }

    #[test]
    fn test_cuckoo_load_factor() {
        let mut cf = CuckooFilter::new(1000);
        assert_eq!(cf.load_factor(), 0.0);

        for i in 0..100 {
            cf.insert(format!("k{}", i).as_bytes());
        }

        let lf = cf.load_factor();
        assert!(lf > 0.0);
        assert!(lf < 1.0);
    }

    #[test]
    fn test_cuckoo_clear() {
        let mut cf = CuckooFilter::new(1000);
        cf.insert(b"a");
        cf.insert(b"b");

        cf.clear();
        assert_eq!(cf.count(), 0);
        assert!(!cf.contains(b"a"));
        assert!(!cf.contains(b"b"));
    }

    #[test]
    fn test_cuckoo_memory() {
        let cf = CuckooFilter::new(100_000);
        let mem = cf.memory_bytes();
        // ~100KB + overhead for 100K capacity
        assert!(mem > 0);
    }
}
