//! Caller-supplied landmark geometry and deterministic symmetry scoring.

use std::fmt;
use std::hash::{Hash, Hasher};

pub const MAX_LANDMARK_POINTS: usize = 4_096;
pub const ISO_19794_5_2005_LANDMARK_SPACE_ID: [u8; 16] = [
    0x19, 0x79, 0x45, 0x20, 0x05, 0x00, 0x50, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
];

pub type LandmarkCoordinates = [f32; 3];
pub type LandmarkPair = (LandmarkCoordinates, LandmarkCoordinates);

#[derive(Clone)]
pub struct LandmarkSet {
    space_id: [u8; 16],
    dimensions: u8,
    pairs: Vec<LandmarkPair>,
    midline: Vec<LandmarkCoordinates>,
}

impl fmt::Debug for LandmarkSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LandmarkSet")
            .field("dimensions", &self.dimensions)
            .field("points", &self.point_count())
            .finish()
    }
}

impl LandmarkSet {
    pub fn from_geometry_json(space_id: [u8; 16], bytes: &[u8]) -> Result<Self, LandmarkError> {
        let value = crate::serde_json::from_slice::<crate::serde_json::Value>(bytes)
            .map_err(|_| LandmarkError::InvalidGeometryJson)?;
        let object = value
            .as_object()
            .ok_or(LandmarkError::InvalidGeometryJson)?;
        if object.len() != 3
            || !object.contains_key("dimensions")
            || !object.contains_key("pairs")
            || !object.contains_key("midline")
        {
            return Err(LandmarkError::InvalidGeometryJson);
        }
        let dimensions = object["dimensions"]
            .as_i64()
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| matches!(value, 2 | 3))
            .ok_or(LandmarkError::InvalidDimensions)?;
        let parse_point = |value: &crate::serde_json::Value| {
            let values = value
                .as_array()
                .filter(|values| values.len() == usize::from(dimensions))
                .ok_or(LandmarkError::InvalidGeometryJson)?;
            let mut point = [0.0_f32; 3];
            for (target, value) in point.iter_mut().zip(values) {
                let coordinate = value.as_f64().ok_or(LandmarkError::InvalidGeometryJson)?;
                let stored = coordinate as f32;
                if !coordinate.is_finite() || !stored.is_finite() {
                    return Err(LandmarkError::NonFiniteCoordinate);
                }
                *target = stored;
            }
            Ok(point)
        };
        let pair_values = object["pairs"]
            .as_array()
            .ok_or(LandmarkError::InvalidGeometryJson)?;
        let mut pairs = Vec::with_capacity(pair_values.len().min(MAX_LANDMARK_POINTS / 2));
        for value in pair_values {
            let pair = value
                .as_object()
                .filter(|pair| {
                    pair.len() == 2 && pair.contains_key("left") && pair.contains_key("right")
                })
                .ok_or(LandmarkError::InvalidGeometryJson)?;
            pairs.push((parse_point(&pair["left"])?, parse_point(&pair["right"])?));
            if pairs.len() * 2 > MAX_LANDMARK_POINTS {
                return Err(LandmarkError::TooManyPoints);
            }
        }
        let midline_values = object["midline"]
            .as_array()
            .ok_or(LandmarkError::InvalidGeometryJson)?;
        if pairs
            .len()
            .checked_mul(2)
            .and_then(|count| count.checked_add(midline_values.len()))
            .is_none_or(|count| count > MAX_LANDMARK_POINTS)
        {
            return Err(LandmarkError::TooManyPoints);
        }
        let midline = midline_values
            .iter()
            .map(parse_point)
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(space_id, dimensions, pairs, midline)
    }

    pub fn new(
        space_id: [u8; 16],
        dimensions: u8,
        pairs: Vec<LandmarkPair>,
        midline: Vec<LandmarkCoordinates>,
    ) -> Result<Self, LandmarkError> {
        if !matches!(dimensions, 2 | 3) {
            return Err(LandmarkError::InvalidDimensions);
        }
        if pairs.len() < 2 {
            return Err(LandmarkError::InsufficientPairs);
        }
        let points = pairs
            .len()
            .checked_mul(2)
            .and_then(|points| points.checked_add(midline.len()))
            .ok_or(LandmarkError::TooManyPoints)?;
        if points > MAX_LANDMARK_POINTS {
            return Err(LandmarkError::TooManyPoints);
        }
        let coordinates = pairs
            .iter()
            .flat_map(|(left, right)| [left, right])
            .chain(midline.iter());
        for point in coordinates {
            if point[..usize::from(dimensions)]
                .iter()
                .any(|coordinate| !coordinate.is_finite())
            {
                return Err(LandmarkError::NonFiniteCoordinate);
            }
            if dimensions == 2 && point[2].to_bits() != 0.0_f32.to_bits() {
                return Err(LandmarkError::InvalidDimensions);
            }
        }
        Ok(Self {
            space_id,
            dimensions,
            pairs,
            midline,
        })
    }

    pub fn space_id(&self) -> [u8; 16] {
        self.space_id
    }

    pub fn dimensions(&self) -> u8 {
        self.dimensions
    }

    pub fn pairs(&self) -> &[LandmarkPair] {
        &self.pairs
    }

    pub fn midline(&self) -> &[LandmarkCoordinates] {
        &self.midline
    }

    pub fn point_count(&self) -> usize {
        self.pairs.len() * 2 + self.midline.len()
    }

    pub fn symmetry_score(&self) -> Result<f64, LandmarkError> {
        let dimensions = usize::from(self.dimensions);
        let mut normal = [0.0_f64; 3];
        for (left, right) in &self.pairs {
            let mut difference = [0.0_f64; 3];
            let mut length_squared = 0.0;
            for axis in 0..dimensions {
                difference[axis] = f64::from(right[axis]) - f64::from(left[axis]);
                length_squared += difference[axis] * difference[axis];
            }
            if length_squared == 0.0 {
                return Err(LandmarkError::DegenerateGeometry);
            }
            let length = length_squared.sqrt();
            for axis in 0..dimensions {
                normal[axis] += difference[axis] / length;
            }
        }
        let normal_length = normal[..dimensions]
            .iter()
            .map(|coordinate| coordinate * coordinate)
            .sum::<f64>()
            .sqrt();
        if normal_length <= self.pairs.len() as f64 * 1.0e-12 {
            return Err(LandmarkError::UndefinedSymmetryAxis);
        }
        for coordinate in &mut normal[..dimensions] {
            *coordinate /= normal_length;
        }

        let mut offset_sum = 0.0;
        for (left, right) in &self.pairs {
            for axis in 0..dimensions {
                offset_sum += normal[axis] * (f64::from(left[axis]) + f64::from(right[axis])) * 0.5;
            }
        }
        for point in &self.midline {
            offset_sum += dot(&normal, point, dimensions);
        }
        let constraint_count = self.pairs.len() + self.midline.len();
        let offset = offset_sum / constraint_count as f64;

        let mut error_squared = 0.0;
        for (left, right) in &self.pairs {
            let signed_distance = dot(&normal, left, dimensions) - offset;
            for axis in 0..dimensions {
                let reflected = f64::from(left[axis]) - 2.0 * signed_distance * normal[axis];
                let residual = reflected - f64::from(right[axis]);
                error_squared += residual * residual;
            }
        }
        for point in &self.midline {
            let distance = dot(&normal, point, dimensions) - offset;
            error_squared += distance * distance;
        }
        let symmetry_rmse = (error_squared / constraint_count as f64).sqrt();

        let mut centroid = [0.0_f64; 3];
        for (left, right) in &self.pairs {
            for axis in 0..dimensions {
                centroid[axis] += f64::from(left[axis]) + f64::from(right[axis]);
            }
        }
        for point in &self.midline {
            for axis in 0..dimensions {
                centroid[axis] += f64::from(point[axis]);
            }
        }
        let point_count = self.point_count();
        for coordinate in &mut centroid[..dimensions] {
            *coordinate /= point_count as f64;
        }
        let mut radius_squared = 0.0;
        for (left, right) in &self.pairs {
            radius_squared += squared_distance(left, &centroid, dimensions);
            radius_squared += squared_distance(right, &centroid, dimensions);
        }
        for point in &self.midline {
            radius_squared += squared_distance(point, &centroid, dimensions);
        }
        let scale = (radius_squared / point_count as f64).sqrt();
        if scale == 0.0 || !scale.is_finite() {
            return Err(LandmarkError::DegenerateGeometry);
        }
        if symmetry_rmse <= scale * 1.0e-12 {
            return Ok(1.0);
        }
        Ok((1.0 - symmetry_rmse / scale).clamp(0.0, 1.0))
    }

    pub fn is_symmetric(&self, threshold: f64) -> Result<bool, LandmarkError> {
        LandmarkError::validate_threshold(threshold)?;
        Ok(self.symmetry_score()? >= threshold)
    }
}

