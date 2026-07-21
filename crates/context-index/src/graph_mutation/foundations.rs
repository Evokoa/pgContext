use core::num::NonZeroU64;

use context_core::DenseVector;

use crate::{
    GraphNeighbors, GraphRecordId, HnswNodeId, MAX_GRAPH_LAYERS, MAX_GRAPH_NEIGHBORS_PER_LAYER,
};

/// Current logical graph layout version.
///
/// Version one names the legacy unversioned/native-endian prototype. The new
/// logical layout starts at two so an adapter cannot reinterpret old bytes.
pub const CURRENT_GRAPH_LAYOUT_VERSION: u16 = 2;

/// Canonical four-byte magic for future version-two graph page payloads.
pub const GRAPH_PAGE_MAGIC: [u8; 4] = *b"PGH2";

/// Fixed common payload-header width for version-two graph pages.
pub const GRAPH_PAGE_HEADER_BYTES: usize = 32;

/// Maximum bounded directory pages a single identity lookup may visit.
pub const MAX_GRAPH_DIRECTORY_DEPTH: usize = 8;

/// Maximum concurrent pending mutation reservations retained by an allocator.
pub const MAX_PENDING_GRAPH_MUTATIONS: usize = 128;

/// Logical bytes reserved for one metapage mutation-id/node-id reservation.
pub const GRAPH_PENDING_RESERVATION_BYTES: usize = 16;

/// Total logical metapage bytes reserved for pending reservations.
pub const GRAPH_PENDING_RESERVATION_REGION_BYTES: usize =
    MAX_PENDING_GRAPH_MUTATIONS * GRAPH_PENDING_RESERVATION_BYTES;

/// Maximum number of page locks one prepared adapter action may request.
pub const MAX_GRAPH_LOCK_TARGETS: usize = 8;

const _: () = assert!(MAX_GRAPH_LAYERS <= u16::MAX as usize);
const _: () =
    assert!(MAX_GRAPH_LAYERS.saturating_mul(MAX_GRAPH_NEIGHBORS_PER_LAYER) <= u16::MAX as usize);

/// Identifier for one semantic graph mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GraphMutationId(NonZeroU64);

impl GraphMutationId {
    /// Creates a mutation identifier.
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Returns the raw mutation identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Monotone revision for one node or adjacency record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GraphRecordRevision(u64);

impl GraphRecordRevision {
    /// Creates a record revision.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the revision value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Validated nonzero count of graph layers stored for one node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GraphLayerCount(usize);

impl GraphLayerCount {
    /// Creates a bounded layer count.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::InvalidPlan`] for zero or values above
    /// [`MAX_GRAPH_LAYERS`].
    pub fn new(value: usize) -> GraphMutationResult<Self> {
        if value == 0 || value > MAX_GRAPH_LAYERS {
            return Err(GraphMutationError::InvalidPlan {
                reason: "graph layer count is zero or exceeds the bound",
            });
        }
        Ok(Self(value))
    }

    /// Returns the validated count.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Logical graph layout version carried by every adapter page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GraphFormatVersion(u16);

impl GraphFormatVersion {
    /// Returns the current graph layout version.
    #[must_use]
    pub const fn current() -> Self {
        Self(CURRENT_GRAPH_LAYOUT_VERSION)
    }

