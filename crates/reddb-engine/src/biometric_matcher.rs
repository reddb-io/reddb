//! Deterministic fingerprint-minutiae comparison without feature extraction.

use reddb_types::{BiometricSample, Minutia, MinutiaeView};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const FINGERPRINT_MATCHER_VERSION: u32 = 3;
const MAX_ALIGNMENT_ANCHORS: usize = 32;
const MAX_DISTANCE_MICROMETRES: f64 = 700.0;
const MAX_ANGLE_DEGREES: f64 = 20.0;
const GRID_CELL_MICROMETRES: u32 = 700;
const KIND_CANDIDATE_BAND_PREFIX: u64 = 0xffff_ffff_ffff_ff00;

pub fn fingerprint_compare(
    left: &BiometricSample,
    right: &BiometricSample,
) -> Result<f64, FingerprintMatchError> {
    let left_views = left
        .minutiae_views()
        .map_err(FingerprintMatchError::Record)?;
    let right_views = right
        .minutiae_views()
        .map_err(FingerprintMatchError::Record)?;
    if left == right {
        return Ok(1.0);
    }
    let mut best = 0.0_f64;
    let mut compatible = false;
    for left_view in &left_views {
        for right_view in &right_views {
            if !positions_compatible(left_view, right_view) {
                continue;
            }
            compatible = true;
            let forward = compare_view_direction(left_view, right_view);
            let reverse = compare_view_direction(right_view, left_view);
            best = best.max((forward + reverse) * 0.5);
        }
    }
    if !compatible {
        return Err(FingerprintMatchError::IncompatibleFingerPositions);
    }
    Ok(best.clamp(0.0, 1.0))
}

pub fn fingerprint_match(
    left: &BiometricSample,
    right: &BiometricSample,
    minimum_score: f64,
) -> Result<bool, FingerprintMatchError> {
    validate_threshold(minimum_score)?;
    Ok(fingerprint_compare(left, right)? >= minimum_score)
}

/// Stable, rotation/translation-invariant local descriptors used only for
/// candidate generation. The exact matcher remains authoritative.
pub fn fingerprint_descriptor_bands(
    sample: &BiometricSample,
) -> Result<Vec<u64>, FingerprintMatchError> {
    let views = sample
        .minutiae_views()
        .map_err(FingerprintMatchError::Record)?;
    let mut descriptors = BTreeSet::new();
    for view in views {
        for (anchor_index, anchor) in view.minutiae.iter().enumerate() {
            if anchor.kind == 0 {
                for compatible_kind in 0..=3 {
                    descriptors.insert(kind_candidate_band(compatible_kind));
                }
            } else {
                descriptors.insert(kind_candidate_band(anchor.kind));
            }
            let mut neighbours = view
                .minutiae
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != anchor_index)
                .map(|(_, neighbour)| {
                    let dx = i64::from(neighbour.x_micrometres) - i64::from(anchor.x_micrometres);
                    let dy = i64::from(neighbour.y_micrometres) - i64::from(anchor.y_micrometres);
                    let distance_squared = dx * dx + dy * dy;
                    (distance_squared, dx, dy, neighbour)
                })
                .collect::<Vec<_>>();
            neighbours
                .sort_by_key(|(distance, dx, dy, neighbour)| (*distance, *dx, *dy, neighbour.kind));
            if neighbours.is_empty() {
                continue;
            }

            let mut hash = 0xcbf2_9ce4_8422_2325_u64;
            descriptor_hash_byte(&mut hash, anchor.kind);
            descriptor_hash_byte(&mut hash, neighbours.len().min(3) as u8);
            for (distance_squared, dx, dy, neighbour) in neighbours.iter().take(3) {
                let distance = (*distance_squared as f64).sqrt();
                let radial_angle = (*dy as f64).atan2(*dx as f64).to_degrees();
                let relative_radial = circular_degrees(radial_angle - angle_degrees(anchor));
                let relative_orientation =
                    circular_degrees(angle_degrees(neighbour) - angle_degrees(anchor));
                descriptor_hash_u16(&mut hash, (distance / 500.0).round() as u16);
                descriptor_hash_u16(&mut hash, (relative_radial / 15.0).round() as u16);
                descriptor_hash_u16(&mut hash, (relative_orientation / 15.0).round() as u16);
                descriptor_hash_byte(&mut hash, neighbour.kind);
            }
            descriptors.insert(hash);
        }
    }
    Ok(descriptors.into_iter().collect())
}

