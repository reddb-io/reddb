//! Bitmap Index using Roaring Bitmaps
//!
//! Extremely efficient for low-cardinality columns (status, type, boolean fields).
//! Each distinct column value gets a roaring bitmap of entity offsets, enabling
//! fast AND/OR/NOT operations and instant COUNT queries.
//!
//! # Example
//!
//! For a column `status` with values `["active", "inactive", "pending"]`:
//! - `"active"` → bitmap {0, 1, 5, 7, 12, ...}
//! - `"inactive"` → bitmap {2, 8, 9, ...}
//! - `"pending"` → bitmap {3, 4, 6, 10, 11, ...}
//!
//! `SELECT COUNT(*) WHERE status = 'active'` → `bitmap["active"].len()` — O(1)
//! `WHERE status = 'active' AND role = 'admin'` → `bitmap_and(status_active, role_admin)`

use std::collections::HashMap;
use std::sync::RwLock;

use roaring::RoaringBitmap;

use super::entity::EntityId;

/// A bitmap index for a single column.
///
/// Maps each distinct value to a `RoaringBitmap` of entity offsets (u32).
/// Entity IDs are mapped to u32 offsets via the `id_to_offset` mapping.
pub struct BitmapColumnIndex {
    /// value → bitmap of entity offsets
    bitmaps: HashMap<Vec<u8>, RoaringBitmap>,
    /// EntityId → u32 offset (for bitmap position)
    id_to_offset: HashMap<EntityId, u32>,
    /// u32 offset → EntityId (reverse mapping)
    offset_to_id: Vec<EntityId>,
    /// Column name
    pub column: String,
    /// Next available offset
    next_offset: u32,
}

impl BitmapColumnIndex {
    /// Create a new bitmap column index
    pub fn new(column: impl Into<String>) -> Self {
        Self {
            bitmaps: HashMap::new(),
            id_to_offset: HashMap::new(),
            offset_to_id: Vec::new(),
            column: column.into(),
            next_offset: 0,
        }
    }

    /// Insert an entity with a given column value
    pub fn insert(&mut self, entity_id: EntityId, value: &[u8]) {
        let offset = *self.id_to_offset.entry(entity_id).or_insert_with(|| {
            let off = self.next_offset;
            self.next_offset += 1;
            self.offset_to_id.push(entity_id);
            off
        });
        self.bitmaps
            .entry(value.to_vec())
            .or_default()
            .insert(offset);
    }

    /// Remove an entity from the index (removes from all value bitmaps)
    pub fn remove(&mut self, entity_id: EntityId) {
        if let Some(offset) = self.id_to_offset.remove(&entity_id) {
            for bitmap in self.bitmaps.values_mut() {
                bitmap.remove(offset);
            }
            // Clean up empty bitmaps
            self.bitmaps.retain(|_, bm| !bm.is_empty());
        }
    }

