/// Repairable partial-mutation classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphRepairReason {
    /// Unpublished node append did not finish.
    InterruptedAppend,
    /// One or more outbound layers are missing.
    InterruptedOutbound,
    /// One or more existing layers were not rewired.
    InterruptedRewire,
    /// Complete topology has not yet been marked ready.
    InterruptedNodePublication,
    /// Ready node was not published in metadata.
    InterruptedPublication,
    /// Pending descriptor references an absent unpublished node.
    MissingUnpublishedNode,
    /// Unpublished adjacency can be discarded or replayed.
    StaleUnpublishedAdjacency,
}

/// Corruption that cannot be repaired from a pending descriptor alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphRebuildReason {
    /// Zero, legacy, or future layout version.
    UnsupportedFormat,
    /// Page role is invalid for its locator.
    InvalidPageKind,
    /// Partial state has no trustworthy mutation descriptor.
    MissingMutationDescriptor,
    /// Published count/root/dimension fields disagree.
    InvalidPublishedState,
    /// A page required by the published generation is corrupt or absent.
    CorruptPublishedPage,
    /// Directory lookup exceeded the fixed page-visit bound.
    DirectoryDepthExceeded,
}

/// Fail-closed graph availability classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphAvailability {
    /// Published state is structurally readable.
    Ready,
    /// Writes freeze while one intact pending descriptor is repaired.
    RepairRequired {
        /// Mutation to resume or discard.
        mutation_id: GraphMutationId,
        /// Repair classification.
        reason: GraphRepairReason,
    },
    /// Serving fails closed until source-authoritative rebuild.
    RebuildRequired {
        /// Rebuild classification.
        reason: GraphRebuildReason,
    },
}

/// Lock acquisition target for one prepared adapter action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphLockTarget {
    /// Relation extension/allocation serialization, held without page locks.
    Extension,
    /// Next-node-id reservation on the metapage, held without other locks.
    Allocator,
    /// Metapage publication, held without data-page locks.
    Meta,
    /// Existing directory/node/adjacency page.
    Data(GraphPageId),
}

/// Validated deterministic lock sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphLockPlan {
    targets: Vec<GraphLockTarget>,
}

impl GraphLockPlan {
    /// Validates the fixed lock protocol.
    ///
    /// Extension and metapage locks are each standalone. Data pages are
    /// strictly ascending, nonzero, unique, and bounded. Adapters release data
    /// page locks in reverse order.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::InvalidLockOrder`] for empty, mixed,
    /// duplicate, descending, zero-page, or oversized plans.
    pub fn new(targets: Vec<GraphLockTarget>) -> GraphMutationResult<Self> {
        if targets.is_empty() || targets.len() > MAX_GRAPH_LOCK_TARGETS {
            return Err(GraphMutationError::InvalidLockOrder {
                reason: "lock target count is empty or exceeds the bound",
            });
        }
        if targets.len() == 1
            && matches!(
                targets[0],
                GraphLockTarget::Extension | GraphLockTarget::Allocator | GraphLockTarget::Meta
            )
        {
            return Ok(Self { targets });
        }
        let mut previous = None;
        for target in &targets {
            let GraphLockTarget::Data(page_id) = target else {
                return Err(GraphMutationError::InvalidLockOrder {
                    reason: "extension, allocator, and metapage locks must be standalone",
                });
            };
            if page_id.get() == 0 || previous.is_some_and(|value| value >= *page_id) {
                return Err(GraphMutationError::InvalidLockOrder {
                    reason: "data pages must be nonzero, unique, and strictly ascending",
                });
            }
            previous = Some(*page_id);
        }
        Ok(Self { targets })
    }

    /// Returns acquisition order. Data plans release in reverse order.
    #[must_use]
    pub fn targets(&self) -> &[GraphLockTarget] {
        &self.targets
    }
}

/// Mutation model result type.
pub type GraphMutationResult<T> = Result<T, GraphMutationError>;

/// Typed failures from mutation planning and replay.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum GraphMutationError {
    /// Prepared plan parameters violate policy or published state.
    #[error("invalid graph mutation plan: {reason}")]
    InvalidPlan {
        /// Stable reason.
        reason: &'static str,
    },
    /// Reader-visible metadata fields disagree.
    #[error("invalid published graph state: {reason}")]
    InvalidPublishedState {
        /// Stable reason.
        reason: &'static str,
    },
    /// Event belongs to another mutation.
    #[error("graph mutation mismatch: expected {expected:?}, got {actual:?}")]
    MutationMismatch {
        /// Expected mutation.
        expected: GraphMutationId,
        /// Actual mutation.
        actual: GraphMutationId,
    },
    /// Event belongs to another node.
    #[error("graph mutation node mismatch: expected {expected:?}, got {actual:?}")]
    NodeMismatch {
        /// Expected node.
        expected: HnswNodeId,
        /// Actual node.
        actual: HnswNodeId,
    },
    /// Event skips or reverses required progress.
    #[error("invalid graph insertion event {event} in phase {phase:?}")]
    InvalidTransition {
        /// Current phase.
        phase: GraphInsertPhase,
        /// Stable event name.
        event: &'static str,
    },
    /// Checked counter or identifier arithmetic overflowed.
    #[error("graph mutation arithmetic overflow while attempting to {operation}")]
    ArithmeticOverflow {
        /// Stable operation name.
        operation: &'static str,
    },
    /// Lock targets violate the fixed acquisition protocol.
    #[error("invalid graph lock order: {reason}")]
    InvalidLockOrder {
        /// Stable reason.
        reason: &'static str,
    },
    /// Optimistic reader state changed before or after root publication.
    #[error(
        "graph publication conflict: expected generation {expected_generation} mutation {expected_mutation_id:?}, got generation {actual_generation} mutation {actual_mutation_id:?}"
    )]
    PublicationConflict {
        /// Expected published generation.
        expected_generation: u64,
        /// Expected publication identity.
        expected_mutation_id: Option<GraphMutationId>,
        /// Observed published generation.
        actual_generation: u64,
        /// Observed publication identity.
        actual_mutation_id: Option<GraphMutationId>,
    },
    /// Expected next-node-id watermark was stale under the allocator lock.
    #[error("graph allocation conflict: expected {expected:?}, got {actual:?}")]
    AllocationConflict {
        /// Watermark used while preparing the reservation.
        expected: HnswNodeId,
        /// Watermark observed under the allocator lock.
        actual: HnswNodeId,
    },
    /// Too many pending mutation reservations are retained.
    #[error("pending graph mutations exceed limit {maximum}")]
    PendingMutationLimit {
        /// Maximum retained reservations.
        maximum: usize,
    },
}
