//! Typed HNSW mutation, publication, and lock-order contracts.

#![allow(clippy::expect_used)]

use context_core::DenseVector;
use context_index::{
    CURRENT_GRAPH_LAYOUT_VERSION, GRAPH_PAGE_HEADER_BYTES, GRAPH_PAGE_MAGIC,
    GRAPH_PENDING_RESERVATION_BYTES, GRAPH_PENDING_RESERVATION_REGION_BYTES, GraphAllocationState,
    GraphAvailability, GraphDirectoryDepth, GraphDirectoryKeyKind, GraphFormatVersion,
    GraphInsertEvent, GraphInsertEventKind, GraphInsertPhase, GraphInsertPlan, GraphLayerCount,
    GraphLockPlan, GraphLockTarget, GraphMutationDescriptorEntry, GraphMutationDescriptorEntryKind,
    GraphMutationDescriptorTransition, GraphMutationError, GraphMutationId, GraphMutationStep,
    GraphNeighbors, GraphNodeReservation, GraphPageCodecError, GraphPageEnvelope, GraphPageId,
    GraphPageKind, GraphPublishedState, GraphRebuildReason, GraphRecordId, GraphRecordRevision,
    GraphRepairReason, HnswNodeId, LayerIndex, MAX_GRAPH_DIRECTORY_DEPTH, MAX_GRAPH_LAYERS,
    MAX_PENDING_GRAPH_MUTATIONS,
};

fn empty_published() -> GraphPublishedState {
    GraphPublishedState::empty(GraphFormatVersion::current())
}

fn mutation(value: u64) -> GraphMutationId {
    GraphMutationId::new(value).expect("mutation fixture id must be nonzero")
}

fn reservation(mutation_id: GraphMutationId, node_id: HnswNodeId) -> GraphNodeReservation {
    let mut allocator = GraphAllocationState::new(node_id);
    allocator
        .reserve(mutation_id, node_id)
        .expect("reservation fixture should succeed")
}

fn event(
    mutation_id: GraphMutationId,
    node_id: HnswNodeId,
    kind: GraphInsertEventKind,
) -> GraphInsertEvent {
    GraphInsertEvent::new(mutation_id, node_id, kind)
}

fn vector(values: &[f32]) -> DenseVector {
    DenseVector::new(values.to_vec()).expect("mutation vector fixture should be valid")
}

#[test]
fn insertion_publication_is_monotone_root_last_and_idempotent() {
    let mutation_id = mutation(7);
    let node_id = HnswNodeId::new(0);
    let mut plan = GraphInsertPlan::new(
        reservation(mutation_id, node_id),
        GraphRecordId::new(99),
        2,
        2,
        3,
        Some(node_id),
        empty_published(),
    )
    .expect("insert plan should be valid");

    assert_eq!(plan.phase(), GraphInsertPhase::Prepared);
    plan = plan
        .transition(event(
            mutation_id,
            node_id,
            GraphInsertEventKind::NodeAppended,
        ))
        .expect("append should advance");
    assert_eq!(plan.phase(), GraphInsertPhase::NodeAppended);
    assert_eq!(
        plan.clone()
            .transition(event(
                mutation_id,
                node_id,
                GraphInsertEventKind::NodeAppended
            ))
            .expect("append replay should be idempotent"),
        plan
    );

    for ordinal in 0..2 {
        plan = plan
            .transition(event(
                mutation_id,
                node_id,
                GraphInsertEventKind::OutboundLayerWritten { ordinal },
            ))
            .expect("outbound layer should advance");
    }
    for ordinal in 0..3 {
        plan = plan
            .transition(event(
                mutation_id,
                node_id,
                GraphInsertEventKind::RewireApplied { ordinal },
            ))
            .expect("rewire should advance");
    }
    plan = plan
        .transition(event(mutation_id, node_id, GraphInsertEventKind::NodeReady))
        .expect("ready should advance");
    assert_eq!(plan.phase(), GraphInsertPhase::ReadyToPublish);
    assert_eq!(plan.visible_state(), empty_published());

    let competing = plan.clone();
    plan = plan
        .transition(event(
            mutation_id,
            node_id,
            GraphInsertEventKind::Published {
                observed_state: empty_published(),
            },
        ))
        .expect("publication should advance");
    assert_eq!(plan.phase(), GraphInsertPhase::Published);
    let published = plan.visible_state();
    assert_eq!(published.generation(), 1);
    assert_eq!(published.node_count(), 1);
    assert_eq!(published.entry_point(), Some(node_id));
    assert_eq!(published.dimensions(), Some(2));
    assert_eq!(published.last_mutation_id(), Some(mutation_id));
    assert_eq!(
        plan.clone()
            .transition(event(
                mutation_id,
                node_id,
                GraphInsertEventKind::Published {
                    observed_state: published,
                },
            ))
            .expect("exact publication replay should be idempotent"),
        plan
    );
    assert_eq!(
        competing
            .transition(event(
                mutation_id,
                node_id,
                GraphInsertEventKind::Published {
                    observed_state: published,
                },
            ))
            .expect("reconstructed same-mutation target should be idempotent"),
        plan
    );

    let other_mutation = mutation(70);
    let mut other = GraphInsertPlan::new(
        reservation(other_mutation, node_id),
        GraphRecordId::new(100),
        2,
        2,
        3,
        Some(node_id),
        empty_published(),
    )
    .expect("competing plan should be valid");
    let mut events = vec![GraphInsertEventKind::NodeAppended];
    events.extend((0..2).map(|ordinal| GraphInsertEventKind::OutboundLayerWritten { ordinal }));
    events.extend((0..3).map(|ordinal| GraphInsertEventKind::RewireApplied { ordinal }));
    events.push(GraphInsertEventKind::NodeReady);
    for kind in events {
        other = other
            .transition(event(other_mutation, node_id, kind))
            .expect("competing plan prefix should advance");
    }
    assert_eq!(
        other.transition(event(
            other_mutation,
            node_id,
            GraphInsertEventKind::Published {
                observed_state: published,
            },
        )),
        Err(GraphMutationError::PublicationConflict {
            expected_generation: 0,
            expected_mutation_id: None,
            actual_generation: 1,
            actual_mutation_id: Some(mutation_id),
        })
    );
}

