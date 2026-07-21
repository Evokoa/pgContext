/// Independent node-id allocator state; it is never a published node count.
#[derive(Debug, PartialEq, Eq)]
pub struct GraphAllocationState {
    next_node_id: HnswNodeId,
    reservations: [Option<GraphNodeReservation>; MAX_PENDING_GRAPH_MUTATIONS],
}

impl GraphAllocationState {
    /// Creates allocator state from a persisted next-id watermark.
    #[must_use]
    pub const fn new(next_node_id: HnswNodeId) -> Self {
        Self {
            next_node_id,
            reservations: [None; MAX_PENDING_GRAPH_MUTATIONS],
        }
    }

    /// Reserves one never-reused graph node id.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::ArithmeticOverflow`] at identifier
    /// exhaustion. Reservation advances the watermark even if later work is
    /// abandoned, so interrupted insertions may leave safe holes.
    pub fn reserve(
        &mut self,
        mutation_id: GraphMutationId,
        expected_next: HnswNodeId,
    ) -> GraphMutationResult<GraphNodeReservation> {
        if let Some(reservation) = self
            .reservations
            .iter()
            .flatten()
            .find(|reservation| reservation.mutation_id() == mutation_id)
        {
            return Ok(*reservation);
        }
        if expected_next != self.next_node_id {
            return Err(GraphMutationError::AllocationConflict {
                expected: expected_next,
                actual: self.next_node_id,
            });
        }
        let Some(slot) = self.reservations.iter().position(Option::is_none) else {
            return Err(GraphMutationError::PendingMutationLimit {
                maximum: MAX_PENDING_GRAPH_MUTATIONS,
            });
        };
        let reserved = self.next_node_id;
        let next = reserved
            .get()
            .checked_add(1)
            .ok_or(GraphMutationError::ArithmeticOverflow {
                operation: "reserve graph node id",
            })?;
        self.reservations[slot] = Some(GraphNodeReservation {
            mutation_id,
            node_id: reserved,
        });
        self.next_node_id = HnswNodeId::new(next);
        Ok(GraphNodeReservation {
            mutation_id,
            node_id: reserved,
        })
    }

    /// Returns the next never-reserved node id.
    #[must_use]
    pub const fn next_node_id(&self) -> HnswNodeId {
        self.next_node_id
    }

    /// Releases one completed or explicitly discarded pending descriptor.
    ///
    /// Mutation identifiers are monotone adapter identities and must never be
    /// reused after release. The node-id watermark is not decreased.
    pub fn release(&mut self, mutation_id: GraphMutationId) -> bool {
        let Some(slot) = self
            .reservations
            .iter_mut()
            .find(|slot| slot.is_some_and(|reservation| reservation.mutation_id() == mutation_id))
        else {
            return false;
        };
        *slot = None;
        true
    }
}

/// One node-id reservation owned by a mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphNodeReservation {
    mutation_id: GraphMutationId,
    node_id: HnswNodeId,
}

impl GraphNodeReservation {
    /// Returns the owning mutation.
    #[must_use]
    pub const fn mutation_id(self) -> GraphMutationId {
        self.mutation_id
    }

    /// Returns the reserved node id.
    #[must_use]
    pub const fn node_id(self) -> HnswNodeId {
        self.node_id
    }
}

/// Persistable publication state for a node record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphNodePublication {
    /// Readers ignore the node and edges targeting it.
    Unpublished,
    /// Node payload and all required rewires are complete.
    Ready,
    /// Source tuple is dead to every supported snapshot; topology is retained
    /// for traversal but the node can never become a result candidate.
    Tombstoned,
}

/// Validated unpublished-node append payload.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphAppendStep {
    reservation: GraphNodeReservation,
    record_id: GraphRecordId,
    vector: DenseVector,
    layer_count: GraphLayerCount,
    target_generation: u64,
}

impl GraphAppendStep {
    /// Returns the reserved identity bound to this append.
    #[must_use]
    pub const fn reservation(&self) -> GraphNodeReservation {
        self.reservation
    }

    /// Returns the adapter record token.
    #[must_use]
    pub const fn record_id(&self) -> GraphRecordId {
        self.record_id
    }

    /// Returns the owned vector payload.
    #[must_use]
    pub const fn vector(&self) -> &DenseVector {
        &self.vector
    }

    /// Returns the bounded layer count.
    #[must_use]
    pub const fn layer_count(&self) -> GraphLayerCount {
        self.layer_count
    }

    /// Returns the unpublished generation this node belongs to.
    #[must_use]
    pub const fn target_generation(&self) -> u64 {
        self.target_generation
    }
}

