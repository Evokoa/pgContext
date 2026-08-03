//! Immutable embedding-profile compatibility tests.

use context_core::{
    DistanceMetric, EmbeddingProfile, Error, IntegerScale, ProfileId, ProviderBinaryLayout,
    ProviderBitOrder, ProviderByteOrder, SourceAuthority, VectorNormalization,
    VectorRepresentation,
};

fn profile(
    representation: VectorRepresentation,
    metric: DistanceMetric,
    binary_layout: Option<ProviderBinaryLayout>,
    integer_scale: Option<IntegerScale>,
) -> Result<EmbeddingProfile, Error> {
    let Some(profile_id) = ProfileId::new(1) else {
        return Err(Error::InvalidVector(
            "test profile identity must be nonzero".to_owned(),
        ));
    };
    EmbeddingProfile::new(
        profile_id,
        representation,
        384,
        VectorNormalization::None,
        metric,
        "provider".to_owned(),
        "model".to_owned(),
        "revision".to_owned(),
        "input".to_owned(),
        "output".to_owned(),
        binary_layout,
        integer_scale,
        7,
    )
}

#[test]
fn integer_and_binary_profiles_are_provider_native_authority() -> Result<(), Error> {
    let int8 = profile(
        VectorRepresentation::Int8,
        DistanceMetric::Cosine,
        None,
        Some(IntegerScale {
            scale: 0.25,
            zero_point: -3,
        }),
    )?;
    let bit = profile(
        VectorRepresentation::Bit,
        DistanceMetric::Hamming,
        Some(ProviderBinaryLayout {
            bit_order: ProviderBitOrder::MostSignificantFirst,
            byte_order: ProviderByteOrder::MostSignificantFirst,
        }),
        None,
    )?;
    assert_eq!(int8.source_authority(), SourceAuthority::ProviderNative);
    assert_eq!(bit.source_authority(), SourceAuthority::ProviderNative);
    Ok(())
}

#[test]
fn profile_rejects_cross_domain_metric_layout_and_scale_drift() {
    assert!(profile(VectorRepresentation::Bit, DistanceMetric::L2, None, None).is_err());
    assert!(
        profile(
            VectorRepresentation::UInt8,
            DistanceMetric::Hamming,
            Some(ProviderBinaryLayout {
                bit_order: ProviderBitOrder::LeastSignificantFirst,
                byte_order: ProviderByteOrder::LeastSignificantFirst,
            }),
            None,
        )
        .is_err()
    );
    assert!(
        profile(
            VectorRepresentation::UInt8,
            DistanceMetric::L2,
            None,
            Some(IntegerScale {
                scale: 1.0,
                zero_point: -1,
            }),
        )
        .is_err()
    );
}
