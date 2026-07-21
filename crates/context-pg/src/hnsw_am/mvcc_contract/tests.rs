fn binding(
    node: usize,
    record: u64,
    block: u32,
    offset: u16,
    generation: u64,
    revision: u64,
) -> HnswSourceBinding {
    HnswSourceBinding::new(
        HnswNodeId::new(node),
        GraphRecordId::new(record),
        HnswHeapTid::new(block, offset).expect("fixture TID is valid"),
        generation,
        GraphRecordRevision::new(revision),
    )
    .expect("fixture binding is valid")
}

#[test]
fn physical_heap_tid_is_validated_and_distinct_from_other_identities() {
    let tid = HnswHeapTid::new(42, 7).expect("nonzero offset is valid");

    assert_eq!(tid.block(), 42);
    assert_eq!(tid.offset(), 7);
    assert!(matches!(
        HnswHeapTid::new(42, 0),
        Err(HnswMvccError::InvalidHeapTid)
    ));
    assert!(matches!(
        HnswHeapTid::new(u32::MAX, 1),
        Err(HnswMvccError::InvalidHeapTid)
    ));
    assert_ne!(tid.block() as u64, PointId::new(9001).get());
    assert_ne!(tid.block() as u64, GraphRecordId::new(7001).get());
    assert_ne!(tid.block() as usize, HnswNodeId::new(3).get());
}

#[test]
fn aborted_and_unpublished_remnants_never_become_result_candidates() {
    let source = binding(1, 11, 5, 2, 8, 3);
    let visible = HnswVisibleHeapRow::new(
        source.heap_tid(),
        HnswExactScore::new(0.25).expect("finite exact score"),
    );

    assert_eq!(
        recheck_ordered_candidate(HnswNodeMvccState::Unpublished(source), Some(visible)),
        Ok(HnswCandidateDisposition::Ignore)
    );
    assert_eq!(
        recheck_ordered_candidate(HnswNodeMvccState::Ready(source), None),
        Ok(HnswCandidateDisposition::ConnectorOnly)
    );
    assert_eq!(
        recheck_ordered_candidate(HnswNodeMvccState::Ready(source), Some(visible)),
        Ok(HnswCandidateDisposition::Eligible(HnswOrderedCandidate::new(
            source.heap_tid(),
            visible.exact_score(),
        )))
    );
}

#[test]
fn exact_recheck_rejects_nonfinite_scores_and_wrong_heap_rows() {
    let source = binding(1, 11, 5, 2, 8, 3);
    assert!(matches!(
        HnswExactScore::new(f64::NAN),
        Err(HnswMvccError::NonFiniteExactScore)
    ));
    let wrong_row = HnswVisibleHeapRow::new(
        HnswHeapTid::new(5, 3).expect("fixture TID is valid"),
        HnswExactScore::new(0.1).expect("finite exact score"),
    );

    assert!(matches!(
        recheck_ordered_candidate(HnswNodeMvccState::Ready(source), Some(wrong_row)),
        Err(HnswMvccError::SourceTidMismatch { .. })
    ));
}

#[test]
fn logical_point_identity_is_supplied_by_authoritative_source_recheck() {
    let source = binding(2, 22, 9, 4, 12, 1);
    let point_id = PointId::new(99_001);
    let row = HnswVisiblePointRow::new(
        source.heap_tid(),
        point_id,
        HnswExactScore::new(-0.75).expect("finite exact score"),
    );

    assert_eq!(
        recheck_logical_candidate(HnswNodeMvccState::Ready(source), Some(row)),
        Ok(HnswCandidateDisposition::Eligible(HnswLogicalCandidate::new(
            source.heap_tid(),
            point_id,
            row.exact_score(),
        )))
    );
    assert_ne!(point_id.get(), u64::from(source.heap_tid().offset()));
}

