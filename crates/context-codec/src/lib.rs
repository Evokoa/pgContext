//! Pure vector codec specifications, training, encoding, and query scoring.

mod artifact;
mod encoded;
mod error;
mod layout;
mod quantization;
mod registration;
mod spec;
mod training;

pub use artifact::TrainedCodecArtifact;
pub use encoded::{
    PreparedQuantizedQuery, QuantizedCodebook, validate_quantization_codebook,
    validate_quantized_code,
};
pub use error::{CodecError, Result};
pub use layout::{CONTIGUOUS_CODE_ALIGNMENT_BYTES, ContiguousCodeView, ContiguousCodes};
pub use quantization::{
    ProductCodebook, ProductQuantizedVector, ProductQuantizer, RerankCandidate, RerankResult,
    ScalarQuantizedVector, ScalarQuantizer, binary_quantize, rerank_by_original_vectors,
};
pub use registration::{RetrievalRegistrationError, validate_retrieval_combination};
pub use spec::{CodecRevision, CodecSpec, ReconstructionPolicy, ScalarBounds};
pub use training::{TrainedQuantizer, train_product_quantizer, train_scalar_quantizer};

/// Runtime codec family used by retrieval registration.
///
/// Persisted storage formats use their own versioned descriptors and must be
/// validated before being converted into this runtime classification.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CodecKind {
    /// Full-precision values with no derived encoding.
    Plain,
    /// One sign bit per dense coordinate.
    Binary,
    /// Uniform scalar byte quantization.
    Scalar,
    /// Product quantization with trained centroid tables.
    Product,
}