/// Validated complete adjacency write payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphAdjacencyStep {
    mutation_id: GraphMutationId,
    inserted_node_id: HnswNodeId,
    adjacency: GraphNeighbors,
    expected_revision: Option<GraphRecordRevision>,
    target_generation: u64,
}

impl GraphAdjacencyStep {
    /// Returns the owning mutation.
    #[must_use]
    pub const fn mutation_id(&self) -> GraphMutationId {
        self.mutation_id
    }

    /// Returns the inserted node whose topology this action advances.
    #[must_use]
    pub const fn inserted_node_id(&self) -> HnswNodeId {
        self.inserted_node_id
    }

    /// Returns the complete bounded adjacency.
    #[must_use]
    pub const fn adjacency(&self) -> &GraphNeighbors {
        &self.adjacency
    }

    /// Returns the optimistic revision for replacement steps.
    #[must_use]
    pub const fn expected_revision(&self) -> Option<GraphRecordRevision> {
        self.expected_revision
    }

    /// Returns the generation this complete adjacency revision belongs to.
    #[must_use]
    pub const fn target_generation(&self) -> u64 {
        self.target_generation
    }
}

/// Validated ready-state mutation payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphReadyStep {
    reservation: GraphNodeReservation,
    expected_revision: GraphRecordRevision,
    target_generation: u64,
}

impl GraphReadyStep {
    /// Returns the reserved node identity.
    #[must_use]
    pub const fn reservation(self) -> GraphNodeReservation {
        self.reservation
    }

    /// Returns the required node revision.
    #[must_use]
    pub const fn expected_revision(self) -> GraphRecordRevision {
        self.expected_revision
    }

    /// Returns the generation whose node becomes ready.
    #[must_use]
    pub const fn target_generation(self) -> u64 {
        self.target_generation
    }
}

/// Validated final reader-state publication payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphPublicationStep {
    reservation: GraphNodeReservation,
    expected_state: GraphPublishedState,
    new_state: GraphPublishedState,
}

/// Persisted resumable header for one in-flight graph mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphMutationDescriptor {
    reservation: GraphNodeReservation,
    record_id: GraphRecordId,
    descriptor_revision: GraphRecordRevision,
    phase: GraphInsertPhase,
    outbound_total: usize,
    outbound_completed: usize,
    rewires_total: usize,
    rewires_completed: usize,
    expected_state: GraphPublishedState,
    target_state: GraphPublishedState,
}

impl GraphMutationDescriptor {
    /// Returns the reservation owned by this descriptor.
    #[must_use]
    pub const fn reservation(self) -> GraphNodeReservation {
        self.reservation
    }

    /// Returns the adapter-owned node-record identity.
    #[must_use]
    pub const fn record_id(self) -> GraphRecordId {
        self.record_id
    }

    /// Returns the optimistic descriptor-record revision.
    #[must_use]
    pub const fn descriptor_revision(self) -> GraphRecordRevision {
        self.descriptor_revision
    }

    /// Returns the persisted insertion phase.
    #[must_use]
    pub const fn phase(self) -> GraphInsertPhase {
        self.phase
    }

    /// Returns `(completed, total)` outbound-layer progress.
    #[must_use]
    pub const fn outbound_progress(self) -> (usize, usize) {
        (self.outbound_completed, self.outbound_total)
    }

    /// Returns `(completed, total)` neighbor-rewire progress.
    #[must_use]
    pub const fn rewire_progress(self) -> (usize, usize) {
        (self.rewires_completed, self.rewires_total)
    }

    /// Returns the complete reader state required before publication.
    #[must_use]
    pub const fn expected_state(self) -> GraphPublishedState {
        self.expected_state
    }

    /// Returns the complete reader state installed by publication.
    #[must_use]
    pub const fn target_state(self) -> GraphPublishedState {
        self.target_state
    }
}

/// Exact optimistic transition for one persisted mutation descriptor header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphMutationDescriptorTransition {
    expected: Option<GraphMutationDescriptor>,
    target: GraphMutationDescriptor,
}

impl GraphMutationDescriptorTransition {
    /// Creates the first persisted descriptor header at revision zero.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::InvalidPlan`] unless the target revision
    /// is zero.
    pub fn create(target: GraphMutationDescriptor) -> GraphMutationResult<Self> {
        if target.descriptor_revision().get() != 0 {
            return Err(GraphMutationError::InvalidPlan {
                reason: "new mutation descriptor must start at revision zero",
            });
        }
        Ok(Self {
            expected: None,
            target,
        })
    }

