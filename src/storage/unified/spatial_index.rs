//! R-Tree Spatial Index
//!
//! Provides efficient spatial queries on GeoPoint, Latitude, and Longitude data.
//! Uses the `rstar` crate for R-tree implementation.
//!
//! # Supported queries
//! - **Radius search**: Find all points within X km of a center point
//! - **Bounding box search**: Find all points within a lat/lon rectangle
//! - **Nearest-K search**: Find the K closest points to a location

use std::collections::HashMap;
use std::sync::RwLock;

use rstar::{primitives::GeomWithData, RTree, AABB};

use super::entity::EntityId;

/// A 2D point in the R-tree, storing (lon, lat) in degrees with an associated EntityId.
/// Note: rstar uses [x, y] convention, so we store (longitude, latitude).
type SpatialEntry = GeomWithData<[f64; 2], EntityId>;

/// Build a spatial entry from lat/lon (degrees) and entity ID
fn make_entry(lat: f64, lon: f64, entity_id: EntityId) -> SpatialEntry {
    GeomWithData::new([lon, lat], entity_id)
}

/// Earth radius in kilometers
const EARTH_RADIUS_KM: f64 = 6371.0;

/// Haversine distance between two points in degrees, returns km
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let lat1_r = lat1.to_radians();
    let lat2_r = lat2.to_radians();

    let a = (dlat / 2.0).sin().powi(2) + lat1_r.cos() * lat2_r.cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    EARTH_RADIUS_KM * c
}

/// Convert a radius in km to approximate degrees (for bounding box pre-filter)
fn km_to_approx_degrees(km: f64) -> f64 {
    km / 111.32 // 1 degree ≈ 111.32 km at equator
}

/// Result of a spatial search
#[derive(Debug, Clone)]
pub struct SpatialSearchResult {
    pub entity_id: EntityId,
    pub distance_km: f64,
}

/// A spatial index for a single collection + column
pub struct SpatialIndex {
    tree: RTree<SpatialEntry>,
    /// EntityId → (lat, lon) for removal and update
    points: HashMap<EntityId, (f64, f64)>,
    /// Column name
    pub column: String,
}

impl SpatialIndex {
    /// Create a new spatial index
    pub fn new(column: impl Into<String>) -> Self {
        Self {
            tree: RTree::new(),
            points: HashMap::new(),
            column: column.into(),
        }
    }

    /// Bulk-load from a list of (entity_id, lat, lon)
    pub fn bulk_load(column: impl Into<String>, data: Vec<(EntityId, f64, f64)>) -> Self {
        let mut points = HashMap::with_capacity(data.len());
        let entries: Vec<SpatialEntry> = data
            .into_iter()
            .map(|(id, lat, lon)| {
                points.insert(id, (lat, lon));
                make_entry(lat, lon, id)
            })
            .collect();
        Self {
            tree: RTree::bulk_load(entries),
            points,
            column: column.into(),
        }
    }

    /// Insert a point
    pub fn insert(&mut self, entity_id: EntityId, lat: f64, lon: f64) {
        // Remove old entry if exists
        if let Some((old_lat, old_lon)) = self.points.remove(&entity_id) {
            self.tree.remove(&make_entry(old_lat, old_lon, entity_id));
        }
        self.tree.insert(make_entry(lat, lon, entity_id));
        self.points.insert(entity_id, (lat, lon));
    }

    /// Remove a point
    pub fn remove(&mut self, entity_id: EntityId) -> bool {
        if let Some((lat, lon)) = self.points.remove(&entity_id) {
            self.tree.remove(&make_entry(lat, lon, entity_id));
            true
        } else {
            false
        }
    }

    /// Search within a radius (km) from a center point.
    /// Returns results sorted by distance ascending.
    pub fn search_radius(
        &self,
        center_lat: f64,
        center_lon: f64,
        radius_km: f64,
        limit: usize,
    ) -> Vec<SpatialSearchResult> {
        // Pre-filter with a bounding box in degrees
        let deg = km_to_approx_degrees(radius_km) * 1.2; // 20% margin for safety
        let aabb = AABB::from_corners(
            [center_lon - deg, center_lat - deg],
            [center_lon + deg, center_lat + deg],
        );

        let mut results: Vec<SpatialSearchResult> = self
            .tree
            .locate_in_envelope(&aabb)
            .filter_map(|entry| {
                let [lon, lat] = *entry.geom();
                let dist = haversine_km(center_lat, center_lon, lat, lon);
                if dist <= radius_km {
                    Some(SpatialSearchResult {
                        entity_id: entry.data,
                        distance_km: dist,
                    })
                } else {
                    None
                }
            })
            .collect();

        results.sort_by(|a, b| a.distance_km.partial_cmp(&b.distance_km).unwrap());
        results.truncate(limit);
        results
    }

