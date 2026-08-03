//! Trained codec artifacts and static query preparation.

use context_core::{DenseVector, DistanceMetric};

use crate::spec::{DEFAULT_PRODUCT_SUBVECTOR_DIMENSIONS, FNV_OFFSET_BASIS, hash_bytes};
use crate::{
    CodecError, CodecKind, CodecRevision, CodecSpec, ContiguousCodes, PreparedQuantizedQuery,
    QuantizedCodebook, ReconstructionPolicy, Result, TrainedQuantizer, train_product_quantizer,
    train_scalar_quantizer, validate_quantization_codebook,
};

/// Trained index-independent codec state for one immutable revision.
///
/// The artifact owns the trained quantizer used to encode rebuildable index
/// rows and the equivalent codebook used by storage and prepared query
/// scorers. Final ranking must follow [`Self::reconstruction_policy`].
#[derive(Debug, Clone, PartialEq)]
pub struct TrainedCodecArtifact {
    spec: CodecSpec,
    revision: CodecRevision,
    dimensions: usize,
    quantizer: Option<TrainedQuantizer>,
    codebook: Option<QuantizedCodebook>,
}

impl TrainedCodecArtifact {
    /// Trains a codec deterministically from a source sample.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`] for an empty or dimensionally inconsistent
    /// sample, incompatible product dimensions, or invalid trained state.
    pub fn train(spec: CodecSpec, sample: &[DenseVector]) -> Result<Self> {
        let dimensions = validate_sample(sample)?;
        let quantizer = match spec.kind() {
            CodecKind::Plain => None,
            CodecKind::Binary => Some(TrainedQuantizer::binary(dimensions)?),
            CodecKind::Scalar => {
                let (levels, bounds) = spec.scalar_parameters().ok_or_else(|| {
                    CodecError::InvalidCode("scalar codec parameters are missing".to_owned())
                })?;
                let bounds = bounds.map(|bounds| (bounds.minimum(), bounds.maximum()));
                Some(train_scalar_quantizer(sample, levels, bounds)?)
            }
            CodecKind::Product => {
                let (subvector_dimensions, centroid_count, iterations) =
                    spec.product_parameters().ok_or_else(|| {
                        CodecError::InvalidCode("product codec parameters are missing".to_owned())
                    })?;
                Some(train_product_quantizer(
                    sample,
                    subvector_dimensions
                        .unwrap_or_else(|| default_subvector_dimensions(dimensions)),
                    centroid_count.min(sample.len()),
                    iterations,
                )?)
            }
        };
        let codebook = quantizer.as_ref().map(codebook_from_quantizer);
        if let Some(codebook) = &codebook {
            validate_quantization_codebook(codebook, dimensions)?;
        }
        let revision = artifact_revision(spec, dimensions, codebook.as_ref());
        Ok(Self {
            spec,
            revision,
            dimensions,
            quantizer,
            codebook,
        })
    }

    /// Returns the normalized configuration used for training.
    #[must_use]
    pub const fn spec(&self) -> CodecSpec {
        self.spec
    }

    /// Returns the immutable trained-artifact revision.
    #[must_use]
    pub const fn revision(&self) -> CodecRevision {
        self.revision
    }

    /// Returns the original dense-vector dimensions.
    #[must_use]
    pub const fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Returns the final-score policy for candidates produced by this artifact.
    #[must_use]
    pub const fn reconstruction_policy(&self) -> ReconstructionPolicy {
        self.spec.reconstruction_policy()
    }

    /// Returns the trained codebook, or `None` for full precision.
    #[must_use]
    pub const fn codebook(&self) -> Option<&QuantizedCodebook> {
        self.codebook.as_ref()
    }

    /// Encodes source vectors into one aligned, fixed-stride contiguous section.
    ///
    /// Full precision returns `Ok(None)` because it has no derived codes.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`] for dimension mismatches, invalid trained state,
    /// or a section-size overflow.
    pub fn encode(&self, vectors: &[DenseVector]) -> Result<Option<ContiguousCodes>> {
        self.encode_refs(vectors.iter())
    }

