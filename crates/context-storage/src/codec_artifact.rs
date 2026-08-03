//! Portable, checksummed codec artifacts with contiguous encoded rows.

use core::fmt;

use context_codec::{
    CONTIGUOUS_CODE_ALIGNMENT_BYTES, CodecRevision, ContiguousCodeView, ContiguousCodes,
    QuantizedCodebook, ReconstructionPolicy, validate_quantization_codebook,
    validate_quantized_code,
};

use crate::hnsw_graph_payload::{
    decode_quantization_codebook, encode_quantization_codebook,
    projected_quantization_codebook_resident_bytes, quantization_mode,
};
use crate::{FNV_OFFSET_BASIS, checksum_bytes};

const MAGIC: [u8; 8] = *b"PGCTXCOD";
const HEADER_LEN: usize = 96;
const ENDIAN_MARKER: u32 = 0x0102_0304;
const CHECKSUM_OFFSET: usize = 88;
const EXACT_SOURCE_RERANK: u8 = 1;

/// Current portable codec-artifact format version.
pub const CURRENT_CODEC_ARTIFACT_VERSION: u16 = 1;

/// Failure while encoding or attaching to a codec artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecArtifactError {
    /// The artifact uses an unknown format and must be rebuilt.
    RebuildRequired {
        /// Unsupported stored version.
        version: u16,
    },
    /// The artifact violates a bounded structural or codec invariant.
    Invalid(String),
    /// The artifact bytes do not match the stored checksum.
    ChecksumMismatch,
}

impl fmt::Display for CodecArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RebuildRequired { version } => {
                write!(
                    formatter,
                    "codec artifact version {version} requires rebuild"
                )
            }
            Self::Invalid(reason) => write!(formatter, "invalid codec artifact: {reason}"),
            Self::ChecksumMismatch => formatter.write_str("codec artifact checksum mismatch"),
        }
    }
}

impl std::error::Error for CodecArtifactError {}

/// Owned trained codebook and its fixed-width encoded row section.
#[derive(Debug, Clone, PartialEq)]
pub struct CodecArtifact {
    revision: CodecRevision,
    reconstruction_policy: ReconstructionPolicy,
    codebook: QuantizedCodebook,
    codes: ContiguousCodes,
}

impl CodecArtifact {
    /// Creates a validated portable codec artifact.
    ///
    /// # Errors
    ///
    /// Returns [`CodecArtifactError::Invalid`] when codebook dimensions,
    /// widths, row padding, or any encoded value is invalid.
    pub fn new(
        revision: CodecRevision,
        reconstruction_policy: ReconstructionPolicy,
        codebook: QuantizedCodebook,
        codes: ContiguousCodes,
    ) -> Result<Self, CodecArtifactError> {
        validate_artifact_parts(&codebook, codes.view())?;
        Ok(Self {
            revision,
            reconstruction_policy,
            codebook,
            codes,
        })
    }

    /// Returns the immutable trained-codebook revision.
    #[must_use]
    pub const fn revision(&self) -> CodecRevision {
        self.revision
    }

    /// Returns the final-score policy.
    #[must_use]
    pub const fn reconstruction_policy(&self) -> ReconstructionPolicy {
        self.reconstruction_policy
    }

    /// Returns the trained codebook.
    #[must_use]
    pub const fn codebook(&self) -> &QuantizedCodebook {
        &self.codebook
    }

    /// Returns the aligned, fixed-stride encoded rows.
    #[must_use]
    pub const fn codes(&self) -> &ContiguousCodes {
        &self.codes
    }
}

/// Validated borrowed codec rows plus their decoded codebook.
///
/// Attaching allocates only the codebook. Calls to [`Self::codes`] and
/// [`ContiguousCodeView::code`] continue to borrow the original artifact bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct CodecArtifactView<'a> {
    revision: CodecRevision,
    reconstruction_policy: ReconstructionPolicy,
    codebook: QuantizedCodebook,
    codes: ContiguousCodeView<'a>,
}

