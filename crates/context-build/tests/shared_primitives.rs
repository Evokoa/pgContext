//! Contract tests for build primitives shared by later retrieval features.

use std::error::Error;

use context_build::{
    FindingCode, FindingLocation, FindingSeverity, MemoryBudget, OrderedRowset, RowsetError,
    SpillError, SpillRun, StructuralFinding, TrainingError, deterministic_kmeans,
    deterministic_metric_clusters, deterministic_metric_clusters_with_workers,
    deterministic_sample,
};
use proptest::prelude::*;

#[test]
fn ordered_rowset_rejects_duplicates_before_mapping() {
    assert_eq!(
        OrderedRowset::new(vec![("b", 2), ("a", 1), ("b", 3)], 8),
        Err(RowsetError::DuplicateKey {
            first: 0,
            duplicate: 2
        })
    );
}

#[test]
fn metric_training_is_byte_identical_across_worker_counts() -> Result<(), TrainingError> {
    let vectors = (0_u16..257)
        .map(|value| {
            vec![
                f32::from(value),
                f32::from(value % 11),
                f32::from(value % 7),
            ]
        })
        .collect::<Vec<_>>();
    let serial = deterministic_metric_clusters_with_workers(
        &vectors,
        13,
        16,
        0x5047_4354,
        context_core::DistanceMetric::L2,
        1,
    )?;
    for workers in [2, 3, 8, 512] {
        assert_eq!(
            deterministic_metric_clusters_with_workers(
                &vectors,
                13,
                16,
                0x5047_4354,
                context_core::DistanceMetric::L2,
                workers,
            )?,
            serial
        );
    }
    assert_eq!(
        deterministic_metric_clusters_with_workers(
            &vectors,
            13,
            16,
            0x5047_4354,
            context_core::DistanceMetric::L2,
            0,
        ),
        Err(TrainingError::InvalidWorkerCount)
    );
    Ok(())
}

#[test]
fn source_validation_reports_missing_keys_in_input_order() -> Result<(), Box<dyn Error>> {
    let rowset = OrderedRowset::new(vec![("b", 2), ("a", 1), ("c", 3)], 8)?;
    assert_eq!(
        rowset.validate_sources(|key| *key != "a" && *key != "c"),
        Err(RowsetError::MissingSource {
            ordinals: vec![1, 2],
        })
    );
    Ok(())
}

#[test]
fn spill_runs_are_budgeted_and_corruption_is_rejected() -> Result<(), Box<dyn Error>> {
    let budget = MemoryBudget::new(24).ok_or(SpillError::RecordExceedsBudget)?;
    let runs = SpillRun::partition(
        &[b"abcd".as_slice(), b"ef".as_slice(), b"ghij".as_slice()],
        budget,
    )?;
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].records()?, vec![b"abcd".to_vec(), b"ef".to_vec()]);

    let mut corrupt = runs[0].encoded().to_vec();
    corrupt[0] ^= 1;
    assert_eq!(
        SpillRun::from_encoded(corrupt),
        Err(SpillError::ChecksumMismatch)
    );
    Ok(())
}

#[test]
fn sampling_and_kmeans_are_seeded_and_deterministic() -> Result<(), TrainingError> {
    let vectors = vec![
        vec![0.0, 0.0],
        vec![0.0, 1.0],
        vec![10.0, 10.0],
        vec![10.0, 11.0],
    ];
    assert_eq!(
        deterministic_sample(vectors.len(), 3, 17),
        deterministic_sample(vectors.len(), 3, 17)
    );
    let first = deterministic_kmeans(&vectors, 2, 12, 17)?;
    let second = deterministic_kmeans(&vectors, 2, 12, 17)?;
    assert_eq!(first, second);
    assert_eq!(first.assignments().len(), vectors.len());
    Ok(())
}

#[test]
fn metric_training_preserves_binary_centroid_domains() -> Result<(), TrainingError> {
    let vectors = vec![
        vec![0.0, 0.0, 0.0, 0.0],
        vec![0.0, 0.0, 0.0, 1.0],
        vec![1.0, 1.0, 1.0, 0.0],
        vec![1.0, 1.0, 1.0, 1.0],
    ];
    for metric in [
        context_core::DistanceMetric::Hamming,
        context_core::DistanceMetric::Jaccard,
    ] {
        let trained = deterministic_metric_clusters(&vectors, 2, 12, 17, metric)?;
        assert!(
            trained
                .centroids()
                .iter()
                .flatten()
                .all(|value| *value == 0.0 || *value == 1.0)
        );
        assert_eq!(trained.assignments().len(), vectors.len());
    }
    Ok(())
}

#[test]
fn large_jaccard_medoid_training_is_bounded_and_deterministic() -> Result<(), TrainingError> {
    let vectors = (0_u16..4_096)
        .map(|value| {
            (0..32)
                .map(|bit| f32::from((value.rotate_left(bit % 16) & 1) != 0))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let serial = deterministic_metric_clusters_with_workers(
        &vectors,
        1,
        2,
        0x5047_4354,
        context_core::DistanceMetric::Jaccard,
        1,
    )?;
    let parallel = deterministic_metric_clusters_with_workers(
        &vectors,
        1,
        2,
        0x5047_4354,
        context_core::DistanceMetric::Jaccard,
        8,
    )?;
    assert_eq!(parallel, serial);
    Ok(())
}

#[test]
fn structural_findings_are_stable_and_redacted() -> Result<(), Box<dyn Error>> {
    let finding = StructuralFinding::new(
        FindingCode::ChecksumMismatch,
        FindingLocation::artifact(7, 3),
    );
    assert_eq!(finding.location().generation(), Some(7));
    assert_eq!(finding.severity(), FindingSeverity::Error);
    assert!(!finding.invariant().contains('/'));
    Ok(())
}

#[test]
fn spill_budget_counts_empty_record_frames_and_checksum() -> Result<(), Box<dyn Error>> {
    let budget = MemoryBudget::new(16).ok_or(SpillError::RecordExceedsBudget)?;
    let empty = b"".as_slice();
    let runs = SpillRun::partition(&[empty, empty, empty], budget)?;
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|run| run.encoded().len() <= budget.bytes()));
    assert_eq!(runs[0].records()?, vec![Vec::<u8>::new(), Vec::new()]);
    Ok(())
}

proptest! {
    #[test]
    fn arbitrary_spill_corruption_never_returns_partial_records(
        records in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..32), 1..24),
        selector in any::<usize>(),
        truncate in any::<bool>(),
    ) {
        let references = records.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let budget = MemoryBudget::new(1024)
            .ok_or_else(|| TestCaseError::fail("budget must be positive"))?;
        let runs = SpillRun::partition(&references, budget).map_err(|error| {
            TestCaseError::fail(error.to_string())
        })?;
        let mut encoded = runs[0].encoded().to_vec();
        if truncate {
            let new_len = selector % encoded.len();
            encoded.truncate(new_len);
        } else {
            let index = selector % encoded.len();
            encoded[index] ^= 1;
        }
        prop_assert!(SpillRun::from_encoded(encoded).is_err());
    }
}
