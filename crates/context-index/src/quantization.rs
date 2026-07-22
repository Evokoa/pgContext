//! Quantization primitives for index candidates.

use context_core::{BitVector, DenseVector, DistanceMetric, Error as CoreError, SearchLimit};

use crate::{HnswError, Result};

/// Uniform scalar quantization codebook for dense vectors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScalarQuantizer {
    min: f32,
    max: f32,
    levels: u16,
}

impl ScalarQuantizer {
    /// Creates a scalar quantization codebook.
    ///
    /// The codebook uses evenly spaced reconstruction levels from `min` to
    /// `max`, inclusive. It supports 2 through 256 levels so each code fits in
    /// one byte.
    ///
    /// # Errors
    ///
    /// Returns an error when bounds are non-finite, `min >= max`, or the level
    /// count cannot fit in a byte code.
    pub fn new(min: f32, max: f32, levels: u16) -> Result<Self> {
        if !min.is_finite() || !max.is_finite() {
            return Err(CoreError::InvalidVector(
                "invalid scalar quantization codebook: bounds must be finite".to_owned(),
            )
            .into());
        }

        if min >= max {
            return Err(CoreError::InvalidVector(
                "invalid scalar quantization codebook: min must be less than max".to_owned(),
            )
            .into());
        }

        if !(2..=256).contains(&levels) {
            return Err(CoreError::InvalidVector(format!(
                "invalid scalar quantization codebook: levels must be in 2..=256, got {levels}"
            ))
            .into());
        }

        Ok(Self { min, max, levels })
    }

    /// Returns the minimum represented value.
    #[must_use]
    pub const fn min(self) -> f32 {
        self.min
    }

    /// Returns the maximum represented value.
    #[must_use]
    pub const fn max(self) -> f32 {
        self.max
    }

    /// Returns the number of reconstruction levels.
    #[must_use]
    pub const fn levels(self) -> u16 {
        self.levels
    }

    /// Quantizes a dense vector to byte codes using nearest reconstruction
    /// levels.
    ///
    /// Values outside the codebook range are clamped to the nearest endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if a code cannot fit in one byte. This cannot happen
    /// for values produced by [`ScalarQuantizer::new`], but remains checked to
    /// keep the conversion boundary explicit.
    pub fn quantize(self, vector: &DenseVector) -> Result<ScalarQuantizedVector> {
        let mut codes = Vec::with_capacity(vector.dimension());
        for value in vector.as_slice() {
            codes.push(self.quantize_value(*value)?);
        }

        ScalarQuantizedVector::new(codes)
    }

    /// Reconstructs a dense vector from scalar byte codes.
    ///
    /// # Errors
    ///
    /// Returns an error when the code vector is empty or contains a code outside
    /// this quantizer's level range.
    pub fn reconstruct(self, vector: &ScalarQuantizedVector) -> Result<DenseVector> {
        let values = vector
            .codes()
            .iter()
            .copied()
            .map(|code| self.reconstruct_value(code))
            .collect::<Result<Vec<_>>>()?;

        Ok(DenseVector::new(values)?)
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamping and the validated 2..=256 level range prove the rounded code is in 0..=255"
    )]
    fn quantize_value(self, value: f32) -> Result<u8> {
        let clamped = value.clamp(self.min, self.max);
        let span = f64::from(self.max) - f64::from(self.min);
        let scale = f64::from(self.levels - 1) / span;
        let code = ((f64::from(clamped) - f64::from(self.min)) * scale).round();
        let code = u16::try_from(code as u32).map_err(|_| {
            CoreError::InvalidVector("scalar quantization code exceeds u16".to_owned())
        })?;
        u8::try_from(code).map_err(|_| {
            CoreError::InvalidVector(format!(
                "invalid scalar quantization codebook: code {code} does not fit in u8"
            ))
            .into()
        })
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "interpolation stays between validated finite f32 endpoints"
    )]
    fn reconstruct_value(self, code: u8) -> Result<f32> {
        if u16::from(code) >= self.levels {
            return Err(CoreError::InvalidVector(format!(
                "scalar quantized code {code} exceeds codebook levels {}",
                self.levels
            ))
            .into());
        }

        let steps = f64::from(self.levels - 1);
        let fraction = f64::from(code) / steps;
        let value = f64::from(self.min) + ((f64::from(self.max) - f64::from(self.min)) * fraction);
        Ok(value as f32)
    }
}

/// Scalar-quantized dense vector byte codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalarQuantizedVector {
    codes: Vec<u8>,
}

impl ScalarQuantizedVector {
    /// Creates scalar byte codes.
    ///
    /// # Errors
    ///
    /// Returns an error when no codes are supplied.
    pub fn new(codes: Vec<u8>) -> Result<Self> {
        if codes.is_empty() {
            return Err(CoreError::InvalidVector(
                "scalar quantized vectors must contain at least one code".to_owned(),
            )
            .into());
        }

        Ok(Self { codes })
    }