impl<'a> CodecArtifactView<'a> {
    /// Projects dynamic codebook bytes without allocating the decoded codebook.
    ///
    /// # Errors
    ///
    /// Returns [`CodecArtifactError`] when the header, section bounds, or
    /// declared codebook shape is invalid.
    pub fn projected_resident_bytes(input: &[u8]) -> Result<usize, CodecArtifactError> {
        if input.len() < HEADER_LEN {
            return Err(invalid("header is truncated"));
        }
        if input[..8] != MAGIC {
            return Err(invalid("magic is invalid"));
        }
        let version = read_u16(input, 8);
        if version != CURRENT_CODEC_ARTIFACT_VERSION {
            return Err(CodecArtifactError::RebuildRequired { version });
        }
        let mode = u32::from(input[16]);
        let codebook_offset = u64_to_usize(read_u64(input, 56), "codebook offset")?;
        let codebook_len = u64_to_usize(read_u64(input, 64), "codebook length")?;
        if codebook_offset != HEADER_LEN {
            return Err(invalid("codebook offset is invalid"));
        }
        let codebook_end = checked_end(codebook_offset, codebook_len, input.len(), "codebook")?;
        projected_quantization_codebook_resident_bytes(mode, &input[codebook_offset..codebook_end])
            .map_err(|error| invalid(error.to_string()))
    }

    /// Validates and attaches to one complete artifact byte slice.
    ///
    /// # Errors
    ///
    /// Returns [`CodecArtifactError::RebuildRequired`] for unknown versions,
    /// [`CodecArtifactError::ChecksumMismatch`] for changed bytes, and
    /// [`CodecArtifactError::Invalid`] for malformed headers, bounds,
    /// alignment, codebook, row, padding, or trailing-byte violations.
    pub fn attach(input: &'a [u8]) -> Result<Self, CodecArtifactError> {
        if input.len() < HEADER_LEN {
            return Err(invalid("header is truncated"));
        }
        if input[..8] != MAGIC {
            return Err(invalid("magic is invalid"));
        }
        let version = read_u16(input, 8);
        if version != CURRENT_CODEC_ARTIFACT_VERSION {
            return Err(CodecArtifactError::RebuildRequired { version });
        }
        if usize::from(read_u16(input, 10)) != HEADER_LEN
            || read_u32(input, 12) != ENDIAN_MARKER
            || input[18..24].iter().any(|byte| *byte != 0)
            || read_u32(input, 36) != 0
        {
            return Err(invalid("header fields are invalid"));
        }
        verify_checksum(input)?;

        let mode = u32::from(input[16]);
        let reconstruction_policy = match input[17] {
            EXACT_SOURCE_RERANK => ReconstructionPolicy::ExactSourceRerank,
            other => return Err(invalid(format!("unknown reconstruction policy {other}"))),
        };
        let dimensions = read_u32(input, 24) as usize;
        let code_width = read_u32(input, 28) as usize;
        let stride = read_u32(input, 32) as usize;
        let revision =
            CodecRevision::new(read_u64(input, 40)).ok_or_else(|| invalid("revision is zero"))?;
        let row_count = u64_to_usize(read_u64(input, 48), "row count")?;
        let codebook_offset = u64_to_usize(read_u64(input, 56), "codebook offset")?;
        let codebook_len = u64_to_usize(read_u64(input, 64), "codebook length")?;
        let codes_offset = u64_to_usize(read_u64(input, 72), "codes offset")?;
        let codes_len = u64_to_usize(read_u64(input, 80), "codes length")?;
        if codebook_offset != HEADER_LEN
            || !codes_offset.is_multiple_of(CONTIGUOUS_CODE_ALIGNMENT_BYTES)
        {
            return Err(invalid("section offsets are invalid or unaligned"));
        }
        let codebook_end = checked_end(codebook_offset, codebook_len, input.len(), "codebook")?;
        if codebook_end > codes_offset
            || input[codebook_end..codes_offset]
                .iter()
                .any(|byte| *byte != 0)
        {
            return Err(invalid("codebook alignment padding is invalid"));
        }
        let codes_end = checked_end(codes_offset, codes_len, input.len(), "codes")?;
        if codes_end != input.len() {
            return Err(invalid("artifact has trailing or missing code bytes"));
        }
        let codebook = decode_quantization_codebook(
            mode,
            dimensions,
            code_width,
            &input[codebook_offset..codebook_end],
        )
        .map_err(|error| invalid(error.to_string()))?
        .ok_or_else(|| invalid("plain mode cannot form a codec artifact"))?;
        let codes = ContiguousCodeView::new(
            code_width,
            stride,
            row_count,
            &input[codes_offset..codes_end],
        )
        .map_err(|error| invalid(error.to_string()))?;
        validate_artifact_parts(&codebook, codes)?;
        Ok(Self {
            revision,
            reconstruction_policy,
            codebook,
            codes,
        })
    }

    /// Returns the immutable trained-codebook revision.
    #[must_use]
    pub const fn revision(&self) -> CodecRevision {
        self.revision
    }

    /// Returns the original dense-vector dimensions.
    #[must_use]
    pub const fn dimensions(&self) -> usize {
        self.codebook.dimensions()
    }