    /// Returns the raw version value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// Classifies decoded version bytes without silently defaulting them.
    #[must_use]
    pub const fn classify(value: u16) -> GraphFormatDisposition {
        if value == CURRENT_GRAPH_LAYOUT_VERSION {
            GraphFormatDisposition::Current(Self(value))
        } else {
            GraphFormatDisposition::RebuildRequired { decoded: value }
        }
    }
}

/// Fail-closed result of decoding a layout version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphFormatDisposition {
    /// The page uses the current logical layout.
    Current(GraphFormatVersion),
    /// Zero, legacy, or future versions require a rebuild.
    RebuildRequired {
        /// Unsupported decoded version.
        decoded: u16,
    },
}

impl GraphFormatDisposition {
    /// Returns whether serving must fail closed pending a rebuild.
    #[must_use]
    pub const fn is_rebuild_required(self) -> bool {
        matches!(self, Self::RebuildRequired { .. })
    }
}

/// Logical role of a versioned graph page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GraphPageKind {
    /// Published root, counters, directory root, and graph availability.
    Meta,
    /// Bounded lookup from graph identities to record locators.
    Directory,
    /// Node payload, publication state, and record revision.
    Node,
    /// One complete `(node, layer)` adjacency record and revision.
    Adjacency,
    /// Persisted mutation intent, progress, and revision-checked repair data.
    MutationDescriptor,
    /// One appended segmented-write-path delta record (live insert or
    /// tombstone), absorbing writes without a full graph splice.
    Delta,
}

impl GraphPageKind {
    /// Every page role in stable declaration order.
    pub const ALL: [Self; 6] = [
        Self::Meta,
        Self::Directory,
        Self::Node,
        Self::Adjacency,
        Self::MutationDescriptor,
        Self::Delta,
    ];

    /// Returns the stable version-two page-kind code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Meta => 1,
            Self::Directory => 2,
            Self::Node => 3,
            Self::Adjacency => 4,
            Self::MutationDescriptor => 5,
            Self::Delta => 6,
        }
    }

    /// Returns the page role for a stable version-two page-kind code, or
    /// `None` when the code names no known role.
    ///
    /// Derived from [`Self::ALL`] so decoders cannot silently reject a role
    /// added to the enum: a new variant becomes decodable as soon as it joins
    /// `ALL`, rather than requiring a parallel match arm to be updated too.
    #[must_use]
    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.code() == code)
    }
}

/// Stable key roles stored by the bounded graph directories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GraphDirectoryKeyKind {
    /// Locates one node record by node id.
    Node,
    /// Locates one adjacency record by node id and layer.
    Adjacency,
    /// Locates one mutation descriptor header by mutation id.
    MutationDescriptor,
    /// Locates one complete descriptor entry by mutation id and ordinal.
    MutationEntry,
}

impl GraphDirectoryKeyKind {
    /// Every key role in stable declaration order.
    pub const ALL: [Self; 4] = [
        Self::Node,
        Self::Adjacency,
        Self::MutationDescriptor,
        Self::MutationEntry,
    ];

    /// Returns the stable version-two directory-key code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Node => 1,
            Self::Adjacency => 2,
            Self::MutationDescriptor => 3,
            Self::MutationEntry => 4,
        }
    }
}

/// Adapter-owned page ordinal used only for lock ordering and locators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GraphPageId(u64);

impl GraphPageId {
    /// Creates a page ordinal.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the page ordinal.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Validated number of directory pages visited by one identity lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GraphDirectoryDepth(usize);

impl GraphDirectoryDepth {
    /// Creates a bounded directory depth.
    ///
    /// # Errors
    ///
    /// Returns [`GraphRebuildReason::DirectoryDepthExceeded`] instead of
    /// permitting an unbounded scan.
    pub fn new(value: usize) -> Result<Self, GraphRebuildReason> {
        if value == 0 || value > MAX_GRAPH_DIRECTORY_DEPTH {
            return Err(GraphRebuildReason::DirectoryDepthExceeded);
        }
        Ok(Self(value))
    }

    /// Returns the bounded page visit count.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Logical header fields every future physical graph page must expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphPageHeader {
    format_version: GraphFormatVersion,
    kind: GraphPageKind,
    generation: u64,
    mutation_id: Option<GraphMutationId>,
}

impl GraphPageHeader {
    /// Creates a logical page header contract.
    #[must_use]
    pub const fn new(
        kind: GraphPageKind,
        generation: u64,
        mutation_id: Option<GraphMutationId>,
    ) -> Self {
        Self {
            format_version: GraphFormatVersion::current(),
            kind,
            generation,
            mutation_id,
        }
    }

    /// Returns the page layout version.
    #[must_use]
    pub const fn format_version(self) -> GraphFormatVersion {
        self.format_version
    }

