//! H3 hexagonal hierarchical geospatial index wrapper.
//!
//! A thin boundary over the pure-Rust [`h3o`] crate that maps its
//! typed `CellIndex` / `LatLng` / `Resolution` to and from the raw
//! `u64` cell ids and `f64` degrees the rest of the engine speaks.
//! Keeping every `h3o` reference inside this module means the storage
//! and query layers stay h3o-agnostic (PRD #1574 slice 1, #1575).
//!
//! This slice is pure and storage-free: encode/decode, kRing
//! (`grid_disk`) and parent truncation only. No on-disk index is
//! touched here — that arrives in later slices.

use geo::{LineString, Polygon};
use h3o::{
    geom::{ContainmentMode, TilerBuilder},
    CellIndex, LatLng, Resolution,
};

pub const MIN_RESOLUTION: i64 = 0;
pub const MAX_RESOLUTION: i64 = 15;

/// Cap on the polyfill cover of a query polygon. Past it the covering
/// set costs more to enumerate than a full scan, so every caller —
/// the `SEARCH SPATIAL WITHIN POLYGON` verb and the `GEO_WITHIN`
/// predicate alike — falls back to the exact scan. Shared so the two
/// surfaces prune identically and stay result-identical.
pub const MAX_POLYGON_COVER_CELLS: usize = 50_000;

/// Cap on the kRing cover of a radius query. Past it the ring costs more to
/// enumerate than a full scan, so every caller falls back to the exact scan.
pub const MAX_COVER_RING: u32 = 128;

/// The kRing radius that covers `radius_km` around a point at `res`.
///
/// `None` when the cover would exceed [`MAX_COVER_RING`] — the caller then
/// takes the exact full scan. Shared by the executor and by `EXPLAIN` so the
/// plan that `EXPLAIN` reports is the plan the executor runs; the two drifting
/// apart would make `EXPLAIN` announce an index route that never happens.
pub fn radius_cover_ring(radius_km: f64, res: u8) -> Option<u32> {
    let edge_km = edge_length_km(res).max(f64::MIN_POSITIVE);
    let k = (radius_km / edge_km).ceil() + 1.0;
    (k.is_finite() && k <= f64::from(MAX_COVER_RING)).then_some(k as u32)
}

pub fn valid_resolution(res: i64) -> Option<u8> {
    if (MIN_RESOLUTION..=MAX_RESOLUTION).contains(&res) {
        Some(res as u8)
    } else {
        None
    }
}

/// Clamp an arbitrary resolution byte into h3o's valid `0..=15` range.
///
/// H3 has 16 resolutions; anything coarser/finer than the bounds is
/// clamped rather than rejected so callers get a usable cell.
fn clamp_resolution(res: u8) -> Resolution {
    Resolution::try_from(res.min(15)).unwrap_or(Resolution::Zero)
}

/// Encode a `(lat, lon)` degree pair into an H3 cell id at `res`.
///
/// Latitude/longitude are degrees. Returns `0` when the coordinate is
/// not a valid WGS-84 point (h3o rejects non-finite / out-of-range
/// inputs); `0` is never a valid H3 cell id so it doubles as a sentinel.
pub fn lat_lng_to_cell(lat: f64, lon: f64, res: u8) -> u64 {
    match LatLng::new(lat, lon) {
        Ok(ll) => u64::from(ll.to_cell(clamp_resolution(res))),
        Err(_) => 0,
    }
}

/// Decode an H3 cell id to its center `(lat, lon)` in degrees.
///
/// Returns `(0.0, 0.0)` for an invalid cell id.
pub fn cell_to_lat_lng(cell: u64) -> (f64, f64) {
    match CellIndex::try_from(cell) {
        Ok(c) => {
            let ll = LatLng::from(c);
            (ll.lat(), ll.lng())
        }
        Err(_) => (0.0, 0.0),
    }
}

