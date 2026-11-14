//! Caller-supplied, extractor-agnostic comparison artifacts.

use std::fmt;
use std::hash::{Hash, Hasher};

/// Largest feature vector admitted by the logical type.
pub const MAX_FEATURE_VECTOR_DIMENSIONS: usize = 65_536;
/// Largest bit signature admitted by the logical type (one MiB).
pub const MAX_BIT_SIGNATURE_BYTES: usize = 1_048_576;

#[derive(Debug, Clone)]
pub struct FeatureVector {
    space_id: [u8; 16],
    values: Vec<f32>,
}

impl PartialEq for FeatureVector {
    fn eq(&self, other: &Self) -> bool {
        self.space_id == other.space_id
            && self.values.len() == other.values.len()
            && self
                .values
                .iter()
                .zip(&other.values)
                .all(|(left, right)| left.to_bits() == right.to_bits())
    }
}

impl FeatureVector {
    pub fn new(space_id: [u8; 16], values: Vec<f32>) -> Result<Self, FeatureError> {
        if values.is_empty() {
            return Err(FeatureError::EmptyFeatureVector);
        }
        if values.len() > MAX_FEATURE_VECTOR_DIMENSIONS {
            return Err(FeatureError::FeatureVectorTooLarge {
                dimensions: values.len(),
                maximum: MAX_FEATURE_VECTOR_DIMENSIONS,
            });
        }
        if values.iter().any(|value| !value.is_finite()) {
            return Err(FeatureError::NonFiniteFeatureValue);
        }
        let norm_squared = values
            .iter()
            .map(|value| f64::from(*value) * f64::from(*value))
            .sum::<f64>();
        if norm_squared == 0.0 {
            return Err(FeatureError::ZeroNormFeatureVector);
        }
        Ok(Self { space_id, values })
    }

    pub fn space_id(&self) -> [u8; 16] {
        self.space_id
    }

    pub fn values(&self) -> &[f32] {
        &self.values
    }

    pub fn dimensions(&self) -> usize {
        self.values.len()
    }

    /// Cosine similarity normalized from `[-1, 1]` into `[0, 1]`.
    pub fn compare(&self, other: &Self) -> Result<f64, FeatureError> {
        if self.space_id != other.space_id {
            return Err(FeatureError::IncompatibleSpace);
        }
        if self.values.len() != other.values.len() {
            return Err(FeatureError::IncompatibleDimensions {
                left: self.values.len(),
                right: other.values.len(),
            });
        }
        if self == other {
            return Ok(1.0);
        }

        let mut dot = 0.0_f64;
        let mut left_norm = 0.0_f64;
        let mut right_norm = 0.0_f64;
        for (left, right) in self.values.iter().zip(&other.values) {
            let left = f64::from(*left);
            let right = f64::from(*right);
            dot += left * right;
            left_norm += left * left;
            right_norm += right * right;
        }
        let cosine = dot / (left_norm.sqrt() * right_norm.sqrt());
        Ok(((cosine.clamp(-1.0, 1.0) + 1.0) * 0.5).clamp(0.0, 1.0))
    }
}

impl Eq for FeatureVector {}