    /// Returns the final-score policy.
    #[must_use]
    pub const fn reconstruction_policy(&self) -> ReconstructionPolicy {
        self.reconstruction_policy
    }

    /// Returns the decoded trained codebook.
    #[must_use]
    pub const fn codebook(&self) -> &QuantizedCodebook {
        &self.codebook
    }

    /// Returns the borrowed contiguous code rows.
    #[must_use]
    pub const fn codes(&self) -> ContiguousCodeView<'a> {
        self.codes
    }

    /// Copies the validated view into an owned artifact.
    ///
    /// # Errors
    ///
    /// Returns [`CodecArtifactError::Invalid`] only if the validated section
    /// can no longer be represented as owned contiguous rows.
    pub fn to_owned(&self) -> Result<CodecArtifact, CodecArtifactError> {
        let rows = (0..self.codes.row_count())
            .map(|row| {
                self.codes
                    .code(row)
                    .map(<[u8]>::to_vec)
                    .ok_or_else(|| invalid(format!("code row {row} is missing")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let codes = ContiguousCodes::from_rows(self.codes.code_width(), &rows)
            .map_err(|error| invalid(error.to_string()))?;
        CodecArtifact::new(
            self.revision,
            self.reconstruction_policy,
            self.codebook.clone(),
            codes,
        )
    }
}

/// Encodes one validated codec artifact.
///
/// # Errors
///
/// Returns [`CodecArtifactError::Invalid`] when a section length or offset
/// exceeds the portable integer domain or overflows `usize`.
pub fn encode_codec_artifact(artifact: &CodecArtifact) -> Result<Vec<u8>, CodecArtifactError> {
    validate_artifact_parts(artifact.codebook(), artifact.codes().view())?;
    let codebook = encode_quantization_codebook(artifact.codebook())
        .map_err(|error| invalid(error.to_string()))?;
    let codes_offset = align_up(
        HEADER_LEN
            .checked_add(codebook.len())
            .ok_or_else(|| invalid("codebook end overflows usize"))?,
    )?;
    let total = codes_offset
        .checked_add(artifact.codes().as_bytes().len())
        .ok_or_else(|| invalid("artifact length overflows usize"))?;
    let mut output = vec![0_u8; total];
    output[..8].copy_from_slice(&MAGIC);
    put_u16(&mut output, 8, CURRENT_CODEC_ARTIFACT_VERSION);
    put_u16(&mut output, 10, usize_to_u16(HEADER_LEN, "header length")?);
    put_u32(&mut output, 12, ENDIAN_MARKER);
    output[16] = u8::try_from(quantization_mode(artifact.codebook()))
        .map_err(|_| invalid("codec mode exceeds u8"))?;
    output[17] = match artifact.reconstruction_policy() {
        ReconstructionPolicy::ExactSourceRerank => EXACT_SOURCE_RERANK,
    };
    put_u32(
        &mut output,
        24,
        usize_to_u32(artifact.codebook().dimensions(), "dimensions")?,
    );
    put_u32(
        &mut output,
        28,
        usize_to_u32(artifact.codes().code_width(), "code width")?,
    );
    put_u32(
        &mut output,
        32,
        usize_to_u32(artifact.codes().stride(), "stride")?,
    );
    put_u64(&mut output, 40, artifact.revision().get());
    put_u64(
        &mut output,
        48,
        usize_to_u64(artifact.codes().row_count(), "row count")?,
    );
    put_u64(
        &mut output,
        56,
        usize_to_u64(HEADER_LEN, "codebook offset")?,
    );
    put_u64(
        &mut output,
        64,
        usize_to_u64(codebook.len(), "codebook length")?,
    );
    put_u64(&mut output, 72, usize_to_u64(codes_offset, "codes offset")?);
    put_u64(
        &mut output,
        80,
        usize_to_u64(artifact.codes().as_bytes().len(), "codes length")?,
    );
    output[HEADER_LEN..HEADER_LEN + codebook.len()].copy_from_slice(&codebook);
    output[codes_offset..].copy_from_slice(artifact.codes().as_bytes());
    let checksum = checksum_bytes(FNV_OFFSET_BASIS, &output);
    put_u64(&mut output, CHECKSUM_OFFSET, checksum);
    Ok(output)
}

fn validate_artifact_parts(
    codebook: &QuantizedCodebook,
    codes: ContiguousCodeView<'_>,
) -> Result<(), CodecArtifactError> {
    validate_quantization_codebook(codebook, codebook.dimensions())
        .map_err(|error| invalid(error.to_string()))?;
    if codes.code_width() != codebook.code_len() {
        return Err(invalid(format!(
            "code width mismatch: expected {}, got {}",
            codebook.code_len(),
            codes.code_width()
        )));
    }
    for row in 0..codes.row_count() {
        let code = codes
            .code(row)
            .ok_or_else(|| invalid(format!("code row {row} is missing")))?;
        validate_quantized_code(codebook, row, code).map_err(|error| invalid(error.to_string()))?;
    }
    Ok(())
}

fn verify_checksum(input: &[u8]) -> Result<(), CodecArtifactError> {
    let stored = read_u64(input, CHECKSUM_OFFSET);
    let mut header = [0_u8; HEADER_LEN];
    header.copy_from_slice(&input[..HEADER_LEN]);
    header[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 8].fill(0);
    let computed = checksum_bytes(
        checksum_bytes(FNV_OFFSET_BASIS, &header),
        &input[HEADER_LEN..],
    );
    if stored != computed {
        return Err(CodecArtifactError::ChecksumMismatch);
    }
    Ok(())
}

fn align_up(value: usize) -> Result<usize, CodecArtifactError> {
    value
        .checked_add(CONTIGUOUS_CODE_ALIGNMENT_BYTES - 1)
        .map(|value| value / CONTIGUOUS_CODE_ALIGNMENT_BYTES * CONTIGUOUS_CODE_ALIGNMENT_BYTES)
        .ok_or_else(|| invalid("aligned section offset overflows usize"))
}

fn checked_end(
    offset: usize,
    length: usize,
    total: usize,
    section: &str,
) -> Result<usize, CodecArtifactError> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| invalid(format!("{section} end overflows usize")))?;
    if end > total {
        return Err(invalid(format!("{section} is truncated")));
    }
    Ok(end)
}

fn invalid(reason: impl Into<String>) -> CodecArtifactError {
    CodecArtifactError::Invalid(reason.into())
}

fn u64_to_usize(value: u64, field: &str) -> Result<usize, CodecArtifactError> {
    usize::try_from(value).map_err(|_| invalid(format!("{field} exceeds usize")))
}

fn usize_to_u16(value: usize, field: &str) -> Result<u16, CodecArtifactError> {
    u16::try_from(value).map_err(|_| invalid(format!("{field} exceeds u16")))
}

fn usize_to_u32(value: usize, field: &str) -> Result<u32, CodecArtifactError> {
    u32::try_from(value).map_err(|_| invalid(format!("{field} exceeds u32")))
}

fn usize_to_u64(value: usize, field: &str) -> Result<u64, CodecArtifactError> {
    u64::try_from(value).map_err(|_| invalid(format!("{field} exceeds u64")))
}

fn read_u16(input: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([input[offset], input[offset + 1]])
}

fn read_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        input[offset],
        input[offset + 1],
        input[offset + 2],
        input[offset + 3],
    ])
}