fn kind_candidate_band(kind: u8) -> u64 {
    KIND_CANDIDATE_BAND_PREFIX | u64::from(kind)
}

fn circular_degrees(angle: f64) -> f64 {
    angle.rem_euclid(360.0)
}

fn descriptor_hash_byte(hash: &mut u64, value: u8) {
    *hash ^= u64::from(value);
    *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
}

fn descriptor_hash_u16(hash: &mut u64, value: u16) {
    for byte in value.to_le_bytes() {
        descriptor_hash_byte(hash, byte);
    }
}

fn positions_compatible(left: &MinutiaeView, right: &MinutiaeView) -> bool {
    left.position == 0 || right.position == 0 || left.position == right.position
}

fn compare_view_direction(left: &MinutiaeView, right: &MinutiaeView) -> f64 {
    if left.minutiae.is_empty() || right.minutiae.is_empty() {
        return 0.0;
    }
    let left_grid = minutiae_grid(&left.minutiae);
    let left_anchors = alignment_anchor_indices(&left.minutiae);
    let right_anchors = alignment_anchor_indices(&right.minutiae);
    let mut best_matches = 0_usize;
    for left_anchor_index in left_anchors {
        let left_anchor = &left.minutiae[left_anchor_index];
        for &right_anchor_index in &right_anchors {
            let right_anchor = &right.minutiae[right_anchor_index];
            if !kinds_compatible(left_anchor.kind, right_anchor.kind) {
                continue;
            }
            let matches = aligned_matches(
                &left.minutiae,
                &right.minutiae,
                &left_grid,
                left_anchor_index,
                right_anchor_index,
            );
            best_matches = best_matches.max(matches);
        }
    }
    best_matches as f64 / left.minutiae.len().max(right.minutiae.len()) as f64
}

fn alignment_anchor_indices(minutiae: &[Minutia]) -> Vec<usize> {
    let mut indices = (0..minutiae.len()).collect::<Vec<_>>();
    indices.sort_by(|left_index, right_index| {
        let left = minutiae[*left_index];
        let right = minutiae[*right_index];
        right
            .quality
            .cmp(&left.quality)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.x_micrometres.cmp(&right.x_micrometres))
            .then_with(|| left.y_micrometres.cmp(&right.y_micrometres))
            .then_with(|| left.angle_microdegrees.cmp(&right.angle_microdegrees))
            .then_with(|| left_index.cmp(right_index))
    });
    indices.truncate(MAX_ALIGNMENT_ANCHORS);
    indices
}

type MinutiaeGrid = BTreeMap<(i64, i64), Vec<usize>>;

fn minutiae_grid(minutiae: &[Minutia]) -> MinutiaeGrid {
    let mut grid = BTreeMap::new();
    for (index, minutia) in minutiae.iter().enumerate() {
        let cell = (
            i64::from(minutia.x_micrometres / GRID_CELL_MICROMETRES),
            i64::from(minutia.y_micrometres / GRID_CELL_MICROMETRES),
        );
        grid.entry(cell).or_insert_with(Vec::new).push(index);
    }
    grid
}

