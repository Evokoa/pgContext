//! Exhaustive retrieval representation/metric/index/codec registration tests.

use context_codec::{CodecKind, RetrievalRegistrationError, validate_retrieval_combination};
use context_core::{DistanceMetric, IndexKind, VectorRepresentation};

#[test]
fn current_supported_combinations_are_registered_once() -> Result<(), RetrievalRegistrationError> {
    let numeric = [
        VectorRepresentation::Dense,
        VectorRepresentation::Half,
        VectorRepresentation::Sparse,
    ];
    let numeric_metrics = [
        DistanceMetric::L2,
        DistanceMetric::NegativeInnerProduct,
        DistanceMetric::Cosine,
        DistanceMetric::L1,
    ];

    for representation in numeric {
        for metric in numeric_metrics {
            validate_retrieval_combination(
                representation,
                metric,
                IndexKind::Exact,
                CodecKind::Plain,
            )?;
            validate_retrieval_combination(
                representation,
                metric,
                IndexKind::Hnsw,
                CodecKind::Plain,
            )?;
        }
    }

    for metric in [DistanceMetric::Hamming, DistanceMetric::Jaccard] {
        validate_retrieval_combination(
            VectorRepresentation::Bit,
            metric,
            IndexKind::Exact,
            CodecKind::Plain,
        )?;
        validate_retrieval_combination(
            VectorRepresentation::Bit,
            metric,
            IndexKind::Hnsw,
            CodecKind::Plain,
        )?;
    }

    for codec in [CodecKind::Binary, CodecKind::Scalar, CodecKind::Product] {
        for metric in numeric_metrics {
            validate_retrieval_combination(
                VectorRepresentation::Dense,
                metric,
                IndexKind::Hnsw,
                codec,
            )?;
        }
    }
    Ok(())
}

#[test]
fn planned_and_invalid_combinations_fail_with_typed_reasons() {
    assert!(matches!(
        validate_retrieval_combination(
            VectorRepresentation::Int8,
            DistanceMetric::L2,
            IndexKind::Hnsw,
            CodecKind::Plain,
        ),
        Err(RetrievalRegistrationError::FeatureUnavailable { .. })
    ));
    assert!(matches!(
        validate_retrieval_combination(
            VectorRepresentation::Dense,
            DistanceMetric::L2,
            IndexKind::IvfFlat,
            CodecKind::Plain,
        ),
        Err(RetrievalRegistrationError::FeatureUnavailable { .. })
    ));
    assert!(matches!(
        validate_retrieval_combination(
            VectorRepresentation::Bit,
            DistanceMetric::Cosine,
            IndexKind::Hnsw,
            CodecKind::Plain,
        ),
        Err(RetrievalRegistrationError::UnsupportedCombination { .. })
    ));
    assert!(matches!(
        validate_retrieval_combination(
            VectorRepresentation::Sparse,
            DistanceMetric::L2,
            IndexKind::Hnsw,
            CodecKind::Scalar,
        ),
        Err(RetrievalRegistrationError::UnsupportedCombination { .. })
    ));
}

#[test]
fn every_registration_tuple_has_one_frozen_classification() {
    let representations = [
        VectorRepresentation::Dense,
        VectorRepresentation::Half,
        VectorRepresentation::Sparse,
        VectorRepresentation::Bit,
        VectorRepresentation::Int8,
        VectorRepresentation::UInt8,
    ];
    let metrics = [
        DistanceMetric::L2,
        DistanceMetric::InnerProduct,
        DistanceMetric::NegativeInnerProduct,
        DistanceMetric::Cosine,
        DistanceMetric::L1,
        DistanceMetric::Hamming,
        DistanceMetric::Jaccard,
    ];
    let indexes = [IndexKind::Exact, IndexKind::Hnsw, IndexKind::IvfFlat];
    let codecs = [
        CodecKind::Plain,
        CodecKind::Binary,
        CodecKind::Scalar,
        CodecKind::Product,
    ];
    let mut counts = [0_usize; 3];

    for representation in representations {
        for metric in metrics {
            for index in indexes {
                for codec in codecs {
                    match validate_retrieval_combination(representation, metric, index, codec) {
                        Ok(()) => counts[0] += 1,
                        Err(RetrievalRegistrationError::FeatureUnavailable { .. }) => {
                            counts[1] += 1
                        }
                        Err(RetrievalRegistrationError::UnsupportedCombination { .. }) => {
                            counts[2] += 1
                        }
                    }
                }
            }
        }
    }
    assert_eq!(counts, [43, 50, 411]);
    assert_eq!(counts.into_iter().sum::<usize>(), 504);

    for invalid in [
        validate_retrieval_combination(
            VectorRepresentation::Int8,
            DistanceMetric::Hamming,
            IndexKind::Exact,
            CodecKind::Product,
        ),
        validate_retrieval_combination(
            VectorRepresentation::Bit,
            DistanceMetric::Cosine,
            IndexKind::IvfFlat,
            CodecKind::Product,
        ),
    ] {
        assert!(matches!(
            invalid,
            Err(RetrievalRegistrationError::UnsupportedCombination { .. })
        ));
    }
}