fn dot(normal: &[f64; 3], point: &LandmarkCoordinates, dimensions: usize) -> f64 {
    (0..dimensions)
        .map(|axis| normal[axis] * f64::from(point[axis]))
        .sum()
}

fn squared_distance(point: &LandmarkCoordinates, centre: &[f64; 3], dimensions: usize) -> f64 {
    (0..dimensions)
        .map(|axis| {
            let difference = f64::from(point[axis]) - centre[axis];
            difference * difference
        })
        .sum()
}

impl PartialEq for LandmarkSet {
    fn eq(&self, other: &Self) -> bool {
        self.space_id == other.space_id
            && self.dimensions == other.dimensions
            && points_equal(&self.pairs, &other.pairs)
            && coordinates_equal(&self.midline, &other.midline)
    }
}

fn points_equal(left: &[LandmarkPair], right: &[LandmarkPair]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|((ll, lr), (rl, rr))| coordinate_equal(ll, rl) && coordinate_equal(lr, rr))
}

fn coordinates_equal(left: &[LandmarkCoordinates], right: &[LandmarkCoordinates]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| coordinate_equal(left, right))
}

fn coordinate_equal(left: &LandmarkCoordinates, right: &LandmarkCoordinates) -> bool {
    left.iter()
        .zip(right)
        .all(|(left, right)| left.to_bits() == right.to_bits())
}

