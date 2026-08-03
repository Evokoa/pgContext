//! Runtime trained quantization state and encoded query scoring.

use context_core::{DenseVector, DistanceMetric};

use crate::{CodecError, Result};

/// Trained quantization codebook independent of any storage layout.
#[derive(Debug, Clone, PartialEq)]
pub enum QuantizedCodebook {
    /// Sign-bit encoding with no trained values.
    Binary {
        /// Original dense-vector dimensions.
        dimensions: usize,
    },
    /// Uniform scalar byte encoding.
    Scalar {
        /// Original dense-vector dimensions.
        dimensions: usize,
        /// Minimum reconstruction value.
        minimum: f32,
        /// Maximum reconstruction value.
        maximum: f32,
        /// Number of reconstruction levels.
        levels: u16,
    },
    /// Product quantization with one centroid table per subvector.
    Product {
        /// Original dense-vector dimensions.
        dimensions: usize,
        /// Dimensions represented by each code byte.
        subvector_dimensions: usize,
        /// Centroid tables in subvector order.
        codebooks: Vec<Vec<DenseVector>>,
    },
}

impl QuantizedCodebook {
    /// Returns the original dense-vector dimensions.
    #[must_use]
    pub const fn dimensions(&self) -> usize {
        match self {
            Self::Binary { dimensions }
            | Self::Scalar { dimensions, .. }
            | Self::Product { dimensions, .. } => *dimensions,
        }
    }

    /// Returns the fixed number of encoded bytes per graph node.
    #[must_use]
    pub fn code_len(&self) -> usize {
        match self {
            Self::Binary { dimensions } => dimensions.div_ceil(8),
            Self::Scalar { dimensions, .. } => *dimensions,
            Self::Product { codebooks, .. } => codebooks.len(),
        }
    }

