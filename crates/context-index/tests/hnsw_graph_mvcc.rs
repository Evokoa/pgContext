//! HNSW tombstone generation, publication, and replay contracts.

#![allow(clippy::expect_used)]

use context_index::{
    GraphAllocationState, GraphFormatVersion, GraphInsertEvent, GraphInsertEventKind,
    GraphInsertPlan, GraphMutationId, GraphMvccError, GraphNodePublication, GraphNodeUse,
    GraphPublishedState, GraphRecordId, GraphRecordRevision, GraphTombstonePlan, HnswNodeId,
};

fn mutation(value: u64) -> GraphMutationId {
    GraphMutationId::new(value).expect("fixture mutation id is nonzero")
}

fn published(
    generation: u64,
    nodes: usize,
    tombstones: usize,
    last_mutation: u64,
) -> GraphPublishedState {
    GraphPublishedState::new_with_tombstones(
        generation,
        nodes,
        tombstones,
        Some(HnswNodeId::new(0)),
        Some(3),
        GraphFormatVersion::current(),
        Some(mutation(last_mutation)),
    )
    .expect("fixture published state is valid")
}

#[test]
fn node_publication_states_separate_topology_from_answer_eligibility() {
    assert_eq!(GraphNodePublication::Unpublished.code(), 0);
    assert_eq!(GraphNodePublication::Ready.code(), 1);
    assert_eq!(GraphNodePublication::Tombstoned.code(), 2);
    assert_eq!(
        GraphNodePublication::Unpublished.node_use(),
        GraphNodeUse::Ignore
    );
    assert_eq!(
        GraphNodePublication::Ready.node_use(),
        GraphNodeUse::TraverseAndRecheck
    );
    assert_eq!(
        GraphNodePublication::Tombstoned.node_use(),
        GraphNodeUse::TraverseOnly
    );
}

#[test]
fn published_state_tracks_structural_and_candidate_counts_separately() {
    let state = published(7, 5, 2, 70);

    assert_eq!(state.node_count(), 5);
    assert_eq!(state.tombstone_count(), 2);
    assert_eq!(state.candidate_node_count(), 3);
    assert!(
        GraphPublishedState::new_with_tombstones(
            7,
            5,
            6,
            Some(HnswNodeId::new(0)),
            Some(3),
            GraphFormatVersion::current(),
            Some(mutation(70)),
        )
        .is_err()
    );
}

#[test]
fn tombstone_storage_preserves_old_generation_until_publication() {
    let previous = published(7, 5, 1, 70);
    let plan = GraphTombstonePlan::new(
        mutation(71),
        HnswNodeId::new(0),
        GraphRecordId::new(900),
        GraphRecordRevision::new(4),
        previous,
    )
    .expect("valid tombstone plan");
    let step = plan.store_step();

    assert!(matches!(
        plan.publication_step(),
        Err(GraphMvccError::InvalidTransition { .. })
    ));

    assert_eq!(step.target_generation(), 8);
    assert_eq!(step.expected_revision(), GraphRecordRevision::new(4));
    assert_eq!(step.target_revision(), GraphRecordRevision::new(5));
    assert_eq!(plan.visible_state(), previous);

    let stored = plan
        .record_store_finished(step)
        .expect("exact store completion applies");
    let publication = stored
        .publication_step()
        .expect("stored plan exposes publication step");
    assert_eq!(publication.tombstone(), step);
    assert_eq!(publication.expected_state(), previous);
    assert_eq!(publication.target_state(), stored.target_state());
    assert_eq!(stored.visible_state(), previous);
    assert!(!stored.is_complete());

    let target = stored.target_state();
    let published = stored
        .record_publication_finished(target)
        .expect("target metadata publication applies");
    assert_eq!(published.visible_state(), target);
    assert!(published.is_complete());
    assert_eq!(target.generation(), 8);
    assert_eq!(target.node_count(), 5);
    assert_eq!(target.tombstone_count(), 2);
    assert_eq!(target.entry_point(), Some(HnswNodeId::new(0)));
    assert_eq!(target.last_mutation_id(), Some(mutation(71)));
}

