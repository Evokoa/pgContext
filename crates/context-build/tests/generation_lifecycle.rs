//! End-to-end properties for the generic generation lifecycle.

use context_build::{
    ArtifactDescriptor, ArtifactKind, BuildError, BuildJobKind, BuildJobState, BuildJobStatus,
    GenerationManifest, GenerationState, PublicationAlias, ValidationResult, WorkerId,
};
use context_core::{ConfigurationRevision, GenerationId, SourceVersion};
use proptest::prelude::*;

#[test]
fn lease_expiry_takeover_preserves_checkpoint_and_publishes_once() -> Result<(), BuildError> {
    let first_worker = WorkerId::new(11).ok_or(BuildError::ZeroIdentity)?;
    let second_worker = WorkerId::new(12).ok_or(BuildError::ZeroIdentity)?;
    let generation = GenerationId::new(3).ok_or(BuildError::ZeroIdentity)?;
    let other_generation = GenerationId::new(4).ok_or(BuildError::ZeroIdentity)?;

    let state = BuildJobState::planned(BuildJobKind::ArtifactBuild, ArtifactKind::HnswSegment, 10)
        .claim(first_worker, 100, 10)?
        .checkpoint_to(4)?
        .recover_expired_lease(111)?;
    assert_eq!(state.status(), BuildJobStatus::Abandoned);
    assert_eq!(state.checkpoint().processed_units(), 4);

    let state = state.claim(second_worker, 111, 10)?.checkpoint_to(10)?;
    assert_eq!(state.status(), BuildJobStatus::Validating);
    assert_eq!(state.attempt(), 2);

    let state = state
        .record_validation(ValidationResult::passed(0))?
        .publish(generation)?;
    assert_eq!(state.status(), BuildJobStatus::Completed);
    assert_eq!(state.published_generation(), Some(generation));
    assert_eq!(state.publish(generation)?, state);
    assert_eq!(
        state.publish(other_generation),
        Err(BuildError::AlreadyPublishedDifferentGeneration)
    );
    Ok(())
}

#[test]
fn cancellation_and_duplicate_checkpoints_are_idempotent() -> Result<(), BuildError> {
    let worker = WorkerId::new(5).ok_or(BuildError::ZeroIdentity)?;
    let state = BuildJobState::planned(
        BuildJobKind::ProjectionBackfill,
        ArtifactKind::VectorProjection,
        8,
    )
    .claim(worker, 20, 5)?
    .checkpoint_to(3)?
    .checkpoint_to(3)?
    .checkpoint_to(2)?
    .request_cancel()?
    .request_cancel()?
    .checkpoint_to(4)?;

    assert_eq!(state.status(), BuildJobStatus::Cancelled);
    assert_eq!(state.checkpoint().processed_units(), 3);
    Ok(())
}

#[test]
fn manifest_validation_pin_and_retirement_are_fail_closed() -> Result<(), BuildError> {
    let generation = GenerationId::new(9).ok_or(BuildError::ZeroIdentity)?;
    let source = SourceVersion::new(22).ok_or(BuildError::ZeroIdentity)?;
    let configuration = ConfigurationRevision::new(7).ok_or(BuildError::ZeroIdentity)?;
    let alias = PublicationAlias::new("active")?;
    let artifacts = vec![
        ArtifactDescriptor::new(ArtifactKind::HnswSegment, "segment-0", 128, 41)?,
        ArtifactDescriptor::new(ArtifactKind::HnswDirectory, "directory", 32, 99)?,
    ];

    let manifest = GenerationManifest::staged(generation, source, configuration, alias, artifacts)?
        .record_validation(ValidationResult::passed(1))?
        .publish()?;
    assert_eq!(manifest.state(), GenerationState::Published);
    assert_eq!(manifest.total_payload_bytes(), 160);

    let manifest = manifest.pin()?.pin()?.retire()?;
    assert_eq!(manifest.state(), GenerationState::Retiring);
    assert_eq!(manifest.reader_pins(), 2);
    let manifest = manifest.unpin()?.unpin()?;
    assert_eq!(manifest.state(), GenerationState::Retired);
    assert_eq!(manifest.unpin(), Err(BuildError::ReaderPinUnderflow));
    Ok(())
}

