use proptest::prelude::*;

use super::{
    ArtifactKind, BuildError, BuildJobKind, BuildJobState, BuildJobStatus, WorkerId,
    build_contract_version,
};

#[test]
fn build_boundary_uses_logical_point_ids() {
    let point_id = super::PointId::new(11);
    assert_eq!(point_id.get(), 11);
    assert_eq!(build_contract_version(), 3);
}

proptest! {
    #[test]
    fn absolute_checkpoints_never_regress_or_exceed_total(
        total in 1_u64..128,
        first in 0_u64..128,
        replay in 0_u64..128,
    ) {
        prop_assume!(first <= total);
        let worker = WorkerId::new(1).ok_or(BuildError::ZeroIdentity)?;
        let state = BuildJobState::planned(
            BuildJobKind::ArtifactBuild,
            ArtifactKind::HnswSegment,
            total,
        ).claim(worker, 0, 10)?.checkpoint_to(first)?;
        if state.status() == BuildJobStatus::Running {
            let replayed = state.checkpoint_to(replay.min(first))?;
            prop_assert_eq!(replayed.checkpoint().processed_units(), first);
        }
    }
}
