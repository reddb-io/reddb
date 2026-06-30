//! R-Tree Spatial Index (opt-in, memory-capped)
//!
//! Provides spatial queries on GeoPoint, Latitude, and Longitude data using
//! the `rstar` crate for an in-RAM R-tree.
//!
//! # Status (PRD #1574 / #1578)
//! This in-RAM R-tree is **no longer the default spatial index**. The default
//! spatial backend is the disk-resident H3 index (cell-id `u64` over the paged
//! B-tree), which keeps RAM at O(working set) rather than O(total points). This
//! R-tree is reachable only via the explicit `CREATE INDEX … USING RTREE`
//! opt-in and is **memory-capped** so it can never silently OOM the process —
//! inserting past the byte budget is refused with [`SpatialIndexError::CapacityExceeded`].
//!
//! Trade-off: prefer the R-tree for **arbitrary shapes / exact small sets**
//! held in RAM; prefer H3 (the default) for **points at scale, on disk**.
//!
//! The cap is configured by `RED_SPATIAL_RTREE_MAX_BYTES` (an approximate byte
//! budget on the resident structure); see [`DEFAULT_RTREE_MAX_BYTES`].
//!
//! # Migration (existing R-tree spatial indexes → H3)
//! To move a spatial column off the in-RAM R-tree onto the disk-resident
//! default, recreate the index with the generic spatial method (or `USING H3`):
//! ```sql
//! DROP INDEX <name> ON <table>;
//! CREATE INDEX <name> ON <table> (<col>) USING SPATIAL;  -- resolves to H3
//! ```
//! No data migration is required — the geo column is unchanged; only the index
//! mechanism (and its RAM profile) changes. `SEARCH SPATIAL` results are
//! identical on both paths (haversine-exact). Keep `USING RTREE` only for
//! small, in-RAM, shape-oriented workloads that fit under the cap.
//!
//! # Supported queries
//! - **Radius search**: Find all points within X km of a center point
//! - **Bounding box search**: Find all points within a lat/lon rectangle
//! - **Nearest-K search**: Find the K closest points to a location

use std::collections::HashMap;

use parking_lot::RwLock;

use rstar::{primitives::GeomWithData, RTree, AABB};

use super::entity::EntityId;

/// Default approximate memory budget for a single in-RAM R-tree spatial
/// index, used when `RED_SPATIAL_RTREE_MAX_BYTES` is unset. 256 MiB at the
/// recalc footprint (~100–150 B/point) bounds a single index to ~1.7–2.5M
/// points — generous for the "small/shape sets" the R-tree is now reserved
/// for, while making the unbounded-OOM-at-scale failure mode impossible.
/// Points at scale belong on the disk-resident H3 default (PRD #1574).
pub const DEFAULT_RTREE_MAX_BYTES: usize = 256 * 1024 * 1024;

/// Environment override for [`DEFAULT_RTREE_MAX_BYTES`]. Mirrors the
/// `RED_*_MAX_BYTES` convention used elsewhere (e.g. `RED_AUDIT_MAX_BYTES`).
const RTREE_MAX_BYTES_ENV: &str = "RED_SPATIAL_RTREE_MAX_BYTES";

/// Approximate resident cost of one indexed point, kept in lock-step with
/// [`SpatialIndex::memory_bytes`]: one R-tree leaf entry plus the parallel
/// `points` HashMap slot. Used to project the post-insert footprint when
/// enforcing the cap.
const PER_POINT_BYTES: usize = std::mem::size_of::<SpatialEntry>() + 32;

/// Resolve the per-index R-tree byte budget from the environment, falling
/// back to [`DEFAULT_RTREE_MAX_BYTES`].
fn resolve_rtree_max_bytes() -> usize {
    std::env::var(RTREE_MAX_BYTES_ENV)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_RTREE_MAX_BYTES)
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpatialIndexError {
    MissingIndex {
        collection: String,
        column: String,
    },
    /// Inserting a *new* point would push the in-RAM R-tree past its
    /// configured byte budget (`RED_SPATIAL_RTREE_MAX_BYTES`). The point is
    /// NOT inserted — the structure refuses to grow rather than risk an OOM.
    /// Use the disk-resident H3 index (the default) for points at scale.
    CapacityExceeded {
        collection: String,
        column: String,
        max_bytes: usize,
        attempted_bytes: usize,
    },
}

