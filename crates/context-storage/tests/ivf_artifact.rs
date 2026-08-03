//! Versioned IVFFlat generation format and corruption contracts.

use context_core::DistanceMetric;
use context_storage::{
    CURRENT_IVF_ARTIFACT_VERSION, IvfArtifactError, IvfArtifactPosting, IvfGenerationArtifact,
    IvfGenerationView, encode_ivf_generation,
};
use proptest::prelude::*;
use std::error::Error;

fn fixture() -> Result<IvfGenerationArtifact, IvfArtifactError> {
    IvfGenerationArtifact::new(
        DistanceMetric::L2,
        vec![vec![0.0, 0.0], vec![10.0, 10.0]],
        vec![
            vec![IvfArtifactPosting::new(1, vec![0.1, 0.0])?],
            vec![IvfArtifactPosting::new(2, vec![10.1, 10.0])?],
        ],
        None,
    )
}

#[test]
fn generation_round_trips_through_a_borrowed_view() -> Result<(), Box<dyn Error>> {
    let bytes = encode_ivf_generation(&fixture()?)?;
    let view = IvfGenerationView::attach(&bytes)?;

    assert_eq!(view.version(), CURRENT_IVF_ARTIFACT_VERSION);
    assert_eq!(view.metric(), DistanceMetric::L2);
    assert_eq!(view.dimensions(), 2);
    assert_eq!(view.list_count(), 2);
    assert_eq!(view.posting_count(), 2);
    assert_eq!(
        view.centroid(1)
            .ok_or("centroid is missing")?
            .collect::<Vec<_>>(),
        vec![10.0, 10.0]
    );
    let posting = view.posting(0, 0)?;
    assert_eq!(posting.point_id(), 1);
    assert_eq!(posting.vector().collect::<Vec<_>>(), vec![0.1, 0.0]);
    Ok(())
}

#[test]
fn unknown_version_checksum_truncation_and_trailing_bytes_fail_closed() -> Result<(), Box<dyn Error>>
{
    let bytes = encode_ivf_generation(&fixture()?)?;

    let mut unknown = bytes.clone();
    unknown[8..10].copy_from_slice(&(CURRENT_IVF_ARTIFACT_VERSION + 1).to_le_bytes());
    assert!(matches!(
        IvfGenerationView::attach(&unknown),
        Err(IvfArtifactError::RebuildRequired { .. })
    ));

    let mut corrupt = bytes.clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 1;
    assert_eq!(
        IvfGenerationView::attach(&corrupt),
        Err(IvfArtifactError::ChecksumMismatch)
    );
    assert!(IvfGenerationView::attach(&bytes[..bytes.len() - 1]).is_err());

    let mut trailing = bytes;
    trailing.push(0);
    assert!(matches!(
        IvfGenerationView::attach(&trailing),
        Err(IvfArtifactError::Invalid(_))
    ));
    Ok(())
}

#[test]
fn constructors_reject_ragged_lists_and_duplicate_point_ids() -> Result<(), Box<dyn Error>> {
    let ragged = IvfGenerationArtifact::new(
        DistanceMetric::L2,
        vec![vec![0.0, 0.0]],
        vec![vec![IvfArtifactPosting::new(1, vec![0.0])?]],
        None,
    );
    assert!(matches!(ragged, Err(IvfArtifactError::Invalid(_))));

    let duplicate = IvfGenerationArtifact::new(
        DistanceMetric::L2,
        vec![vec![0.0], vec![1.0]],
        vec![
            vec![IvfArtifactPosting::new(7, vec![0.0])?],
            vec![IvfArtifactPosting::new(7, vec![1.0])?],
        ],
        None,
    );
    assert!(matches!(duplicate, Err(IvfArtifactError::Invalid(_))));

    let over_policy = IvfGenerationArtifact::new(
        DistanceMetric::L2,
        vec![vec![0.0; context_core::policy::MAX_VECTOR_DIMENSIONS + 1]],
        vec![Vec::new()],
        None,
    );
    assert!(matches!(over_policy, Err(IvfArtifactError::Invalid(_))));
    Ok(())
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_create_an_unvalidated_view(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        if let Ok(view) = IvfGenerationView::attach(&bytes) {
            prop_assert!(view.dimensions() > 0);
            prop_assert!(view.list_count() > 0);
            for list in 0..view.list_count() {
                prop_assert!(view.centroid(list).is_some());
                prop_assert!(view.list_len(list).is_ok());
            }
        }
    }
}
