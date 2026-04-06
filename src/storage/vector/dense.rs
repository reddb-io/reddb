//! Dense Vector Storage
//!
//! Provides packed storage for dense vectors with ID-to-offset mapping.

use super::types::{DenseVector, VectorId};
use std::collections::HashMap;

/// Storage for dense vectors
///
/// Stores vectors in a packed float32 array with O(1) lookup by ID.
pub struct DenseVectorStorage {
    /// Vector dimension
    dim: usize,
    /// Packed vector data (dim * num_vectors floats)
    data: Vec<f32>,
    /// Map from vector ID to index in data array
    id_to_index: HashMap<VectorId, usize>,
    /// Map from index to vector ID (for iteration)
    index_to_id: Vec<VectorId>,
}

impl DenseVectorStorage {
    /// Create new storage with given dimension
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            data: Vec::new(),
            id_to_index: HashMap::new(),
            index_to_id: Vec::new(),
        }
    }

    /// Create with pre-allocated capacity
    pub fn with_capacity(dim: usize, capacity: usize) -> Self {
        Self {
            dim,
            data: Vec::with_capacity(dim * capacity),
            id_to_index: HashMap::with_capacity(capacity),
            index_to_id: Vec::with_capacity(capacity),
        }
    }

    /// Get vector dimension
    #[inline]
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Get number of stored vectors
    #[inline]
    pub fn len(&self) -> usize {
        self.index_to_id.len()
    }

    /// Check if storage is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.index_to_id.is_empty()
    }

    /// Add a vector with given ID
    ///
    /// Returns true if vector was added, false if ID already exists.
    pub fn add(&mut self, id: VectorId, vector: &DenseVector) -> bool {
        if vector.dim() != self.dim {
            return false;
        }

        if self.id_to_index.contains_key(&id) {
            return false;
        }

        let index = self.index_to_id.len();
        self.id_to_index.insert(id, index);
        self.index_to_id.push(id);
        self.data.extend_from_slice(vector.as_slice());

        true
    }

    /// Add multiple vectors in batch
    pub fn add_batch(&mut self, vectors: &[(VectorId, DenseVector)]) -> usize {
        let mut added = 0;
        for (id, vector) in vectors {
            if self.add(*id, vector) {
                added += 1;
            }
        }
        added
    }

    /// Get vector by ID
    pub fn get(&self, id: VectorId) -> Option<DenseVector> {
        let index = *self.id_to_index.get(&id)?;
        let start = index * self.dim;
        let end = start + self.dim;
        Some(DenseVector::new(self.data[start..end].to_vec()))
    }

    /// Get vector slice by ID (zero-copy)
    pub fn get_slice(&self, id: VectorId) -> Option<&[f32]> {
        let index = *self.id_to_index.get(&id)?;
        let start = index * self.dim;
        let end = start + self.dim;
        Some(&self.data[start..end])
    }

    /// Get vector slice by internal index
    #[inline]
    pub fn get_by_index(&self, index: usize) -> Option<&[f32]> {
        if index >= self.index_to_id.len() {
            return None;
        }
        let start = index * self.dim;
        let end = start + self.dim;
        Some(&self.data[start..end])
    }

    /// Get ID at internal index
    #[inline]
    pub fn id_at(&self, index: usize) -> Option<VectorId> {
        self.index_to_id.get(index).copied()
    }

    /// Check if ID exists
    #[inline]
    pub fn contains(&self, id: VectorId) -> bool {
        self.id_to_index.contains_key(&id)
    }

    /// Update vector by ID
    ///
    /// Returns true if updated, false if ID not found or dimension mismatch.
    pub fn update(&mut self, id: VectorId, vector: &DenseVector) -> bool {
        if vector.dim() != self.dim {
            return false;
        }

        if let Some(&index) = self.id_to_index.get(&id) {
            let start = index * self.dim;
            self.data[start..start + self.dim].copy_from_slice(vector.as_slice());
            true
        } else {
            false
        }
    }

    /// Remove vector by ID
    ///
    /// Note: This is O(n) as it requires shifting data.
    /// For frequent deletions, consider using tombstones instead.
    pub fn remove(&mut self, id: VectorId) -> bool {
        if let Some(&index) = self.id_to_index.get(&id) {
            // Remove from data
            let start = index * self.dim;
            self.data.drain(start..start + self.dim);

            // Remove from mappings
            self.id_to_index.remove(&id);
            self.index_to_id.remove(index);

            // Update indices for all vectors after the removed one
            for i in index..self.index_to_id.len() {
                let moved_id = self.index_to_id[i];
                self.id_to_index.insert(moved_id, i);
            }

            true
        } else {
            false
        }
    }

    /// Iterate over all (id, vector) pairs
    pub fn iter(&self) -> impl Iterator<Item = (VectorId, &[f32])> {
        self.index_to_id.iter().enumerate().map(|(idx, &id)| {
            let start = idx * self.dim;
            (id, &self.data[start..start + self.dim])
        })
    }

    /// Get all vector IDs
    pub fn ids(&self) -> &[VectorId] {
        &self.index_to_id
    }

    /// Get raw data buffer (for direct access)
    pub fn raw_data(&self) -> &[f32] {
        &self.data
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Header: dim (4 bytes), count (4 bytes)
        bytes.extend_from_slice(&(self.dim as u32).to_le_bytes());
        bytes.extend_from_slice(&(self.index_to_id.len() as u32).to_le_bytes());

        // IDs (8 bytes each)
        for &id in &self.index_to_id {
            bytes.extend_from_slice(&id.to_le_bytes());
        }

        // Vector data (4 bytes per float)
        for &value in &self.data {
            bytes.extend_from_slice(&value.to_le_bytes());
        }

        bytes
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 {
            return None;
        }

        let dim = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let count = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;

        let expected_len = 8 + count * 8 + count * dim * 4;
        if bytes.len() < expected_len {
            return None;
        }

        let mut offset = 8;

        // Read IDs
        let mut index_to_id = Vec::with_capacity(count);
        let mut id_to_index = HashMap::with_capacity(count);

        for i in 0..count {
            let id = u64::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
                bytes[offset + 4],
                bytes[offset + 5],
                bytes[offset + 6],
                bytes[offset + 7],
            ]);
            index_to_id.push(id);
            id_to_index.insert(id, i);
            offset += 8;
        }

        // Read vector data
        let mut data = Vec::with_capacity(count * dim);
        for _ in 0..(count * dim) {
            let value = f32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]);
            data.push(value);
            offset += 4;
        }

        Some(Self {
            dim,
            data,
            id_to_index,
            index_to_id,
        })
    }

    /// Clear all vectors
    pub fn clear(&mut self) {
        self.data.clear();
        self.id_to_index.clear();
        self.index_to_id.clear();
    }

    /// Reserve capacity for additional vectors
    pub fn reserve(&mut self, additional: usize) {
        self.data.reserve(additional * self.dim);
        self.id_to_index.reserve(additional);
        self.index_to_id.reserve(additional);
    }

    /// Get memory usage in bytes
    pub fn memory_usage(&self) -> usize {
        self.data.len() * 4
            + self.id_to_index.len() * (8 + 8) // HashMap overhead estimate
            + self.index_to_id.len() * 8
    }
}