#[test]
fn vacuum_callback_authority_produces_exact_idempotent_tombstones() {
    let source = binding(3, 33, 10, 5, 21, 7);
    let ready = HnswNodeMvccState::Ready(source);
    let dead = HnswVacuumDeadTid::from_callback(source.heap_tid());
    let epoch = HnswTombstoneEpoch::new(44).expect("nonzero epoch");
    let transition = HnswTombstoneTransition::plan(dead, ready, epoch)
        .expect("callback-confirmed ready node can be tombstoned");

    assert_eq!(transition.expected(), ready);
    assert_eq!(transition.target().binding().record_revision().get(), 8);
    assert_eq!(transition.target().tombstone_epoch(), Some(epoch));
    assert_eq!(
        transition.classify(ready),
        HnswTombstoneApply::Apply
    );
    assert_eq!(
        transition.classify(transition.target()),
        HnswTombstoneApply::AlreadyApplied
    );
    assert_eq!(
        transition.classify(HnswNodeMvccState::Ready(binding(3, 33, 10, 5, 21, 9))),
        HnswTombstoneApply::Conflict
    );
}

#[test]
fn graph_tombstone_step_binds_exact_physical_identity_and_revision() {
    let graph_mutation = context_index::GraphMutationId::new(51).expect("nonzero mutation id");
    let previous = context_index::GraphPublishedState::new(
        50,
        4,
        Some(HnswNodeId::new(0)),
        Some(3),
        context_index::GraphFormatVersion::current(),
        context_index::GraphMutationId::new(50),
    )
    .expect("published fixture is valid");
    let graph_plan = context_index::GraphTombstonePlan::new(
        graph_mutation,
        HnswNodeId::new(2),
        GraphRecordId::new(500),
        GraphRecordRevision::new(7),
        previous,
    )
    .expect("graph tombstone fixture is valid");
    let source = binding(2, 500, 20, 9, 12, 7);
    let epoch = HnswTombstoneEpoch::new(51).expect("nonzero epoch");
    let transition = HnswTombstoneTransition::plan(
        HnswVacuumDeadTid::from_callback(source.heap_tid()),
        HnswNodeMvccState::Ready(source),
        epoch,
    )
    .expect("physical tombstone fixture is valid");
    let bound = transition
        .bind_graph_step(graph_plan.store_step())
        .expect("matching physical and graph identities bind");

    assert_eq!(bound.step(), graph_plan.store_step());
    assert_eq!(bound.heap_tid(), source.heap_tid());
    assert_eq!(bound.tombstone_epoch(), epoch);

    let wrong_record = context_index::GraphTombstonePlan::new(
        graph_mutation,
        HnswNodeId::new(2),
        GraphRecordId::new(501),
        GraphRecordRevision::new(7),
        previous,
    )
    .expect("alternate graph step is valid")
    .store_step();
    assert_eq!(
        transition.bind_graph_step(wrong_record),
        Err(HnswMvccError::GraphTombstoneMismatch)
    );
}

#[test]
fn vacuum_never_tombstones_without_callback_confirmation_or_matching_tid() {
    let source = binding(3, 33, 10, 5, 21, 7);
    let wrong_dead = HnswVacuumDeadTid::from_callback(
        HnswHeapTid::new(10, 6).expect("fixture TID is valid"),
    );
    let epoch = HnswTombstoneEpoch::new(44).expect("nonzero epoch");

    assert!(matches!(
        HnswTombstoneTransition::plan(wrong_dead, HnswNodeMvccState::Ready(source), epoch),
        Err(HnswMvccError::VacuumTidMismatch { .. })
    ));
    assert!(matches!(
        HnswTombstoneTransition::plan(
            HnswVacuumDeadTid::from_callback(source.heap_tid()),
            HnswNodeMvccState::Unpublished(source),
            epoch,
        ),
        Err(HnswMvccError::NodeNotReadyForTombstone)
    ));
}