fn aligned_matches(
    left: &[Minutia],
    right: &[Minutia],
    left_grid: &MinutiaeGrid,
    left_anchor_index: usize,
    right_anchor_index: usize,
) -> usize {
    let left_anchor = left[left_anchor_index];
    let right_anchor = right[right_anchor_index];
    let rotation = angle_degrees(&left_anchor) - angle_degrees(&right_anchor);
    let radians = rotation.to_radians();
    let (sin, cos) = radians.sin_cos();
    let mut candidates = Vec::new();
    for (right_index, right_minutia) in right.iter().enumerate() {
        let dx = f64::from(right_minutia.x_micrometres) - f64::from(right_anchor.x_micrometres);
        let dy = f64::from(right_minutia.y_micrometres) - f64::from(right_anchor.y_micrometres);
        let transformed_x = f64::from(left_anchor.x_micrometres) + dx * cos - dy * sin;
        let transformed_y = f64::from(left_anchor.y_micrometres) + dx * sin + dy * cos;
        let Some((cell_x, cell_y)) = transformed_grid_cell(transformed_x, transformed_y) else {
            continue;
        };
        for x_offset in -1_i64..=1 {
            for y_offset in -1_i64..=1 {
                let Some(left_indices) = left_grid.get(&(cell_x + x_offset, cell_y + y_offset))
                else {
                    continue;
                };
                for &left_index in left_indices {
                    let left_minutia = &left[left_index];
                    if !kinds_compatible(left_minutia.kind, right_minutia.kind) {
                        continue;
                    }
                    let x_error = f64::from(left_minutia.x_micrometres) - transformed_x;
                    let y_error = f64::from(left_minutia.y_micrometres) - transformed_y;
                    let distance_squared = x_error * x_error + y_error * y_error;
                    if distance_squared > MAX_DISTANCE_MICROMETRES * MAX_DISTANCE_MICROMETRES {
                        continue;
                    }
                    let transformed_angle = angle_degrees(right_minutia) + rotation;
                    let angle_error =
                        circular_angle_error(angle_degrees(left_minutia), transformed_angle);
                    if angle_error <= MAX_ANGLE_DEGREES {
                        candidates.push(Candidate {
                            left_index,
                            right_index,
                            distance_squared,
                            angle_error,
                        });
                    }
                }
            }
        }
    }
    candidates.sort_by(|left, right| {
        left.distance_squared
            .total_cmp(&right.distance_squared)
            .then_with(|| left.angle_error.total_cmp(&right.angle_error))
            .then_with(|| left.left_index.cmp(&right.left_index))
            .then_with(|| left.right_index.cmp(&right.right_index))
    });
    let mut used_left = vec![false; left.len()];
    let mut used_right = vec![false; right.len()];
    let mut matches = 0;
    for candidate in candidates {
        if used_left[candidate.left_index] || used_right[candidate.right_index] {
            continue;
        }
        used_left[candidate.left_index] = true;
        used_right[candidate.right_index] = true;
        matches += 1;
    }
    matches
}

fn transformed_grid_cell(x: f64, y: f64) -> Option<(i64, i64)> {
    Some((grid_coordinate(x)?, grid_coordinate(y)?))
}

fn grid_coordinate(value: f64) -> Option<i64> {
    let coordinate = (value / f64::from(GRID_CELL_MICROMETRES)).floor();
    if !coordinate.is_finite() || coordinate < i64::MIN as f64 || coordinate > i64::MAX as f64 {
        return None;
    }
    Some(coordinate as i64)
}

fn kinds_compatible(left: u8, right: u8) -> bool {
    left == 0 || right == 0 || left == right
}

fn angle_degrees(minutia: &Minutia) -> f64 {
    f64::from(minutia.angle_microdegrees) / 1_000_000.0
}

fn circular_angle_error(left: f64, right: f64) -> f64 {
    let difference = (left - right).rem_euclid(360.0);
    difference.min(360.0 - difference)
}

fn validate_threshold(threshold: f64) -> Result<(), FingerprintMatchError> {
    if threshold.is_finite() && (0.0..=1.0).contains(&threshold) {
        Ok(())
    } else {
        Err(FingerprintMatchError::InvalidThreshold)
    }
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    left_index: usize,
    right_index: usize,
    distance_squared: f64,
    angle_error: f64,
}

