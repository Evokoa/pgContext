//! Validated, storage-independent codec configuration.

use crate::{CodecError, CodecKind, Result};

const MIN_SCALAR_LEVELS: u16 = 2;
const MAX_SCALAR_LEVELS: u16 = 256;
const MAX_PRODUCT_CENTROIDS: usize = 256;
pub(crate) const DEFAULT_PRODUCT_SUBVECTOR_DIMENSIONS: usize = 8;

/// Finite increasing reconstruction bounds for uniform scalar quantization.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScalarBounds {
    minimum: f32,
    maximum: f32,
}

impl ScalarBounds {
    /// Creates validated scalar reconstruction bounds.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::InvalidCode`] when either bound is non-finite or
    /// `minimum` is not strictly less than `maximum`.
    pub fn new(minimum: f32, maximum: f32) -> Result<Self> {
        if !minimum.is_finite() || !maximum.is_finite() || minimum >= maximum {
            return Err(CodecError::InvalidCode(
                "scalar bounds must be finite and increasing".to_owned(),
            ));
        }
        Ok(Self { minimum, maximum })
    }

    /// Returns the minimum reconstruction value.
    #[must_use]
    pub const fn minimum(self) -> f32 {
        self.minimum
    }

    /// Returns the maximum reconstruction value.
    #[must_use]
    pub const fn maximum(self) -> f32 {
        self.maximum
    }
}

/// Immutable identity of one normalized codec configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CodecRevision(u64);

impl CodecRevision {
    pub(crate) const fn from_hash(hash: u64) -> Self {
        Self(if hash == 0 { 1 } else { hash })
    }

    /// Creates a stored codec revision, rejecting the reserved zero value.
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// Returns the stable nonzero revision value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Policy for producing final scores from quantized candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReconstructionPolicy {
    /// Re-read the declared authoritative source value and score it exactly.
    ExactSourceRerank,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CodecSpecConfig {
    Plain,
    Binary,
    Scalar {
        levels: u16,
        bounds: Option<ScalarBounds>,
    },
    Product {
        subvector_dimensions: Option<usize>,
        centroid_count: u16,
        iterations: usize,
    },
}

/// Validated codec configuration shared by every index implementation.
///
/// The type normalizes SQL aliases before persistence, so one configuration
/// always maps to one [`CodecRevision`]. It contains no trained values; those
/// belong to a trained artifact produced from an authoritative source sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CodecSpec {
    config: CodecSpecConfig,
}

impl CodecSpec {
    /// Returns the uncompressed serving configuration.
    #[must_use]
    pub const fn plain() -> Self {
        Self {
            config: CodecSpecConfig::Plain,
        }
    }

    /// Returns sign-bit quantization.
    #[must_use]
    pub const fn binary() -> Self {
        Self {
            config: CodecSpecConfig::Binary,
        }
    }

    /// Creates uniform scalar quantization.
    ///
    /// Omit `bounds` to train deterministic bounds from the source sample.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::InvalidCode`] unless `levels` is in `2..=256`.
    pub fn scalar(levels: u16, bounds: Option<ScalarBounds>) -> Result<Self> {
        if !(MIN_SCALAR_LEVELS..=MAX_SCALAR_LEVELS).contains(&levels) {
            return Err(CodecError::InvalidCode(format!(
                "scalar levels must be in 2..=256, got {levels}"
            )));
        }
        Ok(Self {
            config: CodecSpecConfig::Scalar { levels, bounds },
        })
    }

    /// Creates deterministic product quantization training parameters.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::InvalidCode`] when a parameter is zero or when
    /// `centroid_count` exceeds the one-byte code domain.
    pub fn product(
        subvector_dimensions: usize,
        centroid_count: usize,
        iterations: usize,
    ) -> Result<Self> {
        if subvector_dimensions == 0 {
            return Err(CodecError::InvalidCode(
                "product subvector dimensions must be positive".to_owned(),
            ));
        }
        Self::product_with_dimensions(Some(subvector_dimensions), centroid_count, iterations)
    }

