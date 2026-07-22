//! Deterministic quantization-codebook training for rebuildable artifacts.

use context_core::{DenseVector, DistanceMetric, Error as CoreError};

use crate::{
    ProductCodebook, ProductQuantizedVector, ProductQuantizer, Result, ScalarQuantizedVector,
    ScalarQuantizer,
};

/// A trained, self-contained quantizer suitable for persistence in an artifact.
#[derive(Debug, Clone, PartialEq)]
pub enum TrainedQuantizer {
    /// Sign-bit encoding with a fixed original dimension.
    Binary {
        /// Original dense-vector dimensions.
        dimensions: usize,
    },
    /// Uniform scalar byte encoding.
    Scalar {
        /// Trained uniform codebook.
        quantizer: ScalarQuantizer,
        /// Original dense-vector dimensions.
        dimensions: usize,
    },
    /// Product encoding with deterministic per-subvector codebooks.
    Product(ProductQuantizer),
}

impl TrainedQuantizer {
    /// Creates a binary sign quantizer for a fixed dimension.
    ///
    /// # Errors
    ///
    /// Returns an error when `dimensions` is zero.
    pub fn binary(dimensions: usize) -> Result<Self> {
        if dimensions == 0 {
            return Err(CoreError::InvalidVector(
                "binary quantizer dimensions must be positive".to_owned(),
            )
            .into());
        }
        Ok(Self::Binary { dimensions })
    }

    /// Returns the dense-vector dimensions accepted by this quantizer.
    #[must_use]
    pub fn dimensions(&self) -> usize {
        match self {
            Self::Binary { dimensions } => *dimensions,
            Self::Scalar { dimensions, .. } => *dimensions,
            Self::Product(quantizer) => quantizer
                .subvector_dimensions()
                .saturating_mul(quantizer.codebooks().len()),
        }
    }

    /// Encodes one validated dense vector into bounded bytes.
    ///
    /// # Errors
    ///
    /// Returns an error for dimension mismatches or invalid codebook state.
    pub fn quantize(&self, vector: &DenseVector) -> Result<Vec<u8>> {
        match self {
            Self::Binary { dimensions } => {
                require_dimensions(*dimensions, vector.dimension())?;
                Ok(pack_sign_bits(vector))
            }
            Self::Scalar {
                quantizer,
                dimensions,
            } => {
                require_dimensions(*dimensions, vector.dimension())?;
                Ok(quantizer.quantize(vector)?.codes().to_vec())
            }
            Self::Product(quantizer) => Ok(quantizer.quantize(vector)?.codes().to_vec()),
        }
    }

    /// Reconstructs an approximate dense vector from persisted bytes.
    ///
    /// # Errors
    ///
    /// Returns an error for truncated, oversized, or out-of-codebook codes.
    pub fn reconstruct(&self, code: &[u8]) -> Result<DenseVector> {
        match self {
            Self::Binary { dimensions } => unpack_sign_bits(code, *dimensions),
            Self::Scalar {
                quantizer,
                dimensions,
            } => {
                require_dimensions(*dimensions, code.len())?;
                quantizer.reconstruct(&ScalarQuantizedVector::new(code.to_vec())?)
            }
            Self::Product(quantizer) => {
                quantizer.reconstruct(&ProductQuantizedVector::new(code.to_vec())?)
            }
        }
    }

    /// Returns scalar codebook metadata when this is a scalar quantizer.
    #[must_use]
    pub const fn scalar(&self) -> Option<ScalarQuantizer> {
        match self {
            Self::Scalar { quantizer, .. } => Some(*quantizer),
            _ => None,
        }
    }

    /// Returns the product codebook when this is a product quantizer.
    #[must_use]
    pub const fn product(&self) -> Option<&ProductQuantizer> {
        match self {
            Self::Product(quantizer) => Some(quantizer),
            _ => None,
        }
    }
}

/// Trains uniform scalar bounds from a non-empty, dimension-consistent sample.
///
/// Explicit bounds override the observed extrema. A constant observed sample is
/// widened deterministically so the persisted scalar codebook remains valid.
///
/// # Errors
///
/// Returns an error for an empty/inconsistent sample, invalid explicit bounds,
/// or an unsupported level count.
pub fn train_scalar_quantizer(
    sample: &[DenseVector],
    levels: u16,
    explicit_bounds: Option<(f32, f32)>,
) -> Result<TrainedQuantizer> {
    validate_sample(sample)?;
    let (mut minimum, mut maximum) = match explicit_bounds {
        Some(bounds) => bounds,
        None => sample
            .iter()
            .flat_map(|vector| vector.as_slice().iter().copied())
            .fold(
                (f32::INFINITY, f32::NEG_INFINITY),
                |(minimum, maximum), value| (minimum.min(value), maximum.max(value)),
            ),
    };
    if explicit_bounds.is_none() && minimum == maximum {
        let lower = minimum.next_down();
        let upper = maximum.next_up();
        if lower.is_finite() {
            minimum = lower;
        }
        if upper.is_finite() {
            maximum = upper;
        }
    }
    Ok(TrainedQuantizer::Scalar {
        quantizer: ScalarQuantizer::new(minimum, maximum, levels)?,
        dimensions: sample[0].dimension(),
    })
}