#[derive(Debug)]
pub enum FingerprintMatchError {
    Record(reddb_types::BiometricError),
    InvalidThreshold,
    IncompatibleFingerPositions,
}

impl fmt::Display for FingerprintMatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Record(error) => write!(f, "{error}"),
            Self::InvalidThreshold => {
                write!(f, "match threshold must be finite and between 0 and 1")
            }
            Self::IncompatibleFingerPositions => {
                write!(
                    f,
                    "fingerprint templates contain incompatible finger positions"
                )
            }
        }
    }
}

impl std::error::Error for FingerprintMatchError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(points: &[(u32, u32, u32, u8)]) -> MinutiaeView {
        MinutiaeView {
            position: 2,
            view: 0,
            minutiae: points
                .iter()
                .map(|(x, y, angle, kind)| Minutia {
                    x_micrometres: *x,
                    y_micrometres: *y,
                    angle_microdegrees: *angle,
                    kind: *kind,
                    quality: 80,
                })
                .collect(),
        }
    }

    #[test]
    fn alignment_is_translation_invariant_and_symmetric() {
        let left = view(&[
            (1_000, 1_000, 10_000_000, 1),
            (3_000, 1_000, 20_000_000, 2),
            (2_000, 3_000, 30_000_000, 1),
        ]);
        let right = view(&[
            (6_000, 8_000, 10_000_000, 1),
            (8_000, 8_000, 20_000_000, 2),
            (7_000, 10_000, 30_000_000, 1),
        ]);
        assert_eq!(compare_view_direction(&left, &right), 1.0);
        assert_eq!(
            compare_view_direction(&left, &right),
            compare_view_direction(&right, &left)
        );
    }

    #[test]
    fn alignment_is_rotation_invariant() {
        let left = view(&[
            (1_000, 1_000, 0, 1),
            (3_000, 1_000, 0, 2),
            (1_000, 3_000, 90_000_000, 1),
        ]);
        let rotated = view(&[
            (8_000, 6_000, 90_000_000, 1),
            (8_000, 8_000, 90_000_000, 2),
            (6_000, 6_000, 180_000_000, 1),
        ]);
        assert_eq!(compare_view_direction(&left, &rotated), 1.0);
        assert_eq!(compare_view_direction(&rotated, &left), 1.0);
    }

    #[test]
    fn missing_and_spurious_minutiae_keep_score_bounded_and_symmetric() {
        let left = view(&[
            (1_000, 1_000, 0, 1),
            (3_000, 1_000, 0, 2),
            (1_000, 3_000, 90_000_000, 1),
        ]);
        let noisy = view(&[
            (6_000, 8_000, 0, 1),
            (8_000, 8_000, 0, 2),
            (20_000, 20_000, 180_000_000, 1),
        ]);
        let forward = compare_view_direction(&left, &noisy);
        let reverse = compare_view_direction(&noisy, &left);
        assert!((0.0..=1.0).contains(&forward));
        assert!((0.0..=1.0).contains(&reverse));
        assert_eq!(forward, reverse);
    }

    #[test]
    fn threshold_must_be_finite_and_bounded() {
        assert!(validate_threshold(f64::NAN).is_err());
        assert!(validate_threshold(-0.1).is_err());
        assert!(validate_threshold(1.1).is_err());
        assert!(validate_threshold(0.0).is_ok());
        assert!(validate_threshold(1.0).is_ok());
    }

    #[test]
    fn identical_fingerprint_images_still_require_minutiae() {
        let header_length = 32_usize;
        let length = header_length + 14;
        let mut bytes = vec![0; length];
        bytes[..8].copy_from_slice(b"FIR\0010\0");
        let encoded_length = (length as u64).to_be_bytes();
        bytes[8..14].copy_from_slice(&encoded_length[2..]);
        bytes[18] = 1;
        bytes[19] = 1;
        for offset in (20..28).step_by(2) {
            bytes[offset..offset + 2].copy_from_slice(&500_u16.to_be_bytes());
        }
        bytes[28] = 8;
        bytes[header_length..header_length + 4].copy_from_slice(&14_u32.to_be_bytes());
        bytes[header_length + 4..header_length + 9].copy_from_slice(&[1, 1, 1, 100, 0]);
        bytes[header_length + 9..header_length + 11].copy_from_slice(&1_u16.to_be_bytes());
        bytes[header_length + 11..header_length + 13].copy_from_slice(&1_u16.to_be_bytes());
        let image = BiometricSample::with_format(
            reddb_types::BiometricFormat::FingerprintIso19794_4,
            bytes,
        )
        .expect("standard fingerprint image record");

        assert!(matches!(
            fingerprint_compare(&image, &image),
            Err(FingerprintMatchError::Record(
                reddb_types::BiometricError::MinutiaeTemplateRequired
            ))
        ));
    }

    #[test]
    fn alignment_anchor_count_is_bounded_and_deterministic() {
        let minutiae = (0_u32..64)
            .map(|index| Minutia {
                x_micrometres: index * 1_000,
                y_micrometres: index * 500,
                angle_microdegrees: index * 1_000_000,
                kind: (index % 3) as u8,
                quality: (index % 100) as u8,
            })
            .collect::<Vec<_>>();
        let first = alignment_anchor_indices(&minutiae);
        let second = alignment_anchor_indices(&minutiae);
        assert_eq!(first, second);
        assert_eq!(first.len(), MAX_ALIGNMENT_ANCHORS);
    }

    #[test]
    fn descriptor_bands_ignore_translation_and_rotation() {
        let left = view(&[
            (1_000, 1_000, 0, 1),
            (3_000, 1_000, 0, 2),
            (1_000, 3_000, 90_000_000, 1),
        ]);
        let rotated = view(&[
            (8_000, 6_000, 90_000_000, 1),
            (8_000, 8_000, 90_000_000, 2),
            (6_000, 6_000, 180_000_000, 1),
        ]);
        let left_bands = descriptor_bands_for_view(&left);
        let right_bands = descriptor_bands_for_view(&rotated);
        assert_eq!(left_bands, right_bands);
    }

    fn descriptor_bands_for_view(view: &MinutiaeView) -> Vec<u64> {
        let mut descriptors = BTreeSet::new();
        for (anchor_index, anchor) in view.minutiae.iter().enumerate() {
            let mut neighbours = view
                .minutiae
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != anchor_index)
                .map(|(_, neighbour)| {
                    let dx = i64::from(neighbour.x_micrometres) - i64::from(anchor.x_micrometres);
                    let dy = i64::from(neighbour.y_micrometres) - i64::from(anchor.y_micrometres);
                    (dx * dx + dy * dy, dx, dy, neighbour)
                })
                .collect::<Vec<_>>();
            neighbours
                .sort_by_key(|(distance, dx, dy, neighbour)| (*distance, *dx, *dy, neighbour.kind));
            if neighbours.is_empty() {
                continue;
            }
            let mut hash = 0xcbf2_9ce4_8422_2325_u64;
            descriptor_hash_byte(&mut hash, anchor.kind);
            descriptor_hash_byte(&mut hash, neighbours.len().min(3) as u8);
            for (distance_squared, dx, dy, neighbour) in neighbours.iter().take(3) {
                descriptor_hash_u16(
                    &mut hash,
                    ((*distance_squared as f64).sqrt() / 500.0).round() as u16,
                );
                let radial = (*dy as f64).atan2(*dx as f64).to_degrees();
                descriptor_hash_u16(
                    &mut hash,
                    (circular_degrees(radial - angle_degrees(anchor)) / 15.0).round() as u16,
                );
                descriptor_hash_u16(
                    &mut hash,
                    (circular_degrees(angle_degrees(neighbour) - angle_degrees(anchor)) / 15.0)
                        .round() as u16,
                );
                descriptor_hash_byte(&mut hash, neighbour.kind);
            }
            descriptors.insert(hash);
        }
        descriptors.into_iter().collect()
    }
}