fn read_u64(input: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        input[offset],
        input[offset + 1],
        input[offset + 2],
        input[offset + 3],
        input[offset + 4],
        input[offset + 5],
        input[offset + 6],
        input[offset + 7],
    ])
}

fn put_u16(output: &mut [u8], offset: usize, value: u16) {
    output[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(output: &mut [u8], offset: usize, value: u64) {
    output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use context_codec::{CodecSpec, TrainedCodecArtifact};
    use context_core::DenseVector;

    use super::*;

    #[test]
    fn hostile_product_codebook_count_is_rejected_after_a_valid_checksum()
    -> Result<(), Box<dyn std::error::Error>> {
        let sample = vec![
            DenseVector::new(vec![0.0, 0.0])?,
            DenseVector::new(vec![1.0, 1.0])?,
        ];
        let trained = TrainedCodecArtifact::train(CodecSpec::product(1, 2, 1)?, &sample)?;
        let artifact = CodecArtifact::new(
            trained.revision(),
            trained.reconstruction_policy(),
            trained
                .codebook()
                .cloned()
                .ok_or_else(|| std::io::Error::other("product codebook is missing"))?,
            trained
                .encode(&sample)?
                .ok_or_else(|| std::io::Error::other("product codes are missing"))?,
        )?;
        let mut encoded = encode_codec_artifact(&artifact)?;
        put_u32(&mut encoded, HEADER_LEN + 8, u32::MAX);
        put_u64(&mut encoded, CHECKSUM_OFFSET, 0);
        let checksum = checksum_bytes(FNV_OFFSET_BASIS, &encoded);
        put_u64(&mut encoded, CHECKSUM_OFFSET, checksum);

        assert!(matches!(
            CodecArtifactView::attach(&encoded),
            Err(CodecArtifactError::Invalid(message))
                if message.contains("product codebook count")
        ));
        Ok(())
    }
}