    /// Creates one exact compare-and-replace descriptor transition.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::InvalidPlan`] unless identity, totals,
    /// reader states, and monotone progress agree and revision advances by one.
    pub fn advance(
        expected: GraphMutationDescriptor,
        target: GraphMutationDescriptor,
    ) -> GraphMutationResult<Self> {
        let next_revision = expected.descriptor_revision().get().checked_add(1).ok_or(
            GraphMutationError::ArithmeticOverflow {
                operation: "advance mutation descriptor revision",
            },
        )?;
        let identity_matches = expected.reservation() == target.reservation()
            && expected.record_id() == target.record_id()
            && expected.expected_state() == target.expected_state()
            && expected.target_state() == target.target_state()
            && expected.outbound_total == target.outbound_total
            && expected.rewires_total == target.rewires_total;
        let progress_is_monotone = expected.outbound_completed <= target.outbound_completed
            && expected.rewires_completed <= target.rewires_completed;
        if !identity_matches
            || !progress_is_monotone
            || target.descriptor_revision().get() != next_revision
        {
            return Err(GraphMutationError::InvalidPlan {
                reason: "mutation descriptor transition is not an exact monotone revision",
            });
        }
        Ok(Self {
            expected: Some(expected),
            target,
        })
    }

    /// Returns the exact expected header, or none when creating it.
    #[must_use]
    pub const fn expected(self) -> Option<GraphMutationDescriptor> {
        self.expected
    }

    /// Returns the exact replacement header.
    #[must_use]
    pub const fn target(self) -> GraphMutationDescriptor {
        self.target
    }
}

/// Kind of one complete persisted mutation-descriptor entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphMutationDescriptorEntryKind {
    /// Complete outbound adjacency for the inserted node and one layer.
    OutboundLayer,
    /// Complete revision-checked replacement for one existing node and layer.
    NeighborRewire,
}

/// One complete, independently addressable mutation-descriptor payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphMutationDescriptorEntry {
    kind: GraphMutationDescriptorEntryKind,
    ordinal: usize,
    step: GraphAdjacencyStep,
}

impl GraphMutationDescriptorEntry {
    /// Creates a complete outbound-layer descriptor entry.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::InvalidPlan`] if the step is a rewire.
    pub fn outbound(step: GraphAdjacencyStep) -> GraphMutationResult<Self> {
        if step.expected_revision().is_some() {
            return Err(GraphMutationError::InvalidPlan {
                reason: "outbound descriptor entry cannot carry a replacement revision",
            });
        }
        Ok(Self {
            kind: GraphMutationDescriptorEntryKind::OutboundLayer,
            ordinal: step.adjacency().layer().get(),
            step,
        })
    }

    /// Creates a complete revision-checked neighbor-rewire descriptor entry.
    ///
    /// # Errors
    ///
    /// Returns [`GraphMutationError::InvalidPlan`] if the step lacks an
    /// expected revision or the deterministic ordinal exceeds policy.
    pub fn rewire(step: GraphAdjacencyStep, ordinal: usize) -> GraphMutationResult<Self> {
        let maximum = MAX_GRAPH_LAYERS.saturating_mul(MAX_GRAPH_NEIGHBORS_PER_LAYER);
        if step.expected_revision().is_none() || ordinal >= maximum {
            return Err(GraphMutationError::InvalidPlan {
                reason: "rewire descriptor entry requires a revision and bounded ordinal",
            });
        }
        Ok(Self {
            kind: GraphMutationDescriptorEntryKind::NeighborRewire,
            ordinal,
            step,
        })
    }

    /// Returns the descriptor entry kind.
    #[must_use]
    pub const fn kind(&self) -> GraphMutationDescriptorEntryKind {
        self.kind
    }

    /// Returns the zero-based deterministic step ordinal.
    #[must_use]
    pub const fn ordinal(&self) -> usize {
        self.ordinal
    }

    /// Returns the complete bounded adjacency and optional expected revision.
    #[must_use]
    pub const fn step(&self) -> &GraphAdjacencyStep {
        &self.step
    }
}

impl GraphMutationDescriptorEntryKind {
    /// Returns the stable version-two descriptor-entry code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::OutboundLayer => 1,
            Self::NeighborRewire => 2,
        }
    }
}

impl GraphPublicationStep {
    /// Returns the owning mutation.
    #[must_use]
    pub const fn mutation_id(self) -> GraphMutationId {
        self.reservation.mutation_id()
    }

    /// Returns the reservation whose publication becomes visible.
    #[must_use]
    pub const fn reservation(self) -> GraphNodeReservation {
        self.reservation
    }

    /// Returns the complete reader state required under the publication lock.
    #[must_use]
    pub const fn expected_state(self) -> GraphPublishedState {
        self.expected_state
    }

    /// Returns the complete replacement reader state.
    #[must_use]
    pub const fn new_state(self) -> GraphPublishedState {
        self.new_state
    }
}