    /// Search within a bounding box (min_lat, min_lon, max_lat, max_lon)
    pub fn search_bbox(
        &self,
        min_lat: f64,
        min_lon: f64,
        max_lat: f64,
        max_lon: f64,
        limit: usize,
    ) -> Vec<SpatialSearchResult> {
        let aabb = AABB::from_corners([min_lon, min_lat], [max_lon, max_lat]);

        self.tree
            .locate_in_envelope(&aabb)
            .take(limit)
            .map(|entry| SpatialSearchResult {
                entity_id: entry.data,
                distance_km: 0.0, // No reference point for bbox
            })
            .collect()
    }

    /// Find the K nearest points to a location
    pub fn search_nearest(&self, lat: f64, lon: f64, k: usize) -> Vec<SpatialSearchResult> {
        self.tree
            .nearest_neighbor_iter(&[lon, lat])
            .take(k)
            .map(|entry| {
                let [elon, elat] = *entry.geom();
                SpatialSearchResult {
                    entity_id: entry.data,
                    distance_km: haversine_km(lat, lon, elat, elon),
                }
            })
            .collect()
    }

    /// Number of indexed points
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Whether the index is empty
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Approximate memory usage
    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.points.len() * 32 // HashMap overhead
            + self.tree.size() * std::mem::size_of::<SpatialEntry>()
    }
}

/// Manager for spatial indices across collections
pub struct SpatialIndexManager {
    /// (collection, column) → SpatialIndex
    indices: RwLock<HashMap<(String, String), SpatialIndex>>,
}

impl SpatialIndexManager {
    pub fn new() -> Self {
        Self {
            indices: RwLock::new(HashMap::new()),
        }
    }

    /// Create a spatial index
    pub fn create_index(&self, collection: &str, column: &str) {
        let mut indices = self.indices.write().unwrap();
        let key = (collection.to_string(), column.to_string());
        indices
            .entry(key)
            .or_insert_with(|| SpatialIndex::new(column));
    }

    /// Drop a spatial index
    pub fn drop_index(&self, collection: &str, column: &str) -> bool {
        let mut indices = self.indices.write().unwrap();
        indices
            .remove(&(collection.to_string(), column.to_string()))
            .is_some()
    }

    /// Insert a point
    pub fn insert(&self, collection: &str, column: &str, entity_id: EntityId, lat: f64, lon: f64) {
        let mut indices = self.indices.write().unwrap();
        if let Some(index) = indices.get_mut(&(collection.to_string(), column.to_string())) {
            index.insert(entity_id, lat, lon);
        }
    }

    /// Remove a point
    pub fn remove(&self, collection: &str, column: &str, entity_id: EntityId) {
        let mut indices = self.indices.write().unwrap();
        if let Some(index) = indices.get_mut(&(collection.to_string(), column.to_string())) {
            index.remove(entity_id);
        }
    }

    /// Search within a radius
    pub fn search_radius(
        &self,
        collection: &str,
        column: &str,
        center_lat: f64,
        center_lon: f64,
        radius_km: f64,
        limit: usize,
    ) -> Vec<SpatialSearchResult> {
        let indices = self.indices.read().unwrap();
        indices
            .get(&(collection.to_string(), column.to_string()))
            .map(|idx| idx.search_radius(center_lat, center_lon, radius_km, limit))
            .unwrap_or_default()
    }

    /// Search within a bounding box
    pub fn search_bbox(
        &self,
        collection: &str,
        column: &str,
        min_lat: f64,
        min_lon: f64,
        max_lat: f64,
        max_lon: f64,
        limit: usize,
    ) -> Vec<SpatialSearchResult> {
        let indices = self.indices.read().unwrap();
        indices
            .get(&(collection.to_string(), column.to_string()))
            .map(|idx| idx.search_bbox(min_lat, min_lon, max_lat, max_lon, limit))
            .unwrap_or_default()
    }

    /// Find K nearest points
    pub fn search_nearest(
        &self,
        collection: &str,
        column: &str,
        lat: f64,
        lon: f64,
        k: usize,
    ) -> Vec<SpatialSearchResult> {
        let indices = self.indices.read().unwrap();
        indices
            .get(&(collection.to_string(), column.to_string()))
            .map(|idx| idx.search_nearest(lat, lon, k))
            .unwrap_or_default()
    }

