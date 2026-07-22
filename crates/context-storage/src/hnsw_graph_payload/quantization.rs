//! Portable quantization metadata and code validation for HNSW payload v2.

use context_core::{DenseVector, DistanceMetric};

use super::{
    HnswGraphPayloadError, read_f32, read_u16, read_u32, size_of_f32, size_of_u32, usize_to_u32,
};

pub(crate) const QUANTIZATION_NONE: u32 = 0;
const QUANTIZATION_BINARY: u32 = 1;
const QUANTIZATION_SCALAR: u32 = 2;
const QUANTIZATION_PRODUCT: u32 = 3;
const SCALAR_CODEBOOK_LEN: usize = 16;
const PRODUCT_CODEBOOK_HEADER_LEN: usize = 12;
const MAX_PRODUCT_CODEBOOKS: usize = 65_536;

/// Persisted quantization codebook for an HNSW graph payload.
#[derive(Debug, Clone, PartialEq)]
pub enum HnswGraphQuantizationCodebook {
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

impl HnswGraphQuantizationCodebook {
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
    /// Returns [`HnswGraphPayloadError`] when the code has the wrong length,
    /// invalid binary padding, or an index outside its scalar/product codebook.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "interpolation stays between validated finite f32 endpoints"
    )]
    pub fn reconstruct(&self, code: &[u8]) -> Result<DenseVector, HnswGraphPayloadError> {
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
        DenseVector::new(values)
            .map_err(|error| HnswGraphPayloadError::InvalidQuantization(error.to_string()))
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
    /// Returns [`HnswGraphPayloadError`] for malformed codes, dimension
    /// mismatches, unsupported raw-inner-product/binary metrics, or a zero
    /// query vector under cosine distance.
    pub fn approximate_distance(
        &self,
        query: &DenseVector,
        code: &[u8],
        metric: DistanceMetric,
    ) -> Result<f32, HnswGraphPayloadError> {
        validate_quantized_code(self, 0, code)?;
        if query.dimension() != self.dimensions() {
            return Err(HnswGraphPayloadError::InvalidQuantization(format!(
                "query dimensions mismatch: expected {}, got {}",
                self.dimensions(),
                query.dimension()
            )));
        }
        if matches!(metric, DistanceMetric::InnerProduct) {
            return Err(HnswGraphPayloadError::InvalidQuantization(
                "raw inner product is not an ascending HNSW distance".to_owned(),
            ));
        }
        if matches!(metric, DistanceMetric::Hamming | DistanceMetric::Jaccard) {
            return Err(HnswGraphPayloadError::InvalidQuantization(
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
            DistanceMetric::Cosine if query_norm == 0.0 => {
                Err(HnswGraphPayloadError::InvalidQuantization(
                    "cosine distance is undefined for a zero query vector".to_owned(),
                ))
            }
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
    /// Returns [`HnswGraphPayloadError`] for an incompatible query or metric.
    pub fn prepare_query(
        &self,
        query: &DenseVector,
        metric: DistanceMetric,
    ) -> Result<PreparedQuantizedQuery, HnswGraphPayloadError> {
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
    /// Returns [`HnswGraphPayloadError`] for a malformed or out-of-codebook
    /// code.
    pub fn score(&self, code: &[u8]) -> Result<f32, HnswGraphPayloadError> {
        let code_len = self.offsets.len().saturating_sub(1);
        if code.len() != code_len {
            return Err(HnswGraphPayloadError::InvalidQuantization(format!(
                "prepared query code length mismatch: expected {code_len}, got {}",
                code.len()
            )));
        }
        if let (Some(mask), Some(last)) = (self.binary_padding_mask, code.last())
            && last & !mask != 0
        {
            return Err(HnswGraphPayloadError::InvalidQuantization(
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
                return Err(HnswGraphPayloadError::InvalidQuantization(format!(
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
    codebook: &HnswGraphQuantizationCodebook,
    query: &DenseVector,
    metric: DistanceMetric,
) -> Result<(), HnswGraphPayloadError> {
    if query.dimension() != codebook.dimensions() {
        return Err(HnswGraphPayloadError::InvalidQuantization(format!(
            "query dimensions mismatch: expected {}, got {}",
            codebook.dimensions(),
            query.dimension()
        )));
    }
    if matches!(metric, DistanceMetric::InnerProduct) {
        return Err(HnswGraphPayloadError::InvalidQuantization(
            "raw inner product is not an ascending HNSW distance".to_owned(),
        ));
    }
    if matches!(metric, DistanceMetric::Hamming | DistanceMetric::Jaccard) {
        return Err(HnswGraphPayloadError::InvalidQuantization(
            "dense quantization does not support binary HNSW metrics".to_owned(),
        ));
    }
    if metric == DistanceMetric::Cosine && query.as_slice().iter().all(|value| *value == 0.0) {
        return Err(HnswGraphPayloadError::InvalidQuantization(
            "cosine distance is undefined for a zero query vector".to_owned(),
        ));
    }
    Ok(())
}

fn binary_padding_mask(codebook: &HnswGraphQuantizationCodebook) -> Option<u8> {
    let HnswGraphQuantizationCodebook::Binary { dimensions } = codebook else {
        return None;
    };
    let remainder = dimensions % 8;
    (remainder != 0).then(|| (1_u8 << remainder) - 1)
}

/// Quantized codes bound to one graph's record ordering.
#[derive(Debug, Clone, PartialEq)]
pub struct HnswGraphQuantization {
    codebook: HnswGraphQuantizationCodebook,
    codes: Vec<Vec<u8>>,
}

impl HnswGraphQuantization {
    /// Creates persisted quantization data.
    #[must_use]
    pub const fn new(codebook: HnswGraphQuantizationCodebook, codes: Vec<Vec<u8>>) -> Self {
        Self { codebook, codes }
    }

    /// Returns the persisted codebook.
    #[must_use]
    pub const fn codebook(&self) -> &HnswGraphQuantizationCodebook {
        &self.codebook
    }

    /// Returns node codes in graph-record order.
    #[must_use]
    pub fn codes(&self) -> &[Vec<u8>] {
        &self.codes
    }
}

pub(crate) fn quantization_mode(codebook: &HnswGraphQuantizationCodebook) -> u32 {
    match codebook {
        HnswGraphQuantizationCodebook::Binary { .. } => QUANTIZATION_BINARY,
        HnswGraphQuantizationCodebook::Scalar { .. } => QUANTIZATION_SCALAR,
        HnswGraphQuantizationCodebook::Product { .. } => QUANTIZATION_PRODUCT,
    }
}

pub(crate) fn validate_quantization(
    quantization: &HnswGraphQuantization,
    record_count: usize,
    dimensions: usize,
) -> Result<(), HnswGraphPayloadError> {
    validate_quantization_codebook(&quantization.codebook, dimensions)?;
    if quantization.codes.len() != record_count {
        return Err(HnswGraphPayloadError::InvalidQuantization(format!(
            "code count mismatch: expected {record_count}, got {}",
            quantization.codes.len()
        )));
    }
    for (node_index, code) in quantization.codes.iter().enumerate() {
        validate_quantized_code(&quantization.codebook, node_index, code)?;
    }
    Ok(())
}

fn validate_quantization_codebook(
    codebook: &HnswGraphQuantizationCodebook,
    dimensions: usize,
) -> Result<(), HnswGraphPayloadError> {
    if codebook.dimensions() != dimensions {
        return Err(HnswGraphPayloadError::InvalidQuantization(format!(
            "codebook dimensions mismatch: expected {dimensions}, got {}",
            codebook.dimensions()
        )));
    }
    match codebook {
        HnswGraphQuantizationCodebook::Binary { .. } => Ok(()),
        HnswGraphQuantizationCodebook::Scalar {
            minimum,
            maximum,
            levels,
            ..
        } => {
            if !minimum.is_finite() || !maximum.is_finite() || minimum >= maximum {
                return Err(HnswGraphPayloadError::InvalidQuantization(
                    "scalar bounds must be finite and increasing".to_owned(),
                ));
            }
            if !(2..=256).contains(levels) {
                return Err(HnswGraphPayloadError::InvalidQuantization(format!(
                    "scalar levels must be in 2..=256, got {levels}"
                )));
            }
            Ok(())
        }
        HnswGraphQuantizationCodebook::Product {
            subvector_dimensions,
            codebooks,
            ..
        } => {
            if *subvector_dimensions == 0 || !dimensions.is_multiple_of(*subvector_dimensions) {
                return Err(HnswGraphPayloadError::InvalidQuantization(format!(
                    "product subvector dimensions {subvector_dimensions} do not divide {dimensions}"
                )));
            }
            let expected_codebooks = dimensions / subvector_dimensions;
            if codebooks.len() != expected_codebooks {
                return Err(HnswGraphPayloadError::InvalidQuantization(format!(
                    "product codebook count mismatch: expected {expected_codebooks}, got {}",
                    codebooks.len()
                )));
            }
            for (index, centroids) in codebooks.iter().enumerate() {
                if centroids.is_empty() || centroids.len() > 256 {
                    return Err(HnswGraphPayloadError::InvalidQuantization(format!(
                        "product codebook {index} must contain 1..=256 centroids"
                    )));
                }
                if let Some(centroid) = centroids
                    .iter()
                    .find(|centroid| centroid.dimension() != *subvector_dimensions)
                {
                    return Err(HnswGraphPayloadError::InvalidQuantization(format!(
                        "product codebook {index} centroid dimensions mismatch: expected {subvector_dimensions}, got {}",
                        centroid.dimension()
                    )));
                }
            }
            Ok(())
        }
    }
}

pub(crate) fn validate_quantized_code(
    codebook: &HnswGraphQuantizationCodebook,
    node_index: usize,
    code: &[u8],
) -> Result<(), HnswGraphPayloadError> {
    let expected = codebook.code_len();
    if code.len() != expected {
        return Err(HnswGraphPayloadError::InvalidQuantization(format!(
            "node {node_index} code length mismatch: expected {expected}, got {}",
            code.len()
        )));
    }
    match codebook {
        HnswGraphQuantizationCodebook::Binary { dimensions } => {
            let remainder = dimensions % 8;
            if remainder != 0 {
                let mask = (1_u8 << remainder) - 1;
                if code.last().is_some_and(|byte| byte & !mask != 0) {
                    return Err(HnswGraphPayloadError::InvalidQuantization(format!(
                        "node {node_index} binary code has non-zero padding bits"
                    )));
                }
            }
        }
        HnswGraphQuantizationCodebook::Scalar { levels, .. } => {
            if let Some(invalid) = code.iter().find(|value| u16::from(**value) >= *levels) {
                return Err(HnswGraphPayloadError::InvalidQuantization(format!(
                    "node {node_index} scalar code {invalid} exceeds {levels} levels"
                )));
            }
        }
        HnswGraphQuantizationCodebook::Product { codebooks, .. } => {
            for (subvector, (value, centroids)) in code.iter().zip(codebooks).enumerate() {
                if usize::from(*value) >= centroids.len() {
                    return Err(HnswGraphPayloadError::InvalidQuantization(format!(
                        "node {node_index} product code {value} exceeds codebook {subvector} size {}",
                        centroids.len()
                    )));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn encode_quantization_codebook(
    codebook: &HnswGraphQuantizationCodebook,
) -> Result<Vec<u8>, HnswGraphPayloadError> {
    let mut output = Vec::new();
    match codebook {
        HnswGraphQuantizationCodebook::Binary { dimensions } => {
            output.extend_from_slice(&usize_to_u32(*dimensions, 0)?.to_le_bytes());
        }
        HnswGraphQuantizationCodebook::Scalar {
            dimensions,
            minimum,
            maximum,
            levels,
        } => {
            output.extend_from_slice(&usize_to_u32(*dimensions, 0)?.to_le_bytes());
            output.extend_from_slice(&minimum.to_le_bytes());
            output.extend_from_slice(&maximum.to_le_bytes());
            output.extend_from_slice(&levels.to_le_bytes());
            output.extend_from_slice(&0_u16.to_le_bytes());
        }
        HnswGraphQuantizationCodebook::Product {
            dimensions,
            subvector_dimensions,
            codebooks,
        } => {
            output.extend_from_slice(&usize_to_u32(*dimensions, 0)?.to_le_bytes());
            output.extend_from_slice(&usize_to_u32(*subvector_dimensions, 0)?.to_le_bytes());
            output.extend_from_slice(&usize_to_u32(codebooks.len(), 0)?.to_le_bytes());
            for (index, centroids) in codebooks.iter().enumerate() {
                output.extend_from_slice(&usize_to_u32(centroids.len(), index)?.to_le_bytes());
                for centroid in centroids {
                    for value in centroid.as_slice() {
                        output.extend_from_slice(&value.to_le_bytes());
                    }
                }
            }
        }
    }
    Ok(output)
}

pub(crate) fn decode_quantization_codebook(
    mode: u32,
    dimensions: usize,
    code_len: usize,
    bytes: &[u8],
) -> Result<Option<HnswGraphQuantizationCodebook>, HnswGraphPayloadError> {
    let codebook = match mode {
        QUANTIZATION_NONE => {
            if code_len != 0 || !bytes.is_empty() {
                return Err(HnswGraphPayloadError::InvalidQuantization(
                    "unquantized payload declares code or codebook bytes".to_owned(),
                ));
            }
            return Ok(None);
        }
        QUANTIZATION_BINARY => decode_binary_codebook(bytes)?,
        QUANTIZATION_SCALAR => decode_scalar_codebook(bytes)?,
        QUANTIZATION_PRODUCT => decode_product_codebook(bytes)?,
        _ => {
            return Err(HnswGraphPayloadError::InvalidQuantization(format!(
                "unknown quantization mode {mode}"
            )));
        }
    };
    validate_quantization_codebook(&codebook, dimensions)?;
    if codebook.code_len() != code_len {
        return Err(HnswGraphPayloadError::InvalidQuantization(format!(
            "declared code length {code_len} does not match codebook length {}",
            codebook.code_len()
        )));
    }
    Ok(Some(codebook))
}

fn decode_binary_codebook(
    bytes: &[u8],
) -> Result<HnswGraphQuantizationCodebook, HnswGraphPayloadError> {
    if bytes.len() != size_of_u32() {
        return Err(HnswGraphPayloadError::InvalidQuantization(format!(
            "binary codebook length must be 4, got {}",
            bytes.len()
        )));
    }
    Ok(HnswGraphQuantizationCodebook::Binary {
        dimensions: read_u32(bytes, 0) as usize,
    })
}

fn decode_scalar_codebook(
    bytes: &[u8],
) -> Result<HnswGraphQuantizationCodebook, HnswGraphPayloadError> {
    if bytes.len() != SCALAR_CODEBOOK_LEN {
        return Err(HnswGraphPayloadError::InvalidQuantization(format!(
            "scalar codebook length must be {SCALAR_CODEBOOK_LEN}, got {}",
            bytes.len()
        )));
    }
    let reserved = read_u16(bytes, 14);
    if reserved != 0 {
        return Err(HnswGraphPayloadError::InvalidQuantization(format!(
            "scalar codebook reserved field is non-zero: {reserved}"
        )));
    }
    Ok(HnswGraphQuantizationCodebook::Scalar {
        dimensions: read_u32(bytes, 0) as usize,
        minimum: read_f32(bytes, 4),
        maximum: read_f32(bytes, 8),
        levels: read_u16(bytes, 12),
    })
}

fn decode_product_codebook(
    bytes: &[u8],
) -> Result<HnswGraphQuantizationCodebook, HnswGraphPayloadError> {
    if bytes.len() < PRODUCT_CODEBOOK_HEADER_LEN {
        return Err(HnswGraphPayloadError::InvalidQuantization(format!(
            "truncated product codebook header: {} < {PRODUCT_CODEBOOK_HEADER_LEN}",
            bytes.len()
        )));
    }
    let dimensions = read_u32(bytes, 0) as usize;
    let subvector_dimensions = read_u32(bytes, 4) as usize;
    let codebook_count = read_u32(bytes, 8) as usize;
    if codebook_count == 0 || codebook_count > MAX_PRODUCT_CODEBOOKS {
        return Err(HnswGraphPayloadError::InvalidQuantization(format!(
            "product codebook count must be in 1..={MAX_PRODUCT_CODEBOOKS}, got {codebook_count}"
        )));
    }
    let minimum_bytes = PRODUCT_CODEBOOK_HEADER_LEN
        .checked_add(codebook_count.checked_mul(size_of_u32()).ok_or_else(|| {
            HnswGraphPayloadError::InvalidQuantization(
                "product codebook count overflows usize".to_owned(),
            )
        })?)
        .ok_or_else(|| {
            HnswGraphPayloadError::InvalidQuantization(
                "product codebook minimum length overflows usize".to_owned(),
            )
        })?;
    if bytes.len() < minimum_bytes {
        return Err(HnswGraphPayloadError::InvalidQuantization(format!(
            "truncated product codebook headers: expected at least {minimum_bytes}, got {}",
            bytes.len()
        )));
    }
    let mut offset = PRODUCT_CODEBOOK_HEADER_LEN;
    let mut codebooks = Vec::with_capacity(codebook_count);
    for index in 0..codebook_count {
        require_codebook_bytes(bytes, offset, size_of_u32(), index)?;
        let centroid_count = read_u32(bytes, offset) as usize;
        if !(1..=256).contains(&centroid_count) {
            return Err(HnswGraphPayloadError::InvalidQuantization(format!(
                "product codebook {index} must contain 1..=256 centroids, got {centroid_count}"
            )));
        }
        offset += size_of_u32();
        let centroid_bytes = subvector_dimensions
            .checked_mul(size_of_f32())
            .and_then(|value| value.checked_mul(centroid_count))
            .ok_or_else(|| {
                HnswGraphPayloadError::InvalidQuantization(format!(
                    "product codebook {index} byte length overflows usize"
                ))
            })?;
        require_codebook_bytes(bytes, offset, centroid_bytes, index)?;
        let mut centroids = Vec::with_capacity(centroid_count);
        for centroid_index in 0..centroid_count {
            let centroid_offset = offset + centroid_index * subvector_dimensions * size_of_f32();
            let values = (0..subvector_dimensions)
                .map(|dimension| read_f32(bytes, centroid_offset + dimension * size_of_f32()))
                .collect::<Vec<_>>();
            centroids.push(DenseVector::new(values).map_err(|error| {
                HnswGraphPayloadError::InvalidQuantization(format!(
                    "product codebook {index} centroid {centroid_index}: {error}"
                ))
            })?);
        }
        offset += centroid_bytes;
        codebooks.push(centroids);
    }
    if offset != bytes.len() {
        return Err(HnswGraphPayloadError::InvalidQuantization(format!(
            "product codebook has {} trailing bytes",
            bytes.len() - offset
        )));
    }
    Ok(HnswGraphQuantizationCodebook::Product {
        dimensions,
        subvector_dimensions,
        codebooks,
    })
}

fn require_codebook_bytes(
    bytes: &[u8],
    offset: usize,
    length: usize,
    codebook_index: usize,
) -> Result<(), HnswGraphPayloadError> {
    let end = offset.checked_add(length).ok_or_else(|| {
        HnswGraphPayloadError::InvalidQuantization(format!(
            "product codebook {codebook_index} byte length overflows usize"
        ))
    })?;
    if bytes.len() < end {
        return Err(HnswGraphPayloadError::InvalidQuantization(format!(
            "truncated product codebook {codebook_index}: expected through byte {end}, got {}",
            bytes.len()
        )));
    }
    Ok(())
}