    /// Encodes borrowed source vectors without first cloning them into a
    /// second full-precision generation.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`] for dimension mismatches, invalid trained state,
    /// or a section-size overflow.
    pub fn encode_refs<'a, I>(&self, vectors: I) -> Result<Option<ContiguousCodes>>
    where
        I: IntoIterator<Item = &'a DenseVector>,
        I::IntoIter: ExactSizeIterator,
    {
        let Some(quantizer) = &self.quantizer else {
            return Ok(None);
        };
        let mut vectors = vectors.into_iter();
        let row_count = vectors.len();
        let code_width = self
            .codebook
            .as_ref()
            .map_or(0, QuantizedCodebook::code_len);
        ContiguousCodes::from_row_writer(code_width, row_count, |_row, output| {
            let vector = vectors.next().ok_or_else(|| {
                CodecError::InvalidCode("codec source iterator ended early".to_owned())
            })?;
            quantizer.quantize_into(vector, output)
        })
        .map(Some)
    }

    /// Prepares one allocation-free per-code scorer for a query.
    ///
    /// Full precision returns `Ok(None)` so the index adapter can use its
    /// ordinary dense scorer.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`] when the query, metric, or trained codebook is
    /// incompatible.
    pub fn prepare_query(
        &self,
        query: &DenseVector,
        metric: DistanceMetric,
    ) -> Result<Option<PreparedQuantizedQuery>> {
        self.codebook
            .as_ref()
            .map(|codebook| codebook.prepare_query(query, metric))
            .transpose()
    }
}

fn default_subvector_dimensions(dimensions: usize) -> usize {
    (1..=DEFAULT_PRODUCT_SUBVECTOR_DIMENSIONS.min(dimensions))
        .rev()
        .find(|candidate| dimensions.is_multiple_of(*candidate))
        .unwrap_or(1)
}

fn validate_sample(sample: &[DenseVector]) -> Result<usize> {
    let dimensions = sample
        .first()
        .map(DenseVector::dimension)
        .ok_or_else(|| CodecError::InvalidCode("codec training sample is empty".to_owned()))?;
    if let Some(vector) = sample
        .iter()
        .find(|vector| vector.dimension() != dimensions)
    {
        return Err(CodecError::DimensionMismatch {
            left: dimensions,
            right: vector.dimension(),
        });
    }
    Ok(dimensions)
}

fn codebook_from_quantizer(trained: &TrainedQuantizer) -> QuantizedCodebook {
    match trained {
        TrainedQuantizer::Binary { dimensions } => QuantizedCodebook::Binary {
            dimensions: *dimensions,
        },
        TrainedQuantizer::Scalar {
            quantizer,
            dimensions,
        } => QuantizedCodebook::Scalar {
            dimensions: *dimensions,
            minimum: quantizer.min(),
            maximum: quantizer.max(),
            levels: quantizer.levels(),
        },
        TrainedQuantizer::Product(quantizer) => QuantizedCodebook::Product {
            dimensions: trained.dimensions(),
            subvector_dimensions: quantizer.subvector_dimensions(),
            codebooks: quantizer
                .codebooks()
                .iter()
                .map(|codebook| codebook.centroids().to_vec())
                .collect(),
        },
    }
}

fn artifact_revision(
    spec: CodecSpec,
    dimensions: usize,
    codebook: Option<&QuantizedCodebook>,
) -> CodecRevision {
    let mut hash = hash_bytes(FNV_OFFSET_BASIS, &spec.revision().get().to_le_bytes());
    hash = hash_bytes(
        hash,
        &u64::try_from(dimensions).unwrap_or(u64::MAX).to_le_bytes(),
    );
    match codebook {
        None => hash = hash_bytes(hash, &[0]),
        Some(QuantizedCodebook::Binary { .. }) => hash = hash_bytes(hash, &[1]),
        Some(QuantizedCodebook::Scalar {
            minimum,
            maximum,
            levels,
            ..
        }) => {
            hash = hash_bytes(hash, &[2]);
            hash = hash_bytes(hash, &minimum.to_bits().to_le_bytes());
            hash = hash_bytes(hash, &maximum.to_bits().to_le_bytes());
            hash = hash_bytes(hash, &levels.to_le_bytes());
        }
        Some(QuantizedCodebook::Product {
            subvector_dimensions,
            codebooks,
            ..
        }) => {
            hash = hash_bytes(hash, &[3]);
            hash = hash_bytes(
                hash,
                &u64::try_from(*subvector_dimensions)
                    .unwrap_or(u64::MAX)
                    .to_le_bytes(),
            );
            for centroids in codebooks {
                hash = hash_bytes(
                    hash,
                    &u64::try_from(centroids.len())
                        .unwrap_or(u64::MAX)
                        .to_le_bytes(),
                );
                for centroid in centroids {
                    for value in centroid.as_slice() {
                        hash = hash_bytes(hash, &value.to_bits().to_le_bytes());
                    }
                }
            }
        }
    }
    CodecRevision::from_hash(hash)
}