/// Trains deterministic product codebooks with bounded Lloyd iterations.
///
/// Initial centroids are sampled at stable, evenly distributed row positions;
/// empty clusters retain their preceding centroid. This makes rebuilds over an
/// identical ordered source sample byte-reproducible.
///
/// # Errors
///
/// Returns an error for empty/inconsistent samples, incompatible subvector
/// dimensions, zero or excessive centroid counts, or zero iterations.
pub fn train_product_quantizer(
    sample: &[DenseVector],
    subvector_dimensions: usize,
    centroid_count: usize,
    iterations: usize,
) -> Result<TrainedQuantizer> {
    let dimensions = validate_sample(sample)?;
    if subvector_dimensions == 0 || dimensions % subvector_dimensions != 0 {
        return Err(CoreError::InvalidVector(format!(
            "product quantization dimensions {dimensions} are not divisible by subvector dimensions {subvector_dimensions}"
        ))
        .into());
    }
    if centroid_count == 0 || centroid_count > 256 || centroid_count > sample.len() {
        return Err(CoreError::InvalidVector(format!(
            "product quantization centroid count must be in 1..={}: {centroid_count}",
            sample.len().min(256)
        ))
        .into());
    }
    if iterations == 0 {
        return Err(CoreError::InvalidVector(
            "product quantization training iterations must be positive".to_owned(),
        )
        .into());
    }

    let subvector_count = dimensions / subvector_dimensions;
    let mut codebooks = Vec::with_capacity(subvector_count);
    for subvector_index in 0..subvector_count {
        let start = subvector_index * subvector_dimensions;
        let end = start + subvector_dimensions;
        let subvectors = sample
            .iter()
            .map(|vector| DenseVector::new(vector.as_slice()[start..end].to_vec()))
            .collect::<core::result::Result<Vec<_>, _>>()?;
        let mut centroids = (0..centroid_count)
            .map(|index| {
                let sample_index = index.saturating_mul(subvectors.len()) / centroid_count;
                subvectors[sample_index].clone()
            })
            .collect::<Vec<_>>();
        for _ in 0..iterations {
            centroids = lloyd_step(&subvectors, &centroids)?;
        }
        codebooks.push(ProductCodebook::new(centroids)?);
    }
    Ok(TrainedQuantizer::Product(ProductQuantizer::new(
        subvector_dimensions,
        codebooks,
    )?))
}

fn validate_sample(sample: &[DenseVector]) -> Result<usize> {
    let Some(first) = sample.first() else {
        return Err(CoreError::InvalidVector(
            "quantization training sample must not be empty".to_owned(),
        )
        .into());
    };
    let dimensions = first.dimension();
    if let Some(vector) = sample
        .iter()
        .find(|vector| vector.dimension() != dimensions)
    {
        return Err(crate::HnswError::DimensionMismatch {
            left: dimensions,
            right: vector.dimension(),
        });
    }
    Ok(dimensions)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "training sample counts are policy-bounded and only scale centroid means"
)]
fn lloyd_step(subvectors: &[DenseVector], centroids: &[DenseVector]) -> Result<Vec<DenseVector>> {
    let dimensions = centroids[0].dimension();
    let mut sums = vec![vec![0.0_f32; dimensions]; centroids.len()];
    let mut counts = vec![0_usize; centroids.len()];
    for subvector in subvectors {
        let code = nearest_centroid(subvector, centroids)?;
        counts[code] = counts[code].saturating_add(1);
        for (sum, value) in sums[code].iter_mut().zip(subvector.as_slice()) {
            *sum += *value;
        }
    }
    centroids
        .iter()
        .enumerate()
        .map(|(index, previous)| {
            if counts[index] == 0 {
                return Ok(previous.clone());
            }
            let denominator = counts[index] as f32;
            DenseVector::new(sums[index].iter().map(|sum| *sum / denominator).collect())
                .map_err(Into::into)
        })
        .collect()
}

fn nearest_centroid(vector: &DenseVector, centroids: &[DenseVector]) -> Result<usize> {
    let mut best = (0_usize, f32::INFINITY);
    for (index, centroid) in centroids.iter().enumerate() {
        let score = DistanceMetric::L2.distance(vector, centroid)?;
        if score < best.1 {
            best = (index, score);
        }
    }
    Ok(best.0)
}