    /// Get entity IDs matching an exact value
    pub fn get(&self, value: &[u8]) -> Vec<EntityId> {
        self.bitmaps
            .get(value)
            .map(|bm| {
                bm.iter()
                    .filter_map(|off| self.offset_to_id.get(off as usize).copied())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get the bitmap for a value (for combining with AND/OR)
    pub fn get_bitmap(&self, value: &[u8]) -> Option<&RoaringBitmap> {
        self.bitmaps.get(value)
    }

    /// Count entities matching an exact value — O(1)
    pub fn count(&self, value: &[u8]) -> u64 {
        self.bitmaps.get(value).map(|bm| bm.len()).unwrap_or(0)
    }

    /// Get all distinct values and their counts
    pub fn value_counts(&self) -> Vec<(Vec<u8>, u64)> {
        self.bitmaps
            .iter()
            .map(|(val, bm)| (val.clone(), bm.len()))
            .collect()
    }

    /// Number of distinct values (cardinality)
    pub fn cardinality(&self) -> usize {
        self.bitmaps.len()
    }

    /// Total number of indexed entities
    pub fn entity_count(&self) -> usize {
        self.id_to_offset.len()
    }

    /// Approximate memory usage in bytes
    pub fn memory_bytes(&self) -> usize {
        let mut size = std::mem::size_of::<Self>();
        for (key, bm) in &self.bitmaps {
            size += key.len() + bm.serialized_size() + 48;
        }
        size += self.id_to_offset.len() * 16; // HashMap overhead
        size += self.offset_to_id.len() * 8; // Vec<EntityId>
        size
    }
}

/// AND two bitmaps, returning entity IDs from the intersection
pub fn bitmap_and(a: &RoaringBitmap, b: &RoaringBitmap) -> RoaringBitmap {
    a & b
}

/// OR two bitmaps, returning entity IDs from the union
pub fn bitmap_or(a: &RoaringBitmap, b: &RoaringBitmap) -> RoaringBitmap {
    a | b
}

/// NOT a bitmap against a universe bitmap
pub fn bitmap_not(universe: &RoaringBitmap, a: &RoaringBitmap) -> RoaringBitmap {
    universe - a
}

/// Resolve bitmap offsets back to EntityIds
pub fn resolve_offsets(bitmap: &RoaringBitmap, offset_to_id: &[EntityId]) -> Vec<EntityId> {
    bitmap
        .iter()
        .filter_map(|off| offset_to_id.get(off as usize).copied())
        .collect()
}

/// Manager for bitmap indices across collections
pub struct BitmapIndexManager {
    /// (collection, column) → BitmapColumnIndex
    indices: RwLock<HashMap<(String, String), BitmapColumnIndex>>,
}

impl BitmapIndexManager {
    /// Create a new manager
    pub fn new() -> Self {
        Self {
            indices: RwLock::new(HashMap::new()),
        }
    }

    /// Create a bitmap index for a column
    pub fn create_index(&self, collection: &str, column: &str) {
        let mut indices = self.indices.write().unwrap();
        let key = (collection.to_string(), column.to_string());
        indices
            .entry(key)
            .or_insert_with(|| BitmapColumnIndex::new(column));
    }

    /// Drop a bitmap index
    pub fn drop_index(&self, collection: &str, column: &str) -> bool {
        let mut indices = self.indices.write().unwrap();
        indices
            .remove(&(collection.to_string(), column.to_string()))
            .is_some()
    }

    /// Insert a value into the index
    pub fn insert(&self, collection: &str, column: &str, entity_id: EntityId, value: &[u8]) {
        let mut indices = self.indices.write().unwrap();
        if let Some(index) = indices.get_mut(&(collection.to_string(), column.to_string())) {
            index.insert(entity_id, value);
        }
    }

    /// Remove an entity from the index
    pub fn remove(&self, collection: &str, column: &str, entity_id: EntityId) {
        let mut indices = self.indices.write().unwrap();
        if let Some(index) = indices.get_mut(&(collection.to_string(), column.to_string())) {
            index.remove(entity_id);
        }
    }

    /// Count entities matching a value — O(1)
    pub fn count(&self, collection: &str, column: &str, value: &[u8]) -> u64 {
        let indices = self.indices.read().unwrap();
        indices
            .get(&(collection.to_string(), column.to_string()))
            .map(|idx| idx.count(value))
            .unwrap_or(0)
    }

    /// Get entity IDs matching a value
    pub fn lookup(&self, collection: &str, column: &str, value: &[u8]) -> Vec<EntityId> {
        let indices = self.indices.read().unwrap();
        indices
            .get(&(collection.to_string(), column.to_string()))
            .map(|idx| idx.get(value))
            .unwrap_or_default()
    }

    /// Get value distribution for a column (for GROUP BY optimization)
    pub fn value_counts(&self, collection: &str, column: &str) -> Vec<(Vec<u8>, u64)> {
        let indices = self.indices.read().unwrap();
        indices
            .get(&(collection.to_string(), column.to_string()))
            .map(|idx| idx.value_counts())
            .unwrap_or_default()
    }

    /// Get stats for a specific bitmap index
    pub fn index_stats(&self, collection: &str, column: &str) -> Option<BitmapIndexStats> {
        let indices = self.indices.read().unwrap();
        indices
            .get(&(collection.to_string(), column.to_string()))
            .map(|idx| BitmapIndexStats {
                column: column.to_string(),
                collection: collection.to_string(),
                cardinality: idx.cardinality(),
                entity_count: idx.entity_count(),
                memory_bytes: idx.memory_bytes(),
            })
    }
}

impl Default for BitmapIndexManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics for a bitmap index
#[derive(Debug, Clone)]
pub struct BitmapIndexStats {
    pub column: String,
    pub collection: String,
    /// Number of distinct values
    pub cardinality: usize,
    /// Total indexed entities
    pub entity_count: usize,
    /// Memory usage in bytes
    pub memory_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitmap_basic() {
        let mut idx = BitmapColumnIndex::new("status");
        idx.insert(EntityId::new(1), b"active");
        idx.insert(EntityId::new(2), b"active");
        idx.insert(EntityId::new(3), b"inactive");
        idx.insert(EntityId::new(4), b"pending");
        idx.insert(EntityId::new(5), b"active");

        assert_eq!(idx.count(b"active"), 3);
        assert_eq!(idx.count(b"inactive"), 1);
        assert_eq!(idx.count(b"pending"), 1);
        assert_eq!(idx.count(b"unknown"), 0);
        assert_eq!(idx.cardinality(), 3);
        assert_eq!(idx.entity_count(), 5);
    }

    #[test]
    fn test_bitmap_get() {
        let mut idx = BitmapColumnIndex::new("role");
        idx.insert(EntityId::new(10), b"admin");
        idx.insert(EntityId::new(20), b"user");
        idx.insert(EntityId::new(30), b"admin");

        let admins = idx.get(b"admin");
        assert_eq!(admins.len(), 2);
        assert!(admins.contains(&EntityId::new(10)));
        assert!(admins.contains(&EntityId::new(30)));
    }

    #[test]
    fn test_bitmap_remove() {
        let mut idx = BitmapColumnIndex::new("status");
        idx.insert(EntityId::new(1), b"active");
        idx.insert(EntityId::new(2), b"active");

        idx.remove(EntityId::new(1));
        assert_eq!(idx.count(b"active"), 1);
        assert_eq!(idx.get(b"active"), vec![EntityId::new(2)]);
    }

    #[test]
    fn test_bitmap_and_or() {
        let mut status_idx = BitmapColumnIndex::new("status");
        status_idx.insert(EntityId::new(1), b"active");
        status_idx.insert(EntityId::new(2), b"active");
        status_idx.insert(EntityId::new(3), b"inactive");

        let mut role_idx = BitmapColumnIndex::new("role");
        role_idx.insert(EntityId::new(1), b"admin");
        role_idx.insert(EntityId::new(2), b"user");
        role_idx.insert(EntityId::new(3), b"admin");

        // active AND admin = entity 1
        let active_bm = status_idx.get_bitmap(b"active").unwrap();
        let admin_bm = role_idx.get_bitmap(b"admin").unwrap();
        let result = bitmap_and(active_bm, admin_bm);
        assert_eq!(result.len(), 1); // Only entity 1 (offset 0)

        // active OR admin
        let union = bitmap_or(active_bm, admin_bm);
        assert_eq!(union.len(), 3); // entities 1,2,3
    }

    #[test]
    fn test_bitmap_value_counts() {
        let mut idx = BitmapColumnIndex::new("color");
        idx.insert(EntityId::new(1), b"red");
        idx.insert(EntityId::new(2), b"blue");
        idx.insert(EntityId::new(3), b"red");
        idx.insert(EntityId::new(4), b"green");

        let counts = idx.value_counts();
        assert_eq!(counts.len(), 3);

        let red_count = counts.iter().find(|(v, _)| v == b"red").map(|(_, c)| *c);
        assert_eq!(red_count, Some(2));
    }

    #[test]
    fn test_bitmap_manager() {
        let mgr = BitmapIndexManager::new();
        mgr.create_index("users", "status");

        mgr.insert("users", "status", EntityId::new(1), b"active");
        mgr.insert("users", "status", EntityId::new(2), b"active");
        mgr.insert("users", "status", EntityId::new(3), b"banned");

        assert_eq!(mgr.count("users", "status", b"active"), 2);
        assert_eq!(mgr.count("users", "status", b"banned"), 1);

        let results = mgr.lookup("users", "status", b"active");
        assert_eq!(results.len(), 2);

        let stats = mgr.index_stats("users", "status").unwrap();
        assert_eq!(stats.cardinality, 2);
        assert_eq!(stats.entity_count, 3);
    }
}