    /// Reconstructs the approximate navigation vector for one persisted code.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`] when the code has the wrong length,
    /// invalid binary padding, or an index outside its scalar/product codebook.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "interpolation stays between validated finite f32 endpoints"
    )]
    pub fn reconstruct(&self, code: &[u8]) -> Result<DenseVector> {
        validate_quantized_code(self, 0, code)?;
        let values = match self {
            Self::Binary { dimensions } => (0..*dimensions)
                .map(|index| {
                    if code[index / 8] & (1 << (index % 8)) == 0 {
                        -1.0
                    } else {
                        1.0
                    }
                })
                .collect(),
            Self::Scalar {
                minimum,
                maximum,
                levels,
                ..
            } => {
                let steps = f64::from(*levels - 1);
                code.iter()
                    .map(|value| {
                        let fraction = f64::from(*value) / steps;
                        let reconstructed = f64::from(*minimum)
                            + ((f64::from(*maximum) - f64::from(*minimum)) * fraction);
                        reconstructed as f32
                    })
                    .collect()
            }
            Self::Product { codebooks, .. } => {
                let mut values = Vec::with_capacity(self.dimensions());
                for (value, centroids) in code.iter().zip(codebooks) {
                    values.extend_from_slice(centroids[usize::from(*value)].as_slice());
                }
                values
            }
        };
        DenseVector::new(values).map_err(|error| CodecError::InvalidCode(error.to_string()))
    }

    /// Scores one encoded node against a full-precision query without
    /// reconstructing or allocating a dense vector.
    ///
    /// Cosine navigation assigns positive infinity to an encoded zero vector.
    /// This keeps an approximation artifact from aborting traversal while the
    /// authoritative source-row recheck remains the final oracle.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`] for malformed codes, dimension
    /// mismatches, unsupported raw-inner-product/binary metrics, or a zero
    /// query vector under cosine distance.
    pub fn approximate_distance(
        &self,
        query: &DenseVector,
        code: &[u8],
        metric: DistanceMetric,
    ) -> Result<f32> {
        validate_quantized_code(self, 0, code)?;
        if query.dimension() != self.dimensions() {
            return Err(CodecError::InvalidCode(format!(
                "query dimensions mismatch: expected {}, got {}",
                self.dimensions(),
                query.dimension()
            )));
        }
        if matches!(metric, DistanceMetric::InnerProduct) {
            return Err(CodecError::InvalidCode(
                "raw inner product is not an ascending HNSW distance".to_owned(),
            ));
        }
        if matches!(metric, DistanceMetric::Hamming | DistanceMetric::Jaccard) {
            return Err(CodecError::InvalidCode(
                "dense quantization does not support binary HNSW metrics".to_owned(),
            ));
        }

        let mut squared_l2 = 0.0_f32;
        let mut l1 = 0.0_f32;
        let mut dot = 0.0_f32;
        let mut encoded_norm = 0.0_f32;
        let mut query_norm = 0.0_f32;
        self.for_each_reconstructed(code, |index, encoded| {
            let query_value = query.as_slice()[index];
            let difference = query_value - encoded;
            squared_l2 += difference * difference;
            l1 += difference.abs();
            dot += query_value * encoded;
            encoded_norm += encoded * encoded;
            query_norm += query_value * query_value;
        });
        match metric {
            DistanceMetric::L2 => Ok(squared_l2.sqrt()),
            DistanceMetric::L1 => Ok(l1),
            DistanceMetric::NegativeInnerProduct => Ok(-dot),
            DistanceMetric::Cosine if query_norm == 0.0 => Err(CodecError::InvalidCode(
                "cosine distance is undefined for a zero query vector".to_owned(),
            )),
            DistanceMetric::Cosine if encoded_norm == 0.0 => Ok(f32::INFINITY),
            DistanceMetric::Cosine => Ok(1.0 - dot / (query_norm.sqrt() * encoded_norm.sqrt())),
            DistanceMetric::InnerProduct | DistanceMetric::Hamming | DistanceMetric::Jaccard => {
                unreachable!("unsupported metrics return before encoded scoring")
            }
        }
    }

    /// Precomputes query-to-code contributions for repeated encoded scoring.
    ///
    /// The resulting scorer performs work proportional to encoded byte length,
    /// rather than original vector dimensions, for every visited graph node.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`] for an incompatible query or metric.
    pub fn prepare_query(
        &self,
        query: &DenseVector,
        metric: DistanceMetric,
    ) -> Result<PreparedQuantizedQuery> {
        validate_quantization_codebook(self, self.dimensions())?;
        validate_query_contract(self, query, metric)?;
        let mut offsets = Vec::with_capacity(self.code_len() + 1);
        let mut contributions = Vec::new();
        offsets.push(0);
        match self {
            Self::Binary { dimensions } => {
                for byte_index in 0..dimensions.div_ceil(8) {
                    for encoded_byte in u8::MIN..=u8::MAX {
                        let mut contribution = DistanceContribution::default();
                        for bit in 0..8 {
                            let dimension = byte_index * 8 + bit;
                            if dimension == *dimensions {
                                break;
                            }
                            let encoded = if encoded_byte & (1 << bit) == 0 {
                                -1.0
                            } else {
                                1.0
                            };
                            contribution.add(metric, query.as_slice()[dimension], encoded);
                        }
                        contributions.push(contribution);
                    }
                    offsets.push(contributions.len());
                }
            }
            Self::Scalar {
                minimum,
                maximum,
                levels,
                ..
            } => {
                let steps = f64::from(*levels - 1);
                for query_value in query.as_slice() {
                    for encoded in 0..*levels {
                        let fraction = f64::from(encoded) / steps;
                        let reconstructed = f64::from(*minimum)
                            + ((f64::from(*maximum) - f64::from(*minimum)) * fraction);
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "interpolation stays between validated finite f32 endpoints"
                        )]
                        let reconstructed = reconstructed as f32;
                        let mut contribution = DistanceContribution::default();
                        contribution.add(metric, *query_value, reconstructed);
                        contributions.push(contribution);
                    }
                    offsets.push(contributions.len());
                }
            }
            Self::Product { codebooks, .. } => {
                let mut query_offset = 0;
                for centroids in codebooks {
                    for centroid in centroids {
                        let mut contribution = DistanceContribution::default();
                        for (within_subvector, reconstructed) in
                            centroid.as_slice().iter().copied().enumerate()
                        {
                            contribution.add(
                                metric,
                                query.as_slice()[query_offset + within_subvector],
                                reconstructed,
                            );
                        }
                        contributions.push(contribution);
                    }
                    query_offset += centroids[0].dimension();
                    offsets.push(contributions.len());
                }
            }
        }
        Ok(PreparedQuantizedQuery {
            metric,
            query_norm: query.as_slice().iter().map(|value| value * value).sum(),
            offsets,
            contributions,
            binary_padding_mask: binary_padding_mask(self),
        })
    }

    fn for_each_reconstructed(&self, code: &[u8], mut visit: impl FnMut(usize, f32)) {
        match self {
            Self::Binary { dimensions } => {
                for index in 0..*dimensions {
                    let value = if code[index / 8] & (1 << (index % 8)) == 0 {
                        -1.0
                    } else {
                        1.0
                    };
                    visit(index, value);
                }
            }
            Self::Scalar {
                minimum,
                maximum,
                levels,
                ..
            } => {
                let steps = f64::from(*levels - 1);
                for (index, value) in code.iter().copied().enumerate() {
                    let fraction = f64::from(value) / steps;
                    let reconstructed = f64::from(*minimum)
                        + ((f64::from(*maximum) - f64::from(*minimum)) * fraction);
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "interpolation stays between validated finite f32 endpoints"
                    )]
                    visit(index, reconstructed as f32);
                }
            }
            Self::Product {
                subvector_dimensions,
                codebooks,
                ..
            } => {
                for (subvector, (value, centroids)) in code.iter().zip(codebooks).enumerate() {
                    let centroid = &centroids[usize::from(*value)];
                    for (within_subvector, reconstructed) in
                        centroid.as_slice().iter().copied().enumerate()
                    {
                        visit(
                            subvector * subvector_dimensions + within_subvector,
                            reconstructed,
                        );
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct DistanceContribution {
    primary: f32,
    encoded_norm: f32,
}

impl DistanceContribution {
    fn add(&mut self, metric: DistanceMetric, query: f32, encoded: f32) {
        self.primary += match metric {
            DistanceMetric::L2 => {
                let difference = query - encoded;
                difference * difference
            }
            DistanceMetric::L1 => (query - encoded).abs(),
            DistanceMetric::NegativeInnerProduct => -(query * encoded),
            DistanceMetric::Cosine => query * encoded,
            DistanceMetric::InnerProduct | DistanceMetric::Hamming | DistanceMetric::Jaccard => {
                unreachable!("prepared query contract rejects unsupported metrics")
            }
        };
        if metric == DistanceMetric::Cosine {
            self.encoded_norm += encoded * encoded;
        }
    }
}

/// Query-scoped lookup scorer for compact quantized node codes.
#[derive(Debug, Clone)]
pub struct PreparedQuantizedQuery {
    metric: DistanceMetric,
    query_norm: f32,
    offsets: Vec<usize>,
    contributions: Vec<DistanceContribution>,
    binary_padding_mask: Option<u8>,
}

impl PreparedQuantizedQuery {
    /// Scores one encoded node in work proportional to code bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`] for a malformed or out-of-codebook
    /// code.
    pub fn score(&self, code: &[u8]) -> Result<f32> {
        let code_len = self.offsets.len().saturating_sub(1);
        if code.len() != code_len {
            return Err(CodecError::InvalidCode(format!(
                "prepared query code length mismatch: expected {code_len}, got {}",
                code.len()
            )));
        }
        if let (Some(mask), Some(last)) = (self.binary_padding_mask, code.last())
            && last & !mask != 0
        {
            return Err(CodecError::InvalidCode(
                "prepared query binary code has non-zero padding bits".to_owned(),
            ));
        }
        let mut primary = 0.0_f32;
        let mut encoded_norm = 0.0_f32;
        for (position, encoded) in code.iter().copied().enumerate() {
            let start = self.offsets[position];
            let end = self.offsets[position + 1];
            let index = start.saturating_add(usize::from(encoded));
            let Some(contribution) = self.contributions.get(index).filter(|_| index < end) else {
                return Err(CodecError::InvalidCode(format!(
                    "prepared query code {encoded} exceeds position {position} table size {}",
                    end - start
                )));
            };
            primary += contribution.primary;
            encoded_norm += contribution.encoded_norm;
        }
        match self.metric {
            DistanceMetric::L2 => Ok(primary.sqrt()),
            DistanceMetric::L1 | DistanceMetric::NegativeInnerProduct => Ok(primary),
            DistanceMetric::Cosine if encoded_norm == 0.0 => Ok(f32::INFINITY),
            DistanceMetric::Cosine => {
                Ok(1.0 - primary / (self.query_norm.sqrt() * encoded_norm.sqrt()))
            }
            DistanceMetric::InnerProduct | DistanceMetric::Hamming | DistanceMetric::Jaccard => {
                unreachable!("prepared query contract rejects unsupported metrics")
            }
        }
    }
}

fn validate_query_contract(
    codebook: &QuantizedCodebook,
    query: &DenseVector,
    metric: DistanceMetric,
) -> Result<()> {
    if query.dimension() != codebook.dimensions() {
        return Err(CodecError::InvalidCode(format!(
            "query dimensions mismatch: expected {}, got {}",
            codebook.dimensions(),
            query.dimension()
        )));
    }
    if matches!(metric, DistanceMetric::InnerProduct) {
        return Err(CodecError::InvalidCode(
            "raw inner product is not an ascending HNSW distance".to_owned(),
        ));
    }
    if matches!(metric, DistanceMetric::Hamming | DistanceMetric::Jaccard) {
        return Err(CodecError::InvalidCode(
            "dense quantization does not support binary HNSW metrics".to_owned(),
        ));
    }
    if metric == DistanceMetric::Cosine && query.as_slice().iter().all(|value| *value == 0.0) {
        return Err(CodecError::InvalidCode(
            "cosine distance is undefined for a zero query vector".to_owned(),
        ));
    }
    Ok(())
}

fn binary_padding_mask(codebook: &QuantizedCodebook) -> Option<u8> {
    let QuantizedCodebook::Binary { dimensions } = codebook else {
        return None;
    };
    let remainder = dimensions % 8;
    (remainder != 0).then(|| (1_u8 << remainder) - 1)
}

/// Validates trained codec state against an expected vector width.
///
/// # Errors
///
/// Returns [`CodecError`] when the codebook is malformed or dimensionally incompatible.
pub fn validate_quantization_codebook(
    codebook: &QuantizedCodebook,
    dimensions: usize,
) -> Result<()> {
    if dimensions == 0 {
        return Err(CodecError::InvalidCode(
            "codebook dimensions must be non-zero".to_owned(),
        ));
    }
    if codebook.dimensions() != dimensions {
        return Err(CodecError::InvalidCode(format!(
            "codebook dimensions mismatch: expected {dimensions}, got {}",
            codebook.dimensions()
        )));
    }
    match codebook {
        QuantizedCodebook::Binary { .. } => Ok(()),
        QuantizedCodebook::Scalar {
            minimum,
            maximum,
            levels,
            ..
        } => {
            if !minimum.is_finite() || !maximum.is_finite() || minimum >= maximum {
                return Err(CodecError::InvalidCode(
                    "scalar bounds must be finite and increasing".to_owned(),
                ));
            }
            if !(2..=256).contains(levels) {
                return Err(CodecError::InvalidCode(format!(
                    "scalar levels must be in 2..=256, got {levels}"
                )));
            }
            Ok(())
        }
        QuantizedCodebook::Product {
            subvector_dimensions,
            codebooks,
            ..
        } => {
            if *subvector_dimensions == 0 || !dimensions.is_multiple_of(*subvector_dimensions) {
                return Err(CodecError::InvalidCode(format!(
                    "product subvector dimensions {subvector_dimensions} do not divide {dimensions}"
                )));
            }
            let expected_codebooks = dimensions / subvector_dimensions;
            if codebooks.len() != expected_codebooks {
                return Err(CodecError::InvalidCode(format!(
                    "product codebook count mismatch: expected {expected_codebooks}, got {}",
                    codebooks.len()
                )));
            }
            for (index, centroids) in codebooks.iter().enumerate() {
                if centroids.is_empty() || centroids.len() > 256 {
                    return Err(CodecError::InvalidCode(format!(
                        "product codebook {index} must contain 1..=256 centroids"
                    )));
                }
                if let Some(centroid) = centroids
                    .iter()
                    .find(|centroid| centroid.dimension() != *subvector_dimensions)
                {
                    return Err(CodecError::InvalidCode(format!(
                        "product codebook {index} centroid dimensions mismatch: expected {subvector_dimensions}, got {}",
                        centroid.dimension()
                    )));
                }
            }
            Ok(())
        }
    }
}

/// Validates one encoded vector against its trained codebook.
///
/// # Errors
///
/// Returns [`CodecError`] when the code length, padding, or code values are invalid.
pub fn validate_quantized_code(
    codebook: &QuantizedCodebook,
    node_index: usize,
    code: &[u8],
) -> Result<()> {
    validate_quantization_codebook(codebook, codebook.dimensions())?;
    let expected = codebook.code_len();
    if code.len() != expected {
        return Err(CodecError::InvalidCode(format!(
            "node {node_index} code length mismatch: expected {expected}, got {}",
            code.len()
        )));
    }
    match codebook {
        QuantizedCodebook::Binary { dimensions } => {
            let remainder = dimensions % 8;
            if remainder != 0 {
                let mask = (1_u8 << remainder) - 1;
                if code.last().is_some_and(|byte| byte & !mask != 0) {
                    return Err(CodecError::InvalidCode(format!(
                        "node {node_index} binary code has non-zero padding bits"
                    )));
                }
            }
        }
        QuantizedCodebook::Scalar { levels, .. } => {
            if let Some(invalid) = code.iter().find(|value| u16::from(**value) >= *levels) {
                return Err(CodecError::InvalidCode(format!(
                    "node {node_index} scalar code {invalid} exceeds {levels} levels"
                )));
            }
        }
        QuantizedCodebook::Product { codebooks, .. } => {
            for (subvector, (value, centroids)) in code.iter().zip(codebooks).enumerate() {
                if usize::from(*value) >= centroids.len() {
                    return Err(CodecError::InvalidCode(format!(
                        "node {node_index} product code {value} exceeds codebook {subvector} size {}",
                        centroids.len()
                    )));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dense(values: &[f32]) -> Result<DenseVector> {
        DenseVector::new(values.to_vec())
            .map_err(|error| CodecError::InvalidCode(error.to_string()))
    }

    #[test]
    fn prepared_scalar_score_matches_direct_encoded_score() -> Result<()> {
        let codebook = QuantizedCodebook::Scalar {
            dimensions: 3,
            minimum: -1.0,
            maximum: 1.0,
            levels: 256,
        };
        let query = dense(&[0.25, -0.5, 0.75])?;
        let code = [159, 64, 223];

        let direct = codebook.approximate_distance(&query, &code, DistanceMetric::L2)?;
        let prepared = codebook
            .prepare_query(&query, DistanceMetric::L2)?
            .score(&code)?;

        assert!((direct - prepared).abs() <= f32::EPSILON * 8.0);
        Ok(())
    }

    #[test]
    fn binary_padding_is_rejected_by_direct_and_prepared_scoring() -> Result<()> {
        let codebook = QuantizedCodebook::Binary { dimensions: 9 };
        let query = dense(&[1.0; 9])?;
        let invalid_code = [0_u8, 0b0000_0010];

        assert!(
            codebook
                .approximate_distance(&query, &invalid_code, DistanceMetric::L1)
                .is_err()
        );
        assert!(
            codebook
                .prepare_query(&query, DistanceMetric::L1)?
                .score(&invalid_code)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn product_reconstruction_rejects_out_of_codebook_values() -> Result<()> {
        let codebook = QuantizedCodebook::Product {
            dimensions: 2,
            subvector_dimensions: 1,
            codebooks: vec![vec![dense(&[0.0])?], vec![dense(&[1.0])?]],
        };

        assert!(codebook.reconstruct(&[0, 1]).is_err());
        assert_eq!(codebook.reconstruct(&[0, 0])?.as_slice(), &[0.0, 1.0]);
        Ok(())
    }

    #[test]
    fn malformed_public_codebooks_fail_before_scoring_or_allocation() -> Result<()> {
        let query = dense(&[0.0, 1.0])?;
        let malformed_product = QuantizedCodebook::Product {
            dimensions: 2,
            subvector_dimensions: 1,
            codebooks: vec![Vec::new(), vec![dense(&[1.0])?]],
        };
        assert!(
            malformed_product
                .prepare_query(&query, DistanceMetric::L2)
                .is_err()
        );
        assert!(
            malformed_product
                .approximate_distance(&query, &[0, 0], DistanceMetric::L2)
                .is_err()
        );
        assert!(malformed_product.reconstruct(&[0, 0]).is_err());

        let malformed_scalar = QuantizedCodebook::Scalar {
            dimensions: 2,
            minimum: 0.0,
            maximum: 1.0,
            levels: u16::MAX,
        };
        assert!(
            malformed_scalar
                .prepare_query(&query, DistanceMetric::L2)
                .is_err()
        );
        Ok(())
    }
}