fn pack_sign_bits(vector: &DenseVector) -> Vec<u8> {
    let mut code = vec![0_u8; vector.dimension().div_ceil(8)];
    for (index, value) in vector.as_slice().iter().enumerate() {
        if *value >= 0.0 {
            code[index / 8] |= 1 << (index % 8);
        }
    }
    code
}

fn unpack_sign_bits(code: &[u8], dimensions: usize) -> Result<DenseVector> {
    let expected = dimensions.div_ceil(8);
    if code.len() != expected {
        return Err(CoreError::InvalidVector(format!(
            "binary quantized code length mismatch: expected {expected}, got {}",
            code.len()
        ))
        .into());
    }
    let remainder = dimensions % 8;
    if remainder != 0 {
        let valid_mask = (1_u8 << remainder) - 1;
        if code.last().is_some_and(|byte| byte & !valid_mask != 0) {
            return Err(CoreError::InvalidVector(
                "binary quantized code has non-zero padding bits".to_owned(),
            )
            .into());
        }
    }
    DenseVector::new(
        (0..dimensions)
            .map(|index| {
                if code[index / 8] & (1 << (index % 8)) == 0 {
                    -1.0
                } else {
                    1.0
                }
            })
            .collect(),
    )
    .map_err(Into::into)
}

fn require_dimensions(expected: usize, actual: usize) -> Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(crate::HnswError::DimensionMismatch {
            left: expected,
            right: actual,
        })
    }
}

#[cfg(test)]
mod tests {
    use context_core::DenseVector;
    use proptest::prelude::*;

    use super::{TrainedQuantizer, train_product_quantizer, train_scalar_quantizer};

    proptest! {
        #[test]
        fn scalar_training_respects_uniform_error_bound(values in prop::collection::vec(-10.0_f32..10.0, 1..128)) {
            let sample = vec![DenseVector::new(values.clone())?];
            let trained = train_scalar_quantizer(&sample, 256, None)?;
            let reconstructed = trained.reconstruct(&trained.quantize(&sample[0])?)?;
            let Some(scalar) = trained.scalar() else {
                prop_assert!(false, "scalar training returned a non-scalar quantizer");
                return Ok(());
            };
            let scale_rounding = f32::EPSILON
                * (scalar.min().abs() + scalar.max().abs() + 1.0)
                * 4.0;
            let error_bound = (scalar.max() - scalar.min())
                / f32::from(scalar.levels() - 1)
                / 2.0
                + scale_rounding;
            for (actual, approximate) in values.iter().zip(reconstructed.as_slice()) {
                prop_assert!((*actual - *approximate).abs() <= error_bound);
            }
        }
    }

    #[test]
    fn product_training_is_deterministic_and_dimension_preserving() -> crate::Result<()> {
        let sample = [
            DenseVector::new(vec![0.0, 0.0, 1.0, 1.0])?,
            DenseVector::new(vec![0.1, 0.0, 0.9, 1.0])?,
            DenseVector::new(vec![5.0, 5.0, 6.0, 6.0])?,
            DenseVector::new(vec![5.1, 5.0, 5.9, 6.0])?,
        ];
        let first = train_product_quantizer(&sample, 2, 2, 4)?;
        let second = train_product_quantizer(&sample, 2, 2, 4)?;
        assert_eq!(first, second);
        for vector in sample {
            assert_eq!(first.reconstruct(&first.quantize(&vector)?)?.dimension(), 4);
        }
        Ok(())
    }

    #[test]
    fn binary_codes_reject_truncation_and_nonzero_padding() -> crate::Result<()> {
        let quantizer = TrainedQuantizer::binary(9)?;
        let vector = DenseVector::new(vec![-1.0, 0.0, 2.0, -3.0, 1.0, -1.0, 1.0, -1.0, 1.0])?;
        let code = quantizer.quantize(&vector)?;
        assert_eq!(quantizer.reconstruct(&code)?.dimension(), 9);
        assert!(quantizer.reconstruct(&code[..1]).is_err());
        assert!(
            quantizer
                .reconstruct(&[code[0], code[1] | 0b1000_0000])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn scalar_training_handles_constant_and_opposite_finite_extremes() -> crate::Result<()> {
        for value in [1.0e20_f32, f32::MAX, -f32::MAX] {
            let vector = DenseVector::new(vec![value])?;
            let trained = train_scalar_quantizer(core::slice::from_ref(&vector), 256, None)?;
            let reconstructed = trained.reconstruct(&trained.quantize(&vector)?)?;
            assert!(reconstructed.as_slice()[0].is_finite());
        }

        let extremes = DenseVector::new(vec![-f32::MAX, f32::MAX])?;
        let trained = train_scalar_quantizer(core::slice::from_ref(&extremes), 256, None)?;
        let reconstructed = trained.reconstruct(&trained.quantize(&extremes)?)?;
        assert!(
            reconstructed
                .as_slice()
                .iter()
                .all(|value| value.is_finite())
        );
        Ok(())
    }
}