#[test]
fn failed_validation_cannot_publish() -> Result<(), BuildError> {
    let generation = GenerationId::new(1).ok_or(BuildError::ZeroIdentity)?;
    let source = SourceVersion::new(1).ok_or(BuildError::ZeroIdentity)?;
    let configuration = ConfigurationRevision::new(1).ok_or(BuildError::ZeroIdentity)?;
    let manifest = GenerationManifest::staged(
        generation,
        source,
        configuration,
        PublicationAlias::new("active")?,
        vec![ArtifactDescriptor::new(
            ArtifactKind::CertificationEvidence,
            "report",
            1,
            7,
        )?],
    )?
    .record_validation(ValidationResult::failed(2))?;

    assert_eq!(manifest.publish(), Err(BuildError::ValidationFailed));
    Ok(())
}

#[test]
fn lease_renewal_rejects_expired_and_foreign_owners() -> Result<(), BuildError> {
    let owner = WorkerId::new(1).ok_or(BuildError::ZeroIdentity)?;
    let other = WorkerId::new(2).ok_or(BuildError::ZeroIdentity)?;
    let state = BuildJobState::planned(BuildJobKind::Compaction, ArtifactKind::HnswDirectory, 2)
        .claim(owner, 10, 5)?;
    assert_eq!(
        state.renew_lease(other, 11, 5),
        Err(BuildError::LeaseOwnerMismatch)
    );
    assert_eq!(
        state.renew_lease(owner, 15, 5),
        Err(BuildError::LeaseExpired)
    );
    assert_eq!(
        state.recover_expired_lease(14),
        Err(BuildError::LeaseNotExpired)
    );
    Ok(())
}

#[test]
fn duplicate_inventory_and_conflicting_validation_fail_closed() -> Result<(), BuildError> {
    let generation = GenerationId::new(2).ok_or(BuildError::ZeroIdentity)?;
    let source = SourceVersion::new(2).ok_or(BuildError::ZeroIdentity)?;
    let configuration = ConfigurationRevision::new(2).ok_or(BuildError::ZeroIdentity)?;
    let descriptor = ArtifactDescriptor::new(ArtifactKind::IvfPostings, "postings", 4, 9)?;
    assert_eq!(
        GenerationManifest::staged(
            generation,
            source,
            configuration,
            PublicationAlias::new("active")?,
            vec![descriptor.clone(), descriptor],
        ),
        Err(BuildError::DuplicateArtifact)
    );

    let manifest = GenerationManifest::staged(
        generation,
        source,
        configuration,
        PublicationAlias::new("active")?,
        vec![ArtifactDescriptor::new(
            ArtifactKind::IvfCentroids,
            "centroids",
            8,
            10,
        )?],
    )?
    .record_validation(ValidationResult::passed(0))?;
    assert_eq!(
        manifest.record_validation(ValidationResult::failed(1)),
        Err(BuildError::ConflictingValidation)
    );
    Ok(())
}

#[test]
fn every_active_boundary_recovers_only_after_lease_expiry() -> Result<(), BuildError> {
    let worker = WorkerId::new(7).ok_or(BuildError::ZeroIdentity)?;
    let running = BuildJobState::planned(
        BuildJobKind::Certification,
        ArtifactKind::CertificationEvidence,
        1,
    )
    .claim(worker, 10, 2)?;
    let states = [
        running,
        running.request_cancel()?,
        running.checkpoint_to(1)?,
        running
            .checkpoint_to(1)?
            .record_validation(ValidationResult::passed(0))?,
    ];
    for state in states {
        assert_eq!(
            state.recover_expired_lease(11),
            Err(BuildError::LeaseNotExpired)
        );
        let recovered = state.recover_expired_lease(12)?;
        assert_eq!(recovered.status(), BuildJobStatus::Abandoned);
        assert!(recovered.lease().is_none());
    }
    Ok(())
}

#[test]
fn cancellation_policy_matches_every_advertised_active_edge() -> Result<(), BuildError> {
    let worker = WorkerId::new(11).ok_or(BuildError::ZeroIdentity)?;
    let planned = BuildJobState::planned(
        BuildJobKind::Certification,
        ArtifactKind::CertificationEvidence,
        1,
    );
    let running = planned.claim(worker, 0, 2)?;
    let validating = running.checkpoint_to(1)?;
    let publishing = validating.record_validation(ValidationResult::passed(0))?;

    assert_eq!(
        planned.request_cancel()?.status(),
        BuildJobStatus::Cancelled
    );
    assert_eq!(
        running.request_cancel()?.status(),
        BuildJobStatus::CancelRequested
    );
    assert_eq!(
        validating.request_cancel()?.status(),
        BuildJobStatus::CancelRequested
    );
    assert_eq!(
        publishing.request_cancel(),
        Err(BuildError::InvalidTransition)
    );
    assert!(!BuildJobStatus::Publishing.allows_transition(BuildJobStatus::CancelRequested));
    assert!(BuildJobStatus::Validating.allows_transition(BuildJobStatus::CancelRequested));
    Ok(())
}