#[test]
fn every_unpublished_prefix_preserves_the_previous_reader_state() {
    let mutation_id = mutation(8);
    let node_id = HnswNodeId::new(4);
    let previous = GraphPublishedState::new(
        11,
        4,
        Some(HnswNodeId::new(1)),
        Some(3),
        GraphFormatVersion::current(),
        Some(mutation(80)),
    )
    .expect("published fixture should be valid");
    let mut plan = GraphInsertPlan::new(
        reservation(mutation_id, node_id),
        GraphRecordId::new(100),
        3,
        1,
        2,
        Some(node_id),
        previous,
    )
    .expect("insert plan should be valid");

    let events = [
        GraphInsertEventKind::NodeAppended,
        GraphInsertEventKind::OutboundLayerWritten { ordinal: 0 },
        GraphInsertEventKind::RewireApplied { ordinal: 0 },
        GraphInsertEventKind::RewireApplied { ordinal: 1 },
        GraphInsertEventKind::NodeReady,
    ];
    for kind in events {
        plan = plan
            .transition(event(mutation_id, node_id, kind))
            .expect("valid prefix should advance");
        assert_eq!(plan.visible_state(), previous);
    }
}

#[test]
fn insertion_rejects_skips_conflicts_and_wrong_identity_without_mutating() {
    let mutation_id = mutation(9);
    let node_id = HnswNodeId::new(0);
    let plan = GraphInsertPlan::new(
        reservation(mutation_id, node_id),
        GraphRecordId::new(1),
        2,
        1,
        1,
        Some(node_id),
        empty_published(),
    )
    .expect("insert plan should be valid");

    let cases = [
        event(
            mutation_id,
            node_id,
            GraphInsertEventKind::OutboundLayerWritten { ordinal: 0 },
        ),
        event(mutation(10), node_id, GraphInsertEventKind::NodeAppended),
        event(
            mutation_id,
            HnswNodeId::new(1),
            GraphInsertEventKind::NodeAppended,
        ),
        event(
            mutation_id,
            node_id,
            GraphInsertEventKind::Published {
                observed_state: empty_published(),
            },
        ),
    ];

    for invalid in cases {
        assert!(matches!(
            plan.clone().transition(invalid),
            Err(GraphMutationError::InvalidTransition { .. })
                | Err(GraphMutationError::MutationMismatch { .. })
                | Err(GraphMutationError::NodeMismatch { .. })
        ));
        assert_eq!(plan.phase(), GraphInsertPhase::Prepared);
        assert_eq!(plan.visible_state(), empty_published());
    }
}

