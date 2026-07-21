//! Vector representation conversion policy tests.

use context_core::{VectorConversionPolicy, VectorRepresentation};

#[test]
fn vector_conversion_policy_marks_identity_as_lossless() {
    assert_eq!(
        VectorRepresentation::Dense.conversion_policy_to(VectorRepresentation::Dense),
        VectorConversionPolicy::Lossless
    );
    assert_eq!(
        VectorRepresentation::Bit.conversion_policy_to(VectorRepresentation::Bit),
        VectorConversionPolicy::Lossless
    );
}

#[test]
fn vector_conversion_policy_marks_dense_sparse_as_lossless() {
    assert_eq!(
        VectorRepresentation::Dense.conversion_policy_to(VectorRepresentation::Sparse),
        VectorConversionPolicy::Lossless
    );
    assert_eq!(
        VectorRepresentation::Sparse.conversion_policy_to(VectorRepresentation::Dense),
        VectorConversionPolicy::Lossless
    );
}

#[test]
fn vector_conversion_policy_marks_half_downcasts_as_checked_lossy() {
    assert_eq!(
        VectorRepresentation::Dense.conversion_policy_to(VectorRepresentation::Half),
        VectorConversionPolicy::CheckedLossy
    );
    assert_eq!(
        VectorRepresentation::Sparse.conversion_policy_to(VectorRepresentation::Half),
        VectorConversionPolicy::CheckedLossy
    );
}

#[test]
fn vector_conversion_policy_forbids_bit_numeric_casts() {
    assert_eq!(
        VectorRepresentation::Dense.conversion_policy_to(VectorRepresentation::Bit),
        VectorConversionPolicy::Forbidden
    );
    assert_eq!(
        VectorRepresentation::Bit.conversion_policy_to(VectorRepresentation::Dense),
        VectorConversionPolicy::Forbidden
    );
}

#[test]
fn vector_conversion_policy_covers_every_representation_pair() {
    use VectorConversionPolicy::{CheckedLossy, Forbidden, Lossless};
    use VectorRepresentation::{Bit, Dense, Half, Sparse};

    let expected = [
        (Dense, [Lossless, CheckedLossy, Lossless, Forbidden]),
        (Half, [Lossless, Lossless, Lossless, Forbidden]),
        (Sparse, [Lossless, CheckedLossy, Lossless, Forbidden]),
        (Bit, [Forbidden, Forbidden, Forbidden, Lossless]),
    ];
    let targets = [Dense, Half, Sparse, Bit];

    for (source, policies) in expected {
        for (target, expected_policy) in targets.into_iter().zip(policies) {
            assert_eq!(
                source.conversion_policy_to(target),
                expected_policy,
                "unexpected {source:?} to {target:?} conversion policy"
            );
        }
    }
}