proptest! {
    #[test]
    fn arbitrary_job_actions_preserve_shared_lifecycle_invariants(
        actions in prop::collection::vec(any::<u8>(), 0..128)
    ) {
        let worker = WorkerId::new(1)
            .ok_or_else(|| TestCaseError::fail("worker must be non-zero"))?;
        let generation = GenerationId::new(1)
            .ok_or_else(|| TestCaseError::fail("generation must be non-zero"))?;
        let mut state = BuildJobState::planned(
            BuildJobKind::Certification,
            ArtifactKind::CertificationEvidence,
            7,
        );
        let mut now = 0_u64;

        for action in actions {
            now = now.saturating_add(1);
            let before = state;
            let result = match action % 10 {
                0 => state.claim(worker, now, 3),
                1 => state.renew_lease(worker, now, 3),
                2 => state.checkpoint_to(u64::from(action % 10)),
                3 => state.request_cancel(),
                4 => state.record_validation(ValidationResult::passed(u32::from(action % 3))),
                5 => state.record_validation(ValidationResult::failed(u32::from(action % 3))),
                6 => state.publish(generation),
                7 => state.fail(),
                8 => state.retry(),
                _ if state.status() == BuildJobStatus::Publishing => state.revalidate(),
                _ => state.recover_expired_lease(now),
            };
            if let Ok(after) = result {
                prop_assert!(before.status().allows_transition(after.status()));
                prop_assert!(after.checkpoint().processed_units() <= after.checkpoint().total_units());
                if after.checkpoint().processed_units() < before.checkpoint().processed_units() {
                    prop_assert_eq!(after.status(), BuildJobStatus::Planned);
                }
                if matches!(
                    after.status(),
                    BuildJobStatus::Planned
                        | BuildJobStatus::Cancelled
                        | BuildJobStatus::Completed
                        | BuildJobStatus::Failed
                        | BuildJobStatus::Abandoned
                ) {
                    prop_assert!(after.lease().is_none());
                }
                state = after;
            } else {
                prop_assert_eq!(state, before);
            }
        }
    }


    #[test]
    fn arbitrary_publication_actions_fail_closed(
        actions in prop::collection::vec(any::<u8>(), 0..128)
    ) {
        let generation = GenerationId::new(1)
            .ok_or_else(|| TestCaseError::fail("generation must be non-zero"))?;
        let source = SourceVersion::new(1)
            .ok_or_else(|| TestCaseError::fail("source must be non-zero"))?;
        let configuration = ConfigurationRevision::new(1)
            .ok_or_else(|| TestCaseError::fail("configuration must be non-zero"))?;
        let alias = PublicationAlias::new("active")
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let artifact = ArtifactDescriptor::new(
            ArtifactKind::CertificationEvidence,
            "evidence",
            1,
            1,
        )
        .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let mut manifest = GenerationManifest::staged(
            generation,
            source,
            configuration,
            alias,
            vec![artifact],
        )
        .map_err(|error| TestCaseError::fail(error.to_string()))?;

        for action in actions {
            let before = manifest.clone();
            let result = match action % 6 {
                0 => before
                    .clone()
                    .record_validation(ValidationResult::passed(u32::from(action % 3))),
                1 => before
                    .clone()
                    .record_validation(ValidationResult::failed(u32::from(action % 3))),
                2 => before.clone().publish(),
                3 => before.clone().pin(),
                4 => before.clone().unpin(),
                _ => before.clone().retire(),
            };
            if let Ok(after) = result {
                if after.reader_pins() > 0 {
                    prop_assert!(matches!(
                        after.state(),
                        GenerationState::Published | GenerationState::Retiring
                    ));
                }
                if after.state() == GenerationState::Retired {
                    prop_assert_eq!(after.reader_pins(), 0);
                }
                manifest = after;
            } else {
                prop_assert_eq!(&manifest, &before);
            }
        }
    }
}
