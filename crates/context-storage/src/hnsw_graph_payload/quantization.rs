//! Portable quantization metadata and code validation for HNSW payload v2.

use context_codec::{
    QuantizedCodebook, validate_quantization_codebook as validate_codec_codebook,
    validate_quantized_code as validate_codec_code,
};
use context_core::DenseVector;

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

/// Persisted code bytes bound to one graph record ordering.
#[derive(Debug, Clone, PartialEq)]
pub struct HnswGraphQuantization {
    codebook: QuantizedCodebook,
    codes: Vec<Vec<u8>>,
}

impl HnswGraphQuantization {
    /// Creates persisted quantization data.
    #[must_use]
    pub const fn new(codebook: QuantizedCodebook, codes: Vec<Vec<u8>>) -> Self {
        Self { codebook, codes }
    }

    /// Returns the persisted codebook.
    #[must_use]
    pub const fn codebook(&self) -> &QuantizedCodebook {
        &self.codebook
    }

    /// Returns node codes in graph-record order.
    #[must_use]
    pub fn codes(&self) -> &[Vec<u8>] {
        &self.codes
    }
}

pub(crate) fn quantization_mode(codebook: &QuantizedCodebook) -> u32 {
    match codebook {
        QuantizedCodebook::Binary { .. } => QUANTIZATION_BINARY,
        QuantizedCodebook::Scalar { .. } => QUANTIZATION_SCALAR,
        QuantizedCodebook::Product { .. } => QUANTIZATION_PRODUCT,
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
    codebook: &QuantizedCodebook,
    dimensions: usize,
) -> Result<(), HnswGraphPayloadError> {
    validate_codec_codebook(codebook, dimensions)
        .map_err(|error| HnswGraphPayloadError::InvalidQuantization(error.to_string()))
}

pub(crate) fn validate_quantized_code(
    codebook: &QuantizedCodebook,
    node_index: usize,
    code: &[u8],
) -> Result<(), HnswGraphPayloadError> {
    validate_codec_code(codebook, node_index, code)
        .map_err(|error| HnswGraphPayloadError::InvalidQuantization(error.to_string()))
}

pub(crate) fn encode_quantization_codebook(
    codebook: &QuantizedCodebook,
) -> Result<Vec<u8>, HnswGraphPayloadError> {
    let mut output = Vec::new();
    match codebook {
        QuantizedCodebook::Binary { dimensions } => {
            output.extend_from_slice(&usize_to_u32(*dimensions, 0)?.to_le_bytes());
        }
        QuantizedCodebook::Scalar {
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
        QuantizedCodebook::Product {
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
) -> Result<Option<QuantizedCodebook>, HnswGraphPayloadError> {
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

fn decode_binary_codebook(bytes: &[u8]) -> Result<QuantizedCodebook, HnswGraphPayloadError> {
    if bytes.len() != size_of_u32() {
        return Err(HnswGraphPayloadError::InvalidQuantization(format!(
            "binary codebook length must be 4, got {}",
            bytes.len()
        )));
    }
    Ok(QuantizedCodebook::Binary {
        dimensions: read_u32(bytes, 0) as usize,
    })
}

fn decode_scalar_codebook(bytes: &[u8]) -> Result<QuantizedCodebook, HnswGraphPayloadError> {
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
    Ok(QuantizedCodebook::Scalar {
        dimensions: read_u32(bytes, 0) as usize,
        minimum: read_f32(bytes, 4),
        maximum: read_f32(bytes, 8),
        levels: read_u16(bytes, 12),
    })
}

fn decode_product_codebook(bytes: &[u8]) -> Result<QuantizedCodebook, HnswGraphPayloadError> {
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
    Ok(QuantizedCodebook::Product {
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