    /// Creates product quantization with a deterministic dimension-dependent
    /// subvector width.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::InvalidCode`] when `centroid_count` or
    /// `iterations` is invalid.
    pub fn product_auto(centroid_count: usize, iterations: usize) -> Result<Self> {
        Self::product_with_dimensions(None, centroid_count, iterations)
    }

    /// Returns the codec family.
    #[must_use]
    pub const fn kind(self) -> CodecKind {
        match self.config {
            CodecSpecConfig::Plain => CodecKind::Plain,
            CodecSpecConfig::Binary => CodecKind::Binary,
            CodecSpecConfig::Scalar { .. } => CodecKind::Scalar,
            CodecSpecConfig::Product { .. } => CodecKind::Product,
        }
    }

    /// Returns the final-score policy required by this codec.
    #[must_use]
    pub const fn reconstruction_policy(self) -> ReconstructionPolicy {
        ReconstructionPolicy::ExactSourceRerank
    }

    /// Returns a stable identity derived from the normalized configuration.
    #[must_use]
    pub fn revision(self) -> CodecRevision {
        let mut hash = FNV_OFFSET_BASIS;
        let kind = match self.kind() {
            CodecKind::Plain => 0,
            CodecKind::Binary => 1,
            CodecKind::Scalar => 2,
            CodecKind::Product => 3,
        };
        hash = hash_bytes(hash, &[kind]);
        match self.config {
            CodecSpecConfig::Plain | CodecSpecConfig::Binary => {}
            CodecSpecConfig::Scalar { levels, bounds } => {
                hash = hash_bytes(hash, &levels.to_le_bytes());
                if let Some(bounds) = bounds {
                    hash = hash_bytes(hash, &bounds.minimum.to_bits().to_le_bytes());
                    hash = hash_bytes(hash, &bounds.maximum.to_bits().to_le_bytes());
                }
            }
            CodecSpecConfig::Product {
                subvector_dimensions,
                centroid_count,
                iterations,
            } => {
                hash = hash_usize(hash, subvector_dimensions.unwrap_or(0));
                hash = hash_bytes(hash, &centroid_count.to_le_bytes());
                hash = hash_usize(hash, iterations);
            }
        }
        CodecRevision::from_hash(hash)
    }

    /// Returns scalar parameters when this is a scalar codec.
    #[must_use]
    pub const fn scalar_parameters(self) -> Option<(u16, Option<ScalarBounds>)> {
        match self.config {
            CodecSpecConfig::Scalar { levels, bounds } => Some((levels, bounds)),
            _ => None,
        }
    }

    /// Returns product-training parameters when this is a product codec.
    #[must_use]
    pub const fn product_parameters(self) -> Option<(Option<usize>, usize, usize)> {
        match self.config {
            CodecSpecConfig::Product {
                subvector_dimensions,
                centroid_count,
                iterations,
            } => Some((subvector_dimensions, centroid_count as usize, iterations)),
            _ => None,
        }
    }

    fn product_with_dimensions(
        subvector_dimensions: Option<usize>,
        centroid_count: usize,
        iterations: usize,
    ) -> Result<Self> {
        if !(1..=MAX_PRODUCT_CENTROIDS).contains(&centroid_count) {
            return Err(CodecError::InvalidCode(format!(
                "product centroid count must be in 1..=256, got {centroid_count}"
            )));
        }
        if iterations == 0 {
            return Err(CodecError::InvalidCode(
                "product training iterations must be positive".to_owned(),
            ));
        }
        let centroid_count = u16::try_from(centroid_count).map_err(|_| {
            CodecError::InvalidCode(format!(
                "product centroid count exceeds u16: {centroid_count}"
            ))
        })?;
        Ok(Self {
            config: CodecSpecConfig::Product {
                subvector_dimensions,
                centroid_count,
                iterations,
            },
        })
    }
}

pub(crate) const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

pub(crate) fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn hash_usize(hash: u64, value: usize) -> u64 {
    let value = u64::try_from(value).unwrap_or(u64::MAX);
    hash_bytes(hash, &value.to_le_bytes())
}