impl std::fmt::Display for SpatialIndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingIndex { collection, column } => {
                write!(
                    f,
                    "spatial index for column '{column}' was not found in collection '{collection}'"
                )
            }
            Self::CapacityExceeded {
                collection,
                column,
                max_bytes,
                attempted_bytes,
            } => {
                write!(
                    f,
                    "in-RAM R-tree spatial index for column '{column}' in collection \
                     '{collection}' is memory-capped at {max_bytes} bytes \
                     (RED_SPATIAL_RTREE_MAX_BYTES); insert would need ~{attempted_bytes} bytes. \
                     Use the disk-resident H3 spatial index (USING H3 / the default) for points at scale."
                )
            }
        }
    }
}

impl std::error::Error for SpatialIndexError {}

/// Capacity-overflow signal raised by [`SpatialIndex::insert`], which does not
/// know its own `(collection, column)` location. The owning
/// [`SpatialIndexManager`] enriches it into [`SpatialIndexError::CapacityExceeded`].
#[derive(Debug, Clone, PartialEq)]
pub struct SpatialCapacityError {
    pub max_bytes: usize,
    pub attempted_bytes: usize,
}

/// A 2D point in the R-tree, storing (lon, lat) in degrees with an associated EntityId.
/// Note: rstar uses [x, y] convention, so we store (longitude, latitude).
type SpatialEntry = GeomWithData<[f64; 2], EntityId>;

/// Build a spatial entry from lat/lon (degrees) and entity ID
fn make_entry(lat: f64, lon: f64, entity_id: EntityId) -> SpatialEntry {
    GeomWithData::new([lon, lat], entity_id)
}

pub use crate::geo::haversine_km;

fn km_to_approx_degrees(km: f64) -> f64 {
    km / 111.32
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
    /// Approximate resident-byte budget; a *new*-point insert that would push
    /// [`SpatialIndex::memory_bytes`] past this is refused (never silently
    /// OOMs). Resolved from `RED_SPATIAL_RTREE_MAX_BYTES`.
    max_bytes: usize,
}

impl SpatialIndex {
    /// Create a new spatial index with the environment-configured memory cap.
    pub fn new(column: impl Into<String>) -> Self {
        Self::with_max_bytes(column, resolve_rtree_max_bytes())
    }

    /// Create a new spatial index with an explicit memory cap. Bypasses
    /// `RED_SPATIAL_RTREE_MAX_BYTES` resolution so parallel tests don't race
    /// on `set_var` (mirrors `AuditLogger::with_max_bytes`).
    pub fn with_max_bytes(column: impl Into<String>, max_bytes: usize) -> Self {
        Self {
            tree: RTree::new(),
            points: HashMap::new(),
            column: column.into(),
            max_bytes,
        }
    }