impl Eq for LandmarkSet {}

impl Hash for LandmarkSet {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.space_id.hash(state);
        self.dimensions.hash(state);
        self.pairs.len().hash(state);
        for (left, right) in &self.pairs {
            hash_coordinates(left, state);
            hash_coordinates(right, state);
        }
        self.midline.len().hash(state);
        for point in &self.midline {
            hash_coordinates(point, state);
        }
    }
}

fn hash_coordinates<H: Hasher>(coordinates: &LandmarkCoordinates, state: &mut H) {
    for coordinate in coordinates {
        coordinate.to_bits().hash(state);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandmarkError {
    InvalidGeometryJson,
    InvalidDimensions,
    InsufficientPairs,
    TooManyPoints,
    NonFiniteCoordinate,
    DegenerateGeometry,
    UndefinedSymmetryAxis,
    InvalidThreshold,
    UnsupportedFaceFormat,
    AmbiguousFaceRecord,
    UnknownFeatureCode,
    DuplicateFeatureCode,
    IncompleteTopology,
    TruncatedLandmarkBlock,
}

impl LandmarkError {
    pub fn validate_threshold(threshold: f64) -> Result<(), Self> {
        if threshold.is_finite() && (0.0..=1.0).contains(&threshold) {
            Ok(())
        } else {
            Err(Self::InvalidThreshold)
        }
    }
}

impl fmt::Display for LandmarkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidGeometryJson => "landmark geometry JSON is invalid",
            Self::InvalidDimensions => "landmark geometry must use two or three dimensions",
            Self::InsufficientPairs => "landmark geometry requires at least two bilateral pairs",
            Self::TooManyPoints => "landmark geometry exceeds the point limit",
            Self::NonFiniteCoordinate => "landmark coordinates must be finite",
            Self::DegenerateGeometry => "landmark geometry is degenerate",
            Self::UndefinedSymmetryAxis => "landmark symmetry axis is undefined",
            Self::InvalidThreshold => "symmetry threshold must be finite and between 0 and 1",
            Self::UnsupportedFaceFormat => {
                "face record does not contain a supported landmark block"
            }
            Self::AmbiguousFaceRecord => "face record contains multiple landmark blocks",
            Self::UnknownFeatureCode => "face landmark block contains an unsupported feature code",
            Self::DuplicateFeatureCode => "face landmark block contains a duplicate feature code",
            Self::IncompleteTopology => "face landmark block has insufficient bilateral topology",
            Self::TruncatedLandmarkBlock => "face landmark block is truncated",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for LandmarkError {}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACE: [u8; 16] = [7; 16];

    fn symmetric() -> LandmarkSet {
        LandmarkSet::new(
            SPACE,
            2,
            vec![
                ([-2.0, 1.0, 0.0], [2.0, 1.0, 0.0]),
                ([-1.0, -1.0, 0.0], [1.0, -1.0, 0.0]),
            ],
            vec![[0.0, 2.0, 0.0]],
        )
        .expect("valid landmarks")
    }

    #[test]
    fn exact_geometry_scores_one_and_rejects_bad_thresholds() {
        let landmarks = symmetric();
        assert_eq!(landmarks.symmetry_score(), Ok(1.0));
        assert_eq!(landmarks.is_symmetric(1.0), Ok(true));
        assert_eq!(
            landmarks.is_symmetric(f64::NAN),
            Err(LandmarkError::InvalidThreshold)
        );
    }

    #[test]
    fn score_is_invariant_to_translation_rotation_scale_and_global_swap() {
        let source = LandmarkSet::new(
            SPACE,
            2,
            vec![
                ([-2.0, 1.0, 0.0], [2.0, 1.2, 0.0]),
                ([-1.0, -1.0, 0.0], [1.0, -0.8, 0.0]),
            ],
            vec![[0.1, 2.0, 0.0]],
        )
        .expect("valid landmarks");
        let transform = |point: LandmarkCoordinates| {
            let angle = 0.7_f32;
            let x = point[0] * angle.cos() - point[1] * angle.sin();
            let y = point[0] * angle.sin() + point[1] * angle.cos();
            [x * 3.0 + 10.0, y * 3.0 - 4.0, 0.0]
        };
        let transformed = LandmarkSet::new(
            SPACE,
            2,
            source
                .pairs()
                .iter()
                .map(|(left, right)| (transform(*right), transform(*left)))
                .collect(),
            source.midline().iter().copied().map(transform).collect(),
        )
        .expect("transformed landmarks");
        let left = source.symmetry_score().expect("source score");
        let right = transformed.symmetry_score().expect("transformed score");
        assert!((left - right).abs() < 1.0e-6, "{left} != {right}");
        assert!((0.0..=1.0).contains(&left));
    }

    #[test]
    fn degenerate_and_undefined_geometries_are_rejected() {
        let coincident = LandmarkSet::new(
            SPACE,
            2,
            vec![([0.0; 3], [0.0; 3]), ([-1.0, 0.0, 0.0], [1.0, 0.0, 0.0])],
            Vec::new(),
        )
        .expect("structurally valid");
        assert_eq!(
            coincident.symmetry_score(),
            Err(LandmarkError::DegenerateGeometry)
        );

        let cancelled = LandmarkSet::new(
            SPACE,
            2,
            vec![
                ([-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]),
                ([1.0, 1.0, 0.0], [-1.0, 1.0, 0.0]),
            ],
            Vec::new(),
        )
        .expect("structurally valid");
        assert_eq!(
            cancelled.symmetry_score(),
            Err(LandmarkError::UndefinedSymmetryAxis)
        );
    }

    #[test]
    fn three_dimensional_score_is_rigid_and_scale_invariant() {
        let source = LandmarkSet::new(
            SPACE,
            3,
            vec![
                ([-2.0, 1.0, 0.5], [2.0, 1.1, 0.5]),
                ([-1.0, -1.0, -0.5], [1.0, -0.9, -0.5]),
            ],
            vec![[0.1, 2.0, 1.0]],
        )
        .expect("valid 3D landmarks");
        let transform = |point: LandmarkCoordinates| {
            let angle = 0.4_f32;
            let x = point[0] * angle.cos() - point[1] * angle.sin();
            let y = point[0] * angle.sin() + point[1] * angle.cos();
            [x * 2.5 + 4.0, y * 2.5 - 7.0, point[2] * 2.5 + 3.0]
        };
        let transformed = LandmarkSet::new(
            SPACE,
            3,
            source
                .pairs()
                .iter()
                .map(|(left, right)| (transform(*left), transform(*right)))
                .collect(),
            source.midline().iter().copied().map(transform).collect(),
        )
        .expect("transformed 3D landmarks");

        let expected = source.symmetry_score().expect("source score");
        let actual = transformed.symmetry_score().expect("transformed score");
        assert!((expected - actual).abs() < 1.0e-6, "{expected} != {actual}");
    }

    #[test]
    fn constructor_enforces_finite_bounded_unweighted_geometry() {
        assert_eq!(
            LandmarkSet::new(
                SPACE,
                2,
                vec![
                    ([-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]),
                    ([f32::NAN, 1.0, 0.0], [1.0, 1.0, 0.0]),
                ],
                Vec::new(),
            ),
            Err(LandmarkError::NonFiniteCoordinate)
        );
        assert_eq!(
            LandmarkSet::new(
                SPACE,
                2,
                vec![([-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]); 2_049],
                Vec::new(),
            ),
            Err(LandmarkError::TooManyPoints)
        );
        assert_eq!(
            LandmarkSet::from_geometry_json(
                SPACE,
                br#"{"dimensions":2,"pairs":[{"left":[-1,0],"right":[1,0],"weight":1},{"left":[-1,1],"right":[1,1]}],"midline":[]}"#,
            ),
            Err(LandmarkError::InvalidGeometryJson)
        );
    }

    #[test]
    fn debug_output_does_not_expose_coordinates() {
        let debug = format!("{:?}", symmetric());
        assert_eq!(debug, "LandmarkSet { dimensions: 2, points: 5 }");
        assert!(!debug.contains("-2.0"));
    }
}
