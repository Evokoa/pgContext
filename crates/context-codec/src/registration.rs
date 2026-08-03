//! Canonical registration table for retrieval storage combinations.

use context_core::{DistanceMetric, IndexKind, VectorRepresentation};
use thiserror::Error;

use crate::CodecKind;

/// Failure returned when a retrieval storage combination is not registered.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RetrievalRegistrationError {
    /// The combination is part of the roadmap but has no implementation yet.
    #[error("{representation:?}/{metric:?}/{index:?}/{codec:?} is planned but unavailable")]
    FeatureUnavailable {
        /// Requested vector representation.
        representation: VectorRepresentation,
        /// Requested distance metric.
        metric: DistanceMetric,
        /// Requested index family.
        index: IndexKind,
        /// Requested vector codec.
        codec: CodecKind,
    },
    /// The combination is invalid and is not part of the supported contract.
    #[error("unsupported retrieval combination: {representation:?}/{metric:?}/{index:?}/{codec:?}")]
    UnsupportedCombination {
        /// Requested vector representation.
        representation: VectorRepresentation,
        /// Requested distance metric.
        metric: DistanceMetric,
        /// Requested index family.
        index: IndexKind,
        /// Requested vector codec.
        codec: CodecKind,
    },
}

/// Validates a representation × metric × index × codec combination against
/// the single canonical registration table.
///
/// # Errors
///
/// Returns [`RetrievalRegistrationError::FeatureUnavailable`] for roadmap
/// combinations that have not shipped and
/// [`RetrievalRegistrationError::UnsupportedCombination`] for combinations
/// outside the retrieval contract.
pub const fn validate_retrieval_combination(
    representation: VectorRepresentation,
    metric: DistanceMetric,
    index: IndexKind,
    codec: CodecKind,
) -> Result<(), RetrievalRegistrationError> {
    let representation_class = representation_class(representation);
    let metric_class = metric_class(metric);
    let index_class = index_class(index);
    let codec_class = codec_class(codec);
    let structurally_valid = matches!(
        (representation_class, metric_class, index_class, codec_class),
        (
            RepresentationClass::Dense | RepresentationClass::Numeric,
            MetricClass::Numeric | MetricClass::AscendingNumeric,
            IndexClass::Exact,
            CodecClass::Plain,
        ) | (
            RepresentationClass::Dense | RepresentationClass::Numeric,
            MetricClass::AscendingNumeric,
            IndexClass::Hnsw,
            CodecClass::Plain,
        ) | (
            RepresentationClass::Binary,
            MetricClass::Binary,
            IndexClass::Exact | IndexClass::Hnsw,
            CodecClass::Plain,
        ) | (
            RepresentationClass::Dense,
            MetricClass::AscendingNumeric,
            IndexClass::Hnsw | IndexClass::PlannedIvfFlat,
            CodecClass::Derived,
        ) | (
            RepresentationClass::PlannedInteger,
            MetricClass::Numeric | MetricClass::AscendingNumeric,
            IndexClass::Exact,
            CodecClass::Plain,
        ) | (
            RepresentationClass::PlannedInteger,
            MetricClass::AscendingNumeric,
            IndexClass::Hnsw | IndexClass::PlannedIvfFlat,
            CodecClass::Plain,
        ) | (
            RepresentationClass::Dense | RepresentationClass::Numeric,
            MetricClass::AscendingNumeric,
            IndexClass::PlannedIvfFlat,
            CodecClass::Plain,
        )
    );

    if !structurally_valid {
        return Err(RetrievalRegistrationError::UnsupportedCombination {
            representation,
            metric,
            index,
            codec,
        });
    }

    if matches!(representation_class, RepresentationClass::PlannedInteger)
        || matches!(index_class, IndexClass::PlannedIvfFlat)
    {
        return Err(RetrievalRegistrationError::FeatureUnavailable {
            representation,
            metric,
            index,
            codec,
        });
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum RepresentationClass {
    Dense,
    Numeric,
    Binary,
    PlannedInteger,
}

const fn representation_class(representation: VectorRepresentation) -> RepresentationClass {
    match representation {
        VectorRepresentation::Dense => RepresentationClass::Dense,
        VectorRepresentation::Half | VectorRepresentation::Sparse => RepresentationClass::Numeric,
        VectorRepresentation::Bit => RepresentationClass::Binary,
        VectorRepresentation::Int8 | VectorRepresentation::UInt8 => {
            RepresentationClass::PlannedInteger
        }
    }
}

#[derive(Clone, Copy)]
enum MetricClass {
    Numeric,
    AscendingNumeric,
    Binary,
}

const fn metric_class(metric: DistanceMetric) -> MetricClass {
    match metric {
        DistanceMetric::InnerProduct => MetricClass::Numeric,
        DistanceMetric::L2
        | DistanceMetric::NegativeInnerProduct
        | DistanceMetric::Cosine
        | DistanceMetric::L1 => MetricClass::AscendingNumeric,
        DistanceMetric::Hamming | DistanceMetric::Jaccard => MetricClass::Binary,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum IndexClass {
    Exact,
    Hnsw,
    PlannedIvfFlat,
}

const fn index_class(index: IndexKind) -> IndexClass {
    match index {
        IndexKind::Exact => IndexClass::Exact,
        IndexKind::Hnsw => IndexClass::Hnsw,
        IndexKind::IvfFlat => IndexClass::PlannedIvfFlat,
    }
}

#[derive(Clone, Copy)]
enum CodecClass {
    Plain,
    Derived,
}

const fn codec_class(codec: CodecKind) -> CodecClass {
    match codec {
        CodecKind::Plain => CodecClass::Plain,
        CodecKind::Binary | CodecKind::Scalar | CodecKind::Product => CodecClass::Derived,
    }
}