    /// The configured approximate memory budget (bytes).
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
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
            max_bytes: resolve_rtree_max_bytes(),
        }
    }

    /// Insert a point.
    ///
    /// Updating an existing entity (same `entity_id`) is always allowed — it
    /// does not grow the structure. Inserting a *new* point that would push
    /// the index past its configured byte budget is refused with
    /// [`SpatialCapacityError`]; the point is not inserted.
    pub fn insert(
        &mut self,
        entity_id: EntityId,
        lat: f64,
        lon: f64,
    ) -> Result<(), SpatialCapacityError> {
        // Remove old entry if exists (an update never grows the footprint).
        if let Some((old_lat, old_lon)) = self.points.remove(&entity_id) {
            self.tree.remove(&make_entry(old_lat, old_lon, entity_id));
        } else {
            // New point: refuse if it would breach the cap.
            let projected = self.memory_bytes() + PER_POINT_BYTES;
            if projected > self.max_bytes {
                // Re-insert nothing; `points` already had no entry for this id.
                return Err(SpatialCapacityError {
                    max_bytes: self.max_bytes,
                    attempted_bytes: projected,
                });
            }
        }
        self.tree.insert(make_entry(lat, lon, entity_id));
        self.points.insert(entity_id, (lat, lon));
        Ok(())
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
            .locate_in_envelope(aabb)
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

        results.sort_by(|a, b| {
            a.distance_km
                .partial_cmp(&b.distance_km)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
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
            .locate_in_envelope(aabb)
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
            .nearest_neighbor_iter([lon, lat])
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
        let mut indices = self.indices.write();
        let key = (collection.to_string(), column.to_string());
        indices
            .entry(key)
            .or_insert_with(|| SpatialIndex::new(column));
    }

    /// Drop a spatial index
    pub fn drop_index(&self, collection: &str, column: &str) -> bool {
        let mut indices = self.indices.write();
        indices
            .remove(&(collection.to_string(), column.to_string()))
            .is_some()
    }

    /// Insert a point
    pub fn insert(
        &self,
        collection: &str,
        column: &str,
        entity_id: EntityId,
        lat: f64,
        lon: f64,
    ) -> Result<(), SpatialIndexError> {
        let mut indices = self.indices.write();
        if let Some(index) = indices.get_mut(&(collection.to_string(), column.to_string())) {
            index
                .insert(entity_id, lat, lon)
                .map_err(|e| SpatialIndexError::CapacityExceeded {
                    collection: collection.to_string(),
                    column: column.to_string(),
                    max_bytes: e.max_bytes,
                    attempted_bytes: e.attempted_bytes,
                })
        } else {
            Err(SpatialIndexError::MissingIndex {
                collection: collection.to_string(),
                column: column.to_string(),
            })
        }
    }

    /// Remove a point
    pub fn remove(
        &self,
        collection: &str,
        column: &str,
        entity_id: EntityId,
    ) -> Result<bool, SpatialIndexError> {
        let mut indices = self.indices.write();
        if let Some(index) = indices.get_mut(&(collection.to_string(), column.to_string())) {
            Ok(index.remove(entity_id))
        } else {
            Err(SpatialIndexError::MissingIndex {
                collection: collection.to_string(),
                column: column.to_string(),
            })
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
    ) -> Result<Vec<SpatialSearchResult>, SpatialIndexError> {
        let indices = self.indices.read();
        if let Some(idx) = indices.get(&(collection.to_string(), column.to_string())) {
            Ok(idx.search_radius(center_lat, center_lon, radius_km, limit))
        } else {
            Err(SpatialIndexError::MissingIndex {
                collection: collection.to_string(),
                column: column.to_string(),
            })
        }
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
    ) -> Result<Vec<SpatialSearchResult>, SpatialIndexError> {
        let indices = self.indices.read();
        if let Some(idx) = indices.get(&(collection.to_string(), column.to_string())) {
            Ok(idx.search_bbox(min_lat, min_lon, max_lat, max_lon, limit))
        } else {
            Err(SpatialIndexError::MissingIndex {
                collection: collection.to_string(),
                column: column.to_string(),
            })
        }
    }

    /// Find K nearest points
    pub fn search_nearest(
        &self,
        collection: &str,
        column: &str,
        lat: f64,
        lon: f64,
        k: usize,
    ) -> Result<Vec<SpatialSearchResult>, SpatialIndexError> {
        let indices = self.indices.read();
        if let Some(idx) = indices.get(&(collection.to_string(), column.to_string())) {
            Ok(idx.search_nearest(lat, lon, k))
        } else {
            Err(SpatialIndexError::MissingIndex {
                collection: collection.to_string(),
                column: column.to_string(),
            })
        }
    }

    /// Get stats
    pub fn index_stats(
        &self,
        collection: &str,
        column: &str,
    ) -> Result<SpatialIndexStats, SpatialIndexError> {
        let indices = self.indices.read();
        if let Some(idx) = indices.get(&(collection.to_string(), column.to_string())) {
            Ok(SpatialIndexStats {
                column: column.to_string(),
                collection: collection.to_string(),
                point_count: idx.len(),
                memory_bytes: idx.memory_bytes(),
            })
        } else {
            Err(SpatialIndexError::MissingIndex {
                collection: collection.to_string(),
                column: column.to_string(),
            })
        }
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
        idx.insert(EntityId::new(1), 48.8566, 2.3522).unwrap();
        // London
        idx.insert(EntityId::new(2), 51.5074, -0.1278).unwrap();
        // Berlin
        idx.insert(EntityId::new(3), 52.5200, 13.4050).unwrap();
        // Tokyo (far away)
        idx.insert(EntityId::new(4), 35.6762, 139.6503).unwrap();

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
        idx.insert(EntityId::new(1), 48.8566, 2.3522).unwrap(); // Paris
        idx.insert(EntityId::new(2), 51.5074, -0.1278).unwrap(); // London
        idx.insert(EntityId::new(3), 35.6762, 139.6503).unwrap(); // Tokyo

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
        idx.insert(EntityId::new(1), 48.8566, 2.3522).unwrap(); // Paris
        idx.insert(EntityId::new(2), 51.5074, -0.1278).unwrap(); // London
        idx.insert(EntityId::new(3), 52.5200, 13.4050).unwrap(); // Berlin

        // Nearest to Brussels (50.85, 4.35)
        let results = idx.search_nearest(50.8503, 4.3517, 2);
        assert_eq!(results.len(), 2);
        // Paris and London should be closest to Brussels
        assert!(results[0].distance_km < results[1].distance_km);
    }

    #[test]
    fn test_spatial_remove() {
        let mut idx = SpatialIndex::new("location");
        idx.insert(EntityId::new(1), 48.8566, 2.3522).unwrap();
        idx.insert(EntityId::new(2), 51.5074, -0.1278).unwrap();
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

        mgr.insert("sites", "location", EntityId::new(1), 48.8566, 2.3522)
            .expect("spatial insert should succeed");
        mgr.insert("sites", "location", EntityId::new(2), 51.5074, -0.1278)
            .expect("spatial insert should succeed");

        let results = mgr
            .search_radius("sites", "location", 48.8566, 2.3522, 500.0, 10)
            .unwrap();
        assert!(!results.is_empty());

        let stats = mgr.index_stats("sites", "location").unwrap();
        assert_eq!(stats.point_count, 2);
    }

    #[test]
    fn test_spatial_manager_recovers_from_poisoned_lock() {
        let mgr = SpatialIndexManager::new();
        mgr.create_index("sites", "location");

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = mgr.indices.write();
            panic!("poison spatial index manager");
        }));

        mgr.insert("sites", "location", EntityId::new(1), 48.8566, 2.3522)
            .expect("spatial insert should recover after poison");

        let results = mgr
            .search_nearest("sites", "location", 48.8566, 2.3522, 1)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity_id, EntityId::new(1));
    }

    #[test]
    fn test_spatial_manager_lookup_missing_index_returns_error() {
        let mgr = SpatialIndexManager::new();

        let err = mgr
            .search_nearest("sites", "location", 48.8566, 2.3522, 1)
            .expect_err("spatial lookup should fail when the index does not exist");

        assert_eq!(
            err,
            SpatialIndexError::MissingIndex {
                collection: "sites".to_string(),
                column: "location".to_string(),
            }
        );
    }

    /// Cap with headroom for exactly one point. The in-RAM R-tree must
    /// refuse a *second, new* point rather than grow unbounded (PRD #1574 /
    /// #1578) — it never silently OOMs.
    #[test]
    fn test_spatial_insert_refuses_new_point_past_memory_cap() {
        let cap = std::mem::size_of::<SpatialIndex>() + PER_POINT_BYTES;
        let mut idx = SpatialIndex::with_max_bytes("location", cap);

        // First new point fits under the cap.
        idx.insert(EntityId::new(1), 48.8566, 2.3522)
            .expect("first point should fit under the cap");
        assert_eq!(idx.len(), 1);

        // Second *new* point would breach the cap → refused, not inserted.
        let err = idx
            .insert(EntityId::new(2), 51.5074, -0.1278)
            .expect_err("second new point must be refused past the cap");
        assert_eq!(err.max_bytes, cap);
        assert!(err.attempted_bytes > cap, "{err:?}");
        assert_eq!(idx.len(), 1, "refused point must not be inserted");

        // Updating an *existing* entity is always allowed — it does not grow
        // the structure, so the cap does not apply.
        idx.insert(EntityId::new(1), 40.0, -3.0)
            .expect("update of existing point must be allowed at the cap");
        assert_eq!(idx.len(), 1);
    }

    /// The manager enriches the capacity overflow into a
    /// `SpatialIndexError::CapacityExceeded` carrying `(collection, column)`.
    #[test]
    fn test_spatial_manager_surfaces_capacity_error() {
        let mgr = SpatialIndexManager::new();
        // Inject a tiny-cap index directly (the public `create_index` resolves
        // the env-configured cap; the test wants a deterministic small one).
        let cap = std::mem::size_of::<SpatialIndex>() + PER_POINT_BYTES;
        mgr.indices.write().insert(
            ("sites".to_string(), "location".to_string()),
            SpatialIndex::with_max_bytes("location", cap),
        );

        mgr.insert("sites", "location", EntityId::new(1), 48.8566, 2.3522)
            .expect("first point should fit");

        let err = mgr
            .insert("sites", "location", EntityId::new(2), 51.5074, -0.1278)
            .expect_err("manager must surface the capacity overflow");
        match err {
            SpatialIndexError::CapacityExceeded {
                collection,
                column,
                max_bytes,
                ..
            } => {
                assert_eq!(collection, "sites");
                assert_eq!(column, "location");
                assert_eq!(max_bytes, cap);
            }
            other => panic!("expected CapacityExceeded, got {other:?}"),
        }
    }

    /// A freshly constructed index always carries a finite, positive budget —
    /// there is no unbounded-RAM default. The const default is itself bounded.
    #[test]
    fn test_spatial_default_cap_is_bounded() {
        let idx = SpatialIndex::new("location");
        assert!(idx.max_bytes() > 0);
        assert!(DEFAULT_RTREE_MAX_BYTES > 0);
        assert!(DEFAULT_RTREE_MAX_BYTES < usize::MAX);
    }
}