impl Clone for DenseVectorStorage {
    fn clone(&self) -> Self {
        Self {
            dim: self.dim,
            data: self.data.clone(),
            id_to_index: self.id_to_index.clone(),
            index_to_id: self.index_to_id.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_basic() {
        let mut storage = DenseVectorStorage::new(3);

        let v1 = DenseVector::new(vec![1.0, 2.0, 3.0]);
        let v2 = DenseVector::new(vec![4.0, 5.0, 6.0]);

        assert!(storage.add(1, &v1));
        assert!(storage.add(2, &v2));
        assert_eq!(storage.len(), 2);

        // Duplicate ID should fail
        assert!(!storage.add(1, &v1));
    }

    #[test]
    fn test_storage_get() {
        let mut storage = DenseVectorStorage::new(3);

        let v = DenseVector::new(vec![1.0, 2.0, 3.0]);
        storage.add(42, &v);

        let retrieved = storage.get(42).unwrap();
        assert_eq!(retrieved.as_slice(), &[1.0, 2.0, 3.0]);

        assert!(storage.get(999).is_none());
    }

    #[test]
    fn test_storage_get_slice() {
        let mut storage = DenseVectorStorage::new(3);

        let v = DenseVector::new(vec![1.0, 2.0, 3.0]);
        storage.add(42, &v);

        let slice = storage.get_slice(42).unwrap();
        assert_eq!(slice, &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn test_storage_update() {
        let mut storage = DenseVectorStorage::new(3);

        let v1 = DenseVector::new(vec![1.0, 2.0, 3.0]);
        let v2 = DenseVector::new(vec![4.0, 5.0, 6.0]);

        storage.add(1, &v1);
        assert!(storage.update(1, &v2));

        let retrieved = storage.get(1).unwrap();
        assert_eq!(retrieved.as_slice(), &[4.0, 5.0, 6.0]);

        // Update non-existent ID should fail
        assert!(!storage.update(999, &v2));
    }

    #[test]
    fn test_storage_remove() {
        let mut storage = DenseVectorStorage::new(3);

        let v1 = DenseVector::new(vec![1.0, 2.0, 3.0]);
        let v2 = DenseVector::new(vec![4.0, 5.0, 6.0]);
        let v3 = DenseVector::new(vec![7.0, 8.0, 9.0]);

        storage.add(1, &v1);
        storage.add(2, &v2);
        storage.add(3, &v3);

        assert!(storage.remove(2));
        assert_eq!(storage.len(), 2);
        assert!(!storage.contains(2));
        assert!(storage.contains(1));
        assert!(storage.contains(3));

        // Verify remaining vectors are correct
        assert_eq!(storage.get(1).unwrap().as_slice(), &[1.0, 2.0, 3.0]);
        assert_eq!(storage.get(3).unwrap().as_slice(), &[7.0, 8.0, 9.0]);
    }

    #[test]
    fn test_storage_iter() {
        let mut storage = DenseVectorStorage::new(2);

        storage.add(1, &DenseVector::new(vec![1.0, 2.0]));
        storage.add(2, &DenseVector::new(vec![3.0, 4.0]));

        let items: Vec<_> = storage.iter().collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], (1, &[1.0, 2.0][..]));
        assert_eq!(items[1], (2, &[3.0, 4.0][..]));
    }

    #[test]
    fn test_storage_roundtrip() {
        let mut storage = DenseVectorStorage::new(3);

        storage.add(1, &DenseVector::new(vec![1.0, 2.0, 3.0]));
        storage.add(42, &DenseVector::new(vec![4.0, 5.0, 6.0]));
        storage.add(1000, &DenseVector::new(vec![7.0, 8.0, 9.0]));

        let bytes = storage.to_bytes();
        let recovered = DenseVectorStorage::from_bytes(&bytes).unwrap();

        assert_eq!(recovered.dim(), storage.dim());
        assert_eq!(recovered.len(), storage.len());

        for (id, vec) in storage.iter() {
            let rec_vec = recovered.get_slice(id).unwrap();
            assert_eq!(vec, rec_vec);
        }
    }

    #[test]
    fn test_storage_dimension_mismatch() {
        let mut storage = DenseVectorStorage::new(3);

        let wrong_dim = DenseVector::new(vec![1.0, 2.0]); // 2D instead of 3D
        assert!(!storage.add(1, &wrong_dim));
        assert_eq!(storage.len(), 0);
    }

    #[test]
    fn test_storage_batch_add() {
        let mut storage = DenseVectorStorage::new(2);

        let vectors = vec![
            (1, DenseVector::new(vec![1.0, 2.0])),
            (2, DenseVector::new(vec![3.0, 4.0])),
            (1, DenseVector::new(vec![5.0, 6.0])), // Duplicate, should be skipped
        ];

        let added = storage.add_batch(&vectors);
        assert_eq!(added, 2);
        assert_eq!(storage.len(), 2);
    }

    #[test]
    fn test_storage_clear() {
        let mut storage = DenseVectorStorage::new(2);

        storage.add(1, &DenseVector::new(vec![1.0, 2.0]));
        storage.add(2, &DenseVector::new(vec![3.0, 4.0]));

        storage.clear();
        assert!(storage.is_empty());
        assert_eq!(storage.len(), 0);
    }

    #[test]
    fn test_storage_memory_usage() {
        let mut storage = DenseVectorStorage::new(128);

        for i in 0..100 {
            storage.add(i, &DenseVector::zeros(128));
        }

        let usage = storage.memory_usage();
        // At least 100 * 128 * 4 bytes for the data
        assert!(usage >= 100 * 128 * 4);
    }
}