    /// Returns the page role.
    #[must_use]
    pub const fn kind(self) -> GraphPageKind {
        self.kind
    }

    /// Returns the published generation associated with this page.
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Returns the pending mutation, when the page is unpublished state.
    #[must_use]
    pub const fn mutation_id(self) -> Option<GraphMutationId> {
        self.mutation_id
    }
}

/// Only metadata visible to graph readers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphPublishedState {
    generation: u64,
    node_count: usize,
    tombstone_count: usize,
    entry_point: Option<HnswNodeId>,
    dimensions: Option<usize>,
    format_version: GraphFormatVersion,
    last_mutation_id: Option<GraphMutationId>,
}

impl GraphPublishedState {
    /// Returns a valid empty published state.
    #[must_use]
    pub const fn empty(format_version: GraphFormatVersion) -> Self {
        Self {
            generation: 0,
            node_count: 0,
            tombstone_count: 0,
            entry_point: None,
            dimensions: None,
            format_version,
            last_mutation_id: None,
        }
    }

    /// Creates validated reader-visible state.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::InvalidPublishedState`] when empty and
    /// nonempty fields conflict or dimensions are zero.
    pub fn new(
        generation: u64,
        node_count: usize,
        entry_point: Option<HnswNodeId>,
        dimensions: Option<usize>,
        format_version: GraphFormatVersion,
        last_mutation_id: Option<GraphMutationId>,
    ) -> GraphMutationResult<Self> {
        Self::new_with_tombstones(
            generation,
            node_count,
            0,
            entry_point,
            dimensions,
            format_version,
            last_mutation_id,
        )
    }

    /// Creates validated reader-visible state with an explicit tombstone count.
    ///
    /// `node_count` is the structural published node span; tombstones remain
    /// topology connectors and are counted separately from result candidates.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::InvalidPublishedState`] when counts or
    /// empty/nonempty metadata disagree.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_tombstones(
        generation: u64,
        node_count: usize,
        tombstone_count: usize,
        entry_point: Option<HnswNodeId>,
        dimensions: Option<usize>,
        format_version: GraphFormatVersion,
        last_mutation_id: Option<GraphMutationId>,
    ) -> GraphMutationResult<Self> {
        if (node_count == 0
            && (generation != 0
                || tombstone_count != 0
                || entry_point.is_some()
                || dimensions.is_some()
                || last_mutation_id.is_some()))
            || (node_count > 0
                && (entry_point.is_none() || dimensions.is_none() || last_mutation_id.is_none()))
            || (node_count > 0 && generation == 0)
            || tombstone_count > node_count
            || dimensions == Some(0)
        {
            return Err(GraphMutationError::InvalidPublishedState {
                reason: "published/tombstone counts, entry point, and dimensions disagree",
            });
        }
        Ok(Self {
            generation,
            node_count,
            tombstone_count,
            entry_point,
            dimensions,
            format_version,
            last_mutation_id,
        })
    }

    /// Returns the reader-visible generation.
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Returns the structural count of published nodes, including tombstones.
    #[must_use]
    pub const fn node_count(self) -> usize {
        self.node_count
    }

    /// Returns the count of published traversal-only tombstones.
    #[must_use]
    pub const fn tombstone_count(self) -> usize {
        self.tombstone_count
    }

    /// Returns the count of nodes that may become result candidates after
    /// authoritative source recheck.
    #[must_use]
    pub const fn candidate_node_count(self) -> usize {
        self.node_count - self.tombstone_count
    }

    /// Returns the published traversal root.
    #[must_use]
    pub const fn entry_point(self) -> Option<HnswNodeId> {
        self.entry_point
    }

    /// Returns the published vector dimension.
    #[must_use]
    pub const fn dimensions(self) -> Option<usize> {
        self.dimensions
    }

    /// Returns the published layout version.
    #[must_use]
    pub const fn format_version(self) -> GraphFormatVersion {
        self.format_version
    }

    /// Returns the mutation identity committed with this published state.
    #[must_use]
    pub const fn last_mutation_id(self) -> Option<GraphMutationId> {
        self.last_mutation_id
    }
}