    /// Returns scalar byte codes.
    #[must_use]
    pub fn codes(&self) -> &[u8] {
        &self.codes
    }
}

/// One product-quantization subvector codebook.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductCodebook {
    centroids: Vec<DenseVector>,
}

impl ProductCodebook {
    /// Creates a product-quantization subvector codebook.
    ///
    /// # Errors
    ///
    /// Returns an error when the codebook is empty, contains more than 256
    /// centroids, or centroid dimensions are inconsistent.
    pub fn new(centroids: Vec<DenseVector>) -> Result<Self> {
        if centroids.is_empty() {
            return Err(CoreError::InvalidVector(
                "invalid product quantization codebook: centroids must not be empty".to_owned(),
            )
            .into());
        }

        if centroids.len() > 256 {
            return Err(CoreError::InvalidVector(format!(
                "invalid product quantization codebook: centroids must fit in u8 codes, got {}",
                centroids.len()
            ))
            .into());
        }

        let dimensions = centroids[0].dimension();
        if let Some(centroid) = centroids
            .iter()
            .find(|centroid| centroid.dimension() != dimensions)
        {
            return Err(HnswError::DimensionMismatch {
                left: dimensions,
                right: centroid.dimension(),
            });
        }

        Ok(Self { centroids })
    }

    /// Returns the centroids in code order.
    #[must_use]
    pub fn centroids(&self) -> &[DenseVector] {
        &self.centroids
    }
}

/// Product quantizer with one codebook per fixed-size subvector.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductQuantizer {
    subvector_dimensions: usize,
    codebooks: Vec<ProductCodebook>,
}

impl ProductQuantizer {
    /// Creates a product quantizer prototype.
    ///
    /// # Errors
    ///
    /// Returns an error when subvector dimensions are zero, no codebooks are
    /// supplied, or a codebook centroid dimension differs from the configured
    /// subvector dimensions.
    pub fn new(subvector_dimensions: usize, codebooks: Vec<ProductCodebook>) -> Result<Self> {
        if subvector_dimensions == 0 {
            return Err(CoreError::InvalidVector(
                "invalid product quantization codebook: subvector dimensions must be greater than zero"
                    .to_owned(),
            )
            .into());
        }

        if codebooks.is_empty() {
            return Err(CoreError::InvalidVector(
                "invalid product quantization codebook: codebooks must not be empty".to_owned(),
            )
            .into());
        }

        if let Some(codebook) = codebooks
            .iter()
            .find(|codebook| codebook.centroids[0].dimension() != subvector_dimensions)
        {
            return Err(HnswError::DimensionMismatch {
                left: subvector_dimensions,
                right: codebook.centroids[0].dimension(),
            });
        }

        Ok(Self {
            subvector_dimensions,
            codebooks,
        })
    }

    /// Returns the configured dimensions per subvector.
    #[must_use]
    pub const fn subvector_dimensions(&self) -> usize {
        self.subvector_dimensions
    }

    /// Returns the product codebooks.
    #[must_use]
    pub fn codebooks(&self) -> &[ProductCodebook] {
        &self.codebooks
    }

    /// Encodes a dense vector using nearest L2 centroid per subvector.
    ///
    /// # Errors
    ///
    /// Returns an error when the dense vector dimensions do not match the
    /// configured product codebook dimensions or when exact metric evaluation
    /// fails.
    pub fn quantize(&self, vector: &DenseVector) -> Result<ProductQuantizedVector> {
        let expected = self.expected_dimensions()?;
        if vector.dimension() != expected {
            return Err(HnswError::DimensionMismatch {
                left: expected,
                right: vector.dimension(),
            });
        }

        let mut codes = Vec::with_capacity(self.codebooks.len());
        for (codebook, subvector) in self
            .codebooks
            .iter()
            .zip(vector.as_slice().chunks_exact(self.subvector_dimensions))
        {
            let subvector = DenseVector::new(subvector.to_vec())?;
            codes.push(nearest_centroid_code(codebook, &subvector)?);
        }

        ProductQuantizedVector::new(codes)
    }

    /// Reconstructs a dense vector by concatenating coded centroids.
    ///
    /// # Errors
    ///
    /// Returns an error when code count differs from the number of codebooks or
    /// when any code is outside its codebook.
    pub fn reconstruct(&self, vector: &ProductQuantizedVector) -> Result<DenseVector> {
        if vector.codes().len() != self.codebooks.len() {
            return Err(HnswError::DimensionMismatch {
                left: self.codebooks.len(),
                right: vector.codes().len(),
            });
        }

        let mut values = Vec::with_capacity(self.expected_dimensions()?);
        for (codebook, code) in self.codebooks.iter().zip(vector.codes().iter().copied()) {
            let centroid = codebook.centroids.get(usize::from(code)).ok_or_else(|| {
                CoreError::InvalidVector(format!(
                    "product quantized code {code} exceeds codebook size {}",
                    codebook.centroids.len()
                ))
            })?;
            values.extend_from_slice(centroid.as_slice());
        }

        Ok(DenseVector::new(values)?)
    }