#[test]
fn vacuum_callback_false_keeps_nodes_and_true_is_idempotent_for_tombstones() {
    let source = binding(8, 88, 14, 8, 40, 1);
    let epoch = HnswTombstoneEpoch::new(41).expect("nonzero epoch");
    let keep = HnswVacuumCallbackDecision::from_callback(source.heap_tid(), false);
    assert_eq!(
        HnswVacuumAction::plan(keep, HnswNodeMvccState::Ready(source), epoch),
        Ok(HnswVacuumAction::Keep)
    );

    let dead = HnswVacuumCallbackDecision::from_callback(source.heap_tid(), true);
    let HnswVacuumAction::Tombstone(transition) =
        HnswVacuumAction::plan(dead, HnswNodeMvccState::Ready(source), epoch)
            .expect("callback true plans a tombstone")
    else {
        panic!("ready callback-true node must plan a tombstone");
    };
    assert_eq!(
        HnswVacuumAction::plan(dead, transition.target(), epoch),
        Ok(HnswVacuumAction::AlreadyTombstoned)
    );
}

#[test]
fn tid_reuse_requires_every_old_binding_to_be_tombstoned() {
    let old = binding(4, 44, 12, 6, 30, 2);
    let dead = HnswVacuumDeadTid::from_callback(old.heap_tid());
    let epoch = HnswTombstoneEpoch::new(31).expect("nonzero epoch");
    let tombstone = HnswTombstoneTransition::plan(
        dead,
        HnswNodeMvccState::Ready(old),
        epoch,
    )
    .expect("old node can be tombstoned")
    .target();

    assert!(matches!(
        validate_tid_reuse(old.heap_tid(), &[HnswNodeMvccState::Ready(old)]),
        Err(HnswMvccError::TidReuseUnsafe { .. })
    ));
    assert_eq!(validate_tid_reuse(old.heap_tid(), &[tombstone]), Ok(()));

    let replacement = binding(5, 55, 12, 6, 32, 0);
    let row = HnswVisiblePointRow::new(
        replacement.heap_tid(),
        PointId::new(500),
        HnswExactScore::new(0.2).expect("finite exact score"),
    );
    assert_eq!(
        recheck_logical_candidate(tombstone, Some(row)),
        Ok(HnswCandidateDisposition::ConnectorOnly)
    );
    assert!(matches!(
        recheck_logical_candidate(HnswNodeMvccState::Ready(replacement), Some(row)),
        Ok(HnswCandidateDisposition::Eligible(_))
    ));
}

#[test]
fn vacuum_callback_batches_are_fixed_capacity_and_deduplicate_tids() {
    let mut batch = HnswVacuumBatch::new();
    let first = HnswVacuumDeadTid::from_callback(
        HnswHeapTid::new(1, 1).expect("fixture TID is valid"),
    );
    assert_eq!(batch.record(first), Ok(true));
    assert_eq!(batch.record(first), Ok(false));

    let max_offset = u16::try_from(MAX_HNSW_VACUUM_CALLBACK_BATCH)
        .unwrap_or_else(|_| panic!("vacuum callback batch must fit a heap offset"));
    for offset in 2..=max_offset {
        let dead = HnswVacuumDeadTid::from_callback(
            HnswHeapTid::new(1, offset).expect("fixture TID is valid"),
        );
        assert_eq!(batch.record(dead), Ok(true));
    }

    assert_eq!(batch.len(), MAX_HNSW_VACUUM_CALLBACK_BATCH);
    assert!(batch.is_full());
    assert!(matches!(
        batch.record(HnswVacuumDeadTid::from_callback(
            HnswHeapTid::new(2, 1).expect("fixture TID is valid")
        )),
        Err(HnswMvccError::VacuumBatchFull { .. })
    ));

    let apply = batch.finish_callback_phase();
    assert_eq!(apply.len(), MAX_HNSW_VACUUM_CALLBACK_BATCH);
    assert_eq!(apply.iter().next(), Some(first));
}
