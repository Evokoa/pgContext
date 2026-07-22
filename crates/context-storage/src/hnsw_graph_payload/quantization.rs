//! Portable quantization metadata and code validation for HNSW payload v2.

use context_core::DenseVector;

use super::{
    HnswGraphPayloadError, read_f32, read_u16, read_u32, size_of_f32, size_of_u32, usize_to_u32,
};

pub(super) const QUANTIZATION_NONE: u32 = 0;
const QUANTIZATION_BINARY: u32 = 1;
const QUANTIZATION_SCALAR: u32 = 2;
const QUANTIZATION_PRODUCT: u32 = 3;
const SCALAR_CODEBOOK_LEN: usize = 16;
const PRODUCT_CODEBOOK_HEADER_LEN: usize = 12;

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

pub(super) fn quantization_mode(codebook: &HnswGraphQuantizationCodebook) -> u32 {
    match codebook {
        HnswGraphQuantizationCodebook::Binary { .. } => QUANTIZATION_BINARY,
        HnswGraphQuantizationCodebook::Scalar { .. } => QUANTIZATION_SCALAR,
        HnswGraphQuantizationCodebook::Product { .. } => QUANTIZATION_PRODUCT,
    }
}

pub(super) fn validate_quantization(
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

fn validate_quantized_code(
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

pub(super) fn encode_quantization_codebook(
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

pub(super) fn decode_quantization_codebook(
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
    let mut offset = PRODUCT_CODEBOOK_HEADER_LEN;
    let mut codebooks = Vec::with_capacity(codebook_count);
    for index in 0..codebook_count {
        require_codebook_bytes(bytes, offset, size_of_u32(), index)?;
        let centroid_count = read_u32(bytes, offset) as usize;
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