/// kRing: `cell` plus every cell within `k` grid steps of it. For a
/// hexagon and `k == 1` this is 7 cells (self + 6 neighbors).
///
/// Returns an empty vec for an invalid cell id.
pub fn grid_disk(cell: u64, k: u32) -> Vec<u64> {
    match CellIndex::try_from(cell) {
        Ok(c) => c
            .grid_disk::<Vec<CellIndex>>(k)
            .into_iter()
            .map(u64::from)
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Average hexagon edge length, in kilometres, for an H3 resolution.
///
/// Sourced from h3o's per-resolution *average*-edge table, and used by
/// [`radius_cover_ring`] to size the covering kRing for a radius search
/// (`k ≈ radius_km / edge_km`).
///
/// Why an average is safe here: one grid step moves between hexagon centres
/// by the short diagonal, `√3 · edge ≈ 1.73 · edge`, so a cell at grid
/// distance `k` sits at least `k · √3 · edge` away and its nearest point at
/// least `edge · (√3·k − 1)`. Dividing the radius by a plain `edge` therefore
/// over-estimates `k` by roughly 1.73×, and that margin is what absorbs the
/// gap between this average and the smallest real cell at the resolution —
/// H3 cell size varies within a resolution, so the average alone would not
/// justify the bound. The margin is what keeps the cover a safe superset
/// (PRD #1574 slice 3).
pub fn edge_length_km(res: u8) -> f64 {
    clamp_resolution(res).edge_length_km()
}

/// Truncate `cell` to its ancestor cell at the coarser resolution
/// `res` (hierarchical parent).
///
/// Returns `0` for an invalid cell id, or when `res` is finer than the
/// cell's own resolution (no such parent exists).
pub fn cell_to_parent(cell: u64, res: u8) -> u64 {
    match CellIndex::try_from(cell) {
        Ok(c) => c.parent(clamp_resolution(res)).map(u64::from).unwrap_or(0),
        Err(_) => 0,
    }
}

/// Cover a polygon with every H3 cell whose boundary intersects or covers the
/// polygon at `res`.
///
/// Input vertices are `(lat, lon)` degrees. Returns `None` when the geometry
/// cannot be tiled or when the caller-supplied cap would be exceeded, so query
/// execution can fall back to an exact full scan.
pub fn polygon_to_cover_cells(
    vertices: &[(f64, f64)],
    res: u8,
    max_cells: usize,
) -> Option<Vec<u64>> {
    if vertices.len() < 3 {
        return None;
    }
    let mut coords: Vec<(f64, f64)> = vertices.iter().map(|(lat, lon)| (*lon, *lat)).collect();
    if coords.first() != coords.last() {
        coords.push(*coords.first()?);
    }
    let polygon = Polygon::new(LineString::from(coords), vec![]);
    let mut tiler = TilerBuilder::new(clamp_resolution(res))
        .containment_mode(ContainmentMode::Covers)
        .disable_transmeridian_heuristic()
        .build();
    tiler.add(polygon).ok()?;
    if tiler.coverage_size_hint() > max_cells {
        return None;
    }
    let mut cells = Vec::new();
    for cell in tiler.into_coverage() {
        if cells.len() >= max_cells {
            return None;
        }
        cells.push(u64::from(cell));
    }
    cells.sort_unstable();
    cells.dedup();
    Some(cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Paris (Notre-Dame) at resolution 9.
    const PARIS_LAT: f64 = 48.8566;
    const PARIS_LON: f64 = 2.3522;

    #[test]
    fn round_trip_within_cell_tolerance() {
        // res 9 cells are ~174 m edge; the center must be within a
        // few hundred metres of the source coordinate.
        for (lat, lon, res) in [
            (PARIS_LAT, PARIS_LON, 9u8),
            (-23.550_520, -46.633_309, 10), // São Paulo
            (0.0, 0.0, 7),                  // null island
            (51.5074, -0.1278, 12),         // London, fine res
        ] {
            let cell = lat_lng_to_cell(lat, lon, res);
            assert_ne!(cell, 0, "cell should encode for ({lat},{lon})@{res}");
            let (rlat, rlon) = cell_to_lat_lng(cell);
            assert!(
                (rlat - lat).abs() < 0.05 && (rlon - lon).abs() < 0.05,
                "round-trip drift too large: ({lat},{lon}) -> ({rlat},{rlon})"
            );
        }
    }

    #[test]
    fn grid_disk_k1_returns_self_plus_six_neighbors() {
        let cell = lat_lng_to_cell(PARIS_LAT, PARIS_LON, 9);
        let ring = grid_disk(cell, 1);
        assert_eq!(ring.len(), 7, "k=1 disk over a hexagon is self + 6");
        assert!(
            ring.contains(&cell),
            "the disk must include the center cell"
        );
    }

    #[test]
    fn grid_disk_k0_is_just_self() {
        let cell = lat_lng_to_cell(PARIS_LAT, PARIS_LON, 9);
        let ring = grid_disk(cell, 0);
        assert_eq!(ring, vec![cell]);
    }

    #[test]
    fn cell_to_parent_truncates_resolution() {
        let fine = lat_lng_to_cell(PARIS_LAT, PARIS_LON, 9);
        let coarse = cell_to_parent(fine, 5);
        assert_ne!(coarse, 0);
        // The parent at res 5 is the same as encoding the point at res 5.
        assert_eq!(coarse, lat_lng_to_cell(PARIS_LAT, PARIS_LON, 5));
        // A "parent" finer than the cell's own resolution has no answer.
        assert_eq!(cell_to_parent(fine, 12), 0);
    }

    #[test]
    fn edge_length_km_is_positive_and_shrinks_with_resolution() {
        // Coarser resolutions have longer edges than finer ones, and the
        // length is always a usable positive number for the cover sizing.
        let coarse = edge_length_km(5);
        let fine = edge_length_km(12);
        assert!(coarse > 0.0 && fine > 0.0);
        assert!(coarse > fine, "edge length must shrink as resolution grows");
        // Out-of-range bytes clamp to res 15 rather than panicking.
        assert!(edge_length_km(200) > 0.0);
    }

    #[test]
    fn radius_cover_ring_grows_with_radius_and_caps() {
        // A radius under one edge still needs a ring, and the ring grows with
        // the radius at a fixed resolution.
        let small = radius_cover_ring(0.1, 9).expect("small radius must cover");
        let large = radius_cover_ring(5.0, 9).expect("larger radius must cover");
        assert!(small >= 1, "the cover always includes the neighbour ring");
        assert!(large > small, "a wider radius needs a wider ring");
        // Past the cap the caller must fall back to the exact full scan.
        assert_eq!(
            radius_cover_ring(20_000.0, 15),
            None,
            "a cover beyond MAX_COVER_RING must decline, not truncate"
        );
        assert_eq!(radius_cover_ring(f64::NAN, 9), None);
    }

    #[test]
    fn radius_cover_ring_stays_a_superset_of_the_radius() {
        // The ring must reach at least as far as the radius it covers: `k`
        // grid steps span at least `k * edge` km (the √3 short-diagonal
        // margin is on top of that), so `k * edge >= radius` must hold.
        for res in [5u8, 7, 9, 12] {
            let edge = edge_length_km(res);
            for radius_km in [0.05, 0.5, 2.0, 10.0] {
                let Some(k) = radius_cover_ring(radius_km, res) else {
                    continue;
                };
                assert!(
                    f64::from(k) * edge >= radius_km,
                    "ring {k} at res {res} must reach {radius_km} km (edge {edge})"
                );
            }
        }
    }

    #[test]
    fn invalid_inputs_are_sentinelled() {
        // h3o rejects only non-finite coordinates (finite out-of-range
        // values are normalized), so NaN/inf sentinel to 0.
        assert_eq!(lat_lng_to_cell(f64::NAN, 0.0, 9), 0);
        assert_eq!(lat_lng_to_cell(0.0, f64::INFINITY, 9), 0);
        assert_eq!(cell_to_lat_lng(0), (0.0, 0.0));
        assert!(grid_disk(0, 1).is_empty());
        assert_eq!(cell_to_parent(0, 5), 0);
    }
}