#[test]
fn tombstone_store_and_publication_replay_are_idempotent() {
    let previous = published(9, 4, 0, 90);
    let plan = GraphTombstonePlan::new(
        mutation(91),
        HnswNodeId::new(2),
        GraphRecordId::new(901),
        GraphRecordRevision::new(1),
        previous,
    )
    .expect("valid tombstone plan");
    let step = plan.store_step();
    let plan = plan
        .record_store_finished(step)
        .and_then(|plan| plan.record_store_finished(step))
        .expect("exact store replay is idempotent");
    let target = plan.target_state();
    let plan = plan
        .record_publication_finished(target)
        .and_then(|plan| plan.record_publication_finished(target))
        .expect("exact publication replay is idempotent");

    assert!(plan.is_complete());
    assert_eq!(plan.visible_state(), target);
}

#[test]
fn tombstone_conflicts_on_revision_record_and_published_state_drift() {
    let previous = published(11, 6, 1, 110);
    let plan = GraphTombstonePlan::new(
        mutation(111),
        HnswNodeId::new(3),
        GraphRecordId::new(1000),
        GraphRecordRevision::new(8),
        previous,
    )
    .expect("valid tombstone plan");
    let stale = GraphTombstonePlan::new(
        mutation(111),
        HnswNodeId::new(3),
        GraphRecordId::new(1001),
        GraphRecordRevision::new(7),
        previous,
    )
    .expect("alternate exact step is valid")
    .store_step();
    assert!(matches!(
        plan.clone().record_store_finished(stale),
        Err(GraphMvccError::StoreConflict)
    ));

    let step = plan.store_step();
    let stored = plan
        .record_store_finished(step)
        .expect("exact store applies");
    let drift = published(12, 6, 2, 999);
    assert!(matches!(
        stored.record_publication_finished(drift),
        Err(GraphMvccError::PublicationConflict { .. })
    ));
}

#[test]
fn tombstone_plan_rejects_empty_invalid_or_exhausted_state() {
    let empty = GraphPublishedState::empty(GraphFormatVersion::current());
    assert!(matches!(
        GraphTombstonePlan::new(
            mutation(1),
            HnswNodeId::new(0),
            GraphRecordId::new(1),
            GraphRecordRevision::new(0),
            empty,
        ),
        Err(GraphMvccError::InvalidPlan { .. })
    ));

    let all_tombstoned = published(3, 2, 2, 30);
    assert!(matches!(
        GraphTombstonePlan::new(
            mutation(31),
            HnswNodeId::new(1),
            GraphRecordId::new(2),
            GraphRecordRevision::new(0),
            all_tombstoned,
        ),
        Err(GraphMvccError::InvalidPlan { .. })
    ));
}

#[test]
fn insertion_preserves_existing_tombstone_count() {
    let previous = published(7, 3, 1, 70);
    let insert_mutation = mutation(71);
    let node_id = HnswNodeId::new(3);
    let mut allocator = GraphAllocationState::new(node_id);
    let reservation = allocator
        .reserve(insert_mutation, node_id)
        .expect("fixture reservation succeeds");
    let mut plan = GraphInsertPlan::new(
        reservation,
        GraphRecordId::new(901),
        3,
        1,
        0,
        previous.entry_point(),
        previous,
    )
    .expect("insertion beside a tombstone is valid");

    for kind in [
        GraphInsertEventKind::NodeAppended,
        GraphInsertEventKind::OutboundLayerWritten { ordinal: 0 },
        GraphInsertEventKind::NodeReady,
    ] {
        plan = plan
            .transition(GraphInsertEvent::new(insert_mutation, node_id, kind))
            .expect("insertion prefix advances");
    }
    plan = plan
        .transition(GraphInsertEvent::new(
            insert_mutation,
            node_id,
            GraphInsertEventKind::Published {
                observed_state: previous,
            },
        ))
        .expect("insertion publishes");

    assert_eq!(plan.visible_state().node_count(), 4);
    assert_eq!(plan.visible_state().tombstone_count(), 1);
    assert_eq!(plan.visible_state().candidate_node_count(), 3);
}