impl Hash for FeatureVector {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.space_id.hash(state);
        self.values.len().hash(state);
        for value in &self.values {
            value.to_bits().hash(state);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BitSignature {
    space_id: [u8; 16],
    bits: Vec<u8>,
    mask: Vec<u8>,
}

impl BitSignature {
    pub fn new(
        space_id: [u8; 16],
        bits: Vec<u8>,
        mask: Option<Vec<u8>>,
    ) -> Result<Self, FeatureError> {
        if bits.is_empty() {
            return Err(FeatureError::EmptyBitSignature);
        }
        if bits.len() > MAX_BIT_SIGNATURE_BYTES {
            return Err(FeatureError::BitSignatureTooLarge {
                bytes: bits.len(),
                maximum: MAX_BIT_SIGNATURE_BYTES,
            });
        }
        let mask = mask.unwrap_or_else(|| vec![u8::MAX; bits.len()]);
        if mask.len() != bits.len() {
            return Err(FeatureError::InvalidMaskLength {
                bits: bits.len(),
                mask: mask.len(),
            });
        }
        if mask.iter().all(|byte| *byte == 0) {
            return Err(FeatureError::EmptyBitMask);
        }
        Ok(Self {
            space_id,
            bits,
            mask,
        })
    }

    pub fn space_id(&self) -> [u8; 16] {
        self.space_id
    }

    pub fn bits(&self) -> &[u8] {
        &self.bits
    }

    pub fn mask(&self) -> &[u8] {
        &self.mask
    }

    pub fn len_bytes(&self) -> usize {
        self.bits.len()
    }

    /// Hamming similarity over bits marked valid by both masks.
    pub fn compare(&self, other: &Self) -> Result<f64, FeatureError> {
        if self.space_id != other.space_id {
            return Err(FeatureError::IncompatibleSpace);
        }
        if self.bits.len() != other.bits.len() {
            return Err(FeatureError::IncompatibleBitLengths {
                left: self.bits.len(),
                right: other.bits.len(),
            });
        }
        if self == other {
            return Ok(1.0);
        }

        let mut differing = 0_u64;
        let mut valid = 0_u64;
        for index in 0..self.bits.len() {
            let common_mask = self.mask[index] & other.mask[index];
            valid += u64::from(common_mask.count_ones());
            differing +=
                u64::from(((self.bits[index] ^ other.bits[index]) & common_mask).count_ones());
        }
        if valid == 0 {
            return Err(FeatureError::NoCommonUnmaskedBits);
        }
        Ok(1.0 - (differing as f64 / valid as f64))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeatureError {
    EmptyFeatureVector,
    FeatureVectorTooLarge { dimensions: usize, maximum: usize },
    NonFiniteFeatureValue,
    ZeroNormFeatureVector,
    EmptyBitSignature,
    BitSignatureTooLarge { bytes: usize, maximum: usize },
    InvalidMaskLength { bits: usize, mask: usize },
    EmptyBitMask,
    IncompatibleSpace,
    IncompatibleDimensions { left: usize, right: usize },
    IncompatibleBitLengths { left: usize, right: usize },
    NoCommonUnmaskedBits,
    InvalidThreshold,
}

impl FeatureError {
    pub fn validate_threshold(threshold: f64) -> Result<(), Self> {
        if threshold.is_finite() && (0.0..=1.0).contains(&threshold) {
            Ok(())
        } else {
            Err(Self::InvalidThreshold)
        }
    }
}

impl fmt::Display for FeatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFeatureVector => write!(f, "feature vector cannot be empty"),
            Self::FeatureVectorTooLarge {
                dimensions,
                maximum,
            } => write!(
                f,
                "feature vector has {dimensions} dimensions; maximum is {maximum}"
            ),
            Self::NonFiniteFeatureValue => write!(f, "feature vector values must be finite"),
            Self::ZeroNormFeatureVector => write!(f, "feature vector must have non-zero norm"),
            Self::EmptyBitSignature => write!(f, "bit signature cannot be empty"),
            Self::BitSignatureTooLarge { bytes, maximum } => {
                write!(f, "bit signature has {bytes} bytes; maximum is {maximum}")
            }
            Self::InvalidMaskLength { bits, mask } => {
                write!(f, "bit signature has {bits} bytes but mask has {mask}")
            }
            Self::EmptyBitMask => write!(f, "bit signature mask has no valid bits"),
            Self::IncompatibleSpace => write!(f, "feature spaces are incompatible"),
            Self::IncompatibleDimensions { left, right } => {
                write!(
                    f,
                    "feature dimensions are incompatible ({left} and {right})"
                )
            }
            Self::IncompatibleBitLengths { left, right } => {
                write!(
                    f,
                    "bit signature lengths are incompatible ({left} and {right})"
                )
            }
            Self::NoCommonUnmaskedBits => write!(f, "bit signatures have no common valid bits"),
            Self::InvalidThreshold => {
                write!(f, "match threshold must be finite and between 0 and 1")
            }
        }
    }
}

impl std::error::Error for FeatureError {}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACE: [u8; 16] = [7; 16];

    #[test]
    fn cosine_is_normalized_and_symmetric() {
        let left = FeatureVector::new(SPACE, vec![1.0, 0.0]).expect("valid vector");
        let right = FeatureVector::new(SPACE, vec![0.0, 1.0]).expect("valid vector");
        assert_eq!(left.compare(&right), Ok(0.5));
        assert_eq!(left.compare(&right), right.compare(&left));
        assert_eq!(left.compare(&left), Ok(1.0));
    }

