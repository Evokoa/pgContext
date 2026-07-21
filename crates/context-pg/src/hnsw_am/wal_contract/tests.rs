#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use context_core::DenseVector;
    use context_index::{
        GraphAllocationState, GraphFormatVersion, GraphInsertEvent, GraphInsertEventKind,
        GraphInsertPlan, GraphLayerCount, GraphMutationDescriptor, GraphMutationDescriptorEntry,
        GraphMutationDescriptorTransition, GraphMutationId, GraphMutationStep, GraphNeighbors,
        GraphNodeReservation, GraphPageId, GraphPageKind, GraphPublishedState, GraphRecordId,
        GraphRecordRevision, GraphTombstonePlan, HnswNodeId, LayerIndex,
    };

    use super::*;
    use crate::hnsw_am::mvcc_contract::{
        HnswHeapTid, HnswNodeMvccState, HnswSourceBinding, HnswTombstoneEpoch,
        HnswTombstoneTransition, HnswVacuumDeadTid,
    };

    fn mutation(value: u64) -> GraphMutationId {
        GraphMutationId::new(value).expect("fixture mutation id must be nonzero")
    }

    fn reservation() -> GraphNodeReservation {
        let mutation_id = mutation(41);
        let node_id = HnswNodeId::new(2);
        let mut allocator = GraphAllocationState::new(node_id);
        allocator
            .reserve(mutation_id, node_id)
            .expect("fixture reservation should succeed")
    }

    fn plan() -> GraphInsertPlan {
        let previous = GraphPublishedState::new(
            8,
            2,
            Some(HnswNodeId::new(0)),
            Some(2),
            GraphFormatVersion::current(),
            Some(mutation(40)),
        )
        .expect("fixture published state should be valid");
        GraphInsertPlan::new(
            reservation(),
            GraphRecordId::new(90),
            2,
            1,
            1,
            Some(HnswNodeId::new(2)),
            previous,
        )
        .expect("fixture insertion plan should be valid")
    }

    fn event(plan: &GraphInsertPlan, kind: GraphInsertEventKind) -> GraphInsertEvent {
        GraphInsertEvent::new(plan.mutation_id(), plan.node_id(), kind)
    }

    fn existing(page: u64, kind: GraphPageKind, role: HnswWalPageRole) -> HnswWalPageAction {
        HnswWalPageAction::existing(GraphPageId::new(page), kind, role)
            .expect("fixture page action should be valid")
    }

    fn page_set(actions: &[HnswWalPageAction]) -> HnswWalPageSet {
        HnswWalPageSet::new(actions).expect("fixture page set should be valid")
    }

    fn descriptor(plan: &GraphInsertPlan, revision: u64) -> GraphMutationDescriptor {
        plan.descriptor(GraphRecordRevision::new(revision))
            .expect("fixture descriptor should be valid")
    }

    #[test]
    fn pg17_generic_wal_is_the_only_v1_mechanism_and_caps_units_at_four_pages() {
        assert_eq!(HnswWalMechanism::V1, HnswWalMechanism::PostgresGenericWal);
        assert_eq!(MAX_HNSW_WAL_PAGES, 4);
        assert_eq!(
            MAX_HNSW_WAL_PAGES,
            usize::try_from(pgrx::pg_sys::MAX_GENERIC_XLOG_PAGES)
                .expect("PostgreSQL WAL page cap should fit usize")
        );

        let actions = [
            existing(1, GraphPageKind::Node, HnswWalPageRole::NodeRecord),
            existing(
                2,
                GraphPageKind::Directory,
                HnswWalPageRole::DirectoryLocator,
            ),
            existing(
                3,
                GraphPageKind::MutationDescriptor,
                HnswWalPageRole::MutationDescriptorHeader,
            ),
            existing(
                4,
                GraphPageKind::MutationDescriptor,
                HnswWalPageRole::MutationDescriptorEntry,
            ),
        ];
        assert_eq!(
            HnswWalPageSet::new(&actions)
                .expect("four pages should fit")
                .len(),
            MAX_HNSW_WAL_PAGES
        );
        let five = [
            actions[0],
            actions[1],
            actions[2],
            actions[3],
            existing(
                5,
                GraphPageKind::MutationDescriptor,
                HnswWalPageRole::MutationDescriptorHeader,
            ),
        ];
        assert!(matches!(
            HnswWalPageSet::new(&five),
            Err(HnswWalError::PageLimitExceeded {
                maximum: 4,
                actual: 5
            })
        ));
    }

    #[test]
    fn page_sets_are_fixed_capacity_unique_and_in_registration_lock_order() {
        assert!(matches!(
            HnswWalPageSet::new(&[]),
            Err(HnswWalError::InvalidPageSet { .. })
        ));
        let node = existing(2, GraphPageKind::Node, HnswWalPageRole::NodeRecord);
        assert!(matches!(
            HnswWalPageSet::new(&[node, node]),
            Err(HnswWalError::InvalidPageSet { .. })
        ));
        assert!(matches!(
            HnswWalPageSet::new(&[
                existing(
                    3,
                    GraphPageKind::MutationDescriptor,
                    HnswWalPageRole::MutationDescriptorHeader,
                ),
                node,
            ]),
            Err(HnswWalError::InvalidPageSet { .. })
        ));
        assert!(
            HnswWalPageAction::existing(
                GraphPageId::new(0),
                GraphPageKind::Node,
                HnswWalPageRole::NodeRecord,
            )
            .is_err()
        );

        let ordered = page_set(&[
            node,
            existing(
                3,
                GraphPageKind::MutationDescriptor,
                HnswWalPageRole::MutationDescriptorHeader,
            ),
        ]);
        assert_eq!(
            ordered
                .iter()
                .map(HnswWalPageAction::page_id)
                .collect::<Vec<_>>(),
            vec![GraphPageId::new(2), GraphPageId::new(3)]
        );
        assert_eq!(ordered.len(), ordered.iter().len());
    }

    #[test]
    fn page_initialization_is_one_unreachable_nonzero_full_image() {
        let unit = HnswWalUnit::initialize_page(
            mutation(41),
            GraphPageId::new(7),
            GraphPageKind::MutationDescriptor,
        )
        .expect("descriptor page initialization should be valid");
        assert_eq!(unit.kind(), HnswWalUnitKind::InitializePage);
        assert_eq!(unit.mechanism(), HnswWalMechanism::V1);
        assert_eq!(
            unit.lock_scope(),
            HnswWalLockScope::RelationExtensionThenNewPage
        );
        assert_eq!(unit.visibility(), HnswWalVisibility::NoPublishedChange);
        assert_eq!(unit.pages().len(), 1);
        let page = unit.pages().iter().next().expect("one page is registered");
        assert_eq!(page.page_id(), GraphPageId::new(7));
        assert_eq!(page.image(), HnswWalImage::FullImage);
        assert_eq!(page.role(), HnswWalPageRole::PageInitialization);
        assert!(unit.completion_after_finish().is_none());

        assert!(
            HnswWalUnit::initialize_page(mutation(41), GraphPageId::new(0), GraphPageKind::Node,)
                .is_err()
        );
        assert!(
            HnswWalUnit::initialize_page(mutation(41), GraphPageId::new(3), GraphPageKind::Meta,)
                .is_err()
        );
        assert!(
            HnswWalPageAction::existing(
                GraphPageId::new(3),
                GraphPageKind::Node,
                HnswWalPageRole::PageInitialization,
            )
            .is_err()
        );
    }

    #[test]
    fn reservation_and_publication_are_separate_block_zero_units() {
        let mut plan = plan();
        let reserve = HnswWalUnit::reserve_node(reservation());
        assert_eq!(reserve.kind(), HnswWalUnitKind::ReserveNodeId);
        let page = reserve
            .pages()
            .iter()
            .next()
            .expect("reservation registers metapage");
        assert_eq!(reserve.pages().len(), 1);
        assert_eq!(reserve.lock_scope(), HnswWalLockScope::AllocatorMeta);
        assert_eq!(page.page_id(), GraphPageId::new(0));
        assert_eq!(page.kind(), GraphPageKind::Meta);
        assert_eq!(page.image(), HnswWalImage::Delta);
        assert_eq!(page.role(), HnswWalPageRole::AllocatorState);
        assert_eq!(reserve.visibility(), HnswWalVisibility::NoPublishedChange);

        for kind in [
            GraphInsertEventKind::NodeAppended,
            GraphInsertEventKind::OutboundLayerWritten { ordinal: 0 },
            GraphInsertEventKind::RewireApplied { ordinal: 0 },
            GraphInsertEventKind::NodeReady,
        ] {
            let next = event(&plan, kind);
            plan = plan
                .transition(next)
                .expect("fixture transition should advance");
        }
        let GraphMutationStep::Publish(step) =
            plan.publication_step().expect("ready plan should publish")
        else {
            panic!("expected publication step")
        };
        let publication = HnswWalUnit::publish_root(step);
        assert_eq!(publication.kind(), HnswWalUnitKind::PublishRoot);
        assert_eq!(publication.lock_scope(), HnswWalLockScope::PublicationMeta);
        assert_eq!(publication.pages().len(), 1);
        assert_eq!(
            publication
                .pages()
                .iter()
                .next()
                .expect("publication registers metapage")
                .role(),
            HnswWalPageRole::MetadataPublication
        );
        assert_eq!(
            publication.visibility(),
            HnswWalVisibility::Publish {
                expected: step.expected_state(),
                target: step.new_state(),
            }
        );
        assert!(publication.completion_after_finish().is_none());
        let first_finish = publication
            .publication_completion_after_finish(step.expected_state())
            .expect("expected pre-state should publish");
        let published = plan
            .clone()
            .transition(first_finish)
            .expect("successful finish should advance publication");
        let replay_finish = publication
            .publication_completion_after_finish(step.new_state())
            .expect("exact target replay should be accepted");
        assert_eq!(
            published
                .clone()
                .transition(replay_finish)
                .expect("exact target replay should be idempotent"),
            published
        );
        let third_state = GraphPublishedState::new(
            step.new_state().generation(),
            step.expected_state().node_count(),
            step.expected_state().entry_point(),
            step.expected_state().dimensions(),
            step.expected_state().format_version(),
            Some(mutation(99)),
        )
        .expect("third publication fixture should be valid");
        assert!(matches!(
            publication.publication_completion_after_finish(third_state),
            Err(HnswWalError::PublicationConflict)
        ));
    }

    #[test]
    fn append_outbound_rewire_and_ready_units_bind_complete_semantic_steps() {
        let mut plan = plan();
        let GraphMutationStep::AppendUnpublishedNode(append_step) = plan
            .append_step(
                DenseVector::new(vec![1.0, 0.0]).expect("vector fixture is valid"),
                GraphLayerCount::new(1).expect("layer fixture is valid"),
            )
            .expect("append step should be valid")
        else {
            panic!("expected append step")
        };
        let after_append = plan
            .clone()
            .transition(event(&plan, GraphInsertEventKind::NodeAppended))
            .expect("append target should be valid");
        let append_descriptor =
            GraphMutationDescriptorTransition::create(descriptor(&after_append, 0))
                .expect("append descriptor creation should be valid");
        let append_reservation = append_step.reservation();
        let append_generation = append_step.target_generation();
        let append_pages = page_set(&[
            HnswWalPageAction::append_node(
                GraphPageId::new(2),
                append_generation,
                append_reservation.node_id(),
            )
            .expect("append node page should be valid"),
            HnswWalPageAction::directory(
                GraphPageId::new(3),
                HnswWalDirectoryWrite::InsertNodeAndDescriptor {
                    generation: append_generation,
                    node_id: append_reservation.node_id(),
                    mutation_id: append_reservation.mutation_id(),
                },
            )
            .expect("append directory page should be valid"),
            HnswWalPageAction::descriptor_header(GraphPageId::new(4), append_descriptor)
                .expect("append descriptor page should be valid"),
        ]);
        let append = HnswWalUnit::append_unpublished(
            append_step,
            append_descriptor,
            append_pages,
        )
        .expect("append WAL unit should be valid");
        assert_eq!(append.kind(), HnswWalUnitKind::AppendUnpublishedNode);
        assert_eq!(append.lock_scope(), HnswWalLockScope::DataPages);
        assert!(matches!(
            append.semantic(),
            HnswWalSemanticAction::AppendUnpublished { .. }
        ));
        assert_eq!(
            append.completion_after_finish(),
            Some(event(&plan, GraphInsertEventKind::NodeAppended))
        );
        assert_eq!(plan.phase(), GraphInsertPhase::Prepared);
        plan = plan
            .transition(
                append
                    .completion_after_finish()
                    .expect("append finish completes state"),
            )
            .expect("append finish should advance state");
        assert_eq!(plan, after_append);

        let outbound = GraphNeighbors::new(
            3,
            plan.node_id(),
            LayerIndex::base(),
            vec![HnswNodeId::new(0)],
        )
        .expect("outbound fixture is valid");
        let GraphMutationStep::WriteOutboundLayer(outbound_step) = plan
            .outbound_step(outbound)
            .expect("outbound step should be valid")
        else {
            panic!("expected outbound step")
        };
        let outbound_entry = GraphMutationDescriptorEntry::outbound(outbound_step)
            .expect("outbound descriptor entry should be valid");
        let after_outbound = plan
            .clone()
            .transition(event(
                &plan,
                GraphInsertEventKind::OutboundLayerWritten { ordinal: 0 },
            ))
            .expect("outbound target should be valid");
        let outbound_descriptor = GraphMutationDescriptorTransition::advance(
            descriptor(&plan, 0),
            descriptor(&after_outbound, 1),
        )
        .expect("outbound descriptor transition should be valid");
        let outbound_step = outbound_entry.step();
        let outbound_pages = page_set(&[
            HnswWalPageAction::append_adjacency(
                GraphPageId::new(5),
                outbound_step.target_generation(),
                outbound_step.adjacency().node_id(),
                outbound_step.adjacency().layer(),
                None,
            )
            .expect("outbound adjacency page should be valid"),
            HnswWalPageAction::directory(
                GraphPageId::new(6),
                HnswWalDirectoryWrite::InsertAdjacencyAndEntry {
                    generation: outbound_step.target_generation(),
                    node_id: outbound_step.adjacency().node_id(),
                    layer: outbound_step.adjacency().layer(),
                    mutation_id: outbound_step.mutation_id(),
                    ordinal: outbound_entry.ordinal(),
                },
            )
            .expect("outbound directory page should be valid"),
            HnswWalPageAction::descriptor_header(GraphPageId::new(7), outbound_descriptor)
                .expect("outbound descriptor header should be valid"),
            HnswWalPageAction::descriptor_entry(GraphPageId::new(8), &outbound_entry)
                .expect("outbound descriptor entry should be valid"),
        ]);
        let outbound_unit = HnswWalUnit::write_outbound(
            outbound_entry,
            outbound_descriptor,
            outbound_pages,
        )
        .expect("outbound WAL unit should be valid");
        assert_eq!(outbound_unit.kind(), HnswWalUnitKind::WriteOutboundLayer);
        assert_eq!(
            outbound_unit.completion_after_finish(),
            Some(event(
                &plan,
                GraphInsertEventKind::OutboundLayerWritten { ordinal: 0 },
            ))
        );
        plan = plan
            .transition(
                outbound_unit
                    .completion_after_finish()
                    .expect("outbound unit completes state"),
            )
            .expect("outbound finish should advance state");
        assert_eq!(plan, after_outbound);

        let rewire = GraphNeighbors::new(
            3,
            HnswNodeId::new(0),
            LayerIndex::base(),
            vec![plan.node_id()],
        )
        .expect("rewire fixture is valid");
        let GraphMutationStep::ReplaceNeighbors(rewire_step) = plan
            .rewire_step(rewire, GraphRecordRevision::new(3))
            .expect("rewire step should be valid")
        else {
            panic!("expected rewire step")
        };
        let rewire_entry = GraphMutationDescriptorEntry::rewire(rewire_step, 0)
            .expect("rewire descriptor entry should be valid");
        let after_rewire = plan
            .clone()
            .transition(event(
                &plan,
                GraphInsertEventKind::RewireApplied { ordinal: 0 },
            ))
            .expect("rewire target should be valid");
        let rewire_descriptor = GraphMutationDescriptorTransition::advance(
            descriptor(&plan, 1),
            descriptor(&after_rewire, 2),
        )
        .expect("rewire descriptor transition should be valid");
        let rewire_step = rewire_entry.step();
        let rewire_pages = page_set(&[
            HnswWalPageAction::append_adjacency(
                GraphPageId::new(9),
                rewire_step.target_generation(),
                rewire_step.adjacency().node_id(),
                rewire_step.adjacency().layer(),
                rewire_step.expected_revision(),
            )
            .expect("rewire adjacency page should be valid"),
            HnswWalPageAction::directory(
                GraphPageId::new(10),
                HnswWalDirectoryWrite::InsertAdjacencyAndEntry {
                    generation: rewire_step.target_generation(),
                    node_id: rewire_step.adjacency().node_id(),
                    layer: rewire_step.adjacency().layer(),
                    mutation_id: rewire_step.mutation_id(),
                    ordinal: rewire_entry.ordinal(),
                },
            )
            .expect("rewire directory page should be valid"),
            HnswWalPageAction::descriptor_header(GraphPageId::new(11), rewire_descriptor)
                .expect("rewire descriptor header should be valid"),
            HnswWalPageAction::descriptor_entry(GraphPageId::new(12), &rewire_entry)
                .expect("rewire descriptor entry should be valid"),
        ]);
        let published_generation_pages = page_set(&[
            HnswWalPageAction::append_adjacency(
                GraphPageId::new(15),
                8,
                rewire_step.adjacency().node_id(),
                rewire_step.adjacency().layer(),
                rewire_step.expected_revision(),
            )
            .expect("published-generation adjacency fixture should be valid in isolation"),
            HnswWalPageAction::directory(
                GraphPageId::new(16),
                HnswWalDirectoryWrite::InsertAdjacencyAndEntry {
                    generation: 8,
                    node_id: rewire_step.adjacency().node_id(),
                    layer: rewire_step.adjacency().layer(),
                    mutation_id: rewire_step.mutation_id(),
                    ordinal: rewire_entry.ordinal(),
                },
            )
            .expect("published-generation locator fixture should be valid in isolation"),
            HnswWalPageAction::descriptor_header(GraphPageId::new(17), rewire_descriptor)
                .expect("rewire descriptor header fixture should be valid"),
            HnswWalPageAction::descriptor_entry(GraphPageId::new(18), &rewire_entry)
                .expect("rewire descriptor entry fixture should be valid"),
        ]);
        assert!(matches!(
            HnswWalUnit::replace_neighbor_layer(
                rewire_entry.clone(),
                rewire_descriptor,
                published_generation_pages,
            ),
            Err(HnswWalError::InvalidUnit { .. })
        ));
        let rewire_unit = HnswWalUnit::replace_neighbor_layer(
            rewire_entry,
            rewire_descriptor,
            rewire_pages,
        )
        .expect("rewire WAL unit should be valid");
        assert_eq!(rewire_unit.kind(), HnswWalUnitKind::ReplaceNeighborLayer);
        assert!(rewire_unit.pages().contains_write(
            HnswWalPageRole::AdjacencyRecord,
            HnswWalPageWrite::AppendAdjacency {
                generation: 9,
                node_id: HnswNodeId::new(0),
                layer: LayerIndex::base(),
                expected_revision: Some(GraphRecordRevision::new(3)),
            },
        ));
        assert!(!rewire_unit.pages().contains_write(
            HnswWalPageRole::AdjacencyRecord,
            HnswWalPageWrite::AppendAdjacency {
                generation: 8,
                node_id: HnswNodeId::new(0),
                layer: LayerIndex::base(),
                expected_revision: Some(GraphRecordRevision::new(3)),
            },
        ));
        assert_eq!(
            rewire_unit.completion_after_finish(),
            Some(event(
                &plan,
                GraphInsertEventKind::RewireApplied { ordinal: 0 },
            ))
        );
        plan = plan
            .transition(
                rewire_unit
                    .completion_after_finish()
                    .expect("rewire unit completes state"),
            )
            .expect("rewire finish should advance state");
        assert_eq!(plan, after_rewire);

        let GraphMutationStep::MarkNodeReady(ready_step) = plan
            .ready_step(GraphRecordRevision::new(4))
            .expect("ready step should be valid")
        else {
            panic!("expected ready step")
        };
        let after_ready = plan
            .clone()
            .transition(event(&plan, GraphInsertEventKind::NodeReady))
            .expect("ready target should be valid");
        let ready_descriptor = GraphMutationDescriptorTransition::advance(
            descriptor(&plan, 2),
            descriptor(&after_ready, 3),
        )
        .expect("ready descriptor transition should be valid");
        let ready_pages = page_set(&[
            HnswWalPageAction::revise_node(
                GraphPageId::new(13),
                ready_step.target_generation(),
                ready_step.reservation().node_id(),
                ready_step.expected_revision(),
            )
            .expect("ready node page should be valid"),
            HnswWalPageAction::descriptor_header(GraphPageId::new(14), ready_descriptor)
                .expect("ready descriptor page should be valid"),
        ]);
        let ready = HnswWalUnit::mark_node_ready(
            ready_step,
            ready_descriptor,
            ready_pages,
        )
        .expect("ready WAL unit should be valid");
        assert_eq!(ready.kind(), HnswWalUnitKind::MarkNodeReady);
        assert_eq!(
            ready.completion_after_finish(),
            Some(event(&plan, GraphInsertEventKind::NodeReady))
        );
        assert_eq!(ready.visibility(), HnswWalVisibility::NoPublishedChange);
        assert_eq!(
            plan.clone()
                .transition(
                    ready
                        .completion_after_finish()
                        .expect("ready finish completes state"),
                )
                .expect("ready finish should advance state"),
            after_ready
        );
    }

    #[test]
    fn unit_page_role_matrix_rejects_missing_descriptors_and_wrong_page_kinds() {
        let plan = plan();
        let GraphMutationStep::AppendUnpublishedNode(append_step) = plan
            .append_step(
                DenseVector::new(vec![1.0, 0.0]).expect("vector fixture is valid"),
                GraphLayerCount::new(1).expect("layer fixture is valid"),
            )
            .expect("append step should be valid")
        else {
            panic!("expected append step")
        };
        let after_append = plan
            .clone()
            .transition(event(&plan, GraphInsertEventKind::NodeAppended))
            .expect("append target should be valid");
        let append_descriptor =
            GraphMutationDescriptorTransition::create(descriptor(&after_append, 0))
                .expect("append descriptor should be valid");
        let wrong_phase_descriptor =
            GraphMutationDescriptorTransition::create(descriptor(&plan, 0))
                .expect("prepared descriptor fixture should be valid");
        let reservation = append_step.reservation();
        let generation = append_step.target_generation();
        let append_node = HnswWalPageAction::append_node(
            GraphPageId::new(2),
            generation,
            reservation.node_id(),
        )
        .expect("append node fixture should be valid");
        let append_directory = HnswWalPageAction::directory(
            GraphPageId::new(3),
            HnswWalDirectoryWrite::InsertNodeAndDescriptor {
                generation,
                node_id: reservation.node_id(),
                mutation_id: reservation.mutation_id(),
            },
        )
        .expect("append directory fixture should be valid");
        assert!(matches!(
            HnswWalUnit::append_unpublished(
                append_step.clone(),
                wrong_phase_descriptor,
                page_set(&[
                    append_node,
                    append_directory,
                    HnswWalPageAction::descriptor_header(
                        GraphPageId::new(4),
                        wrong_phase_descriptor,
                    )
                    .expect("wrong-phase descriptor page should still be structurally valid"),
                ]),
            ),
            Err(HnswWalError::InvalidUnit { .. })
        ));
        assert!(matches!(
            HnswWalUnit::append_unpublished(
                append_step.clone(),
                append_descriptor,
                page_set(&[append_node, append_directory]),
            ),
            Err(HnswWalError::InvalidUnit { .. })
        ));
        assert!(matches!(
            HnswWalUnit::append_unpublished(
                append_step,
                append_descriptor,
                page_set(&[
                    append_node,
                    append_directory,
                    HnswWalPageAction::descriptor_header(
                        GraphPageId::new(4),
                        append_descriptor,
                    )
                    .expect("append descriptor fixture should be valid"),
                    HnswWalPageAction::descriptor_header(
                        GraphPageId::new(5),
                        append_descriptor,
                    )
                    .expect("duplicate descriptor fixture should be structurally valid"),
                ]),
            ),
            Err(HnswWalError::InvalidUnit { .. })
        ));
        assert!(
            HnswWalPageAction::existing(
                GraphPageId::new(2),
                GraphPageKind::Meta,
                HnswWalPageRole::AdjacencyRecord,
            )
            .is_err()
        );
        assert!(
            HnswWalPageAction::existing(
                GraphPageId::new(2),
                GraphPageKind::Directory,
                HnswWalPageRole::MetadataPublication,
            )
            .is_err()
        );
    }

    #[test]
    fn reservation_release_and_descriptor_cleanup_are_idempotent_nonvisible_units() {
        let reservation = reservation();
        let release = HnswWalUnit::release_reservation(reservation);
        assert_eq!(release.kind(), HnswWalUnitKind::ReleaseReservation);
        assert_eq!(release.mutation_id(), reservation.mutation_id());
        assert_eq!(release.lock_scope(), HnswWalLockScope::AllocatorMeta);
        assert_eq!(release.visibility(), HnswWalVisibility::NoPublishedChange);
        assert!(release.completion_after_finish().is_none());

        let cleanup = HnswWalUnit::cleanup_descriptor(
            reservation.mutation_id(),
            GraphRecordRevision::new(9),
            page_set(&[
                HnswWalPageAction::directory(
                    GraphPageId::new(11),
                    HnswWalDirectoryWrite::RemoveDescriptor {
                        mutation_id: reservation.mutation_id(),
                        expected_revision: GraphRecordRevision::new(9),
                    },
                )
                .expect("cleanup directory page should be valid"),
                HnswWalPageAction::delete_descriptor_header(
                    GraphPageId::new(12),
                    reservation.mutation_id(),
                    GraphRecordRevision::new(9),
                )
                .expect("cleanup descriptor page should be valid"),
            ]),
        )
        .expect("descriptor cleanup should be valid");
        assert_eq!(cleanup.kind(), HnswWalUnitKind::CleanupDescriptor);
        assert_eq!(cleanup.lock_scope(), HnswWalLockScope::DataPages);
        assert_eq!(cleanup.visibility(), HnswWalVisibility::NoPublishedChange);
        assert!(cleanup.completion_after_finish().is_none());
    }

    #[test]
    fn tombstone_storage_and_publication_are_distinct_exact_wal_units() {
        let previous = GraphPublishedState::new_with_tombstones(
            12,
            4,
            1,
            Some(HnswNodeId::new(0)),
            Some(2),
            GraphFormatVersion::current(),
            Some(mutation(80)),
        )
        .expect("published tombstone fixture is valid");
        let plan = GraphTombstonePlan::new(
            mutation(81),
            HnswNodeId::new(2),
            GraphRecordId::new(901),
            GraphRecordRevision::new(6),
            previous,
        )
        .expect("tombstone fixture plan is valid");
        let step = plan.store_step();
        let source = HnswSourceBinding::new(
            step.node_id(),
            step.record_id(),
            HnswHeapTid::new(3, 9).expect("fixture heap TID is valid"),
            previous.generation(),
            step.expected_revision(),
        )
        .expect("fixture source binding is valid");
        let epoch = HnswTombstoneEpoch::new(81).expect("fixture epoch is nonzero");
        let bound = HnswTombstoneTransition::plan(
            HnswVacuumDeadTid::from_callback(source.heap_tid()),
            HnswNodeMvccState::Ready(source),
            epoch,
        )
        .and_then(|transition| transition.bind_graph_step(step))
        .expect("physical and graph tombstone steps bind exactly");
        let store = HnswWalUnit::store_tombstone(
            bound,
            page_set(&[
                HnswWalPageAction::tombstone_node(GraphPageId::new(20), bound)
                    .expect("tombstone node action is valid"),
                HnswWalPageAction::directory(
                    GraphPageId::new(21),
                    tombstone_locator_write(bound),
                )
                .expect("tombstone locator action is valid"),
            ]),
        )
        .expect("exact tombstone store unit is valid");

        assert_eq!(store.kind(), HnswWalUnitKind::StoreTombstone);
        assert_eq!(store.lock_scope(), HnswWalLockScope::DataPages);
        assert_eq!(store.visibility(), HnswWalVisibility::NoPublishedChange);
        assert_eq!(
            store
                .tombstone_store_completion_after_finish()
                .expect("store completion exposes exact step"),
            step
        );
        assert!(matches!(
            store.semantic(),
            HnswWalSemanticAction::StoreTombstone(observed) if *observed == bound
        ));

        assert!(matches!(
            plan.publication_step(),
            Err(context_index::GraphMvccError::InvalidTransition { .. })
        ));
        let stored_plan = plan
            .clone()
            .record_store_finished(step)
            .expect("exact tombstone store completes graph plan");
        let publication_step = stored_plan
            .publication_step()
            .expect("stored tombstone exposes publication step");
        let publication = HnswWalUnit::publish_tombstone(publication_step);
        assert_eq!(publication.kind(), HnswWalUnitKind::PublishTombstone);
        assert_eq!(
            publication.lock_scope(),
            HnswWalLockScope::PublicationMeta
        );
        assert_eq!(publication.pages().len(), 1);
        assert_eq!(publication.pages().iter().next().map(HnswWalPageAction::page_id), Some(GraphPageId::new(0)));
        assert_eq!(
            publication.visibility(),
            HnswWalVisibility::Publish {
                expected: previous,
                target: plan.target_state(),
            }
        );
        assert_eq!(
            publication
                .tombstone_publication_completion_after_finish(plan.target_state())
                .expect("exact publication completion is accepted"),
            plan.target_state()
        );
        assert!(matches!(
            publication.tombstone_publication_completion_after_finish(previous),
            Err(HnswWalError::PublicationConflict)
        ));
    }
}