    fn expected_dimensions(&self) -> Result<usize> {
        self.subvector_dimensions
            .checked_mul(self.codebooks.len())
            .ok_or_else(|| {
                CoreError::InvalidVector(
                    "invalid product quantization codebook: dimensions overflow usize".to_owned(),
                )
                .into()
            })
    }
}

/// Product-quantized dense vector byte codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductQuantizedVector {
    codes: Vec<u8>,
}

impl ProductQuantizedVector {
    /// Creates product quantization byte codes.
    ///
    /// # Errors
    ///
    /// Returns an error when no codes are supplied.
    pub fn new(codes: Vec<u8>) -> Result<Self> {
        if codes.is_empty() {
            return Err(CoreError::InvalidVector(
                "product quantized vectors must contain at least one code".to_owned(),
            )
            .into());
        }

        Ok(Self { codes })
    }

    /// Returns product quantization byte codes.
    #[must_use]
    pub fn codes(&self) -> &[u8] {
        &self.codes
    }
}

fn nearest_centroid_code(codebook: &ProductCodebook, subvector: &DenseVector) -> Result<u8> {
    let mut best_code = 0u8;
    let mut best_score = f32::INFINITY;

    for (code, centroid) in codebook.centroids.iter().enumerate() {
        let score = DistanceMetric::L2.distance(subvector, centroid)?;
        if score < best_score {
            best_code = u8::try_from(code).map_err(|_| {
                CoreError::InvalidVector(format!(
                    "invalid product quantization codebook: code {code} does not fit in u8"
                ))
            })?;
            best_score = score;
        }
    }

    Ok(best_code)
}

/// Converts a dense vector to a binary sign code.
///
/// Values greater than or equal to zero become `1`; values below zero become
/// `0`. The source vector has already been validated by [`DenseVector`].
///
/// # Errors
///
/// Returns an error if the generated bit vector is invalid.
pub fn binary_quantize(vector: &DenseVector) -> Result<BitVector> {
    let bits = vector
        .as_slice()
        .iter()
        .map(|value| *value >= 0.0)
        .collect();

    Ok(BitVector::new(bits)?)
}

/// Candidate produced by an approximate or quantized search before exact rerank.
#[derive(Debug, Clone, PartialEq)]
pub struct RerankCandidate {
    point_id: u64,
    original_vector: Option<DenseVector>,
}

impl RerankCandidate {
    /// Creates a rerank candidate with its original vector available.
    #[must_use]
    pub const fn with_original(point_id: u64, original_vector: DenseVector) -> Self {
        Self {
            point_id,
            original_vector: Some(original_vector),
        }
    }

    /// Creates a rerank candidate that is missing original-vector data.
    #[must_use]
    pub const fn missing_original(point_id: u64) -> Self {
        Self {
            point_id,
            original_vector: None,
        }
    }

    /// Returns the stable point identifier.
    #[must_use]
    pub const fn point_id(&self) -> u64 {
        self.point_id
    }

    fn original_vector(&self) -> Result<&DenseVector> {
        self.original_vector.as_ref().ok_or_else(|| {
            CoreError::InvalidVector(format!(
                "missing original vector for rerank point {}",
                self.point_id
            ))
            .into()
        })
    }
}

/// Exact rerank result for one candidate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RerankResult {
    point_id: u64,
    score: f32,
}

impl RerankResult {
    /// Returns the stable point identifier.
    #[must_use]
    pub const fn point_id(self) -> u64 {
        self.point_id
    }

    /// Returns the exact metric score.
    #[must_use]
    pub const fn score(self) -> f32 {
        self.score
    }
}

/// Reranks candidates by exact distance to their original dense vectors.
///
/// Results are ordered by score and then point id for deterministic ties.
///
/// # Errors
///
/// Returns an error when a candidate is missing original-vector data or when
/// exact metric evaluation fails.
pub fn rerank_by_original_vectors(
    query: &DenseVector,
    candidates: &[RerankCandidate],
    metric: DistanceMetric,
    limit: SearchLimit,
) -> Result<Vec<RerankResult>> {
    let mut results = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let score = metric.distance(query, candidate.original_vector()?)?;
        results.push(RerankResult {
            point_id: candidate.point_id,
            score,
        });
    }

    results.sort_by(|left, right| {
        left.score
            .total_cmp(&right.score)
            .then_with(|| left.point_id.cmp(&right.point_id))
    });
    results.truncate(limit.get());

    Ok(results)
}
