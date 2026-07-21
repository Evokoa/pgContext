use context_index::{
    GraphMutationDescriptorEntry, GraphMutationDescriptorTransition, GraphMutationId, GraphPageId,
    GraphPageKind, GraphRecordId, GraphRecordRevision, HnswNodeId, LayerIndex,
};

use crate::hnsw_am::mvcc_contract::{HnswBoundTombstone, HnswHeapTid, HnswTombstoneEpoch};

use super::{HnswWalError, HnswWalResult, MAX_HNSW_WAL_PAGES, descriptor_header_write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::hnsw_am) enum HnswWalImage {
    Delta,
    FullImage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::hnsw_am) enum HnswWalPageRole {
    AllocatorState,
    PageInitialization,
    DirectoryLocator,
    NodeRecord,
    AdjacencyRecord,
    MutationDescriptorHeader,
    MutationDescriptorEntry,
    MetadataPublication,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::hnsw_am) enum HnswWalDirectoryWrite {
    InsertNodeAndDescriptor {
        generation: u64,
        node_id: HnswNodeId,
        mutation_id: GraphMutationId,
    },
    InsertAdjacencyAndEntry {
        generation: u64,
        node_id: HnswNodeId,
        layer: LayerIndex,
        mutation_id: GraphMutationId,
        ordinal: usize,
    },
    RemoveDescriptor {
        mutation_id: GraphMutationId,
        expected_revision: GraphRecordRevision,
    },
    InsertTombstoneLocator {
        generation: u64,
        node_id: HnswNodeId,
        record_id: GraphRecordId,
        expected_revision: GraphRecordRevision,
        target_revision: GraphRecordRevision,
        heap_tid: HnswHeapTid,
        tombstone_epoch: HnswTombstoneEpoch,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::hnsw_am) enum HnswWalPageWrite {
    AllocatorState,
    PageInitialization,
    Directory(HnswWalDirectoryWrite),
    AppendNode {
        generation: u64,
        node_id: HnswNodeId,
    },
    ReviseNode {
        generation: u64,
        node_id: HnswNodeId,
        expected_revision: GraphRecordRevision,
    },
    TombstoneNode {
        generation: u64,
        node_id: HnswNodeId,
        record_id: GraphRecordId,
        expected_revision: GraphRecordRevision,
        target_revision: GraphRecordRevision,
        heap_tid: HnswHeapTid,
        tombstone_epoch: HnswTombstoneEpoch,
    },
    AppendAdjacency {
        generation: u64,
        node_id: HnswNodeId,
        layer: LayerIndex,
        expected_revision: Option<GraphRecordRevision>,
    },
    CreateDescriptorHeader {
        mutation_id: GraphMutationId,
        target_revision: GraphRecordRevision,
    },
    ReviseDescriptorHeader {
        mutation_id: GraphMutationId,
        expected_revision: GraphRecordRevision,
        target_revision: GraphRecordRevision,
    },
    InsertDescriptorEntry {
        mutation_id: GraphMutationId,
        ordinal: usize,
    },
    DeleteDescriptorHeader {
        mutation_id: GraphMutationId,
        expected_revision: GraphRecordRevision,
    },
    MetadataPublication,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::hnsw_am) struct HnswWalPageAction {
    page_id: GraphPageId,
    kind: GraphPageKind,
    image: HnswWalImage,
    write: HnswWalPageWrite,
}

impl HnswWalPageAction {
    #[cfg(test)]
    pub(super) fn existing(
        page_id: GraphPageId,
        kind: GraphPageKind,
        role: HnswWalPageRole,
    ) -> HnswWalResult<Self> {
        let fixture_mutation = GraphMutationId::new(1).ok_or(HnswWalError::InvalidPageAction {
            reason: "fixture mutation id is invalid",
        })?;
        let write = match (kind, role) {
            (GraphPageKind::Meta, HnswWalPageRole::AllocatorState) if page_id.get() == 0 => {
                HnswWalPageWrite::AllocatorState
            }
            (GraphPageKind::Meta, HnswWalPageRole::MetadataPublication) if page_id.get() == 0 => {
                HnswWalPageWrite::MetadataPublication
            }
            (GraphPageKind::Directory, HnswWalPageRole::DirectoryLocator) if page_id.get() > 0 => {
                HnswWalPageWrite::Directory(HnswWalDirectoryWrite::InsertNodeAndDescriptor {
                    generation: 1,
                    node_id: HnswNodeId::new(0),
                    mutation_id: fixture_mutation,
                })
            }
            (GraphPageKind::Node, HnswWalPageRole::NodeRecord) if page_id.get() > 0 => {
                HnswWalPageWrite::AppendNode {
                    generation: 1,
                    node_id: HnswNodeId::new(0),
                }
            }
            (GraphPageKind::Adjacency, HnswWalPageRole::AdjacencyRecord) if page_id.get() > 0 => {
                HnswWalPageWrite::AppendAdjacency {
                    generation: 1,
                    node_id: HnswNodeId::new(0),
                    layer: LayerIndex::base(),
                    expected_revision: None,
                }
            }
            (GraphPageKind::MutationDescriptor, HnswWalPageRole::MutationDescriptorHeader)
                if page_id.get() > 0 =>
            {
                HnswWalPageWrite::CreateDescriptorHeader {
                    mutation_id: fixture_mutation,
                    target_revision: GraphRecordRevision::new(0),
                }
            }
            (GraphPageKind::MutationDescriptor, HnswWalPageRole::MutationDescriptorEntry)
                if page_id.get() > 0 =>
            {
                HnswWalPageWrite::InsertDescriptorEntry {
                    mutation_id: fixture_mutation,
                    ordinal: 0,
                }
            }
            _ => {
                return Err(HnswWalError::InvalidPageAction {
                    reason: "page id, kind, and semantic role do not match",
                });
            }
        };
        Ok(Self::existing_write(page_id, kind, write))
    }

    fn existing_write(page_id: GraphPageId, kind: GraphPageKind, write: HnswWalPageWrite) -> Self {
        Self {
            page_id,
            kind,
            image: HnswWalImage::Delta,
            write,
        }
    }

    fn data_write(
        page_id: GraphPageId,
        kind: GraphPageKind,
        write: HnswWalPageWrite,
    ) -> HnswWalResult<Self> {
        if page_id.get() == 0 || kind == GraphPageKind::Meta {
            return Err(HnswWalError::InvalidPageAction {
                reason: "data WAL writes require a nonzero non-meta page",
            });
        }
        Ok(Self::existing_write(page_id, kind, write))
    }

    pub(in crate::hnsw_am) fn directory(
        page_id: GraphPageId,
        write: HnswWalDirectoryWrite,
    ) -> HnswWalResult<Self> {
        if matches!(
            write,
            HnswWalDirectoryWrite::InsertNodeAndDescriptor { generation: 0, .. }
                | HnswWalDirectoryWrite::InsertAdjacencyAndEntry { generation: 0, .. }
        ) {
            return Err(HnswWalError::InvalidPageAction {
                reason: "versioned directory inserts require a nonzero generation",
            });
        }
        Self::data_write(
            page_id,
            GraphPageKind::Directory,
            HnswWalPageWrite::Directory(write),
        )
    }

    pub(in crate::hnsw_am) fn append_node(
        page_id: GraphPageId,
        generation: u64,
        node_id: HnswNodeId,
    ) -> HnswWalResult<Self> {
        if generation == 0 {
            return Err(HnswWalError::InvalidPageAction {
                reason: "node append generation must be nonzero",
            });
        }
        Self::data_write(
            page_id,
            GraphPageKind::Node,
            HnswWalPageWrite::AppendNode {
                generation,
                node_id,
            },
        )
    }

    pub(in crate::hnsw_am) fn revise_node(
        page_id: GraphPageId,
        generation: u64,
        node_id: HnswNodeId,
        expected_revision: GraphRecordRevision,
    ) -> HnswWalResult<Self> {
        if generation == 0 {
            return Err(HnswWalError::InvalidPageAction {
                reason: "node revision generation must be nonzero",
            });
        }
        Self::data_write(
            page_id,
            GraphPageKind::Node,
            HnswWalPageWrite::ReviseNode {
                generation,
                node_id,
                expected_revision,
            },
        )
    }

    pub(in crate::hnsw_am) fn append_adjacency(
        page_id: GraphPageId,
        generation: u64,
        node_id: HnswNodeId,
        layer: LayerIndex,
        expected_revision: Option<GraphRecordRevision>,
    ) -> HnswWalResult<Self> {
        if generation == 0 {
            return Err(HnswWalError::InvalidPageAction {
                reason: "adjacency append generation must be nonzero",
            });
        }
        Self::data_write(
            page_id,
            GraphPageKind::Adjacency,
            HnswWalPageWrite::AppendAdjacency {
                generation,
                node_id,
                layer,
                expected_revision,
            },
        )
    }

    pub(in crate::hnsw_am) fn tombstone_node(
        page_id: GraphPageId,
        bound: HnswBoundTombstone,
    ) -> HnswWalResult<Self> {
        Self::data_write(page_id, GraphPageKind::Node, tombstone_node_write(bound))
    }

    pub(in crate::hnsw_am) fn descriptor_header(
        page_id: GraphPageId,
        transition: GraphMutationDescriptorTransition,
    ) -> HnswWalResult<Self> {
        Self::data_write(
            page_id,
            GraphPageKind::MutationDescriptor,
            descriptor_header_write(transition),
        )
    }

    pub(in crate::hnsw_am) fn descriptor_entry(
        page_id: GraphPageId,
        entry: &GraphMutationDescriptorEntry,
    ) -> HnswWalResult<Self> {
        Self::data_write(
            page_id,
            GraphPageKind::MutationDescriptor,
            HnswWalPageWrite::InsertDescriptorEntry {
                mutation_id: entry.step().mutation_id(),
                ordinal: entry.ordinal(),
            },
        )
    }

    pub(in crate::hnsw_am) fn delete_descriptor_header(
        page_id: GraphPageId,
        mutation_id: GraphMutationId,
        expected_revision: GraphRecordRevision,
    ) -> HnswWalResult<Self> {
        Self::data_write(
            page_id,
            GraphPageKind::MutationDescriptor,
            HnswWalPageWrite::DeleteDescriptorHeader {
                mutation_id,
                expected_revision,
            },
        )
    }

    pub(super) fn initialization(page_id: GraphPageId, kind: GraphPageKind) -> HnswWalResult<Self> {
        if page_id.get() == 0 || kind == GraphPageKind::Meta {
            return Err(HnswWalError::InvalidPageAction {
                reason: "page initialization requires one nonzero non-meta page",
            });
        }
        Ok(Self {
            page_id,
            kind,
            image: HnswWalImage::FullImage,
            write: HnswWalPageWrite::PageInitialization,
        })
    }

    pub(super) const fn meta_allocator() -> Self {
        Self {
            page_id: GraphPageId::new(0),
            kind: GraphPageKind::Meta,
            image: HnswWalImage::Delta,
            write: HnswWalPageWrite::AllocatorState,
        }
    }

    pub(super) const fn meta_publication() -> Self {
        Self {
            page_id: GraphPageId::new(0),
            kind: GraphPageKind::Meta,
            image: HnswWalImage::Delta,
            write: HnswWalPageWrite::MetadataPublication,
        }
    }

    pub(in crate::hnsw_am) const fn page_id(&self) -> GraphPageId {
        self.page_id
    }

    pub(in crate::hnsw_am) const fn kind(&self) -> GraphPageKind {
        self.kind
    }

    pub(in crate::hnsw_am) const fn image(&self) -> HnswWalImage {
        self.image
    }

    pub(in crate::hnsw_am) const fn role(&self) -> HnswWalPageRole {
        match self.write {
            HnswWalPageWrite::AllocatorState => HnswWalPageRole::AllocatorState,
            HnswWalPageWrite::PageInitialization => HnswWalPageRole::PageInitialization,
            HnswWalPageWrite::Directory(_) => HnswWalPageRole::DirectoryLocator,
            HnswWalPageWrite::AppendNode { .. }
            | HnswWalPageWrite::ReviseNode { .. }
            | HnswWalPageWrite::TombstoneNode { .. } => HnswWalPageRole::NodeRecord,
            HnswWalPageWrite::AppendAdjacency { .. } => HnswWalPageRole::AdjacencyRecord,
            HnswWalPageWrite::CreateDescriptorHeader { .. }
            | HnswWalPageWrite::ReviseDescriptorHeader { .. }
            | HnswWalPageWrite::DeleteDescriptorHeader { .. } => {
                HnswWalPageRole::MutationDescriptorHeader
            }
            HnswWalPageWrite::InsertDescriptorEntry { .. } => {
                HnswWalPageRole::MutationDescriptorEntry
            }
            HnswWalPageWrite::MetadataPublication => HnswWalPageRole::MetadataPublication,
        }
    }

    pub(in crate::hnsw_am) const fn write(&self) -> HnswWalPageWrite {
        self.write
    }
}

pub(super) const fn tombstone_node_write(bound: HnswBoundTombstone) -> HnswWalPageWrite {
    let step = bound.step();
    HnswWalPageWrite::TombstoneNode {
        generation: step.target_generation(),
        node_id: step.node_id(),
        record_id: step.record_id(),
        expected_revision: step.expected_revision(),
        target_revision: step.target_revision(),
        heap_tid: bound.heap_tid(),
        tombstone_epoch: bound.tombstone_epoch(),
    }
}

pub(super) const fn tombstone_locator_write(bound: HnswBoundTombstone) -> HnswWalDirectoryWrite {
    let step = bound.step();
    HnswWalDirectoryWrite::InsertTombstoneLocator {
        generation: step.target_generation(),
        node_id: step.node_id(),
        record_id: step.record_id(),
        expected_revision: step.expected_revision(),
        target_revision: step.target_revision(),
        heap_tid: bound.heap_tid(),
        tombstone_epoch: bound.tombstone_epoch(),
    }
}

const EMPTY_PAGE_ACTION: HnswWalPageAction = HnswWalPageAction::meta_allocator();

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::hnsw_am) struct HnswWalPageSet {
    actions: [HnswWalPageAction; MAX_HNSW_WAL_PAGES],
    len: usize,
}

