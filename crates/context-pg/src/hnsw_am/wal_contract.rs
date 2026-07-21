//! Pure planning contract for PostgreSQL Generic-WAL HNSW page actions.
//!
//! This module owns no buffers and emits no WAL. It prepares fixed-capacity,
//! lock-ordered semantic units for the physical adapter to consume.

use core::fmt;

use context_index::{
    GRAPH_PENDING_RESERVATION_REGION_BYTES, GraphAppendStep, GraphInsertEvent,
    GraphInsertEventKind, GraphInsertPhase, GraphMutationDescriptorEntry,
    GraphMutationDescriptorEntryKind, GraphMutationDescriptorTransition, GraphMutationId,
    GraphNodeReservation, GraphPageId, GraphPageKind, GraphPublicationStep, GraphPublishedState,
    GraphReadyStep, GraphRecordRevision, GraphTombstonePublicationStep, GraphTombstoneStep,
    HnswNodeId,
};

pub(super) mod critical_section;
mod page_action;

pub(super) use page_action::*;

use super::mvcc_contract::HnswBoundTombstone;

pub(super) const MAX_HNSW_WAL_PAGES: usize = pgrx::pg_sys::MAX_GENERIC_XLOG_PAGES as usize;

const _: () = assert!(GRAPH_PENDING_RESERVATION_REGION_BYTES < pgrx::pg_sys::BLCKSZ as usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HnswWalMechanism {
    PostgresGenericWal,
}

impl HnswWalMechanism {
    pub(super) const V1: Self = Self::PostgresGenericWal;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HnswWalUnitKind {
    ReserveNodeId,
    InitializePage,
    AppendUnpublishedNode,
    WriteOutboundLayer,
    ReplaceNeighborLayer,
    MarkNodeReady,
    PublishRoot,
    ReleaseReservation,
    CleanupDescriptor,
    StoreTombstone,
    PublishTombstone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HnswWalVisibility {
    NoPublishedChange,
    Publish {
        expected: GraphPublishedState,
        target: GraphPublishedState,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HnswWalLockScope {
    AllocatorMeta,
    RelationExtensionThenNewPage,
    DataPages,
    PublicationMeta,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum HnswWalSemanticAction {
    ReserveNode(GraphNodeReservation),
    InitializePage {
        mutation_id: GraphMutationId,
        page_id: GraphPageId,
        kind: GraphPageKind,
    },
    AppendUnpublished {
        step: GraphAppendStep,
        descriptor: GraphMutationDescriptorTransition,
    },
    WriteOutbound {
        entry: GraphMutationDescriptorEntry,
        descriptor: GraphMutationDescriptorTransition,
    },
    ReplaceNeighbor {
        entry: GraphMutationDescriptorEntry,
        descriptor: GraphMutationDescriptorTransition,
    },
    MarkNodeReady {
        step: GraphReadyStep,
        descriptor: GraphMutationDescriptorTransition,
    },
    PublishRoot(GraphPublicationStep),
    ReleaseReservation(GraphNodeReservation),
    CleanupDescriptor {
        mutation_id: GraphMutationId,
        expected_revision: GraphRecordRevision,
    },
    StoreTombstone(HnswBoundTombstone),
    PublishTombstone {
        step: GraphTombstoneStep,
        expected: GraphPublishedState,
        target: GraphPublishedState,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct HnswWalUnit {
    mechanism: HnswWalMechanism,
    kind: HnswWalUnitKind,
    mutation_id: GraphMutationId,
    semantic: HnswWalSemanticAction,
    pages: HnswWalPageSet,
    lock_scope: HnswWalLockScope,
    visibility: HnswWalVisibility,
    completion_after_finish: Option<GraphInsertEvent>,
}

impl HnswWalUnit {
    pub(super) fn reserve_node(reservation: GraphNodeReservation) -> Self {
        Self::standalone_meta(
            HnswWalUnitKind::ReserveNodeId,
            reservation.mutation_id(),
            HnswWalSemanticAction::ReserveNode(reservation),
        )
    }

    pub(super) fn initialize_page(
        mutation_id: GraphMutationId,
        page_id: GraphPageId,
        kind: GraphPageKind,
    ) -> HnswWalResult<Self> {
        let action = HnswWalPageAction::initialization(page_id, kind)?;
        Ok(Self {
            mechanism: HnswWalMechanism::V1,
            kind: HnswWalUnitKind::InitializePage,
            mutation_id,
            semantic: HnswWalSemanticAction::InitializePage {
                mutation_id,
                page_id,
                kind,
            },
            pages: HnswWalPageSet::singleton(action),
            lock_scope: HnswWalLockScope::RelationExtensionThenNewPage,
            visibility: HnswWalVisibility::NoPublishedChange,
            completion_after_finish: None,
        })
    }

    pub(super) fn append_unpublished(
        step: GraphAppendStep,
        descriptor: GraphMutationDescriptorTransition,
        pages: HnswWalPageSet,
    ) -> HnswWalResult<Self> {
        validate_data_roles(
            &pages,
            &[
                HnswWalPageRole::NodeRecord,
                HnswWalPageRole::MutationDescriptorHeader,
                HnswWalPageRole::DirectoryLocator,
            ],
            &[
                HnswWalPageRole::NodeRecord,
                HnswWalPageRole::DirectoryLocator,
                HnswWalPageRole::MutationDescriptorHeader,
            ],
        )?;
        let reservation = step.reservation();
        let target = descriptor.target();
        if descriptor.expected().is_some()
            || target.phase() != GraphInsertPhase::NodeAppended
            || target.reservation() != reservation
            || target.record_id() != step.record_id()
            || target.target_state().generation() != step.target_generation()
        {
            return Err(HnswWalError::InvalidUnit {
                reason: "append must create the exact node-appended descriptor header",
            });
        }
        let expected_node = HnswWalPageWrite::AppendNode {
            generation: step.target_generation(),
            node_id: reservation.node_id(),
        };
        let expected_directory =
            HnswWalPageWrite::Directory(HnswWalDirectoryWrite::InsertNodeAndDescriptor {
                generation: step.target_generation(),
                node_id: reservation.node_id(),
                mutation_id: reservation.mutation_id(),
            });
        if !pages.contains_write(HnswWalPageRole::NodeRecord, expected_node)
            || !pages.contains_write(
                HnswWalPageRole::MutationDescriptorHeader,
                descriptor_header_write(descriptor),
            )
            || !pages.contains_write(HnswWalPageRole::DirectoryLocator, expected_directory)
        {
            return Err(HnswWalError::InvalidUnit {
                reason: "append page writes do not match node generation and locator keys",
            });
        }
        Ok(Self::data(
            HnswWalUnitKind::AppendUnpublishedNode,
            reservation.mutation_id(),
            HnswWalSemanticAction::AppendUnpublished { step, descriptor },
            pages,
            GraphInsertEventKind::NodeAppended,
            reservation.node_id(),
        ))
    }

    pub(super) fn write_outbound(
        entry: GraphMutationDescriptorEntry,
        descriptor: GraphMutationDescriptorTransition,
        pages: HnswWalPageSet,
    ) -> HnswWalResult<Self> {
        if entry.kind() != GraphMutationDescriptorEntryKind::OutboundLayer
            || entry.step().expected_revision().is_some()
        {
            return Err(HnswWalError::InvalidUnit {
                reason: "outbound WAL unit requires one complete outbound descriptor entry",
            });
        }
        validate_topology_roles(&pages)?;
        let Some(expected) = descriptor.expected() else {
            return Err(HnswWalError::InvalidUnit {
                reason: "outbound WAL unit requires an existing descriptor header",
            });
        };
        let target = descriptor.target();
        let step = entry.step();
        let (expected_completed, expected_total) = expected.outbound_progress();
        let (target_completed, target_total) = target.outbound_progress();
        if entry.ordinal() != expected_completed
            || target_completed != expected_completed.saturating_add(1)
            || target_total != expected_total
            || target.rewire_progress() != expected.rewire_progress()
            || target.reservation().mutation_id() != step.mutation_id()
            || target.reservation().node_id() != step.inserted_node_id()
            || target.target_state().generation() != step.target_generation()
        {
            return Err(HnswWalError::InvalidUnit {
                reason: "outbound entry and exact descriptor transition disagree",
            });
        }
        let expected_adjacency = HnswWalPageWrite::AppendAdjacency {
            generation: step.target_generation(),
            node_id: step.adjacency().node_id(),
            layer: step.adjacency().layer(),
            expected_revision: None,
        };
        let expected_entry = HnswWalPageWrite::InsertDescriptorEntry {
            mutation_id: step.mutation_id(),
            ordinal: entry.ordinal(),
        };
        let expected_directory =
            HnswWalPageWrite::Directory(HnswWalDirectoryWrite::InsertAdjacencyAndEntry {
                generation: step.target_generation(),
                node_id: step.adjacency().node_id(),
                layer: step.adjacency().layer(),
                mutation_id: step.mutation_id(),
                ordinal: entry.ordinal(),
            });
        if !pages.contains_write(HnswWalPageRole::AdjacencyRecord, expected_adjacency)
            || !pages.contains_write(
                HnswWalPageRole::MutationDescriptorHeader,
                descriptor_header_write(descriptor),
            )
            || !pages.contains_write(HnswWalPageRole::MutationDescriptorEntry, expected_entry)
            || !pages.contains_write(HnswWalPageRole::DirectoryLocator, expected_directory)
        {
            return Err(HnswWalError::InvalidUnit {
                reason: "outbound page writes do not match target generation and locator keys",
            });
        }
        let mutation_id = step.mutation_id();
        let node_id = step.inserted_node_id();
        let ordinal = entry.ordinal();
        Ok(Self::data(
            HnswWalUnitKind::WriteOutboundLayer,
            mutation_id,
            HnswWalSemanticAction::WriteOutbound { entry, descriptor },
            pages,
            GraphInsertEventKind::OutboundLayerWritten { ordinal },
            node_id,
        ))
    }

    pub(super) fn replace_neighbor_layer(
        entry: GraphMutationDescriptorEntry,
        descriptor: GraphMutationDescriptorTransition,
        pages: HnswWalPageSet,
    ) -> HnswWalResult<Self> {
        if entry.kind() != GraphMutationDescriptorEntryKind::NeighborRewire
            || entry.step().expected_revision().is_none()
        {
            return Err(HnswWalError::InvalidUnit {
                reason: "rewire WAL unit requires one complete revision-checked entry",
            });
        }
        validate_topology_roles(&pages)?;
        let Some(expected) = descriptor.expected() else {
            return Err(HnswWalError::InvalidUnit {
                reason: "rewire WAL unit requires an existing descriptor header",
            });
        };
        let target = descriptor.target();
        let step = entry.step();
        let (expected_completed, expected_total) = expected.rewire_progress();
        let (target_completed, target_total) = target.rewire_progress();
        if entry.ordinal() != expected_completed
            || target_completed != expected_completed.saturating_add(1)
            || target_total != expected_total
            || target.outbound_progress() != expected.outbound_progress()
            || target.reservation().mutation_id() != step.mutation_id()
            || target.reservation().node_id() != step.inserted_node_id()
            || target.target_state().generation() != step.target_generation()
        {
            return Err(HnswWalError::InvalidUnit {
                reason: "rewire entry and exact descriptor transition disagree",
            });
        }
        let expected_adjacency = HnswWalPageWrite::AppendAdjacency {
            generation: step.target_generation(),
            node_id: step.adjacency().node_id(),
            layer: step.adjacency().layer(),
            expected_revision: step.expected_revision(),
        };
        let expected_entry = HnswWalPageWrite::InsertDescriptorEntry {
            mutation_id: step.mutation_id(),
            ordinal: entry.ordinal(),
        };
        let expected_directory =
            HnswWalPageWrite::Directory(HnswWalDirectoryWrite::InsertAdjacencyAndEntry {
                generation: step.target_generation(),
                node_id: step.adjacency().node_id(),
                layer: step.adjacency().layer(),
                mutation_id: step.mutation_id(),
                ordinal: entry.ordinal(),
            });
        if !pages.contains_write(HnswWalPageRole::AdjacencyRecord, expected_adjacency)
            || !pages.contains_write(
                HnswWalPageRole::MutationDescriptorHeader,
                descriptor_header_write(descriptor),
            )
            || !pages.contains_write(HnswWalPageRole::MutationDescriptorEntry, expected_entry)
            || !pages.contains_write(HnswWalPageRole::DirectoryLocator, expected_directory)
        {
            return Err(HnswWalError::InvalidUnit {
                reason: "rewire page writes must append the exact next-generation locator",
            });
        }
        let mutation_id = step.mutation_id();
        let node_id = step.inserted_node_id();
        let ordinal = entry.ordinal();
        Ok(Self::data(
            HnswWalUnitKind::ReplaceNeighborLayer,
            mutation_id,
            HnswWalSemanticAction::ReplaceNeighbor { entry, descriptor },
            pages,
            GraphInsertEventKind::RewireApplied { ordinal },
            node_id,
        ))
    }

    pub(super) fn mark_node_ready(
        step: GraphReadyStep,
        descriptor: GraphMutationDescriptorTransition,
        pages: HnswWalPageSet,
    ) -> HnswWalResult<Self> {
        validate_data_roles(
            &pages,
            &[
                HnswWalPageRole::NodeRecord,
                HnswWalPageRole::MutationDescriptorHeader,
            ],
            &[
                HnswWalPageRole::NodeRecord,
                HnswWalPageRole::MutationDescriptorHeader,
            ],
        )?;
        let reservation = step.reservation();
        let Some(expected) = descriptor.expected() else {
            return Err(HnswWalError::InvalidUnit {
                reason: "node-ready WAL unit requires an existing descriptor header",
            });
        };
        let target = descriptor.target();
        if expected.phase() != GraphInsertPhase::ReadyToMarkNode
            || target.phase() != GraphInsertPhase::ReadyToPublish
            || expected.outbound_progress() != target.outbound_progress()
            || expected.rewire_progress() != target.rewire_progress()
            || target.reservation() != reservation
            || target.target_state().generation() != step.target_generation()
        {
            return Err(HnswWalError::InvalidUnit {
                reason: "node-ready step and exact descriptor transition disagree",
            });
        }
        let expected_node = HnswWalPageWrite::ReviseNode {
            generation: step.target_generation(),
            node_id: reservation.node_id(),
            expected_revision: step.expected_revision(),
        };
        if !pages.contains_write(HnswWalPageRole::NodeRecord, expected_node)
            || !pages.contains_write(
                HnswWalPageRole::MutationDescriptorHeader,
                descriptor_header_write(descriptor),
            )
        {
            return Err(HnswWalError::InvalidUnit {
                reason: "node-ready page writes do not match generation and revisions",
            });
        }
        Ok(Self::data(
            HnswWalUnitKind::MarkNodeReady,
            reservation.mutation_id(),
            HnswWalSemanticAction::MarkNodeReady { step, descriptor },
            pages,
            GraphInsertEventKind::NodeReady,
            reservation.node_id(),
        ))
    }

    pub(super) fn publish_root(step: GraphPublicationStep) -> Self {
        let reservation = step.reservation();
        Self {
            mechanism: HnswWalMechanism::V1,
            kind: HnswWalUnitKind::PublishRoot,
            mutation_id: reservation.mutation_id(),
            semantic: HnswWalSemanticAction::PublishRoot(step),
            pages: HnswWalPageSet::singleton(HnswWalPageAction::meta_publication()),
            lock_scope: HnswWalLockScope::PublicationMeta,
            visibility: HnswWalVisibility::Publish {
                expected: step.expected_state(),
                target: step.new_state(),
            },
            completion_after_finish: None,
        }
    }

    pub(super) fn release_reservation(reservation: GraphNodeReservation) -> Self {
        Self::standalone_meta(
            HnswWalUnitKind::ReleaseReservation,
            reservation.mutation_id(),
            HnswWalSemanticAction::ReleaseReservation(reservation),
        )
    }

    pub(super) fn cleanup_descriptor(
        mutation_id: GraphMutationId,
        expected_revision: GraphRecordRevision,
        pages: HnswWalPageSet,
    ) -> HnswWalResult<Self> {
        validate_data_roles(
            &pages,
            &[
                HnswWalPageRole::MutationDescriptorHeader,
                HnswWalPageRole::DirectoryLocator,
            ],
            &[
                HnswWalPageRole::DirectoryLocator,
                HnswWalPageRole::MutationDescriptorHeader,
            ],
        )?;
        if !pages.contains_write(
            HnswWalPageRole::MutationDescriptorHeader,
            HnswWalPageWrite::DeleteDescriptorHeader {
                mutation_id,
                expected_revision,
            },
        ) || !pages.contains_write(
            HnswWalPageRole::DirectoryLocator,
            HnswWalPageWrite::Directory(HnswWalDirectoryWrite::RemoveDescriptor {
                mutation_id,
                expected_revision,
            }),
        ) {
            return Err(HnswWalError::InvalidUnit {
                reason: "descriptor cleanup page writes do not match expected identity/revision",
            });
        }
        Ok(Self {
            mechanism: HnswWalMechanism::V1,
            kind: HnswWalUnitKind::CleanupDescriptor,
            mutation_id,
            semantic: HnswWalSemanticAction::CleanupDescriptor {
                mutation_id,
                expected_revision,
            },
            pages,
            lock_scope: HnswWalLockScope::DataPages,
            visibility: HnswWalVisibility::NoPublishedChange,
            completion_after_finish: None,
        })
    }

    pub(super) fn store_tombstone(
        bound: HnswBoundTombstone,
        pages: HnswWalPageSet,
    ) -> HnswWalResult<Self> {
        let step = bound.step();
        validate_data_roles(
            &pages,
            &[
                HnswWalPageRole::NodeRecord,
                HnswWalPageRole::DirectoryLocator,
            ],
            &[
                HnswWalPageRole::NodeRecord,
                HnswWalPageRole::DirectoryLocator,
            ],
        )?;
        if !pages.contains_write(HnswWalPageRole::NodeRecord, tombstone_node_write(bound))
            || !pages.contains_write(
                HnswWalPageRole::DirectoryLocator,
                HnswWalPageWrite::Directory(tombstone_locator_write(bound)),
            )
        {
            return Err(HnswWalError::InvalidUnit {
                reason: "tombstone page writes do not match the exact prepared step",
            });
        }
        Ok(Self {
            mechanism: HnswWalMechanism::V1,
            kind: HnswWalUnitKind::StoreTombstone,
            mutation_id: step.mutation_id(),
            semantic: HnswWalSemanticAction::StoreTombstone(bound),
            pages,
            lock_scope: HnswWalLockScope::DataPages,
            visibility: HnswWalVisibility::NoPublishedChange,
            completion_after_finish: None,
        })
    }

    pub(super) fn publish_tombstone(publication: GraphTombstonePublicationStep) -> Self {
        let step = publication.tombstone();
        let expected = publication.expected_state();
        let target = publication.target_state();
        Self {
            mechanism: HnswWalMechanism::V1,
            kind: HnswWalUnitKind::PublishTombstone,
            mutation_id: step.mutation_id(),
            semantic: HnswWalSemanticAction::PublishTombstone {
                step,
                expected,
                target,
            },
            pages: HnswWalPageSet::singleton(HnswWalPageAction::meta_publication()),
            lock_scope: HnswWalLockScope::PublicationMeta,
            visibility: HnswWalVisibility::Publish { expected, target },
            completion_after_finish: None,
        }
    }

    fn standalone_meta(
        kind: HnswWalUnitKind,
        mutation_id: GraphMutationId,
        semantic: HnswWalSemanticAction,
    ) -> Self {
        Self {
            mechanism: HnswWalMechanism::V1,
            kind,
            mutation_id,
            semantic,
            pages: HnswWalPageSet::singleton(HnswWalPageAction::meta_allocator()),
            lock_scope: HnswWalLockScope::AllocatorMeta,
            visibility: HnswWalVisibility::NoPublishedChange,
            completion_after_finish: None,
        }
    }

    fn data(
        kind: HnswWalUnitKind,
        mutation_id: GraphMutationId,
        semantic: HnswWalSemanticAction,
        pages: HnswWalPageSet,
        completion: GraphInsertEventKind,
        node_id: HnswNodeId,
    ) -> Self {
        Self {
            mechanism: HnswWalMechanism::V1,
            kind,
            mutation_id,
            semantic,
            pages,
            lock_scope: HnswWalLockScope::DataPages,
            visibility: HnswWalVisibility::NoPublishedChange,
            completion_after_finish: Some(GraphInsertEvent::new(mutation_id, node_id, completion)),
        }
    }

    pub(super) const fn mechanism(&self) -> HnswWalMechanism {
        self.mechanism
    }

    pub(super) const fn kind(&self) -> HnswWalUnitKind {
        self.kind
    }

    pub(super) const fn mutation_id(&self) -> GraphMutationId {
        self.mutation_id
    }

    pub(super) const fn semantic(&self) -> &HnswWalSemanticAction {
        &self.semantic
    }

    pub(super) const fn pages(&self) -> &HnswWalPageSet {
        &self.pages
    }

    pub(super) const fn lock_scope(&self) -> HnswWalLockScope {
        self.lock_scope
    }

    pub(super) const fn visibility(&self) -> HnswWalVisibility {
        self.visibility
    }

    pub(super) const fn completion_after_finish(&self) -> Option<GraphInsertEvent> {
        self.completion_after_finish
    }

    pub(super) fn tombstone_store_completion_after_finish(
        &self,
    ) -> HnswWalResult<GraphTombstoneStep> {
        let HnswWalSemanticAction::StoreTombstone(bound) = self.semantic() else {
            return Err(HnswWalError::InvalidUnit {
                reason: "only a tombstone-store unit completes a tombstone step",
            });
        };
        Ok(bound.step())
    }

    pub(super) fn tombstone_publication_completion_after_finish(
        &self,
        observed_state: GraphPublishedState,
    ) -> HnswWalResult<GraphPublishedState> {
        let HnswWalSemanticAction::PublishTombstone { target, .. } = self.semantic() else {
            return Err(HnswWalError::InvalidUnit {
                reason: "only a tombstone-publication unit accepts tombstone metadata",
            });
        };
        if observed_state != *target {
            return Err(HnswWalError::PublicationConflict);
        }
        Ok(observed_state)
    }

    pub(super) fn publication_completion_after_finish(
        &self,
        observed_state: GraphPublishedState,
    ) -> HnswWalResult<GraphInsertEvent> {
        let HnswWalSemanticAction::PublishRoot(step) = self.semantic() else {
            return Err(HnswWalError::InvalidUnit {
                reason: "only a root-publication unit accepts published state",
            });
        };
        let expected = step.expected_state();
        let target = step.new_state();
        if observed_state != expected && observed_state != target {
            return Err(HnswWalError::PublicationConflict);
        }
        let reservation = step.reservation();
        Ok(GraphInsertEvent::new(
            reservation.mutation_id(),
            reservation.node_id(),
            GraphInsertEventKind::Published { observed_state },
        ))
    }
}

fn validate_topology_roles(pages: &HnswWalPageSet) -> HnswWalResult<()> {
    validate_data_roles(
        pages,
        &[
            HnswWalPageRole::AdjacencyRecord,
            HnswWalPageRole::MutationDescriptorHeader,
            HnswWalPageRole::MutationDescriptorEntry,
        ],
        &[
            HnswWalPageRole::AdjacencyRecord,
            HnswWalPageRole::DirectoryLocator,
            HnswWalPageRole::MutationDescriptorHeader,
            HnswWalPageRole::MutationDescriptorEntry,
        ],
    )
}

fn descriptor_header_write(transition: GraphMutationDescriptorTransition) -> HnswWalPageWrite {
    let target = transition.target();
    match transition.expected() {
        None => HnswWalPageWrite::CreateDescriptorHeader {
            mutation_id: target.reservation().mutation_id(),
            target_revision: target.descriptor_revision(),
        },
        Some(expected) => HnswWalPageWrite::ReviseDescriptorHeader {
            mutation_id: target.reservation().mutation_id(),
            expected_revision: expected.descriptor_revision(),
            target_revision: target.descriptor_revision(),
        },
    }
}

fn validate_data_roles(
    pages: &HnswWalPageSet,
    required: &[HnswWalPageRole],
    allowed: &[HnswWalPageRole],
) -> HnswWalResult<()> {
    if required
        .iter()
        .any(|role| pages.iter().filter(|page| page.role() == *role).count() != 1)
        || pages.iter().any(|page| !allowed.contains(&page.role()))
    {
        return Err(HnswWalError::InvalidUnit {
            reason: "registered page roles do not match the atomic unit",
        });
    }
    Ok(())
}

pub(super) type HnswWalResult<T> = Result<T, HnswWalError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HnswWalError {
    InvalidPageAction { reason: &'static str },
    InvalidPageSet { reason: &'static str },
    PageLimitExceeded { maximum: usize, actual: usize },
    InvalidUnit { reason: &'static str },
    PublicationConflict,
}

impl fmt::Display for HnswWalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPageAction { reason } => write!(formatter, "invalid WAL page: {reason}"),
            Self::InvalidPageSet { reason } => write!(formatter, "invalid WAL page set: {reason}"),
            Self::PageLimitExceeded { maximum, actual } => write!(
                formatter,
                "WAL page limit exceeded: maximum {maximum}, got {actual}"
            ),
            Self::InvalidUnit { reason } => write!(formatter, "invalid WAL unit: {reason}"),
            Self::PublicationConflict => formatter.write_str("WAL publication state conflict"),
        }
    }
}

impl std::error::Error for HnswWalError {}

#[cfg(test)]
include!("wal_contract/tests.rs");

#[cfg(test)]
mod critical_section_tests;