#[test]
fn node_reservations_are_distinct_and_do_not_publish_counts() {
    assert_eq!(GraphMutationId::new(0), None);
    let mut allocator = GraphAllocationState::new(HnswNodeId::new(4));
    let stale_watermark = allocator.next_node_id();
    let first = allocator
        .reserve(mutation(1), stale_watermark)
        .expect("first reservation should succeed");
    let replay = allocator
        .reserve(mutation(1), stale_watermark)
        .expect("reservation replay should be idempotent");
    assert_eq!(replay, first);
    assert_eq!(
        allocator.reserve(mutation(2), stale_watermark),
        Err(GraphMutationError::AllocationConflict {
            expected: HnswNodeId::new(4),
            actual: HnswNodeId::new(5),
        })
    );
    let current_watermark = allocator.next_node_id();
    let second = allocator
        .reserve(mutation(2), current_watermark)
        .expect("second reservation should succeed");

    assert_eq!(first.node_id(), HnswNodeId::new(4));
    assert_eq!(second.node_id(), HnswNodeId::new(5));
    assert_eq!(allocator.next_node_id(), HnswNodeId::new(6));
    assert!(allocator.release(mutation(1)));
    assert!(!allocator.release(mutation(1)));
    assert_eq!(allocator.next_node_id(), HnswNodeId::new(6));
    assert_eq!(empty_published().node_count(), 0);
}

#[test]
fn interruption_prefixes_are_typed_repair_states() {
    let mutation_id = mutation(12);
    let node_id = HnswNodeId::new(0);
    let mut plan = GraphInsertPlan::new(
        reservation(mutation_id, node_id),
        GraphRecordId::new(1),
        2,
        2,
        1,
        Some(node_id),
        empty_published(),
    )
    .expect("insert plan should be valid");
    assert_eq!(plan.interruption_availability(), GraphAvailability::Ready);

    plan = plan
        .transition(event(
            mutation_id,
            node_id,
            GraphInsertEventKind::NodeAppended,
        ))
        .expect("append should advance");
    assert_eq!(
        plan.interruption_availability(),
        GraphAvailability::RepairRequired {
            mutation_id,
            reason: GraphRepairReason::InterruptedAppend,
        }
    );
    plan = plan
        .transition(event(
            mutation_id,
            node_id,
            GraphInsertEventKind::OutboundLayerWritten { ordinal: 0 },
        ))
        .expect("outbound layer should advance");
    assert_eq!(
        plan.interruption_availability(),
        GraphAvailability::RepairRequired {
            mutation_id,
            reason: GraphRepairReason::InterruptedOutbound,
        }
    );

    assert_eq!(
        GraphLayerCount::new(1).expect("one layer is valid").get(),
        1
    );
    assert!(GraphLayerCount::new(0).is_err());
    assert!(GraphLayerCount::new(MAX_GRAPH_LAYERS + 1).is_err());
}

#[test]
fn plans_reject_rootless_publication_and_zero_rewires_have_a_ready_phase() {
    let previous = GraphPublishedState::new(
        1,
        1,
        Some(HnswNodeId::new(0)),
        Some(2),
        GraphFormatVersion::current(),
        Some(mutation(81)),
    )
    .expect("published fixture should be valid");
    let rejected = GraphInsertPlan::new(
        reservation(mutation(13), HnswNodeId::new(1)),
        GraphRecordId::new(2),
        2,
        1,
        0,
        None,
        previous,
    );
    assert!(matches!(
        rejected,
        Err(GraphMutationError::InvalidPlan { .. })
    ));

    let mutation_id = mutation(14);
    let node_id = HnswNodeId::new(0);
    let mut plan = GraphInsertPlan::new(
        reservation(mutation_id, node_id),
        GraphRecordId::new(1),
        2,
        1,
        0,
        Some(node_id),
        empty_published(),
    )
    .expect("zero-rewire plan should be valid");
    plan = plan
        .transition(event(
            mutation_id,
            node_id,
            GraphInsertEventKind::NodeAppended,
        ))
        .expect("append should advance");
    plan = plan
        .transition(event(
            mutation_id,
            node_id,
            GraphInsertEventKind::OutboundLayerWritten { ordinal: 0 },
        ))
        .expect("outbound layer should advance");
    assert_eq!(plan.phase(), GraphInsertPhase::ReadyToMarkNode);
    assert_eq!(
        plan.interruption_availability(),
        GraphAvailability::RepairRequired {
            mutation_id,
            reason: GraphRepairReason::InterruptedNodePublication,
        }
    );
}