impl HnswWalPageSet {
    pub(in crate::hnsw_am) fn new(actions: &[HnswWalPageAction]) -> HnswWalResult<Self> {
        if actions.is_empty() {
            return Err(HnswWalError::InvalidPageSet {
                reason: "a WAL unit must register at least one page",
            });
        }
        if actions.len() > MAX_HNSW_WAL_PAGES {
            return Err(HnswWalError::PageLimitExceeded {
                maximum: MAX_HNSW_WAL_PAGES,
                actual: actions.len(),
            });
        }
        if actions.len() > 1 && actions.iter().any(|action| action.page_id().get() == 0) {
            return Err(HnswWalError::InvalidPageSet {
                reason: "allocator and publication metapage actions must be standalone",
            });
        }
        if actions
            .windows(2)
            .any(|pair| pair[0].page_id() >= pair[1].page_id())
        {
            return Err(HnswWalError::InvalidPageSet {
                reason: "registered pages must be unique and strictly ascending",
            });
        }
        let mut fixed = [EMPTY_PAGE_ACTION; MAX_HNSW_WAL_PAGES];
        fixed[..actions.len()].copy_from_slice(actions);
        Ok(Self {
            actions: fixed,
            len: actions.len(),
        })
    }

    pub(super) const fn singleton(action: HnswWalPageAction) -> Self {
        let mut actions = [EMPTY_PAGE_ACTION; MAX_HNSW_WAL_PAGES];
        actions[0] = action;
        Self { actions, len: 1 }
    }

    pub(in crate::hnsw_am) const fn len(&self) -> usize {
        self.len
    }

    pub(in crate::hnsw_am) fn iter(&self) -> core::slice::Iter<'_, HnswWalPageAction> {
        self.actions[..self.len].iter()
    }

    pub(super) fn contains_write(&self, role: HnswWalPageRole, write: HnswWalPageWrite) -> bool {
        self.iter()
            .any(|action| action.role() == role && action.write() == write)
    }
}
