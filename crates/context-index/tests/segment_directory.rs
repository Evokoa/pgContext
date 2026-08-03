//! Property tests for bounded segmented-HNSW directory policy.

use context_index::{SegmentDirectory, SegmentDirectoryError, SegmentPolicy};
use proptest::prelude::*;

fn policy() -> Result<SegmentPolicy, SegmentDirectoryError> {
    SegmentPolicy::new(8, 4, 2, 3)
}

#[test]
fn compaction_publication_is_old_valid_or_new_valid() -> Result<(), SegmentDirectoryError> {
    let mut directory = SegmentDirectory::new(policy()?);
    directory.append_delta_rows(4)?;
    directory.rotate_delta(40)?;
    directory.append_delta_rows(4)?;
    directory.rotate_delta(50)?;
    let before = directory.clone();
    let plan = directory.prepare_compaction()?;

    assert_eq!(
        directory, before,
        "preparation cannot publish partial state"
    );
    let publication = directory.publish_compaction(&plan, plan.source_rows(), 70)?;
    assert_eq!(publication.retired.len(), 2);
    assert_eq!(directory.segments(), &[publication.published]);
    assert_eq!(
        directory.publish_compaction(&plan, plan.source_rows(), 70),
        Err(SegmentDirectoryError::StalePlan)
    );
    Ok(())
}

#[test]
fn pins_delay_retired_segment_reclamation() -> Result<(), SegmentDirectoryError> {
    let mut directory = SegmentDirectory::new(policy()?);
    for bytes in [40, 50] {
        directory.append_delta_rows(4)?;
        directory.rotate_delta(bytes)?;
    }
    let snapshot = directory.pin_snapshot();
    let plan = directory.prepare_compaction()?;
    directory.publish_compaction(&plan, plan.source_rows(), 70)?;
    assert!(directory.reclaimable_segments().is_empty());
    directory.unpin_snapshot(&snapshot)?;
    assert_eq!(directory.reclaimable_segments().len(), 2);
    Ok(())
}

proptest! {
    #[test]
    fn arbitrary_rotation_compaction_and_pin_sequences_remain_bounded(
        actions in prop::collection::vec(any::<u8>(), 0..256)
    ) {
        let policy = policy().map_err(|error| TestCaseError::fail(error.to_string()))?;
        let mut directory = SegmentDirectory::new(policy);
        let mut snapshots = Vec::new();
        for action in actions {
            match action % 6 {
                0 | 1 => {
                    let room = policy.delta_max_rows() - directory.delta_rows();
                    let _ = directory.append_delta_rows(u64::from(action).min(room));
                }
                2 => {
                    let _ = directory.rotate_delta(u64::from(action).saturating_add(1));
                }
                3 => {
                    if let Ok(plan) = directory.prepare_compaction() {
                        let _ = directory.publish_compaction(
                            &plan,
                            plan.source_rows(),
                            plan.source_bytes().max(1),
                        );
                    }
                }
                4 => snapshots.push(directory.pin_snapshot()),
                _ => {
                    if let Some(snapshot) = snapshots.pop() {
                        directory.unpin_snapshot(&snapshot)?;
                        let _ = directory.reclaimable_segments();
                    }
                }
            }
            prop_assert!(directory.segments().len() <= policy.max_segments());
            prop_assert!(directory.delta_rows() <= policy.delta_max_rows());
            let ids = directory
                .segments()
                .iter()
                .map(|segment| segment.id())
                .collect::<std::collections::BTreeSet<_>>();
            prop_assert_eq!(ids.len(), directory.segments().len());
        }
    }
}