#[test]
fn semantic_steps_are_bound_to_the_plan_reservation_and_publication()
-> Result<(), Box<dyn std::error::Error>> {
    let mutation_id = mutation(15);
    let node_id = HnswNodeId::new(1);
    let previous = GraphPublishedState::new(
        4,
        1,
        Some(HnswNodeId::new(0)),
        Some(2),
        GraphFormatVersion::current(),
        Some(mutation(82)),
    )
    .expect("published fixture should be valid");
    let mut plan = GraphInsertPlan::new(
        reservation(mutation_id, node_id),
        GraphRecordId::new(20),
        2,
        1,
        1,
        Some(node_id),
        previous,
    )
    .expect("insert plan should be valid");

    let append = plan
        .append_step(
            vector(&[1.0, 0.0]),
            GraphLayerCount::new(1).expect("one layer should be valid"),
        )
        .expect("append step should match the plan");
    let GraphMutationStep::AppendUnpublishedNode(append) = append else {
        return Err("expected append step".into());
    };
    assert_eq!(append.reservation().mutation_id(), mutation_id);
    assert_eq!(append.reservation().node_id(), node_id);
    assert_eq!(append.target_generation(), 5);

    let wrong_node = GraphNeighbors::new(2, HnswNodeId::new(0), LayerIndex::base(), vec![node_id])
        .expect("existing-node adjacency should be valid");
    assert!(matches!(
        plan.outbound_step(wrong_node.clone()),
        Err(GraphMutationError::NodeMismatch { .. })
    ));
    let outbound = GraphNeighbors::new(2, node_id, LayerIndex::base(), vec![HnswNodeId::new(0)])
        .expect("new-node adjacency should be valid");
    let GraphMutationStep::WriteOutboundLayer(outbound_step) = plan
        .outbound_step(outbound)
        .expect("outbound step should match the reservation")
    else {
        return Err("expected outbound step".into());
    };
    assert_eq!(outbound_step.adjacency().node_id(), node_id);
    assert_eq!(outbound_step.target_generation(), 5);
    let GraphMutationStep::ReplaceNeighbors(rewire_step) = plan
        .rewire_step(wrong_node, GraphRecordRevision::new(3))
        .expect("existing-node rewire should be valid")
    else {
        return Err("expected rewire step".into());
    };
    assert_eq!(
        rewire_step.expected_revision(),
        Some(GraphRecordRevision::new(3))
    );
    assert_eq!(rewire_step.target_generation(), 5);
    let missing_inserted_node =
        GraphNeighbors::new(2, HnswNodeId::new(0), LayerIndex::base(), vec![])
            .expect("empty existing adjacency should be valid");
    assert!(matches!(
        plan.rewire_step(missing_inserted_node, GraphRecordRevision::new(3)),
        Err(GraphMutationError::InvalidPlan { .. })
    ));
    assert!(plan.publication_step().is_err());

    for kind in [
        GraphInsertEventKind::NodeAppended,
        GraphInsertEventKind::OutboundLayerWritten { ordinal: 0 },
        GraphInsertEventKind::RewireApplied { ordinal: 0 },
        GraphInsertEventKind::NodeReady,
    ] {
        plan = plan
            .transition(event(mutation_id, node_id, kind))
            .expect("valid step event should advance");
    }
    let GraphMutationStep::Publish(publication) = plan
        .publication_step()
        .expect("publication should now be ready")
    else {
        return Err("expected publication step".into());
    };
    assert_eq!(publication.mutation_id(), mutation_id);
    assert_eq!(publication.expected_state(), previous);
    assert_eq!(publication.new_state().generation(), 5);
    Ok(())
}