    /// Get stats
    pub fn index_stats(&self, collection: &str, column: &str) -> Option<SpatialIndexStats> {
        let indices = self.indices.read().unwrap();
        indices
            .get(&(collection.to_string(), column.to_string()))
            .map(|idx| SpatialIndexStats {
                column: column.to_string(),
                collection: collection.to_string(),
                point_count: idx.len(),
                memory_bytes: idx.memory_bytes(),
            })
    }
}

impl Default for SpatialIndexManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct SpatialIndexStats {
    pub column: String,
    pub collection: String,
    pub point_count: usize,
    pub memory_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_haversine() {
        // Paris to London ≈ 344 km
        let d = haversine_km(48.8566, 2.3522, 51.5074, -0.1278);
        assert!((d - 344.0).abs() < 5.0, "Paris-London: {d} km");
    }

    #[test]
    fn test_spatial_insert_and_radius() {
        let mut idx = SpatialIndex::new("location");

        // Paris
        idx.insert(EntityId::new(1), 48.8566, 2.3522);
        // London
        idx.insert(EntityId::new(2), 51.5074, -0.1278);
        // Berlin
        idx.insert(EntityId::new(3), 52.5200, 13.4050);
        // Tokyo (far away)
        idx.insert(EntityId::new(4), 35.6762, 139.6503);

        // Search 500km from Paris — should find Paris + London, not Berlin or Tokyo
        let results = idx.search_radius(48.8566, 2.3522, 500.0, 10);
        let ids: Vec<u64> = results.iter().map(|r| r.entity_id.raw()).collect();
        assert!(ids.contains(&1), "Should find Paris");
        assert!(ids.contains(&2), "Should find London");
        assert!(!ids.contains(&4), "Should NOT find Tokyo");
    }

    #[test]
    fn test_spatial_bbox() {
        let mut idx = SpatialIndex::new("location");
        idx.insert(EntityId::new(1), 48.8566, 2.3522); // Paris
        idx.insert(EntityId::new(2), 51.5074, -0.1278); // London
        idx.insert(EntityId::new(3), 35.6762, 139.6503); // Tokyo

        // Bounding box covering Europe
        let results = idx.search_bbox(40.0, -10.0, 55.0, 20.0, 10);
        let ids: Vec<u64> = results.iter().map(|r| r.entity_id.raw()).collect();
        assert!(ids.contains(&1)); // Paris
        assert!(ids.contains(&2)); // London
        assert!(!ids.contains(&3)); // Tokyo outside
    }

    #[test]
    fn test_spatial_nearest() {
        let mut idx = SpatialIndex::new("location");
        idx.insert(EntityId::new(1), 48.8566, 2.3522); // Paris
        idx.insert(EntityId::new(2), 51.5074, -0.1278); // London
        idx.insert(EntityId::new(3), 52.5200, 13.4050); // Berlin

        // Nearest to Brussels (50.85, 4.35)
        let results = idx.search_nearest(50.8503, 4.3517, 2);
        assert_eq!(results.len(), 2);
        // Paris and London should be closest to Brussels
        assert!(results[0].distance_km < results[1].distance_km);
    }

    #[test]
    fn test_spatial_remove() {
        let mut idx = SpatialIndex::new("location");
        idx.insert(EntityId::new(1), 48.8566, 2.3522);
        idx.insert(EntityId::new(2), 51.5074, -0.1278);
        assert_eq!(idx.len(), 2);

        idx.remove(EntityId::new(1));
        assert_eq!(idx.len(), 1);

        let results = idx.search_nearest(48.8566, 2.3522, 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity_id, EntityId::new(2));
    }

    #[test]
    fn test_spatial_bulk_load() {
        let data = vec![
            (EntityId::new(1), 48.8566, 2.3522),
            (EntityId::new(2), 51.5074, -0.1278),
            (EntityId::new(3), 52.5200, 13.4050),
        ];
        let idx = SpatialIndex::bulk_load("location", data);
        assert_eq!(idx.len(), 3);
    }

    #[test]
    fn test_spatial_manager() {
        let mgr = SpatialIndexManager::new();
        mgr.create_index("sites", "location");

        mgr.insert("sites", "location", EntityId::new(1), 48.8566, 2.3522);
        mgr.insert("sites", "location", EntityId::new(2), 51.5074, -0.1278);

        let results = mgr.search_radius("sites", "location", 48.8566, 2.3522, 500.0, 10);
        assert!(!results.is_empty());

        let stats = mgr.index_stats("sites", "location").unwrap();
        assert_eq!(stats.point_count, 2);
    }
}