    #[test]
    fn masked_hamming_uses_only_common_valid_bits() {
        let left = BitSignature::new(SPACE, vec![0b1111_0000], Some(vec![0b1111_0000]))
            .expect("valid signature");
        let right = BitSignature::new(SPACE, vec![0b1010_1111], Some(vec![0b1111_1111]))
            .expect("valid signature");
        assert_eq!(left.compare(&right), Ok(0.5));
        assert_eq!(left.compare(&right), right.compare(&left));
    }

    #[test]
    fn incompatible_spaces_are_errors() {
        let left = FeatureVector::new(SPACE, vec![1.0]).expect("valid vector");
        let right = FeatureVector::new([8; 16], vec![1.0]).expect("valid vector");
        assert_eq!(left.compare(&right), Err(FeatureError::IncompatibleSpace));
    }

    #[test]
    fn dimensions_lengths_masks_and_thresholds_fail_explicitly() {
        let short = FeatureVector::new(SPACE, vec![1.0]).expect("valid vector");
        let long = FeatureVector::new(SPACE, vec![1.0, 0.0]).expect("valid vector");
        assert_eq!(
            short.compare(&long),
            Err(FeatureError::IncompatibleDimensions { left: 1, right: 2 })
        );

        let short = BitSignature::new(SPACE, vec![0], None).expect("valid bits");
        let long = BitSignature::new(SPACE, vec![0, 0], None).expect("valid bits");
        assert_eq!(
            short.compare(&long),
            Err(FeatureError::IncompatibleBitLengths { left: 1, right: 2 })
        );
        assert_eq!(
            BitSignature::new(SPACE, vec![0], Some(vec![0])),
            Err(FeatureError::EmptyBitMask)
        );
        assert_eq!(
            BitSignature::new(SPACE, vec![0], Some(Vec::new())),
            Err(FeatureError::InvalidMaskLength { bits: 1, mask: 0 })
        );
        assert_eq!(
            FeatureError::validate_threshold(f64::NAN),
            Err(FeatureError::InvalidThreshold)
        );
        assert_eq!(
            FeatureError::validate_threshold(f64::INFINITY),
            Err(FeatureError::InvalidThreshold)
        );
        assert_eq!(
            FeatureError::validate_threshold(-0.01),
            Err(FeatureError::InvalidThreshold)
        );
        assert_eq!(
            FeatureError::validate_threshold(1.01),
            Err(FeatureError::InvalidThreshold)
        );
    }

    proptest::proptest! {
        #[test]
        fn cosine_comparison_is_deterministic_symmetric_and_bounded(
            left in proptest::collection::vec(-1_000_i16..=1_000, 16),
            right in proptest::collection::vec(-1_000_i16..=1_000, 16),
        ) {
            proptest::prop_assume!(left.iter().any(|value| *value != 0));
            proptest::prop_assume!(right.iter().any(|value| *value != 0));
            let left = FeatureVector::new(
                SPACE,
                left.into_iter().map(f32::from).collect(),
            ).expect("non-zero finite vector");
            let right = FeatureVector::new(
                SPACE,
                right.into_iter().map(f32::from).collect(),
            ).expect("non-zero finite vector");
            let first = left.compare(&right).expect("compatible vectors");
            let second = left.compare(&right).expect("repeat comparison");
            let reverse = right.compare(&left).expect("reverse comparison");
            proptest::prop_assert!((0.0..=1.0).contains(&first));
            proptest::prop_assert_eq!(first, second);
            proptest::prop_assert_eq!(first, reverse);
        }

        #[test]
        fn hamming_comparison_is_deterministic_symmetric_and_bounded(
            left in proptest::collection::vec(proptest::num::u8::ANY, 32),
            right in proptest::collection::vec(proptest::num::u8::ANY, 32),
        ) {
            let left = BitSignature::new(SPACE, left, None).expect("valid bits");
            let right = BitSignature::new(SPACE, right, None).expect("valid bits");
            let first = left.compare(&right).expect("compatible signatures");
            let second = left.compare(&right).expect("repeat comparison");
            let reverse = right.compare(&left).expect("reverse comparison");
            proptest::prop_assert!((0.0..=1.0).contains(&first));
            proptest::prop_assert_eq!(first, second);
            proptest::prop_assert_eq!(first, reverse);
        }
    }
}