#[test]
fn mutation_descriptors_persist_complete_progress_state_and_repair_entries()
-> Result<(), Box<dyn std::error::Error>> {
    let mutation_id = mutation(16);
    let node_id = HnswNodeId::new(1);
    let previous = GraphPublishedState::new(
        4,
        1,
        Some(HnswNodeId::new(0)),
        Some(2),
        GraphFormatVersion::current(),
        Some(mutation(83)),
    )?;
    let mut plan = GraphInsertPlan::new(
        reservation(mutation_id, node_id),
        GraphRecordId::new(21),
        2,
        1,
        1,
        Some(node_id),
        previous,
    )?;
    let descriptor = plan.descriptor(GraphRecordRevision::new(0))?;
    assert_eq!(descriptor.reservation().mutation_id(), mutation_id);
    assert_eq!(descriptor.record_id(), GraphRecordId::new(21));
    assert_eq!(
        descriptor.descriptor_revision(),
        GraphRecordRevision::new(0)
    );
    assert_eq!(descriptor.phase(), GraphInsertPhase::Prepared);
    assert_eq!(descriptor.outbound_progress(), (0, 1));
    assert_eq!(descriptor.rewire_progress(), (0, 1));
    assert_eq!(descriptor.expected_state(), previous);
    assert_eq!(descriptor.target_state().generation(), 5);
    assert_eq!(
        descriptor.target_state().last_mutation_id(),
        Some(mutation_id)
    );
    assert_eq!(
        GraphMutationDescriptorTransition::create(descriptor)?.target(),
        descriptor
    );

    let outbound = GraphNeighbors::new(2, node_id, LayerIndex::base(), vec![HnswNodeId::new(0)])?;
    let GraphMutationStep::WriteOutboundLayer(outbound_step) = plan.outbound_step(outbound)? else {
        return Err("expected outbound step".into());
    };
    let outbound_entry = GraphMutationDescriptorEntry::outbound(outbound_step)?;
    assert_eq!(
        outbound_entry.kind(),
        GraphMutationDescriptorEntryKind::OutboundLayer
    );
    assert_eq!(outbound_entry.kind().code(), 1);
    assert_eq!(outbound_entry.ordinal(), 0);
    assert_eq!(outbound_entry.step().expected_revision(), None);

    let rewire = GraphNeighbors::new(2, HnswNodeId::new(0), LayerIndex::base(), vec![node_id])?;
    let GraphMutationStep::ReplaceNeighbors(rewire_step) =
        plan.rewire_step(rewire, GraphRecordRevision::new(3))?
    else {
        return Err("expected rewire step".into());
    };
    let rewire_entry = GraphMutationDescriptorEntry::rewire(rewire_step, 0)?;
    assert_eq!(
        rewire_entry.kind(),
        GraphMutationDescriptorEntryKind::NeighborRewire
    );
    assert_eq!(rewire_entry.kind().code(), 2);
    assert_eq!(
        rewire_entry.step().expected_revision(),
        Some(GraphRecordRevision::new(3))
    );

    for kind in [
        GraphInsertEventKind::NodeAppended,
        GraphInsertEventKind::OutboundLayerWritten { ordinal: 0 },
        GraphInsertEventKind::RewireApplied { ordinal: 0 },
    ] {
        plan = plan.transition(event(mutation_id, node_id, kind))?;
    }
    let progressed = plan.descriptor(GraphRecordRevision::new(1))?;
    assert_eq!(progressed.phase(), GraphInsertPhase::ReadyToMarkNode);
    assert_eq!(progressed.phase().code(), 4);
    assert_eq!(progressed.outbound_progress(), (1, 1));
    assert_eq!(progressed.rewire_progress(), (1, 1));
    let transition = GraphMutationDescriptorTransition::advance(descriptor, progressed)?;
    assert_eq!(transition.expected(), Some(descriptor));
    assert_eq!(transition.target(), progressed);
    Ok(())
}

#[test]
fn page_layout_versions_and_kinds_fail_closed() {
    assert_eq!(GRAPH_PAGE_MAGIC, *b"PGH2");
    assert_eq!(GRAPH_PAGE_HEADER_BYTES, 32);
    assert_eq!(MAX_GRAPH_DIRECTORY_DEPTH, 8);
    assert_eq!(
        GraphDirectoryDepth::new(MAX_GRAPH_DIRECTORY_DEPTH)
            .expect("maximum directory depth should be valid")
            .get(),
        MAX_GRAPH_DIRECTORY_DEPTH
    );
    assert_eq!(
        GraphDirectoryDepth::new(MAX_GRAPH_DIRECTORY_DEPTH + 1),
        Err(GraphRebuildReason::DirectoryDepthExceeded)
    );
    assert_eq!(
        GraphFormatVersion::current().get(),
        CURRENT_GRAPH_LAYOUT_VERSION
    );
    assert!(GraphFormatVersion::classify(0).is_rebuild_required());
    assert!(GraphFormatVersion::classify(1).is_rebuild_required());
    assert!(GraphFormatVersion::classify(CURRENT_GRAPH_LAYOUT_VERSION + 1).is_rebuild_required());
    assert!(!GraphFormatVersion::classify(CURRENT_GRAPH_LAYOUT_VERSION).is_rebuild_required());

    assert_eq!(
        GraphPageKind::ALL,
        [
            GraphPageKind::Meta,
            GraphPageKind::Directory,
            GraphPageKind::Node,
            GraphPageKind::Adjacency,
            GraphPageKind::MutationDescriptor,
            GraphPageKind::Delta,
        ]
    );
    assert_eq!(
        GraphPageKind::ALL.map(GraphPageKind::code),
        [1, 2, 3, 4, 5, 6]
    );
    assert_eq!(
        GraphDirectoryKeyKind::ALL.map(GraphDirectoryKeyKind::code),
        [1, 2, 3, 4]
    );
    assert_eq!(MAX_PENDING_GRAPH_MUTATIONS, 128);
    assert_eq!(GRAPH_PENDING_RESERVATION_BYTES, 16);
    assert_eq!(GRAPH_PENDING_RESERVATION_REGION_BYTES, 2_048);
}

#[test]
fn lock_order_separates_extension_meta_and_sorted_data_pages() {
    assert!(GraphLockPlan::new(vec![GraphLockTarget::Extension]).is_ok());
    assert!(GraphLockPlan::new(vec![GraphLockTarget::Allocator]).is_ok());
    assert!(GraphLockPlan::new(vec![GraphLockTarget::Meta]).is_ok());
    assert!(
        GraphLockPlan::new(vec![
            GraphLockTarget::Data(GraphPageId::new(2)),
            GraphLockTarget::Data(GraphPageId::new(9)),
        ])
        .is_ok()
    );

    let rejected = [
        vec![
            GraphLockTarget::Extension,
            GraphLockTarget::Data(GraphPageId::new(1)),
        ],
        vec![
            GraphLockTarget::Meta,
            GraphLockTarget::Data(GraphPageId::new(1)),
        ],
        vec![
            GraphLockTarget::Allocator,
            GraphLockTarget::Data(GraphPageId::new(1)),
        ],
        vec![
            GraphLockTarget::Data(GraphPageId::new(9)),
            GraphLockTarget::Data(GraphPageId::new(2)),
        ],
        vec![
            GraphLockTarget::Data(GraphPageId::new(2)),
            GraphLockTarget::Data(GraphPageId::new(2)),
        ],
    ];
    for targets in rejected {
        assert!(matches!(
            GraphLockPlan::new(targets),
            Err(GraphMutationError::InvalidLockOrder { .. })
        ));
    }
}

#[test]
fn data_page_envelopes_round_trip_every_non_meta_page_kind() {
    // Driven by `GraphPageKind::ALL` rather than a hand-listed set: the
    // decoder previously enumerated roles individually and rejected `Delta`
    // as corrupt once the segmented write path started appending those pages.
    for kind in GraphPageKind::ALL {
        if kind == GraphPageKind::Meta {
            // Meta is a metadata page, never a data page; `new` must refuse it.
            assert!(matches!(
                GraphPageEnvelope::new(kind, 7, 3),
                Err(GraphPageCodecError::Corrupt { .. })
            ));
            continue;
        }

        let envelope = GraphPageEnvelope::new(kind, 7, 3).expect("data-page envelope");
        let bytes = envelope.encode().expect("encode data-page envelope");
        let decoded = GraphPageEnvelope::decode(&bytes).expect("decode data-page envelope");

        assert_eq!(decoded, envelope);
        assert_eq!(decoded.kind(), kind);
        assert_eq!(decoded.generation(), 7);
        assert_eq!(decoded.page_id(), 3);
    }
}

#[test]
fn data_page_envelope_rejects_a_kind_code_outside_the_enum() {
    let bytes = GraphPageEnvelope::new(GraphPageKind::Node, 7, 3)
        .expect("data-page envelope")
        .encode()
        .expect("encode data-page envelope");

    let unknown_code = GraphPageKind::ALL
        .into_iter()
        .map(GraphPageKind::code)
        .max()
        .expect("page kinds are non-empty")
        + 1;
    let mut unknown_kind = bytes;
    unknown_kind[6] = unknown_code;

    assert!(matches!(
        GraphPageEnvelope::decode(&unknown_kind),
        Err(GraphPageCodecError::Corrupt {
            reason: "data page has invalid kind"
        })
    ));
}
